//! Keyboard shortcuts: the usual file-manager set
//! plus Finder's ⌘↑ / ⌘↓. Letters are matched by key position rather than
//! by the character typed, so ⌘A still works on a Thai or Russian layout
//! (where that key types "ฟ" or "ф").

use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{Key, Modifiers};

use internal::dto;

use crate::handler::explorer::{Jump, Request};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shortcut {
    /// Sent to the active pane.
    Pane(Request),
    /// ⌘1: make the other pane active.
    SwitchPane,
    /// ⌘X: mark the active pane's selection to be moved by the next ⌘V.
    Cut,
    /// ⌘C: remember the active pane's selection.
    Copy,
    /// ⌘⇧C: add the active pane's selection to what is remembered
    /// (a "copy to queue"), e.g. from several folders.
    CopyToQueue,
    /// ⌘V: copy (or, after ⌘X, move) what was remembered into the active
    /// pane's folder.
    Paste,
    /// ⇧⌘F: search the whole phone (the search panel takes the phone pane's place).
    Search,
    /// ⌘,: open (or close) Settings, as in every Mac app.
    Settings,
    /// ⌘/ (or ⌘?): open (or close) Help.
    Help,
    /// Space or ⌘Y: show the selection in Quick Look (or close it), as in
    /// Finder.
    QuickLook,
    /// ⌘I: Get Info on the selection, or on the folder shown.
    GetInfo,
    /// ⌘Z: undo the last rename, move or new folder, in whichever pane it
    /// was made.
    Undo,
    /// ⌘+ (or ⌘=): the active pane's grid tiles one step bigger.
    TilesBigger,
    /// ⌘−: the active pane's grid tiles one step smaller.
    TilesSmaller,
    /// ⇧⌘.: hidden files on or off in the active pane, as in Finder.
    ToggleHidden,
    /// ⌃⌘0: the active pane's rows under headers or not, as Finder's Use
    /// Groups.
    UseGroups,
    /// ⌘Q: quit, asking first while a transfer runs.
    Quit,
    /// ⇧⌘H, ⇧⌘D, ⌥⌘L: the Mac pane goes to Home, Desktop or Downloads.
    GoToMac(MacPlace),
    /// Typing with no ⌘: select the first name starting with what was
    /// typed, as in Finder. Matched by the character typed (not the key's
    /// position), so Thai or Russian names are found by typing them.
    TypeSelect(String),
}

/// A folder of the Mac's that has a key of its own, as in Finder's Go menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacPlace {
    Home,
    Desktop,
    Downloads,
}

impl MacPlace {
    /// Its path, under the home folder `home`.
    pub fn path(self, home: &str) -> String {
        let home = std::path::Path::new(home);
        let path = match self {
            MacPlace::Home => home.to_path_buf(),
            MacPlace::Desktop => home.join("Desktop"),
            MacPlace::Downloads => home.join("Downloads"),
        };
        path.to_string_lossy().into_owned()
    }
}

pub fn shortcut(key: &Key, physical: Physical, modifiers: Modifiers) -> Option<Shortcut> {
    let command = modifiers.command();
    let shift = modifiers.shift();
    // ⌃ and ⌥ belong to other things (the system, input methods); a key held
    // with either is never one of ours, but for Finder's own: ⌃⌘N (New Folder
    // with Selection), ⌃⌘0 (Use Groups), ⌥⌘C (Copy as Pathname), ⌥⌘L
    // (Downloads) and ⌥↑/⌥↓
    // (the first or last item). Only the modifiers a shortcut is documented
    // with count, so what the Help dialog lists is all there is.
    if command && !shift && modifiers.control() != modifiers.alt() {
        match (physical, modifiers.control()) {
            (Physical::Code(Code::KeyN), true) => {
                return Some(Shortcut::Pane(Request::NewFolderWithItems));
            }
            (Physical::Code(Code::Digit0), true) => return Some(Shortcut::UseGroups),
            (Physical::Code(Code::KeyC), false) => {
                return Some(Shortcut::Pane(Request::CopyPath));
            }
            (Physical::Code(Code::KeyL), false) => {
                return Some(Shortcut::GoToMac(MacPlace::Downloads));
            }
            _ => {}
        }
    }
    if modifiers.alt() && !command && !modifiers.control() {
        let to = match key {
            Key::Named(Named::ArrowUp) => Jump::First,
            Key::Named(Named::ArrowDown) => Jump::Last,
            _ => return None,
        };
        return Some(Shortcut::Pane(Request::Jump { to, extend: shift }));
    }
    if modifiers.control() || modifiers.alt() {
        return None;
    }

    if let Key::Named(named) = key {
        if (*named, command, shift) == (Named::Space, false, false) {
            return Some(Shortcut::QuickLook);
        }
        let request = match (named, command, shift) {
            (Named::ArrowUp, true, false) => Request::Up,
            (Named::ArrowDown, true, false) => Request::OpenSelected,
            (Named::ArrowUp, false, extend) => Request::Step { delta: -1, extend },
            (Named::ArrowDown, false, extend) => Request::Step { delta: 1, extend },
            (Named::ArrowLeft, false, extend) => Request::Sideways { delta: -1, extend },
            (Named::ArrowRight, false, extend) => Request::Sideways { delta: 1, extend },
            (Named::Home, false, extend) => Request::Jump {
                to: Jump::First,
                extend,
            },
            (Named::End, false, extend) => Request::Jump {
                to: Jump::Last,
                extend,
            },
            (Named::PageUp, false, extend) => Request::Jump {
                to: Jump::PageUp,
                extend,
            },
            (Named::PageDown, false, extend) => Request::Jump {
                to: Jump::PageDown,
                extend,
            },
            (Named::Enter, false, false) => Request::OpenSelected,
            // ⌫ and ⌘⌫ (as in Finder's Move to Trash), and the forward-delete key.
            (Named::Backspace, _, false) => Request::Delete,
            (Named::Delete, false, false) => Request::Delete,
            (Named::Escape, false, false) => Request::Escape,
            _ => return None,
        };
        return Some(Shortcut::Pane(request));
    }

    if !command {
        return match key {
            Key::Character(typed) if !typed.is_empty() && !typed.chars().any(char::is_control) => {
                Some(Shortcut::TypeSelect(typed.to_string()))
            }
            _ => None,
        };
    }
    let Physical::Code(code) = physical else {
        return None;
    };
    let shortcut = match (code, shift) {
        (Code::KeyC, true) => Shortcut::CopyToQueue,
        // ⇧⌘/ is ⌘? on most layouts, the Mac's usual key for Help.
        (Code::Slash, _) => Shortcut::Help,
        (Code::KeyC, false) => Shortcut::Copy,
        (Code::KeyX, false) => Shortcut::Cut,
        (Code::KeyN, false) => Shortcut::Pane(Request::NewFolder),
        (Code::KeyD, false) => Shortcut::Pane(Request::Rename),
        (Code::KeyR, false) => Shortcut::Pane(Request::Refresh),
        (Code::KeyB, false) => Shortcut::Pane(Request::Up),
        (Code::BracketLeft, false) => Shortcut::Pane(Request::Back),
        (Code::BracketRight, false) => Shortcut::Pane(Request::Forward),
        (Code::KeyA, false) => Shortcut::Pane(Request::SelectAll),
        (Code::KeyV, false) => Shortcut::Paste,
        (Code::Digit1, false) => Shortcut::SwitchPane,
        (Code::Comma, false) => Shortcut::Settings,
        (Code::KeyF, false) => Shortcut::Pane(Request::FocusFilter),
        (Code::KeyF, true) => Shortcut::Search,
        (Code::KeyG, true) => Shortcut::Pane(Request::GoToFolder),
        (Code::KeyI, false) => Shortcut::GetInfo,
        (Code::KeyY, false) => Shortcut::QuickLook,
        (Code::KeyZ, false) => Shortcut::Undo,
        // ⌘+ is ⇧⌘= on most layouts; ⌘= works too, as in Finder.
        (Code::Equal, _) => Shortcut::TilesBigger,
        (Code::Minus, false) => Shortcut::TilesSmaller,
        (Code::Period, true) => Shortcut::ToggleHidden,
        (Code::KeyQ, false) => Shortcut::Quit,
        (Code::KeyH, true) => Shortcut::GoToMac(MacPlace::Home),
        (Code::KeyD, true) => Shortcut::GoToMac(MacPlace::Desktop),
        _ => return None,
    };
    Some(shortcut)
}

/// One key press: what the keyboard reports for it. Only the tests press
/// keys; the app itself is told about presses by the window.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Press {
    key: Key,
    code: Code,
    modifiers: Modifiers,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Press {
    fn named(named: Named, modifiers: Modifiers) -> Self {
        Self {
            key: Key::Named(named),
            // Named keys are matched by name; the position is not looked at.
            code: Code::Space,
            modifiers,
        }
    }

    /// A letter or symbol, matched by its position on the keyboard.
    fn at(code: Code, modifiers: Modifiers) -> Self {
        Self {
            key: Key::Character("x".into()),
            code,
            modifiers,
        }
    }

    fn shortcut(&self) -> Option<Shortcut> {
        shortcut(&self.key, Physical::Code(self.code), self.modifiers)
    }
}

/// A line of the Shortcuts sheet, with the key presses that prove it: every
/// press must produce `action` (a test checks), so the sheet cannot say
/// something the app does not do.
#[derive(Debug, Clone)]
pub struct Row {
    pub group: &'static str,
    pub label: &'static str,
    /// As a Mac menu writes it: "⌘N", "⇧⌘C", "↑ or ⌘↓". One alternative per
    /// press, separated by " or ".
    pub keys: &'static str,
    /// The proof: the tests press these and expect `action`. The menu bar
    /// shows the first that holds ⌘ (`menu_keys`).
    presses: Vec<Press>,
    action: Shortcut,
}

impl Row {
    /// What this row does.
    pub fn action(&self) -> &Shortcut {
        &self.action
    }

    /// The key a menu shows for this row: its first press that holds ⌘.
    /// A plain key (space, ↵, ⌫) is never put in a menu: macOS hands every
    /// press of a menu's key to the menu bar first, and typing must not go
    /// through it. `None` when no press holds ⌘.
    pub fn menu_keys(&self) -> Option<dto::MenuKeys> {
        self.presses
            .iter()
            .filter(|press| press.modifiers.command())
            .find_map(Press::menu_keys)
    }
}

impl Press {
    /// This press as a menu writes its key equivalent: the key as AppKit
    /// names it, and the modifiers.
    fn menu_keys(&self) -> Option<dto::MenuKeys> {
        let key = match &self.key {
            Key::Named(named) => match named {
                // AppKit's function-key characters (NSUpArrowFunctionKey, …).
                Named::ArrowUp => "\u{F700}",
                Named::ArrowDown => "\u{F701}",
                Named::ArrowLeft => "\u{F702}",
                Named::ArrowRight => "\u{F703}",
                Named::Delete => "\u{F728}",
                Named::Backspace => "\u{8}",
                Named::Enter => "\r",
                Named::Escape => "\u{1b}",
                Named::Space => " ",
                _ => return None,
            }
            .to_owned(),
            Key::Character(_) => match self.code {
                Code::Comma => ",".to_owned(),
                Code::BracketLeft => "[".to_owned(),
                Code::BracketRight => "]".to_owned(),
                Code::Slash => "/".to_owned(),
                Code::Period => ".".to_owned(),
                Code::Equal => "=".to_owned(),
                Code::Minus => "-".to_owned(),
                code => {
                    let name = format!("{code:?}");
                    let key = name
                        .strip_prefix("Key")
                        .or_else(|| name.strip_prefix("Digit"))?;
                    if key.len() != 1 {
                        return None;
                    }
                    key.to_lowercase()
                }
            },
            _ => return None,
        };
        Some(dto::MenuKeys {
            key,
            command: self.modifiers.command(),
            shift: self.modifiers.shift(),
            option: self.modifiers.alt(),
            control: self.modifiers.control(),
        })
    }
}

fn row(
    group: &'static str,
    label: &'static str,
    keys: &'static str,
    action: Shortcut,
    presses: Vec<Press>,
) -> Row {
    Row {
        group,
        label,
        keys,
        presses,
        action,
    }
}

const NONE: Modifiers = Modifiers::empty();
const CMD: Modifiers = Modifiers::COMMAND;
const SHIFT: Modifiers = Modifiers::SHIFT;
const ALT: Modifiers = Modifiers::ALT;

/// Every keyboard shortcut, for the Help dialog. The shortcuts themselves are
/// `shortcut()` above; this lists them with their keys, and its tests keep the
/// two in step in both directions: nothing listed that doesn't work, and
/// nothing that works that isn't listed.
pub fn catalog() -> Vec<Row> {
    use Named::{
        ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Backspace, Delete, End, Enter, Escape, Home,
        PageDown, PageUp, Space,
    };
    let pane = |request| Shortcut::Pane(request);
    let step = |delta, extend| pane(Request::Step { delta, extend });
    let jump = |to, extend| pane(Request::Jump { to, extend });
    let sideways = |delta, extend| pane(Request::Sideways { delta, extend });

    vec![
        row(
            "Moving around",
            "Select the item above",
            "↑",
            step(-1, false),
            vec![Press::named(ArrowUp, NONE)],
        ),
        row(
            "Moving around",
            "Select the item below",
            "↓",
            step(1, false),
            vec![Press::named(ArrowDown, NONE)],
        ),
        row(
            "Moving around",
            "Select the previous tile in the grid",
            "←",
            sideways(-1, false),
            vec![Press::named(ArrowLeft, NONE)],
        ),
        row(
            "Moving around",
            "Select the next tile in the grid",
            "→",
            sideways(1, false),
            vec![Press::named(ArrowRight, NONE)],
        ),
        row(
            "Moving around",
            "Add the item above to the selection",
            "⇧↑",
            step(-1, true),
            vec![Press::named(ArrowUp, SHIFT)],
        ),
        row(
            "Moving around",
            "Add the item below to the selection",
            "⇧↓",
            step(1, true),
            vec![Press::named(ArrowDown, SHIFT)],
        ),
        row(
            "Moving around",
            "Add the previous tile to the selection",
            "⇧←",
            sideways(-1, true),
            vec![Press::named(ArrowLeft, SHIFT)],
        ),
        row(
            "Moving around",
            "Add the next tile to the selection",
            "⇧→",
            sideways(1, true),
            vec![Press::named(ArrowRight, SHIFT)],
        ),
        row(
            "Moving around",
            "Select the first item",
            "⌥↑ or home",
            jump(Jump::First, false),
            vec![Press::named(ArrowUp, ALT), Press::named(Home, NONE)],
        ),
        row(
            "Moving around",
            "Select the last item",
            "⌥↓ or end",
            jump(Jump::Last, false),
            vec![Press::named(ArrowDown, ALT), Press::named(End, NONE)],
        ),
        row(
            "Moving around",
            "Add everything up to the first item to the selection",
            "⇧⌥↑ or ⇧home",
            jump(Jump::First, true),
            vec![
                Press::named(ArrowUp, ALT | SHIFT),
                Press::named(Home, SHIFT),
            ],
        ),
        row(
            "Moving around",
            "Add everything down to the last item to the selection",
            "⇧⌥↓ or ⇧end",
            jump(Jump::Last, true),
            vec![
                Press::named(ArrowDown, ALT | SHIFT),
                Press::named(End, SHIFT),
            ],
        ),
        row(
            "Moving around",
            "Select the item a screen up",
            "page up",
            jump(Jump::PageUp, false),
            vec![Press::named(PageUp, NONE)],
        ),
        row(
            "Moving around",
            "Select the item a screen down",
            "page down",
            jump(Jump::PageDown, false),
            vec![Press::named(PageDown, NONE)],
        ),
        row(
            "Moving around",
            "Add a screen up to the selection",
            "⇧page up",
            jump(Jump::PageUp, true),
            vec![Press::named(PageUp, SHIFT)],
        ),
        row(
            "Moving around",
            "Add a screen down to the selection",
            "⇧page down",
            jump(Jump::PageDown, true),
            vec![Press::named(PageDown, SHIFT)],
        ),
        row(
            "Moving around",
            "Open the selected folder, or the file in its Mac app",
            "↵ or ⌘↓",
            pane(Request::OpenSelected),
            vec![Press::named(Enter, NONE), Press::named(ArrowDown, CMD)],
        ),
        row(
            "Moving around",
            "Go up one folder",
            "⌘↑ or ⌘B",
            pane(Request::Up),
            vec![Press::named(ArrowUp, CMD), Press::at(Code::KeyB, CMD)],
        ),
        row(
            "Moving around",
            "Back to the folder shown before",
            "⌘[",
            pane(Request::Back),
            vec![Press::at(Code::BracketLeft, CMD)],
        ),
        row(
            "Moving around",
            "Forward again",
            "⌘]",
            pane(Request::Forward),
            vec![Press::at(Code::BracketRight, CMD)],
        ),
        row(
            "Moving around",
            "Go to a folder by typing its path",
            "⇧⌘G",
            pane(Request::GoToFolder),
            vec![Press::at(Code::KeyG, CMD | SHIFT)],
        ),
        row(
            "Moving around",
            "Go to Home on the Mac",
            "⇧⌘H",
            Shortcut::GoToMac(MacPlace::Home),
            vec![Press::at(Code::KeyH, CMD | SHIFT)],
        ),
        row(
            "Moving around",
            "Go to the Desktop on the Mac",
            "⇧⌘D",
            Shortcut::GoToMac(MacPlace::Desktop),
            vec![Press::at(Code::KeyD, CMD | SHIFT)],
        ),
        row(
            "Moving around",
            "Go to Downloads on the Mac",
            "⌥⌘L",
            Shortcut::GoToMac(MacPlace::Downloads),
            vec![Press::at(Code::KeyL, CMD | ALT)],
        ),
        row(
            "Moving around",
            "Show or hide hidden files",
            "⇧⌘.",
            Shortcut::ToggleHidden,
            vec![Press::at(Code::Period, CMD | SHIFT)],
        ),
        row(
            "Moving around",
            "Group the files under headers, or not",
            "⌃⌘0",
            Shortcut::UseGroups,
            vec![Press::at(Code::Digit0, CMD | Modifiers::CTRL)],
        ),
        row(
            "Moving around",
            "Switch between the Mac and the phone",
            "⌘1",
            Shortcut::SwitchPane,
            vec![Press::at(Code::Digit1, CMD)],
        ),
        row(
            "Moving around",
            "Reload the folder from the phone",
            "⌘R",
            pane(Request::Refresh),
            vec![Press::at(Code::KeyR, CMD)],
        ),
        row(
            "Selecting",
            "Filter the folder by name (esc clears it; twice from the field)",
            "⌘F",
            pane(Request::FocusFilter),
            vec![Press::at(Code::KeyF, CMD)],
        ),
        row(
            "Selecting",
            "Type the start of a name to select it",
            "letters",
            // Any typed text; the tests check the kind, not the letter.
            Shortcut::TypeSelect(String::new()),
            vec![Press::at(Code::KeyA, NONE)],
        ),
        row(
            "Selecting",
            "Select everything in the folder",
            "⌘A",
            pane(Request::SelectAll),
            vec![Press::at(Code::KeyA, CMD)],
        ),
        row(
            "Selecting",
            "Quick Look the selection (phone files are fetched first)",
            "space or ⌘Y",
            Shortcut::QuickLook,
            vec![Press::named(Space, NONE), Press::at(Code::KeyY, CMD)],
        ),
        row(
            "Selecting",
            "Get Info: what the selection holds, folders counted (or the folder shown)",
            "⌘I",
            Shortcut::GetInfo,
            vec![Press::at(Code::KeyI, CMD)],
        ),
        row(
            "Selecting",
            "Clear the selection or close a dialog",
            "esc",
            pane(Request::Escape),
            vec![Press::named(Escape, NONE)],
        ),
        row(
            "Changing files",
            "New folder",
            "⌘N",
            pane(Request::NewFolder),
            vec![Press::at(Code::KeyN, CMD)],
        ),
        row(
            "Changing files",
            "Put the selection in a new folder",
            "⌃⌘N",
            pane(Request::NewFolderWithItems),
            vec![Press::at(Code::KeyN, CMD | Modifiers::CTRL)],
        ),
        row(
            "Changing files",
            "Undo the last rename, move or new folder",
            "⌘Z",
            Shortcut::Undo,
            vec![Press::at(Code::KeyZ, CMD)],
        ),
        row(
            "Changing files",
            "Rename the selection (several at once by a pattern)",
            "⌘D",
            pane(Request::Rename),
            vec![Press::at(Code::KeyD, CMD)],
        ),
        row(
            "Changing files",
            "Delete the selection",
            "⌫ or ⌘⌫ or ⌦",
            pane(Request::Delete),
            vec![
                Press::named(Backspace, NONE),
                Press::named(Backspace, CMD),
                Press::named(Delete, NONE),
            ],
        ),
        row(
            "Copying",
            "Cut the selection, to move it with Paste",
            "⌘X",
            Shortcut::Cut,
            vec![Press::at(Code::KeyX, CMD)],
        ),
        row(
            "Copying",
            "Copy the selection",
            "⌘C",
            Shortcut::Copy,
            vec![Press::at(Code::KeyC, CMD)],
        ),
        row(
            "Copying",
            "Copy the selection's paths as text",
            "⌥⌘C",
            pane(Request::CopyPath),
            vec![Press::at(Code::KeyC, CMD | Modifiers::ALT)],
        ),
        row(
            "Copying",
            "Add the selection to what is copied",
            "⇧⌘C",
            Shortcut::CopyToQueue,
            vec![Press::at(Code::KeyC, CMD | SHIFT)],
        ),
        row(
            "Copying",
            "Paste into this folder (moves what ⌘X cut)",
            "⌘V",
            Shortcut::Paste,
            vec![Press::at(Code::KeyV, CMD)],
        ),
        row(
            "The phone",
            "Search the whole phone by name or kind",
            "⇧⌘F",
            Shortcut::Search,
            vec![Press::at(Code::KeyF, CMD | SHIFT)],
        ),
        row(
            "Moving around",
            "Bigger tiles in Grid view",
            "⌘= or ⇧⌘= (⌘+)",
            Shortcut::TilesBigger,
            vec![
                Press::at(Code::Equal, CMD),
                Press::at(Code::Equal, CMD | SHIFT),
            ],
        ),
        row(
            "Moving around",
            "Smaller tiles in Grid view",
            "⌘−",
            Shortcut::TilesSmaller,
            vec![Press::at(Code::Minus, CMD)],
        ),
        row(
            "The app",
            "Quit (asks first while a transfer runs)",
            "⌘Q",
            Shortcut::Quit,
            vec![Press::at(Code::KeyQ, CMD)],
        ),
        row(
            "The app",
            "Settings",
            "⌘,",
            Shortcut::Settings,
            vec![Press::at(Code::Comma, CMD)],
        ),
        row(
            "The app",
            "Help and keyboard shortcuts",
            "⌘/ or ⌘?",
            Shortcut::Help,
            vec![
                Press::at(Code::Slash, CMD),
                Press::at(Code::Slash, CMD | SHIFT),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(c: &str, code: Code, modifiers: Modifiers) -> Option<Shortcut> {
        shortcut(&Key::Character(c.into()), Physical::Code(code), modifiers)
    }

    fn named(named: Named, modifiers: Modifiers) -> Option<Shortcut> {
        shortcut(&Key::Named(named), Physical::Code(Code::Space), modifiers)
    }

    const CMD: Modifiers = Modifiers::COMMAND;

    #[test]
    fn command_letters_map_to_their_actions() {
        assert_eq!(
            letter("n", Code::KeyN, CMD),
            Some(Shortcut::Pane(Request::NewFolder))
        );
        assert_eq!(
            letter("d", Code::KeyD, CMD),
            Some(Shortcut::Pane(Request::Rename))
        );
        assert_eq!(
            letter("r", Code::KeyR, CMD),
            Some(Shortcut::Pane(Request::Refresh))
        );
        assert_eq!(
            letter("b", Code::KeyB, CMD),
            Some(Shortcut::Pane(Request::Up))
        );
        assert_eq!(
            letter("a", Code::KeyA, CMD),
            Some(Shortcut::Pane(Request::SelectAll))
        );
        assert_eq!(letter("c", Code::KeyC, CMD), Some(Shortcut::Copy));
        assert_eq!(letter("v", Code::KeyV, CMD), Some(Shortcut::Paste));
        assert_eq!(letter("x", Code::KeyX, CMD), Some(Shortcut::Cut));
        assert_eq!(
            letter("x", Code::KeyX, CMD | SHIFT),
            None,
            "⇧⌘X is not a shortcut"
        );
        assert_eq!(
            letter("x", Code::KeyX, NONE),
            Some(Shortcut::TypeSelect("x".into())),
            "x alone is typing"
        );
        assert_eq!(letter("1", Code::Digit1, CMD), Some(Shortcut::SwitchPane));
        assert_eq!(letter(",", Code::Comma, CMD), Some(Shortcut::Settings));
    }

    #[test]
    fn letters_are_matched_by_position_not_by_the_typed_character() {
        // ⌘A on a Thai layout types "ฟ".
        assert_eq!(
            letter("ฟ", Code::KeyA, CMD),
            Some(Shortcut::Pane(Request::SelectAll))
        );
    }

    #[test]
    fn letters_without_command_type_to_select_by_the_character_typed() {
        assert_eq!(
            letter("n", Code::KeyN, Modifiers::empty()),
            Some(Shortcut::TypeSelect("n".into()))
        );
        assert_eq!(
            letter("ฟ", Code::KeyA, Modifiers::empty()),
            Some(Shortcut::TypeSelect("ฟ".into())),
            "a Thai layout types Thai"
        );
        assert_eq!(
            letter("N", Code::KeyN, Modifiers::SHIFT),
            Some(Shortcut::TypeSelect("N".into()))
        );
        assert_eq!(
            letter("\u{7}", Code::KeyG, Modifiers::empty()),
            None,
            "no control characters"
        );
        assert_eq!(letter("", Code::KeyG, Modifiers::empty()), None);
        assert_eq!(letter("z", Code::KeyZ, CMD), Some(Shortcut::Undo));
    }

    /// Whether `press` does `action`; any typing is the one typing line.
    fn does(press: &Press, action: &Shortcut) -> bool {
        match (press.shortcut(), action) {
            (Some(Shortcut::TypeSelect(_)), Shortcut::TypeSelect(_)) => true,
            (done, action) => done.as_ref() == Some(action),
        }
    }

    #[test]
    fn shift_command_f_searches_the_phone_and_command_f_filters() {
        assert_eq!(
            letter("f", Code::KeyF, CMD | Modifiers::SHIFT),
            Some(Shortcut::Search)
        );
        // ⇧⌘F on a Thai layout types "ด".
        assert_eq!(
            letter("ด", Code::KeyF, CMD | Modifiers::SHIFT),
            Some(Shortcut::Search)
        );
        // ⌘F alone filters the folder, in either pane.
        assert_eq!(
            letter("f", Code::KeyF, CMD),
            Some(Shortcut::Pane(Request::FocusFilter))
        );
    }

    #[test]
    fn space_alone_is_quick_look() {
        assert_eq!(
            named(Named::Space, Modifiers::empty()),
            Some(Shortcut::QuickLook)
        );
        assert_eq!(named(Named::Space, CMD), None);
        assert_eq!(named(Named::Space, Modifiers::SHIFT), None);
    }

    #[test]
    fn copy_to_queue_is_shift_command_c() {
        assert_eq!(
            letter("c", Code::KeyC, CMD | Modifiers::SHIFT),
            Some(Shortcut::CopyToQueue)
        );
    }

    #[test]
    fn an_unknown_physical_key_does_nothing() {
        let unknown = Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified);

        assert_eq!(shortcut(&Key::Character("n".into()), unknown, CMD), None);
    }

    #[test]
    fn arrows_move_and_shift_arrows_extend() {
        assert_eq!(
            named(Named::ArrowDown, Modifiers::empty()),
            Some(Shortcut::Pane(Request::Step {
                delta: 1,
                extend: false
            }))
        );
        assert_eq!(
            named(Named::ArrowUp, Modifiers::SHIFT),
            Some(Shortcut::Pane(Request::Step {
                delta: -1,
                extend: true
            }))
        );
    }

    #[test]
    fn finder_command_arrows_go_up_and_open() {
        assert_eq!(
            named(Named::ArrowUp, CMD),
            Some(Shortcut::Pane(Request::Up))
        );
        assert_eq!(
            named(Named::ArrowDown, CMD),
            Some(Shortcut::Pane(Request::OpenSelected))
        );
    }

    #[test]
    fn return_backspace_delete_and_escape() {
        let none = Modifiers::empty();
        assert_eq!(
            named(Named::Enter, none),
            Some(Shortcut::Pane(Request::OpenSelected))
        );
        assert_eq!(
            named(Named::Backspace, none),
            Some(Shortcut::Pane(Request::Delete))
        );
        assert_eq!(
            named(Named::Backspace, CMD),
            Some(Shortcut::Pane(Request::Delete))
        );
        assert_eq!(
            named(Named::Delete, none),
            Some(Shortcut::Pane(Request::Delete))
        );
        assert_eq!(
            named(Named::Escape, none),
            Some(Shortcut::Pane(Request::Escape))
        );
        assert_eq!(named(Named::Tab, none), None);
    }

    #[test]
    fn left_and_right_move_between_tiles() {
        assert_eq!(
            named(Named::ArrowLeft, Modifiers::empty()),
            Some(Shortcut::Pane(Request::Sideways {
                delta: -1,
                extend: false
            }))
        );
        assert_eq!(
            named(Named::ArrowRight, Modifiers::SHIFT),
            Some(Shortcut::Pane(Request::Sideways {
                delta: 1,
                extend: true
            }))
        );
    }

    #[test]
    fn every_listed_key_does_what_its_line_says() {
        for row in catalog() {
            for press in &row.presses {
                assert!(
                    does(press, &row.action),
                    "{:?} ({}) does not do {:?}",
                    row.label,
                    row.keys,
                    row.action
                );
            }
        }
    }

    #[test]
    fn the_keys_shown_match_the_presses_that_were_tried() {
        for row in catalog() {
            let shown: Vec<&str> = row.keys.split(" or ").collect();
            assert_eq!(
                shown.len(),
                row.presses.len(),
                "{:?}: {}",
                row.label,
                row.keys
            );
            for (text, press) in shown.iter().zip(&row.presses) {
                assert_eq!(
                    text.contains('⌘'),
                    press.modifiers.command(),
                    "{:?}: {text:?} and its press disagree about ⌘",
                    row.label
                );
                // A `?` is a shifted `/`, so it already says ⇧.
                assert_eq!(
                    text.contains('⇧') || text.contains('?'),
                    press.modifiers.shift(),
                    "{:?}: {text:?} and its press disagree about ⇧",
                    row.label
                );
                assert!(!text.is_empty());
            }
        }
    }

    /// Every key position and named key worth trying.
    fn every_key() -> Vec<Press> {
        use Named::*;
        let codes = [
            Code::KeyA,
            Code::KeyB,
            Code::KeyC,
            Code::KeyD,
            Code::KeyE,
            Code::KeyF,
            Code::KeyG,
            Code::KeyH,
            Code::KeyI,
            Code::KeyJ,
            Code::KeyK,
            Code::KeyL,
            Code::KeyM,
            Code::KeyN,
            Code::KeyO,
            Code::KeyP,
            Code::KeyQ,
            Code::KeyR,
            Code::KeyS,
            Code::KeyT,
            Code::KeyU,
            Code::KeyV,
            Code::KeyW,
            Code::KeyX,
            Code::KeyY,
            Code::KeyZ,
            Code::Digit0,
            Code::Digit1,
            Code::Digit2,
            Code::Digit3,
            Code::Digit4,
            Code::Digit5,
            Code::Digit6,
            Code::Digit7,
            Code::Digit8,
            Code::Digit9,
            Code::Comma,
            Code::Period,
            Code::Slash,
            Code::Semicolon,
            Code::Quote,
            Code::Minus,
            Code::Equal,
            Code::Backslash,
            Code::BracketLeft,
            Code::BracketRight,
            Code::Backquote,
            Code::Space,
        ];
        let named = [
            ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Enter, Backspace, Delete, Escape, Tab,
            Space, Home, End, PageUp, PageDown, F1, F5,
        ];
        let modifiers = [
            NONE,
            SHIFT,
            CMD,
            CMD | SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::ALT | SHIFT,
            CMD | Modifiers::ALT,
            CMD | Modifiers::CTRL,
            CMD | Modifiers::CTRL | SHIFT,
        ];
        let mut all = Vec::new();
        for m in modifiers {
            all.extend(codes.iter().map(|code| Press::at(*code, m)));
            all.extend(named.iter().map(|n| Press::named(*n, m)));
        }
        all
    }

    /// Whether two presses are the same key with the same modifiers: named
    /// keys by name, everything else by position on the keyboard.
    fn same_press(a: &Press, b: &Press) -> bool {
        let same_key = match (&a.key, &b.key) {
            (Key::Named(x), Key::Named(y)) => x == y,
            (Key::Character(_), Key::Character(_)) => a.code == b.code,
            _ => false,
        };
        same_key && a.modifiers == b.modifiers
    }

    #[test]
    fn every_key_press_that_works_is_listed_exactly() {
        let rows = catalog();
        let listed: Vec<&Press> = rows.iter().flat_map(|row| &row.presses).collect();

        for press in every_key() {
            let Some(action) = press.shortcut() else {
                continue;
            };
            if let Shortcut::TypeSelect(_) = action {
                assert!(
                    rows.iter().any(|row| does(&press, &row.action)),
                    "typing is listed"
                );
                continue;
            }
            assert!(
                listed.iter().any(|listed| same_press(listed, &press)),
                "{:?} (position {:?}) with {:?} does {action:?}, which the Shortcuts sheet does not list",
                press.key,
                press.code,
                press.modifiers
            );
        }
    }

    #[test]
    fn keys_held_with_control_or_option_are_never_ours_but_finders_own() {
        for press in every_key() {
            let character = matches!(press.key, Key::Character(_));
            let at =
                |code, modifiers| character && press.code == code && press.modifiers == modifiers;
            let new_folder = at(Code::KeyN, CMD | Modifiers::CTRL);
            let groups = at(Code::Digit0, CMD | Modifiers::CTRL);
            let copy_path = at(Code::KeyC, CMD | Modifiers::ALT);
            let downloads = at(Code::KeyL, CMD | Modifiers::ALT);
            let option_arrow = matches!(press.key, Key::Named(Named::ArrowUp | Named::ArrowDown))
                && (press.modifiers == Modifiers::ALT || press.modifiers == Modifiers::ALT | SHIFT);
            if new_folder {
                assert_eq!(
                    press.shortcut(),
                    Some(Shortcut::Pane(Request::NewFolderWithItems))
                );
            } else if groups {
                assert_eq!(press.shortcut(), Some(Shortcut::UseGroups));
            } else if copy_path {
                assert_eq!(press.shortcut(), Some(Shortcut::Pane(Request::CopyPath)));
            } else if downloads {
                assert_eq!(
                    press.shortcut(),
                    Some(Shortcut::GoToMac(MacPlace::Downloads))
                );
            } else if option_arrow {
                let to = if press.key == Key::Named(Named::ArrowUp) {
                    Jump::First
                } else {
                    Jump::Last
                };
                assert_eq!(
                    press.shortcut(),
                    Some(Shortcut::Pane(Request::Jump {
                        to,
                        extend: press.modifiers.shift()
                    }))
                );
            } else if press.modifiers.control() || press.modifiers.alt() {
                assert_eq!(
                    press.shortcut(),
                    None,
                    "{:?} {:?}",
                    press.key,
                    press.modifiers
                );
            }
        }
    }

    #[test]
    fn extra_shift_does_not_turn_one_shortcut_into_another() {
        // ⇧⌘N, ⇧⌘A … are not ⌘N, ⌘A (⇧⌘D is the Desktop, as in Finder).
        for code in [Code::KeyN, Code::KeyA, Code::KeyR, Code::KeyB, Code::KeyV] {
            assert_eq!(letter("x", code, CMD | Modifiers::SHIFT), None, "{code:?}");
        }
        assert_eq!(
            named(Named::ArrowUp, CMD | Modifiers::SHIFT),
            None,
            "⇧⌘↑ is not ⌘↑"
        );
        assert_eq!(named(Named::Enter, Modifiers::SHIFT), None);
        assert_eq!(named(Named::Escape, Modifiers::SHIFT), None);
    }

    #[test]
    fn the_macs_own_places_are_under_its_home_folder() {
        assert_eq!(MacPlace::Home.path("/Users/me"), "/Users/me");
        assert_eq!(MacPlace::Desktop.path("/Users/me"), "/Users/me/Desktop");
        assert_eq!(MacPlace::Downloads.path("/Users/me"), "/Users/me/Downloads");
    }

    #[test]
    fn nothing_is_listed_twice() {
        let rows = catalog();
        let mut labels: Vec<&str> = rows.iter().map(|row| row.label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), rows.len(), "two lines have the same label");
        assert!(
            rows.iter()
                .all(|row| !row.group.is_empty() && !row.label.is_empty())
        );
    }

    #[test]
    fn help_opens_on_command_slash_and_command_question_mark() {
        assert_eq!(letter("/", Code::Slash, CMD), Some(Shortcut::Help));
        assert_eq!(
            letter("?", Code::Slash, CMD | Modifiers::SHIFT),
            Some(Shortcut::Help)
        );
        assert_eq!(
            letter("/", Code::Slash, Modifiers::empty()),
            Some(Shortcut::TypeSelect("/".into())),
            "/ alone is typing"
        );
    }
}
