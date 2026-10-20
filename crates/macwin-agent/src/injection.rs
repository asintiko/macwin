//! Ordered, non-blocking submission to the OS input injector.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use macwin_core::keys::Pressed;
use macwin_core::latency::StageTimes;
use macwin_core::types::{InjectCommand, Point};
use macwin_platform::{InputInjector, PlatformError};
use tokio::sync::{Notify, watch};
use tracing::warn;

const QUEUE_CAPACITY: usize = 1024;

struct Pending {
    command: InjectCommand,
    received: Option<Instant>,
}

pub(crate) struct InjectionQueue {
    commands: mpsc::SyncSender<Option<Pending>>,
    error: watch::Receiver<Option<String>>,
}

impl InjectionQueue {
    pub fn send(&self, command: InjectCommand, received: Option<Instant>) -> Result<(), String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        self.commands
            .try_send(Some(Pending { command, received }))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => "input injection queue is full".into(),
                mpsc::TrySendError::Disconnected(_) => "input injection thread stopped".into(),
            })
    }

    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }
}

pub(crate) struct InjectionWorker {
    commands: mpsc::SyncSender<Option<Pending>>,
    stopped: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl InjectionWorker {
    pub fn start(
        mut backend: Box<dyn InputInjector>,
        stages: Arc<Mutex<StageTimes>>,
        wake: Arc<Notify>,
    ) -> Result<(InjectionQueue, Self), PlatformError> {
        let (commands, pending) = mpsc::sync_channel::<Option<Pending>>(QUEUE_CAPACITY);
        let (error_tx, error) = watch::channel(None);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let join = thread::Builder::new()
            .name("macwin-injection".into())
            .spawn(move || {
                let mut pressed = Pressed::default();
                let mut at = Point::default();
                while let Ok(Some(Pending { command, received })) = pending.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // A failed native call may already have applied a press.
                    match command {
                        InjectCommand::Key {
                            hid, down: true, ..
                        } => {
                            pressed.keys.insert(hid);
                        }
                        InjectCommand::Button {
                            button,
                            down: true,
                            at: pos,
                        } => {
                            pressed.buttons.insert(button);
                            at = pos;
                        }
                        _ => {}
                    }
                    if let Err(e) = inject(&mut *backend, command) {
                        error_tx.send_replace(Some(format!("input injection failed: {e}")));
                        wake.notify_one();
                        break;
                    }
                    match command {
                        InjectCommand::Key {
                            hid, down: false, ..
                        } => {
                            pressed.keys.remove(hid);
                        }
                        InjectCommand::Button {
                            button,
                            down: false,
                            at: pos,
                        } => {
                            pressed.buttons.remove(button);
                            at = pos;
                        }
                        InjectCommand::MoveTo(pos) => at = pos,
                        _ => {}
                    }
                    if let Some(received) = received {
                        stages
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record_receive(received.elapsed());
                    }
                }
                // Stop discards queued input, but never leaves an applied press held.
                for command in pressed.release_all(at) {
                    if let Err(e) = inject(&mut *backend, command) {
                        warn!(error = %e, "cannot release injected input");
                    }
                }
            })
            .map_err(|e| {
                PlatformError::Backend(format!("cannot start the injection thread: {e}"))
            })?;
        Ok((
            InjectionQueue {
                commands: commands.clone(),
                error,
            },
            Self {
                commands,
                stopped,
                join: Some(join),
            },
        ))
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.commands.try_send(None);
    }
}

impl Drop for InjectionWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn inject(backend: &mut dyn InputInjector, command: InjectCommand) -> Result<(), PlatformError> {
    catch_unwind(AssertUnwindSafe(|| backend.inject(command)))
        .unwrap_or_else(|_| Err(PlatformError::Backend("input injector panicked".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use macwin_core::types::{Hid, MouseButton};
    use macwin_platform::testing::FakeInjector;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);
    const KEY_A: Hid = Hid(0x07_0004);

    struct Injector<F>(F);

    impl<F: FnMut(InjectCommand) -> Result<(), PlatformError> + Send> InputInjector for Injector<F> {
        fn inject(&mut self, command: InjectCommand) -> Result<(), PlatformError> {
            (self.0)(command)
        }
    }

    fn key(hid: Hid, down: bool) -> InjectCommand {
        InjectCommand::Key {
            hid,
            down,
            repeat: false,
        }
    }

    #[test]
    fn preserves_order_and_releases_applied_input_before_joining() {
        let (mut backend, state) = FakeInjector::new();
        let (applied, events) = mpsc::channel();
        let (queue, worker) = InjectionWorker::start(
            Box::new(Injector(move |command| {
                backend.inject(command)?;
                applied.send(()).unwrap();
                Ok(())
            })),
            Arc::new(Mutex::new(StageTimes::default())),
            Arc::new(Notify::new()),
        )
        .unwrap();
        let at = Point::new(8, 9);
        let commands = [
            key(Hid::LEFT_CTRL, true),
            key(KEY_A, true),
            InjectCommand::Button {
                button: MouseButton::Left,
                down: true,
                at: Point::default(),
            },
            InjectCommand::MoveTo(at),
        ];
        for command in commands {
            queue.send(command, None).unwrap();
        }
        for _ in commands {
            events.recv_timeout(WAIT).unwrap();
        }
        drop(worker);
        let mut expected = commands.to_vec();
        expected.extend([
            InjectCommand::Button {
                button: MouseButton::Left,
                down: false,
                at,
            },
            key(KEY_A, false),
            key(Hid::LEFT_CTRL, false),
        ]);
        assert_eq!(state.commands(), expected);
        assert!(state.pressed_keys().is_empty());
        assert!(state.pressed_buttons().is_empty());
        assert_eq!(state.unmatched_ups(), 0);
        assert!(queue.send(key(KEY_A, true), None).is_err());
    }

    #[test]
    fn a_blocked_injector_cannot_block_submission_or_hide_its_latency() {
        let (mut backend, state) = FakeInjector::new();
        let (entered, ready) = mpsc::channel();
        let (resume, blocked) = mpsc::channel();
        let mut blocked = Some(blocked);
        let stages = Arc::new(Mutex::new(StageTimes::default()));
        let (queue, worker) = InjectionWorker::start(
            Box::new(Injector(move |command| {
                backend.inject(command)?;
                if let Some(blocked) = blocked.take() {
                    entered.send(()).unwrap();
                    blocked.recv_timeout(WAIT).unwrap();
                }
                Ok(())
            })),
            stages.clone(),
            Arc::new(Notify::new()),
        )
        .unwrap();
        queue
            .send(
                key(KEY_A, true),
                Some(Instant::now() - Duration::from_millis(20)),
            )
            .unwrap();
        ready.recv_timeout(WAIT).unwrap();
        assert_eq!(stages.lock().unwrap().summary().receive.count, 0);
        for _ in 0..QUEUE_CAPACITY {
            queue
                .send(InjectCommand::MoveTo(Point::new(50, 60)), None)
                .unwrap();
        }
        assert_eq!(
            queue.send(key(KEY_A, false), None),
            Err("input injection queue is full".into())
        );
        worker.stop();
        resume.send(()).unwrap();
        drop(worker);
        assert_eq!(state.commands(), vec![key(KEY_A, true), key(KEY_A, false)]);
        assert!(state.pressed_keys().is_empty());
        let receive = stages.lock().unwrap().summary().receive;
        assert_eq!(receive.count, 1);
        assert!(receive.max.unwrap() >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn errors_and_panics_wake_the_driver_and_release_even_a_partial_press() {
        for panics in [false, true] {
            let (mut backend, state) = FakeInjector::new();
            let wake = Arc::new(Notify::new());
            let (queue, worker) = InjectionWorker::start(
                Box::new(Injector(move |command| {
                    backend.inject(command)?;
                    if command == key(KEY_A, true) {
                        assert!(!panics, "injection panic");
                        return Err(PlatformError::Backend("injection error".into()));
                    }
                    Ok(())
                })),
                Arc::new(Mutex::new(StageTimes::default())),
                wake.clone(),
            )
            .unwrap();
            queue.send(key(Hid::LEFT_CTRL, true), None).unwrap();
            queue.send(key(KEY_A, true), None).unwrap();
            tokio::time::timeout(WAIT, wake.notified()).await.unwrap();
            assert!(queue.error().unwrap().contains(if panics {
                "panicked"
            } else {
                "injection error"
            }));
            drop(worker);
            assert!(state.pressed_keys().is_empty());
            assert_eq!(state.unmatched_ups(), 0);
            assert_eq!(
                &state.commands()[2..],
                &[key(KEY_A, false), key(Hid::LEFT_CTRL, false)]
            );
        }
    }
}
