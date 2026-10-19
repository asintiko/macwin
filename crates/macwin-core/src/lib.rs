//! OS-independent core of macwin: domain vocabulary, edge-mapping geometry,
//! the generated HID keymap, modifier remapping, pressed-key tracking,
//! heartbeat timing, the emergency-chord detector, the controller state
//! machine (the [`Engine`]) and clipboard synchronisation.
//!
//! Pure Rust: no I/O, no threads and no clocks (time is always passed in).
#![forbid(unsafe_code)]

pub mod arrange;
pub mod clipboard;
pub mod engine;
pub mod geometry;
pub mod heartbeat;
pub mod keymap;
pub mod keys;
pub mod latency;
pub mod layout;
pub mod msg;
pub mod os;
pub mod remap;
pub mod safety;
pub mod types;

pub use engine::{
    Action, CloseReason, Engine, EngineConfig, LocalOutcome, PeerInfo, Timings, TransportRole,
};
pub use types::{
    CaptureFault, CapturedEvent, GeometryError, GrabCommand, Hid, InjectCommand, ModifierMask,
    MouseButton, Os, Point, PointF, Rect, RoleKind, ScreenInfo, Side, WheelUnit,
};
