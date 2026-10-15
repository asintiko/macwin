//! Pairing: two machines that do not know each other's certificates yet
//! exchange them, authenticated by a short one-time code that the user
//! copies from one screen to the other.
//!
//! Protocol (ALPN `macwin-pair/1`, TLS 1.3 accepting any certificate):
//! 1. The dialer opens one bidirectional stream and both sides send a
//!    [`PairHello`] carrying their SPAKE2 message (Ed25519 group; idA / idB
//!    are the SHA-256 fingerprints of the dialer's / listener's TLS
//!    certificates, as each side sees them).
//! 2. Both derive the SPAKE2 key `k` and a transcript hash `T` over a TLS
//!    exporter value (binding everything to this TLS connection), both
//!    fingerprints and both hello frames.
//! 3. The dialer sends `HMAC-SHA256(k, "dialer" || T)`. The listener checks
//!    it, answers with `HMAC-SHA256(k, "listener" || T)`, the dialer checks
//!    that and sends `Done`.
//!
//! A man in the middle holds two TLS connections with different
//! certificates and exporters, so without the code it can produce neither
//! confirmation. Each attempt tests at most one guess of the code: SPAKE2
//! gives no offline dictionary attack, and the listener sends nothing
//! derived from `k` before the dialer has proven it knows the code. The
//! agent burns the code after every attempt that reached the exchange
//! ([`NetError::PairingFailed`]), so a 6-digit code leaves an attacker one
//! chance in a million per attempt.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use macwin_core::msg::{PairConfirm, PairHello, PairMsg};
use macwin_core::types::{Os, Side};
use macwin_proto::frame::{self, FrameBuffer};
use macwin_proto::{PROTOCOL_VERSION, check_versions, decode_pair, encode_pair};
use quinn::{Connection, ConnectionError, Endpoint, ReadError, RecvStream, SendStream, VarInt};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{digest, hmac};
use rustls::pki_types::CertificateDer;
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};
use tracing::{debug, info};

use crate::fingerprint::Fingerprint;
use crate::identity::{CERT_NAME, Identity};
use crate::{NetError, codes, tls};

/// Bound on the exchange once the TLS handshake is done.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
/// Digits in a pairing code.
pub const CODE_DIGITS: usize = 6;
const EXPORTER_LABEL: &[u8] = b"EXPORTER-macwin-pairing-v1";
const TRANSCRIPT_LABEL: &[u8] = b"macwin-pairing-v1";
/// How long the dialer waits for the listener's close after `Done`.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// Time allowed for a connecting peer to send its opening `PairHello`
/// before the connection is dropped without burning the pairing code.
const HELLO_WAIT: Duration = Duration::from_secs(5);

/// A one-time pairing code: six decimal digits.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);

impl PairingCode {
    /// A uniformly random code from the system RNG.
    pub fn generate() -> Result<PairingCode, NetError> {
        let rng = SystemRandom::new();
        // 4_294_000_000 is a multiple of 10^6 below 2^32: rejection sampling
        // keeps every code equally likely.
        loop {
            let mut b = [0u8; 4];
            rng.fill(&mut b)
                .map_err(|_| NetError::PairingFailed("no system randomness".into()))?;
            let v = u32::from_le_bytes(b);
            if v < 4_294_000_000 {
                return Ok(PairingCode(format!("{:06}", v % 1_000_000)));
            }
        }
    }

    /// Accepts the six digits with optional spaces or dashes ("123-456").
    pub fn parse(s: &str) -> Result<PairingCode, NetError> {
        let digits: String = s.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
        if digits.len() != CODE_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(NetError::InvalidCode(
                "expected 6 digits, for example 123-456",
            ));
        }
        Ok(PairingCode(digits))
    }

    pub fn digits(&self) -> &str {
        &self.0
    }
}

/// Shown to users as `123-456`.
impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", &self.0[..3], &self.0[3..])
    }
}

/// Never prints the digits.
impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(******)")
    }
}

impl std::str::FromStr for PairingCode {
    type Err = NetError;

    fn from_str(s: &str) -> Result<PairingCode, NetError> {
        PairingCode::parse(s)
    }
}

/// What this machine tells the other one about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairInfo {
    pub os: Os,
    pub device_name: String,
    /// "My peer is to my left/right", if the user chose it.
    pub peer_side: Option<Side>,
}

/// The result of a successful pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    /// The peer's certificate fingerprint, authenticated by the code.
    pub fingerprint: Fingerprint,
    pub os: Os,
    pub device_name: String,
    /// The peer's "my peer is to my left/right", if it chose one.
    pub peer_side: Option<Side>,
    pub remote: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Dialer,
    Listener,
}

/// A bound pairing listener.
pub struct PairListener {
    endpoint: Endpoint,
    id: Identity,
}

/// Binds a pairing listener on `bind` presenting `id`.
pub async fn pair_listen(bind: SocketAddr, id: &Identity) -> Result<PairListener, NetError> {
    let endpoint = Endpoint::server(tls::pairing_server_config(id)?, bind)?;
    Ok(PairListener {
        endpoint,
        id: id.clone(),
    })
}

impl PairListener {
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Waits for the next pairing attempt and runs it with `code`.
    /// [`NetError::PairingFailed`] means the attempt used up the code; any
    /// other error (for example a failed TLS handshake) left it unused.
    pub async fn accept(
        &self,
        code: &PairingCode,
        info: &PairInfo,
    ) -> Result<PairedPeer, NetError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(NetError::EndpointClosed)?;
        let conn = incoming.await?;
        debug!(remote = %conn.remote_address(), "pairing attempt");
        let result = run(&conn, Role::Listener, &self.id, code, info).await;
        if result.is_err() {
            conn.close(VarInt::from_u32(codes::PAIRING_FAILED), b"pairing failed");
        }
        result
    }

    /// Stops listening and lets pending closes reach their peers.
    pub async fn shutdown(self) {
        self.endpoint.close(VarInt::from_u32(codes::NORMAL), b"");
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

/// Dials a pairing listener at `remote` from a socket bound to `bind`.
pub async fn pair_connect(
    bind: SocketAddr,
    remote: SocketAddr,
    id: &Identity,
    code: &PairingCode,
    info: &PairInfo,
) -> Result<PairedPeer, NetError> {
    let endpoint = Endpoint::client(bind)?;
    let conn = endpoint
        .connect_with(tls::pairing_client_config(id)?, remote, CERT_NAME)?
        .await?;
    let result = run(&conn, Role::Dialer, id, code, info).await;
    if result.is_err() {
        conn.close(VarInt::from_u32(codes::PAIRING_FAILED), b"pairing failed");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    result
}

/// The exchange with its time bound.
async fn run(
    conn: &Connection,
    role: Role,
    id: &Identity,
    code: &PairingCode,
    info: &PairInfo,
) -> Result<PairedPeer, NetError> {
    match tokio::time::timeout(PAIR_TIMEOUT, exchange(conn, role, id, code, info)).await {
        Ok(r) => r,
        Err(_) => Err(NetError::PairingFailed(
            "the other machine did not finish in time".into(),
        )),
    }
}

fn peer_fingerprint(conn: &Connection) -> Result<Fingerprint, NetError> {
    let certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or(NetError::StreamSetup("the peer presented no certificate"))?;
    let first = certs
        .first()
        .ok_or(NetError::StreamSetup("the peer presented no certificate"))?;
    Ok(Fingerprint::of_cert(first.as_ref()))
}

/// Maps the peer closing with `PAIRING_FAILED` to the error users expect.
fn conn_err(e: ConnectionError) -> NetError {
    match &e {
        ConnectionError::ApplicationClosed(c)
            if c.error_code == VarInt::from_u32(codes::PAIRING_FAILED) =>
        {
            NetError::PairingFailed("the other machine rejected the code".into())
        }
        _ => NetError::Connection(e),
    }
}

async fn write_msg(send: &mut SendStream, m: &PairMsg) -> Result<Vec<u8>, NetError> {
    let body = encode_pair(m);
    send.write_all(&frame::encode(&body))
        .await
        .map_err(|e| match e {
            quinn::WriteError::ConnectionLost(c) => conn_err(c),
            _ => NetError::StreamSetup("pairing stream write failed"),
        })?;
    Ok(body)
}

async fn read_frame(recv: &mut RecvStream, buf: &mut FrameBuffer) -> Result<Bytes, NetError> {
    loop {
        if let Some(body) = buf.next_frame()? {
            return Ok(body);
        }
        match recv.read_chunk(4096, true).await {
            Ok(Some(chunk)) => buf.push(&chunk.bytes),
            Ok(None) => return Err(NetError::StreamSetup("the pairing stream ended early")),
            Err(ReadError::ConnectionLost(c)) => return Err(conn_err(c)),
            Err(_) => return Err(NetError::StreamSetup("the pairing stream failed")),
        }
    }
}

fn unexpected(what: &'static str) -> NetError {
    NetError::PairingFailed(format!("protocol error: expected {what}"))
}

/// SHA-256 over the exporter, both fingerprints and both hello frames.
fn transcript(
    exporter: &[u8; 32],
    fp_dialer: &Fingerprint,
    fp_listener: &Fingerprint,
    hello_dialer: &[u8],
    hello_listener: &[u8],
) -> digest::Digest {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(TRANSCRIPT_LABEL);
    ctx.update(exporter);
    ctx.update(&fp_dialer.0);
    ctx.update(&fp_listener.0);
    for hello in [hello_dialer, hello_listener] {
        ctx.update(&(hello.len() as u64).to_be_bytes());
        ctx.update(hello);
    }
    ctx.finish()
}

fn confirm_input(who: &[u8], t: &digest::Digest) -> Vec<u8> {
    [who, b"\0", t.as_ref()].concat()
}

async fn exchange(
    conn: &Connection,
    role: Role,
    id: &Identity,
    code: &PairingCode,
    info: &PairInfo,
) -> Result<PairedPeer, NetError> {
    let own_fp = id.fingerprint();
    let peer_fp = peer_fingerprint(conn)?;
    if peer_fp == own_fp {
        return Err(NetError::PairingFailed(
            "the other side has this machine's own identity".into(),
        ));
    }
    let (fp_dialer, fp_listener) = match role {
        Role::Dialer => (own_fp, peer_fp),
        Role::Listener => (peer_fp, own_fp),
    };
    let (mut send, mut recv) = match role {
        Role::Dialer => conn.open_bi().await.map_err(conn_err)?,
        Role::Listener => conn.accept_bi().await.map_err(conn_err)?,
    };
    let password = Password::new(code.digits().as_bytes());
    let id_a = SpakeIdentity::new(&fp_dialer.0);
    let id_b = SpakeIdentity::new(&fp_listener.0);
    let (spake, spake_msg) = match role {
        Role::Dialer => Spake2::<Ed25519Group>::start_a(&password, &id_a, &id_b),
        Role::Listener => Spake2::<Ed25519Group>::start_b(&password, &id_a, &id_b),
    };
    let hello = PairMsg::Hello(PairHello {
        protocol_version: PROTOCOL_VERSION,
        os: info.os,
        device_name: info.device_name.clone(),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        peer_side: info.peer_side,
        spake: spake_msg,
    });
    // From here on the attempt has used up the code, whatever happens.
    let used = |e: NetError| match e {
        NetError::PairingFailed(_) => e,
        other => NetError::PairingFailed(other.to_string()),
    };
    let mut buf = FrameBuffer::new();
    let (own_hello, peer_hello_body, peer_hello) = match role {
        Role::Dialer => {
            let own_hello = write_msg(&mut send, &hello).await?;
            let peer_hello_body = read_frame(&mut recv, &mut buf).await.map_err(used)?;
            let PairMsg::Hello(peer_hello) =
                decode_pair(&peer_hello_body).map_err(|e| used(e.into()))?
            else {
                return Err(unexpected("a pairing hello"));
            };
            check_versions(PROTOCOL_VERSION, peer_hello.protocol_version)
                .map_err(|e| used(e.into()))?;
            (own_hello, peer_hello_body, peer_hello)
        }
        Role::Listener => {
            // Wait for a valid PairHello from the dialer before committing our
            // SPAKE2 share or burning the one-time pairing code.
            let peer_hello_body =
                match tokio::time::timeout(HELLO_WAIT, read_frame(&mut recv, &mut buf)).await {
                    Ok(Ok(body)) => body,
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Err(NetError::StreamSetup("the peer sent no pairing hello")),
                };
            let PairMsg::Hello(peer_hello) = decode_pair(&peer_hello_body)
                .map_err(|_| NetError::StreamSetup("invalid pairing hello"))?
            else {
                return Err(NetError::StreamSetup("expected a pairing hello"));
            };
            check_versions(PROTOCOL_VERSION, peer_hello.protocol_version)?;
            let own_hello = write_msg(&mut send, &hello).await.map_err(used)?;
            (own_hello, peer_hello_body, peer_hello)
        }
    };
    let key = spake
        .finish(&peer_hello.spake)
        .map_err(|_| NetError::PairingFailed("invalid SPAKE2 message".into()))?;
    let mut exporter = [0u8; 32];
    conn.export_keying_material(&mut exporter, EXPORTER_LABEL, b"")
        .map_err(|_| NetError::PairingFailed("the TLS exporter is unavailable".into()))?;
    let (hello_dialer, hello_listener) = match role {
        Role::Dialer => (own_hello.as_slice(), &peer_hello_body[..]),
        Role::Listener => (&peer_hello_body[..], own_hello.as_slice()),
    };
    let t = transcript(
        &exporter,
        &fp_dialer,
        &fp_listener,
        hello_dialer,
        hello_listener,
    );
    let mac_key = hmac::Key::new(hmac::HMAC_SHA256, &key);
    let dialer_input = confirm_input(b"dialer", &t);
    let listener_input = confirm_input(b"listener", &t);
    let confirm = |input: &[u8]| {
        let tag = hmac::sign(&mac_key, input);
        let mut mac = [0u8; 32];
        mac.copy_from_slice(tag.as_ref());
        PairMsg::Confirm(PairConfirm { mac })
    };
    match role {
        Role::Dialer => {
            write_msg(&mut send, &confirm(&dialer_input))
                .await
                .map_err(used)?;
            let reply = read_frame(&mut recv, &mut buf).await.map_err(used)?;
            let PairMsg::Confirm(c) = decode_pair(&reply).map_err(|e| used(e.into()))? else {
                return Err(unexpected("a confirmation"));
            };
            hmac::verify(&mac_key, &listener_input, &c.mac).map_err(|_| {
                NetError::PairingFailed("the other machine's confirmation is invalid".into())
            })?;
            write_msg(&mut send, &PairMsg::Done).await.map_err(used)?;
            let _ = send.finish();
            // The listener closes once it has the Done.
            let _ = tokio::time::timeout(CLOSE_WAIT, conn.closed()).await;
        }
        Role::Listener => {
            let first = read_frame(&mut recv, &mut buf).await.map_err(used)?;
            let PairMsg::Confirm(c) = decode_pair(&first).map_err(|e| used(e.into()))? else {
                return Err(unexpected("a confirmation"));
            };
            hmac::verify(&mac_key, &dialer_input, &c.mac).map_err(|_| {
                NetError::PairingFailed("wrong code (or a device that does not know it)".into())
            })?;
            write_msg(&mut send, &confirm(&listener_input))
                .await
                .map_err(used)?;
            let done = read_frame(&mut recv, &mut buf).await.map_err(used)?;
            if decode_pair(&done).map_err(|e| used(e.into()))? != PairMsg::Done {
                return Err(unexpected("done"));
            }
            conn.close(VarInt::from_u32(codes::NORMAL), b"paired");
        }
    }
    let peer = PairedPeer {
        fingerprint: peer_fp,
        os: peer_hello.os,
        device_name: peer_hello.device_name,
        peer_side: peer_hello.peer_side,
        remote: conn.remote_address(),
    };
    info!(peer = %peer.device_name, fingerprint = %peer.fingerprint, ?role, "paired");
    Ok(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_parse_display_and_hide() {
        let c = PairingCode::parse("123-456").unwrap();
        assert_eq!(c.digits(), "123456");
        assert_eq!(c.to_string(), "123-456");
        assert_eq!(format!("{c:?}"), "PairingCode(******)");
        assert_eq!(PairingCode::parse(" 123 456 ").unwrap(), c);
        assert_eq!("123456".parse::<PairingCode>().unwrap(), c);
        for bad in ["", "12345", "1234567", "12a456", "123_456", "１２３４５６"] {
            assert!(PairingCode::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn generated_codes_are_six_digits_and_vary() {
        let codes: Vec<PairingCode> = (0..20).map(|_| PairingCode::generate().unwrap()).collect();
        for c in &codes {
            assert_eq!(c.digits().len(), 6);
            assert!(c.digits().bytes().all(|b| b.is_ascii_digit()));
        }
        assert!(codes.iter().any(|c| c != &codes[0]), "20 equal codes");
    }

    #[test]
    fn transcript_depends_on_every_input() {
        let e = [1u8; 32];
        let (a, b) = (Fingerprint([2; 32]), Fingerprint([3; 32]));
        let base = transcript(&e, &a, &b, b"hd", b"hl");
        let variants = [
            transcript(&[9; 32], &a, &b, b"hd", b"hl"),
            transcript(&e, &b, &a, b"hd", b"hl"),
            transcript(&e, &a, &b, b"hdx", b"hl"),
            transcript(&e, &a, &b, b"hd", b"hlx"),
            // Moving a byte between the hellos changes it too.
            transcript(&e, &a, &b, b"h", b"dhl"),
        ];
        for v in variants {
            assert_ne!(v.as_ref(), base.as_ref());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_drops_before_hello_does_not_burn_the_code() {
        let mac = Identity::generate().unwrap();
        let pc = Identity::generate().unwrap();
        let local: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let l = pair_listen(local, &pc).await.unwrap();
        let addr = l.local_addr().unwrap();
        let code = PairingCode::parse("123456").unwrap();
        let info = PairInfo {
            os: Os::Windows,
            device_name: "pc".into(),
            peer_side: None,
        };
        let server = tokio::spawn(async move {
            let r = l.accept(&code, &info).await;
            l.shutdown().await;
            r
        });
        let endpoint = Endpoint::client(local).unwrap();
        let conn = endpoint
            .connect_with(tls::pairing_client_config(&mac).unwrap(), addr, CERT_NAME)
            .unwrap()
            .await
            .unwrap();
        let (mut send, _recv) = conn.open_bi().await.unwrap();
        let _ = send.finish();
        conn.close(VarInt::from_u32(0), b"bye");
        let res = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&res, Err(e) if !matches!(e, NetError::PairingFailed(_))),
            "dropping before PairHello must not burn the code: {res:?}"
        );
    }
}
