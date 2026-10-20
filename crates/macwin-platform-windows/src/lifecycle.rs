use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct ActiveOwner<T> {
    current: Mutex<Option<Arc<T>>>,
}

impl<T> ActiveOwner<T> {
    pub(crate) const fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    pub(crate) fn current(&self) -> Option<Arc<T>> {
        lock(&self.current).clone()
    }

    pub(crate) fn reserve(&self, owner: Arc<T>) -> bool {
        let mut current = lock(&self.current);
        if current.is_some() {
            return false;
        }
        *current = Some(owner);
        true
    }

    pub(crate) fn clear_if(&self, owner: &Arc<T>) -> bool {
        let mut current = lock(&self.current);
        if current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, owner))
        {
            *current = None;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct ThreadState {
    thread_id: Option<u32>,
    cancelled: bool,
    published: bool,
}

#[derive(Default)]
pub(crate) struct ThreadControl {
    state: Mutex<ThreadState>,
}

impl ThreadControl {
    pub(crate) fn publish(&self, thread_id: u32) -> bool {
        let mut state = lock(&self.state);
        if state.cancelled || state.published {
            return false;
        }
        state.published = true;
        state.thread_id = Some(thread_id);
        true
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        lock(&self.state).cancelled
    }

    pub(crate) fn with_running_thread<R>(&self, post: impl FnOnce(Option<u32>) -> R) -> R {
        let state = lock(&self.state);
        post(if state.cancelled {
            None
        } else {
            state.thread_id
        })
    }

    pub(crate) fn cancel_with<R>(&self, post_quit: impl FnOnce(Option<u32>) -> R) -> R {
        let mut state = lock(&self.state);
        state.cancelled = true;
        post_quit(state.thread_id)
    }

    pub(crate) fn finish(&self, thread_id: u32) {
        let mut state = lock(&self.state);
        state.cancelled = true;
        if state.thread_id == Some(thread_id) {
            state.thread_id = None;
        }
    }
}

pub(crate) fn transition_and_post<S, M>(
    state: &Mutex<S>,
    transition: impl FnOnce(&mut S) -> M,
    post: impl FnOnce(M) -> bool,
    rollback: impl FnOnce(&mut S),
) -> bool {
    let mut state = lock(state);
    let message = transition(&mut state);
    if post(message) {
        true
    } else {
        rollback(&mut state);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveOwner, ThreadControl, lock, transition_and_post};
    use std::sync::{Arc, Barrier, Mutex, TryLockError};
    use std::thread;

    #[test]
    fn owner_reservation_is_atomic_and_cleanup_is_scoped() {
        let active = Arc::new(ActiveOwner::new());
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let active = active.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let owner = Arc::new(());
                    barrier.wait();
                    active.reserve(owner)
                })
            })
            .collect();
        barrier.wait();
        let reservations = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|reserved| *reserved)
            .count();

        assert_eq!(reservations, 1);
        let owner = active.current().unwrap();
        let other = Arc::new(());
        assert!(!active.clear_if(&other));
        assert!(Arc::ptr_eq(&active.current().unwrap(), &owner));
        assert!(active.clear_if(&owner));
        assert!(active.current().is_none());
    }

    #[test]
    fn cancellation_before_queue_readiness_prevents_late_start() {
        let thread = ThreadControl::default();
        assert_eq!(thread.cancel_with(|thread_id| thread_id), None);
        assert!(!thread.publish(17));
        assert!(thread.is_cancelled());
    }

    #[test]
    fn finished_thread_is_never_used_as_a_post_target() {
        let thread = ThreadControl::default();
        assert!(thread.publish(23));
        thread.finish(23);
        assert_eq!(thread.with_running_thread(|thread_id| thread_id), None);
        assert!(!thread.publish(23));
    }

    #[test]
    fn serialized_transitions_and_posts_keep_the_same_order() {
        let state = Arc::new(Mutex::new(Vec::new()));
        let posted = Arc::new(Mutex::new(Vec::new()));
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = [1, 2]
            .into_iter()
            .map(|command| {
                let state = state.clone();
                let posted = posted.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    assert!(transition_and_post(
                        &state,
                        |state| {
                            state.push(command);
                            command
                        },
                        |message| {
                            assert!(matches!(state.try_lock(), Err(TryLockError::WouldBlock)));
                            lock(&posted).push(message);
                            true
                        },
                        |_| {},
                    ));
                })
            })
            .collect();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(*lock(&state), *lock(&posted));
    }

    #[test]
    fn failed_post_rolls_back_the_local_transition() {
        let state = Mutex::new(false);
        let posted = transition_and_post(
            &state,
            |grabbed| {
                *grabbed = true;
            },
            |_| false,
            |grabbed| *grabbed = false,
        );

        assert!(!posted);
        assert!(!*lock(&state));
    }
}
