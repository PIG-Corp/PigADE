//! Pure mouse reporting encoder for acknowledged Alacritty terminal modes.
//!
//! The wire formats follow xterm's primary control-sequence reference, §Mouse
//! Tracking: <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#Mouse_Tracking>.
//! In this core, `MOUSE_REPORT_CLICK` is DECSET 1000 tracking; there is no X10
//! tracking bit (DECSET 9). “Legacy” below names the CSI M byte encoding used
//! by 1000/1002/1003, not X10's press-only tracking semantics.

// Mirrored from vendor/alacritty_terminal/src/term/mod.rs. Keep these values
// aligned with the selected state.2 terminal profile.
pub const MOUSE_REPORT_CLICK: u32 = 1 << 3; // DECSET 1000.
pub const SGR_MOUSE: u32 = 1 << 5; // DECSET 1006.
pub const MOUSE_MOTION: u32 = 1 << 6; // DECSET 1003.
pub const MOUSE_DRAG: u32 = 1 << 13; // DECSET 1002.
pub const UTF8_MOUSE: u32 = 1 << 14; // DECSET 1005.

const MOUSE_MODE: u32 = MOUSE_REPORT_CLICK | MOUSE_MOTION | MOUSE_DRAG;
const MAX_LEGACY_COORDINATE: usize = 223; // 1-based, before adding the 32-byte offset.
const MAX_UTF8_COORDINATE: usize = 2015; // 1-based, before adding the 32-byte offset.
const MAX_SGR_COLUMNS: usize = 1024; // Current host Geometry limit.
const MAX_SGR_ROWS: usize = 512;
/// Bound each wheel event; excess whole lines in one input are discarded.
pub const MAX_WHEEL_STEPS_PER_EVENT: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    const fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }

    const fn bit(self) -> u8 {
        1 << self.code()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    Up,
    Down,
}

/// Zero-based terminal cell coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift is reserved for local terminal selection and is never encoded.
    pub shift: bool,
    /// Encoded as the xterm Meta modifier bit (8).
    pub alt: bool,
    /// Encoded as the xterm Control modifier bit (16).
    pub control: bool,
}

/// State needed to report drag motion with a stable pressed button and to
/// suppress duplicate same-cell motion reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Encoder {
    pressed_buttons: u8,
    drag_button: Option<Button>,
    last_pointer_position: Option<Position>,
    last_motion_signature: Option<MotionSignature>,
    last_event_was_motion: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MotionSignature {
    position: Position,
    code: u16,
    tracking_mode: u32,
    encoding_mode: u32,
}

impl Encoder {
    /// Encode a press if the acknowledged mode enables mouse reporting.
    pub fn press(
        &mut self,
        button: Button,
        position: Position,
        modifiers: Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        if modifiers.shift {
            return None;
        }
        tracking_mode(modes)?;
        let bytes = encode_event(
            u16::from(button.code()) + modifier_bits(modifiers),
            position,
            false,
            modes,
        )?;
        self.pressed_buttons |= button.bit();
        if self.drag_button.is_none() {
            self.drag_button = Some(button);
        }
        self.last_pointer_position = Some(position);
        self.last_event_was_motion = false;
        Some(bytes)
    }

    /// Encode a matching release. Shift still opts out of new local selection,
    /// but an already-reported press is released to avoid leaving the app stuck.
    pub fn release(
        &mut self,
        button: Button,
        position: Position,
        modifiers: Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        let was_pressed = self.pressed_buttons & button.bit() != 0;
        if !was_pressed {
            return None;
        }
        self.pressed_buttons &= !button.bit();
        if self.drag_button == Some(button) {
            self.drag_button = [Button::Left, Button::Middle, Button::Right]
                .into_iter()
                .find(|candidate| self.pressed_buttons & candidate.bit() != 0);
        }
        self.last_pointer_position = Some(position);
        self.last_event_was_motion = false;

        tracking_mode(modes)?;
        let sgr = modes & SGR_MOUSE != 0;
        let code = if sgr { button.code() } else { 3 };
        encode_event(
            u16::from(code) + modifier_bits(modifiers),
            position,
            true,
            modes,
        )
    }

    /// Encode one cell-motion event for 1002 drag or 1003 any-motion modes.
    /// Repeated motion within the same cell is coalesced.
    pub fn motion(
        &mut self,
        position: Position,
        modifiers: Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        if modifiers.shift {
            return None;
        }
        let tracking = tracking_mode(modes)?;
        let code = match tracking {
            TrackingMode::Click => return None,
            TrackingMode::Drag => self.drag_button?.code(),
            TrackingMode::Any => self.drag_button.map_or(3, Button::code),
        };
        let code = u16::from(code) + 32 + modifier_bits(modifiers);
        let signature = MotionSignature {
            position,
            code,
            tracking_mode: modes & MOUSE_MODE,
            encoding_mode: modes & (SGR_MOUSE | UTF8_MOUSE),
        };
        if self.last_pointer_position == Some(position)
            && (!self.last_event_was_motion || self.last_motion_signature == Some(signature))
        {
            return None;
        }
        let bytes = encode_event(code, position, false, modes)?;
        self.last_pointer_position = Some(position);
        self.last_motion_signature = Some(signature);
        self.last_event_was_motion = true;
        Some(bytes)
    }

    /// Encode a wheel step under any active mouse-tracking mode.
    pub fn wheel(
        &mut self,
        wheel: Wheel,
        position: Position,
        modifiers: Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        if modifiers.shift {
            return None;
        }
        tracking_mode(modes)?;
        let code = match wheel {
            Wheel::Up => 64,
            Wheel::Down => 65,
        };
        encode_event(code + modifier_bits(modifiers), position, false, modes)
    }

    /// Whether a button press was reported and still needs a matching release.
    pub fn has_pressed(&self) -> bool {
        self.pressed_buttons != 0
    }

    /// Encode releases for every remembered button in stable left/middle/right
    /// order. State is cleared even when the acknowledged mode or coordinates
    /// cannot encode releases, so callers can use this to cancel focus/session
    /// state without leaving a phantom pressed button.
    pub fn release_all(
        &mut self,
        position: Position,
        modifiers: Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        if !self.has_pressed() {
            return None;
        }
        if tracking_mode(modes).is_none() {
            self.reset();
            return None;
        }

        let mut reports = Vec::new();
        for button in [Button::Left, Button::Middle, Button::Right] {
            if self.pressed_buttons & button.bit() == 0 {
                continue;
            }
            let code = if modes & SGR_MOUSE != 0 {
                button.code()
            } else {
                3
            };
            let Some(encoded) = encode_event(
                u16::from(code) + modifier_bits(modifiers),
                position,
                true,
                modes,
            ) else {
                self.reset();
                return None;
            };
            reports.extend(encoded);
        }

        self.reset();
        Some(reports)
    }

    /// Clear any remembered physical button state, e.g. when the pointer leaves
    /// the terminal surface or its owning session changes.
    pub fn reset(&mut self) {
        self.pressed_buttons = 0;
        self.drag_button = None;
        self.last_pointer_position = None;
        self.last_motion_signature = None;
        self.last_event_was_motion = false;
    }
}

/// Bounded conversion of signed line deltas into wheel steps. Clone this value
/// before `take_steps` and retain the clone only when the corresponding input
/// event is admitted by the host queue. Non-finite input is ignored without
/// changing the fractional remainder. At most 32 whole steps are emitted from
/// one call; further whole steps in that raw delta are discarded. The signed
/// fractional remainder is retained (positive means up, negative means down)
/// and should be reset on session changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelAccumulator {
    remainder: f32,
}

impl WheelAccumulator {
    pub fn take_steps(&mut self, line_delta: f32) -> Vec<Wheel> {
        if !line_delta.is_finite() {
            return Vec::new();
        }

        let total = self.remainder + line_delta;
        if !total.is_finite() {
            self.remainder = 0.0;
            return Vec::new();
        }

        let step_count = total.abs().floor().min(MAX_WHEEL_STEPS_PER_EVENT as f32) as usize;
        self.remainder = total - total.trunc();
        let wheel = if total.is_sign_negative() {
            Wheel::Down
        } else {
            Wheel::Up
        };
        vec![wheel; step_count]
    }

    pub fn reset(&mut self) {
        self.remainder = 0.0;
    }

    #[cfg(test)]
    pub fn remainder(&self) -> f32 {
        self.remainder
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackingMode {
    Click,
    Drag,
    Any,
}

fn tracking_mode(modes: u32) -> Option<TrackingMode> {
    if modes & MOUSE_MOTION != 0 {
        Some(TrackingMode::Any)
    } else if modes & MOUSE_DRAG != 0 {
        Some(TrackingMode::Drag)
    } else if modes & MOUSE_REPORT_CLICK != 0 {
        Some(TrackingMode::Click)
    } else {
        None
    }
}

fn modifier_bits(modifiers: Modifiers) -> u16 {
    u16::from(modifiers.alt) * 8 + u16::from(modifiers.control) * 16
}

fn encode_event(code: u16, position: Position, release: bool, modes: u32) -> Option<Vec<u8>> {
    let x = position.column.checked_add(1)?;
    let y = position.row.checked_add(1)?;

    if modes & SGR_MOUSE != 0 {
        if x > MAX_SGR_COLUMNS || y > MAX_SGR_ROWS {
            return None;
        }
        return Some(
            format!(
                "\x1b[<{code};{x};{}{ending}",
                y,
                ending = if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        );
    }

    let (x, y) = if modes & UTF8_MOUSE != 0 {
        if x > MAX_UTF8_COORDINATE || y > MAX_UTF8_COORDINATE {
            return None;
        }
        (x + 32, y + 32)
    } else {
        if x > MAX_LEGACY_COORDINATE || y > MAX_LEGACY_COORDINATE {
            return None;
        }
        (x + 32, y + 32)
    };

    let code = u32::from(code).checked_add(32)?;
    let mut bytes = b"\x1b[M".to_vec();
    if modes & UTF8_MOUSE != 0 {
        push_utf8_codepoint(&mut bytes, code)?;
        push_utf8_codepoint(&mut bytes, u32::try_from(x).ok()?)?;
        push_utf8_codepoint(&mut bytes, u32::try_from(y).ok()?)?;
    } else {
        bytes.push(u8::try_from(code).ok()?);
        bytes.push(u8::try_from(x).ok()?);
        bytes.push(u8::try_from(y).ok()?);
    }
    Some(bytes)
}

fn push_utf8_codepoint(bytes: &mut Vec<u8>, codepoint: u32) -> Option<()> {
    let character = char::from_u32(codepoint)?;
    let mut buffer = [0; 4];
    bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: u32 = MOUSE_REPORT_CLICK;
    const SGR: u32 = MOUSE_REPORT_CLICK | SGR_MOUSE;
    const UTF8: u32 = MOUSE_REPORT_CLICK | UTF8_MOUSE;
    const DRAG: u32 = MOUSE_DRAG;
    const ANY: u32 = MOUSE_MOTION;
    const POS: Position = Position { row: 4, column: 9 };

    fn bytes(value: Option<Vec<u8>>) -> Vec<u8> {
        value.expect("expected a mouse report")
    }

    #[test]
    fn button_presses_and_releases_use_exact_sgr_bytes() {
        for (button, code) in [(Button::Left, 0), (Button::Middle, 1), (Button::Right, 2)] {
            let mut encoder = Encoder::default();
            assert_eq!(
                bytes(encoder.press(button, POS, Modifiers::default(), SGR)),
                format!("\x1b[<{code};10;5M").into_bytes()
            );
            assert_eq!(
                bytes(encoder.release(button, POS, Modifiers::default(), SGR)),
                format!("\x1b[<{code};10;5m").into_bytes()
            );
        }
    }

    #[test]
    fn legacy_release_and_wheel_use_x10_byte_layout() {
        let mut encoder = Encoder::default();
        assert_eq!(
            bytes(encoder.press(Button::Left, POS, Modifiers::default(), LEGACY)),
            b"\x1b[M *%" // Cb=32, x=42, y=37.
        );
        assert_eq!(
            bytes(encoder.release(Button::Left, POS, Modifiers::default(), LEGACY)),
            b"\x1b[M#*%" // Cb=35 (release), x=42, y=37.
        );
        assert_eq!(
            bytes(encoder.wheel(Wheel::Up, POS, Modifiers::default(), LEGACY)),
            b"\x1b[M`*%" // Cb=96 (wheel up), x=42, y=37.
        );
        assert_eq!(
            bytes(encoder.wheel(Wheel::Down, POS, Modifiers::default(), LEGACY)),
            b"\x1b[Ma*%" // Cb=97 (wheel down), x=42, y=37.
        );
    }

    #[test]
    fn alt_and_control_modifiers_are_preserved_but_shift_selects_locally() {
        let mut encoder = Encoder::default();
        let modifiers = Modifiers {
            alt: true,
            control: true,
            shift: false,
        };
        assert_eq!(
            bytes(encoder.press(Button::Right, POS, modifiers, SGR)),
            b"\x1b[<26;10;5M"
        );
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(encoder.press(Button::Left, POS, shift, SGR), None);
        assert_eq!(
            encoder.motion(Position { row: 5, column: 9 }, shift, DRAG),
            None
        );
        // A locally shifted release still closes the previously reported drag.
        assert_eq!(
            bytes(encoder.release(Button::Right, POS, shift, SGR)),
            b"\x1b[<2;10;5m"
        );
    }

    #[test]
    fn drag_and_any_motion_track_pressed_button_and_deduplicate_cells() {
        let next = Position { row: 4, column: 10 };
        let mut encoder = Encoder::default();
        assert_eq!(
            encoder.motion(next, Modifiers::default(), DRAG),
            None,
            "drag mode needs a pressed button"
        );
        encoder.press(Button::Middle, POS, Modifiers::default(), DRAG);
        assert_eq!(encoder.motion(POS, Modifiers::default(), DRAG), None);
        assert_eq!(
            bytes(encoder.motion(next, Modifiers::default(), DRAG)),
            b"\x1b[MA+%" // middle button 1 + motion 32 + byte offset 32.
        );
        assert_eq!(encoder.motion(next, Modifiers::default(), DRAG), None);

        let mut encoder = Encoder::default();
        assert_eq!(
            bytes(encoder.motion(POS, Modifiers::default(), ANY)),
            b"\x1b[MC*%" // no button (3) + motion 32, then 1-based x/y + 32.
        );
        encoder.press(Button::Right, POS, Modifiers::default(), ANY);
        assert_eq!(
            bytes(encoder.motion(next, Modifiers::default(), ANY)),
            b"\x1b[MB+%" // right button 2 + motion 32.
        );
    }

    #[test]
    fn disabled_and_click_only_motion_are_ignored() {
        let mut encoder = Encoder::default();
        assert_eq!(
            encoder.press(Button::Left, POS, Modifiers::default(), 0),
            None
        );
        assert_eq!(encoder.wheel(Wheel::Up, POS, Modifiers::default(), 0), None);
        assert_eq!(encoder.motion(POS, Modifiers::default(), LEGACY), None);
    }

    #[test]
    fn legacy_and_utf8_coordinate_limits_reject_without_wrapping() {
        let mut encoder = Encoder::default();
        let max_legacy = Position {
            row: 222,
            column: 222,
        };
        assert!(
            encoder
                .press(Button::Left, max_legacy, Modifiers::default(), LEGACY)
                .is_some()
        );
        assert_eq!(
            encoder.press(
                Button::Left,
                Position {
                    row: 223,
                    column: 1
                },
                Modifiers::default(),
                LEGACY
            ),
            None
        );
        let max_utf8 = Position {
            row: 2014,
            column: 2014,
        };
        assert!(
            encoder
                .press(Button::Left, max_utf8, Modifiers::default(), UTF8)
                .is_some()
        );
        assert_eq!(
            encoder.press(
                Button::Left,
                Position {
                    row: 2015,
                    column: 1
                },
                Modifiers::default(),
                UTF8
            ),
            None
        );
        assert!(
            encoder
                .press(
                    Button::Left,
                    Position {
                        row: 511,
                        column: 1023
                    },
                    Modifiers::default(),
                    SGR,
                )
                .is_some()
        );
        assert_eq!(
            encoder.press(
                Button::Left,
                Position {
                    row: 0,
                    column: 1024
                },
                Modifiers::default(),
                SGR
            ),
            None
        );
        assert_eq!(
            encoder.press(
                Button::Left,
                Position {
                    row: usize::MAX,
                    column: 0
                },
                Modifiers::default(),
                SGR
            ),
            None
        );
    }

    #[test]
    fn utf8_encoding_is_used_for_legacy_coordinates_when_1005_is_enabled() {
        let mut encoder = Encoder::default();
        let position = Position {
            row: 200,
            column: 180,
        };
        let encoded = bytes(encoder.press(Button::Left, position, Modifiers::default(), UTF8));
        assert_eq!(&encoded[..3], b"\x1b[M");
        assert!(std::str::from_utf8(&encoded[3..]).is_ok());
        assert_eq!(encoded.len(), 8); // One Cb byte, two two-byte coordinates.
    }

    #[test]
    fn release_clears_button_state_and_reset_clears_all_motion_state() {
        let mut encoder = Encoder::default();
        encoder.press(Button::Left, POS, Modifiers::default(), DRAG);
        assert!(
            encoder
                .release(Button::Left, POS, Modifiers::default(), DRAG)
                .is_some()
        );
        assert_eq!(
            encoder.motion(Position { row: 4, column: 11 }, Modifiers::default(), DRAG),
            None
        );
        encoder.press(Button::Right, POS, Modifiers::default(), ANY);
        encoder.reset();
        assert_eq!(
            encoder.motion(Position { row: 4, column: 11 }, Modifiers::default(), ANY),
            Some(b"\x1b[MC,%".to_vec())
        );
    }

    #[test]
    fn release_all_emits_bounded_ordered_releases_and_clears_state() {
        let mut encoder = Encoder::default();
        encoder.press(Button::Right, POS, Modifiers::default(), SGR);
        encoder.press(Button::Left, POS, Modifiers::default(), SGR);
        encoder.press(Button::Middle, POS, Modifiers::default(), SGR);
        assert!(encoder.has_pressed());
        assert_eq!(
            bytes(encoder.release_all(POS, Modifiers::default(), SGR)),
            b"\x1b[<0;10;5m\x1b[<1;10;5m\x1b[<2;10;5m"
        );
        assert!(!encoder.has_pressed());
        assert_eq!(encoder.release_all(POS, Modifiers::default(), SGR), None);

        encoder.press(Button::Left, POS, Modifiers::default(), SGR);
        assert_eq!(encoder.release_all(POS, Modifiers::default(), 0), None);
        assert!(!encoder.has_pressed());
    }

    #[test]
    fn motion_deduplication_distinguishes_modifiers_buttons_and_modes() {
        let first = Position { row: 5, column: 10 };
        let second = Position { row: 5, column: 11 };
        let mut encoder = Encoder::default();
        encoder.press(Button::Left, POS, Modifiers::default(), ANY);
        assert_eq!(
            bytes(encoder.motion(first, Modifiers::default(), ANY)),
            b"\x1b[M@+&"
        );
        assert_eq!(encoder.motion(first, Modifiers::default(), ANY), None);
        assert_eq!(
            bytes(encoder.motion(
                first,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                ANY,
            )),
            b"\x1b[MH+&"
        );

        // Move away and switch the held button; the same destination now has
        // a different motion code and therefore must be reported.
        encoder.motion(second, Modifiers::default(), ANY);
        encoder.press(Button::Right, POS, Modifiers::default(), ANY);
        encoder.release(Button::Left, POS, Modifiers::default(), ANY);
        assert_eq!(
            bytes(encoder.motion(second, Modifiers::default(), ANY)),
            b"\x1b[MB,&"
        );

        // The encoding mode is part of the signature even at the same cell.
        assert_eq!(
            bytes(encoder.motion(second, Modifiers::default(), ANY | SGR_MOUSE)),
            b"\x1b[<34;12;6M"
        );
    }

    #[test]
    fn wheel_accumulator_is_finite_bounded_and_resettable() {
        let mut accumulator = WheelAccumulator::default();
        assert!(accumulator.take_steps(0.5).is_empty());
        assert_eq!(accumulator.remainder(), 0.5);
        assert_eq!(accumulator.take_steps(0.75), vec![Wheel::Up]);
        assert_eq!(accumulator.remainder(), 0.25);
        assert_eq!(accumulator.take_steps(-2.5), vec![Wheel::Down; 2]);
        assert_eq!(accumulator.remainder(), -0.25);

        let huge = accumulator.take_steps(10_000.5);
        assert_eq!(huge.len(), MAX_WHEEL_STEPS_PER_EVENT);
        assert!(huge.iter().all(|wheel| *wheel == Wheel::Up));
        assert_eq!(accumulator.remainder(), 0.25);

        let remainder = accumulator.remainder();
        assert!(accumulator.take_steps(f32::NAN).is_empty());
        assert!(accumulator.take_steps(f32::INFINITY).is_empty());
        assert_eq!(accumulator.remainder(), remainder);
        accumulator.reset();
        assert_eq!(accumulator.remainder(), 0.0);
    }
}
