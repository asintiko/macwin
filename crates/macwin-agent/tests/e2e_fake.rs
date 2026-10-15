//! Two drivers with fake backends talking over real QUIC on 127.0.0.1.
//!
//! A = Mac 1440x900 (peer on its right, dialer), B = Windows 1920x1080 (peer
//! on its left, listener).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use macwin_agent::driver::{
    Backends, ClipboardSettings, Connected, SessionConfig, SessionEnd, SessionHandle, kvm_hello,
    spawn_session,
};
use macwin_core::arrange::{Edge, Placement};
use macwin_core::engine::{Timings, TransportRole};
use macwin_core::msg::{ButtonMsg, Enter, InputMsg, KeyMsg};
use macwin_core::types::{
    CapturedEvent, GrabCommand, Hid, InjectCommand, MouseButton, Os, Point, PointF, Rect, RoleKind,
    ScreenInfo, Side, WheelUnit,
};
use macwin_net::{Identity, Session, codes, connect, listen};
use macwin_platform::testing::{
    FakeCapture, FakeCaptureHandle, FakeClipboard, FakeClipboardHandle, FakeInjector,
    FakeInjectorHandle, FakeScreens,
};

const T: Duration = Duration::from_secs(5);
const KEY_A: Hid = Hid(0x07_0004);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn screens(w: i32, h: i32) -> Vec<ScreenInfo> {
    vec![ScreenInfo {
        id: 1,
        rect: Rect::new(0, 0, w, h).unwrap(),
        native_per_logical: 1.0,
        primary: true,
    }]
}

/// `placement` is where this machine put the other one; `None` makes it a
/// secondary that follows the other's.
fn cfg(os: Os, name: &str, placement: Option<Placement>) -> SessionConfig {
    SessionConfig {
        local_os: os,
        os_version: "test".into(),
        device_name: name.into(),
        placement,
        remap: true,
        timings: Timings::default(),
        clipboard: None,
    }
}

/// Clipboard sharing of every kind with a fast poll; `max` limits each
/// kind. The staging directory lives as long as the test process.
fn sharing(max: usize) -> Option<ClipboardSettings> {
    let staging = tempfile::tempdir().unwrap().keep();
    Some(ClipboardSettings {
        max_text_bytes: max,
        images: true,
        max_image_bytes: max,
        files: true,
        max_files_bytes: max as u64,
        staging,
        poll: Duration::from_millis(5),
    })
}

fn mac_cfg() -> SessionConfig {
    cfg(Os::MacOs, "mac", Some(Placement::side(Side::Right)))
}

fn win_cfg() -> SessionConfig {
    cfg(Os::Windows, "pc", Some(Placement::side(Side::Left)))
}

struct Peer {
    handle: SessionHandle,
    capture: FakeCaptureHandle,
    injector: FakeInjectorHandle,
    clipboard: FakeClipboardHandle,
}

impl Peer {
    fn role(&self) -> RoleKind {
        *self.handle.role.borrow()
    }
}

fn spawn_peer(
    cfg: SessionConfig,
    scr: Vec<ScreenInfo>,
    session: Session,
    role: TransportRole,
) -> Peer {
    let (capture, capture_handle) = FakeCapture::new();
    let (injector, injector_handle) = FakeInjector::new();
    let (clipboard, clipboard_handle) = FakeClipboard::new();
    let backends = Backends {
        capture: Box::new(capture),
        injector: Box::new(injector),
        screens: Box::new(FakeScreens(scr)),
        clipboard: Some(Box::new(clipboard)),
    };
    let handle = spawn_session(
        cfg,
        backends,
        Connected {
            link: session.link,
            peer: session.peer,
            role,
        },
    )
    .unwrap();
    Peer {
        handle,
        capture: capture_handle,
        injector: injector_handle,
        clipboard: clipboard_handle,
    }
}

/// Connects the two QUIC sessions: (dialer side A, listener side B).
async fn sessions(
    a_hello: macwin_core::msg::Hello,
    b_hello: macwin_core::msg::Hello,
) -> (Session, Session) {
    let a_id = Identity::generate().unwrap();
    let b_id = Identity::generate().unwrap();
    let listener = listen(localhost(), &b_id, a_id.fingerprint())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { listener.accept(&b_hello).await.unwrap() });
    let a = tokio::time::timeout(
        T,
        connect(localhost(), addr, &a_id, b_id.fingerprint(), &a_hello),
    )
    .await
    .unwrap()
    .unwrap();
    let b = tokio::time::timeout(T, server).await.unwrap().unwrap();
    (a, b)
}

async fn start() -> (Peer, Peer) {
    start_with(mac_cfg(), win_cfg()).await
}

async fn start_with(a_cfg: SessionConfig, b_cfg: SessionConfig) -> (Peer, Peer) {
    let a_hello = kvm_hello(&a_cfg, screens(1440, 900));
    let b_hello = kvm_hello(&b_cfg, screens(1920, 1080));
    let (a, b) = sessions(a_hello.clone(), b_hello.clone()).await;
    // 1. Each side sees exactly the other's Hello.
    assert_eq!(a.peer, b_hello);
    assert_eq!(b.peer, a_hello);
    (
        spawn_peer(a_cfg, screens(1440, 900), a, TransportRole::Dialer),
        spawn_peer(b_cfg, screens(1920, 1080), b, TransportRole::Listener),
    )
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn mv(x: f64, y: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::new(x, y),
        delta: PointF::default(),
    }
}

fn delta(dx: f64, dy: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::default(),
        delta: PointF::new(dx, dy),
    }
}

fn key(hid: Hid, down: bool) -> CapturedEvent {
    CapturedEvent::Key {
        hid,
        down,
        repeat: false,
    }
}

/// A pushes its cursor over its right edge onto B.
async fn a_controls_b(a: &Peer, b: &Peer) {
    assert!(
        !a.capture.emit(mv(1439.5, 450.0)),
        "the crossing move is swallowed"
    );
    assert_eq!(a.role(), RoleKind::Controlling);
    assert_eq!(
        a.capture.commands().last(),
        Some(&GrabCommand::Grab {
            exit: Point::new(1439, 450)
        })
    );
    wait_until("B controlled", || b.role() == RoleKind::Controlled).await;
    wait_until("entry MoveTo on B", || {
        b.injector.last_pos() == Some(Point::new(0, 540))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crossing_from_a_to_b_with_motion_and_remap() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(delta(10.0, -5.0)));
    wait_until("datagram motion", || {
        b.injector.last_pos() == Some(Point::new(10, 535))
    })
    .await;
    // Cmd on the Mac keyboard arrives as Ctrl on the PC.
    assert!(!a.capture.emit(key(Hid::LEFT_GUI, true)));
    wait_until("LeftCtrl held on B", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_CTRL)
    })
    .await;
    assert!(!b.injector.pressed_keys().contains(&Hid::LEFT_GUI));
    assert!(!a.capture.emit(key(Hid::LEFT_GUI, false)));
    wait_until("LeftCtrl released on B", || {
        b.injector.pressed_keys().is_empty()
    })
    .await;
    assert_eq!(b.injector.unmatched_ups(), 0);
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn trackpad_scrolling_stays_in_pixels() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    let wheel = |dx, dy, unit| CapturedEvent::Wheel { dx, dy, unit };
    assert!(!a.capture.emit(wheel(0, -7, WheelUnit::Pixel)));
    assert!(!a.capture.emit(wheel(0, 120, WheelUnit::Notch120)));
    let wheels = || {
        b.injector
            .commands()
            .into_iter()
            .filter(|c| matches!(c, InjectCommand::Wheel { .. }))
            .collect::<Vec<_>>()
    };
    wait_until("both scrolls injected on B", || wheels().len() == 2).await;
    assert_eq!(
        wheels(),
        [
            InjectCommand::Wheel {
                dx: 0,
                dy: -7,
                unit: WheelUnit::Pixel
            },
            InjectCommand::Wheel {
                dx: 0,
                dy: 120,
                unit: WheelUnit::Notch120
            },
        ]
    );
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn crossing_from_b_to_a() {
    let (a, b) = start().await;
    assert!(!b.capture.emit(mv(0.0, 540.0)));
    assert_eq!(b.role(), RoleKind::Controlling);
    assert_eq!(
        b.capture.commands().last(),
        Some(&GrabCommand::Grab {
            exit: Point::new(0, 540)
        })
    );
    wait_until("A controlled", || a.role() == RoleKind::Controlled).await;
    wait_until("entry MoveTo on A", || {
        a.injector.last_pos() == Some(Point::new(1439, 450))
    })
    .await;
    // Back over the Mac's right edge: B gets its cursor back just inside its left edge.
    assert!(!b.capture.emit(delta(5.0, 0.0)));
    assert_eq!(b.role(), RoleKind::Idle);
    assert_eq!(
        b.capture.commands().last(),
        Some(&GrabCommand::Release {
            warp_to: Point::new(2, 540)
        })
    );
    wait_until("A idle", || a.role() == RoleKind::Idle).await;
    b.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn physical_input_on_the_target_takes_control_back() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(
        b.capture.emit(key(KEY_A, true)),
        "the key reaches B's own OS"
    );
    assert_eq!(b.role(), RoleKind::Idle);
    wait_until("A released", || {
        a.capture.commands().last()
            == Some(&GrabCommand::Release {
                warp_to: Point::new(1437, 450),
            })
    })
    .await;
    wait_until("A idle", || a.role() == RoleKind::Idle).await;
    a.handle.shutdown();
}

/// A raw (driverless) dialer against B's driver.
async fn raw_a_and_driver_b() -> (Session, Peer) {
    let a_hello = kvm_hello(&mac_cfg(), screens(1440, 900));
    let b_hello = kvm_hello(&win_cfg(), screens(1920, 1080));
    let (a, b) = sessions(a_hello, b_hello).await;
    (
        a,
        spawn_peer(win_cfg(), screens(1920, 1080), b, TransportRole::Listener),
    )
}

fn press_shift_and_button(a: &Session) {
    a.link
        .tx
        .send_input(InputMsg::Enter(Enter {
            epoch: 1,
            at: Point::new(0, 540),
        }))
        .unwrap();
    a.link
        .tx
        .send_input(InputMsg::Key(KeyMsg {
            hid: Hid::LEFT_SHIFT,
            down: true,
            repeat: false,
        }))
        .unwrap();
    a.link
        .tx
        .send_input(InputMsg::Button(ButtonMsg {
            button: MouseButton::Left,
            down: true,
            at: Point::new(5, 5),
        }))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_disconnect_releases_everything() {
    let (a, b) = raw_a_and_driver_b().await;
    press_shift_and_button(&a);
    wait_until("B holds shift + button", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_SHIFT)
            && b.injector.pressed_buttons() == vec![MouseButton::Left]
    })
    .await;
    a.link.tx.close_now(codes::INTERNAL);
    wait_until("B released", || {
        b.injector.pressed_keys().is_empty() && b.injector.pressed_buttons().is_empty()
    })
    .await;
    let end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(end, SessionEnd::PeerClosed);
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_peer_hits_the_heartbeat_timeout() {
    let (a, b) = raw_a_and_driver_b().await;
    press_shift_and_button(&a);
    wait_until("B holds shift", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_SHIFT)
    })
    .await;
    let silent_since = Instant::now();
    // A keeps the QUIC connection alive (keep-alives) but its "Engine" is silent.
    wait_until("B released after the timeout", || {
        b.injector.pressed_keys().is_empty()
    })
    .await;
    let waited = silent_since.elapsed();
    assert!(
        waited >= Duration::from_millis(1300) && waited <= Duration::from_secs(3),
        "released after {waited:?}"
    );
    assert!(b.injector.pressed_buttons().is_empty());
    let end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(end, SessionEnd::HeartbeatTimeout);
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn emergency_chord_stops_the_session_and_releases_the_peer() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(key(KEY_A, true)));
    wait_until("B holds KeyA", || {
        b.injector.pressed_keys().contains(&KEY_A)
    })
    .await;
    // The backend already released the grab locally when it reports this.
    a.capture
        .emit_fault(macwin_core::types::CaptureFault::EmergencyChord);
    let a_end = tokio::time::timeout(T, a.handle.join()).await.unwrap();
    assert_eq!(a_end, SessionEnd::EmergencyStop);
    assert!(a.capture.stopped());
    wait_until("B released", || b.injector.pressed_keys().is_empty()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_while_controlling_releases_the_peer() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(key(KEY_A, true)));
    wait_until("B holds KeyA", || {
        b.injector.pressed_keys().contains(&KEY_A)
    })
    .await;
    a.handle.shutdown();
    let a_end = tokio::time::timeout(T, a.handle.join()).await.unwrap();
    assert_eq!(a_end, SessionEnd::Shutdown);
    assert!(a.capture.stopped());
    assert!(
        matches!(
            a.capture.commands().last(),
            Some(GrabCommand::Release { .. })
        ),
        "A ungrabbed"
    );
    wait_until("B released", || b.injector.pressed_keys().is_empty()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
    assert_eq!(b.injector.unmatched_ups(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_session_handle_stops_capture_and_releases_the_peer() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(key(KEY_A, true)));
    wait_until("B holds KeyA", || {
        b.injector.pressed_keys().contains(&KEY_A)
    })
    .await;

    drop(a.handle);

    wait_until("A capture stopped", || {
        a.capture.stopped() && a.capture.backend_dropped()
    })
    .await;
    assert!(matches!(
        a.capture.commands().last(),
        Some(GrabCommand::Release { .. })
    ));
    wait_until("B released", || b.injector.pressed_keys().is_empty()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
    assert_eq!(b.injector.unmatched_ups(), 0);
}

fn with_clipboard(mut c: SessionConfig, max: usize) -> SessionConfig {
    c.clipboard = sharing(max);
    c
}

/// Long enough for several clipboard polls and a round trip.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_text_crosses_both_ways_without_echo() {
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 1024),
        with_clipboard(win_cfg(), 1024),
    )
    .await;
    a.clipboard.copy_text("hello from the mac");
    wait_until("B got the text", || {
        b.clipboard.text().as_deref() == Some("hello from the mac")
    })
    .await;
    b.clipboard.copy_text("привет с ПК");
    wait_until("A got the text", || {
        a.clipboard.text().as_deref() == Some("привет с ПК")
    })
    .await;
    settle().await;
    // Each side wrote exactly once: no echo, no loop.
    assert_eq!(a.clipboard.text_writes(), vec!["привет с ПК".to_string()]);
    assert_eq!(
        b.clipboard.text_writes(),
        vec!["hello from the mac".to_string()]
    );
    // A clipboard manager re-copying the received text does not bounce it back.
    a.clipboard.recopy();
    settle().await;
    assert_eq!(b.clipboard.writes().len(), 1);
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_images_cross_both_ways() {
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 1024),
        with_clipboard(win_cfg(), 1024),
    )
    .await;
    a.clipboard.copy_image(b"\x89PNG fake screenshot");
    wait_until("B got the image", || {
        b.clipboard.image().as_deref() == Some(&b"\x89PNG fake screenshot"[..])
    })
    .await;
    b.clipboard.copy_image(b"\x89PNG other");
    wait_until("A got the image", || {
        a.clipboard.image().as_deref() == Some(&b"\x89PNG other"[..])
    })
    .await;
    settle().await;
    assert_eq!(a.clipboard.writes().len(), 1);
    assert_eq!(b.clipboard.writes().len(), 1);
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_received_image_re_encoded_by_a_clipboard_manager_is_not_sent_back() {
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 4096),
        with_clipboard(win_cfg(), 4096),
    )
    .await;
    let (w, h) = (8u32, 8u32);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 5) as u8).collect();
    let rgba = macwin_platform::image::Rgba {
        width: w,
        height: h,
        pixels: rgb
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
    };
    let png = macwin_platform::image::encode_png(&rgba).unwrap();
    a.clipboard.copy_image(&png);
    wait_until("B got the image", || {
        b.clipboard.image().as_deref() == Some(&png[..])
    })
    .await;
    // B's clipboard manager puts the same picture back as an RGB PNG.
    let mut rgb_png = Vec::new();
    let mut e = png::Encoder::new(&mut rgb_png, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&rgb).unwrap();
    assert_ne!(rgb_png, png);
    b.clipboard.copy_image(&rgb_png);
    settle().await;
    assert!(a.clipboard.writes().is_empty(), "the image bounced back");
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn copied_files_arrive_on_the_other_clipboard() {
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 1 << 20),
        with_clipboard(win_cfg(), 1 << 20),
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(src.path().join("Отчёт/img")).unwrap();
    std::fs::write(src.path().join("Отчёт/readme.txt"), b"hello").unwrap();
    std::fs::write(src.path().join("Отчёт/img/a.bin"), vec![3u8; 200_000]).unwrap();
    std::fs::write(src.path().join("notes.md"), b"# notes").unwrap();
    a.clipboard
        .copy_files(&[src.path().join("Отчёт"), src.path().join("notes.md")]);
    wait_until("B has files on its clipboard", || {
        b.clipboard.files().is_some()
    })
    .await;
    let files = b.clipboard.files().unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        files[0].ends_with("Отчёт") && files[1].ends_with("notes.md"),
        "{files:?}"
    );
    assert_eq!(
        std::fs::read(files[0].join("readme.txt")).unwrap(),
        b"hello"
    );
    assert_eq!(
        std::fs::read(files[0].join("img/a.bin")).unwrap(),
        vec![3u8; 200_000]
    );
    assert_eq!(std::fs::read(&files[1]).unwrap(), b"# notes");
    // A clipboard manager re-copying the received files does not send them back.
    b.clipboard.recopy();
    settle().await;
    assert!(
        a.clipboard.writes().is_empty(),
        "{:?}",
        a.clipboard.writes()
    );
    // A newer copy replaces the staged files on the next transfer.
    std::fs::write(src.path().join("second.txt"), b"2").unwrap();
    a.clipboard.copy_files(&[src.path().join("second.txt")]);
    wait_until("B got the second transfer", || {
        b.clipboard
            .files()
            .is_some_and(|f| f.len() == 1 && f[0].ends_with("second.txt"))
    })
    .await;
    wait_until("the previous transfer was cleaned up", || {
        !files[0].exists()
    })
    .await;
    a.handle.shutdown();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn copying_again_after_a_failed_send_sends_again() {
    use std::os::unix::fs::PermissionsExt;
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 1 << 20),
        with_clipboard(win_cfg(), 1 << 20),
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    let file = src.path().join("locked.txt");
    std::fs::write(&file, b"secret plans").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&file).is_ok() {
        return; // running as root: permissions do not stop reads
    }
    a.clipboard.copy_files(std::slice::from_ref(&file));
    settle().await;
    assert!(b.clipboard.writes().is_empty());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    a.clipboard.recopy();
    wait_until("B got the files", || b.clipboard.files().is_some()).await;
    let files = b.clipboard.files().unwrap();
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"secret plans");
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn received_files_stay_pasteable_after_a_reconnect() {
    let a_cfg = with_clipboard(mac_cfg(), 1 << 20);
    let b_cfg = with_clipboard(win_cfg(), 1 << 20);
    let (a, b) = start_with(a_cfg.clone(), b_cfg.clone()).await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("keep.txt"), b"kept").unwrap();
    a.clipboard.copy_files(&[src.path().join("keep.txt")]);
    wait_until("B has the files", || b.clipboard.files().is_some()).await;
    let pasted = b.clipboard.files().unwrap();
    a.handle.shutdown();
    assert_eq!(b.handle.join().await, SessionEnd::PeerClosed);
    // The same machines connect again (same staging directories).
    let (_a2, _b2) = start_with(a_cfg, b_cfg).await;
    settle().await;
    assert_eq!(std::fs::read(&pasted[0]).unwrap(), b"kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_copy_overrides_files_still_arriving() {
    let b_cfg = with_clipboard(win_cfg(), 64 << 20);
    let staging = b_cfg.clipboard.as_ref().unwrap().staging.clone();
    let (a, b) = start_with(with_clipboard(mac_cfg(), 64 << 20), b_cfg).await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("big.bin"), vec![1u8; 48 << 20]).unwrap();
    a.clipboard.copy_files(&[src.path().join("big.bin")]);
    // The user copies text on B while the files are still arriving.
    wait_until("B started receiving", || {
        std::fs::read_dir(&staging).is_ok_and(|mut d| d.next().is_some())
    })
    .await;
    b.clipboard.copy_text("newer");
    wait_until("A got the text", || {
        a.clipboard.text().as_deref() == Some("newer")
    })
    .await;
    settle().await;
    assert!(
        b.clipboard.writes().is_empty(),
        "the overtaken files were not put on the clipboard"
    );
    wait_until("the partial transfer was removed", || {
        std::fs::read_dir(&staging).map_or(true, |d| d.count() == 0)
    })
    .await;
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn files_over_the_limit_are_not_sent() {
    let (a, b) = start_with(
        with_clipboard(mac_cfg(), 100),
        with_clipboard(win_cfg(), 100),
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("big.bin"), vec![0u8; 1000]).unwrap();
    a.clipboard.copy_files(&[src.path().join("big.bin")]);
    settle().await;
    assert!(b.clipboard.writes().is_empty());
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn private_large_and_non_text_content_is_not_shared() {
    let (a, b) = start_with(with_clipboard(mac_cfg(), 8), with_clipboard(win_cfg(), 8)).await;
    a.clipboard.copy_sensitive("hunter2");
    a.clipboard.copy_text("way over the 8-byte limit");
    a.clipboard.copy_image(b"an image over the limit");
    settle().await;
    assert!(
        b.clipboard.writes().is_empty(),
        "{:?}",
        b.clipboard.writes()
    );
    // Sharing still works afterwards.
    a.clipboard.copy_text("short");
    wait_until("B got the short text", || {
        b.clipboard.text().as_deref() == Some("short")
    })
    .await;
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn no_clipboard_sharing_unless_both_sides_enable_it() {
    let (a, b) = start_with(with_clipboard(mac_cfg(), 1024), win_cfg()).await;
    a.clipboard.copy_text("stays on the mac");
    b.clipboard.copy_text("stays on the pc");
    settle().await;
    assert!(a.clipboard.writes().is_empty() && b.clipboard.writes().is_empty());
    assert_eq!(a.clipboard.content_reads(), 0, "A never read its clipboard");
    a.handle.shutdown();
}

/// Regression (Phase 0b): `spawn_session` dropped the capture backend on
/// return, and real backends stop capturing when dropped.
#[tokio::test(flavor = "multi_thread")]
async fn the_capture_backend_lives_exactly_as_long_as_the_session() {
    let (a, b) = start().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!a.capture.backend_dropped() && !b.capture.backend_dropped());
    a.handle.shutdown();
    let end = tokio::time::timeout(T, a.handle.join()).await.unwrap();
    assert_eq!(end, SessionEnd::Shutdown);
    wait_until("A's backend released", || a.capture.backend_dropped()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
    wait_until("B's backend released", || b.capture.backend_dropped()).await;
}
