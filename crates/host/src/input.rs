//! Display mode input: turns the iPad's `Pointer` messages into Windows
//! pointer frames (touch, pen) and mouse actions.
//!
//! Windows touch injection is frame based: every injected frame must list all
//! contacts currently down, so this keeps per-contact state. The mapping is
//! platform independent; `inject` (Windows only) performs it.

use std::collections::BTreeMap;

use padbridge_protocol::{Message, Phase, PointerKind};

/// Where the streamed monitor sits on the Windows desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenRect {
    fn to_pixels(self, x: f32, y: f32) -> (i32, i32) {
        let px = (x.clamp(0.0, 1.0) * (self.width.saturating_sub(1)) as f32).round() as i32;
        let py = (y.clamp(0.0, 1.0) * (self.height.saturating_sub(1)) as f32).round() as i32;
        (self.left + px, self.top + py)
    }
}

/// Pointer flag set, mirroring Windows `POINTER_FLAG_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub down: bool,
    pub update: bool,
    pub up: bool,
    pub in_range: bool,
    pub in_contact: bool,
    pub canceled: bool,
    pub primary: bool,
}

impl Flags {
    const DOWN: Flags = Flags { down: true, in_range: true, in_contact: true, ..Flags::NONE };
    const MOVE: Flags = Flags { update: true, in_range: true, in_contact: true, ..Flags::NONE };
    const UP: Flags = Flags { up: true, ..Flags::NONE };
    const HOVER: Flags = Flags { update: true, in_range: true, ..Flags::NONE };
    const LEAVE: Flags = Flags { update: true, ..Flags::NONE };
    const NONE: Flags = Flags {
        down: false,
        update: false,
        up: false,
        in_range: false,
        in_contact: false,
        canceled: false,
        primary: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Small stable id (touch slots are 0..MAX_TOUCHES).
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub flags: Flags,
    /// 0..=1024 (Windows pen/touch pressure range).
    pub pressure: u32,
    /// Degrees, -90..=90.
    pub tilt_x: i32,
    pub tilt_y: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputAction {
    /// One injection: all current touch contacts.
    Touch(Vec<Contact>),
    Pen(Contact),
    MouseMove { x: i32, y: i32 },
    MouseButton { down: bool },
}

pub const MAX_TOUCHES: usize = 10;

#[derive(Debug, Clone, Copy)]
struct TouchState {
    slot: u32,
    x: i32,
    y: i32,
}

#[derive(Debug, Default)]
pub struct InputMapper {
    touches: BTreeMap<u32, TouchState>,
    primary: Option<u32>,
    pen_in_contact: bool,
    mouse_down: bool,
}

impl InputMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Maps one message; non-pointer messages give nothing.
    pub fn handle(&mut self, msg: &Message, screen: ScreenRect) -> Vec<InputAction> {
        let Message::Pointer { id, kind, phase, x, y, pressure, altitude, azimuth } = *msg else {
            return Vec::new();
        };
        let (px, py) = screen.to_pixels(x, y);
        match kind {
            PointerKind::Touch => self.touch(id, phase, px, py).into_iter().collect(),
            PointerKind::Pen => self.pen(phase, px, py, pressure, altitude, azimuth),
            PointerKind::Mouse => self.mouse(phase, px, py),
        }
    }

    fn touch(&mut self, id: u32, phase: Phase, x: i32, y: i32) -> Option<InputAction> {
        let (flags, remove) = match phase {
            Phase::Hover => return None,
            Phase::Down => (Flags::DOWN, false),
            Phase::Move if self.touches.contains_key(&id) => (Flags::MOVE, false),
            // A move for an unknown contact (e.g. its Down was lost): start it.
            Phase::Move => (Flags::DOWN, false),
            Phase::Up if self.touches.contains_key(&id) => (Flags::UP, true),
            Phase::Cancel if self.touches.contains_key(&id) => (Flags { canceled: true, ..Flags::UP }, true),
            Phase::Up | Phase::Cancel => return None,
        };
        if flags.down {
            if self.touches.len() >= MAX_TOUCHES && !self.touches.contains_key(&id) {
                return None;
            }
            let used: Vec<u32> = self.touches.values().map(|t| t.slot).collect();
            let slot = (0..MAX_TOUCHES as u32).find(|s| !used.contains(s)).unwrap_or(0);
            self.touches.insert(id, TouchState { slot, x, y });
            if self.primary.is_none() {
                self.primary = Some(id);
            }
        }
        if let Some(t) = self.touches.get_mut(&id) {
            t.x = x;
            t.y = y;
        }

        let contacts = self
            .touches
            .iter()
            .map(|(&cid, t)| Contact {
                id: t.slot,
                x: t.x,
                y: t.y,
                flags: Flags { primary: self.primary == Some(cid), ..if cid == id { flags } else { Flags::MOVE } },
                pressure: 0,
                tilt_x: 0,
                tilt_y: 0,
            })
            .collect();
        if remove {
            self.touches.remove(&id);
            if self.primary == Some(id) {
                self.primary = None;
            }
        }
        Some(InputAction::Touch(contacts))
    }

    fn pen(&mut self, phase: Phase, x: i32, y: i32, pressure: f32, altitude: f32, azimuth: f32) -> Vec<InputAction> {
        // Tilt from the stylus angle: 0 when upright, leaning along the azimuth.
        let lean = (90.0 - altitude.to_degrees()).clamp(0.0, 90.0);
        let contact = |flags: Flags, pressure: u32| Contact {
            id: 0,
            x,
            y,
            flags: Flags { primary: true, ..flags },
            pressure,
            tilt_x: (lean * azimuth.cos()).round() as i32,
            tilt_y: (lean * azimuth.sin()).round() as i32,
        };
        let pressure = (pressure.clamp(0.0, 1.0) * 1024.0) as u32;
        match phase {
            Phase::Hover if !self.pen_in_contact => vec![InputAction::Pen(contact(Flags::HOVER, 0))],
            Phase::Hover => Vec::new(),
            Phase::Down => {
                self.pen_in_contact = true;
                vec![InputAction::Pen(contact(Flags::DOWN, pressure.max(1)))]
            }
            Phase::Move if self.pen_in_contact => vec![InputAction::Pen(contact(Flags::MOVE, pressure.max(1)))],
            Phase::Move => {
                self.pen_in_contact = true;
                vec![InputAction::Pen(contact(Flags::DOWN, pressure.max(1)))]
            }
            Phase::Up | Phase::Cancel if self.pen_in_contact => {
                self.pen_in_contact = false;
                let canceled = phase == Phase::Cancel;
                vec![
                    InputAction::Pen(contact(Flags { in_range: true, canceled, ..Flags::UP }, 0)),
                    // The iPad only reports hover on some models: leave range so
                    // Windows hides the pen cursor; a later hover brings it back.
                    InputAction::Pen(contact(Flags::LEAVE, 0)),
                ]
            }
            // Hover ended.
            Phase::Up | Phase::Cancel => vec![InputAction::Pen(contact(Flags::LEAVE, 0))],
        }
    }

    fn mouse(&mut self, phase: Phase, x: i32, y: i32) -> Vec<InputAction> {
        let mut actions = vec![InputAction::MouseMove { x, y }];
        match phase {
            Phase::Down if !self.mouse_down => {
                self.mouse_down = true;
                actions.push(InputAction::MouseButton { down: true });
            }
            Phase::Up | Phase::Cancel if self.mouse_down => {
                self.mouse_down = false;
                actions.push(InputAction::MouseButton { down: false });
            }
            _ => {}
        }
        actions
    }
}

#[cfg(windows)]
pub use inject::Injector;

#[cfg(windows)]
mod inject {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Controls::{
        CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
        POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
    };
    use windows::Win32::UI::Input::Pointer::{
        InjectSyntheticPointerInput, POINTER_FLAGS, POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT,
        POINTER_FLAG_INRANGE, POINTER_FLAG_PRIMARY, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_INFO,
        POINTER_PEN_INFO, POINTER_TOUCH_INFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SetCursorPos, PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH,
    };

    use super::{Contact, Flags, InputAction, MAX_TOUCHES};
    use crate::BoxError;

    fn flags(f: Flags) -> POINTER_FLAGS {
        let mut out = POINTER_FLAGS(0);
        for (on, flag) in [
            (f.down, POINTER_FLAG_DOWN),
            (f.update, POINTER_FLAG_UPDATE),
            (f.up, POINTER_FLAG_UP),
            (f.in_range, POINTER_FLAG_INRANGE),
            (f.in_contact, POINTER_FLAG_INCONTACT),
            (f.canceled, POINTER_FLAG_CANCELED),
            (f.primary, POINTER_FLAG_PRIMARY),
        ] {
            if on {
                out |= flag;
            }
        }
        out
    }

    fn pointer_info(kind: POINTER_INPUT_TYPE, c: &Contact) -> POINTER_INFO {
        POINTER_INFO {
            pointerType: kind,
            pointerId: c.id,
            pointerFlags: flags(c.flags),
            ptPixelLocation: POINT { x: c.x, y: c.y },
            ..Default::default()
        }
    }

    /// Injects input through synthetic pointer devices (Windows 10 1809+).
    /// Injection into elevated (administrator) windows is blocked by Windows
    /// unless PadBridge itself runs elevated.
    pub struct Injector {
        touch: HSYNTHETICPOINTERDEVICE,
        pen: HSYNTHETICPOINTERDEVICE,
    }

    // The handles are plain device handles usable from any thread.
    unsafe impl Send for Injector {}

    impl Injector {
        pub fn new() -> Result<Self, BoxError> {
            unsafe {
                let touch = CreateSyntheticPointerDevice(PT_TOUCH, MAX_TOUCHES as u32, POINTER_FEEDBACK_DEFAULT)?;
                let pen = CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT)?;
                Ok(Self { touch, pen })
            }
        }

        pub fn apply(&self, action: &InputAction) -> Result<(), BoxError> {
            unsafe {
                match action {
                    InputAction::Touch(contacts) => {
                        let infos: Vec<POINTER_TYPE_INFO> = contacts
                            .iter()
                            .map(|c| POINTER_TYPE_INFO {
                                r#type: PT_TOUCH,
                                Anonymous: POINTER_TYPE_INFO_0 {
                                    touchInfo: POINTER_TOUCH_INFO {
                                        pointerInfo: pointer_info(PT_TOUCH, c),
                                        ..Default::default()
                                    },
                                },
                            })
                            .collect();
                        if !infos.is_empty() {
                            InjectSyntheticPointerInput(self.touch, &infos)?;
                        }
                    }
                    InputAction::Pen(c) => {
                        let info = POINTER_TYPE_INFO {
                            r#type: PT_PEN,
                            Anonymous: POINTER_TYPE_INFO_0 {
                                penInfo: POINTER_PEN_INFO {
                                    pointerInfo: pointer_info(PT_PEN, c),
                                    penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                                    pressure: c.pressure,
                                    tiltX: c.tilt_x,
                                    tiltY: c.tilt_y,
                                    ..Default::default()
                                },
                            },
                        };
                        InjectSyntheticPointerInput(self.pen, &[info])?;
                    }
                    InputAction::MouseMove { x, y } => SetCursorPos(*x, *y)?,
                    InputAction::MouseButton { down } => {
                        let input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dwFlags: if *down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP },
                                    ..Default::default()
                                },
                            },
                        };
                        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                    }
                }
            }
            Ok(())
        }
    }

    impl Drop for Injector {
        fn drop(&mut self) {
            unsafe {
                DestroySyntheticPointerDevice(self.touch);
                DestroySyntheticPointerDevice(self.pen);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: ScreenRect = ScreenRect { left: 1920, top: 0, width: 1001, height: 501 };

    fn ptr(id: u32, kind: PointerKind, phase: Phase, x: f32, y: f32) -> Message {
        Message::Pointer { id, kind, phase, x, y, pressure: 0.5, altitude: std::f32::consts::FRAC_PI_2, azimuth: 0.0 }
    }

    fn touches(actions: Vec<InputAction>) -> Vec<Contact> {
        match actions.as_slice() {
            [InputAction::Touch(c)] => c.clone(),
            other => panic!("expected one touch frame, got {other:?}"),
        }
    }

    #[test]
    fn maps_normalized_coordinates_onto_the_monitor() {
        let mut m = InputMapper::new();
        let c = touches(m.handle(&ptr(7, PointerKind::Touch, Phase::Down, 0.5, 1.0), SCREEN));
        assert_eq!((c[0].x, c[0].y), (1920 + 500, 500));
    }

    #[test]
    fn two_finger_frames_list_every_contact() {
        let mut m = InputMapper::new();
        let a = touches(m.handle(&ptr(100, PointerKind::Touch, Phase::Down, 0.1, 0.1), SCREEN));
        assert_eq!(a.len(), 1);
        assert!(a[0].flags.down && a[0].flags.primary);

        let b = touches(m.handle(&ptr(200, PointerKind::Touch, Phase::Down, 0.9, 0.9), SCREEN));
        assert_eq!(b.len(), 2);
        let first = b.iter().find(|c| c.id == 0).unwrap();
        let second = b.iter().find(|c| c.id == 1).unwrap();
        assert!(first.flags.update && first.flags.in_contact && first.flags.primary);
        assert!(second.flags.down && !second.flags.primary);

        let up = touches(m.handle(&ptr(100, PointerKind::Touch, Phase::Up, 0.1, 0.1), SCREEN));
        assert_eq!(up.len(), 2);
        assert!(up.iter().find(|c| c.id == 0).unwrap().flags.up);

        // The freed slot is reused; the remaining finger keeps slot 1.
        let c = touches(m.handle(&ptr(300, PointerKind::Touch, Phase::Down, 0.5, 0.5), SCREEN));
        assert_eq!(c.iter().map(|c| c.id).collect::<Vec<_>>(), vec![1, 0]);
    }

    #[test]
    fn stray_touch_events_are_tolerated() {
        let mut m = InputMapper::new();
        assert!(m.handle(&ptr(1, PointerKind::Touch, Phase::Up, 0.0, 0.0), SCREEN).is_empty());
        let c = touches(m.handle(&ptr(1, PointerKind::Touch, Phase::Move, 0.0, 0.0), SCREEN));
        assert!(c[0].flags.down, "a move without down starts the contact");
        let c = touches(m.handle(&ptr(1, PointerKind::Touch, Phase::Cancel, 0.0, 0.0), SCREEN));
        assert!(c[0].flags.up && c[0].flags.canceled);
    }

    #[test]
    fn pen_down_move_up_then_leaves_range() {
        let mut m = InputMapper::new();
        let down = m.handle(&ptr(1, PointerKind::Pen, Phase::Down, 0.5, 0.5), SCREEN);
        assert!(matches!(&down[..], [InputAction::Pen(c)] if c.flags.down && c.pressure == 512 && c.tilt_x == 0));
        let mv = m.handle(&ptr(1, PointerKind::Pen, Phase::Move, 0.6, 0.5), SCREEN);
        assert!(matches!(&mv[..], [InputAction::Pen(c)] if c.flags.in_contact && c.flags.update));
        let up = m.handle(&ptr(1, PointerKind::Pen, Phase::Up, 0.6, 0.5), SCREEN);
        assert!(matches!(&up[..], [InputAction::Pen(a), InputAction::Pen(b)]
            if a.flags.up && a.flags.in_range && b.flags.update && !b.flags.in_range));
    }

    #[test]
    fn pen_tilt_follows_altitude_and_azimuth() {
        let mut m = InputMapper::new();
        let msg = Message::Pointer {
            id: 1,
            kind: PointerKind::Pen,
            phase: Phase::Down,
            x: 0.5,
            y: 0.5,
            pressure: 1.0,
            altitude: 60f32.to_radians(),
            azimuth: 0.0,
        };
        let actions = m.handle(&msg, SCREEN);
        assert!(matches!(&actions[..], [InputAction::Pen(c)] if c.tilt_x == 30 && c.tilt_y == 0 && c.pressure == 1024));
    }

    #[test]
    fn mouse_clicks() {
        let mut m = InputMapper::new();
        let a = m.handle(&ptr(1, PointerKind::Mouse, Phase::Down, 0.0, 0.0), SCREEN);
        assert_eq!(a, vec![InputAction::MouseMove { x: 1920, y: 0 }, InputAction::MouseButton { down: true }]);
        let a = m.handle(&ptr(1, PointerKind::Mouse, Phase::Up, 0.0, 0.0), SCREEN);
        assert_eq!(a, vec![InputAction::MouseMove { x: 1920, y: 0 }, InputAction::MouseButton { down: false }]);
    }
}
