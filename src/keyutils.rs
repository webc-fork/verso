//! Convert winit keyboard events to the `keyboard_types` events Servo expects.

use keyboard_types::{Code, Key, KeyState, Location, Modifiers, NamedKey};
use winit::{
    event::KeyEvent,
    keyboard::{
        Key as WinitKey, KeyCode as WinitKeyCode, KeyLocation as WinitKeyLocation, ModifiersState,
        NamedKey as WinitNamedKey, PhysicalKey,
    },
};

/// Convert a winit key event into the `KeyboardEvent` Servo consumes.
pub fn keyboard_event_from_winit(
    key_event: &KeyEvent,
    modifiers: ModifiersState,
) -> keyboard_types::KeyboardEvent {
    keyboard_types::KeyboardEvent {
        state: key_state_from_winit(key_event),
        key: key_from_winit(key_event),
        code: code_from_winit(key_event),
        location: location_from_winit(key_event),
        modifiers: modifiers_from_winit(modifiers),
        repeat: false,
        is_composing: false,
    }
}

fn key_state_from_winit(key_event: &KeyEvent) -> KeyState {
    match key_event.state {
        winit::event::ElementState::Pressed => KeyState::Down,
        winit::event::ElementState::Released => KeyState::Up,
    }
}

fn key_from_winit(key_event: &KeyEvent) -> Key {
    match &key_event.logical_key {
        WinitKey::Named(WinitNamedKey::Space) => Key::Character(" ".to_string()),
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(named) => Key::Named(named_key_from_winit(named)),
        WinitKey::Unidentified(_) => Key::Named(NamedKey::Unidentified),
        WinitKey::Dead(_) => Key::Named(NamedKey::Dead),
    }
}

fn named_key_from_winit(named: &WinitNamedKey) -> NamedKey {
    use WinitNamedKey as W;
    use keyboard_types::NamedKey as K;
    match named {
        W::Alt => K::Alt,
        W::AltGraph => K::AltGraph,
        W::CapsLock => K::CapsLock,
        W::Control => K::Control,
        W::Fn => K::Fn,
        W::FnLock => K::FnLock,
        // `Hyper` and `Super` are legacy names for the `Meta` key in the UI Events spec.
        W::Hyper | W::Meta | W::Super => K::Meta,
        W::NumLock => K::NumLock,
        W::ScrollLock => K::ScrollLock,
        W::Shift => K::Shift,
        W::Symbol => K::Symbol,
        W::SymbolLock => K::SymbolLock,
        W::Enter => K::Enter,
        W::Tab => K::Tab,
        W::ArrowDown => K::ArrowDown,
        W::ArrowLeft => K::ArrowLeft,
        W::ArrowRight => K::ArrowRight,
        W::ArrowUp => K::ArrowUp,
        W::End => K::End,
        W::Home => K::Home,
        W::PageDown => K::PageDown,
        W::PageUp => K::PageUp,
        W::Backspace => K::Backspace,
        W::Clear => K::Clear,
        W::Copy => K::Copy,
        W::CrSel => K::CrSel,
        W::Cut => K::Cut,
        W::Delete => K::Delete,
        W::EraseEof => K::EraseEof,
        W::ExSel => K::ExSel,
        W::Insert => K::Insert,
        W::Paste => K::Paste,
        W::Redo => K::Redo,
        W::Undo => K::Undo,
        W::Attn => K::Attn,
        W::Cancel => K::Cancel,
        W::ContextMenu => K::ContextMenu,
        W::Escape => K::Escape,
        W::Execute => K::Execute,
        W::Find => K::Find,
        W::Help => K::Help,
        W::Pause => K::Pause,
        W::Play => K::Play,
        W::Props => K::Props,
        W::Select => K::Select,
        W::ZoomIn => K::ZoomIn,
        W::ZoomOut => K::ZoomOut,
        W::BrightnessDown => K::BrightnessDown,
        W::BrightnessUp => K::BrightnessUp,
        W::F1 => K::F1,
        W::F2 => K::F2,
        W::F3 => K::F3,
        W::F4 => K::F4,
        W::F5 => K::F5,
        W::F6 => K::F6,
        W::F7 => K::F7,
        W::F8 => K::F8,
        W::F9 => K::F9,
        W::F10 => K::F10,
        W::F11 => K::F11,
        W::F12 => K::F12,
        W::PrintScreen => K::PrintScreen,
        _ => K::Unidentified,
    }
}

fn code_from_winit(key_event: &KeyEvent) -> Code {
    let PhysicalKey::Code(winit_code) = key_event.physical_key else {
        return Code::Unidentified;
    };
    use WinitKeyCode as W;
    use keyboard_types::Code as K;
    match winit_code {
        W::KeyA => K::KeyA,
        W::KeyB => K::KeyB,
        W::KeyC => K::KeyC,
        W::KeyD => K::KeyD,
        W::KeyE => K::KeyE,
        W::KeyF => K::KeyF,
        W::KeyG => K::KeyG,
        W::KeyH => K::KeyH,
        W::KeyI => K::KeyI,
        W::KeyJ => K::KeyJ,
        W::KeyK => K::KeyK,
        W::KeyL => K::KeyL,
        W::KeyM => K::KeyM,
        W::KeyN => K::KeyN,
        W::KeyO => K::KeyO,
        W::KeyP => K::KeyP,
        W::KeyQ => K::KeyQ,
        W::KeyR => K::KeyR,
        W::KeyS => K::KeyS,
        W::KeyT => K::KeyT,
        W::KeyU => K::KeyU,
        W::KeyV => K::KeyV,
        W::KeyW => K::KeyW,
        W::KeyX => K::KeyX,
        W::KeyY => K::KeyY,
        W::KeyZ => K::KeyZ,
        W::Backquote => K::Backquote,
        W::Backslash => K::Backslash,
        W::BracketLeft => K::BracketLeft,
        W::BracketRight => K::BracketRight,
        W::Comma => K::Comma,
        W::Digit0 => K::Digit0,
        W::Digit1 => K::Digit1,
        W::Digit2 => K::Digit2,
        W::Digit3 => K::Digit3,
        W::Digit4 => K::Digit4,
        W::Digit5 => K::Digit5,
        W::Digit6 => K::Digit6,
        W::Digit7 => K::Digit7,
        W::Digit8 => K::Digit8,
        W::Digit9 => K::Digit9,
        W::Equal => K::Equal,
        W::Minus => K::Minus,
        W::Period => K::Period,
        W::Quote => K::Quote,
        W::Semicolon => K::Semicolon,
        W::Slash => K::Slash,
        W::AltLeft => K::AltLeft,
        W::AltRight => K::AltRight,
        W::Backspace => K::Backspace,
        W::CapsLock => K::CapsLock,
        W::ContextMenu => K::ContextMenu,
        W::ControlLeft => K::ControlLeft,
        W::ControlRight => K::ControlRight,
        W::Delete => K::Delete,
        W::ArrowDown => K::ArrowDown,
        W::End => K::End,
        W::Enter => K::Enter,
        W::Escape => K::Escape,
        W::F1 => K::F1,
        W::F2 => K::F2,
        W::F3 => K::F3,
        W::F4 => K::F4,
        W::F5 => K::F5,
        W::F6 => K::F6,
        W::F7 => K::F7,
        W::F8 => K::F8,
        W::F9 => K::F9,
        W::F10 => K::F10,
        W::F11 => K::F11,
        W::F12 => K::F12,
        W::Home => K::Home,
        W::Insert => K::Insert,
        W::ArrowLeft => K::ArrowLeft,
        W::SuperLeft => K::MetaLeft,
        W::SuperRight => K::MetaRight,
        W::PageDown => K::PageDown,
        W::PageUp => K::PageUp,
        W::ArrowRight => K::ArrowRight,
        W::ShiftLeft => K::ShiftLeft,
        W::ShiftRight => K::ShiftRight,
        W::Tab => K::Tab,
        W::ArrowUp => K::ArrowUp,
        W::PrintScreen => K::PrintScreen,
        W::ScrollLock => K::ScrollLock,
        W::Pause => K::Pause,
        W::NumLock => K::NumLock,
        _ => K::Unidentified,
    }
}

fn location_from_winit(key_event: &KeyEvent) -> Location {
    match key_event.location {
        WinitKeyLocation::Standard => Location::Standard,
        WinitKeyLocation::Left => Location::Left,
        WinitKeyLocation::Right => Location::Right,
        WinitKeyLocation::Numpad => Location::Numpad,
    }
}

fn modifiers_from_winit(mods: ModifiersState) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::CONTROL, mods.control_key());
    modifiers.set(Modifiers::SHIFT, mods.shift_key());
    modifiers.set(Modifiers::ALT, mods.alt_key());
    modifiers.set(Modifiers::META, mods.super_key());
    modifiers
}
