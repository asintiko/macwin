//! `config.toml` in the config directory: this computer's role, the paired
//! peer, the screen arrangement and the settings of `macwin run`. Unknown
//! keys are errors (they are usually typos), and every value is validated
//! when the file is read.

use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use macwin_core::arrange::{Edge, Placement};
use macwin_core::types::{Os, Side};
use macwin_net::Fingerprint;
use macwin_net::discovery::DISCOVERY_PORT;
use macwin_platform::fs::{create_private_dir, write_atomic};
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "config.toml";
/// The UDP port macwin listens on by default.
pub const DEFAULT_PORT: u16 = 47800;
/// Default limit of shared clipboard texts.
pub const DEFAULT_CLIPBOARD_MAX_KB: u32 = 4096;
/// Largest configurable clipboard limit (the protocol's 16 MiB).
pub const MAX_CLIPBOARD_MAX_KB: u32 = (macwin_proto::MAX_CLIPBOARD_TEXT / 1024) as u32;

const HEADER: &str = "# macwin configuration. Change it with `macwin config set KEY VALUE`,\n\
                      # or edit it while macwin is not running.\n\n";

/// Default limit of shared copied files, in MiB.
pub const DEFAULT_FILES_MAX_MB: u32 = 512;
/// Largest configurable file limit (the protocol's 64 GiB).
pub const MAX_FILES_MAX_MB: u32 = 65_536;

/// Validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where this machine listens when it does not dial its peer.
    pub listen: SocketAddr,
    /// Swap Ctrl and Cmd/Win when controlling the other OS.
    pub remap: bool,
    /// Answer and send LAN discovery broadcasts (finding the machine that
    /// waits for pairing, and the peer after its address changed).
    pub discovery: bool,
    /// The UDP port of LAN discovery.
    pub discovery_port: u16,
    pub clipboard: ClipboardConfig,
    pub peer: Option<PeerConfig>,
    /// What this computer does in the pair; `None` until it is chosen (the
    /// command line never needs one).
    pub role: Option<Role>,
    /// Where the primary computer put the peer's screens next to its own.
    /// It outlives the pairing: a new peer takes the same place.
    pub arrangement: Option<Placement>,
    /// The language of the window and of the messages.
    pub language: Language,
}

/// What a computer does in the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Its keyboard and mouse control the other computer, and it owns the
    /// screen arrangement.
    Primary,
    /// It is controlled from the primary computer, and shows the pairing
    /// code.
    Secondary,
}

/// The language of the window and of the messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Language {
    /// Russian, the default.
    #[default]
    Ru,
    /// English.
    En,
}

/// A primary computer needs to know where the peer's screens are, and
/// neither an arrangement nor the legacy `side` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeedsPlacement;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardConfig {
    pub enabled: bool,
    /// Texts above this many KiB are not shared.
    pub max_text_kb: u32,
    /// Share images.
    pub images: bool,
    /// Share copied files.
    pub files: bool,
    /// Copied files above this many MiB in total are not shared.
    pub max_files_mb: u32,
}

impl ClipboardConfig {
    pub fn max_text_bytes(&self) -> usize {
        self.max_text_kb as usize * 1024
    }

    pub fn max_files_bytes(&self) -> u64 {
        u64::from(self.max_files_mb) * 1024 * 1024
    }
}

/// The paired machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerConfig {
    pub fingerprint: Fingerprint,
    pub name: String,
    pub os: Os,
    /// Where the peer's screens are ("the peer is to my left/right").
    pub side: Option<Side>,
    /// `HOST:PORT` this machine dials; `None`: this machine listens.
    pub address: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            remap: true,
            discovery: true,
            discovery_port: DISCOVERY_PORT,
            clipboard: ClipboardConfig {
                enabled: true,
                max_text_kb: DEFAULT_CLIPBOARD_MAX_KB,
                images: true,
                files: true,
                max_files_mb: DEFAULT_FILES_MAX_MB,
            },
            peer: None,
            role: None,
            arrangement: None,
            language: Language::default(),
        }
    }
}

// Plain values come before the tables: TOML needs them first.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    listen: String,
    remap: bool,
    discovery: bool,
    discovery_port: u16,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    clipboard: RawClipboard,
    #[serde(skip_serializing_if = "Option::is_none")]
    arrangement: Option<RawArrangement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<RawPeer>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArrangement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display: Option<u32>,
    edge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<i32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawClipboard {
    enabled: bool,
    max_text_kb: u32,
    images: bool,
    files: bool,
    max_files_mb: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPeer {
    fingerprint: String,
    name: String,
    os: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    side: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    address: Option<String>,
}

impl Default for RawConfig {
    fn default() -> RawConfig {
        RawConfig::from(&Config::default())
    }
}

impl Default for RawClipboard {
    fn default() -> RawClipboard {
        RawConfig::default().clipboard
    }
}

pub fn os_name(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Windows => "windows",
    }
}

pub fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

fn parse_os(s: &str) -> anyhow::Result<Os> {
    match s {
        "macos" => Ok(Os::MacOs),
        "windows" => Ok(Os::Windows),
        _ => bail!("peer.os: expected \"macos\" or \"windows\", got {s:?}"),
    }
}

pub fn parse_side(s: &str) -> anyhow::Result<Side> {
    match s {
        "left" => Ok(Side::Left),
        "right" => Ok(Side::Right),
        _ => bail!("expected \"left\" or \"right\", got {s:?}"),
    }
}

pub fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

pub fn parse_edge(s: &str) -> anyhow::Result<Edge> {
    match s {
        "left" => Ok(Edge::Left),
        "right" => Ok(Edge::Right),
        "top" => Ok(Edge::Top),
        "bottom" => Ok(Edge::Bottom),
        _ => bail!("expected \"left\", \"right\", \"top\" or \"bottom\", got {s:?}"),
    }
}

pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Primary => "primary",
        Role::Secondary => "secondary",
    }
}

pub fn parse_role(s: &str) -> anyhow::Result<Role> {
    match s {
        "primary" => Ok(Role::Primary),
        "secondary" => Ok(Role::Secondary),
        _ => bail!("expected \"primary\" or \"secondary\", got {s:?}"),
    }
}

pub fn language_name(language: Language) -> &'static str {
    match language {
        Language::Ru => "ru",
        Language::En => "en",
    }
}

pub fn parse_language(s: &str) -> anyhow::Result<Language> {
    match s {
        "ru" => Ok(Language::Ru),
        "en" => Ok(Language::En),
        _ => bail!("expected \"ru\" or \"en\", got {s:?}"),
    }
}

/// `HOST`, `HOST:PORT`, `IP`, `IP:PORT` or `[IPv6]:PORT`; adds the default
/// port when there is none.
pub fn normalize_address(s: &str) -> anyhow::Result<String> {
    let s = s.trim();
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT).to_string());
    }
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr.to_string());
    }
    let valid_host = |h: &str| {
        !h.is_empty()
            && h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    match s.rsplit_once(':') {
        None if valid_host(s) => Ok(format!("{s}:{DEFAULT_PORT}")),
        Some((host, port)) if valid_host(host) && port.parse::<u16>().is_ok_and(|p| p != 0) => {
            Ok(format!("{host}:{port}"))
        }
        _ => bail!("expected HOST[:PORT], got {s:?}"),
    }
}

fn check_max_kb(kb: u32) -> anyhow::Result<u32> {
    if (1..=MAX_CLIPBOARD_MAX_KB).contains(&kb) {
        Ok(kb)
    } else {
        bail!("clipboard.max_text_kb: expected 1..={MAX_CLIPBOARD_MAX_KB}, got {kb}")
    }
}

fn check_max_mb(mb: u32) -> anyhow::Result<u32> {
    if (1..=MAX_FILES_MAX_MB).contains(&mb) {
        Ok(mb)
    } else {
        bail!("clipboard.max_files_mb: expected 1..={MAX_FILES_MAX_MB}, got {mb}")
    }
}

fn check_port(port: u16) -> anyhow::Result<u16> {
    if port == 0 {
        bail!("discovery_port: expected 1..=65535, got 0")
    }
    Ok(port)
}

fn parse_bool(key: &str, v: &str) -> anyhow::Result<bool> {
    match v {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => bail!("{key}: expected on or off, got {v:?}"),
    }
}

impl From<&Config> for RawConfig {
    fn from(c: &Config) -> RawConfig {
        RawConfig {
            listen: c.listen.to_string(),
            remap: c.remap,
            discovery: c.discovery,
            discovery_port: c.discovery_port,
            language: language_name(c.language).into(),
            role: c.role.map(|r| role_name(r).into()),
            clipboard: RawClipboard {
                enabled: c.clipboard.enabled,
                max_text_kb: c.clipboard.max_text_kb,
                images: c.clipboard.images,
                files: c.clipboard.files,
                max_files_mb: c.clipboard.max_files_mb,
            },
            arrangement: c.arrangement.map(|p| RawArrangement {
                display: p.display,
                edge: edge_name(p.edge).into(),
                offset: p.offset,
            }),
            peer: c.peer.as_ref().map(|p| RawPeer {
                fingerprint: p.fingerprint.to_string(),
                name: p.name.clone(),
                os: os_name(p.os).into(),
                side: p.side.map(|s| side_name(s).into()),
                address: p.address.clone(),
            }),
        }
    }
}

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(r: RawConfig) -> anyhow::Result<Config> {
        let listen = r
            .listen
            .parse()
            .map_err(|_| anyhow!("listen: expected ADDR:PORT, got {:?}", r.listen))?;
        let peer = match r.peer {
            None => None,
            Some(p) => Some(PeerConfig {
                fingerprint: p
                    .fingerprint
                    .parse()
                    .map_err(|e| anyhow!("peer.fingerprint: {e}"))?,
                name: p.name,
                os: parse_os(&p.os)?,
                side: p
                    .side
                    .as_deref()
                    .map(parse_side)
                    .transpose()
                    .context("peer.side")?,
                address: p
                    .address
                    .as_deref()
                    .map(normalize_address)
                    .transpose()
                    .context("peer.address")?,
            }),
        };
        let arrangement = r
            .arrangement
            .map(|a| -> anyhow::Result<Placement> {
                Ok(Placement {
                    display: a.display,
                    edge: parse_edge(&a.edge).context("arrangement.edge")?,
                    offset: a.offset,
                })
            })
            .transpose()?;
        Ok(Config {
            listen,
            remap: r.remap,
            discovery: r.discovery,
            discovery_port: check_port(r.discovery_port)?,
            role: r
                .role
                .as_deref()
                .map(parse_role)
                .transpose()
                .context("role")?,
            arrangement,
            language: parse_language(&r.language).context("language")?,
            clipboard: ClipboardConfig {
                enabled: r.clipboard.enabled,
                max_text_kb: check_max_kb(r.clipboard.max_text_kb)?,
                images: r.clipboard.images,
                files: r.clipboard.files,
                max_files_mb: check_max_mb(r.clipboard.max_files_mb)?,
            },
            peer,
        })
    }
}

impl Config {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(CONFIG_FILE)
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Config> {
        let raw: RawConfig = toml::from_str(text)?;
        Config::try_from(raw)
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string(&RawConfig::from(self)).unwrap_or_default();
        format!("{HEADER}{body}")
    }

    /// The config in `dir`, or the defaults when there is no file yet.
    pub fn load(dir: &Path) -> anyhow::Result<Config> {
        let path = Config::path(dir);
        match fs::read_to_string(&path) {
            Ok(text) => {
                Config::from_toml(&text).with_context(|| format!("invalid {}", path.display()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Writes the config atomically.
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let path = Config::path(dir);
        create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        write_atomic(&path, self.to_toml().as_bytes(), false)
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// The placement of the peer's screens this machine announces to it: a
    /// primary's arrangement, else the legacy `peer.side`. A secondary
    /// announces none and follows the primary's (`Ok(None)`); a machine
    /// without a role uses the legacy side only.
    pub fn session_placement(&self) -> Result<Option<Placement>, NeedsPlacement> {
        let legacy = self.peer.as_ref().and_then(|p| p.side).map(Placement::side);
        match self.role {
            Some(Role::Secondary) => Ok(None),
            Some(Role::Primary) => self
                .arrangement
                .or(legacy)
                .map(Some)
                .ok_or(NeedsPlacement),
            None => legacy.map(Some).ok_or(NeedsPlacement),
        }
    }

    fn peer_mut(&mut self) -> anyhow::Result<&mut PeerConfig> {
        self.peer
            .as_mut()
            .ok_or_else(|| anyhow!("not paired yet: run `macwin pair` first"))
    }

    /// `macwin config set KEY VALUE`.
    pub fn set(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
        let none = matches!(value, "none" | "");
        match key {
            "side" => {
                self.peer_mut()?.side = if none {
                    None
                } else {
                    Some(parse_side(value).context("side")?)
                };
            }
            "address" => {
                self.peer_mut()?.address = if none {
                    None
                } else {
                    Some(normalize_address(value).context("address")?)
                };
            }
            "listen" => {
                self.listen = value
                    .parse()
                    .map_err(|_| anyhow!("listen: expected ADDR:PORT, got {value:?}"))?;
            }
            "remap" => self.remap = parse_bool(key, value)?,
            "discovery" => self.discovery = parse_bool(key, value)?,
            "discovery-port" => {
                let port = value
                    .parse()
                    .map_err(|_| anyhow!("discovery-port: expected 1-65535, got {value:?}"))?;
                self.discovery_port = check_port(port)?;
            }
            "clipboard" => self.clipboard.enabled = parse_bool(key, value)?,
            "clipboard-max-kb" => {
                let kb = value
                    .parse()
                    .map_err(|_| anyhow!("clipboard-max-kb: expected a number, got {value:?}"))?;
                self.clipboard.max_text_kb = check_max_kb(kb)?;
            }
            "clipboard-images" => self.clipboard.images = parse_bool(key, value)?,
            "clipboard-files" => self.clipboard.files = parse_bool(key, value)?,
            "clipboard-files-max-mb" => {
                let mb = value.parse().map_err(|_| {
                    anyhow!("clipboard-files-max-mb: expected a number, got {value:?}")
                })?;
                self.clipboard.max_files_mb = check_max_mb(mb)?;
            }
            "role" => {
                self.role = if none {
                    None
                } else {
                    Some(parse_role(value).context("role")?)
                };
            }
            "language" => self.language = parse_language(value).context("language")?,
            _ => bail!(
                "unknown key {key:?}; keys: side, address, listen, remap, discovery, \
                 discovery-port, clipboard, clipboard-max-kb, clipboard-images, \
                 clipboard-files, clipboard-files-max-mb, role, language"
            ),
        }
        Ok(())
    }
}

/// After the paired peer answered from `addr`: stores it as the peer's
/// address when the configured one is an IP address. Hostnames stay (they
/// may only be unresolvable for a while), and so does a config that was
/// re-paired meanwhile. Re-reads the file, so concurrent edits survive.
/// True when the file changed.
pub fn remember_peer_address(
    dir: &Path,
    peer: Fingerprint,
    addr: SocketAddr,
) -> anyhow::Result<bool> {
    let mut config = Config::load(dir)?;
    let Some(p) = config.peer.as_mut().filter(|p| p.fingerprint == peer) else {
        return Ok(false);
    };
    let new = addr.to_string();
    match &p.address {
        Some(a) if *a == new || a.parse::<SocketAddr>().is_err() => return Ok(false),
        None => return Ok(false),
        Some(_) => p.address = Some(new),
    }
    config.save(dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn paired() -> Config {
        Config {
            peer: Some(PeerConfig {
                fingerprint: FP.parse().unwrap(),
                name: "DESKTOP-1".into(),
                os: Os::Windows,
                side: Some(Side::Right),
                address: Some("192.168.1.20:47800".into()),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn defaults_when_empty_or_missing() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), Config::default());
        let d = Config::default();
        assert_eq!(d.listen.port(), DEFAULT_PORT);
        assert!(d.remap && d.clipboard.enabled && d.peer.is_none());
        assert!(d.discovery && d.discovery_port == DISCOVERY_PORT);
        assert_eq!(d.clipboard.max_text_bytes(), 4096 * 1024);
        assert_eq!(
            (d.role, d.arrangement, d.language),
            (None, None, Language::Ru)
        );
    }

    #[test]
    fn round_trip_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = paired();
        c.save(&dir.path().join("sub")).unwrap();
        let text = fs::read_to_string(dir.path().join("sub").join(CONFIG_FILE)).unwrap();
        assert!(text.starts_with("# macwin configuration"), "{text}");
        assert!(
            text.contains("[peer]") && text.contains("side = \"right\""),
            "{text}"
        );
        assert_eq!(Config::load(&dir.path().join("sub")).unwrap(), c);
        // Without a peer there is no [peer] table.
        assert!(!Config::default().to_toml().contains("[peer]"));
    }

    #[test]
    fn invalid_values_and_unknown_keys_are_errors() {
        for (text, needle) in [
            ("listen = \"nope\"", "listen"),
            ("lsiten = \"0.0.0.0:1\"", "unknown field"),
            ("[clipboard]\nmax_text_kb = 0", "max_text_kb"),
            ("discovery_port = 0", "discovery_port"),
            ("discovery_port = 70000", "discovery_port"),
            ("[clipboard]\nmax_text_kb = 99999", "max_text_kb"),
            ("[clipboard]\nenable = true", "unknown field"),
            (
                "[peer]\nfingerprint = \"xyz\"\nname = \"a\"\nos = \"windows\"",
                "fingerprint",
            ),
            (
                &format!("[peer]\nfingerprint = \"{FP}\"\nname = \"a\"\nos = \"linux\""),
                "peer.os",
            ),
            (
                &format!(
                    "[peer]\nfingerprint = \"{FP}\"\nname = \"a\"\nos = \"macos\"\nside = \"up\""
                ),
                "side",
            ),
            (
                &format!(
                    "[peer]\nfingerprint = \"{FP}\"\nname = \"a\"\nos = \"macos\"\naddress = \"a b\""
                ),
                "address",
            ),
            ("[peer]\nname = \"a\"", "fingerprint"),
            ("role = \"boss\"", "role"),
            ("language = \"de\"", "language"),
            ("[arrangement]\nedge = \"up\"", "arrangement.edge"),
            ("[arrangement]\ndisplay = 1", "missing field `edge`"),
            ("[arrangement]\nedge = \"left\"\nzone = 1", "unknown field"),
            ("[arrangement]\nedge = \"left\"\noffset = \"x\"", "offset"),
            ("[arrangement]\nedge = \"left\"\ndisplay = -1", "display"),
        ] {
            let err = format!("{:#}", Config::from_toml(text).unwrap_err());
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn addresses_are_normalized() {
        for (input, out) in [
            ("192.168.1.20", "192.168.1.20:47800"),
            ("192.168.1.20:5000", "192.168.1.20:5000"),
            ("pc.local", "pc.local:47800"),
            ("pc.local:5000", "pc.local:5000"),
            ("fe80::1", "[fe80::1]:47800"),
            ("[fe80::1]:5000", "[fe80::1]:5000"),
            (" DESKTOP-1 ", "DESKTOP-1:47800"),
        ] {
            assert_eq!(normalize_address(input).unwrap(), out, "{input}");
        }
        for bad in [
            "",
            "a b",
            "host:0",
            "host:99999",
            "host:x",
            ":5000",
            "a:b:c",
        ] {
            assert!(normalize_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn found_addresses_replace_ip_addresses_only() {
        let dir = tempfile::tempdir().unwrap();
        let fp: Fingerprint = FP.parse().unwrap();
        let new: SocketAddr = "192.168.1.77:47800".parse().unwrap();
        // Not paired, or paired with another machine: nothing changes.
        assert!(!remember_peer_address(dir.path(), fp, new).unwrap());
        let mut other = paired();
        other.peer.as_mut().unwrap().fingerprint = Fingerprint([9; 32]);
        other.save(dir.path()).unwrap();
        assert!(!remember_peer_address(dir.path(), fp, new).unwrap());
        // An IP address is replaced, then nothing is left to change.
        paired().save(dir.path()).unwrap();
        assert!(remember_peer_address(dir.path(), fp, new).unwrap());
        let peer = Config::load(dir.path()).unwrap().peer.unwrap();
        assert_eq!(peer.address.as_deref(), Some("192.168.1.77:47800"));
        assert!(!remember_peer_address(dir.path(), fp, new).unwrap());
        // A hostname stays, and a listening machine does not start dialing.
        for address in [Some("pc.local:47800".to_string()), None] {
            let mut c = paired();
            c.peer.as_mut().unwrap().address = address.clone();
            c.save(dir.path()).unwrap();
            assert!(!remember_peer_address(dir.path(), fp, new).unwrap());
            assert_eq!(
                Config::load(dir.path()).unwrap().peer.unwrap().address,
                address
            );
        }
    }

    #[test]
    fn set_updates_and_validates() {
        let mut c = Config::default();
        assert!(
            c.set("side", "left")
                .unwrap_err()
                .to_string()
                .contains("not paired")
        );
        c.set("listen", "0.0.0.0:5000").unwrap();
        c.set("remap", "off").unwrap();
        c.set("clipboard", "off").unwrap();
        c.set("clipboard-max-kb", "64").unwrap();
        assert_eq!(c.listen.port(), 5000);
        assert!(!c.remap && !c.clipboard.enabled);
        assert_eq!(c.clipboard.max_text_kb, 64);
        assert!(c.set("clipboard-max-kb", "0").is_err());
        c.set("clipboard-images", "off").unwrap();
        c.set("clipboard-files", "off").unwrap();
        c.set("clipboard-files-max-mb", "100").unwrap();
        assert!(!c.clipboard.images && !c.clipboard.files);
        assert_eq!(c.clipboard.max_files_bytes(), 100 * 1024 * 1024);
        assert!(c.set("clipboard-files-max-mb", "0").is_err());
        assert!(c.set("clipboard-files-max-mb", "70000").is_err());
        assert!(c.set("remap", "maybe").is_err());
        c.set("discovery", "off").unwrap();
        c.set("discovery-port", "5001").unwrap();
        assert!(!c.discovery && c.discovery_port == 5001);
        assert!(c.set("discovery-port", "0").is_err());
        assert!(c.set("discovery-port", "x").is_err());
        assert!(c.set("colour", "red").is_err());
        let mut p = paired();
        p.set("side", "none").unwrap();
        p.set("address", "pc.local").unwrap();
        let peer = p.peer.as_ref().unwrap();
        assert_eq!(peer.side, None);
        assert_eq!(peer.address.as_deref(), Some("pc.local:47800"));
        p.set("address", "none").unwrap();
        assert_eq!(p.peer.unwrap().address, None);
    }

    fn arranged() -> Placement {
        Placement {
            display: Some(2),
            edge: Edge::Top,
            offset: Some(-30),
        }
    }

    #[test]
    fn files_without_the_new_keys_still_load() {
        let text = format!(
            "listen = \"0.0.0.0:47800\"\nremap = false\n\n[peer]\nfingerprint = \"{FP}\"\n\
             name = \"a\"\nos = \"macos\"\nside = \"left\"\n"
        );
        let c = Config::from_toml(&text).unwrap();
        assert_eq!(
            (c.role, c.arrangement, c.language),
            (None, None, Language::Ru)
        );
        assert!(!c.remap);
        assert_eq!(c.peer.unwrap().side, Some(Side::Left));
        // Saving a legacy config adds the language and nothing else.
        let text = Config::default().to_toml();
        assert!(text.contains("language = \"ru\""), "{text}");
        assert!(!text.contains("role") && !text.contains("arrangement"), "{text}");
    }

    #[test]
    fn roles_language_and_arrangement_round_trip() {
        let mut c = paired();
        c.role = Some(Role::Primary);
        c.language = Language::En;
        c.arrangement = Some(arranged());
        let text = c.to_toml();
        for needle in [
            "role = \"primary\"",
            "language = \"en\"",
            "[arrangement]",
            "display = 2",
            "edge = \"top\"",
            "offset = -30",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert_eq!(Config::from_toml(&text).unwrap(), c);
        let dir = tempfile::tempdir().unwrap();
        c.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), c);

        // Every part but the edge is optional.
        let c = Config::from_toml("role = \"secondary\"\n[arrangement]\nedge = \"left\"").unwrap();
        assert_eq!(c.role, Some(Role::Secondary));
        assert_eq!(c.arrangement, Some(Placement::side(Side::Left)));
        let text = c.to_toml();
        assert!(
            text.contains("edge = \"left\"") && !text.contains("display") && !text.contains("offset"),
            "{text}"
        );
        for edge in Edge::ALL {
            assert_eq!(parse_edge(edge_name(edge)).unwrap(), edge);
        }
    }

    #[test]
    fn the_arrangement_outlives_the_peer() {
        let mut c = paired();
        c.arrangement = Some(arranged());
        c.peer = None;
        let back = Config::from_toml(&c.to_toml()).unwrap();
        assert_eq!(back.arrangement, Some(arranged()));
        assert!(back.peer.is_none());
    }

    #[test]
    fn the_session_placement_depends_on_the_role() {
        let legacy = Placement::side(Side::Right);
        let mut c = paired();
        c.arrangement = Some(arranged());
        // Without a role only the legacy side counts.
        assert_eq!(c.session_placement(), Ok(Some(legacy)));
        c.peer.as_mut().unwrap().side = None;
        assert_eq!(c.session_placement(), Err(NeedsPlacement));
        // A primary: the arrangement, else the legacy side, else it has to ask.
        c.role = Some(Role::Primary);
        assert_eq!(c.session_placement(), Ok(Some(arranged())));
        c.arrangement = None;
        assert_eq!(c.session_placement(), Err(NeedsPlacement));
        c.peer.as_mut().unwrap().side = Some(Side::Right);
        assert_eq!(c.session_placement(), Ok(Some(legacy)));
        // A secondary places nothing, whatever the file says.
        c.role = Some(Role::Secondary);
        c.arrangement = Some(arranged());
        assert_eq!(c.session_placement(), Ok(None));
        c.peer = None;
        assert_eq!(c.session_placement(), Ok(None));
    }

    #[test]
    fn set_changes_the_role_and_the_language() {
        let mut c = Config::default();
        c.set("role", "primary").unwrap();
        assert_eq!(c.role, Some(Role::Primary));
        c.set("role", "secondary").unwrap();
        assert_eq!(c.role, Some(Role::Secondary));
        c.set("role", "none").unwrap();
        assert_eq!(c.role, None);
        c.set("role", "primary").unwrap();
        c.set("role", "").unwrap();
        assert_eq!(c.role, None);
        let err = format!("{:#}", c.set("role", "boss").unwrap_err());
        assert!(err.contains("role") && err.contains("primary"), "{err}");
        c.set("language", "en").unwrap();
        assert_eq!(c.language, Language::En);
        c.set("language", "ru").unwrap();
        assert_eq!(c.language, Language::Ru);
        assert!(c.set("language", "de").is_err());
        assert!(c.set("language", "none").is_err(), "a language always applies");
        let err = c.set("colour", "red").unwrap_err().to_string();
        assert!(err.contains("role, language"), "{err}");
    }
}
