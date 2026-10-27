/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Browser keyboard shortcuts. Add or change bindings in [`SHORTCUTS`] or
//! [`PLATFORM_SHORTCUTS`], and actions in [`BrowserAction`].

use keyboard_types::{Code, Key, KeyboardEvent, Modifiers, NamedKey};

use self::BrowserAction::*;
use self::ShortcutKey::{Char, Named};
use super::keyutils::{CMD_OR_ALT, CMD_OR_CONTROL};

/// Something the browser can do from a keyboard shortcut or the application menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserAction {
    NewTab,
    CloseTab,
    ReopenClosedTab,
    NextTab,
    PreviousTab,
    SelectTab(usize),
    SelectLastTab,
    MoveTabLeft,
    MoveTabRight,
    NewWindow,
    CloseWindow,
    Quit,
    Back,
    Forward,
    Reload,
    Home,
    FocusLocationBar,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    ToggleFullscreen,
    ExitDocumentFullscreen,
    History,
    Settings,
    Processes,
    ViewSource,
    ToggleDeveloperTools,
    ToggleAppMenu,
    CaptureWebRender,
    ToggleRenderTargetDebug,
    ToggleTextureCacheDebug,
    ToggleProfiler,
}

/// When a shortcut runs relative to the page that has keyboard focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Dispatch {
    /// Runs before the page, which never sees the key (Chrome's reserved shortcuts).
    Reserved,
    /// Runs after the page, unless it called `preventDefault()`.
    PageFirst,
}

#[derive(Clone, Copy)]
enum ShortcutKey {
    /// Matched without case against the typed character.
    Char(char),
    Named(NamedKey),
}

struct Shortcut {
    modifiers: Modifiers,
    key: ShortcutKey,
    action: BrowserAction,
    dispatch: Dispatch,
}

const fn reserved(modifiers: Modifiers, key: ShortcutKey, action: BrowserAction) -> Shortcut {
    Shortcut {
        modifiers,
        key,
        action,
        dispatch: Dispatch::Reserved,
    }
}

const fn page_first(modifiers: Modifiers, key: ShortcutKey, action: BrowserAction) -> Shortcut {
    Shortcut {
        modifiers,
        key,
        action,
        dispatch: Dispatch::PageFirst,
    }
}

const NONE: Modifiers = Modifiers::empty();
const SHIFT: Modifiers = Modifiers::SHIFT;
const CONTROL: Modifiers = Modifiers::CONTROL;
const CONTROL_SHIFT: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);
const CMD_OR_CONTROL_SHIFT: Modifiers = CMD_OR_CONTROL.union(Modifiers::SHIFT);

/// Shortcuts shared by all platforms. The first binding of an action is the one
/// shown in the application menu.
const SHORTCUTS: &[Shortcut] = &[
    reserved(CMD_OR_CONTROL, Char('t'), NewTab),
    reserved(CMD_OR_CONTROL, Char('w'), CloseTab),
    reserved(CMD_OR_CONTROL_SHIFT, Char('t'), ReopenClosedTab),
    reserved(CONTROL, Named(NamedKey::Tab), NextTab),
    reserved(CONTROL_SHIFT, Named(NamedKey::Tab), PreviousTab),
    reserved(CONTROL, Named(NamedKey::PageDown), NextTab),
    reserved(CONTROL, Named(NamedKey::PageUp), PreviousTab),
    reserved(CMD_OR_CONTROL, Char('n'), NewWindow),
    reserved(CMD_OR_CONTROL_SHIFT, Char('w'), CloseWindow),
    reserved(CMD_OR_CONTROL, Char('q'), Quit),
    page_first(CONTROL_SHIFT, Named(NamedKey::PageUp), MoveTabLeft),
    page_first(CONTROL_SHIFT, Named(NamedKey::PageDown), MoveTabRight),
    page_first(CMD_OR_CONTROL, Char('1'), SelectTab(0)),
    page_first(CMD_OR_CONTROL, Char('2'), SelectTab(1)),
    page_first(CMD_OR_CONTROL, Char('3'), SelectTab(2)),
    page_first(CMD_OR_CONTROL, Char('4'), SelectTab(3)),
    page_first(CMD_OR_CONTROL, Char('5'), SelectTab(4)),
    page_first(CMD_OR_CONTROL, Char('6'), SelectTab(5)),
    page_first(CMD_OR_CONTROL, Char('7'), SelectTab(6)),
    page_first(CMD_OR_CONTROL, Char('8'), SelectTab(7)),
    page_first(CMD_OR_CONTROL, Char('9'), SelectLastTab),
    page_first(CMD_OR_ALT, Named(NamedKey::ArrowLeft), Back),
    page_first(CMD_OR_ALT, Named(NamedKey::ArrowRight), Forward),
    page_first(CMD_OR_CONTROL, Char('r'), Reload),
    page_first(NONE, Named(NamedKey::F5), Reload),
    page_first(CMD_OR_CONTROL_SHIFT, Char('r'), Reload),
    page_first(SHIFT, Named(NamedKey::F5), Reload),
    page_first(CONTROL, Named(NamedKey::F5), Reload),
    page_first(CMD_OR_CONTROL, Char('l'), FocusLocationBar),
    page_first(CMD_OR_CONTROL, Char('k'), FocusLocationBar),
    page_first(CMD_OR_CONTROL, Char('='), ZoomIn),
    page_first(CMD_OR_CONTROL, Char('+'), ZoomIn),
    page_first(CMD_OR_CONTROL_SHIFT, Char('+'), ZoomIn),
    page_first(CMD_OR_CONTROL, Char('-'), ZoomOut),
    page_first(CMD_OR_CONTROL_SHIFT, Char('_'), ZoomOut),
    page_first(CMD_OR_CONTROL, Char('0'), ResetZoom),
    page_first(NONE, Named(NamedKey::F12), ToggleDeveloperTools),
    reserved(NONE, Named(NamedKey::Escape), ExitDocumentFullscreen),
    reserved(CONTROL, Named(NamedKey::F9), CaptureWebRender),
    reserved(CONTROL, Named(NamedKey::F10), ToggleRenderTargetDebug),
    reserved(CONTROL, Named(NamedKey::F11), ToggleTextureCacheDebug),
    reserved(CONTROL, Named(NamedKey::F12), ToggleProfiler),
];

#[cfg(target_os = "macos")]
const PLATFORM_SHORTCUTS: &[Shortcut] = &[
    reserved(
        Modifiers::META.union(Modifiers::ALT),
        Named(NamedKey::ArrowRight),
        NextTab,
    ),
    reserved(
        Modifiers::META.union(Modifiers::ALT),
        Named(NamedKey::ArrowLeft),
        PreviousTab,
    ),
    reserved(Modifiers::META.union(SHIFT), Char('}'), NextTab),
    reserved(Modifiers::META.union(SHIFT), Char('{'), PreviousTab),
    page_first(Modifiers::META, Char('['), Back),
    page_first(Modifiers::META, Char(']'), Forward),
    page_first(Modifiers::META.union(SHIFT), Char('h'), Home),
    page_first(Modifiers::META, Char('y'), History),
    page_first(Modifiers::META, Char(','), Settings),
    page_first(Modifiers::META.union(Modifiers::ALT), Char('u'), ViewSource),
    page_first(
        Modifiers::META.union(Modifiers::ALT),
        Char('i'),
        ToggleDeveloperTools,
    ),
    page_first(
        Modifiers::META.union(Modifiers::CONTROL),
        Char('f'),
        ToggleFullscreen,
    ),
];

#[cfg(not(target_os = "macos"))]
const PLATFORM_SHORTCUTS: &[Shortcut] = &[
    reserved(CONTROL, Named(NamedKey::F4), CloseTab),
    page_first(Modifiers::ALT, Named(NamedKey::Home), Home),
    page_first(Modifiers::ALT, Char('d'), FocusLocationBar),
    page_first(NONE, Named(NamedKey::F6), FocusLocationBar),
    page_first(CONTROL, Char('e'), FocusLocationBar),
    page_first(CONTROL, Char('h'), History),
    page_first(SHIFT, Named(NamedKey::Escape), Processes),
    page_first(CONTROL, Char('u'), ViewSource),
    page_first(CONTROL_SHIFT, Char('i'), ToggleDeveloperTools),
    page_first(Modifiers::ALT, Char('f'), ToggleAppMenu),
    page_first(Modifiers::ALT, Char('e'), ToggleAppMenu),
    page_first(NONE, Named(NamedKey::F10), ToggleAppMenu),
    page_first(NONE, Named(NamedKey::F11), ToggleFullscreen),
];

fn all_shortcuts() -> impl Iterator<Item = &'static Shortcut> {
    SHORTCUTS.iter().chain(PLATFORM_SHORTCUTS)
}

/// The action bound to `event`, pressed or released, and when it runs.
///
/// A character is first matched as typed, then, if that finds nothing, as the
/// key at the same place on a US keyboard, so that Ctrl+1 works on AZERTY.
pub(crate) fn shortcut_for(event: &KeyboardEvent) -> Option<(BrowserAction, Dispatch)> {
    let modifiers = event.modifiers &
        (Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::ALT | Modifiers::META);
    let find = |typed: &Key| {
        all_shortcuts()
            .find(|shortcut| shortcut.modifiers == modifiers && key_matches(shortcut.key, typed))
            .map(|shortcut| (shortcut.action, shortcut.dispatch))
    };
    find(&event.key).or_else(|| {
        let us_layout_key = us_layout_character(event)?;
        find(&Key::Character(us_layout_key.to_string()))
    })
}

fn key_matches(key: ShortcutKey, typed: &Key) -> bool {
    match (key, typed) {
        (ShortcutKey::Char(character), Key::Character(text)) => {
            let mut buffer = [0; 4];
            text.eq_ignore_ascii_case(character.encode_utf8(&mut buffer))
        },
        (ShortcutKey::Named(named), Key::Named(typed)) => named == *typed,
        _ => false,
    }
}

/// The letter or digit printed on the physical key on a US keyboard, when the
/// typed key is not already an ASCII letter or digit.
fn us_layout_character(event: &KeyboardEvent) -> Option<char> {
    match &event.key {
        Key::Character(text)
            if text.len() == 1 && text.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            return None;
        },
        Key::Character(_) | Key::Named(NamedKey::Unidentified) => {},
        Key::Named(_) => return None,
    }
    let character = match event.code {
        Code::Digit0 => '0',
        Code::Digit1 => '1',
        Code::Digit2 => '2',
        Code::Digit3 => '3',
        Code::Digit4 => '4',
        Code::Digit5 => '5',
        Code::Digit6 => '6',
        Code::Digit7 => '7',
        Code::Digit8 => '8',
        Code::Digit9 => '9',
        Code::KeyA => 'a',
        Code::KeyB => 'b',
        Code::KeyC => 'c',
        Code::KeyD => 'd',
        Code::KeyE => 'e',
        Code::KeyF => 'f',
        Code::KeyG => 'g',
        Code::KeyH => 'h',
        Code::KeyI => 'i',
        Code::KeyJ => 'j',
        Code::KeyK => 'k',
        Code::KeyL => 'l',
        Code::KeyM => 'm',
        Code::KeyN => 'n',
        Code::KeyO => 'o',
        Code::KeyP => 'p',
        Code::KeyQ => 'q',
        Code::KeyR => 'r',
        Code::KeyS => 's',
        Code::KeyT => 't',
        Code::KeyU => 'u',
        Code::KeyV => 'v',
        Code::KeyW => 'w',
        Code::KeyX => 'x',
        Code::KeyY => 'y',
        Code::KeyZ => 'z',
        _ => return None,
    };
    Some(character)
}

/// Whether a text field of the browser interface handles `key` itself, even when
/// a shortcut uses it (Cmd+Left moves the caret on macOS).
pub(crate) fn is_text_editing_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::ArrowLeft |
                NamedKey::ArrowRight |
                NamedKey::ArrowUp |
                NamedKey::ArrowDown |
                NamedKey::Home |
                NamedKey::End |
                NamedKey::Backspace |
                NamedKey::Delete
        )
    )
}

/// The first shortcut bound to `action`, written as menus show it.
pub(crate) fn shortcut_label(action: BrowserAction) -> Option<String> {
    let shortcut = all_shortcuts().find(|shortcut| shortcut.action == action)?;
    let key = match shortcut.key {
        ShortcutKey::Char(character) => character.to_ascii_uppercase().to_string(),
        ShortcutKey::Named(NamedKey::Escape) => "Esc".to_owned(),
        ShortcutKey::Named(NamedKey::ArrowLeft) => "Left".to_owned(),
        ShortcutKey::Named(NamedKey::ArrowRight) => "Right".to_owned(),
        ShortcutKey::Named(named) => named.to_string(),
    };
    let modifiers = shortcut.modifiers;
    let label: String = if cfg!(target_os = "macos") {
        [
            (Modifiers::CONTROL, "⌃"),
            (Modifiers::ALT, "⌥"),
            (Modifiers::SHIFT, "⇧"),
            (Modifiers::META, "⌘"),
        ]
        .iter()
        .filter(|(modifier, _)| modifiers.contains(*modifier))
        .map(|(_, symbol)| *symbol)
        .chain([key.as_str()])
        .collect()
    } else {
        [
            (Modifiers::CONTROL, "Ctrl"),
            (Modifiers::ALT, "Alt"),
            (Modifiers::SHIFT, "Shift"),
            (Modifiers::META, "Super"),
        ]
        .iter()
        .filter(|(modifier, _)| modifiers.contains(*modifier))
        .map(|(_, name)| *name)
        .chain([key.as_str()])
        .collect::<Vec<_>>()
        .join("+")
    };
    Some(label)
}

#[cfg(test)]
mod tests {
    use keyboard_types::{KeyState, Location};

    use super::*;

    fn event(modifiers: Modifiers, key: Key, code: Code) -> KeyboardEvent {
        KeyboardEvent {
            state: KeyState::Down,
            key,
            code,
            location: Location::Standard,
            modifiers,
            repeat: false,
            is_composing: false,
        }
    }

    fn typed(modifiers: Modifiers, text: &str, code: Code) -> Option<(BrowserAction, Dispatch)> {
        shortcut_for(&event(modifiers, Key::Character(text.into()), code))
    }

    fn named(modifiers: Modifiers, key: NamedKey) -> Option<(BrowserAction, Dispatch)> {
        shortcut_for(&event(modifiers, Key::Named(key), Code::Unidentified))
    }

    #[test]
    fn tab_switching_is_reserved() {
        assert_eq!(
            named(CONTROL, NamedKey::Tab),
            Some((NextTab, Dispatch::Reserved))
        );
        assert_eq!(
            named(CONTROL_SHIFT, NamedKey::Tab),
            Some((PreviousTab, Dispatch::Reserved))
        );
        assert_eq!(
            typed(CMD_OR_CONTROL_SHIFT, "T", Code::KeyT),
            Some((ReopenClosedTab, Dispatch::Reserved))
        );
    }

    #[test]
    fn letters_match_without_case() {
        assert_eq!(
            typed(CMD_OR_CONTROL, "t", Code::KeyT).map(|(a, _)| a),
            Some(NewTab)
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "T", Code::KeyT).map(|(a, _)| a),
            Some(NewTab)
        );
    }

    #[test]
    fn modifiers_must_match_exactly() {
        assert_eq!(named(NONE, NamedKey::Tab), None);
        assert_eq!(typed(NONE, "t", Code::KeyT), None);
        assert_eq!(
            typed(CMD_OR_CONTROL.union(Modifiers::ALT), "t", Code::KeyT),
            None
        );
    }

    #[test]
    fn lock_modifiers_are_ignored() {
        let modifiers = CMD_OR_CONTROL.union(Modifiers::NUM_LOCK);
        assert_eq!(
            typed(modifiers, "t", Code::KeyT).map(|(a, _)| a),
            Some(NewTab)
        );
    }

    #[test]
    fn page_can_override_most_shortcuts() {
        assert_eq!(
            typed(CMD_OR_CONTROL, "l", Code::KeyL),
            Some((FocusLocationBar, Dispatch::PageFirst))
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "1", Code::Digit1),
            Some((SelectTab(0), Dispatch::PageFirst))
        );
    }

    #[test]
    fn zoom_in_accepts_shifted_plus() {
        assert_eq!(
            typed(CMD_OR_CONTROL, "=", Code::Equal).map(|(a, _)| a),
            Some(ZoomIn)
        );
        assert_eq!(
            typed(CMD_OR_CONTROL_SHIFT, "+", Code::Equal).map(|(a, _)| a),
            Some(ZoomIn)
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "+", Code::NumpadAdd).map(|(a, _)| a),
            Some(ZoomIn)
        );
    }

    #[test]
    fn azerty_digit_row_selects_tabs() {
        assert_eq!(
            typed(CMD_OR_CONTROL, "&", Code::Digit1).map(|(a, _)| a),
            Some(SelectTab(0))
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "é", Code::Digit2).map(|(a, _)| a),
            Some(SelectTab(1))
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "_", Code::Digit8).map(|(a, _)| a),
            Some(SelectTab(7))
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "ç", Code::Digit9).map(|(a, _)| a),
            Some(SelectLastTab)
        );
        assert_eq!(
            typed(CMD_OR_CONTROL, "à", Code::Digit0).map(|(a, _)| a),
            Some(ResetZoom)
        );
    }

    #[test]
    fn typed_character_wins_over_key_position() {
        // AZERTY prints "-" on the 6 key: Ctrl+- zooms out, as in Chrome and Firefox.
        assert_eq!(
            typed(CMD_OR_CONTROL, "-", Code::Digit6).map(|(a, _)| a),
            Some(ZoomOut)
        );
        // Dvorak prints "y" where QWERTY has "t".
        assert_eq!(typed(CONTROL, "y", Code::KeyT), None);
        // Shifted digits stay unbound on QWERTY.
        assert_eq!(typed(CMD_OR_CONTROL_SHIFT, "!", Code::Digit1), None);
    }

    #[test]
    fn non_latin_layouts_use_key_position() {
        assert_eq!(
            typed(CMD_OR_CONTROL, "е", Code::KeyT).map(|(a, _)| a),
            Some(NewTab)
        );
    }

    #[test]
    fn named_keys_do_not_fall_back_to_position() {
        assert_eq!(
            shortcut_for(&event(
                CMD_OR_CONTROL,
                Key::Named(NamedKey::Enter),
                Code::KeyT
            )),
            None
        );
    }

    #[test]
    fn no_two_shortcuts_share_a_key() {
        let shortcuts: Vec<_> = all_shortcuts().collect();
        for (index, shortcut) in shortcuts.iter().enumerate() {
            for other in &shortcuts[index + 1..] {
                let same_key = match (shortcut.key, other.key) {
                    (ShortcutKey::Char(a), ShortcutKey::Char(b)) => a.eq_ignore_ascii_case(&b),
                    (ShortcutKey::Named(a), ShortcutKey::Named(b)) => a == b,
                    _ => false,
                };
                assert!(
                    !(same_key && shortcut.modifiers == other.modifiers),
                    "{:?} and {:?} share a key",
                    shortcut.action,
                    other.action
                );
            }
        }
    }

    #[test]
    fn labels_use_the_first_binding() {
        if cfg!(target_os = "macos") {
            assert_eq!(shortcut_label(NewTab).as_deref(), Some("⌘T"));
            assert_eq!(shortcut_label(ReopenClosedTab).as_deref(), Some("⇧⌘T"));
        } else {
            assert_eq!(shortcut_label(NewTab).as_deref(), Some("Ctrl+T"));
            assert_eq!(
                shortcut_label(ReopenClosedTab).as_deref(),
                Some("Ctrl+Shift+T")
            );
            assert_eq!(shortcut_label(Processes).as_deref(), Some("Shift+Esc"));
            assert_eq!(shortcut_label(ToggleDeveloperTools).as_deref(), Some("F12"));
        }
        assert_eq!(
            shortcut_label(ExitDocumentFullscreen).as_deref(),
            Some("Esc")
        );
    }

    #[test]
    fn text_fields_keep_caret_keys() {
        assert!(is_text_editing_key(&Key::Named(NamedKey::ArrowLeft)));
        assert!(is_text_editing_key(&Key::Named(NamedKey::Home)));
        assert!(!is_text_editing_key(&Key::Named(NamedKey::Tab)));
        assert!(!is_text_editing_key(&Key::Character("t".into())));
    }
}
