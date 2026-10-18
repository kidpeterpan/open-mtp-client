use internal::dto::SortKey;

use super::menu::Action as MenuAction;

/// What the user asked the file browser to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Search the whole volume (the app opens the search panel).
    Search,
    /// Mirror this folder and the Mac pane's (`app.rs` opens the dialog).
    Mirror,
    /// A click on an entry: selects it, or with ⌘ toggles it and with ⇧
    /// extends the selection to it.
    Select(String),
    /// The button went down on a row away from its name: the start of a
    /// lasso. The list then says where (`Message::PressedAt`).
    PressRow(String),
    SelectAll,
    /// A click on a group's header: selects that group (⇧ adds, ⌘ toggles).
    SelectGroup(usize),
    /// Show files as a list or a grid of thumbnails.
    SetView(internal::dto::ViewMode),
    /// ←/→: move `delta` tiles in the grid.
    Sideways {
        delta: isize,
        extend: bool,
    },
    /// ↑/↓: move the cursor `delta` rows (a whole row of tiles in the grid);
    /// `extend` with ⇧.
    Step {
        delta: isize,
        extend: bool,
    },
    /// Home/End, ⌥↑/⌥↓, Page Up/Down: the first or last row, or a screen up
    /// or down; `extend` with ⇧.
    Jump {
        to: Jump,
        extend: bool,
    },
    /// Return: open the single selected entry.
    OpenSelected,
    /// ⌘F: put the cursor in the folder's filter.
    FocusFilter,
    /// The filter's text: only names holding every word are shown.
    Filter(String),
    /// The filter's kind: only files of it are shown (`None`: every kind).
    FilterKind(Option<internal::dto::FileKind>),
    /// Only files changed on the days this keeps; `None` for any date.
    FilterDate(Option<internal::dto::DateRange>),
    /// Return in the filter: open what it leaves, or pick the first of it.
    SubmitFilter,
    /// Escape: close the open dialog, or clear the selection.
    Escape,
    /// Open an entry by name (double click); folders are entered.
    Open(String),
    /// Jump to an absolute path (breadcrumb click).
    GoTo(String),
    Up,
    /// A column's left edge was pressed in the header of a list this wide:
    /// dragging it makes the column wider or narrower until the button comes
    /// up.
    ResizeColumn(super::layout::Edge, u16),
    /// A header edge was double-clicked: the usual widths again.
    ResetColumns,
    /// Open System Settings where the app is let into a refused folder.
    OpenPrivacySettings(internal::dto::PrivacyPane),
    /// The folder shown before this one (⌘[), and back again (⌘]).
    Back,
    Forward,
    /// Typed with no ⌘, `at` that moment: select the first name starting
    /// with what was typed in the last moment (Finder's type-select).
    TypeAhead {
        text: String,
        at: std::time::Instant,
    },
    Refresh,
    SetShowHidden(bool),
    /// Column header click: sort by this key, or flip the order if it already is.
    SortBy(SortKey),
    SetFoldersFirst(bool),
    NewFolder,
    /// Make a folder and move the selection into it (⌃⌘N, Finder's New
    /// Folder with Selection).
    NewFolderWithItems,
    /// Go to a folder by typing its path (⇧⌘G).
    GoToFolder,
    /// Go to a folder: one of the names offered under the path was clicked.
    ChooseFolder(String),
    /// Put the selected items' full paths on the clipboard, one per line
    /// (⌥⌘C, Finder's Copy as Pathname).
    CopyPath,
    /// Rename the selected entry, or the selected entries by one pattern.
    Rename,
    /// Delete the selected entry (straight to the Trash on the Mac).
    Delete,
    /// The name typed in the open dialog.
    EditName(String),
    /// Renaming several: how the new names are made.
    RenameMode(ManyMode),
    /// Renaming several: what was typed in one of its fields.
    RenameField(ManyField, String),
    /// Renaming several: added text goes after the name (else before).
    RenameAtEnd(bool),
    /// Confirm the open dialog.
    Submit,
    /// Close the open dialog without changing anything.
    Cancel,
    /// Copy the selection to the other pane; `app.rs` carries it out.
    Transfer,
    /// An entry was right-clicked: it becomes the selection, unless it is
    /// already part of it, and the menu that opens next is about it.
    MenuOn(String),
    /// A command picked from the right-click menu.
    Menu(MenuAction),
    /// Escape, or a click outside the menu.
    CloseMenu,
    /// Close the banner a paste left above the list.
    DismissNotice,
    /// With a button held on a row (a possible drag): the pointer came over `name`.
    PointerEntered(String),
    /// The pointer left the row `name`; leaving the one that was pressed
    /// starts a drag.
    PointerLeft(String),
    /// The mouse button came up, anywhere in the window: a press ends, and a
    /// drag is dropped on the folder it is over.
    PointerReleased,
}

/// How several items get new names at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManyMode {
    Replace,
    Add,
    Number,
}

/// A field of the dialog that renames several items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManyField {
    Find,
    With,
    Text,
    Base,
    Start,
}

/// Where a long jump through the list goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    First,
    Last,
    /// A screen up.
    PageUp,
    /// A screen down.
    PageDown,
}
