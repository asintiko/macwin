//! Implementations of the `macwin` subcommands.

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use macwin_core::arrange::Placement;
use macwin_core::engine::{Timings, TransportRole};
use macwin_core::keymap;
use macwin_core::types::{
    CaptureFault, CapturedEvent, GrabCommand, Hid, InjectCommand, Point, ScreenInfo, Side,
};
use macwin_net::discovery::{DISCOVERY_WAIT, responder_addr};
use macwin_net::pairing::{PairInfo, PairingCode};
use macwin_net::{Fingerprint, Identity, NetError, Session, config_dir, connect, listen};
use macwin_platform::{CaptureSink, ClipboardInfo};
use tracing::{error, info, warn};

use crate::autostart::{self, AutostartState};
use crate::cli::{AutostartArg, PairArgs, SpikeCaptureArgs, SpikeKvmArgs, SpikeNetArgs};
use crate::config::{
    Config, edge_name, normalize_address, os_name, remember_peer_address, side_name,
};
use crate::driver::{
    ClipboardSettings, Connected, SessionConfig, SessionEnd, kvm_hello, spawn_session,
};
use crate::identity_store::{IDENTITY_SECRET, IdentityOrigin, KeyStoreKind, load_or_create};
use crate::lan::{discovery_targets, reachable_ip};
use crate::pairing::{
    agree_side, find_waiting, pairing_responder, store_pairing, wait_for_pairing,
};
use crate::runner::{self, Mode, RunDiscovery, RunEvent, RunExit, RunOptions, bind_for, resolve};
use crate::spike_net::{self, SpikeOpts, spike_hello};
use crate::tray::{self, DiscoveryTargets, MakeBackends, TrayEnv};
use crate::window::WindowBackend;
use crate::{device_name, host_os, platform};
use crate::{instance, logging};

/// The emergency chord, as shown to users.
pub const EMERGENCY_CHORD: &str = "Ctrl+Alt+Shift+Esc (Control+Option+Shift+Esc on a Mac)";
/// This machine's identity and where it came from.
pub struct LoadedIdentity {
    pub identity: Identity,
    pub config_dir: PathBuf,
    /// Where the key is kept, for users.
    pub store: String,
}

/// Loads (or creates, or migrates) this machine's identity.
pub fn load_identity() -> anyhow::Result<LoadedIdentity> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let kind = KeyStoreKind::from_env(|k| std::env::var_os(k))?;
    let store = platform::secret_store(kind, &dir)
        .with_context(|| format!("the {} key store is unavailable", kind.name()))?;
    let (identity, origin) = load_or_create(store.as_ref(), &dir)?;
    let where_ = store.describe(IDENTITY_SECRET);
    match origin {
        IdentityOrigin::Loaded => {}
        IdentityOrigin::Created => eprintln!("created a new identity in {where_}"),
        IdentityOrigin::Migrated => {
            eprintln!("moved the identity from {} into {where_}", dir.display())
        }
    }
    Ok(LoadedIdentity {
        identity,
        config_dir: dir,
        store: where_,
    })
}

pub fn identity() -> anyhow::Result<()> {
    let id = load_identity()?;
    println!("fingerprint: {}", id.identity.fingerprint());
    println!("key: {}", id.store);
    println!("config dir: {}", id.config_dir.display());
    Ok(())
}

fn supported_os() -> anyhow::Result<macwin_core::types::Os> {
    host_os().ok_or_else(|| anyhow!("unsupported OS: macwin runs on macOS and Windows"))
}

// ---------- pairing and configuration ----------

/// `discoverable`: the other machine can find this one on the LAN.
fn show_code(code: &PairingCode, local: SocketAddr, discoverable: bool) {
    let host = match reachable_ip(local) {
        Some(ip) => SocketAddr::new(ip, local.port()).to_string(),
        None => format!("<this machine's IP>:{}", local.port()),
    };
    let digits = code.digits();
    println!("pairing code: {code}");
    if discoverable {
        println!("on the other machine run:\n  macwin pair --code {digits}");
        println!("or, if it does not find this machine:\n  macwin pair {host} --code {digits}");
    } else {
        println!("on the other machine run:\n  macwin pair {host} --code {digits}");
    }
    let _ = std::io::stdout().flush();
}

fn prompt_code() -> anyhow::Result<PairingCode> {
    if !std::io::stdin().is_terminal() {
        bail!("pass the code shown on the other machine with --code");
    }
    print!("code shown on the other machine: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(PairingCode::parse(&line)?)
}

pub async fn pair(args: PairArgs) -> anyhow::Result<()> {
    let os = supported_os()?;
    let id = load_identity()?;
    let config = Config::load(&id.config_dir)?;
    let info = PairInfo {
        os,
        device_name: device_name(),
        peer_side: args.side.map(Side::from),
    };
    let (peer, address) = match (args.address, args.code) {
        (Some(target), code) => {
            let target = normalize_address(&target)?;
            let code = match code {
                Some(c) => PairingCode::parse(&c)?,
                None => prompt_code()?,
            };
            let remote = resolve(&target).await?;
            println!("pairing with {target}");
            let peer =
                macwin_net::pair_connect(bind_for(remote), remote, &id.identity, &code, &info)
                    .await
                    .with_context(|| format!("pairing with {target} failed"))?;
            (peer, Some(target))
        }
        (None, Some(code)) => {
            let code = PairingCode::parse(&code)?;
            println!("looking for the other machine on the local network");
            let targets = discovery_targets(config.discovery_port);
            let found = find_waiting(&config, id.identity.fingerprint(), &targets).await?;
            println!(
                "found \"{}\" ({}) at {}",
                found.device_name,
                os_name(found.os),
                found.addr
            );
            let peer = macwin_net::pair_connect(
                bind_for(found.addr),
                found.addr,
                &id.identity,
                &code,
                &info,
            )
            .await
            .with_context(|| format!("pairing with {} failed", found.addr))?;
            (peer, Some(found.addr.to_string()))
        }
        (None, None) => {
            let bind = args.listen.unwrap_or(config.listen);
            let listener = macwin_net::pair_listen(bind, &id.identity)
                .await
                .with_context(|| format!("cannot listen on {bind}"))?;
            let local = listener.local_addr()?;
            println!("listening on {local}");
            let responder =
                match pairing_responder(&config, local, &info, id.identity.fingerprint()).await {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "warning: the other machine cannot find this one by itself (UDP \
                             port {}: {e}); type the address shown below there",
                            config.discovery_port
                        );
                        None
                    }
                };
            let discoverable = responder.is_some();
            let show = |code: &PairingCode, used: Option<&str>| {
                if let Some(why) = used {
                    eprintln!("pairing attempt failed: {why}");
                    println!("that code is used up; a new one:");
                }
                show_code(code, local, discoverable);
            };
            let result = tokio::select! {
                r = wait_for_pairing(&listener, &info, show) => r,
                _ = tokio::signal::ctrl_c() => Err(anyhow!("pairing cancelled")),
            };
            drop(responder);
            listener.shutdown().await;
            (result?, None)
        }
    };
    let (side, conflict) = agree_side(info.peer_side, peer.peer_side);
    if let (Some(s), true) = (side, conflict) {
        eprintln!(
            "warning: both machines put the other on the {}; keeping this machine's choice",
            side_name(s)
        );
    }
    if let Some(old) = store_pairing(&id.config_dir, &peer, side, address)? {
        println!("replaced the previously paired \"{old}\"");
    }
    println!(
        "paired with \"{}\" ({}), fingerprint {}",
        peer.device_name,
        os_name(peer.os),
        peer.fingerprint
    );
    match side {
        Some(s) => println!("its screens are on the {} of this machine", side_name(s)),
        None => println!("tell macwin where its screens are: macwin config set side left|right"),
    }
    println!("start sharing with: macwin run");
    Ok(())
}

pub fn unpair() -> anyhow::Result<()> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let mut config = Config::load(&dir)?;
    match config.peer.take() {
        None => println!("not paired"),
        Some(p) => {
            config.save(&dir)?;
            println!("forgot \"{}\"; run `macwin unpair` there as well", p.name);
        }
    }
    Ok(())
}

pub fn config_show() -> anyhow::Result<()> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let config = Config::load(&dir)?;
    println!("# {}", Config::path(&dir).display());
    print!("{}", config.to_toml());
    Ok(())
}

pub fn config_set(key: &str, value: &str) -> anyhow::Result<()> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let mut config = Config::load(&dir)?;
    config.set(key, value)?;
    config.save(&dir)?;
    println!("{key} = {value}");
    Ok(())
}

pub fn autostart(action: AutostartArg) -> anyhow::Result<()> {
    supported_os()?;
    let a = autostart::for_this_machine()?;
    match action {
        AutostartArg::On => {
            if cfg!(target_os = "macos")
                && let Ok(exe) = std::env::current_exe()
                && autostart::app_bundle(&exe).is_none()
            {
                eprintln!(
                    "warning: this macwin is not inside MacWin.app; macOS will ask for its \
                     permissions separately"
                );
            }
            a.enable()?;
            println!("macwin starts at login ({})", a.describe());
        }
        AutostartArg::Off => {
            a.disable()?;
            println!("macwin no longer starts at login");
        }
        AutostartArg::Status => match a.state()? {
            AutostartState::On => println!("on ({})", a.describe()),
            AutostartState::Off => println!("off"),
            AutostartState::Other => println!(
                "on, but for another copy of macwin ({}); `macwin autostart on` switches it to \
                 this one",
                a.describe()
            ),
        },
    }
    Ok(())
}

// ---------- macwin run ----------

fn print_event(e: RunEvent) {
    match e {
        RunEvent::Listening(addr) => println!("listening on {addr}"),
        RunEvent::Dialing(target) => println!("connecting to {target}"),
        RunEvent::Connected { name, remote, .. } => println!("connected to \"{name}\" ({remote})"),
        RunEvent::PeerFound(addr) => println!("found the other machine at its new address {addr}"),
        RunEvent::Role(role) => info!(?role, "role"),
        RunEvent::Ended(end) => println!("session ended: {end:?}"),
        RunEvent::Retrying { after, error } if after.is_zero() => {
            println!("a connection attempt failed: {error}")
        }
        RunEvent::Retrying { after, error } => {
            println!(
                "connection failed: {error}; retrying in {} s",
                after.as_secs()
            )
        }
    }
    let _ = std::io::stdout().flush();
}

/// The cache directory (the instance lock, received files, the tray log).
fn cache_dir() -> anyhow::Result<PathBuf> {
    Ok(macwin_net::cache_dir(|k| std::env::var_os(k))?)
}

const ALREADY_RUNNING: &str =
    "MacWin is already running on this computer (the app or `macwin run`); quit it first";

pub async fn run() -> anyhow::Result<()> {
    let os = supported_os()?;
    platform::init();
    let _lock = instance::acquire(&cache_dir()?)?.context(ALREADY_RUNNING)?;
    let id = load_identity()?;
    let config = Config::load(&id.config_dir)?;
    let peer = config.peer.clone().context(
        "not paired yet: run `macwin pair` on one machine and the command it prints on the other",
    )?;
    // A secondary follows the placement its primary announces.
    let placement = config.session_placement().map_err(|_| {
        anyhow!(
            "tell macwin where the peer's screens are: arrange them in the MacWin window, or run \
             macwin config set side left|right"
        )
    })?;
    let staging = staging_dir()?;
    let session = session_config(&config, os, device_name(), placement, staging);
    let with_clipboard = session.clipboard.is_some();
    let sharing = match &session.clipboard {
        None => "off".to_string(),
        Some(s) => {
            let mut kinds = vec!["text"];
            if s.images {
                kinds.push("images");
            }
            if s.files {
                kinds.push("files");
            }
            kinds.join(", ")
        }
    };
    let mode = match &peer.address {
        Some(a) => Mode::Dial(a.clone()),
        None => Mode::Listen(config.listen),
    };
    let discovery = run_discovery(&config);
    let whose = match placement {
        Some(p) => format!(
            "whose screens are on the {} of {}",
            edge_name(p.edge),
            p.display
                .map_or("this machine".to_string(), |d| format!("display {d}"))
        ),
        None => "which places this machine (it is the primary)".to_string(),
    };
    println!(
        "sharing with \"{}\" ({}), {whose}; clipboard: {sharing}",
        peer.name,
        os_name(peer.os),
    );
    println!("emergency release: {EMERGENCY_CHORD}. Ctrl+C here stops macwin.");
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop_tx.send(true);
    });
    let opts = RunOptions {
        session,
        mode,
        peer: peer.fingerprint,
        retry_min: runner::RETRY_MIN,
        retry_max: runner::RETRY_MAX,
        connect_timeout: runner::CONNECT_TIMEOUT,
        discovery,
    };
    let dir = id.config_dir.clone();
    let peer_fp = peer.fingerprint;
    let on_event = move |e: RunEvent| {
        if let RunEvent::PeerFound(addr) = &e {
            save_found_address(&dir, peer_fp, *addr);
        }
        print_event(e);
    };
    let exit = runner::run(
        opts,
        id.identity,
        || platform::backends(with_clipboard),
        on_event,
        stop_rx,
    )
    .await?;
    match exit {
        RunExit::Stopped => println!("stopped"),
        RunExit::EmergencyStop => println!("stopped by the emergency chord"),
    }
    Ok(())
}

/// Where received files wait until they are pasted.
pub fn staging_dir() -> anyhow::Result<PathBuf> {
    Ok(cache_dir()?.join("incoming"))
}

// ---------- macwin tray: the app ----------

/// The app (`macwin tray`, MacWin.app, macwin-tray.exe): the window and the
/// menu bar item (macOS) or notification area icon (Windows) on this (main)
/// thread, everything else on a tokio runtime. `background` (start at login)
/// opens no window until it is asked for. Logs go to a file; errors before
/// the UI is up are also shown in an alert.
pub fn tray(background: bool) -> anyhow::Result<()> {
    let log = logging::log_path(|k| std::env::var_os(k));
    match &log {
        Some(path) => {
            if let Err(e) = logging::init_file(path) {
                logging::init();
                warn!(error = %e, path = %path.display(), "cannot open the log file");
            }
        }
        None => logging::init(),
    }
    let result = run_tray_app(background);
    if let Err(e) = &result {
        error!(error = %format!("{e:#}"), "the app stopped");
        platform::alert("MacWin", &format!("MacWin cannot start: {e:#}"));
    }
    result
}

fn run_tray_app(background: bool) -> anyhow::Result<()> {
    let os = supported_os()?;
    platform::init();
    let Some(_lock) = instance::acquire(&cache_dir()?)? else {
        info!("another macwin is running");
        // At login there is nothing to show; otherwise the running copy
        // shows its window.
        if !background && !platform::show_running_instance() {
            platform::alert("MacWin", ALREADY_RUNNING);
        }
        return Ok(());
    };
    let id = load_identity()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        background, "app starting"
    );
    if !background {
        // Not at login: the prompts need the user's attention.
        platform::request_permissions();
    }
    let autostart = autostart::for_this_machine().ok();
    if let Some(a) = &autostart {
        match a.migrate() {
            Ok(true) => info!("the login item of an older version now starts without the window"),
            Ok(false) => {}
            Err(e) => warn!(error = %format!("{e:#}"), "cannot update the login item"),
        }
    }
    let make_backends: MakeBackends = Arc::new(platform::backends);
    let discovery_targets: DiscoveryTargets = Arc::new(discovery_targets);
    let name = device_name();
    let env = TrayEnv {
        config_dir: id.config_dir,
        identity: id.identity,
        os,
        device_name: name.clone(),
        staging: staging_dir()?,
        make_backends,
        autostart,
        discovery_targets,
        discovery_wait: DISCOVERY_WAIT,
        retry_min: runner::RETRY_MIN,
        retry_max: runner::RETRY_MAX,
        connect_timeout: runner::CONNECT_TIMEOUT,
        window: true,
        screens: Arc::new(platform::screen_descs),
        permissions: platform::permission_probe(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the runtime")?;
    let (app, controller) = tray::start(env);
    let controller = runtime.spawn(controller);
    let quitter = app.clone();
    runtime.spawn(async move {
        // Without a console (macwin-tray.exe) this never fires; an error must not quit.
        if tokio::signal::ctrl_c().await.is_ok() {
            quitter.quit();
        }
    });
    let backend = Arc::new(WindowBackend::new(
        app.clone(),
        os,
        name,
        Box::new(platform::display_names),
        Box::new(platform::open_permission_settings),
    ));
    // On macOS this ends the process once the controller has finished.
    platform::run_window(app.clone(), backend, background);
    app.quit();
    let _ = runtime.block_on(tokio::time::timeout(Duration::from_secs(5), controller));
    runtime.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}

/// The session settings `config` asks for.
pub fn session_config(
    config: &Config,
    os: macwin_core::types::Os,
    device_name: String,
    placement: Option<Placement>,
    staging: PathBuf,
) -> SessionConfig {
    let c = &config.clipboard;
    SessionConfig {
        local_os: os,
        os_version: String::new(),
        device_name,
        placement,
        remap: config.remap,
        timings: Timings::default(),
        clipboard: c.enabled.then(|| ClipboardSettings {
            max_text_bytes: c.max_text_bytes(),
            images: c.images,
            max_image_bytes: macwin_proto::MAX_CLIPBOARD_IMAGE,
            files: c.files,
            max_files_bytes: c.max_files_bytes(),
            staging,
            poll: crate::clipboard::POLL_INTERVAL,
        }),
    }
}

/// LAN discovery settings of the run loop, from the config.
pub fn run_discovery(config: &Config) -> Option<RunDiscovery> {
    config.discovery.then(|| RunDiscovery {
        responder: responder_addr(config.listen, config.discovery_port),
        targets: discovery_targets(config.discovery_port),
        wait: DISCOVERY_WAIT,
    })
}

/// Stores the peer's new address (see [`remember_peer_address`]).
pub fn save_found_address(dir: &Path, peer: Fingerprint, addr: SocketAddr) {
    match remember_peer_address(dir, peer, addr) {
        Ok(true) => info!(%addr, "saved the other machine's new address"),
        Ok(false) => {}
        Err(e) => warn!(error = %format!("{e:#}"), "cannot save the other machine's new address"),
    }
}

/// Listens until one peer completes the handshake.
async fn accept_one(
    addr: SocketAddr,
    id: &Identity,
    args_fp: macwin_net::Fingerprint,
    hello: &macwin_core::msg::Hello,
) -> anyhow::Result<Session> {
    let listener = listen(addr, id, args_fp)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    println!("listening on {}", listener.local_addr()?);
    std::io::stdout().flush()?;
    loop {
        match listener.accept(hello).await {
            Ok(s) => return Ok(s),
            Err(NetError::EndpointClosed) => bail!("the listener closed"),
            Err(e) => warn!(error = %e, "a peer failed the handshake; still listening"),
        }
    }
}

pub async fn spike_net(args: SpikeNetArgs) -> anyhow::Result<()> {
    let os = supported_os()?;
    let id = load_identity()?.identity;
    println!("local fingerprint: {}", id.fingerprint());
    let hello = spike_hello(os, &device_name());
    if let Some(addr) = args.listen {
        let session = accept_one(addr, &id, args.peer_fp, &hello).await?;
        println!(
            "peer connected: {} ({})",
            session.peer.device_name, session.remote
        );
        spike_net::run_listener(session).await?;
        println!("peer closed the session");
        return Ok(());
    }
    let remote = resolve(
        &args
            .connect
            .context("either --listen or --connect is required")?,
    )
    .await?;
    let session = connect(bind_for(remote), remote, &id, args.peer_fp, &hello)
        .await
        .with_context(|| format!("cannot connect to {remote}"))?;
    println!(
        "connected to {} ({})",
        session.peer.device_name, session.remote
    );
    let opts = SpikeOpts {
        count: args.count.max(1),
        interval: Duration::from_millis(args.interval_ms),
    };
    let report = spike_net::run_dialer(session, opts).await?;
    println!("{report}");
    Ok(())
}

// ---------- probes ----------

fn describe_screen(s: &ScreenInfo) -> String {
    format!(
        "screen {}: {}x{} at ({}, {}), scale {:.2}{}",
        s.id,
        s.rect.width(),
        s.rect.height(),
        s.rect.left(),
        s.rect.top(),
        s.native_per_logical,
        if s.primary { ", primary" } else { "" }
    )
}

pub fn probe_screens() -> anyhow::Result<()> {
    platform::init();
    for s in platform::screens().screens()? {
        println!("{}", describe_screen(&s));
    }
    Ok(())
}

pub fn probe_permissions(prompt: bool) {
    print!("{}", platform::permissions_report(prompt));
}

fn print_clipboard(i: &ClipboardInfo) {
    println!(
        "change {}: {} format(s){}",
        i.change_count,
        i.formats.len(),
        if i.ours { ", written by macwin" } else { "" }
    );
    for f in &i.formats {
        match f.size {
            Some(n) => println!("  {} ({n} bytes)", f.name),
            None => println!("  {}", f.name),
        }
    }
}

pub async fn probe_clipboard(watch: bool) -> anyhow::Result<()> {
    let cb = platform::clipboard();
    println!("clipboard formats and sizes only; the content is never shown");
    print_clipboard(&cb.inspect()?);
    if !watch {
        return Ok(());
    }
    println!("watching for changes (Ctrl+C to stop)");
    let mut last = cb.change_count()?;
    let mut tick = tokio::time::interval(Duration::from_millis(300));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = cb.change_count()?;
                if now != last {
                    last = now;
                    print_clipboard(&cb.inspect()?);
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

// ---------- local spikes ----------

fn primary_center(screens: &[ScreenInfo]) -> Point {
    let s = screens.iter().find(|s| s.primary).unwrap_or(&screens[0]);
    Point::new(
        s.rect.left() + s.rect.width() / 2,
        s.rect.top() + s.rect.height() / 2,
    )
}

fn describe_key(hid: Hid) -> String {
    match keymap::name(hid) {
        Some(n) => format!("{n} ({:#08x})", hid.0),
        None => format!("{:#08x}", hid.0),
    }
}

/// Prints events (mouse moves at most 10 per second) and swallows them when
/// grabbing.
struct PrintSink {
    swallow: bool,
    faults: Mutex<mpsc::Sender<CaptureFault>>,
    last_move: Mutex<Option<Instant>>,
}

impl CaptureSink for PrintSink {
    fn on_event(&self, ev: CapturedEvent) -> bool {
        match ev {
            CapturedEvent::MouseMove { pos, delta } => {
                let mut last = self.last_move.lock().unwrap_or_else(|e| e.into_inner());
                if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                    *last = Some(Instant::now());
                    println!(
                        "move   pos ({:.1}, {:.1}) delta ({:.1}, {:.1})",
                        pos.x, pos.y, delta.x, delta.y
                    );
                }
            }
            CapturedEvent::Button { button, down, pos } => {
                println!(
                    "button {button:?} {} at ({:.1}, {:.1})",
                    if down { "down" } else { "up" },
                    pos.x,
                    pos.y
                );
            }
            CapturedEvent::Wheel { dx, dy, .. } => println!("wheel  dx {dx} dy {dy}"),
            CapturedEvent::Key { hid, down, repeat } => println!(
                "key    {} {}{}",
                describe_key(hid),
                if down { "down" } else { "up" },
                if repeat { " (repeat)" } else { "" }
            ),
        }
        !self.swallow
    }

    fn on_fault(&self, fault: CaptureFault) {
        let _ = self
            .faults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(fault);
    }
}

pub async fn spike_capture(args: SpikeCaptureArgs) -> anyhow::Result<()> {
    platform::init();
    let screens = platform::screens().screens()?;
    let center = primary_center(&screens);
    let (fault_tx, fault_rx) = mpsc::channel();
    let sink = Arc::new(PrintSink {
        swallow: args.grab,
        faults: Mutex::new(fault_tx),
        last_move: Mutex::new(None),
    });
    let mut capture = platform::capture();
    let control = capture.start(sink)?;
    if args.grab {
        println!(
            "GRABBING for {} s: local input is swallowed and the cursor is hidden at the screen center.\n\
             Press {EMERGENCY_CHORD} to release early.",
            args.seconds
        );
        control.command(GrabCommand::Grab { exit: center });
    } else {
        println!(
            "printing local input for {} s (Ctrl+C to stop); nothing is swallowed",
            args.seconds
        );
    }
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut outcome = "timeout";
    while Instant::now() < deadline {
        if let Ok(fault) = fault_rx.try_recv() {
            println!("fail-safe fired: {fault:?}");
            outcome = "fail-safe";
            break;
        }
        if args.grab {
            control.renew_lease(Instant::now() + Duration::from_secs(1));
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            _ = tokio::signal::ctrl_c() => { outcome = "Ctrl+C"; break; }
        }
    }
    if args.grab {
        control.command(GrabCommand::Release { warp_to: center });
    }
    control.stop();
    drop(capture);
    println!("capture stopped ({outcome})");
    Ok(())
}

pub async fn spike_inject() -> anyhow::Result<()> {
    platform::init();
    let screens = platform::screens().screens()?;
    let c = primary_center(&screens);
    let mut injector = platform::injector()?;
    println!(
        "In 3 seconds the cursor draws a 200 px square around the screen center, then \"hello\" is typed\n\
         with physical keys (the active layout decides the letters: a Russian layout gives \"руддщ\").\n\
         Focus a text field now."
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let half = 100;
    let corners = [
        Point::new(c.x - half, c.y - half),
        Point::new(c.x + half, c.y - half),
        Point::new(c.x + half, c.y + half),
        Point::new(c.x - half, c.y + half),
        Point::new(c.x - half, c.y - half),
    ];
    injector.inject(InjectCommand::MoveTo(corners[0]))?;
    for w in corners.windows(2) {
        for step in 1..=25 {
            let p = Point::new(
                w[0].x + (w[1].x - w[0].x) * step / 25,
                w[0].y + (w[1].y - w[0].y) * step / 25,
            );
            injector.inject(InjectCommand::MoveTo(p))?;
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }
    // HID usages of h, e, l, l, o.
    for usage in [0x0B, 0x08, 0x0F, 0x0F, 0x12] {
        let hid = Hid(0x07_0000 | usage);
        injector.inject(InjectCommand::Key {
            hid,
            down: true,
            repeat: false,
        })?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        injector.inject(InjectCommand::Key {
            hid,
            down: false,
            repeat: false,
        })?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    println!("done");
    Ok(())
}

// ---------- KVM ----------

pub async fn spike_kvm(args: SpikeKvmArgs) -> anyhow::Result<()> {
    let os = supported_os()?;
    platform::init();
    let id = load_identity()?.identity;
    println!("local fingerprint: {}", id.fingerprint());
    let backends = platform::backends(false).context("cannot start the input backends")?;
    let screens = backends.screens.screens()?;
    for s in &screens {
        println!("{}", describe_screen(s));
    }
    let cfg = SessionConfig {
        local_os: os,
        os_version: String::new(),
        device_name: device_name(),
        placement: Some(Placement::side(args.peer_side.into())),
        remap: !args.no_remap,
        timings: Timings::default(),
        clipboard: None,
    };
    let hello = kvm_hello(&cfg, screens);
    let (session, role) = match args.listen {
        Some(addr) => (
            accept_one(addr, &id, args.peer_fp, &hello).await?,
            TransportRole::Listener,
        ),
        None => {
            let remote = resolve(
                &args
                    .connect
                    .context("either --listen or --connect is required")?,
            )
            .await?;
            let s = connect(bind_for(remote), remote, &id, args.peer_fp, &hello)
                .await
                .with_context(|| format!("cannot connect to {remote}"))?;
            (s, TransportRole::Dialer)
        }
    };
    println!(
        "connected to {} ({}). Move the cursor over the {:?} edge to cross.\n\
         Emergency release: {EMERGENCY_CHORD}. Ctrl+C here stops the session.",
        session.peer.device_name, session.remote, args.peer_side
    );
    let handle = spawn_session(
        cfg,
        backends,
        Connected {
            link: session.link,
            peer: session.peer,
            role,
        },
    )?;
    let stopper = handle.stopper();
    let link = handle.link();
    let mut roles = handle.role.clone();
    tokio::spawn(async move {
        while roles.changed().await.is_ok() {
            let role = *roles.borrow_and_update();
            println!("role: {role:?}");
        }
    });
    let join = handle.join();
    tokio::pin!(join);
    let end = tokio::select! {
        end = &mut join => end,
        _ = tokio::signal::ctrl_c() => {
            stopper.shutdown();
            join.await
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), link.wait_closed()).await;
    println!("session ended: {end:?}");
    match end {
        SessionEnd::Shutdown | SessionEnd::EmergencyStop | SessionEnd::PeerClosed => Ok(()),
        other => Err(anyhow!("the session failed: {other:?}")),
    }
}
