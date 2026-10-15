//! Clipboard sharing of one session: the ordering rules
//! ([`ClipboardSync`]), the clipboard thread, and file transfers in both
//! directions. Owned by the session's run task; every method returns
//! immediately (transfers run in their own tasks and report back through
//! [`SharingEvents`]).
//!
//! At most one transfer runs each way, and only while it carries the newest
//! version: any newer change, local or from the peer, cancels both (an
//! incoming one is removed from the staging directory).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use macwin_core::clipboard::{
    ClipboardPolicy, ClipboardSync, FilesOffer, LocalClipboard, Outgoing, RemoteOutcome,
};
use macwin_core::engine::TransportRole;
use macwin_core::msg::{
    CAP_CLIPBOARD_FILES, CAP_CLIPBOARD_IMAGE, ClipboardMsg, FilesHeader, Hello,
};
use macwin_net::link::{FilesDelivery, IncomingFiles, LinkSender};
use macwin_platform::{ClipboardBackend, PlatformError, SnapshotLimits};
use macwin_proto::MAX_CLIPBOARD_IMAGE;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::clipboard::{ClipboardWorker, ClipboardWrite, POLL_INTERVAL, WorkerLimits};
use crate::transfer::{Staging, receive_into, send_data};

/// Clipboard sharing settings of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSettings {
    /// Texts above this size are neither sent nor written.
    pub max_text_bytes: usize,
    /// Share images (PNG).
    pub images: bool,
    pub max_image_bytes: usize,
    /// Share copied files.
    pub files: bool,
    /// Copied files above this total are not sent, nor accepted.
    pub max_files_bytes: u64,
    /// Where received files wait until they are pasted.
    pub staging: PathBuf,
    /// How often the clipboard is checked for changes.
    pub poll: Duration,
}

impl ClipboardSettings {
    /// Text only.
    pub fn text(max_text_bytes: usize) -> ClipboardSettings {
        ClipboardSettings {
            max_text_bytes,
            images: false,
            max_image_bytes: 0,
            files: false,
            max_files_bytes: 0,
            staging: PathBuf::new(),
            poll: POLL_INTERVAL,
        }
    }
}

/// What a transfer task reports.
pub enum TransferEvent {
    /// An incoming transfer finished (or failed) in `dir`.
    Received {
        header: FilesHeader,
        dir: PathBuf,
        result: io::Result<Vec<PathBuf>>,
    },
    /// Sending the files of version `clock` failed.
    SendFailed { clock: u64 },
}

/// What the run loop waits on for this module.
pub struct SharingEvents {
    /// Local clipboard changes from the clipboard thread.
    pub local: mpsc::Receiver<LocalClipboard>,
    /// Transfers that finished or failed.
    pub transfers: mpsc::Receiver<TransferEvent>,
}

/// An incoming transfer in progress.
struct Receiving {
    task: JoinHandle<()>,
    dir: PathBuf,
}

pub struct Sharing {
    sync: ClipboardSync,
    worker: ClipboardWorker,
    link: LinkSender,
    staging: Option<Staging>,
    sending: Option<JoinHandle<()>>,
    receiving: Option<Receiving>,
    events: mpsc::Sender<TransferEvent>,
}

/// What both machines accept in this session.
pub fn session_policy(s: &ClipboardSettings, peer: &Hello) -> ClipboardPolicy {
    ClipboardPolicy {
        max_text_bytes: s.max_text_bytes,
        images: s.images && peer.has_capability(CAP_CLIPBOARD_IMAGE),
        max_image_bytes: s.max_image_bytes.min(MAX_CLIPBOARD_IMAGE),
        files: s.files && peer.has_capability(CAP_CLIPBOARD_FILES),
        max_files_bytes: s.max_files_bytes,
    }
}

impl Sharing {
    /// Starts the clipboard thread. Must run inside a tokio runtime.
    pub fn start(
        settings: &ClipboardSettings,
        backend: Box<dyn ClipboardBackend>,
        peer: &Hello,
        role: TransportRole,
        link: LinkSender,
    ) -> Result<(Sharing, SharingEvents), PlatformError> {
        let policy = session_policy(settings, peer);
        let limits = WorkerLimits {
            snapshot: SnapshotLimits {
                max_text_bytes: policy.max_text_bytes,
                images: policy.images,
                max_image_bytes: policy.max_image_bytes,
                files: policy.files,
            },
            max_files_bytes: policy.max_files_bytes,
        };
        let (worker, local) = ClipboardWorker::start(backend, limits, settings.poll)?;
        info!(
            images = policy.images,
            files = policy.files,
            "clipboard sharing is on"
        );
        let (events, transfers) = mpsc::channel(4);
        Ok((
            Sharing {
                sync: ClipboardSync::new(role, policy),
                worker,
                link,
                staging: policy.files.then(|| Staging::new(&settings.staging)),
                sending: None,
                receiving: None,
                events,
            },
            SharingEvents { local, transfers },
        ))
    }

    fn cancel_send(&mut self) {
        if let Some(task) = self.sending.take() {
            task.abort();
        }
    }

    /// Stops the incoming transfer and removes what it wrote, once its
    /// task is really gone.
    fn cancel_receive(&mut self) {
        if let Some(Receiving { task, dir }) = self.receiving.take() {
            task.abort();
            tokio::spawn(async move {
                let _ = task.await;
                Staging::discard(&dir).await;
            });
        }
    }

    /// A local change: share it (a newer change cancels the transfers still
    /// running).
    pub fn on_local(&mut self, content: LocalClipboard) {
        let out = self.sync.on_local_change(content);
        debug!(kind = ?out.kind, "local clipboard change");
        let Some(send) = out.send else {
            return;
        };
        self.cancel_send();
        self.cancel_receive();
        match send {
            Outgoing::Msg(m) => self.send(m),
            Outgoing::Files { header, sources } => {
                let link = self.link.clone();
                let events = self.events.clone();
                self.sending = Some(tokio::spawn(async move {
                    let entries = header.entries.len();
                    let bytes = header.total_bytes();
                    let result = async {
                        let mut out = link
                            .send_files(&header)
                            .await
                            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
                        send_data(&mut out, &header.entries, &sources).await?;
                        out.finish().await
                    }
                    .await;
                    match result {
                        Ok(FilesDelivery::Delivered) => info!(entries, bytes, "files sent"),
                        Ok(FilesDelivery::Stopped) => {
                            debug!("the peer stopped the transfer (it has something newer)");
                        }
                        Err(e) => {
                            warn!(error = %e, "sending files failed");
                            let clock = header.clock;
                            let _ = events.send(TransferEvent::SendFailed { clock }).await;
                        }
                    }
                }));
            }
        }
    }

    fn send(&mut self, m: ClipboardMsg) {
        let clock = m.clock();
        if let Err(e) = self.link.send_clipboard(m) {
            debug!(error = %e, "clipboard update not sent");
            self.sync.on_send_failed(clock);
        }
    }

    /// A text, image or notice from the peer.
    pub fn on_remote(&mut self, m: ClipboardMsg) {
        let outcome = self.sync.on_remote(m);
        if outcome != RemoteOutcome::Stale {
            self.cancel_send();
            self.cancel_receive();
        }
        match outcome {
            RemoteOutcome::WriteText(text) => {
                debug!(bytes = text.len(), "writing the peer's text");
                self.worker.write(ClipboardWrite::Text(text));
            }
            RemoteOutcome::WriteImage(png) => {
                debug!(bytes = png.len(), "writing the peer's image");
                self.worker.write(ClipboardWrite::Image(png));
            }
            other => debug!(outcome = ?other, "peer clipboard update"),
        }
    }

    /// The peer started sending files: receive them (dropping `incoming`
    /// stops the sender).
    pub fn on_files(&mut self, incoming: IncomingFiles) {
        let offer = self.sync.on_files_offer(&incoming.header);
        if offer != FilesOffer::Stale {
            self.cancel_send();
            self.cancel_receive();
        }
        let Some(staging) = self.staging.clone().filter(|_| offer == FilesOffer::Accept) else {
            debug!(?offer, "incoming files not accepted");
            return;
        };
        let header = incoming.header.clone();
        let dir = staging.reserve();
        let events = self.events.clone();
        let task_dir = dir.clone();
        let task = tokio::spawn(async move {
            let result = async {
                staging.create(&task_dir).await?;
                receive_into(incoming, &task_dir).await
            }
            .await;
            let _ = events
                .send(TransferEvent::Received {
                    header,
                    dir: task_dir,
                    result,
                })
                .await;
        });
        self.receiving = Some(Receiving { task, dir });
    }

    /// A transfer finished or failed.
    pub fn on_transfer(&mut self, event: TransferEvent) {
        match event {
            TransferEvent::SendFailed { clock } => {
                if let Some(notice) = self.sync.on_files_failed(clock) {
                    self.send(notice);
                }
            }
            TransferEvent::Received {
                header,
                dir,
                result,
            } => {
                if self.receiving.as_ref().is_some_and(|r| r.dir == dir) {
                    self.receiving = None;
                }
                self.on_received(&header, &dir, result);
            }
        }
    }

    /// Puts received files on the clipboard unless something newer arrived
    /// meanwhile; only then are older transfers removed.
    fn on_received(&mut self, header: &FilesHeader, dir: &Path, result: io::Result<Vec<PathBuf>>) {
        let paths = match result {
            Ok(paths) if self.sync.on_files_received(header) => paths,
            Ok(_) => {
                debug!("received files were superseded by a newer copy");
                discard(dir);
                return;
            }
            Err(e) => {
                warn!(error = %e, "receiving files failed");
                discard(dir);
                return;
            }
        };
        info!(
            items = paths.len(),
            bytes = header.total_bytes(),
            "files received; paste them in Finder / Explorer"
        );
        self.worker.write(ClipboardWrite::Files(paths));
        if let Some(staging) = self.staging.clone() {
            let dir = dir.to_path_buf();
            tokio::spawn(async move {
                if let Err(e) = staging.keep_only(&dir).await {
                    debug!(error = %e, "cannot remove older received files");
                }
            });
        }
    }
}

fn discard(dir: &Path) {
    let dir = dir.to_path_buf();
    tokio::spawn(async move { Staging::discard(&dir).await });
}

impl Drop for Sharing {
    fn drop(&mut self) {
        self.cancel_send();
        if let Some(Receiving { task, dir }) = self.receiving.take() {
            // The session is over: a partial transfer is of no use. Best
            // effort (the task may still finish a write); the next pasted
            // transfer removes whatever stays.
            task.abort();
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
