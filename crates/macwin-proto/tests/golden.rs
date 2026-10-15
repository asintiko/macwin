//! Golden bytes: these lock the wire format. If one of them changes, peers of
//! different versions stop understanding each other.

use macwin_core::arrange::{Edge, Placement};
use macwin_core::msg::{
    ClipboardMsg, ClipboardText, ControlMsg, DatagramMsg, Heartbeat, Hello, InputMsg, KeyMsg,
    MouseAbs, PairMsg,
};
use macwin_core::types::{Hid, Os, Point, Rect, RoleKind, ScreenInfo};
use macwin_proto::{
    decode_clipboard, decode_control, decode_datagram, decode_input, decode_pair, encode_clipboard,
    encode_control, encode_datagram, encode_input, encode_pair, frame,
};

#[test]
fn clipboard_text() {
    let m = ClipboardMsg::Text(ClipboardText {
        clock: 1,
        text: "hi".into(),
    });
    let bytes = encode_clipboard(&m);
    assert_eq!(bytes, [0x0A, 0x06, 0x08, 0x01, 0x12, 0x02, 0x68, 0x69]);
    assert_eq!(decode_clipboard(&bytes).unwrap(), m);
}

#[test]
fn pair_done() {
    let bytes = encode_pair(&PairMsg::Done);
    assert_eq!(bytes, [0x1A, 0x00]);
    assert_eq!(decode_pair(&bytes).unwrap(), PairMsg::Done);
}

#[test]
fn key_a_down() {
    let m = InputMsg::Key(KeyMsg {
        hid: Hid(0x07_0004),
        down: true,
        repeat: false,
    });
    let bytes = encode_input(&m);
    assert_eq!(bytes, [0x22, 0x06, 0x08, 0x84, 0x80, 0x1c, 0x10, 0x01]);
    assert_eq!(decode_input(&bytes).unwrap(), m);
    assert_eq!(
        &frame::encode(&bytes)[..],
        [0x08, 0x22, 0x06, 0x08, 0x84, 0x80, 0x1c, 0x10, 0x01]
    );
}

#[test]
fn heartbeat() {
    let m = ControlMsg::Heartbeat(Heartbeat {
        epoch: 3,
        role: RoleKind::Controlling,
    });
    let bytes = encode_control(&m);
    assert_eq!(bytes, [0x12, 0x04, 0x08, 0x03, 0x10, 0x02]);
    assert_eq!(decode_control(&bytes).unwrap(), m);
}

#[test]
fn mouse_abs_datagram() {
    let m = DatagramMsg::MouseAbs(MouseAbs {
        epoch: 1,
        seq: 2,
        at: Point::new(-1, 5),
    });
    let bytes = encode_datagram(&m);
    assert_eq!(
        bytes,
        [
            0x0A, 0x0A, 0x08, 0x01, 0x10, 0x02, 0x1A, 0x04, 0x08, 0x01, 0x10, 0x0A
        ]
    );
    assert_eq!(decode_datagram(&bytes).unwrap(), m);
}

/// A one-screen Mac Hello (protocol 1.3) announcing `placement`.
fn hello(placement: Option<Placement>) -> ControlMsg {
    ControlMsg::Hello(Hello {
        protocol_version: 0x0001_0003,
        capabilities: vec![],
        os: Os::MacOs,
        os_version: String::new(),
        device_name: "m".into(),
        screens: vec![ScreenInfo {
            id: 1,
            rect: Rect::new(0, 0, 2, 1).unwrap(),
            native_per_logical: 1.0,
            primary: true,
        }],
        peer_side: None,
        placement,
        agent_version: String::new(),
    })
}

#[test]
fn hello_with_a_placement() {
    let m = hello(Some(Placement {
        display: Some(1),
        edge: Edge::Left,
        offset: Some(-2),
    }));
    let bytes = encode_control(&m);
    assert_eq!(
        bytes,
        [
            0x0A, 0x24, // control frame: hello, 36 bytes
            0x08, 0x83, 0x80, 0x04, // protocol version 1.3
            0x18, 0x01, // os: macOS
            0x2A, 0x01, 0x6D, // device name "m"
            0x32, 0x11, // one screen, 17 bytes
            0x08, 0x01, 0x20, 0x02, 0x28, 0x01, // id 1, 2x1
            0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F, // scale 1.0
            0x38, 0x01, // primary
            0x4A, 0x06, // placement, 6 bytes
            0x08, 0x01, // display 1
            0x10, 0x01, // edge: left
            0x18, 0x03, // offset -2 (zigzag)
        ]
    );
    assert_eq!(decode_control(&bytes).unwrap(), m);
}

#[test]
fn hello_with_a_centred_automatic_placement() {
    let m = hello(Some(Placement {
        display: None,
        edge: Edge::Bottom,
        offset: None,
    }));
    let bytes = encode_control(&m);
    // Display 0 and an absent offset are left out: only the edge remains.
    assert_eq!(bytes[bytes.len() - 4..], [0x4A, 0x02, 0x10, 0x04]);
    assert_eq!(decode_control(&bytes).unwrap(), m);
    // No placement, no tag 9.
    let plain = encode_control(&hello(None));
    assert!(!plain.contains(&0x4A));
    assert_eq!(plain.len(), bytes.len() - 4);
}
