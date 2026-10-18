//! The real menu bar, as AppKit treats it. AppKit may only be touched on the
//! main thread, and the test harness runs tests on threads of its own, so
//! this test has a `main` of its own (`harness = false` in `Cargo.toml`). It
//! puts a small menu bar up in the test process (no window, no Dock icon,
//! nothing on the screen) and checks:
//!
//! - a ⌘ key press shown on one of the app's commands is handed to the
//!   window (so a text field keeps ⌘C and ⌘V), and runs no command itself;
//! - one shown on a macOS item (⌘M) stays with the menu;
//! - a click on a command reaches the app with its tag;
//! - greyed and ticked commands show so, and a greyed command still lets its
//!   own key press through.

use std::sync::{Arc, Mutex};

use internal::dto::{Menu, MenuEntry, MenuKeys, MenuRole, MenuState, SystemAction};
use internal::service::local::{
    MenuBar, Sharer, SystemMenuBar, SystemSharer, menu_choose, menu_item_state, menu_press,
    submenu_choose, submenu_items,
};

fn cmd(key: &str) -> Option<MenuKeys> {
    Some(MenuKeys {
        key: key.into(),
        command: true,
        shift: false,
        option: false,
        control: false,
    })
}

fn main() {
    let menus = vec![
        Menu {
            title: "App".into(),
            role: MenuRole::App,
            entries: vec![MenuEntry::Services, MenuEntry::Separator],
        },
        Menu {
            title: "File".into(),
            role: MenuRole::Other,
            entries: vec![
                MenuEntry::Command {
                    title: "New Folder".into(),
                    keys: cmd("n"),
                    tag: 1,
                },
                MenuEntry::Command {
                    title: "Paste".into(),
                    keys: cmd("v"),
                    tag: 2,
                },
                MenuEntry::Command {
                    title: "Show Hidden Files".into(),
                    keys: None,
                    tag: 3,
                },
                MenuEntry::Submenu {
                    title: "Recent Folders".into(),
                    entries: vec![MenuEntry::Command {
                        title: "Clear Menu".into(),
                        keys: None,
                        tag: 4,
                    }],
                },
            ],
        },
        Menu {
            title: "Window".into(),
            role: MenuRole::Window,
            entries: vec![MenuEntry::System {
                title: "Minimize".into(),
                keys: cmd("m"),
                action: SystemAction::Minimize,
            }],
        },
        Menu {
            title: "Help".into(),
            role: MenuRole::Help,
            entries: vec![MenuEntry::System {
                title: "Zoom".into(),
                keys: None,
                action: SystemAction::Zoom,
            }],
        },
    ];
    let clicked = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&clicked);
    SystemMenuBar.install(&menus, Box::new(move |tag| seen.lock().unwrap().push(tag)));

    assert_eq!(
        menu_press("n", true, false, false, false),
        Some(1),
        "⌘N goes to the window, where the keyboard map runs it"
    );
    assert_eq!(
        menu_press("v", true, false, false, false),
        Some(1),
        "⌘V goes to the window: a text field pastes"
    );
    assert!(
        clicked.lock().unwrap().is_empty(),
        "no key press ran a command itself"
    );
    assert_eq!(
        menu_press("m", true, false, false, false),
        Some(0),
        "⌘M is macOS's (Minimize): the menu keeps it"
    );

    menu_choose(1, 0).expect("File > New Folder exists");
    assert_eq!(
        *clicked.lock().unwrap(),
        [1],
        "a click reaches the app with its tag"
    );
    assert_eq!(menu_choose(1, 9), None, "no such item");

    assert_eq!(
        menu_item_state(1, 1),
        Some((true, false)),
        "Paste, as built"
    );
    SystemMenuBar.set_state(&MenuState {
        disabled: vec![2],
        checked: vec![3],
    });
    assert_eq!(
        menu_item_state(1, 1),
        Some((false, false)),
        "Paste is greyed"
    );
    assert_eq!(
        menu_item_state(1, 2),
        Some((true, true)),
        "Show Hidden Files is ticked"
    );
    assert_eq!(
        menu_press("v", true, false, false, false),
        Some(1),
        "a greyed item still lets its own key through"
    );
    assert_eq!(*clicked.lock().unwrap(), [1], "and runs nothing");

    assert_eq!(
        submenu_items("Recent Folders"),
        Some(vec!["Clear Menu".to_owned()]),
        "a submenu of the app's own, as built"
    );
    SystemMenuBar.fill(
        "Recent Folders",
        &[
            MenuEntry::Command {
                title: "Camera".into(),
                keys: None,
                tag: 10_000,
            },
            MenuEntry::Separator,
            MenuEntry::Command {
                title: "Clear Menu".into(),
                keys: None,
                tag: 4,
            },
        ],
    );
    assert_eq!(
        submenu_items("Recent Folders"),
        Some(vec![
            "Camera".to_owned(),
            String::new(),
            "Clear Menu".to_owned()
        ]),
        "filled again in place"
    );
    submenu_choose("Recent Folders", 0).expect("Camera is there");
    assert_eq!(
        *clicked.lock().unwrap(),
        [1, 10_000],
        "a click in a filled submenu reaches the app"
    );
    assert_eq!(submenu_items("Nowhere"), None);
    SystemMenuBar.fill("Nowhere", &[]);

    assert!(
        !SystemSharer.share(&[std::env::temp_dir()], Some((10.0, 10.0))),
        "on the main thread but with no window: the share menu says it can't show"
    );
    println!("menu bar: all checks passed");
}
