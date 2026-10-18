//! The Mac menu bar, as plain data: what each menu holds, and which of the
//! app's own commands are greyed out or ticked right now. The app's commands
//! are known by a number (`tag`); what a tag means is the app's business.

/// One menu of the menu bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub role: MenuRole,
    pub entries: Vec<MenuEntry>,
}

/// Menus macOS treats specially.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuRole {
    /// The first menu, under the app's name.
    App,
    /// macOS adds the list of windows to it.
    Window,
    /// macOS adds a search field that finds any command in every menu.
    Help,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// One of the app's commands; a click is told by its tag.
    Command {
        title: String,
        keys: Option<MenuKeys>,
        tag: u32,
    },
    /// Something macOS does itself (hide, quit, minimize, …).
    System {
        title: String,
        keys: Option<MenuKeys>,
        action: SystemAction,
    },
    /// The Services submenu macOS fills.
    Services,
    /// A submenu of the app's own, which may be filled again later
    /// (`MenuBar::fill`, by its title).
    Submenu {
        title: String,
        entries: Vec<MenuEntry>,
    },
    Separator,
}

/// What macOS does for a `System` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Minimize,
    Zoom,
    FullScreen,
    BringAllToFront,
}

/// A key equivalent as a menu shows it: the key as AppKit names it (a
/// letter, a symbol, or one of AppKit's function-key characters) and the
/// modifiers held with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuKeys {
    pub key: String,
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

/// Which commands can't be used right now (greyed out), and which are
/// ticked (a choice that is on).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuState {
    pub disabled: Vec<u32>,
    pub checked: Vec<u32>,
}
