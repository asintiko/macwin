//! Clipboard synchronisation between the two machines: plain text, images
//! (PNG) and copied files. Pure logic: the driver reports local clipboard
//! changes and the peer's updates, and executes the answers (send to the
//! peer, write locally).
//!
//! Ordering: every local change gets the next value of a Lamport clock. A
//! received update is applied only when its version `(clock, origin)` is
//! newer than the version of the local clipboard; equal clocks go to the
//! listener's update. Both machines therefore settle on the same content
//! even when the user copies on both at once, and late or reordered updates
//! never overwrite a newer copy. Files arrive over time: their version is
//! taken when the transfer starts, and they are written only if nothing
//! newer arrived by the time it finished.
//!
//! Echoes: the driver does not report its own writes (it remembers the
//! change count after writing, and backends tag macwin's writes, which
//! arrive here as [`LocalClipboard::Ours`]). As a last line of defence, a
//! local content equal to the one both sides already share is not resent,
//! so a clipboard manager that re-copies our writes cannot start a loop.
//! Files compare by their relative names and sizes, not by location.
//!
//! Content is never logged: the [`fmt::Debug`] impls print sizes only.

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use crate::engine::TransportRole;
use crate::msg::{ClipboardImage, ClipboardMsg, ClipboardText, FileEntry, FilesHeader};

/// Copied files, ready to send: the entries of the transfer and, for each,
/// the local path its data is read from.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalFiles {
    pub entries: Vec<FileEntry>,
    /// `sources[i]` is the local path of `entries[i]`.
    pub sources: Vec<PathBuf>,
}

impl LocalFiles {
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }
}

/// Counts only: names and paths stay out of logs.
impl fmt::Debug for LocalFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LocalFiles({} entries, {} bytes)",
            self.entries.len(),
            self.total_bytes()
        )
    }
}

/// What the local clipboard holds after a change.
#[derive(Clone, PartialEq, Eq)]
pub enum LocalClipboard {
    /// macwin wrote it (the backend found its origin marker).
    Ours,
    /// The source app marked it as concealed or transient (password
    /// managers do): never shared.
    Sensitive,
    /// Nothing macwin shares (empty, or a kind it does not handle).
    Unsupported,
    /// Over the size limit; `bytes` is a lower bound of the size.
    TooLarge {
        bytes: u64,
    },
    Text(String),
    /// PNG-encoded.
    Image(Vec<u8>),
    Files(LocalFiles),
}

impl fmt::Debug for LocalClipboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalClipboard::Ours => f.write_str("Ours"),
            LocalClipboard::Sensitive => f.write_str("Sensitive"),
            LocalClipboard::Unsupported => f.write_str("Unsupported"),
            LocalClipboard::TooLarge { bytes } => write!(f, "TooLarge({bytes} bytes)"),
            LocalClipboard::Text(t) => write!(f, "Text({} bytes)", t.len()),
            LocalClipboard::Image(png) => write!(f, "Image({} bytes)", png.len()),
            LocalClipboard::Files(files) => write!(
                f,
                "Files({} entries, {} bytes)",
                files.entries.len(),
                files.total_bytes()
            ),
        }
    }
}

/// What may be shared in this session: both machines must accept a kind
/// for it to travel, in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    pub max_text_bytes: usize,
    pub images: bool,
    pub max_image_bytes: usize,
    pub files: bool,
    pub max_files_bytes: u64,
}

impl ClipboardPolicy {
    /// Text only.
    pub fn text_only(max_text_bytes: usize) -> ClipboardPolicy {
        ClipboardPolicy {
            max_text_bytes,
            images: false,
            max_image_bytes: 0,
            files: false,
            max_files_bytes: 0,
        }
    }
}

/// What happened to a local change, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// The content is sent to the peer.
    Shared,
    /// macwin's own write: nothing to do.
    Ours,
    /// The content both sides already share (re-copied): nothing to do.
    Echo,
    /// Not shared; the peer is told that something else was copied.
    Sensitive,
    Unsupported,
    TooLarge,
}

/// What the driver sends for a local change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Msg(ClipboardMsg),
    /// Start a file transfer: the header, then the data of `sources`.
    Files {
        header: FilesHeader,
        sources: Vec<PathBuf>,
    },
}

/// The answer to a local change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutcome {
    pub send: Option<Outgoing>,
    pub kind: LocalKind,
}

/// The answer to an update from the peer.
#[derive(Clone, PartialEq, Eq)]
pub enum RemoteOutcome {
    WriteText(String),
    WriteImage(Vec<u8>),
    /// The peer copied something that is not shared; nothing to write.
    Noted,
    /// Older than the local clipboard: ignored.
    Stale,
    /// Larger than the local limit or of a kind this machine does not take:
    /// ignored (but it is the newest version).
    Refused,
}

impl fmt::Debug for RemoteOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteOutcome::WriteText(t) => write!(f, "WriteText({} bytes)", t.len()),
            RemoteOutcome::WriteImage(png) => write!(f, "WriteImage({} bytes)", png.len()),
            RemoteOutcome::Noted => f.write_str("Noted"),
            RemoteOutcome::Stale => f.write_str("Stale"),
            RemoteOutcome::Refused => f.write_str("Refused"),
        }
    }
}

/// The answer to the start of an incoming file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesOffer {
    /// Receive it, then call [`ClipboardSync::on_files_received`].
    Accept,
    Stale,
    /// Too large or not accepted here.
    Refused,
}

/// A clipboard version; the derived order is (clock, origin), and the
/// listener's version wins a tie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    clock: u64,
    by_listener: bool,
}

/// Synchronisation state of one session.
#[derive(Debug)]
pub struct ClipboardSync {
    local_is_listener: bool,
    policy: ClipboardPolicy,
    clock: u64,
    /// Version of what the local clipboard holds (or is being received).
    current: Option<Version>,
    /// Signature of the content both machines are known to hold.
    shared: Option<u64>,
}

fn signature<T: Hash + ?Sized>(kind: u8, value: &T) -> u64 {
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);
    value.hash(&mut h);
    h.finish()
}

fn text_signature(text: &str) -> u64 {
    signature(1, text)
}

fn image_signature(png: &[u8]) -> u64 {
    signature(2, png)
}

/// Location-independent: relative names, sizes and kinds.
pub fn files_signature(entries: &[FileEntry]) -> u64 {
    let mut h = DefaultHasher::new();
    3u8.hash(&mut h);
    for e in entries {
        e.path.hash(&mut h);
        e.size.hash(&mut h);
        e.dir.hash(&mut h);
    }
    h.finish()
}

impl ClipboardSync {
    /// `role` is this machine's transport role in the session (it breaks
    /// ties).
    pub fn new(role: TransportRole, policy: ClipboardPolicy) -> ClipboardSync {
        ClipboardSync {
            local_is_listener: role == TransportRole::Listener,
            policy,
            clock: 0,
            current: None,
            shared: None,
        }
    }

    pub fn policy(&self) -> ClipboardPolicy {
        self.policy
    }

    fn next_local_version(&mut self) -> Version {
        self.clock = self.clock.saturating_add(1);
        let v = Version {
            clock: self.clock,
            by_listener: self.local_is_listener,
        };
        self.current = Some(v);
        v
    }

    /// Shares content with signature `sig` unless the peer already has it.
    fn share(&mut self, sig: u64, make: impl FnOnce(u64) -> Outgoing) -> LocalOutcome {
        if self.shared == Some(sig) {
            return LocalOutcome {
                send: None,
                kind: LocalKind::Echo,
            };
        }
        let v = self.next_local_version();
        self.shared = Some(sig);
        LocalOutcome {
            send: Some(make(v.clock)),
            kind: LocalKind::Shared,
        }
    }

    /// The local clipboard changed (and not by macwin's last write).
    pub fn on_local_change(&mut self, content: LocalClipboard) -> LocalOutcome {
        let p = self.policy;
        let withheld = match content {
            LocalClipboard::Ours => {
                return LocalOutcome {
                    send: None,
                    kind: LocalKind::Ours,
                };
            }
            LocalClipboard::Text(text) if text.len() <= p.max_text_bytes => {
                return self.share(text_signature(&text), |clock| {
                    Outgoing::Msg(ClipboardMsg::Text(ClipboardText { clock, text }))
                });
            }
            LocalClipboard::Image(png) if p.images && png.len() <= p.max_image_bytes => {
                return self.share(image_signature(&png), |clock| {
                    Outgoing::Msg(ClipboardMsg::Image(ClipboardImage { clock, png }))
                });
            }
            LocalClipboard::Files(files) if p.files && files.total_bytes() <= p.max_files_bytes => {
                let sig = files_signature(&files.entries);
                let LocalFiles { entries, sources } = files;
                return self.share(sig, |clock| Outgoing::Files {
                    header: FilesHeader { clock, entries },
                    sources,
                });
            }
            LocalClipboard::Image(_) if !p.images => LocalKind::Unsupported,
            LocalClipboard::Files(_) if !p.files => LocalKind::Unsupported,
            LocalClipboard::Text(_)
            | LocalClipboard::Image(_)
            | LocalClipboard::Files(_)
            | LocalClipboard::TooLarge { .. } => LocalKind::TooLarge,
            LocalClipboard::Sensitive => LocalKind::Sensitive,
            LocalClipboard::Unsupported => LocalKind::Unsupported,
        };
        // Something newer than the shared content is on the clipboard now:
        // tell the peer, so neither side treats the old one as shared.
        let v = self.next_local_version();
        self.shared = None;
        LocalOutcome {
            send: Some(Outgoing::Msg(ClipboardMsg::Other { clock: v.clock })),
            kind: withheld,
        }
    }

    /// Advances the clock for a remote version; `None` when it is stale.
    fn accept_remote(&mut self, clock: u64) -> Option<Version> {
        self.clock = self.clock.max(clock);
        let v = Version {
            clock,
            by_listener: !self.local_is_listener,
        };
        if self.current.is_some_and(|cur| v <= cur) {
            return None;
        }
        self.current = Some(v);
        Some(v)
    }

    /// The peer sent a text, an image or a notice.
    pub fn on_remote(&mut self, msg: ClipboardMsg) -> RemoteOutcome {
        if self.accept_remote(msg.clock()).is_none() {
            return RemoteOutcome::Stale;
        }
        let p = self.policy;
        match msg {
            ClipboardMsg::Text(t) if t.text.len() <= p.max_text_bytes => {
                self.shared = Some(text_signature(&t.text));
                RemoteOutcome::WriteText(t.text)
            }
            ClipboardMsg::Image(i) if p.images && i.png.len() <= p.max_image_bytes => {
                self.shared = Some(image_signature(&i.png));
                RemoteOutcome::WriteImage(i.png)
            }
            ClipboardMsg::Other { .. } => {
                self.shared = None;
                RemoteOutcome::Noted
            }
            ClipboardMsg::Text(_) | ClipboardMsg::Image(_) => {
                self.shared = None;
                RemoteOutcome::Refused
            }
        }
    }

    /// The peer started sending files.
    pub fn on_files_offer(&mut self, header: &FilesHeader) -> FilesOffer {
        if self.accept_remote(header.clock).is_none() {
            return FilesOffer::Stale;
        }
        self.shared = None;
        if !self.policy.files || header.total_bytes() > self.policy.max_files_bytes {
            return FilesOffer::Refused;
        }
        FilesOffer::Accept
    }

    /// Sending the files of version `clock` failed. Unless something newer
    /// is on the clipboard already, they no longer count as shared (copying
    /// them again sends them again), and the peer gets a notice of the
    /// version, in case the transfer never reached it.
    pub fn on_files_failed(&mut self, clock: u64) -> Option<ClipboardMsg> {
        let v = Version {
            clock,
            by_listener: self.local_is_listener,
        };
        if self.current != Some(v) {
            return None;
        }
        self.shared = None;
        Some(ClipboardMsg::Other { clock })
    }

    /// Sending a clipboard message of version `clock` failed before reaching
    /// the link: clear the echo signature so copying the same content again
    /// retries the send.
    pub fn on_send_failed(&mut self, clock: u64) {
        let v = Version {
            clock,
            by_listener: self.local_is_listener,
        };
        if self.current == Some(v) {
            self.shared = None;
        }
    }

    /// An accepted transfer arrived completely. True when it is still the
    /// newest content, i.e. the files should be put on the clipboard.
    pub fn on_files_received(&mut self, header: &FilesHeader) -> bool {
        let v = Version {
            clock: header.clock,
            by_listener: !self.local_is_listener,
        };
        if self.current != Some(v) {
            return false;
        }
        self.shared = Some(files_signature(&header.entries));
        true
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn policy() -> ClipboardPolicy {
        ClipboardPolicy {
            max_text_bytes: 100,
            images: true,
            max_image_bytes: 100,
            files: true,
            max_files_bytes: 1000,
        }
    }

    fn sync(role: TransportRole) -> ClipboardSync {
        ClipboardSync::new(role, policy())
    }

    fn text(s: &str) -> LocalClipboard {
        LocalClipboard::Text(s.into())
    }

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            size,
            dir: false,
            executable: false,
            mtime_ms: 0,
        }
    }

    fn files(entries: Vec<FileEntry>, root: &str) -> LocalClipboard {
        let sources = entries
            .iter()
            .map(|e| PathBuf::from(root).join(&e.path))
            .collect();
        LocalClipboard::Files(LocalFiles { entries, sources })
    }

    fn sent_text(o: &LocalOutcome) -> Option<(u64, &str)> {
        match &o.send {
            Some(Outgoing::Msg(ClipboardMsg::Text(t))) => Some((t.clock, t.text.as_str())),
            _ => None,
        }
    }

    fn is_other(o: &LocalOutcome) -> bool {
        matches!(o.send, Some(Outgoing::Msg(ClipboardMsg::Other { .. })))
    }

    fn msg(clock: u64, s: &str) -> ClipboardMsg {
        ClipboardMsg::Text(ClipboardText {
            clock,
            text: s.into(),
        })
    }

    #[test]
    fn local_texts_are_sent_with_increasing_clocks() {
        let mut s = sync(TransportRole::Dialer);
        let a = s.on_local_change(text("a"));
        assert_eq!((sent_text(&a), a.kind), (Some((1, "a")), LocalKind::Shared));
        let b = s.on_local_change(text("b"));
        assert_eq!(sent_text(&b), Some((2, "b")));
    }

    #[test]
    fn own_writes_and_echoes_are_not_resent() {
        let mut s = sync(TransportRole::Dialer);
        let ours = s.on_local_change(LocalClipboard::Ours);
        assert_eq!((ours.send, ours.kind), (None, LocalKind::Ours));
        s.on_local_change(text("a"));
        let again = s.on_local_change(text("a"));
        assert_eq!((again.send, again.kind), (None, LocalKind::Echo));
        assert_eq!(
            s.on_remote(msg(5, "remote")),
            RemoteOutcome::WriteText("remote".into())
        );
        assert_eq!(s.on_local_change(text("remote")).kind, LocalKind::Echo);
        assert_eq!(sent_text(&s.on_local_change(text("x"))), Some((6, "x")));
    }

    #[test]
    fn withheld_content_sends_a_contentless_notice() {
        let mut s = ClipboardSync::new(
            TransportRole::Listener,
            ClipboardPolicy {
                max_text_bytes: 4,
                ..policy()
            },
        );
        s.on_local_change(text("a"));
        for (content, kind) in [
            (LocalClipboard::Sensitive, LocalKind::Sensitive),
            (LocalClipboard::Unsupported, LocalKind::Unsupported),
            (LocalClipboard::TooLarge { bytes: 9 }, LocalKind::TooLarge),
            (text("too long"), LocalKind::TooLarge),
            (LocalClipboard::Image(vec![0; 101]), LocalKind::TooLarge),
            (files(vec![entry("big", 1001)], "/r"), LocalKind::TooLarge),
        ] {
            let o = s.on_local_change(content);
            assert_eq!(o.kind, kind);
            assert!(is_other(&o));
        }
        assert_eq!(sent_text(&s.on_local_change(text("a"))), Some((8, "a")));
    }

    #[test]
    fn kinds_the_policy_excludes_are_withheld_both_ways() {
        let mut s = ClipboardSync::new(TransportRole::Dialer, ClipboardPolicy::text_only(100));
        assert_eq!(
            s.on_local_change(LocalClipboard::Image(vec![1])).kind,
            LocalKind::Unsupported
        );
        assert_eq!(
            s.on_local_change(files(vec![entry("a", 1)], "/r")).kind,
            LocalKind::Unsupported
        );
        let img = ClipboardMsg::Image(ClipboardImage {
            clock: 10,
            png: vec![1],
        });
        assert_eq!(s.on_remote(img), RemoteOutcome::Refused);
        let header = FilesHeader {
            clock: 11,
            entries: vec![entry("a", 1)],
        };
        assert_eq!(s.on_files_offer(&header), FilesOffer::Refused);
    }

    #[test]
    fn images_are_shared_and_echoes_detected() {
        let mut s = sync(TransportRole::Dialer);
        let o = s.on_local_change(LocalClipboard::Image(vec![1, 2, 3]));
        assert!(matches!(
            o.send,
            Some(Outgoing::Msg(ClipboardMsg::Image(ClipboardImage {
                clock: 1,
                ..
            })))
        ));
        assert_eq!(
            s.on_local_change(LocalClipboard::Image(vec![1, 2, 3])).kind,
            LocalKind::Echo
        );
        let remote = ClipboardMsg::Image(ClipboardImage {
            clock: 5,
            png: vec![9],
        });
        assert_eq!(s.on_remote(remote), RemoteOutcome::WriteImage(vec![9]));
        assert_eq!(
            s.on_local_change(LocalClipboard::Image(vec![9])).kind,
            LocalKind::Echo
        );
    }

    #[test]
    fn files_are_offered_received_and_compared_by_name_and_size() {
        let mut sender = sync(TransportRole::Dialer);
        let mut receiver = sync(TransportRole::Listener);
        let entries = vec![
            FileEntry {
                path: "dir".into(),
                size: 0,
                dir: true,
                executable: false,
                mtime_ms: 0,
            },
            entry("dir/a.txt", 5),
        ];
        let out = sender.on_local_change(files(entries.clone(), "/Users/me"));
        let Some(Outgoing::Files { header, sources }) = out.send else {
            panic!("no transfer: {out:?}")
        };
        assert_eq!(header.clock, 1);
        assert_eq!(sources[1], PathBuf::from("/Users/me/dir/a.txt"));
        assert_eq!(receiver.on_files_offer(&header), FilesOffer::Accept);
        assert!(receiver.on_files_received(&header));
        // The receiver's copy sits elsewhere; re-copying it is still an echo.
        assert_eq!(
            receiver
                .on_local_change(files(entries.clone(), "/staging/7"))
                .kind,
            LocalKind::Echo
        );
        // The same files re-copied on the sender are an echo too.
        assert_eq!(
            sender.on_local_change(files(entries, "/Users/me")).kind,
            LocalKind::Echo
        );
    }

    #[test]
    fn failed_sends_are_retried_on_the_next_copy() {
        let mut s = sync(TransportRole::Dialer);
        let copy = || files(vec![entry("a", 1)], "/r");
        let Some(Outgoing::Files { header, .. }) = s.on_local_change(copy()).send else {
            panic!("no transfer")
        };
        assert_eq!(
            s.on_files_failed(header.clock),
            Some(ClipboardMsg::Other {
                clock: header.clock
            })
        );
        let again = s.on_local_change(copy());
        assert_eq!(again.kind, LocalKind::Shared);
        // A failure of an overtaken transfer changes nothing.
        let Some(Outgoing::Files { header: newer, .. }) = again.send else {
            panic!("no transfer")
        };
        s.on_local_change(text("t"));
        assert_eq!(s.on_files_failed(newer.clock), None);
        assert_eq!(s.on_local_change(text("t")).kind, LocalKind::Echo);
    }

    #[test]
    fn a_transfer_overtaken_by_a_newer_change_is_not_written() {
        let mut s = sync(TransportRole::Listener);
        let header = FilesHeader {
            clock: 3,
            entries: vec![entry("a", 1)],
        };
        assert_eq!(s.on_files_offer(&header), FilesOffer::Accept);
        // The user copies text while the files are still arriving.
        assert_eq!(sent_text(&s.on_local_change(text("t"))), Some((4, "t")));
        assert!(!s.on_files_received(&header));
        // An older offer is stale.
        let old = FilesHeader {
            clock: 2,
            entries: vec![],
        };
        assert_eq!(s.on_files_offer(&old), FilesOffer::Stale);
        let big = FilesHeader {
            clock: 9,
            entries: vec![entry("big", 5000)],
        };
        assert_eq!(s.on_files_offer(&big), FilesOffer::Refused);
    }

    #[test]
    fn stale_updates_are_ignored_and_clocks_advance() {
        let mut s = sync(TransportRole::Dialer);
        s.on_local_change(text("a"));
        s.on_local_change(text("b"));
        assert_eq!(s.on_remote(msg(1, "old")), RemoteOutcome::Stale);
        assert_eq!(
            s.on_remote(msg(10, "new")),
            RemoteOutcome::WriteText("new".into())
        );
        assert_eq!(s.on_remote(msg(9, "late")), RemoteOutcome::Stale);
        assert_eq!(sent_text(&s.on_local_change(text("c"))), Some((11, "c")));
    }

    #[test]
    fn remote_notices_and_oversized_content_update_the_version_only() {
        let mut s = ClipboardSync::new(
            TransportRole::Dialer,
            ClipboardPolicy {
                max_text_bytes: 3,
                ..policy()
            },
        );
        s.on_local_change(text("a"));
        assert_eq!(
            s.on_remote(ClipboardMsg::Other { clock: 4 }),
            RemoteOutcome::Noted
        );
        assert_eq!(sent_text(&s.on_local_change(text("a"))), Some((5, "a")));
        assert_eq!(s.on_remote(msg(7, "long")), RemoteOutcome::Refused);
        assert_eq!(s.on_remote(msg(6, "abc")), RemoteOutcome::Stale);
    }

    #[test]
    fn simultaneous_copies_converge_on_the_listeners_content() {
        let mut dialer = sync(TransportRole::Dialer);
        let mut listener = sync(TransportRole::Listener);
        let Some(Outgoing::Msg(from_dialer)) = dialer.on_local_change(text("d")).send else {
            panic!()
        };
        let Some(Outgoing::Msg(from_listener)) = listener.on_local_change(text("l")).send else {
            panic!()
        };
        assert_eq!(
            dialer.on_remote(from_listener),
            RemoteOutcome::WriteText("l".into())
        );
        assert_eq!(listener.on_remote(from_dialer), RemoteOutcome::Stale);
    }

    #[test]
    fn debug_output_never_contains_content() {
        let secret = "hunter2";
        for dbg in [
            format!("{:?}", text(secret)),
            format!("{:?}", RemoteOutcome::WriteText(secret.into())),
            format!("{:?}", msg(1, secret)),
            format!("{:?}", LocalClipboard::Image(secret.as_bytes().to_vec())),
        ] {
            assert!(!dbg.contains(secret), "{dbg}");
        }
    }

    // ---- convergence under arbitrary interleavings and reordering ----

    #[derive(Debug, Clone)]
    enum Op {
        /// The user copies on side 0 or 1: text, an image, files or
        /// something unshared.
        Copy(usize, u8, u8),
        /// Deliver the in-flight message at this index (mod len) to side.
        Deliver(usize, usize),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0usize..2, 0u8..4, 0u8..3).prop_map(|(s, kind, n)| Op::Copy(s, kind, n)),
            (0usize..2, any::<usize>()).prop_map(|(s, i)| Op::Deliver(s, i)),
        ]
    }

    /// What a side's clipboard shows, as a comparable label.
    fn label(kind: u8, n: u8) -> Option<String> {
        match kind {
            0 => Some(format!("text{n}")),
            1 => Some(format!("image{n}")),
            2 => Some(format!("files{n}")),
            _ => None,
        }
    }

    fn content(kind: u8, n: u8) -> LocalClipboard {
        match kind {
            0 => LocalClipboard::Text(format!("text{n}")),
            1 => LocalClipboard::Image(vec![n; 3]),
            2 => files(vec![entry(&format!("files{n}"), u64::from(n))], "/src"),
            _ => LocalClipboard::Unsupported,
        }
    }

    enum Wire {
        Msg(ClipboardMsg),
        Files(FilesHeader),
    }

    struct Side {
        sync: ClipboardSync,
        clipboard: Option<String>,
        inbox: Vec<Wire>,
    }

    fn deliver(sides: &mut [Side; 2], to: usize, index: usize) {
        if sides[to].inbox.is_empty() {
            return;
        }
        let i = index % sides[to].inbox.len();
        match sides[to].inbox.swap_remove(i) {
            Wire::Msg(m) => match sides[to].sync.on_remote(m) {
                RemoteOutcome::WriteText(t) => sides[to].clipboard = Some(t),
                RemoteOutcome::WriteImage(png) => {
                    sides[to].clipboard = Some(format!("image{}", png[0]));
                }
                _ => {}
            },
            Wire::Files(h) => {
                if sides[to].sync.on_files_offer(&h) == FilesOffer::Accept
                    && sides[to].sync.on_files_received(&h)
                {
                    sides[to].clipboard = Some(h.entries[0].path.clone());
                }
            }
        }
    }

    proptest! {
        #[test]
        fn prop_both_sides_converge(ops in prop::collection::vec(arb_op(), 0..60)) {
            let mut sides = [
                Side { sync: sync(TransportRole::Dialer), clipboard: None, inbox: vec![] },
                Side { sync: sync(TransportRole::Listener), clipboard: None, inbox: vec![] },
            ];
            let mut newest: Option<(Version, Option<String>)> = None;
            for op in ops {
                match op {
                    Op::Copy(s, kind, n) => {
                        let out = sides[s].sync.on_local_change(content(kind, n));
                        sides[s].clipboard = label(kind, n);
                        match out.send {
                            Some(Outgoing::Msg(m)) => sides[1 - s].inbox.push(Wire::Msg(m)),
                            Some(Outgoing::Files { header, .. }) => {
                                sides[1 - s].inbox.push(Wire::Files(header));
                            }
                            None => {}
                        }
                        let v = sides[s].sync.current.unwrap();
                        if newest.as_ref().is_none_or(|(nv, _)| v > *nv) {
                            newest = Some((v, label(kind, n)));
                        }
                    }
                    Op::Deliver(s, i) => deliver(&mut sides, s, i),
                }
            }
            for s in 0..2 {
                while !sides[s].inbox.is_empty() {
                    deliver(&mut sides, s, 0);
                }
            }
            prop_assert_eq!(sides[0].sync.current, sides[1].sync.current);
            if let Some((_, Some(newest))) = newest {
                prop_assert_eq!(sides[0].clipboard.as_deref(), Some(newest.as_str()));
                prop_assert_eq!(sides[1].clipboard.as_deref(), Some(newest.as_str()));
            }
        }
    }

    #[test]
    fn failed_message_send_clears_echo_guard_for_current_version() {
        let mut a = ClipboardSync::new(TransportRole::Dialer, policy());
        let out = a.on_local_change(LocalClipboard::Text("retry me".into()));
        let Some(Outgoing::Msg(ClipboardMsg::Text(t))) = out.send else {
            panic!("expected text outgoing");
        };
        assert_eq!(
            a.on_local_change(LocalClipboard::Text("retry me".into()))
                .kind,
            LocalKind::Echo
        );
        a.on_send_failed(t.clock);
        let retry = a.on_local_change(LocalClipboard::Text("retry me".into()));
        assert_eq!(retry.kind, LocalKind::Shared);
        assert!(retry.send.is_some());
    }

    #[test]
    fn files_header_total_bytes_saturates_on_overflow() {
        let header = FilesHeader {
            clock: 1,
            entries: vec![entry("a", u64::MAX - 10), entry("b", 100)],
        };
        assert_eq!(header.total_bytes(), u64::MAX);
        let mut b = ClipboardSync::new(TransportRole::Listener, policy());
        assert_eq!(b.on_files_offer(&header), FilesOffer::Refused);
    }
}
