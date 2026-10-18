use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::{StreamExt, stream};
use internal::dto;

use super::mapper;
use crate::handler::help::Tab;
use crate::keymap::Shortcut;

/// Puts the menus on the Mac's menu bar and greys out or ticks commands.
pub trait Service: Send + Sync {
    /// Puts `menus` up; a click on a command calls `on_click` with its tag.
    fn install(&self, menus: &[dto::Menu], on_click: Box<dyn Fn(u32) + Send + Sync>);
    fn set_state(&self, state: &dto::MenuState);
    /// Puts `entries` in the submenu called `title`, in place of what it held.
    fn fill(&self, title: &str, entries: &[dto::MenuEntry]);
}

/// What a menu command does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Exactly what its key press does.
    Key(Shortcut),
    /// Help, open on that tab.
    Help(Tab),
    /// The page for reporting a problem.
    ReportProblem,
    ImportPhotos,
    Mirror,
    /// Calculate Folder Sizes in the active pane.
    ToggleFolderSizes,
    /// Select, in the active pane, what the other pane's folder lacks or has.
    SelectAgainst(crate::handler::explorer::Against),
    /// Reads the selection's files and compares them with the other pane's
    /// of the same name, byte for byte.
    CheckContents,
    /// Groups the active pane's rows by this (View › Group by …).
    GroupBy(internal::dto::GroupBy),
    /// Fetches the selected phone file to edit in its Mac app.
    EditOnMac,
    /// List or grid, in the active pane.
    ViewAs(dto::ViewMode),
    /// Go to the recent folder at this place in Go › Recent Folders.
    Recent(usize),
    /// Forget the recent folders.
    ClearRecent,
    /// The Mac's share menu for the active pane's selection.
    Share,
}

/// What decides which commands can be used now, read from the app after
/// each change. Nothing in it grows with the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Settings, Help, Get Info, the import or the mirror dialog is up.
    pub dialog_open: bool,
    /// The active pane shows a folder.
    pub pane_open: bool,
    /// How many items are selected in the active pane.
    pub selected: usize,
    /// Something was copied or cut.
    pub can_paste: bool,
    pub phone_open: bool,
    /// The active pane's settings.
    pub show_hidden: bool,
    pub folder_sizes: bool,
    /// What the active pane's rows are grouped by.
    pub group_by: internal::dto::GroupBy,
    pub view: dto::ViewMode,
    /// The active pane's tiles are as big, or as small, as they go.
    pub tiles_biggest: bool,
    pub tiles_smallest: bool,
    /// The latest change, in either pane, can be undone.
    pub can_undo: bool,
    /// The active pane has folders to go back, or forward, to.
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// What Go › Recent Folders lists, newest first: at most a few dozen.
    pub recent: Vec<String>,
    /// Both panes show a listed folder, so one can be compared with the other.
    pub can_compare: bool,
    /// The phone pane is active with one file selected.
    pub can_edit: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// The command with this tag was clicked.
    Chosen(u32),
}

/// The Mac menu bar: File, Edit, View, Go, Window and Help, with each
/// command's shortcut, greyed out when it can't be used.
pub struct Handler {
    service: Arc<dyn Service>,
    menus: Vec<dto::Menu>,
    commands: Vec<Command>,
    /// The window has been in front: the app has finished starting.
    ready: bool,
    installed: bool,
    /// What the menus were last told, so they are told only of changes.
    shown: Option<dto::MenuState>,
    /// What Go › Recent Folders was last filled with.
    recent_shown: Vec<String>,
    clicks: Clicks,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        let (menus, commands) = mapper::menus();
        Self {
            service,
            menus,
            commands,
            ready: false,
            installed: false,
            shown: None,
            recent_shown: Vec::new(),
            clicks: Clicks::new(),
        }
    }

    /// The command behind a tag: one of the menus' own, or a recent folder.
    pub fn command(&self, tag: u32) -> Option<Command> {
        match tag.checked_sub(mapper::RECENT_TAG) {
            Some(at) => Some(Command::Recent(at as usize)),
            None => self.commands.get(tag as usize).cloned(),
        }
    }

    /// Whether the menus have been put up.
    #[cfg(test)]
    pub fn is_installed(&self) -> bool {
        self.installed
    }

    /// The tag of `command`, as a click would send it.
    #[cfg(test)]
    pub fn tag_of(&self, command: &Command) -> Option<u32> {
        self.commands
            .iter()
            .position(|known| known == command)
            .map(|tag| tag as u32)
    }

    /// The window came to the front. Only then are the menus put up: macOS
    /// puts a default menu bar up when the app has finished starting, which
    /// would replace menus put up before (it did), and the menu bar is only
    /// shown while the app is in front anyway.
    pub fn window_focused(&mut self) {
        self.ready = true;
    }

    /// Puts the menus up the first time it can (`window_focused`), then
    /// greys out and ticks what `state` says, only when that changed.
    pub fn sync(&mut self, state: &State) {
        if !self.ready {
            return;
        }
        if !self.installed {
            self.installed = true;
            let sender = self.clicks.sender.clone();
            let menus = mapper::with_recent(&self.menus, &self.commands, &state.recent);
            self.service.install(
                &menus,
                Box::new(move |tag| {
                    let _ = sender.unbounded_send(tag);
                }),
            );
            self.recent_shown = state.recent.clone();
        } else if self.recent_shown != state.recent {
            self.service.fill(
                mapper::RECENT_TITLE,
                &mapper::recent_entries(&self.commands, &state.recent),
            );
            self.recent_shown = state.recent.clone();
        }
        let now = mapper::to_menu_state(&self.commands, state);
        if self.shown.as_ref() != Some(&now) {
            self.service.set_state(&now);
            self.shown = Some(now);
        }
    }

    /// The clicks on commands, as messages.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.clicks.clone(), Clicks::stream)
    }
}

/// Clicks arrive from AppKit through a channel; the subscription reads it.
/// One reader for the handler's life: iced keeps a subscription whose data
/// hashes the same running, and starts it once.
#[derive(Clone)]
struct Clicks {
    sender: mpsc::UnboundedSender<u32>,
    receiver: Arc<Mutex<Option<mpsc::UnboundedReceiver<u32>>>>,
}

impl Clicks {
    fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            sender,
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }

    fn stream(&self) -> stream::BoxStream<'static, Message> {
        match self.receiver.lock().ok().and_then(|mut taken| taken.take()) {
            Some(receiver) => receiver.map(Message::Chosen).boxed(),
            None => stream::empty().boxed(),
        }
    }
}

impl Hash for Clicks {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.receiver).hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::FakeMenuBar;

    fn state() -> State {
        State {
            dialog_open: false,
            pane_open: true,
            selected: 0,
            can_paste: false,
            phone_open: false,
            show_hidden: false,
            folder_sizes: false,
            group_by: internal::dto::GroupBy::None,
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
    fn recent_folders_fill_their_submenu_when_they_change_and_click_through() {
        let service = Arc::new(FakeMenuBar::default());
        let mut handler = Handler::new(service.clone());
        handler.window_focused();
        let first = State {
            recent: vec!["Desktop".into()],
            ..state()
        };
        handler.sync(&first);
        let installed = service.installed.lock().unwrap()[0].clone();
        let go = installed.iter().find(|menu| menu.title == "Go").unwrap();
        let Some(dto::MenuEntry::Submenu { title, entries }) = go.entries.last() else {
            panic!("Recent Folders is last in Go");
        };
        assert_eq!(title, mapper::RECENT_TITLE);
        assert!(matches!(
            &entries[0],
            dto::MenuEntry::Command { title, tag, .. }
                if title == "Desktop" && *tag == mapper::RECENT_TAG
        ));

        handler.sync(&first);
        assert!(service.filled.lock().unwrap().is_empty(), "no change");
        handler.sync(&State {
            recent: vec!["Camera — Pixel 9".into(), "Desktop".into()],
            ..state()
        });
        let filled = service.filled.lock().unwrap().clone();
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0].0, mapper::RECENT_TITLE);
        assert!(matches!(
            &filled[0].1[1],
            dto::MenuEntry::Command { title, tag, .. }
                if title == "Desktop" && *tag == mapper::RECENT_TAG + 1
        ));

        assert_eq!(
            handler.command(mapper::RECENT_TAG + 1),
            Some(Command::Recent(1))
        );
        let clear = handler.tag_of(&Command::ClearRecent).unwrap();
        assert_eq!(handler.command(clear), Some(Command::ClearRecent));
    }

    #[test]
    fn the_menus_go_up_once_and_are_told_only_of_changes() {
        let service = Arc::new(FakeMenuBar::default());
        let mut handler = Handler::new(service.clone());
        handler.sync(&state());
        assert!(
            service.installed.lock().unwrap().is_empty()
                && service.states.lock().unwrap().is_empty(),
            "nothing before the window has been in front: macOS would replace it"
        );

        handler.window_focused();
        handler.sync(&state());
        handler.sync(&state());
        handler.sync(&State {
            selected: 1,
            ..state()
        });

        assert_eq!(service.installed.lock().unwrap().len(), 1);
        assert_eq!(
            service.installed.lock().unwrap()[0].len(),
            7,
            "the app menu, File, Edit, View, Go, Window, Help"
        );
        assert_eq!(
            service.states.lock().unwrap().len(),
            2,
            "the same state twice is told once"
        );
    }

    #[tokio::test]
    async fn a_click_arrives_as_the_message_for_its_command() {
        let service = Arc::new(FakeMenuBar::default());
        let mut handler = Handler::new(service.clone());
        handler.window_focused();
        handler.sync(&state());
        let copy = handler
            .commands
            .iter()
            .position(|command| *command == Command::Key(Shortcut::Copy))
            .unwrap() as u32;

        assert!(service.click(copy));
        let mut clicks = handler.clicks.stream();
        let Some(Message::Chosen(tag)) = clicks.next().await else {
            panic!("the click came through");
        };

        assert_eq!(tag, copy);
        assert_eq!(handler.command(tag), Some(Command::Key(Shortcut::Copy)));
        assert_eq!(handler.command(9_999), None);
        assert!(
            handler.clicks.stream().next().await.is_none(),
            "one reader: a second stream is empty"
        );
    }

    #[test]
    fn the_subscription_is_the_same_one_from_one_update_to_the_next() {
        let handler = Handler::new(Arc::new(FakeMenuBar::default()));
        let hash = |clicks: &Clicks| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            clicks.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&handler.clicks), hash(&handler.clicks.clone()));
        assert_ne!(hash(&handler.clicks), hash(&Clicks::new()));
        let _ = handler.subscription();
    }
}
