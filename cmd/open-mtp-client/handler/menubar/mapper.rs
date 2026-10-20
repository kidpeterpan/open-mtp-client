//! What the menu bar holds, and what each command needs to be usable. The
//! keys a command shows come from the keyboard map's catalog (the table the
//! Help sheet lists and the tests prove), so the menus, the shortcuts and
//! Help can't disagree.

use internal::dto;

use super::handler::{Command, State};
use crate::handler::explorer::{Against, Request};
use crate::handler::help::Tab;
use crate::i18n::t;
use crate::keymap::{self, MacPlace, Shortcut};

/// What the app is called in the menus.
pub const APP_NAME: &str = "open-mtp-client";

/// The submenu in Go listing the folders shown lately.
pub fn recent_title() -> &'static str {
    t("menubar.recent")
}

/// The tag of the first recent folder; the next ones follow. Far above the
/// menus' own commands, whose tags count from 0.
pub const RECENT_TAG: u32 = 10_000;

/// One line of a menu, before tags are given out.
enum Line {
    Command(&'static str, Command),
    System(&'static str, Option<dto::MenuKeys>, dto::SystemAction),
    Services,
    /// Go › Recent Folders, filled from the app's state.
    Recent,
    Separator,
}

fn command(title: &'static str, command: Command) -> Line {
    Line::Command(title, command)
}

fn key(shortcut: Shortcut) -> Command {
    Command::Key(shortcut)
}

fn pane(request: Request) -> Command {
    Command::Key(Shortcut::Pane(request))
}

fn system_keys(key: &str, option: bool, control: bool) -> Option<dto::MenuKeys> {
    Some(dto::MenuKeys {
        key: key.to_owned(),
        command: true,
        shift: false,
        option,
        control,
    })
}

/// The menus, in order, as titles and lines.
fn layout() -> Vec<(&'static str, dto::MenuRole, Vec<Line>)> {
    use dto::MenuRole::{App, Help, Other, Window};
    use dto::SystemAction::{
        BringAllToFront, FullScreen, Hide, HideOthers, Minimize, ShowAll, Zoom,
    };
    vec![
        (
            APP_NAME,
            App,
            vec![
                command(t("menubar.about"), Command::Help(Tab::About)),
                Line::Separator,
                command(t("menubar.settings"), key(Shortcut::Settings)),
                Line::Separator,
                Line::Services,
                Line::Separator,
                Line::System(t("menubar.hide"), system_keys("h", false, false), Hide),
                Line::System(
                    t("menubar.hide_others"),
                    system_keys("h", true, false),
                    HideOthers,
                ),
                Line::System(t("menubar.show_all"), None, ShowAll),
                Line::Separator,
                // Ours, not AppKit's terminate: a running transfer is asked
                // about and stopped where it can carry on.
                command(t("menubar.quit"), key(Shortcut::Quit)),
            ],
        ),
        (
            t("menubar.file"),
            Other,
            vec![
                command(t("menu.item.new_folder"), pane(Request::NewFolder)),
                command(
                    t("menu.item.new_folder_with_selection"),
                    pane(Request::NewFolderWithItems),
                ),
                Line::Separator,
                command(t("menu.item.open"), pane(Request::OpenSelected)),
                command(t("menubar.quick_look"), key(Shortcut::QuickLook)),
                command(t("menu.item.get_info"), key(Shortcut::GetInfo)),
                command(t("menu.item.share"), Command::Share),
                command(t("menu.item.edit_on_mac"), Command::EditOnMac),
                Line::Separator,
                command(t("menu.item.rename"), pane(Request::Rename)),
                command(t("menu.item.delete"), pane(Request::Delete)),
                Line::Separator,
                command(t("menubar.import"), Command::ImportPhotos),
                command(t("explorer.action.mirror"), Command::Mirror),
            ],
        ),
        (
            t("menubar.edit"),
            Other,
            vec![
                command(t("menubar.undo"), key(Shortcut::Undo)),
                Line::Separator,
                command(t("menu.item.cut"), key(Shortcut::Cut)),
                command(t("menu.item.copy"), key(Shortcut::Copy)),
                command(t("menubar.add_to_copied"), key(Shortcut::CopyToQueue)),
                command(t("menu.item.copy_path"), pane(Request::CopyPath)),
                command(t("menu.item.paste"), key(Shortcut::Paste)),
                Line::Separator,
                command(t("menu.item.select_all"), pane(Request::SelectAll)),
                command(
                    t("menu.item.select_not_in_other"),
                    Command::SelectAgainst(Against::Missing),
                ),
                command(
                    t("menu.item.select_also_in_other"),
                    Command::SelectAgainst(Against::Same),
                ),
                command(t("menubar.check_contents"), Command::CheckContents),
                Line::Separator,
                command(t("menubar.filter"), pane(Request::FocusFilter)),
                command(t("menubar.search_phone"), key(Shortcut::Search)),
            ],
        ),
        (
            t("menubar.view"),
            Other,
            vec![
                command(t("menubar.as_list"), Command::ViewAs(dto::ViewMode::List)),
                command(t("menubar.as_grid"), Command::ViewAs(dto::ViewMode::Grid)),
                command(t("menubar.tiles_bigger"), key(Shortcut::TilesBigger)),
                command(t("menubar.tiles_smaller"), key(Shortcut::TilesSmaller)),
                Line::Separator,
                command(t("menubar.show_hidden"), key(Shortcut::ToggleHidden)),
                command(t("menubar.folder_sizes"), Command::ToggleFolderSizes),
                Line::Separator,
                command(t("menubar.use_groups"), key(Shortcut::UseGroups)),
                command(
                    t("menubar.group_date"),
                    Command::GroupBy(dto::GroupBy::Date),
                ),
                command(
                    t("menubar.group_kind"),
                    Command::GroupBy(dto::GroupBy::Kind),
                ),
                command(
                    t("menubar.group_size"),
                    Command::GroupBy(dto::GroupBy::Size),
                ),
                Line::Separator,
                command(t("menubar.reload"), pane(Request::Refresh)),
                Line::Separator,
                Line::System(
                    t("menubar.full_screen"),
                    system_keys("f", false, true),
                    FullScreen,
                ),
            ],
        ),
        (
            t("menubar.go"),
            Other,
            vec![
                command(t("menubar.back"), pane(Request::Back)),
                command(t("menubar.forward"), pane(Request::Forward)),
                command(t("menubar.enclosing"), pane(Request::Up)),
                command(t("menubar.go_to_folder"), pane(Request::GoToFolder)),
                Line::Separator,
                command(t("places.home"), key(Shortcut::GoToMac(MacPlace::Home))),
                command(
                    t("places.desktop"),
                    key(Shortcut::GoToMac(MacPlace::Desktop)),
                ),
                command(
                    t("places.downloads"),
                    key(Shortcut::GoToMac(MacPlace::Downloads)),
                ),
                Line::Separator,
                command(t("menubar.switch_pane"), key(Shortcut::SwitchPane)),
                Line::Separator,
                Line::Recent,
            ],
        ),
        (
            t("menubar.window"),
            Window,
            vec![
                Line::System(
                    t("menubar.minimize"),
                    system_keys("m", false, false),
                    Minimize,
                ),
                Line::System(t("menubar.zoom"), None, Zoom),
                Line::Separator,
                Line::System(t("menubar.bring_all"), None, BringAllToFront),
            ],
        ),
        (
            t("menubar.help"),
            Help,
            vec![
                command(t("menubar.help_app"), key(Shortcut::Help)),
                command(
                    t("menubar.keyboard_shortcuts"),
                    Command::Help(Tab::Shortcuts),
                ),
                command(
                    t("help.tab.troubleshooting"),
                    Command::Help(Tab::Troubleshooting),
                ),
                command(t("menubar.whats_new"), Command::Help(Tab::WhatsNew)),
                Line::Separator,
                command(t("menubar.report_problem"), Command::ReportProblem),
            ],
        ),
    ]
}

/// The key a command shows: its catalog row's first ⌘ press.
fn keys_for(command: &Command, catalog: &[keymap::Row]) -> Option<dto::MenuKeys> {
    let Command::Key(shortcut) = command else {
        return None;
    };
    catalog
        .iter()
        .find(|row| row.action() == shortcut)
        .and_then(keymap::Row::menu_keys)
}

/// The menu bar, and the command behind each tag (a tag is its index).
pub fn menus() -> (Vec<dto::Menu>, Vec<Command>) {
    let catalog = keymap::catalog();
    let mut commands = Vec::new();
    let menus = layout()
        .into_iter()
        .map(|(title, role, lines)| dto::Menu {
            title: title.to_owned(),
            role,
            entries: lines
                .into_iter()
                .map(|line| match line {
                    Line::Command(title, command) => {
                        let keys = keys_for(&command, &catalog);
                        let tag = commands.len() as u32;
                        commands.push(command);
                        dto::MenuEntry::Command {
                            title: title.to_owned(),
                            keys,
                            tag,
                        }
                    }
                    Line::System(title, keys, action) => dto::MenuEntry::System {
                        title: title.to_owned(),
                        keys,
                        action,
                    },
                    Line::Services => dto::MenuEntry::Services,
                    Line::Recent => {
                        commands.push(Command::ClearRecent);
                        dto::MenuEntry::Submenu {
                            title: recent_title().to_owned(),
                            entries: recent_entries(&commands, &[]),
                        }
                    }
                    Line::Separator => dto::MenuEntry::Separator,
                })
                .collect(),
        })
        .collect();
    (menus, commands)
}

/// What Go › Recent Folders holds: each folder (tagged from `RECENT_TAG`),
/// then Clear Menu, as Finder's has.
pub fn recent_entries(commands: &[Command], recent: &[String]) -> Vec<dto::MenuEntry> {
    let clear = commands
        .iter()
        .position(|command| *command == Command::ClearRecent)
        .map(|tag| dto::MenuEntry::Command {
            title: t("menubar.clear_menu").to_owned(),
            keys: None,
            tag: tag as u32,
        });
    let folders = recent
        .iter()
        .enumerate()
        .map(|(at, title)| dto::MenuEntry::Command {
            title: title.clone(),
            keys: None,
            tag: RECENT_TAG + at as u32,
        });
    let separator = (!recent.is_empty()).then_some(dto::MenuEntry::Separator);
    folders.chain(separator).chain(clear).collect()
}

/// `menus` with Go › Recent Folders filled with `recent`.
pub fn with_recent(menus: &[dto::Menu], commands: &[Command], recent: &[String]) -> Vec<dto::Menu> {
    let mut menus = menus.to_vec();
    for entry in menus.iter_mut().flat_map(|menu| menu.entries.iter_mut()) {
        if let dto::MenuEntry::Submenu { title, entries } = entry
            && title == recent_title()
        {
            *entries = recent_entries(commands, recent);
        }
    }
    menus
}

/// Whether `command` can be used in `state`. What a key press would do
/// nothing with is greyed out.
pub fn is_enabled(command: &Command, state: &State) -> bool {
    match command {
        // The app's own windows open, and it quits, whatever else is up.
        Command::Help(_)
        | Command::ReportProblem
        | Command::Key(Shortcut::Settings | Shortcut::Help | Shortcut::Quit) => true,
        _ if state.dialog_open => false,
        Command::Key(
            Shortcut::Cut
            | Shortcut::Copy
            | Shortcut::CopyToQueue
            | Shortcut::QuickLook
            | Shortcut::Pane(
                Request::OpenSelected
                | Request::Delete
                | Request::NewFolderWithItems
                | Request::CopyPath,
            ),
        )
        | Command::Share => state.pane_open && state.selected > 0,
        Command::Key(Shortcut::Pane(Request::Rename)) => state.pane_open && state.selected >= 1,
        Command::Key(Shortcut::Undo) => state.can_undo,
        Command::Key(Shortcut::Pane(Request::Back)) => state.can_go_back,
        Command::Key(Shortcut::Pane(Request::Forward)) => state.can_go_forward,
        Command::Key(Shortcut::Paste) => state.pane_open && state.can_paste,
        Command::Key(Shortcut::Search) | Command::ImportPhotos | Command::Mirror => {
            state.phone_open
        }
        Command::Key(Shortcut::SwitchPane) => true,
        // The Mac pane is always there to go to, shown or not.
        Command::Key(Shortcut::GoToMac(_)) => !state.dialog_open,
        Command::Key(Shortcut::TilesBigger) => {
            state.pane_open && state.view == dto::ViewMode::Grid && !state.tiles_biggest
        }
        Command::Key(Shortcut::TilesSmaller) => {
            state.pane_open && state.view == dto::ViewMode::Grid && !state.tiles_smallest
        }
        Command::Recent(_) => true,
        Command::SelectAgainst(_) => state.can_compare,
        Command::CheckContents => state.can_compare && state.selected > 0,
        Command::EditOnMac => state.can_edit && !state.dialog_open,
        Command::ClearRecent => !state.recent.is_empty(),
        Command::Key(_) | Command::ToggleFolderSizes | Command::ViewAs(_) | Command::GroupBy(_) => {
            state.pane_open
        }
    }
}

/// Whether `command` shows a tick in `state`.
pub fn is_checked(command: &Command, state: &State) -> bool {
    match command {
        Command::Key(Shortcut::ToggleHidden) => state.show_hidden,
        Command::ToggleFolderSizes => state.folder_sizes,
        Command::ViewAs(view) => *view == state.view,
        Command::Key(Shortcut::UseGroups) => state.group_by != dto::GroupBy::None,
        Command::GroupBy(by) => *by == state.group_by,
        _ => false,
    }
}

/// The greyed and ticked tags in `state`.
pub fn to_menu_state(commands: &[Command], state: &State) -> dto::MenuState {
    let tags = |keep: &dyn Fn(&Command) -> bool| {
        commands
            .iter()
            .enumerate()
            .filter(|(_, command)| keep(command))
            .map(|(tag, _)| tag as u32)
            .collect()
    };
    let mut disabled: Vec<u32> = tags(&|command| !is_enabled(command, state));
    // A recent folder is a place to go: not while a dialog is up.
    if state.dialog_open {
        disabled.extend((0..state.recent.len() as u32).map(|at| RECENT_TAG + at));
    }
    dto::MenuState {
        disabled,
        checked: tags(&|command| is_checked(command, state)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry, those in submenus too.
    fn flat(menus: &[dto::Menu]) -> Vec<dto::MenuEntry> {
        fn walk(entries: &[dto::MenuEntry], out: &mut Vec<dto::MenuEntry>) {
            for entry in entries {
                out.push(entry.clone());
                if let dto::MenuEntry::Submenu { entries, .. } = entry {
                    walk(entries, out);
                }
            }
        }
        let mut out = Vec::new();
        for menu in menus {
            walk(&menu.entries, &mut out);
        }
        out
    }

    fn commands_with_keys() -> Vec<(String, Command, Option<dto::MenuKeys>)> {
        let (menus, commands) = menus();
        flat(&menus)
            .iter()
            .filter_map(|entry| match entry {
                dto::MenuEntry::Command { title, keys, tag } => {
                    Some((title.clone(), commands[*tag as usize].clone(), keys.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Shortcuts that belong to moving inside a list, not to a menu.
    fn not_for_menus(shortcut: &Shortcut) -> bool {
        matches!(
            shortcut,
            Shortcut::Pane(
                Request::Step { .. }
                    | Request::Sideways { .. }
                    | Request::Jump { .. }
                    | Request::Escape
            ) | Shortcut::TypeSelect(_)
        )
    }

    #[test]
    fn every_shortcut_has_a_menu_item_and_every_menu_key_is_a_shortcut() {
        let items = commands_with_keys();
        for row in keymap::catalog() {
            if not_for_menus(row.action()) {
                continue;
            }
            let item = items
                .iter()
                .find(|(_, command, _)| *command == Command::Key(row.action().clone()));
            let Some((title, _, keys)) = item else {
                panic!("“{}” ({}) is in no menu", row.label, row.keys);
            };
            assert_eq!(
                *keys,
                row.menu_keys(),
                "“{title}” shows the key Help lists for “{}”",
                row.label
            );
        }
        for (title, command, keys) in &items {
            if let Command::Key(shortcut) = command {
                assert!(
                    keymap::catalog().iter().any(|row| row.action() == shortcut),
                    "“{title}” runs a shortcut Help doesn't list"
                );
            } else {
                assert_eq!(*keys, None, "“{title}” has no shortcut, so shows none");
            }
        }
    }

    #[test]
    fn go_holds_the_macs_own_places_with_finders_keys_greyed_only_under_a_dialog() {
        let items = commands_with_keys();
        let shown = |title: &str| {
            let (_, command, keys) = items.iter().find(|(t, ..)| t == title).unwrap();
            let keys = keys.clone().unwrap();
            (command.clone(), keys.key, keys.shift, keys.option)
        };
        assert_eq!(
            shown("Home"),
            (
                key(Shortcut::GoToMac(MacPlace::Home)),
                "h".into(),
                true,
                false
            )
        );
        assert_eq!(
            shown("Desktop"),
            (
                key(Shortcut::GoToMac(MacPlace::Desktop)),
                "d".into(),
                true,
                false
            )
        );
        assert_eq!(
            shown("Downloads"),
            (
                key(Shortcut::GoToMac(MacPlace::Downloads)),
                "l".into(),
                false,
                true
            )
        );
        assert_eq!(
            shown("Show Hidden Files"),
            (key(Shortcut::ToggleHidden), ".".into(), true, false)
        );

        let home = key(Shortcut::GoToMac(MacPlace::Home));
        let no_pane = State {
            pane_open: false,
            ..state()
        };
        assert!(is_enabled(&home, &no_pane), "the Mac pane is always there");
        let dialog = State {
            dialog_open: true,
            ..state()
        };
        assert!(!is_enabled(&home, &dialog));
    }

    #[test]
    fn edit_selects_against_the_other_pane_only_once_both_have_listed() {
        let items = commands_with_keys();
        for (title, want) in [
            ("Select Not in Other Pane", Against::Missing),
            ("Select Also in Other Pane", Against::Same),
        ] {
            let (_, command, keys) = items.iter().find(|(t, ..)| t == title).unwrap();
            assert_eq!(*command, Command::SelectAgainst(want));
            assert_eq!(*keys, None);
            assert!(!is_enabled(command, &state()));
            let both = State {
                can_compare: true,
                ..state()
            };
            assert!(is_enabled(command, &both));
        }
    }

    #[test]
    fn edit_on_mac_needs_one_phone_file_and_no_dialog() {
        let items = commands_with_keys();
        let (_, command, _) = items
            .iter()
            .find(|(title, ..)| title == "Edit on Mac")
            .unwrap();
        assert_eq!(*command, Command::EditOnMac);
        for (can_edit, dialog_open, enabled) in [
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let state = State {
                can_edit,
                dialog_open,
                ..state()
            };
            assert_eq!(is_enabled(command, &state), enabled);
        }
    }

    #[test]
    fn use_groups_and_group_by_are_ticked_as_the_active_pane_is_grouped() {
        let items = commands_with_keys();
        let find = |title: &str| items.iter().find(|(t, ..)| t == title).unwrap().1.clone();
        let use_groups = find("Use Groups");
        assert_eq!(use_groups, key(Shortcut::UseGroups));
        let by_kind = find("Group by Kind");
        let grouped = State {
            group_by: dto::GroupBy::Kind,
            ..state()
        };
        assert!(is_checked(&use_groups, &grouped));
        assert!(is_checked(&by_kind, &grouped));
        assert!(!is_checked(&find("Group by Date"), &grouped));
        assert!(!is_checked(&use_groups, &state()));
        assert!(is_enabled(&by_kind, &state()));
        assert!(!is_enabled(
            &by_kind,
            &State {
                pane_open: false,
                ..state()
            }
        ));
    }

    #[test]
    fn check_contents_needs_both_panes_listed_and_something_selected() {
        let items = commands_with_keys();
        let (_, command, keys) = items
            .iter()
            .find(|(title, ..)| title == "Check Contents Against Other Pane")
            .unwrap();
        assert_eq!(*command, Command::CheckContents);
        assert_eq!(*keys, None);
        for (can_compare, selected, enabled) in
            [(false, 3, false), (true, 0, false), (true, 1, true)]
        {
            let state = State {
                can_compare,
                selected,
                ..state()
            };
            assert_eq!(
                is_enabled(command, &state),
                enabled,
                "{can_compare} {selected}"
            );
        }
    }

    #[test]
    fn quit_is_ours_with_command_q_and_never_greyed() {
        let items = commands_with_keys();
        let (_, command, keys) = items
            .iter()
            .find(|(title, ..)| title == "Quit open-mtp-client")
            .unwrap();
        assert_eq!(*command, key(Shortcut::Quit));
        let keys = keys.clone().unwrap();
        assert_eq!(
            (keys.key.as_str(), keys.command, keys.shift),
            ("q", true, false)
        );
        let blocked = State {
            dialog_open: true,
            pane_open: false,
            ..state()
        };
        assert!(is_enabled(command, &blocked));
    }

    #[test]
    fn every_key_shown_holds_command_and_none_is_shown_twice() {
        let (menus, _) = menus();
        let mut seen = Vec::new();
        for entry in &flat(&menus) {
            let (title, keys) = match entry {
                dto::MenuEntry::Command { title, keys, .. }
                | dto::MenuEntry::System { title, keys, .. } => (title, keys),
                _ => continue,
            };
            let Some(keys) = keys else { continue };
            assert!(
                keys.command,
                "“{title}”: typing must never pass through a menu"
            );
            assert!(!seen.contains(keys), "“{title}” shows a key already shown");
            seen.push(keys.clone());
        }
    }

    #[test]
    fn tags_number_the_commands_in_order() {
        let (menus, commands) = menus();
        let tags: Vec<u32> = flat(&menus)
            .iter()
            .filter_map(|entry| match entry {
                dto::MenuEntry::Command { tag, .. } => Some(*tag),
                _ => None,
            })
            .collect();
        assert_eq!(tags, (0..commands.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn the_menus_are_the_macs_usual_ones() {
        let (menus, _) = menus();
        let titles: Vec<&str> = menus.iter().map(|menu| menu.title.as_str()).collect();
        assert_eq!(
            titles,
            [APP_NAME, "File", "Edit", "View", "Go", "Window", "Help"]
        );
        assert_eq!(menus[5].role, dto::MenuRole::Window);
        assert_eq!(menus[6].role, dto::MenuRole::Help);
        let shown = |title: &str| {
            commands_with_keys()
                .into_iter()
                .find(|(name, _, _)| name == title)
                .and_then(|(_, _, keys)| keys)
                .map(|keys| (keys.key, keys.shift))
        };
        assert_eq!(shown("Settings…"), Some((",".into(), false)));
        assert_eq!(shown("Add to Copied"), Some(("c".into(), true)));
        assert_eq!(
            shown("Open"),
            Some(("\u{F701}".into(), false)),
            "⌘↓, not ↵: a plain key is never a menu's"
        );
        assert_eq!(shown("Delete…"), Some(("\u{8}".into(), false)), "⌘⌫");
        assert_eq!(shown("Quick Look"), Some(("y".into(), false)), "⌘Y");
    }

    fn state() -> State {
        State {
            dialog_open: false,
            pane_open: true,
            selected: 0,
            can_paste: false,
            phone_open: false,
            show_hidden: false,
            folder_sizes: false,
            group_by: dto::GroupBy::None,
            view: dto::ViewMode::List,
            tiles_biggest: false,
            tiles_smallest: false,
            can_undo: false,
            can_go_back: false,
            can_go_forward: false,
            recent: Vec::new(),
            can_compare: false,
            can_edit: false,
        }
    }

    #[test]
    fn recent_folders_end_with_clear_menu_greyed_while_there_are_none() {
        let (menus, commands) = menus();
        let clear = commands
            .iter()
            .position(|command| *command == Command::ClearRecent)
            .unwrap() as u32;
        let none = recent_entries(&commands, &[]);
        assert!(matches!(
            none.as_slice(),
            [dto::MenuEntry::Command { title, tag, .. }] if title == "Clear Menu" && *tag == clear
        ));
        assert!(to_menu_state(&commands, &state()).disabled.contains(&clear));

        let two = State {
            recent: vec!["Desktop".into(), "Camera — Pixel 9".into()],
            ..state()
        };
        let filled = with_recent(&menus, &commands, &two.recent);
        let go = filled.iter().find(|menu| menu.title == "Go").unwrap();
        let Some(dto::MenuEntry::Submenu { entries, .. }) = go.entries.last() else {
            panic!("Recent Folders is last in Go");
        };
        let titles: Vec<String> = entries
            .iter()
            .map(|entry| match entry {
                dto::MenuEntry::Command { title, .. } => title.clone(),
                dto::MenuEntry::Separator => "—".into(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(titles, ["Desktop", "Camera — Pixel 9", "—", "Clear Menu"]);
        let ready = to_menu_state(&commands, &two);
        assert!(!ready.disabled.contains(&clear));
        assert!(!ready.disabled.contains(&RECENT_TAG));

        let dialog = to_menu_state(
            &commands,
            &State {
                dialog_open: true,
                ..two
            },
        );
        assert!(dialog.disabled.contains(&RECENT_TAG));
        assert!(dialog.disabled.contains(&(RECENT_TAG + 1)));
        assert!(dialog.disabled.contains(&clear));
    }

    #[test]
    fn what_a_key_would_do_nothing_with_is_greyed() {
        let none = state();
        assert!(!is_enabled(&key(Shortcut::Copy), &none), "nothing selected");
        assert!(!is_enabled(&pane(Request::Rename), &none));
        assert!(!is_enabled(&pane(Request::NewFolderWithItems), &none));
        assert!(!is_enabled(&pane(Request::CopyPath), &none));
        assert!(!is_enabled(&Command::Share, &none), "nothing to share");
        assert!(!is_enabled(&key(Shortcut::Paste), &none), "nothing copied");
        assert!(!is_enabled(&Command::ImportPhotos, &none), "no phone");
        assert!(is_enabled(&pane(Request::NewFolder), &none));
        assert!(
            !is_enabled(&pane(Request::Back), &none),
            "nowhere to go back to"
        );
        assert!(!is_enabled(&pane(Request::Forward), &none));
        let walked = State {
            can_go_back: true,
            can_go_forward: true,
            ..state()
        };
        assert!(is_enabled(&pane(Request::Back), &walked));
        assert!(is_enabled(&pane(Request::Forward), &walked));

        let one = State {
            selected: 1,
            can_paste: true,
            phone_open: true,
            ..state()
        };
        assert!(is_enabled(&key(Shortcut::Copy), &one));
        assert!(is_enabled(&pane(Request::Rename), &one));
        assert!(is_enabled(&pane(Request::NewFolderWithItems), &one));
        assert!(is_enabled(&pane(Request::CopyPath), &one));
        assert!(is_enabled(&Command::Share, &one));
        assert!(is_enabled(&key(Shortcut::Paste), &one));
        assert!(is_enabled(&Command::Mirror, &one));
        assert!(is_enabled(&key(Shortcut::Search), &one));
        let two = State {
            selected: 2,
            ..one.clone()
        };
        assert!(
            is_enabled(&pane(Request::Rename), &two),
            "several are renamed together"
        );

        let closed = State {
            pane_open: false,
            ..one
        };
        assert!(!is_enabled(&pane(Request::NewFolder), &closed));
        assert!(!is_enabled(&key(Shortcut::Copy), &closed));
    }

    #[test]
    fn tiles_grow_and_shrink_only_in_a_grid_and_up_to_their_limits() {
        let bigger = key(Shortcut::TilesBigger);
        let smaller = key(Shortcut::TilesSmaller);
        let grid = State {
            view: dto::ViewMode::Grid,
            ..state()
        };
        assert!(is_enabled(&bigger, &grid) && is_enabled(&smaller, &grid));
        assert!(!is_enabled(&bigger, &state()), "a list has no tiles");
        assert!(!is_enabled(&smaller, &state()));
        let biggest = State {
            tiles_biggest: true,
            ..grid.clone()
        };
        assert!(!is_enabled(&bigger, &biggest) && is_enabled(&smaller, &biggest));
        let smallest = State {
            tiles_smallest: true,
            ..grid
        };
        assert!(is_enabled(&bigger, &smallest) && !is_enabled(&smaller, &smallest));
    }

    #[test]
    fn a_dialog_greys_everything_but_the_apps_own_windows() {
        let dialog = State {
            dialog_open: true,
            selected: 3,
            can_paste: true,
            phone_open: true,
            ..state()
        };
        let (_, commands) = menus();
        for command in &commands {
            let own = matches!(
                command,
                Command::Help(_)
                    | Command::ReportProblem
                    | Command::Key(Shortcut::Settings | Shortcut::Help | Shortcut::Quit)
            );
            assert_eq!(is_enabled(command, &dialog), own, "{command:?}");
        }
    }

    #[test]
    fn the_view_and_hidden_files_are_ticked_as_they_are() {
        let (_, commands) = menus();
        let tag = |wanted: &Command| commands.iter().position(|c| c == wanted).unwrap() as u32;
        let grid = State {
            view: dto::ViewMode::Grid,
            show_hidden: true,
            ..state()
        };

        let menu_state = to_menu_state(&commands, &grid);

        assert_eq!(
            menu_state.checked,
            [
                tag(&Command::ViewAs(dto::ViewMode::Grid)),
                tag(&key(Shortcut::ToggleHidden))
            ]
        );
        assert!(menu_state.disabled.contains(&tag(&key(Shortcut::Copy))));
        assert!(
            !menu_state
                .disabled
                .contains(&tag(&pane(Request::NewFolder)))
        );
    }
}
