//! The right-click menu: which commands it offers for what was clicked, and
//! where it goes so it stays inside the pane. It offers Rename, Copy, Copy to
//! Queue, Paste, New Folder, "Open in Finder", Open, the transfer, Delete,
//! Select All and Refresh, and shows each command's shortcut like a Mac menu
//! does.

use iced::{Point, Rectangle, Size};

pub const WIDTH: f32 = 240.0;
pub const ITEM_HEIGHT: f32 = 26.0;
pub const SEPARATOR_HEIGHT: f32 = 9.0;
pub const PADDING: f32 = 5.0;

/// A command in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    /// Download or upload the selection to the other pane.
    Transfer,
    /// Marks the selection to be moved by the next paste (the phone only).
    Cut,
    Copy,
    CopyToQueue,
    Paste,
    NewFolder,
    NewFolderWithItems,
    CopyPath,
    Rename,
    Delete,
    SelectAll,
    Refresh,
    ShowInFinder,
    /// Pin the selected folder to the sidebar.
    Pin,
    /// Pin the folder the pane shows to the sidebar.
    PinHere,
    /// Get Info on the selection.
    GetInfo,
    /// Get Info on the folder the pane shows.
    GetInfoHere,
    /// The Mac's share menu for the selection (phone files fetched first).
    Share,
    /// Select what the other pane's folder doesn't have as it is here.
    SelectNotInOther,
    /// Select what the other pane's folder has as it is here.
    SelectAlsoInOther,
    /// Compare the selection with what has its names in the other pane's
    /// folder, byte for byte.
    CheckAgainstOther,
    /// Fetch the phone file to edit in its Mac app, and send it back later.
    EditOnMac,
}

impl Action {
    /// Carried out by `app.rs`, which knows both panes and the clipboard.
    pub fn is_app_wide(self) -> bool {
        matches!(
            self,
            Action::Transfer
                | Action::Cut
                | Action::Copy
                | Action::CopyToQueue
                | Action::Paste
                | Action::Pin
                | Action::PinHere
                | Action::GetInfo
                | Action::GetInfoHere
                | Action::Share
                | Action::SelectNotInOther
                | Action::SelectAlsoInOther
                | Action::CheckAgainstOther
                | Action::EditOnMac
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// As a Mac menu shows it, e.g. "⇧⌘C"; empty when there is none.
    pub shortcut: &'static str,
    pub action: Action,
    pub enabled: bool,
}

/// What the menu needs to know about the pane and the app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// Right-clicked on an entry (which is then selected), not on empty space.
    pub on_entry: bool,
    pub selected: usize,
    /// The selection is one folder (which can be pinned).
    pub single_folder: bool,
    /// Label of the copy to the other side ("Download"), if there is one.
    pub transfer: Option<String>,
    /// The clipboard holds items from the other side.
    pub can_paste: bool,
    /// The pane is on the Mac, so items can be shown in Finder.
    pub on_mac: bool,
    /// Deleting is permanent (the phone), so it asks first.
    pub deletes_permanently: bool,
    /// The other pane is shown, so its folder can be compared with this one.
    pub other_pane: bool,
    /// On the phone: "Edit on Mac" is offered, enabled when the selection is
    /// one file. `None` on the Mac, where an app opens a file as it is.
    pub edit_on_mac: Option<bool>,
}

fn item(label: impl Into<String>, shortcut: &'static str, action: Action, enabled: bool) -> Item {
    Item {
        label: label.into(),
        shortcut,
        action,
        enabled,
    }
}

/// The menu's sections, separated by a line.
pub fn sections(context: &Context) -> Vec<Vec<Item>> {
    let paste = item("Paste", "⌘V", Action::Paste, context.can_paste);
    if !context.on_entry {
        return vec![
            vec![item("New Folder", "⌘N", Action::NewFolder, true), paste],
            vec![
                item("Select All", "⌘A", Action::SelectAll, true),
                item(
                    "Select Not in Other Pane",
                    "",
                    Action::SelectNotInOther,
                    context.other_pane,
                ),
                item(
                    "Select Also in Other Pane",
                    "",
                    Action::SelectAlsoInOther,
                    context.other_pane,
                ),
                item("Refresh", "⌘R", Action::Refresh, true),
            ],
            vec![
                item("Get Info", "⌘I", Action::GetInfoHere, true),
                item("Add This Folder to Sidebar", "", Action::PinHere, true),
            ],
        ];
    }

    // A folder opens in the pane, a file in its Mac app.
    let mut first = vec![item("Open", "↵", Action::Open, context.selected == 1)];
    if let Some(enabled) = context.edit_on_mac {
        first.push(item("Edit on Mac", "", Action::EditOnMac, enabled));
    }
    if let Some(label) = &context.transfer {
        let what = match context.selected {
            1 => String::new(),
            n => format!(" {n} Items"),
        };
        first.push(item(format!("{label}{what}"), "", Action::Transfer, true));
        // Where there is a side to copy to, there is one to check against.
        first.push(item(
            "Check Against Other Pane",
            "",
            Action::CheckAgainstOther,
            context.other_pane,
        ));
    }
    first.push(item("Get Info", "⌘I", Action::GetInfo, true));
    first.push(item("Share…", "", Action::Share, true));
    first.push(item(
        "Add to Sidebar",
        "",
        Action::Pin,
        context.single_folder,
    ));

    let delete = if context.deletes_permanently {
        "Delete…"
    } else {
        "Move to Trash"
    };
    let mut edit = vec![
        item(
            "New Folder with Selection",
            "⌃⌘N",
            Action::NewFolderWithItems,
            context.selected >= 1,
        ),
        item("Rename…", "⌘D", Action::Rename, context.selected >= 1),
        item(delete, "⌘⌫", Action::Delete, true),
    ];
    if context.on_mac {
        edit.push(item("Show in Finder", "", Action::ShowInFinder, true));
    }

    let clipboard = vec![
        item("Cut", "⌘X", Action::Cut, true),
        item("Copy", "⌘C", Action::Copy, true),
        item("Copy to Queue", "⇧⌘C", Action::CopyToQueue, true),
        item("Copy Path", "⌥⌘C", Action::CopyPath, true),
        paste,
    ];

    vec![first, clipboard, edit]
}

/// How big the menu with these sections is drawn; every row has a fixed
/// height, so this is exact.
pub fn size(sections: &[Vec<Item>]) -> Size {
    let items: usize = sections.iter().map(Vec::len).sum();
    let separators = sections.len().saturating_sub(1);
    Size::new(
        WIDTH,
        items as f32 * ITEM_HEIGHT + separators as f32 * SEPARATOR_HEIGHT + 2.0 * PADDING,
    )
}

/// Where the menu's top-left corner goes for a click at `at`: below and to
/// the right of the cursor, flipped to the other side where it would leave
/// `area`, and kept inside it when it is bigger than the room either way.
pub fn place(at: Point, size: Size, area: Rectangle) -> Point {
    let axis = |at: f32, length: f32, start: f32, extent: f32| {
        let end = start + extent;
        let wanted = if at + length <= end { at } else { at - length };
        wanted.min(end - length).max(start)
    };
    Point::new(
        axis(at.x, size.width, area.x, area.width),
        axis(at.y, size.height, area.y, area.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(sections: &[Vec<Item>]) -> Vec<Vec<&str>> {
        sections
            .iter()
            .map(|section| section.iter().map(|item| item.label.as_str()).collect())
            .collect()
    }

    fn find(sections: &[Vec<Item>], action: Action) -> &Item {
        sections
            .iter()
            .flatten()
            .find(|item| item.action == action)
            .unwrap()
    }

    fn on_a_mac_file() -> Context {
        Context {
            on_entry: true,
            selected: 1,
            single_folder: false,
            transfer: Some("Upload".into()),
            can_paste: false,
            on_mac: true,
            deletes_permanently: false,
            other_pane: true,
            edit_on_mac: None,
        }
    }

    #[test]
    fn a_phone_file_can_be_edited_on_the_mac_and_a_mac_file_has_no_such_item() {
        let phone = |single_file| {
            sections(&Context {
                on_entry: true,
                selected: 1,
                edit_on_mac: Some(single_file),
                ..Context::default()
            })
        };
        let file = phone(true);
        let item = find(&file, Action::EditOnMac);
        assert_eq!(item.label, "Edit on Mac");
        assert!(item.enabled);
        assert!(!find(&phone(false), Action::EditOnMac).enabled, "a folder");
        assert!(Action::EditOnMac.is_app_wide());
        let mac = sections(&on_a_mac_file());
        assert!(
            mac.iter()
                .flatten()
                .all(|item| item.action != Action::EditOnMac)
        );
    }

    #[test]
    fn an_item_can_be_checked_against_the_other_pane_once_it_is_shown() {
        let alone = sections(&Context {
            on_entry: true,
            selected: 2,
            transfer: Some("Download".into()),
            ..Context::default()
        });
        let check = find(&alone, Action::CheckAgainstOther);
        assert_eq!(check.label, "Check Against Other Pane");
        assert!(!check.enabled, "nothing to check against");
        let both = sections(&Context {
            on_entry: true,
            selected: 2,
            transfer: Some("Download".into()),
            other_pane: true,
            ..Context::default()
        });
        assert!(find(&both, Action::CheckAgainstOther).enabled);
        assert!(Action::CheckAgainstOther.is_app_wide());
    }

    #[test]
    fn empty_space_offers_folder_wide_commands() {
        let sections = sections(&Context::default());

        assert_eq!(
            labels(&sections),
            [
                vec!["New Folder", "Paste"],
                vec![
                    "Select All",
                    "Select Not in Other Pane",
                    "Select Also in Other Pane",
                    "Refresh"
                ],
                vec!["Get Info", "Add This Folder to Sidebar"]
            ]
        );
        assert!(!find(&sections, Action::Paste).enabled);
        assert!(Action::PinHere.is_app_wide() && Action::Pin.is_app_wide());
        assert!(
            !find(&sections, Action::SelectNotInOther).enabled,
            "no other pane to compare with"
        );
        let both = super::sections(&Context {
            other_pane: true,
            ..Context::default()
        });
        assert!(find(&both, Action::SelectNotInOther).enabled);
        assert!(find(&both, Action::SelectAlsoInOther).enabled);
        assert!(Action::SelectNotInOther.is_app_wide() && Action::SelectAlsoInOther.is_app_wide());
    }

    #[test]
    fn only_a_single_folder_can_be_added_to_the_sidebar() {
        let file = sections(&on_a_mac_file());
        let one_folder = sections(&Context {
            single_folder: true,
            ..on_a_mac_file()
        });

        assert!(!find(&file, Action::Pin).enabled);
        assert!(find(&one_folder, Action::Pin).enabled);
    }

    #[test]
    fn a_mac_file_can_be_opened_sent_copied_edited_and_shown_in_finder() {
        let sections = sections(&on_a_mac_file());

        assert_eq!(
            labels(&sections),
            [
                vec![
                    "Open",
                    "Upload",
                    "Check Against Other Pane",
                    "Get Info",
                    "Share…",
                    "Add to Sidebar"
                ],
                vec!["Cut", "Copy", "Copy to Queue", "Copy Path", "Paste"],
                vec![
                    "New Folder with Selection",
                    "Rename…",
                    "Move to Trash",
                    "Show in Finder"
                ],
            ]
        );
        assert!(
            find(&sections, Action::Open).enabled,
            "a file opens in its Mac app"
        );
        assert!(find(&sections, Action::Rename).enabled);
    }

    #[test]
    fn shortcuts_are_shown_the_mac_way() {
        let sections = sections(&on_a_mac_file());

        assert_eq!(find(&sections, Action::CopyToQueue).shortcut, "⇧⌘C");
        assert_eq!(find(&sections, Action::Delete).shortcut, "⌘⌫");
        assert_eq!(find(&sections, Action::Transfer).shortcut, "");
    }

    #[test]
    fn only_a_single_item_opens() {
        let two = sections(&Context {
            selected: 2,
            ..on_a_mac_file()
        });

        assert!(!find(&two, Action::Open).enabled);
    }

    #[test]
    fn several_items_count_in_the_transfer_and_are_renamed_together() {
        let sections = sections(&Context {
            selected: 3,
            ..on_a_mac_file()
        });

        assert_eq!(find(&sections, Action::Transfer).label, "Upload 3 Items");
        assert!(find(&sections, Action::Rename).enabled);
    }

    #[test]
    fn the_phone_deletes_for_good_and_has_no_finder() {
        let sections = sections(&Context {
            on_mac: false,
            deletes_permanently: true,
            transfer: Some("Download".into()),
            ..on_a_mac_file()
        });

        assert_eq!(find(&sections, Action::Delete).label, "Delete…");
        assert!(
            !sections
                .iter()
                .flatten()
                .any(|item| item.action == Action::ShowInFinder)
        );
    }

    #[test]
    fn without_a_phone_there_is_nothing_to_transfer() {
        let sections = sections(&Context {
            transfer: None,
            ..on_a_mac_file()
        });

        assert_eq!(
            labels(&sections)[0],
            ["Open", "Get Info", "Share…", "Add to Sidebar"]
        );
        assert!(Action::Share.is_app_wide());
    }

    #[test]
    fn paste_follows_the_clipboard() {
        let sections = sections(&Context {
            can_paste: true,
            ..on_a_mac_file()
        });

        assert!(find(&sections, Action::Paste).enabled);
    }

    #[test]
    fn transfer_and_the_clipboard_belong_to_the_app() {
        for action in [
            Action::Transfer,
            Action::Copy,
            Action::CopyToQueue,
            Action::Paste,
            Action::GetInfo,
            Action::GetInfoHere,
        ] {
            assert!(action.is_app_wide(), "{action:?}");
        }
        for action in [
            Action::Open,
            Action::NewFolder,
            Action::NewFolderWithItems,
            Action::CopyPath,
            Action::Rename,
            Action::Delete,
            Action::SelectAll,
            Action::Refresh,
            Action::ShowInFinder,
        ] {
            assert!(!action.is_app_wide(), "{action:?}");
        }
    }

    #[test]
    fn size_counts_rows_separators_and_padding() {
        let sections = sections(&Context::default());

        assert_eq!(
            size(&sections),
            Size::new(
                WIDTH,
                8.0 * ITEM_HEIGHT + 2.0 * SEPARATOR_HEIGHT + 2.0 * PADDING
            )
        );
        assert_eq!(size(&[]), Size::new(WIDTH, 2.0 * PADDING));
    }

    const AREA: Rectangle = Rectangle {
        x: 100.0,
        y: 50.0,
        width: 600.0,
        height: 400.0,
    };
    const MENU: Size = Size {
        width: 240.0,
        height: 120.0,
    };

    #[test]
    fn the_menu_opens_below_and_right_of_the_cursor() {
        assert_eq!(
            place(Point::new(200.0, 100.0), MENU, AREA),
            Point::new(200.0, 100.0)
        );
    }

    #[test]
    fn near_the_right_or_bottom_edge_it_flips() {
        assert_eq!(
            place(Point::new(650.0, 420.0), MENU, AREA),
            Point::new(410.0, 300.0)
        );
    }

    #[test]
    fn a_menu_with_no_room_either_way_stays_inside() {
        let narrow = Rectangle {
            width: 300.0,
            ..AREA
        };

        // At x=300 it overflows the right edge, and flipping left would
        // leave the pane too: it sticks to the left edge.
        assert_eq!(place(Point::new(300.0, 60.0), MENU, narrow).x, 100.0);
        // Too tall for the pane: pinned to the top.
        let tall = Size::new(240.0, 500.0);
        assert_eq!(place(Point::new(200.0, 100.0), tall, AREA).y, 50.0);
    }

    #[test]
    fn both_menus_offer_cut_before_copy() {
        // The phone's cut moves inside it or to the Mac; the Mac's moves to
        // the phone.
        let phone = sections(&Context {
            on_mac: false,
            ..on_a_mac_file()
        });
        let mac = sections(&on_a_mac_file());

        for sections in [&phone, &mac] {
            assert_eq!(
                labels(sections)[1],
                ["Cut", "Copy", "Copy to Queue", "Copy Path", "Paste"],
                "Finder's order"
            );
            assert_eq!(find(sections, Action::Cut).shortcut, "⌘X");
            assert!(find(sections, Action::Cut).enabled);
        }
    }

    #[test]
    fn cut_is_carried_out_by_the_app_because_it_owns_the_clipboard() {
        assert!(Action::Cut.is_app_wide());
        assert!(!Action::Delete.is_app_wide());
    }
}
