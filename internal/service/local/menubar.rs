//! The Mac's menu bar for the app. A seam: the app puts the real menus up,
//! tests record.
//!
//! **Key presses still reach the window.** AppKit offers a key press to the
//! menu bar before the window hears of it, so a menu item showing ⌘V would
//! take ⌘V away from a text field (the filter, a new name). A menu's
//! delegate can't stop that (`menuHasKeyEquivalent:…` answering no was
//! asked and then ignored: the `menu_bar` test showed it). So when one of
//! the app's commands is chosen by its own key press (AppKit's current event
//! is that press), it hands the press on to the window instead of acting,
//! and the app's keyboard map (or the text field) takes it as before; a
//! greyed item still lets its own press through. A click calls the app back
//! with the command's tag. Items macOS acts on (⌘Q, ⌘H, ⌘M, …) behave as in
//! any app.
//!
//! AppKit may only be touched on the main thread (where the app's updates
//! run); anywhere else, tests included, nothing happens.

use std::sync::Mutex;

use crate::dto::{Menu, MenuEntry, MenuKeys, MenuState};

/// What a click on one of the app's commands calls, with its tag.
pub type OnClick = Box<dyn Fn(u32) + Send + Sync>;

pub trait MenuBar: Send + Sync {
    /// Puts `menus` on the Mac's menu bar, in place of what was there; a
    /// click on one of the app's commands calls `on_click` with its tag.
    fn install(&self, menus: &[Menu], on_click: OnClick);
    /// Greys out and ticks the app's commands, as the menus will show them
    /// next time one opens.
    fn set_state(&self, state: &MenuState);
    /// Puts `entries` in the submenu called `title`, in place of what it held.
    fn fill(&self, title: &str, entries: &[MenuEntry]);
}

/// What tests see: every set of menus put up and every state, and a way to
/// click a command.
#[derive(Default)]
pub struct RecordingMenuBar {
    pub installed: Mutex<Vec<Vec<Menu>>>,
    pub states: Mutex<Vec<MenuState>>,
    /// Every submenu filled again: its title and what it holds.
    pub filled: Mutex<Vec<(String, Vec<MenuEntry>)>>,
    on_click: Mutex<Option<OnClick>>,
}

impl RecordingMenuBar {
    /// Clicks the command `tag`, as a user would; false before any menus
    /// were put up.
    pub fn click(&self, tag: u32) -> bool {
        match self.on_click.lock().unwrap().as_ref() {
            Some(on_click) => {
                on_click(tag);
                true
            }
            None => false,
        }
    }
}

impl MenuBar for RecordingMenuBar {
    fn install(&self, menus: &[Menu], on_click: OnClick) {
        self.installed.lock().unwrap().push(menus.to_vec());
        *self.on_click.lock().unwrap() = Some(on_click);
    }

    fn set_state(&self, state: &MenuState) {
        self.states.lock().unwrap().push(state.clone());
    }

    fn fill(&self, title: &str, entries: &[MenuEntry]) {
        self.filled
            .lock()
            .unwrap()
            .push((title.to_owned(), entries.to_vec()));
    }
}

/// The real menu bar.
#[derive(Default)]
pub struct SystemMenuBar;

impl MenuBar for SystemMenuBar {
    fn install(&self, menus: &[Menu], on_click: OnClick) {
        system::install(menus, on_click);
    }

    fn set_state(&self, state: &MenuState) {
        system::set_state(state);
    }

    fn fill(&self, title: &str, entries: &[MenuEntry]) {
        system::fill(title, entries);
    }
}

/// The modifiers held with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Held {
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

/// Whether a key press (the characters it typed, modifiers aside, and the
/// modifiers held) is `keys`. Letters match in either case: with ⇧ held,
/// AppKit reports the capital.
pub(crate) fn is_press_of(keys: &MenuKeys, typed: &str, held: Held) -> bool {
    !keys.key.is_empty()
        && same_key(typed, &keys.key)
        && held
            == Held {
                command: keys.command,
                shift: keys.shift,
                option: keys.option,
                control: keys.control,
            }
}

/// The ⌫ key types DEL (0x7F), while a menu writes it as backspace (0x08);
/// letters match in either case.
fn same_key(typed: &str, key: &str) -> bool {
    let normal = |text: &str| text.replace('\u{7f}', "\u{8}").to_lowercase();
    normal(typed) == normal(key)
}

mod system {
    use std::cell::RefCell;
    use std::collections::HashSet;

    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, Sel};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_app_kit::{
        NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEvent,
        NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSWindow,
    };
    use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSString};

    use super::{Held, OnClick, is_press_of};
    use crate::dto::{Menu, MenuEntry, MenuKeys, MenuRole, MenuState, SystemAction};

    pub(super) struct Ivars {
        on_click: Box<dyn Fn(u32)>,
        disabled: RefCell<HashSet<u32>>,
        checked: RefCell<HashSet<u32>>,
    }

    declare_class!(
        /// Receives the app's commands from the menus and says which can be
        /// used.
        pub(super) struct Target;

        // SAFETY: NSObject has no subclassing requirements; the class is
        // only used on the main thread (`MainThreadOnly`) and has no `Drop`.
        unsafe impl ClassType for Target {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "OpenMtpClientMenuTarget";
        }

        impl DeclaredClass for Target {
            type Ivars = Ivars;
        }

        unsafe impl Target {
            /// A command was chosen: by a click (or from the keyboard inside
            /// an open menu), or by its own key press. A key press is handed
            /// to the window, where the app's keyboard map (or a text field)
            /// takes it as if no menu showed that key.
            #[method(commandChosen:)]
            fn command_chosen(&self, item: &NSMenuItem) {
                if let Some(event) = own_key_press(item) {
                    forward(&event);
                    return;
                }
                // SAFETY: reading a menu item's tag on the main thread.
                if let Ok(tag) = u32::try_from(unsafe { item.tag() }) {
                    (self.ivars().on_click)(tag);
                }
            }

            /// Greyed out and ticked as the app last said. An item's own key
            /// press is let through even when it is greyed out, to reach the
            /// window: a text field pastes with ⌘V whatever the files' Paste
            /// can do.
            #[method(validateMenuItem:)]
            fn validate_menu_item(&self, item: &NSMenuItem) -> Bool {
                // SAFETY: reading a menu item's tag on the main thread.
                let Ok(tag) = u32::try_from(unsafe { item.tag() }) else {
                    return Bool::YES;
                };
                let on = self.ivars().checked.borrow().contains(&tag);
                // SAFETY: setting a state AppKit defines, on the main thread.
                unsafe {
                    item.setState(if on {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    })
                };
                Bool::new(
                    own_key_press(item).is_some()
                        || !self.ivars().disabled.borrow().contains(&tag),
                )
            }
        }

        unsafe impl NSObjectProtocol for Target {}
    );

    impl Target {
        fn new(mtm: MainThreadMarker, on_click: Box<dyn Fn(u32)>) -> Retained<Self> {
            let this = mtm.alloc::<Self>().set_ivars(Ivars {
                on_click,
                disabled: RefCell::new(HashSet::new()),
                checked: RefCell::new(HashSet::new()),
            });
            // SAFETY: NSObject's designated initialiser, on a fresh object.
            unsafe { msg_send_id![super(this), init] }
        }
    }

    thread_local! {
        /// The menus' target: menu items only point at it weakly, so it is
        /// kept here for as long as the menus are up.
        static TARGET: RefCell<Option<Retained<Target>>> = const { RefCell::new(None) };
        /// How many key presses the menus handed to the window: what the
        /// `menu_bar` test checks.
        static FORWARDED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// The key press under way, if it is `item`'s own key equivalent: what
    /// AppKit is handling when it chose the item by its key.
    fn own_key_press(item: &NSMenuItem) -> Option<Retained<NSEvent>> {
        let mtm = MainThreadMarker::new()?;
        let event = NSApplication::sharedApplication(mtm).currentEvent()?;
        // SAFETY: reading an event and a menu item on the main thread.
        unsafe {
            if event.r#type() != NSEventType::KeyDown {
                return None;
            }
            let typed = event.charactersIgnoringModifiers()?.to_string();
            is_press_of(&keys_of(item), &typed, held(event.modifierFlags())).then_some(event)
        }
    }

    /// Hands a key press to the window in front, as if no menu had seen it.
    fn forward(event: &NSEvent) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        FORWARDED.with(|count| count.set(count.get() + 1));
        if let Some(window) = NSApplication::sharedApplication(mtm).keyWindow() {
            window.sendEvent(event);
        }
    }

    fn held(flags: NSEventModifierFlags) -> Held {
        Held {
            command: flags.contains(NSEventModifierFlags::NSEventModifierFlagCommand),
            shift: flags.contains(NSEventModifierFlags::NSEventModifierFlagShift),
            option: flags.contains(NSEventModifierFlags::NSEventModifierFlagOption),
            control: flags.contains(NSEventModifierFlags::NSEventModifierFlagControl),
        }
    }

    fn mask(keys: &MenuKeys) -> NSEventModifierFlags {
        let mut mask = NSEventModifierFlags::empty();
        for (on, flag) in [
            (
                keys.command,
                NSEventModifierFlags::NSEventModifierFlagCommand,
            ),
            (keys.shift, NSEventModifierFlags::NSEventModifierFlagShift),
            (keys.option, NSEventModifierFlags::NSEventModifierFlagOption),
            (
                keys.control,
                NSEventModifierFlags::NSEventModifierFlagControl,
            ),
        ] {
            if on {
                mask |= flag;
            }
        }
        mask
    }

    /// An item's key equivalent, read back from AppKit.
    unsafe fn keys_of(item: &NSMenuItem) -> MenuKeys {
        // SAFETY: reading an item's key equivalent on the main thread.
        let (key, flags) = unsafe { (item.keyEquivalent(), item.keyEquivalentModifierMask()) };
        let held = held(flags);
        MenuKeys {
            key: key.to_string(),
            command: held.command,
            shift: held.shift,
            option: held.option,
            control: held.control,
        }
    }

    fn selector(action: SystemAction) -> Sel {
        match action {
            SystemAction::Hide => sel!(hide:),
            SystemAction::HideOthers => sel!(hideOtherApplications:),
            SystemAction::ShowAll => sel!(unhideAllApplications:),
            SystemAction::Quit => sel!(terminate:),
            SystemAction::Minimize => sel!(performMiniaturize:),
            SystemAction::Zoom => sel!(performZoom:),
            SystemAction::FullScreen => sel!(toggleFullScreen:),
            SystemAction::BringAllToFront => sel!(arrangeInFront:),
        }
    }

    fn item(
        mtm: MainThreadMarker,
        title: &str,
        action: Option<Sel>,
        keys: Option<&MenuKeys>,
    ) -> Retained<NSMenuItem> {
        let key = NSString::from_str(keys.map_or("", |keys| keys.key.as_str()));
        // SAFETY: a fresh item with a title, an action AppKit or `Target`
        // answers, and a key equivalent, on the main thread.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(title),
                action,
                &key,
            )
        };
        if let Some(keys) = keys {
            item.setKeyEquivalentModifierMask(mask(keys));
        }
        item
    }

    pub(super) fn install(menus: &[Menu], on_click: OnClick) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let target = Target::new(mtm, on_click);
        // One window: no Show Tab Bar in View or tab commands in Window,
        // which macOS adds to those menus unless told not to.
        NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
        let app = NSApplication::sharedApplication(mtm);
        let bar = NSMenu::new(mtm);
        for menu in menus {
            // SAFETY: a fresh menu with a title.
            let submenu =
                unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(&menu.title)) };
            add_entries(mtm, &submenu, &menu.entries, &target);
            let holder = NSMenuItem::new(mtm);
            holder.setSubmenu(Some(&submenu));
            bar.addItem(&holder);
            // SAFETY: telling AppKit which menus are the window and help menus.
            match menu.role {
                MenuRole::Window => unsafe { app.setWindowsMenu(Some(&submenu)) },
                MenuRole::Help => unsafe { app.setHelpMenu(Some(&submenu)) },
                MenuRole::App | MenuRole::Other => {}
            }
        }
        app.setMainMenu(Some(&bar));
        TARGET.with_borrow_mut(|kept| *kept = Some(target));
    }

    /// Adds `entries` to `menu`, the app's commands pointing at `target`.
    fn add_entries(
        mtm: MainThreadMarker,
        menu: &NSMenu,
        entries: &[MenuEntry],
        target: &Retained<Target>,
    ) {
        let app = NSApplication::sharedApplication(mtm);
        for entry in entries {
            match entry {
                MenuEntry::Command { title, keys, tag } => {
                    let item = item(mtm, title, Some(sel!(commandChosen:)), keys.as_ref());
                    // SAFETY: `target` answers `commandChosen:` and is kept
                    // alive in `TARGET`.
                    unsafe {
                        item.setTarget(Some(target));
                        item.setTag(*tag as isize);
                    }
                    menu.addItem(&item);
                }
                MenuEntry::System {
                    title,
                    keys,
                    action,
                } => {
                    menu.addItem(&item(mtm, title, Some(selector(*action)), keys.as_ref()));
                }
                MenuEntry::Services => {
                    // SAFETY: a fresh menu, handed to AppKit to fill.
                    let services = unsafe {
                        let services =
                            NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Services"));
                        app.setServicesMenu(Some(&services));
                        services
                    };
                    let holder = item(mtm, "Services", None, None);
                    holder.setSubmenu(Some(&services));
                    menu.addItem(&holder);
                }
                MenuEntry::Submenu { title, entries } => {
                    // SAFETY: a fresh menu with a title.
                    let inner =
                        unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title)) };
                    add_entries(mtm, &inner, entries, target);
                    let holder = item(mtm, title, None, None);
                    holder.setSubmenu(Some(&inner));
                    menu.addItem(&holder);
                }
                MenuEntry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            }
        }
    }

    /// The submenu called `title` inside one of the menu bar's menus.
    fn submenu_titled(mtm: MainThreadMarker, title: &str) -> Option<Retained<NSMenu>> {
        let title = NSString::from_str(title);
        // SAFETY: reading menus and their items on the main thread.
        unsafe {
            let bar = NSApplication::sharedApplication(mtm).mainMenu()?;
            for at in 0..bar.numberOfItems() {
                let Some(menu) = bar.itemAtIndex(at).and_then(|holder| holder.submenu()) else {
                    continue;
                };
                if let Some(inner) = menu.itemWithTitle(&title).and_then(|item| item.submenu()) {
                    return Some(inner);
                }
            }
        }
        None
    }

    pub(super) fn fill(title: &str, entries: &[MenuEntry]) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(menu) = submenu_titled(mtm, title) else {
            return;
        };
        TARGET.with_borrow(|target| {
            if let Some(target) = target {
                // SAFETY: emptying a menu of the app's own, on the main thread.
                unsafe { menu.removeAllItems() };
                add_entries(mtm, &menu, entries, target);
            }
        });
    }

    pub(super) fn set_state(state: &MenuState) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        TARGET.with_borrow(|target| {
            if let Some(target) = target {
                *target.ivars().disabled.borrow_mut() = state.disabled.iter().copied().collect();
                *target.ivars().checked.borrow_mut() = state.checked.iter().copied().collect();
            }
        });
    }

    /// Item `item` of menu `menu` as AppKit would show it now: enabled,
    /// ticked. Asks the item's target, as AppKit does before a menu opens.
    pub(super) fn item_state(menu: usize, item: usize) -> Option<(bool, bool)> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: reading menus and an item that exists, on the main thread.
        unsafe {
            let bar = app.mainMenu()?;
            let submenu = bar.itemAtIndex(isize::try_from(menu).ok()?)?.submenu()?;
            submenu.update();
            let item = submenu.itemAtIndex(isize::try_from(item).ok()?)?;
            Some((item.isEnabled(), item.state() == NSControlStateValueOn))
        }
    }

    /// The titles of the items in the submenu called `title`.
    pub(super) fn submenu_items(title: &str) -> Option<Vec<String>> {
        let mtm = MainThreadMarker::new()?;
        let menu = submenu_titled(mtm, title)?;
        // SAFETY: reading a menu's items on the main thread.
        unsafe {
            Some(
                (0..menu.numberOfItems())
                    .filter_map(|at| menu.itemAtIndex(at))
                    .map(|item| item.title().to_string())
                    .collect(),
            )
        }
    }

    /// Chooses item `item` of the submenu called `title`, as a click would.
    pub(super) fn choose_in(title: &str, item: usize) -> Option<()> {
        let mtm = MainThreadMarker::new()?;
        let menu = submenu_titled(mtm, title)?;
        let index = isize::try_from(item).ok()?;
        // SAFETY: choosing an item that exists, on the main thread.
        unsafe {
            if index >= menu.numberOfItems() {
                return None;
            }
            menu.performActionForItemAtIndex(index);
        }
        Some(())
    }

    pub(super) fn choose(menu: usize, item: usize) -> Option<()> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: reading menus and choosing an item that exists, on the
        // main thread.
        unsafe {
            let bar = app.mainMenu()?;
            let submenu = bar.itemAtIndex(isize::try_from(menu).ok()?)?.submenu()?;
            let index = isize::try_from(item).ok()?;
            if index >= submenu.numberOfItems() {
                return None;
            }
            submenu.performActionForItemAtIndex(index);
        }
        Some(())
    }

    /// What the menu bar up now does with a press of `key` with `held`,
    /// sent to the app as macOS would send it: how many presses it handed
    /// on to the window. What the `menu_bar` test checks.
    pub(crate) fn press(key: &str, held: Held) -> Option<usize> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        let keys = MenuKeys {
            key: key.to_owned(),
            command: held.command,
            shift: held.shift,
            option: held.option,
            control: held.control,
        };
        let characters = NSString::from_str(key);
        // SAFETY: a key-down event built from plain values, on the main thread.
        let event = unsafe {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown,
                objc2_foundation::NSPoint::new(0.0, 0.0),
                mask(&keys),
                0.0,
                0,
                None,
                &characters,
                &characters,
                false,
                0,
            )
        }?;
        let before = FORWARDED.with(std::cell::Cell::get);
        // Through the queue, as the run loop does: taking the event from the
        // queue is what makes it the app's current event.
        app.postEvent_atStart(&event, true);
        // SAFETY: taking the event just posted and handing it to the app, on
        // the main thread.
        unsafe {
            let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                objc2_app_kit::NSEventMask::Any,
                Some(&objc2_foundation::NSDate::distantPast()),
                objc2_foundation::NSDefaultRunLoopMode,
                true,
            )?;
            app.sendEvent(&event);
        }
        Some(FORWARDED.with(std::cell::Cell::get) - before)
    }
}

/// Whether item `item` of menu `menu` (counted from 0, the app menu first)
/// of the menu bar up now is enabled and ticked, as AppKit would show it.
/// `None` off the main thread or when there is no such item.
pub fn menu_item_state(menu: usize, item: usize) -> Option<(bool, bool)> {
    system::item_state(menu, item)
}

/// The titles in the submenu called `title` of the menu bar up now: what
/// the `menu_bar` test checks. `None` off the main thread or when there is
/// no such submenu.
pub fn submenu_items(title: &str) -> Option<Vec<String>> {
    system::submenu_items(title)
}

/// Chooses item `item` of the submenu called `title`, as a click would.
pub fn submenu_choose(title: &str, item: usize) -> Option<()> {
    system::choose_in(title, item)
}

/// Chooses item `item` of menu `menu` (counted from 0, the app menu first)
/// of the menu bar up now, as a click would: what the `menu_bar` test
/// checks. `None` off the main thread or when there is no such item.
pub fn menu_choose(menu: usize, item: usize) -> Option<()> {
    system::choose(menu, item)
}

/// Sends the app a press of `key` (as AppKit names it) with the modifiers
/// given, as macOS would, and says how many presses the menus handed on to
/// the window. `None` off the main thread.
pub fn menu_press(
    key: &str,
    command: bool,
    shift: bool,
    option: bool,
    control: bool,
) -> Option<usize> {
    system::press(
        key,
        Held {
            command,
            shift,
            option,
            control,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{MenuEntry, MenuRole};

    fn keys(key: &str, command: bool, shift: bool, option: bool, control: bool) -> MenuKeys {
        MenuKeys {
            key: key.into(),
            command,
            shift,
            option,
            control,
        }
    }

    const CMD: Held = Held {
        command: true,
        shift: false,
        option: false,
        control: false,
    };

    #[test]
    fn a_press_is_the_key_equivalent_only_with_exactly_its_modifiers() {
        let quit = keys("q", true, false, false, false);
        assert!(is_press_of(&quit, "q", CMD));
        assert!(!is_press_of(&quit, "q", Held::default()), "q alone types q");
        assert!(
            !is_press_of(&quit, "q", Held { shift: true, ..CMD }),
            "⇧⌘Q is not ⌘Q"
        );
        assert!(!is_press_of(&quit, "w", CMD));

        let hide_others = keys("h", true, false, true, false);
        assert!(is_press_of(
            &hide_others,
            "h",
            Held {
                option: true,
                ..CMD
            }
        ));
        assert!(!is_press_of(&hide_others, "h", CMD));

        let shifted = keys("c", true, true, false, false);
        assert!(
            is_press_of(&shifted, "C", Held { shift: true, ..CMD }),
            "with ⇧ AppKit reports the capital"
        );
        assert!(
            !is_press_of(&keys("", false, false, false, false), "", Held::default()),
            "an item without a key never matches"
        );
        assert!(
            is_press_of(&keys("\u{8}", true, false, false, false), "\u{7f}", CMD),
            "⌘⌫: the key types DEL, the menu writes backspace"
        );
    }

    #[test]
    fn the_recorder_keeps_the_menus_and_clicks_through() {
        let bar = RecordingMenuBar::default();
        assert!(!bar.click(3), "nothing to click before the menus are up");

        let clicked = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&clicked);
        let menus = vec![Menu {
            title: "File".into(),
            role: MenuRole::Other,
            entries: vec![MenuEntry::Separator],
        }];
        bar.install(&menus, Box::new(move |tag| seen.lock().unwrap().push(tag)));
        bar.set_state(&MenuState {
            disabled: vec![3],
            checked: vec![],
        });

        assert!(bar.click(3));
        assert_eq!(*clicked.lock().unwrap(), [3]);
        assert_eq!(*bar.installed.lock().unwrap(), [menus]);
        assert_eq!(bar.states.lock().unwrap()[0].disabled, [3]);
    }

    #[test]
    fn the_real_menu_bar_touches_nothing_off_the_main_thread() {
        // Tests run on worker threads: nothing is put on the user's menu bar.
        SystemMenuBar.install(&[], Box::new(|_| {}));
        SystemMenuBar.set_state(&MenuState::default());
        assert_eq!(menu_press("q", true, false, false, false), None);
    }
}
