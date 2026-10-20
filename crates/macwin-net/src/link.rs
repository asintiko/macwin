//! A transport-agnostic message link: what the driver talks to.
//!
//! The QUIC implementation runs one writer task per stream, one reader task
//! per stream, one datagram reader, one acceptor of unidirectional streams
//! and a supervisor. Every decode error on any channel closes the
//! connection as a protocol violation. Exactly one [`Inbound::Closed`] is
//! delivered per link, after every message that arrived before the close.
//!
//! Clipboard updates travel on their own unidirectional streams (one per
//! message, type byte [`STREAM_CLIPBOARD`], lowest priority), so a large
//! text never delays heartbeats or input. They are best effort: an update
//! still in flight when the link closes is lost.
//!
//! [`Link::memory_pair`] is an in-process implementation for tests.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use macwin_core::msg::{ClipboardMsg, ControlMsg, DatagramMsg, FilesHeader, InputMsg};
use macwin_proto::frame::{self, FrameBuffer};
use macwin_proto::{
    MAX_CLIPBOARD_FRAME, MAX_FILES_HEADER, ProtoError, clipboard_within_limits, decode_clipboard,
    decode_control, decode_datagram, decode_files_header, decode_input, encode_clipboard,
    encode_control, encode_datagram, encode_files_header, encode_input,
};
use quinn::{
    Connection, ConnectionError, Endpoint, ReadToEndError, RecvStream, SendStream, VarInt,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

use crate::codes;

/// First byte of a unidirectional stream carrying one [`ClipboardMsg`].
pub const STREAM_CLIPBOARD: u8 = 0x10;
/// First byte of a unidirectional stream carrying copied files: a
/// length-prefixed [`FilesHeader`], then the data of its entries in order.
pub const STREAM_FILES: u8 = 0x11;
/// Send priority of clipboard streams (control is 0, input 1).
const CLIPBOARD_PRIORITY: i32 = -1;
/// Send priority of file streams: below everything else.
const FILES_PRIORITY: i32 = -2;
/// Bound on receiving one clipboard stream.
pub const CLIPBOARD_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on receiving the header of a files stream.
pub const FILES_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// Stream reset code of a cancelled file transfer.
pub const FILES_CANCELLED: u32 = 1;
/// Incoming file transfers queued towards the driver.
const FILES_QUEUE: usize = 4;
/// Buffer of an in-memory file transfer.
const MEMORY_PIPE: usize = 64 * 1024;

/// A file transfer from the peer: its header, then the data of the
/// entries in order (`header.total_bytes()` bytes, then the end). Dropping
/// it before the end tells the sender to stop.
pub struct IncomingFiles {
    pub header: FilesHeader,
    pub body: Box<dyn AsyncRead + Send + Unpin>,
}

impl fmt::Debug for IncomingFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingFiles")
            .field("clock", &self.header.clock)
            .field("entries", &self.header.entries.len())
            .field("bytes", &self.header.total_bytes())
            .finish()
    }
}

/// The data side of an outgoing file transfer. Write the entries' data in
/// header order, then [`OutgoingFiles::finish`]; [`OutgoingFiles::cancel`]
/// aborts it (so does dropping it before `finish` returns).
pub struct OutgoingFiles {
    inner: Option<OutgoingInner>,
}

enum OutgoingInner {
    Quic(SendStream),
    Memory(DuplexStream),
}

/// How a finished transfer ended on the receiving side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesDelivery {
    /// The peer read everything.
    Delivered,
    /// The peer stopped reading (it had something newer, or refused it).
    Stopped,
}

impl OutgoingFiles {
    fn pin_inner(&mut self) -> io::Result<Pin<&mut (dyn AsyncWrite + Send + Unpin)>> {
        match self.inner.as_mut() {
            Some(OutgoingInner::Quic(s)) => Ok(Pin::new(s)),
            Some(OutgoingInner::Memory(d)) => Ok(Pin::new(d)),
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "transfer closed")),
        }
    }

    /// Ends the transfer and waits until the peer has read it all or
    /// stopped reading.
    pub async fn finish(mut self) -> io::Result<FilesDelivery> {
        match self.inner.take() {
            Some(OutgoingInner::Quic(mut s)) => {
                s.finish()
                    .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
                match s.stopped().await {
                    Ok(None) => Ok(FilesDelivery::Delivered),
                    Ok(Some(_)) => Ok(FilesDelivery::Stopped),
                    Err(e) => Err(io::Error::new(io::ErrorKind::BrokenPipe, e)),
                }
            }
            Some(OutgoingInner::Memory(mut d)) => {
                d.shutdown().await?;
                Ok(FilesDelivery::Delivered)
            }
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "transfer closed")),
        }
    }

    /// Aborts the transfer; the peer discards what it received.
    pub fn cancel(mut self) {
        self.abort();
    }

    fn abort(&mut self) {
        if let Some(OutgoingInner::Quic(mut s)) = self.inner.take() {
            let _ = s.reset(VarInt::from_u32(FILES_CANCELLED));
        }
    }
}

impl Drop for OutgoingFiles {
    fn drop(&mut self) {
        // A dropped quinn stream would otherwise finish gracefully, and the
        // peer would see a short transfer instead of an abort.
        self.abort();
    }
}

impl AsyncWrite for OutgoingFiles {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut().pin_inner() {
            Ok(w) => w.poll_write(cx, buf),
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut().pin_inner() {
            Ok(w) => w.poll_flush(cx),
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut().pin_inner() {
            Ok(w) => w.poll_shutdown(cx),
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// Reads from a memory pipe; exists so tests see the same type-erased body
/// as with QUIC.
struct MemoryBody(DuplexStream);

impl AsyncRead for MemoryBody {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

/// Outgoing queue length per stream; overflowing it closes the link.
pub const QUEUE_CAPACITY: usize = 4096;
/// Inbound queue length.
pub const INBOUND_CAPACITY: usize = 4096;
/// Bound on flushing and finishing streams during a graceful close.
pub const GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// Bound on waiting for the close to reach the peer in [`LinkSender::wait_closed`].
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const READ_CHUNK: usize = 64 * 1024;

/// Something that arrived on the link.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Control(ControlMsg),
    Input(InputMsg),
    Datagram(DatagramMsg),
    Clipboard(ClipboardMsg),
    /// The link is gone; always the last item.
    Closed(LinkClosed),
}

/// Why a link closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkClosed {
    /// The peer closed with an application close code.
    PeerClosed { code: u64 },
    /// This side closed with the given code.
    LocalClosed { code: u64 },
    /// No packets from the peer within the idle timeout.
    TimedOut,
    /// Any other transport-level failure.
    Transport(String),
    /// The peer sent something undecodable; this side closed the link.
    ProtocolViolation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the link is closed")]
    Closed,
    #[error("send queue overflow; the link was closed")]
    Overflow,
    #[error("the message exceeds the size limit")]
    TooLarge,
}

/// Both halves of a link.
pub struct Link {
    pub tx: LinkSender,
    pub rx: mpsc::Receiver<Inbound>,
    /// File transfers from the peer, in arrival order.
    pub files: mpsc::Receiver<IncomingFiles>,
}

/// Cheap-to-clone sending half. All methods are non-blocking and safe to
/// call from any thread inside a tokio runtime context or not.
#[derive(Clone)]
pub struct LinkSender {
    inner: Arc<Inner>,
}

enum Inner {
    Quic(QuicShared),
    Memory(MemoryShared),
}

type Reason = Arc<Mutex<Option<LinkClosed>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records why the link closed; the first reason wins.
fn set_reason(reason: &Mutex<Option<LinkClosed>>, r: LinkClosed) {
    let mut slot = lock(reason);
    if slot.is_none() {
        *slot = Some(r);
    }
}

impl LinkSender {
    pub fn send_control(&self, m: ControlMsg) -> Result<(), LinkError> {
        match &*self.inner {
            Inner::Quic(q) => q.send_framed(&q.control, &encode_control(&m)),
            Inner::Memory(mem) => mem.send(Inbound::Control(m)),
        }
    }

    pub fn send_input(&self, m: InputMsg) -> Result<(), LinkError> {
        match &*self.inner {
            Inner::Quic(q) => q.send_framed(&q.input, &encode_input(&m)),
            Inner::Memory(mem) => mem.send(Inbound::Input(m)),
        }
    }

    /// Sends a clipboard update on its own stream (best effort, see the
    /// module docs). Fails only when the link is closed or the text exceeds
    /// [`macwin_proto::MAX_CLIPBOARD_TEXT`].
    pub fn send_clipboard(&self, m: ClipboardMsg) -> Result<(), LinkError> {
        if !clipboard_within_limits(&m) {
            return Err(LinkError::TooLarge);
        }
        match &*self.inner {
            Inner::Quic(q) => q.send_clipboard(&m),
            Inner::Memory(mem) => mem.send(Inbound::Clipboard(m)),
        }
    }

    /// Starts a file transfer: sends `header` on a stream of its own and
    /// returns the writer for the entries' data.
    pub async fn send_files(&self, header: &FilesHeader) -> Result<OutgoingFiles, LinkError> {
        let body = encode_files_header(header);
        if body.len() > MAX_FILES_HEADER {
            return Err(LinkError::TooLarge);
        }
        match &*self.inner {
            Inner::Quic(q) => {
                if q.closing.load(Ordering::Acquire) {
                    return Err(LinkError::Closed);
                }
                let mut send = q.conn.open_uni().await.map_err(|_| LinkError::Closed)?;
                let _ = send.set_priority(FILES_PRIORITY);
                let mut prefix = vec![STREAM_FILES];
                prefix.extend_from_slice(&frame::encode(&body));
                send.write_all(&prefix)
                    .await
                    .map_err(|_| LinkError::Closed)?;
                Ok(OutgoingFiles {
                    inner: Some(OutgoingInner::Quic(send)),
                })
            }
            Inner::Memory(mem) => {
                if mem.closed.load(Ordering::Acquire) {
                    return Err(LinkError::Closed);
                }
                let (writer, reader) = tokio::io::duplex(MEMORY_PIPE);
                let incoming = IncomingFiles {
                    header: header.clone(),
                    body: Box::new(MemoryBody(reader)),
                };
                mem.peer_files
                    .send(incoming)
                    .await
                    .map_err(|_| LinkError::Closed)?;
                Ok(OutgoingFiles {
                    inner: Some(OutgoingInner::Memory(writer)),
                })
            }
        }
    }

    /// Unreliable: dropped silently when the link is closed or congested.
    pub fn send_datagram(&self, m: DatagramMsg) {
        match &*self.inner {
            Inner::Quic(q) => {
                if q.closing.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = q.conn.send_datagram(Bytes::from(encode_datagram(&m))) {
                    debug!(error = %e, "datagram dropped");
                }
            }
            Inner::Memory(mem) => {
                if !mem.closed.load(Ordering::Acquire) {
                    let _ = mem.peer.try_send(Inbound::Datagram(m));
                }
            }
        }
    }

    /// Graceful close: flushes and finishes both streams (bounded by
    /// [`GRACEFUL_CLOSE_TIMEOUT`]), then closes the connection with `code`.
    /// Later sends fail with [`LinkError::Closed`].
    pub fn close(&self, code: u32) {
        match &*self.inner {
            Inner::Quic(q) => q.close(code),
            Inner::Memory(mem) => mem.close(code),
        }
    }

    /// Immediate close; queued messages may be lost.
    pub fn close_now(&self, code: u32) {
        match &*self.inner {
            Inner::Quic(q) => q.close_now(code),
            Inner::Memory(mem) => mem.close(code),
        }
    }

    pub fn is_closed(&self) -> bool {
        match &*self.inner {
            Inner::Quic(q) => q.closing.load(Ordering::Acquire) || q.conn.close_reason().is_some(),
            Inner::Memory(mem) => mem.closed.load(Ordering::Acquire),
        }
    }

    /// Waits until the connection is closed and the close has been sent to
    /// the peer (bounded by [`DRAIN_TIMEOUT`]). Call before exiting a
    /// process so the peer sees an orderly close instead of a timeout.
    pub async fn wait_closed(&self) {
        if let Inner::Quic(q) = &*self.inner {
            q.conn.closed().await;
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, q.endpoint.wait_idle()).await;
        }
    }
}

struct QuicShared {
    conn: Connection,
    /// Kept so the endpoint outlives every user of the connection.
    endpoint: Endpoint,
    control: mpsc::Sender<Bytes>,
    input: mpsc::Sender<Bytes>,
    closing: AtomicBool,
    finish: watch::Sender<bool>,
    writers: Mutex<Vec<JoinHandle<()>>>,
    reason: Reason,
    runtime: tokio::runtime::Handle,
}

impl QuicShared {
    fn send_framed(&self, queue: &mpsc::Sender<Bytes>, body: &[u8]) -> Result<(), LinkError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(LinkError::Closed);
        }
        match queue.try_send(frame::encode(body)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                warn!("send queue overflow; closing the link");
                self.close_now(codes::INTERNAL);
                Err(LinkError::Overflow)
            }
            Err(TrySendError::Closed(_)) => Err(LinkError::Closed),
        }
    }

    fn send_clipboard(&self, m: &ClipboardMsg) -> Result<(), LinkError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(LinkError::Closed);
        }
        let body = encode_clipboard(m);
        if body.len() > MAX_CLIPBOARD_FRAME {
            return Err(LinkError::TooLarge);
        }
        let conn = self.conn.clone();
        self.runtime.spawn(async move {
            if let Err(e) = write_clipboard(&conn, &body).await {
                debug!(error = %e, "clipboard transfer failed");
            }
        });
        Ok(())
    }

    fn close(&self, code: u32) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        set_reason(
            &self.reason,
            LinkClosed::LocalClosed {
                code: u64::from(code),
            },
        );
        let _ = self.finish.send(true);
        let writers = std::mem::take(&mut *lock(&self.writers));
        let conn = self.conn.clone();
        self.runtime.spawn(async move {
            let flush = async {
                for w in writers {
                    let _ = w.await;
                }
            };
            if tokio::time::timeout(GRACEFUL_CLOSE_TIMEOUT, flush)
                .await
                .is_err()
            {
                debug!("graceful close timed out; closing anyway");
            }
            conn.close(VarInt::from_u32(code), b"");
        });
    }

    fn close_now(&self, code: u32) {
        self.closing.store(true, Ordering::Release);
        set_reason(
            &self.reason,
            LinkClosed::LocalClosed {
                code: u64::from(code),
            },
        );
        self.conn.close(VarInt::from_u32(code), b"");
    }
}

impl Drop for QuicShared {
    fn drop(&mut self) {
        // Every sender is gone: nobody can use the connection any more.
        if !self.closing.load(Ordering::Acquire) {
            self.close_now(codes::NORMAL);
        }
    }
}

struct MemoryShared {
    peer: mpsc::Sender<Inbound>,
    own: mpsc::Sender<Inbound>,
    peer_files: mpsc::Sender<IncomingFiles>,
    closed: Arc<AtomicBool>,
}

impl MemoryShared {
    fn send(&self, m: Inbound) -> Result<(), LinkError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(LinkError::Closed);
        }
        match self.peer.try_send(m) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.close(codes::INTERNAL);
                Err(LinkError::Overflow)
            }
            Err(TrySendError::Closed(_)) => Err(LinkError::Closed),
        }
    }

    fn close(&self, code: u32) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let code = u64::from(code);
            let _ = self
                .peer
                .try_send(Inbound::Closed(LinkClosed::PeerClosed { code }));
            let _ = self
                .own
                .try_send(Inbound::Closed(LinkClosed::LocalClosed { code }));
        }
    }
}

impl Link {
    /// Two connected in-process links (no QUIC), for driver unit tests.
    pub fn memory_pair() -> (Link, Link) {
        // One extra slot so the final Closed always fits.
        let (a_tx, a_rx) = mpsc::channel(INBOUND_CAPACITY + 1);
        let (b_tx, b_rx) = mpsc::channel(INBOUND_CAPACITY + 1);
        let (a_files_tx, a_files) = mpsc::channel(FILES_QUEUE);
        let (b_files_tx, b_files) = mpsc::channel(FILES_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        let a = Link {
            tx: LinkSender {
                inner: Arc::new(Inner::Memory(MemoryShared {
                    peer: b_tx.clone(),
                    own: a_tx.clone(),
                    peer_files: b_files_tx,
                    closed: closed.clone(),
                })),
            },
            rx: a_rx,
            files: a_files,
        };
        let b = Link {
            tx: LinkSender {
                inner: Arc::new(Inner::Memory(MemoryShared {
                    peer: a_tx,
                    own: b_tx,
                    peer_files: a_files_tx,
                    closed,
                })),
            },
            rx: b_rx,
            files: b_files,
        };
        (a, b)
    }
}

/// The streams of an established session, handed over by `session.rs`.
pub(crate) struct Streams {
    pub(crate) control_send: SendStream,
    pub(crate) control_recv: RecvStream,
    /// Bytes already read from the control stream past the Hello.
    pub(crate) control_buf: FrameBuffer,
    pub(crate) input_send: SendStream,
    pub(crate) input_recv: RecvStream,
}

/// Starts the QUIC tasks of an established session.
pub(crate) fn spawn_quic(endpoint: Endpoint, conn: Connection, streams: Streams) -> Link {
    let runtime = tokio::runtime::Handle::current();
    let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
    let (files_tx, files_rx) = mpsc::channel(FILES_QUEUE);
    let (control_tx, control_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (input_tx, input_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (finish_tx, finish_rx) = watch::channel(false);
    let reason: Reason = Arc::new(Mutex::new(None));
    let writers = vec![
        runtime.spawn(writer(streams.control_send, control_rx, finish_rx.clone())),
        runtime.spawn(writer(streams.input_send, input_rx, finish_rx)),
    ];
    let readers = vec![
        runtime.spawn(stream_reader(
            streams.control_recv,
            streams.control_buf,
            Channel::Control,
            inbound_tx.clone(),
            conn.clone(),
            reason.clone(),
        )),
        runtime.spawn(stream_reader(
            streams.input_recv,
            FrameBuffer::new(),
            Channel::Input,
            inbound_tx.clone(),
            conn.clone(),
            reason.clone(),
        )),
        runtime.spawn(datagram_reader(
            conn.clone(),
            inbound_tx.clone(),
            reason.clone(),
        )),
        runtime.spawn(uni_acceptor(
            conn.clone(),
            inbound_tx.clone(),
            files_tx,
            reason.clone(),
        )),
    ];
    runtime.spawn(supervisor(
        conn.clone(),
        readers,
        inbound_tx,
        reason.clone(),
    ));
    Link {
        tx: LinkSender {
            inner: Arc::new(Inner::Quic(QuicShared {
                conn,
                endpoint,
                control: control_tx,
                input: input_tx,
                closing: AtomicBool::new(false),
                finish: finish_tx,
                writers: Mutex::new(writers),
                reason,
                runtime,
            })),
        },
        rx: inbound_rx,
        files: files_rx,
    }
}

#[derive(Debug, Clone, Copy)]
enum Channel {
    Control,
    Input,
}

fn decode(ch: Channel, body: &[u8]) -> Result<Inbound, ProtoError> {
    match ch {
        Channel::Control => decode_control(body).map(Inbound::Control),
        Channel::Input => decode_input(body).map(Inbound::Input),
    }
}

fn violation(conn: &Connection, reason: &Mutex<Option<LinkClosed>>, msg: String) {
    warn!(%msg, "peer protocol violation; closing the link");
    set_reason(reason, LinkClosed::ProtocolViolation(msg));
    conn.close(
        VarInt::from_u32(codes::PROTOCOL_VIOLATION),
        b"protocol violation",
    );
}

fn violation_if_open(conn: &Connection, reason: &Reason, msg: String) {
    if conn.close_reason().is_none() {
        violation(conn, reason, msg);
    }
}

async fn required_stream_ended(
    conn: &Connection,
    reason: &Reason,
    channel: Channel,
    detail: String,
    allow_graceful_close: bool,
) {
    if conn.close_reason().is_some() {
        return;
    }
    if allow_graceful_close {
        // A graceful close finishes both streams before closing the connection.
        let grace = GRACEFUL_CLOSE_TIMEOUT + Duration::from_millis(250);
        if tokio::time::timeout(grace, conn.closed()).await.is_ok() {
            return;
        }
    }
    violation_if_open(conn, reason, format!("{channel:?} stream {detail}"));
}

async fn writer(
    mut send: SendStream,
    mut rx: mpsc::Receiver<Bytes>,
    mut finish: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(bytes) => {
                    if send.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                None => break,
            },
            // The async block drops the watch guard (which is !Send) before
            // the branch completes.
            () = async { let _ = finish.wait_for(|f| *f).await; } => break,
        }
    }
    // Graceful close: flush what is queued, finish, wait for the peer's ACK.
    while let Ok(bytes) = rx.try_recv() {
        if send.write_all(&bytes).await.is_err() {
            return;
        }
    }
    if send.finish().is_ok() {
        let _ = send.stopped().await;
    }
}

async fn stream_reader(
    mut recv: RecvStream,
    mut buf: FrameBuffer,
    ch: Channel,
    tx: mpsc::Sender<Inbound>,
    conn: Connection,
    reason: Reason,
) {
    loop {
        loop {
            match buf.next_frame() {
                Ok(Some(body)) => match decode(ch, &body) {
                    Ok(msg) => {
                        if tx.send(msg).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        violation(&conn, &reason, format!("{ch:?} frame: {e}"));
                        return;
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    violation(&conn, &reason, format!("{ch:?} framing: {e}"));
                    return;
                }
            }
        }
        match recv.read_chunk(READ_CHUNK, true).await {
            Ok(Some(chunk)) => buf.push(&chunk.bytes),
            Ok(None) => {
                debug!(?ch, "peer finished its stream");
                let buffered = buf.buffered();
                let (detail, allow_graceful_close) = if buffered == 0 {
                    ("ended before connection close".to_string(), true)
                } else {
                    (
                        format!("ended with an incomplete frame ({buffered} buffered bytes)"),
                        false,
                    )
                };
                required_stream_ended(&conn, &reason, ch, detail, allow_graceful_close).await;
                return;
            }
            Err(e) => {
                debug!(?ch, error = %e, "stream read ended");
                required_stream_ended(&conn, &reason, ch, format!("read failed: {e}"), false).await;
                return;
            }
        }
    }
}

async fn datagram_reader(conn: Connection, tx: mpsc::Sender<Inbound>, reason: Reason) {
    loop {
        let Ok(bytes) = conn.read_datagram().await else {
            return;
        };
        match decode_datagram(&bytes) {
            Ok(m) => {
                if tx.send(Inbound::Datagram(m)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                violation(&conn, &reason, format!("datagram: {e}"));
                return;
            }
        }
    }
}

async fn write_clipboard(conn: &Connection, body: &[u8]) -> Result<(), String> {
    let mut send = conn.open_uni().await.map_err(|e| e.to_string())?;
    let _ = send.set_priority(CLIPBOARD_PRIORITY);
    send.write_all(&[STREAM_CLIPBOARD])
        .await
        .map_err(|e| e.to_string())?;
    send.write_all(body).await.map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())
}

/// Accepts the peer's unidirectional streams and reads each in its own
/// task. Returns once the connection is closed and every reader is done.
async fn uni_acceptor(
    conn: Connection,
    tx: mpsc::Sender<Inbound>,
    files: mpsc::Sender<IncomingFiles>,
    reason: Reason,
) {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            accepted = conn.accept_uni() => match accepted {
                Ok(recv) => {
                    readers.spawn(uni_reader(
                        recv,
                        conn.clone(),
                        tx.clone(),
                        files.clone(),
                        reason.clone(),
                    ));
                }
                Err(_) => break,
            },
            Some(_) = readers.join_next(), if !readers.is_empty() => {}
        }
    }
    while readers.join_next().await.is_some() {}
}

async fn uni_reader(
    mut recv: RecvStream,
    conn: Connection,
    tx: mpsc::Sender<Inbound>,
    files: mpsc::Sender<IncomingFiles>,
    reason: Reason,
) {
    let mut kind = [0u8; 1];
    match tokio::time::timeout(FILES_HEADER_TIMEOUT, recv.read_exact(&mut kind)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            debug!(error = %e, "unidirectional stream ended early");
            return;
        }
        Err(_) => {
            let _ = recv.stop(VarInt::from_u32(codes::NORMAL));
            return;
        }
    }
    match kind[0] {
        STREAM_CLIPBOARD => read_clipboard(recv, &conn, &tx, &reason).await,
        STREAM_FILES => read_files_header(recv, &conn, &files, &reason).await,
        other => violation(
            &conn,
            &reason,
            format!("unknown unidirectional stream type {other:#04x}"),
        ),
    }
}

async fn read_clipboard(
    mut recv: RecvStream,
    conn: &Connection,
    tx: &mpsc::Sender<Inbound>,
    reason: &Reason,
) {
    let read = tokio::time::timeout(
        CLIPBOARD_READ_TIMEOUT,
        recv.read_to_end(MAX_CLIPBOARD_FRAME),
    )
    .await;
    let body = match read {
        Ok(Ok(data)) => data,
        Ok(Err(ReadToEndError::TooLong)) => {
            violation(conn, reason, "clipboard stream over the limit".into());
            return;
        }
        Ok(Err(e)) => {
            debug!(error = %e, "clipboard stream ended early");
            return;
        }
        Err(_) => {
            debug!("clipboard stream timed out");
            let _ = recv.stop(VarInt::from_u32(codes::NORMAL));
            return;
        }
    };
    match decode_clipboard(&body) {
        Ok(m) => {
            let _ = tx.send(Inbound::Clipboard(m)).await;
        }
        Err(e) => violation(conn, reason, format!("clipboard frame: {e}")),
    }
}

/// Reads a protobuf varint length prefix.
async fn read_len(recv: &mut RecvStream) -> io::Result<u64> {
    let mut len: u64 = 0;
    for i in 0..10 {
        let mut byte = [0u8; 1];
        recv.read_exact(&mut byte).await.map_err(io::Error::other)?;
        len |= u64::from(byte[0] & 0x7F) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Ok(len);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "overlong varint",
    ))
}

/// Reads the header of a files stream and hands the rest to the driver.
async fn read_files_header(
    mut recv: RecvStream,
    conn: &Connection,
    files: &mpsc::Sender<IncomingFiles>,
    reason: &Reason,
) {
    let read = async {
        let len = read_len(&mut recv).await?;
        if len > MAX_FILES_HEADER as u64 {
            return Ok(Err("files header over the limit".to_string()));
        }
        // Grows as data arrives: a peer announcing 16 MiB on many streams
        // does not get that memory up front.
        let mut body = Vec::new();
        (&mut recv).take(len).read_to_end(&mut body).await?;
        if body.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the files header ended early",
            ));
        }
        io::Result::Ok(Ok(body))
    };
    let body = match tokio::time::timeout(FILES_HEADER_TIMEOUT, read).await {
        Ok(Ok(Ok(body))) => body,
        Ok(Ok(Err(why))) => {
            violation(conn, reason, why);
            return;
        }
        Ok(Err(e)) => {
            debug!(error = %e, "files stream ended in its header");
            return;
        }
        Err(_) => {
            debug!("files header timed out");
            let _ = recv.stop(VarInt::from_u32(codes::NORMAL));
            return;
        }
    };
    match decode_files_header(&body) {
        Ok(header) => {
            let incoming = IncomingFiles {
                header,
                body: Box::new(recv),
            };
            // A full queue or a gone driver drops the transfer, which stops
            // the sender.
            let _ = files.try_send(incoming);
        }
        Err(e) => violation(conn, reason, format!("files header: {e}")),
    }
}

fn map_close(e: &ConnectionError) -> LinkClosed {
    match e {
        ConnectionError::ApplicationClosed(c) => LinkClosed::PeerClosed {
            code: c.error_code.into_inner(),
        },
        ConnectionError::LocallyClosed => LinkClosed::LocalClosed {
            code: u64::from(codes::NORMAL),
        },
        ConnectionError::TimedOut => LinkClosed::TimedOut,
        other => LinkClosed::Transport(other.to_string()),
    }
}

/// Waits for the connection to close and for every reader to drain, then
/// reports exactly one `Closed`.
async fn supervisor(
    conn: Connection,
    readers: Vec<JoinHandle<()>>,
    tx: mpsc::Sender<Inbound>,
    reason: Reason,
) {
    let err = conn.closed().await;
    for r in readers {
        let _ = r.await;
    }
    let closed = lock(&reason).take().unwrap_or_else(|| map_close(&err));
    debug!(?closed, "link closed");
    let _ = tx.send(Inbound::Closed(closed)).await;
}

#[cfg(test)]
mod tests {
    use macwin_core::msg::{Leave, Ping};

    use super::*;

    #[tokio::test]
    async fn memory_pair_round_trip_and_single_close() {
        let (a, mut b) = Link::memory_pair();
        let ping = Ping {
            id: 1,
            sent_micros: 2,
        };
        a.tx.send_control(ControlMsg::Ping(ping)).unwrap();
        a.tx.send_input(InputMsg::Leave(Leave { epoch: 3 }))
            .unwrap();
        a.tx.send_datagram(DatagramMsg::Ping(ping));
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Control(ControlMsg::Ping(ping)))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Input(InputMsg::Leave(Leave { epoch: 3 })))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Datagram(DatagramMsg::Ping(ping)))
        );
        b.tx.close(codes::INTERNAL);
        b.tx.close(codes::NORMAL); // second close is a no-op
        let mut a = a;
        assert_eq!(
            a.rx.recv().await,
            Some(Inbound::Closed(LinkClosed::PeerClosed { code: 5 }))
        );
        assert_eq!(
            b.rx.recv().await,
            Some(Inbound::Closed(LinkClosed::LocalClosed { code: 5 }))
        );
        assert_eq!(
            a.tx.send_control(ControlMsg::Ping(ping)),
            Err(LinkError::Closed)
        );
        assert!(a.tx.is_closed() && b.tx.is_closed());
    }
}
