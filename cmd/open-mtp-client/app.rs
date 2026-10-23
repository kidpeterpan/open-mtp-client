//! Root of the UI: routes messages to the handler that owns them and lays
//! the handlers' views out into one window: a sidebar plus a device pane
//! and, optionally, a Mac pane beside it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard::{self, Modifiers};
use iced::widget::{
    button, center, column, container, mouse_area, opaque, row, space, stack, text,
};
use iced::{Border, Color, Element, Length, Subscription, Task, Theme, theme, window};

use internal::dto;
use internal::service::local;

use crate::handler::drop_spot::{Origin, SidebarDrop, Spot};
use crate::handler::{
    crash, device, explorer, guard, help, import, info, menubar, mirror, places, preview, search,
    settings, transfer,
};
// Free Up Space; `space` is iced's widget here.
use crate::handler::space as freeing;
use crate::i18n::{english_only, key, t, tf};
use crate::keymap::{self, Shortcut};
use crate::placement;
use crate::wire::Handlers;

const SIDEBAR_WIDTH: f32 = 260.0;
/// How often Places looks for disks plugged in or ejected.
const PLACES_EVERY: Duration = Duration::from_secs(5);
/// How often a lasso held past a list's edge scrolls it: about a frame.
const LASSO_BEAT: Duration = Duration::from_millis(16);
/// How often a drag held over a folder looks at the time: well inside
/// `explorer::SPRING_DELAY`, so the folder opens close to when it should.
const SPRING_BEAT: Duration = Duration::from_millis(100);
/// The Mac has a single volume; its id is ignored by the local service.
const MAC_VOLUME: u64 = 0;
const DOWNLOAD_ACTION: &str = key("label.download");
const UPLOAD_ACTION: &str = key("label.upload");
/// Finder sends one drop event per file; files arriving this close together
/// are one drop.
const DROP_SETTLE: Duration = Duration::from_millis(150);
/// A phone reports each changed file separately; changes this close
/// together cause one refresh.
const CHANGE_SETTLE: Duration = Duration::from_millis(500);
/// Dragging or resizing the window reports every step; its place is kept
/// this long after the last one, so a drag writes the settings once.
const WINDOW_SETTLE: Duration = Duration::from_millis(500);

/// What the user chose when quitting with a transfer under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    /// Stop what runs where it can carry on next time, then quit.
    StopAndQuit,
    /// Don't quit.
    KeepGoing,
    /// While stopping: quit at once (a phone that no longer answers).
    QuitNow,
}

#[derive(Clone, Copy)]
enum Leaving {
    ToSwitch,
    ToOpen,
}

/// Where a quit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quitting {
    /// Asking whether to stop the transfer under way.
    Asking,
    /// Stopping it; the app ends once nothing runs.
    Stopping,
}

#[derive(Debug, Clone)]
// A finished transfer's summary makes `Transfer` the big variant; it comes
// once per transfer, so boxing it would buy nothing but noise at every use.
#[allow(clippy::large_enum_variant)]
pub enum Message {
    Device(device::Message),
    DevicePane(explorer::Message),
    LocalPane(explorer::Message),
    Transfer(transfer::Message),
    Settings(settings::Message),
    Help(help::Message),
    Search(search::Message),
    Import(import::Message),
    Space(freeing::Message),
    Mirror(mirror::Message),
    Preview(preview::Message),
    /// The banner about the app's own failures.
    Crash(crash::Message),
    Places(places::Message),
    /// The Get Info dialog (⌘I).
    Info(info::Message),
    /// A command chosen from the Mac's menu bar.
    Menu(menubar::Message),
    /// macOS switched between light and dark.
    SystemTheme(theme::Mode),
    /// The mouse button came up somewhere in the window: ends a press on a row,
    /// and drops a drag.
    PointerReleased,
    /// A beat of the clock while a lasso is held past a list's edge.
    LassoTick,
    /// A beat of the clock while a drag is held over a folder: it opens once
    /// held long enough.
    SpringTick(Instant),
    /// The window's close button (it doesn't end the app by itself: a
    /// running transfer is asked about first).
    CloseRequested,
    /// The answer to "a transfer is under way" on quitting.
    Quit(QuitChoice),
    /// The window came to the front (`true`) or went behind another app's:
    /// a transfer ending then is notified.
    Focused(bool),
    /// The window was moved or resized (or went full screen).
    WindowChanged(window::Id),
    /// No window change for a moment since change number `n`.
    WindowSettled(u64),
    /// Where the window is now, read from it once it settled.
    WindowPlaced {
        mode: window::Mode,
        position: Option<iced::Point>,
        size: iced::Size,
    },
    /// Keys no widget used, e.g. not typed into a name field.
    Keyboard(keyboard::Event),
    /// The mouse's back (`false`) or forward (`true`) side button.
    SideButton(bool),
    /// The pointer is at `x` in the window while a column is resized.
    ColumnResize(f32),
    /// Files dragged from Finder over or onto the window.
    Drop(FileDrop),
    /// The files of one drop have all arrived.
    DropSettled,
    /// The phone's files changed a moment ago; time to refresh.
    PhoneChangesSettled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDrop {
    Hovered,
    Left,
    Dropped(PathBuf),
}

/// One of the two file panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Mac,
    Phone,
}

/// What ⌘C or ⌘X remembered: selections from one pane, e.g. several folders
/// added with ⌘⇧C.
#[derive(Debug, Clone)]
struct Clipboard {
    side: Side,
    selections: Vec<explorer::Selection>,
    /// Cut with ⌘X (phone files only): a paste inside the phone moves them
    /// instead of copying, and they are drawn dim until then.
    cut: bool,
}

pub struct App {
    device: device::Handler,
    device_pane: explorer::Handler,
    local_pane: explorer::Handler,
    transfer: transfer::Handler,
    settings: settings::Handler,
    help: help::Handler,
    /// Searching the whole phone; it takes the phone pane's place when open.
    search: search::Handler,
    /// The import dialog, over the window while open.
    import: import::Handler,
    space: freeing::Handler,
    /// The mirror dialog, over the window while open.
    mirror: mirror::Handler,
    /// Quick Look (Space), and the banner of a phone fetch for it.
    preview: preview::Handler,
    /// What to say when the app's own code failed (a report was written).
    crash: crash::Handler,
    /// The sidebar's places and pinned folders.
    places: places::Handler,
    /// Get Info (⌘I).
    info: info::Handler,
    /// The Mac's menu bar.
    menu: menubar::Handler,
    languages: Arc<dyn local::Languages>,
    language_applied: Option<dto::LanguageChoice>,
    /// The Mac home, which the import dialog shows paths from.
    home: String,
    /// The Mac's appearance, for the "Match the Mac" theme.
    system_theme: theme::Mode,
    /// The pane keyboard shortcuts go to; outlined in the view.
    active: Side,
    /// What ⌘C or ⌘X remembered.
    clipboard: Option<Clipboard>,
    /// Files are being dragged over the window.
    drop_hovering: bool,
    /// A quit asked about, or under way while a transfer stops.
    quitting: Option<Quitting>,
    /// Transfers kept from last time for phones not open now: offered when
    /// their phone opens, kept in the file until then.
    saved_elsewhere: Vec<dto::SavedTransfer>,
    /// The transfer queue's `changes()` when it was last kept: nothing to
    /// compare until it moves on.
    transfers_kept_at: Option<u64>,
    /// The sidebar spot a drag from a pane is over, and since when (held
    /// long enough, it opens in its pane).
    sidebar_over: Option<(Spot, Instant)>,
    /// The spot held over has opened already: once per spot.
    sidebar_sprung: bool,
    /// The keys held down: ⌘ makes a drop on the other pane a move.
    modifiers: Modifiers,
    /// Files of a drop still arriving.
    dropped: Vec<PathBuf>,
    /// A refresh for phone-side changes is already scheduled.
    refresh_pending: bool,
    /// The key of the phone last open, to tell another phone from the same
    /// one opened again.
    phone: Option<String>,
    /// The window, once it has reported a change, and how many changes so
    /// far: only the last change of a drag is kept.
    window: Option<window::Id>,
    window_changes: u64,
}

impl App {
    /// Opens the Mac pane at home and starts connecting right away, so a
    /// phone that is already plugged in shows up without a click.
    pub fn new(handlers: Handlers) -> (Self, Task<Message>) {
        let mut app = Self {
            device: handlers.device,
            device_pane: handlers.device_pane,
            local_pane: handlers.local_pane,
            transfer: handlers.transfer,
            settings: handlers.settings,
            help: handlers.help,
            search: handlers.search,
            import: handlers.import,
            space: handlers.space,
            mirror: handlers.mirror,
            preview: handlers.preview,
            crash: handlers.crash,
            places: handlers.places,
            info: handlers.info,
            menu: handlers.menubar,
            languages: handlers.languages,
            language_applied: None,
            home: handlers.local_home.clone(),
            system_theme: theme::Mode::None,
            active: Side::Mac,
            clipboard: None,
            drop_hovering: false,
            quitting: None,
            saved_elsewhere: Vec::new(),
            transfers_kept_at: None,
            sidebar_over: None,
            sidebar_sprung: false,
            modifiers: Modifiers::empty(),
            dropped: Vec::new(),
            refresh_pending: false,
            phone: None,
            window: None,
            window_changes: 0,
        };
        let _ = app.apply_settings();
        let connect = app.device.connect().map(Message::Device);
        let watch = app.device.watch().map(Message::Device);
        // Where the Mac pane was left, or Home.
        let start = app
            .settings
            .folders()
            .mac
            .clone()
            .unwrap_or_else(|| handlers.local_home.clone());
        let home = app
            .local_pane
            .open_with_fallback(
                explorer::Volume {
                    id: MAC_VOLUME,
                    name: english_only("Mac").to_owned(),
                    home: Some(handlers.local_home.clone()),
                    deletes_permanently: false,
                    transfer_action: None,
                    shows_in_finder: true,
                    pastes_within: true,
                    phone_tools: false,
                    hides_app_folders: false,
                },
                start,
                handlers.local_home,
            )
            .map(Message::LocalPane);
        let appearance = iced::system::theme().map(Message::SystemTheme);
        let places = app.places.refresh().map(Message::Places);
        app.greet(help::whats_new::current());
        app.offer_saved_transfers();
        // Files being edited when the app last ended come back.
        let edits = app.transfer.load_edits().map(Message::Transfer);
        (
            app,
            Task::batch([connect, watch, home, appearance, places, edits]),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let released = matches!(message, Message::PointerReleased);
        let task = self.route(message);
        // One atomic load: a panic in a task or a worker thread leaves the
        // app running without the work it was doing, which is said.
        self.crash.check();
        self.transfer.set_link(self.device.link());
        let sprung = self.sync_drags(released);
        if self.sidebar_drop().is_none() {
            self.sidebar_over = None;
        }
        self.keep_folders();
        self.keep_transfers();
        let open = self.device.open_key();
        if self.transfer.open_phone() != open.as_deref() {
            self.transfer.set_open_phone(open);
        }
        self.menu.sync(&self.menu_state());
        Task::batch([task, sprung])
    }

    /// The transfers not finished last time: the Mac's own are offered at
    /// once; a phone's wait until that phone opens (`offer_phone_transfers`).
    fn offer_saved_transfers(&mut self) {
        let (mac, phones): (Vec<_>, Vec<_>) = self
            .settings
            .saved_transfers()
            .iter()
            .cloned()
            .partition(|saved| saved.phone.is_none());
        self.transfer
            .offer(mac.into_iter().map(|saved| saved.job).collect());
        self.saved_elsewhere = phones;
    }

    /// `key`'s phone has opened: its transfers from last time are offered.
    fn offer_phone_transfers(&mut self, key: &str) {
        let (theirs, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.saved_elsewhere)
            .into_iter()
            .partition(|saved| saved.phone.as_deref() == Some(key));
        self.saved_elsewhere = others;
        self.transfer
            .offer(theirs.into_iter().map(|saved| saved.job).collect());
    }

    /// Keeps what the queue still has to do, so a quit, a crash or a restart
    /// doesn't lose it: written only when it changes.
    fn keep_transfers(&mut self) {
        // Every change to the queue, or to what is kept for other phones,
        // goes through the transfer handler, which counts it.
        let changes = self.transfer.changes();
        if self.transfers_kept_at == Some(changes) {
            return;
        }
        self.transfers_kept_at = Some(changes);
        let phone = self.phone.clone();
        let mut now = self.saved_elsewhere.clone();
        now.extend(
            self.transfer
                .saved()
                .into_iter()
                .map(|(uses_phone, job)| dto::SavedTransfer {
                    phone: if uses_phone { phone.clone() } else { None },
                    job,
                }),
        );
        self.settings.keep_transfers(now);
    }

    /// Remembers where each pane is, to open there next time: written only
    /// when a pane's folder changes.
    fn keep_folders(&mut self) {
        if let Some(path) = self.local_pane.current_path() {
            let path = path.to_owned();
            self.settings.keep_mac_folder(&path);
        }
        if let (Some(phone), Some((storage_id, path))) =
            (self.phone.clone(), self.device_pane.location())
        {
            let path = path.to_owned();
            self.settings.keep_phone_folder(&phone, storage_id, &path);
        }
    }

    /// What decides which menu commands can be used now: a handful of
    /// flags, nothing that grows with the folder.
    fn menu_state(&self) -> menubar::State {
        let side = self.target();
        let pane = self.pane_of(side);
        let settings = self.settings.current();
        menubar::State {
            dialog_open: self.dialog_is_open(),
            pane_open: pane.is_open(),
            selected: pane.selected_count(),
            can_paste: self.clipboard.is_some(),
            phone_open: self.device_pane.is_open(),
            show_hidden: match side {
                Side::Mac => settings.show_hidden_on_mac,
                Side::Phone => settings.show_hidden_on_phone,
            },
            group_by: match side {
                Side::Mac => settings.mac_group,
                Side::Phone => settings.phone_group,
            },
            folder_sizes: match side {
                Side::Mac => settings.mac_folder_sizes,
                Side::Phone => settings.phone_folder_sizes,
            },
            view: match side {
                Side::Mac => settings.mac_view,
                Side::Phone => settings.phone_view,
            },
            tiles_biggest: match side {
                Side::Mac => settings.mac_tiles.is_biggest(),
                Side::Phone => settings.phone_tiles.is_biggest(),
            },
            tiles_smallest: match side {
                Side::Mac => settings.mac_tiles.is_smallest(),
                Side::Phone => settings.phone_tiles.is_smallest(),
            },
            can_undo: {
                let latest = if self.local_pane.changed_at() >= self.device_pane.changed_at() {
                    &self.local_pane
                } else {
                    &self.device_pane
                };
                latest.can_undo()
            },
            can_go_back: pane.can_go_back(),
            can_go_forward: pane.can_go_forward(),
            can_compare: self.can_compare(),
            can_edit: self.active == Side::Phone && self.device_pane.single_file().is_some(),
            recent: self
                .recent_folders()
                .into_iter()
                .map(|(label, _)| label)
                .collect(),
        }
    }

    /// Settings, Help, Get Info, the import or the mirror dialog is up.
    fn dialog_is_open(&self) -> bool {
        self.quitting.is_some()
            || self.settings.is_open()
            || self.help.is_open()
            || self.info.is_open()
            || self.import.is_open()
            || self.mirror.is_open()
            || self.space.is_open()
    }

    /// What Go › Recent Folders lists: the open phone's folders shown lately
    /// (on storages it has now), then the Mac's, each newest first.
    fn recent_folders(&self) -> Vec<(String, Recent)> {
        let folders = self.settings.folders();
        let mut recent = Vec::new();
        if let (Some(phone), Some(name), Some((_, storages))) = (
            self.phone.as_deref(),
            self.device.open_name(),
            self.device.open_phone(),
        ) {
            for kept in folders.recent_on(phone) {
                let Some((_, storage)) = storages.iter().find(|(id, _)| *id == kept.storage_id)
                else {
                    continue;
                };
                recent.push((
                    phone_recent_label(&kept.path, storage, name),
                    Recent::Phone {
                        storage_id: kept.storage_id,
                        path: kept.path.clone(),
                    },
                ));
            }
        }
        let labels = mac_recent_labels(&folders.recent_mac, &self.home);
        recent.extend(
            labels
                .into_iter()
                .zip(&folders.recent_mac)
                .map(|(label, path)| (label, Recent::Mac(path.clone()))),
        );
        recent
    }

    /// Carries out a command chosen from the menu bar: a shortcut's command
    /// exactly as its key does, the rest through the same messages as the
    /// buttons that offer them.
    fn on_menu(&mut self, command: menubar::Command) -> Task<Message> {
        let help = |request| Message::Help(help::Message::Request(request));
        match command {
            // Chosen from a menu, Settings and Help open; their keys toggle.
            menubar::Command::Key(Shortcut::Settings) => self.update(Message::Settings(
                settings::Message::Request(settings::Request::Open),
            )),
            menubar::Command::Key(Shortcut::Help) => self.update(help(help::Request::Open)),
            menubar::Command::Key(shortcut) => self.run(shortcut),
            menubar::Command::Help(tab) => {
                let _ = self.update(help(help::Request::Open));
                self.update(help(help::Request::Show(tab)))
            }
            menubar::Command::ReportProblem => {
                self.update(help(help::Request::OpenLink(help::Link::NewIssue)))
            }
            menubar::Command::ImportPhotos => self.update(Message::Device(
                device::Message::Request(device::Request::Import),
            )),
            menubar::Command::Mirror => self.update(Message::DevicePane(
                explorer::Message::Request(explorer::Request::Mirror),
            )),
            menubar::Command::SelectAgainst(want) => {
                self.select_against(self.target(), want);
                Task::none()
            }
            menubar::Command::CheckContents => self.check_contents(self.target()),
            menubar::Command::Recent(at) => match self.recent_folders().into_iter().nth(at) {
                Some((_, Recent::Mac(path))) => self.update(Message::Places(places::Message::Go(
                    places::Target::Mac(path),
                ))),
                Some((_, Recent::Phone { storage_id, path })) => {
                    self.go_to_phone_pin(storage_id, path)
                }
                None => Task::none(),
            },
            menubar::Command::Share => {
                let side = self.target();
                self.share(side, None)
            }
            menubar::Command::EditOnMac => self.edit_on_mac(),
            menubar::Command::ClearRecent => {
                self.settings.clear_recent();
                Task::none()
            }
            menubar::Command::ToggleFolderSizes => {
                let side = self.target();
                let saved = self.change_settings(|s| match side {
                    Side::Mac => s.mac_folder_sizes = !s.mac_folder_sizes,
                    Side::Phone => s.phone_folder_sizes = !s.phone_folder_sizes,
                });
                Task::batch([saved, self.apply_settings()])
            }
            menubar::Command::GroupBy(by) => self.set_group(self.target(), by),
            menubar::Command::ViewAs(view) => {
                let request = explorer::Message::Request(explorer::Request::SetView(view));
                match self.target() {
                    Side::Mac => self.update(Message::LocalPane(request)),
                    Side::Phone => self.update(Message::DevicePane(request)),
                }
            }
        }
    }

    /// Groups the rows of the pane on `side` by `by`, and keeps it.
    fn set_group(&mut self, side: Side, by: dto::GroupBy) -> Task<Message> {
        self.change_settings(|s| match side {
            Side::Mac => s.mac_group = by,
            Side::Phone => s.phone_group = by,
        })
    }

    /// Tells each pane whether the other is dragging, so its rows say what
    /// the pointer is over and a drop can land there. Only a pane that is
    /// shown is told, and only a drag that has started.
    /// Tells each pane about the other's drag. `released`: the button just
    /// came up, so a drag that ends now was dropped (not cancelled).
    fn sync_drags(&mut self, released: bool) -> Task<Message> {
        let phone_shown = self.device_pane.is_open() && !self.search.is_open();
        let mac_shown = self.settings.current().show_mac_pane;
        let from_mac = self
            .local_pane
            .dragging()
            .filter(|_| phone_shown && mac_shown);
        let from_phone = self
            .device_pane
            .dragging()
            .filter(|_| phone_shown && mac_shown);
        let device = self
            .device_pane
            .set_incoming(from_mac, released)
            .map(Message::DevicePane);
        let local = self
            .local_pane
            .set_incoming(from_phone, released)
            .map(Message::LocalPane);
        self.device_pane.set_other_pane(mac_shown);
        self.local_pane.set_other_pane(phone_shown);
        Task::batch([device, local])
    }

    /// Quits, or, with a transfer running, paused or waiting, asks first:
    /// a quit would otherwise end it mid-request, its part never stamped
    /// for the next try.
    fn ask_to_quit(&mut self) -> Task<Message> {
        if self.quitting == Some(Quitting::Stopping) {
            return Task::none();
        }
        if !self.transfer.is_busy() {
            return iced::exit();
        }
        self.device_pane.close_menu();
        self.local_pane.close_menu();
        self.quitting = Some(Quitting::Asking);
        Task::none()
    }

    fn answer_quit(&mut self, choice: QuitChoice) -> Task<Message> {
        match (self.quitting, choice) {
            (Some(Quitting::Asking), QuitChoice::KeepGoing) => {
                self.quitting = None;
                Task::none()
            }
            (Some(Quitting::Asking), QuitChoice::StopAndQuit) => {
                self.quitting = Some(Quitting::Stopping);
                if self.transfer.stop_for_quit() {
                    Task::none()
                } else {
                    iced::exit()
                }
            }
            (Some(Quitting::Stopping), QuitChoice::QuitNow) => iced::exit(),
            _ => Task::none(),
        }
    }

    /// What a drag from a pane means for the sidebar: where it comes from
    /// and the spot it is over; `None` while nothing is dragged.
    fn sidebar_drop(&self) -> Option<SidebarDrop> {
        let from = if let Some((_, folder)) = self.local_pane.dragged_from() {
            Origin::Mac {
                folder: folder.to_owned(),
            }
        } else {
            let (storage_id, folder) = self.device_pane.dragged_from()?;
            Origin::Phone {
                storage_id,
                folder: folder.to_owned(),
            }
        };
        Some(SidebarDrop {
            over: self.sidebar_over.as_ref().map(|(spot, _)| spot.clone()),
            from,
        })
    }

    /// The pointer of a drag came onto a sidebar spot (`over`) or left it:
    /// leaving one forgets it only if it is still the one held over, since
    /// the next spot's arrival may be heard first.
    fn sidebar_hover(&mut self, spot: Spot, over: bool) {
        let held = self.sidebar_over.as_ref().map(|(spot, _)| spot);
        if over && held != Some(&spot) {
            self.sidebar_over = Some((spot, Instant::now()));
            self.sidebar_sprung = false;
        } else if !over && held == Some(&spot) {
            self.sidebar_over = None;
        }
    }

    /// A drag waits over a sidebar spot that hasn't opened yet.
    fn sidebar_spring_pending(&self) -> bool {
        self.sidebar_over.is_some() && !self.sidebar_sprung
    }

    /// A drag held over a sidebar spot long enough (or Space, `at_once`):
    /// its folder opens in its pane, once, as a folder in a pane springs
    /// open. A phone spot opens only on the storage the pane shows.
    fn spring_sidebar(&mut self, now: Instant, at_once: bool) -> Task<Message> {
        let Some((spot, since)) = self.sidebar_over.clone() else {
            return Task::none();
        };
        let held = now.saturating_duration_since(since) >= explorer::SPRING_DELAY;
        if self.sidebar_sprung || !(at_once || held) {
            return Task::none();
        }
        self.sidebar_sprung = true;
        match spot {
            Spot::Mac(path) if self.settings.current().show_mac_pane => {
                self.local_pane.spring_to(path).map(Message::LocalPane)
            }
            Spot::Phone { storage_id, path }
                if self.device_pane.volume_id() == Some(storage_id) && !self.search.is_open() =>
            {
                self.device_pane.spring_to(path).map(Message::DevicePane)
            }
            _ => Task::none(),
        }
    }

    /// A drag let go over a sidebar spot: its selection goes there, moved
    /// on the same disk or storage and copied anywhere else (⌥ and ⌘ turn
    /// that round, as in Finder). `None` when it wasn't let go over one.
    fn drop_on_sidebar(&mut self) -> Option<Task<Message>> {
        let drop = self.sidebar_drop()?;
        let spot = drop.over.clone().filter(|spot| drop.accepts(spot))?;
        self.sidebar_over = None;
        let mode = drop.mode(&spot, self.modifiers.command(), self.modifiers.alt());
        Some(match (drop.from, spot) {
            (Origin::Mac { .. }, Spot::Mac(path)) => {
                self.active = Side::Mac;
                let selection = self.local_pane.dragged()?;
                self.local_pane
                    .paste_into(mode, vec![selection], path)
                    .map(Message::LocalPane)
            }
            (Origin::Mac { .. }, Spot::Phone { storage_id, path }) => {
                self.active = Side::Phone;
                let selection = self.local_pane.dragged()?;
                self.upload_to(mode, mac_paths(&selection), storage_id, path)
            }
            (Origin::Phone { .. }, Spot::Mac(path)) => {
                self.active = Side::Mac;
                let selection = self.device_pane.dragged();
                self.download_into(mode, selection, path)
            }
            (Origin::Phone { .. }, Spot::Phone { path, .. }) => {
                self.active = Side::Phone;
                let selection = self.device_pane.dragged()?;
                self.device_pane
                    .paste_into(mode, vec![selection], path)
                    .map(Message::DevicePane)
            }
        })
    }

    /// A drag is under way in either pane (Space then opens a folder).
    fn spring_held(&self) -> bool {
        self.device_pane.dragging().is_some() || self.local_pane.dragging().is_some()
    }

    /// A drag let go over the other pane: its selection is copied there, or
    /// moved with ⌘ (as Finder does between disks), into the folder row under
    /// the pointer or else the folder shown.
    fn drop_across(&mut self) -> Task<Message> {
        let mode = if self.modifiers.command() {
            dto::Paste::Move
        } else {
            dto::Paste::Copy
        };
        if self.device_pane.dragging().is_some()
            && let Some(into) = self.local_pane.drop_into()
        {
            self.active = Side::Mac;
            let selection = self.device_pane.dragged();
            return self.download_into(mode, selection, into);
        }
        if self.local_pane.dragging().is_some()
            && let Some(into) = self.device_pane.drop_into()
            && let Some(selection) = self.local_pane.dragged()
        {
            self.active = Side::Phone;
            return self.upload_into(mode, mac_paths(&selection), into);
        }
        Task::none()
    }

    fn route(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Device(device::Message::Changed(dto::ContentChange::Files)) => {
                if self.refresh_pending {
                    return Task::none();
                }
                self.refresh_pending = true;
                // Built lazily: the timer needs the runtime the task runs on.
                guard::perform(
                    async { tokio::time::sleep(CHANGE_SETTLE).await },
                    || (),
                    |()| Message::PhoneChangesSettled,
                )
            }
            Message::PhoneChangesSettled => {
                self.refresh_pending = false;
                // A transfer's own files cause these changes; it refreshes
                // the pane itself when it finishes.
                if self.transfer.is_running() {
                    return Task::none();
                }
                self.device_pane.reload().map(Message::DevicePane)
            }
            Message::Device(device::Message::Request(device::Request::GoToPlace(path))) => self
                .update(Message::DevicePane(explorer::Message::Request(
                    explorer::Request::GoTo(path),
                ))),
            Message::Device(device::Message::Request(device::Request::Import)) => {
                self.open_import()
            }
            Message::Import(import::Message::Request(import::Request::RemoveAfter(on))) => {
                let saved = self.change_settings(|s| s.import_remove = on);
                let task = self
                    .import
                    .update(import::Message::Request(import::Request::RemoveAfter(on)))
                    .map(Message::Import);
                Task::batch([saved, task])
            }
            Message::Import(import::Message::Request(import::Request::Folders(folders))) => {
                let saved = self.change_settings(|s| s.import_folders = folders);
                let looked = self
                    .import
                    .update(import::Message::Request(import::Request::Folders(folders)))
                    .map(Message::Import);
                Task::batch([saved, looked])
            }
            Message::Import(import::Message::Request(import::Request::Import)) => {
                let request = self.import.request();
                self.import.close();
                match request {
                    Some(request) => self.transfer.import(request).map(Message::Transfer),
                    None => Task::none(),
                }
            }
            Message::Import(message) => self.import.update(message).map(Message::Import),
            Message::Device(device::Message::Request(device::Request::FreeUpSpace)) => {
                self.open_space()
            }
            Message::Space(freeing::Message::Request(freeing::Request::BiggestFiles)) => {
                let _ = self
                    .space
                    .update(freeing::Message::Request(freeing::Request::BiggestFiles));
                let opened = self.open_search();
                let ordered = self
                    .search
                    .update(search::Message::Request(search::Request::SortBy(
                        dto::SortKey::Size,
                    )))
                    .map(Message::Search);
                Task::batch([opened, ordered])
            }
            Message::Space(message) => {
                // A crash part-way counts too: some files may be gone.
                let removed = matches!(&message, freeing::Message::Removed { result: Ok(_), .. })
                    || matches!(
                        &message,
                        freeing::Message::Removed { result: Err(error), .. }
                            if error.source == dto::Error::Crashed
                    );
                let task = self.space.update(message).map(Message::Space);
                if !removed {
                    return task;
                }
                // What is on the phone changed: its storage bars and the
                // folder shown are read again.
                let refresh = self.device_pane.reload().map(Message::DevicePane);
                Task::batch([
                    task,
                    self.device.reload_storages().map(Message::Device),
                    refresh,
                ])
            }
            Message::DevicePane(explorer::Message::OpenFile(name)) => {
                let Some((storage_id, folder)) = self.device_pane.location() else {
                    return Task::none();
                };
                let folder = folder.to_owned();
                self.preview
                    .phone_open(dto::PreviewRequest {
                        storage_id,
                        folder,
                        names: vec![name],
                    })
                    .map(Message::Preview)
            }
            Message::LocalPane(explorer::Message::OpenFile(name)) => {
                let Some(folder) = self.local_pane.current_path() else {
                    return Task::none();
                };
                let path = std::path::Path::new(folder).join(name);
                self.preview
                    .mac_open(path.to_string_lossy().into_owned())
                    .map(Message::Preview)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::Mirror)) => {
                self.open_mirror()
            }
            Message::Mirror(mirror::Message::Request(mirror::Request::Start)) => {
                let request = self.mirror.request();
                self.mirror.close();
                match request {
                    Some(request) => self.transfer.mirror(request).map(Message::Transfer),
                    None => Task::none(),
                }
            }
            Message::Mirror(mirror::Message::Request(mirror::Request::Save)) => {
                if let Some(saved) = self.mirror.to_save() {
                    self.places.save_mirror(saved);
                }
                self.sync_mirror_saved();
                Task::none()
            }
            Message::Mirror(message) => {
                let task = self.mirror.update(message).map(Message::Mirror);
                self.sync_mirror_saved();
                task
            }
            Message::Places(places::Message::DropHover(spot, over))
            | Message::Device(device::Message::DropHover(spot, over)) => {
                self.sidebar_hover(spot, over);
                Task::none()
            }
            Message::Places(places::Message::OpenMirror { at, storage_id }) => {
                self.open_saved_mirror(at, storage_id)
            }
            Message::Places(places::Message::Go(places::Target::Mac(path))) => {
                self.active = Side::Mac;
                self.update(Message::LocalPane(explorer::Message::Request(
                    explorer::Request::GoTo(path),
                )))
            }
            Message::Places(places::Message::Go(places::Target::Phone { storage_id, path })) => {
                self.go_to_phone_pin(storage_id, path)
            }
            Message::Places(message) => self.places.update(message).map(Message::Places),
            Message::Info(info::Message::Request(info::Request::ShowKind(kind))) => {
                let on_phone = self.info.is_about_the_phone();
                let _ = self
                    .info
                    .update(info::Message::Request(info::Request::ShowKind(kind)));
                let Some(file_kind) = kind.file_kind() else {
                    return Task::none();
                };
                if on_phone {
                    // The phone's search, that kind, the biggest first.
                    let opened = self.open_search();
                    let kind = self
                        .search
                        .update(search::Message::Request(search::Request::Kind(Some(
                            file_kind,
                        ))))
                        .map(Message::Search);
                    let ordered = self
                        .search
                        .update(search::Message::Request(search::Request::SortBy(
                            dto::SortKey::Size,
                        )))
                        .map(Message::Search);
                    Task::batch([opened, kind, ordered])
                } else {
                    self.active = Side::Mac;
                    self.update(Message::LocalPane(explorer::Message::Request(
                        explorer::Request::FilterKind(Some(file_kind)),
                    )))
                }
            }
            Message::Info(message) => self.info.update(message).map(Message::Info),
            Message::Crash(message) => self.crash.update(message).map(Message::Crash),
            Message::Preview(message) => {
                if let Some(error) = message.device_lost() {
                    self.device.lose_device(error.clone());
                    self.close_device_pane();
                    return Task::none();
                }
                self.preview.update(message).map(Message::Preview)
            }
            Message::Device(device::Message::Request(device::Request::Open(_)))
                if let Some(why) = self.why_not_switch() =>
            {
                self.device.refuse_switch(why);
                Task::none()
            }
            Message::Device(message) => {
                // The phone too, not only the storage: two phones can have
                // storages with the same id and name.
                let before = self.device.opened();
                let task = self.device.update(message).map(Message::Device);
                let after = self.device.opened();
                if before == after {
                    return task;
                }
                if let Some((key, ..)) = &after
                    && self.phone.as_ref() != Some(key)
                {
                    // Another phone than last time: what was done on that one
                    // is not run again on this one, and what was offered for it
                    // is kept for when it comes back.
                    if let Some(last) = self.phone.clone() {
                        self.transfer.forget_phone();
                        let withdrawn = self.transfer.withdraw_offered();
                        self.saved_elsewhere
                            .extend(withdrawn.into_iter().map(|job| dto::SavedTransfer {
                                phone: Some(last.clone()),
                                job,
                            }));
                    }
                    self.phone = Some(key.clone());
                    self.offer_phone_transfers(key);
                }
                // The same phone open again: a transfer that stopped when it
                // was lost carries on (another phone ended it above).
                let resumed = if after.is_some() {
                    self.transfer.phone_back().map(Message::Transfer)
                } else {
                    Task::none()
                };
                let after = after.map(|(_, id, name)| (id, name));

                // Uploading needs somewhere on the phone to go.
                self.local_pane
                    .set_transfer_action(after.is_some().then_some(UPLOAD_ACTION));

                // A new storage was picked, or the device went away. It opens
                // where this phone's pane was left on it, or at the top.
                let start = match (&self.phone, &after) {
                    (Some(phone), Some((id, _))) => self
                        .settings
                        .folders()
                        .phone(phone, *id)
                        .unwrap_or("/")
                        .to_owned(),
                    _ => "/".to_owned(),
                };
                let follow_up = match after {
                    Some((id, name)) => self
                        .device_pane
                        .open_with_fallback(
                            explorer::Volume {
                                id,
                                name,
                                home: None,
                                deletes_permanently: true,
                                transfer_action: Some(DOWNLOAD_ACTION),
                                shows_in_finder: false,
                                pastes_within: true,
                                phone_tools: true,
                                hides_app_folders: self.device.is_android(),
                            },
                            start,
                            "/".to_owned(),
                        )
                        .map(Message::DevicePane),
                    None => {
                        self.close_device_pane();
                        Task::none()
                    }
                };
                Task::batch([task, follow_up, resumed])
            }
            Message::DevicePane(explorer::Message::DeleteKeepingCopy(request)) => {
                self.transfer.delete_keeping(request).map(Message::Transfer)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::Menu(action)))
                if action.is_app_wide() =>
            {
                self.menu_action(Side::Phone, action)
            }
            Message::LocalPane(explorer::Message::Request(explorer::Request::Menu(action)))
                if action.is_app_wide() =>
            {
                self.menu_action(Side::Mac, action)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::Search)) => {
                self.open_search()
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::Transfer)) => {
                self.active = Side::Phone;
                let selection = self.device_pane.selection();
                self.start_download(selection)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::SetShowHidden(
                on,
            ))) => {
                self.active = Side::Phone;
                self.change_settings(|s| s.show_hidden_on_phone = on)
            }
            Message::DevicePane(explorer::Message::Request(
                explorer::Request::SetFoldersFirst(on),
            ))
            | Message::LocalPane(explorer::Message::Request(explorer::Request::SetFoldersFirst(
                on,
            ))) => self.change_settings(|s| s.folders_first = on),
            Message::LocalPane(explorer::Message::Request(explorer::Request::SetShowHidden(
                on,
            ))) => {
                self.active = Side::Mac;
                self.change_settings(|s| s.show_hidden_on_mac = on)
            }
            Message::LocalPane(explorer::Message::Request(explorer::Request::SortBy(key))) => {
                self.active = Side::Mac;
                let _ = self
                    .local_pane
                    .update(explorer::Message::Request(explorer::Request::SortBy(key)));
                let order = self.local_pane.sort_order();
                self.change_settings(|s| s.mac_order = order)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::SortBy(key))) => {
                self.active = Side::Phone;
                let _ = self
                    .device_pane
                    .update(explorer::Message::Request(explorer::Request::SortBy(key)));
                let order = self.device_pane.sort_order();
                self.change_settings(|s| s.phone_order = order)
            }
            Message::LocalPane(explorer::Message::Request(explorer::Request::SetView(view))) => {
                self.active = Side::Mac;
                self.change_settings(|s| s.mac_view = view)
            }
            Message::DevicePane(explorer::Message::Request(explorer::Request::SetView(view))) => {
                self.active = Side::Phone;
                self.change_settings(|s| s.phone_view = view)
            }
            Message::DevicePane(message) => {
                if is_interaction(&message) {
                    self.active = Side::Phone;
                }
                if matches!(message, explorer::Message::ShowMenu { .. }) {
                    self.local_pane.close_menu();
                }
                if let Some(error) = message.device_lost()
                    && self.device_pane.is_about_now(&message)
                {
                    self.device.lose_device(error.clone());
                    self.close_device_pane();
                    return Task::none();
                }
                // A delete, rename, new folder or paste on the phone that went
                // through changes how full it is (a delete frees space).
                let changed = matches!(
                    message,
                    explorer::Message::Edited { result: Ok(_), .. }
                        | explorer::Message::Pasted { result: Ok(_), .. }
                );
                // What was cut has been moved, so there is nothing left to
                // paste; a paste that failed outright leaves it for another go.
                if matches!(message, explorer::Message::Pasted { result: Ok(_), .. })
                    && self
                        .clipboard
                        .as_ref()
                        .is_some_and(|clipboard| clipboard.cut)
                {
                    self.set_clipboard(None);
                }
                let space = if changed {
                    self.device.reload_storages().map(Message::Device)
                } else {
                    Task::none()
                };
                // A phone that can't copy or move by itself: do it through the Mac.
                let via_mac = message
                    .via_mac()
                    .map(|(mode, from, volume_id, path)| {
                        self.queue_through_mac(mode, from, volume_id, path)
                    })
                    .unwrap_or_else(Task::none);
                let task = self.device_pane.update(message).map(Message::DevicePane);
                Task::batch([task, space, via_mac])
            }
            Message::LocalPane(explorer::Message::Request(explorer::Request::Transfer)) => {
                self.active = Side::Mac;
                let selection = self.local_pane.selection();
                self.start_upload(selection)
            }
            Message::LocalPane(message) => {
                if is_interaction(&message) {
                    self.active = Side::Mac;
                }
                if matches!(message, explorer::Message::ShowMenu { .. }) {
                    self.device_pane.close_menu();
                }
                // What was cut has been moved, so there is nothing left to
                // paste; a paste that failed outright leaves it for another go.
                if matches!(message, explorer::Message::Pasted { result: Ok(_), .. })
                    && self
                        .clipboard
                        .as_ref()
                        .is_some_and(|clipboard| clipboard.cut)
                {
                    self.set_clipboard(None);
                }
                // A copy too big to do on the spot is queued, with progress.
                let on_mac = message
                    .via_mac()
                    .map(|(mode, from, _, path)| self.queue_on_mac(mode, from, path))
                    .unwrap_or_else(Task::none);
                let task = self.local_pane.update(message).map(Message::LocalPane);
                Task::batch([task, on_mac])
            }
            Message::Keyboard(event) => self.on_key(event),
            Message::SideButton(forward) => self.run(Shortcut::Pane(if forward {
                explorer::Request::Forward
            } else {
                explorer::Request::Back
            })),
            Message::Drop(FileDrop::Hovered) => {
                self.drop_hovering = true;
                Task::none()
            }
            Message::Drop(FileDrop::Left) => {
                self.drop_hovering = false;
                Task::none()
            }
            Message::Drop(FileDrop::Dropped(path)) => {
                self.drop_hovering = false;
                let first = self.dropped.is_empty();
                self.dropped.push(path);
                if first {
                    // Built lazily: the timer needs the runtime the task runs on.
                    guard::perform(
                        async { tokio::time::sleep(DROP_SETTLE).await },
                        || (),
                        |()| Message::DropSettled,
                    )
                } else {
                    Task::none()
                }
            }
            Message::DropSettled => {
                let sources = std::mem::take(&mut self.dropped)
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                self.active = Side::Phone;
                self.upload(sources)
            }
            Message::Transfer(message) => {
                if let Some(error) = message.device_lost() {
                    self.device.lose_device(error.clone());
                    self.close_device_pane();
                    self.local_pane.set_transfer_action(None);
                }
                let refresh = match message.finished() {
                    Some(summary) => self.show_result(summary),
                    None => Task::none(),
                };
                // A finished transfer, whole or part, has changed how full the
                // phone is; ask rather than wait for it to say.
                let space = if matches!(message, transfer::Message::Finished(_)) {
                    self.device.reload_storages().map(Message::Device)
                } else {
                    Task::none()
                };
                let task = self.transfer.update(message).map(Message::Transfer);
                // Quitting once what ran has stopped.
                if self.quitting == Some(Quitting::Stopping) && !self.transfer.is_running() {
                    return Task::batch([task, iced::exit()]);
                }
                Task::batch([task, refresh, space])
            }
            Message::CloseRequested => self.ask_to_quit(),
            Message::Quit(choice) => self.answer_quit(choice),
            Message::Settings(message) => {
                // One dialog at a time.
                if matches!(message, settings::Message::Request(settings::Request::Open)) {
                    self.help.close();
                    self.info.close();
                }
                let _ = self.settings.update(message);
                self.apply_settings()
            }
            Message::Help(help::Message::Request(help::Request::Open)) => {
                self.open_help();
                Task::none()
            }
            Message::Help(message) => self.help.update(message).map(Message::Help),
            Message::Menu(menubar::Message::Chosen(tag)) => match self.menu.command(tag) {
                Some(command) => self.on_menu(command),
                None => Task::none(),
            },
            Message::Search(search::Message::Request(search::Request::Download)) => {
                self.download_search_results()
            }
            Message::Search(search::Message::Request(search::Request::ShowInFolder)) => {
                self.show_search_result()
            }
            Message::Search(message) => {
                if matches!(message, search::Message::Request(_)) {
                    self.active = Side::Phone;
                }
                // A walk that lost the phone says so in the device panel,
                // as the panes do.
                if let Some(error) = message.device_lost() {
                    self.device.lose_device(error.clone());
                    self.close_device_pane();
                    return Task::none();
                }
                self.search.update(message).map(Message::Search)
            }
            Message::SystemTheme(mode) => {
                self.system_theme = mode;
                Task::none()
            }
            // Both panes are told, without either being made the active one: a
            // press that did not start a drag is nobody's click here, and a
            // drag is dropped by the pane it started in.
            Message::WindowChanged(id) => {
                self.window = Some(id);
                self.window_changes += 1;
                let change = self.window_changes;
                // The timer is made inside the task: outside the runtime it
                // would panic.
                guard::perform(
                    async move {
                        tokio::time::sleep(WINDOW_SETTLE).await;
                        change
                    },
                    move || change,
                    Message::WindowSettled,
                )
            }
            Message::WindowSettled(change) => match self.window {
                Some(id) if change == self.window_changes => window::mode(id).then(move |mode| {
                    window::position(id).then(move |position| {
                        window::size(id).map(move |size| Message::WindowPlaced {
                            mode,
                            position,
                            size,
                        })
                    })
                }),
                // A later change is still settling.
                _ => Task::none(),
            },
            Message::WindowPlaced {
                mode,
                position,
                size,
            } => {
                let before = self.settings.current().window;
                let now = placement::kept(before, mode, position, size);
                self.change_settings(|settings| settings.window = now)
            }
            Message::Focused(focused) => {
                self.transfer.set_focused(focused);
                if focused {
                    self.menu.window_focused();
                    // Back from System Settings, perhaps having allowed a
                    // folder macOS refused.
                    return self
                        .local_pane
                        .read_again_if_refused()
                        .map(Message::LocalPane);
                }
                Task::none()
            }
            Message::LassoTick => Task::batch([
                self.device_pane
                    .update(explorer::Message::LassoTick)
                    .map(Message::DevicePane),
                self.local_pane
                    .update(explorer::Message::LassoTick)
                    .map(Message::LocalPane),
            ]),
            Message::SpringTick(now) => Task::batch([
                self.device_pane
                    .update(explorer::Message::SpringTick(now))
                    .map(Message::DevicePane),
                self.local_pane
                    .update(explorer::Message::SpringTick(now))
                    .map(Message::LocalPane),
                self.spring_sidebar(now, false),
            ]),
            Message::PointerReleased => {
                // The sidebar or across first: the panes' own release ends
                // the drag.
                let across = match self.drop_on_sidebar() {
                    Some(dropped) => dropped,
                    None => self.drop_across(),
                };
                let released = explorer::Message::Request(explorer::Request::PointerReleased);
                let tasks = Task::batch([
                    across,
                    self.device_pane
                        .update(released.clone())
                        .map(Message::DevicePane),
                    self.local_pane.update(released).map(Message::LocalPane),
                ]);
                // A column resize (or a reset) ended: its widths are
                // remembered, once.
                let (mac, phone) = (
                    self.local_pane.column_widths(),
                    self.device_pane.column_widths(),
                );
                let settings = self.settings.current();
                if (mac, phone) == (settings.mac_columns, settings.phone_columns) {
                    return tasks;
                }
                let saved = self.change_settings(|s| {
                    s.mac_columns = mac;
                    s.phone_columns = phone;
                });
                Task::batch([tasks, saved])
            }
            Message::ColumnResize(x) => {
                let moved = explorer::Message::ResizeMoved(x);
                if self.local_pane.is_resizing() {
                    self.local_pane.update(moved).map(Message::LocalPane)
                } else if self.device_pane.is_resizing() {
                    self.device_pane.update(moved).map(Message::DevicePane)
                } else {
                    Task::none()
                }
            }
        }
    }

    fn change_settings(&mut self, edit: impl FnOnce(&mut dto::Settings)) -> Task<Message> {
        self.settings.change(edit);
        self.apply_settings()
    }

    fn apply_language(&mut self) {
        let choice = self.settings.current().language;
        if self.language_applied == Some(choice) {
            return;
        }
        self.language_applied = Some(choice);
        let language = choice.resolve(&self.languages.preferred());
        if language == crate::i18n::language() {
            return;
        }
        crate::i18n::set_language(language);
        self.settings.language_changed();
        self.local_pane.language_changed();
        self.device_pane.language_changed();
        self.transfer.language_changed();
        self.device.language_changed();
        self.search.language_changed();
        self.help.language_changed();
        self.places.language_changed();
        self.preview.language_changed();
        self.crash.language_changed();
        self.info.language_changed();
        self.import.language_changed();
        self.mirror.language_changed();
        self.space.language_changed();
        self.menu.language_changed();
    }

    /// Pushes the settings into the panes; a hidden-files change reloads.
    fn apply_settings(&mut self) -> Task<Message> {
        self.apply_language();
        let settings = self.settings.current();
        self.device.set_auto_connect(settings.auto_connect);
        self.transfer.set_sound(settings.finish_sound);
        self.device_pane.set_keep_deleted(settings.keep_deleted);
        for pane in [&mut self.local_pane, &mut self.device_pane] {
            pane.set_folders_first(settings.folders_first);
            pane.set_status_bar(settings.status_bar);
        }
        self.local_pane.set_sort_order(settings.mac_order);
        self.device_pane.set_sort_order(settings.phone_order);
        self.local_pane.set_group_by(settings.mac_group);
        self.device_pane.set_group_by(settings.phone_group);
        self.local_pane.set_column_widths(settings.mac_columns);
        self.device_pane.set_column_widths(settings.phone_columns);
        Task::batch([
            self.local_pane
                .set_folder_sizes(settings.mac_folder_sizes)
                .map(Message::LocalPane),
            self.device_pane
                .set_folder_sizes(settings.phone_folder_sizes)
                .map(Message::DevicePane),
            self.local_pane
                .set_show_hidden(settings.show_hidden_on_mac)
                .map(Message::LocalPane),
            self.device_pane
                .set_show_hidden(settings.show_hidden_on_phone)
                .map(Message::DevicePane),
            self.local_pane
                .set_view(settings.mac_view)
                .map(Message::LocalPane),
            self.device_pane
                .set_view(settings.phone_view)
                .map(Message::DevicePane),
            self.local_pane
                .set_density(settings.list_density)
                .map(Message::LocalPane),
            self.device_pane
                .set_density(settings.list_density)
                .map(Message::DevicePane),
            self.local_pane
                .set_tile_size(settings.mac_tiles)
                .map(Message::LocalPane),
            self.device_pane
                .set_tile_size(settings.phone_tiles)
                .map(Message::DevicePane),
        ])
    }

    /// Makes the active pane's grid tiles one step bigger or smaller, saved
    /// for that pane; only in Grid view (in a list there is nothing to see).
    fn step_tiles(&mut self, bigger: bool) -> Task<Message> {
        let side = self.target();
        let settings = self.settings.current();
        let (view, size) = match side {
            Side::Mac => (settings.mac_view, settings.mac_tiles),
            Side::Phone => (settings.phone_view, settings.phone_tiles),
        };
        if view != dto::ViewMode::Grid {
            return Task::none();
        }
        let size = if bigger {
            size.bigger()
        } else {
            size.smaller()
        };
        self.change_settings(|s| match side {
            Side::Mac => s.mac_tiles = size,
            Side::Phone => s.phone_tiles = size,
        })
    }

    /// Refreshes the pane a transfer went to, selecting what arrived.
    fn show_result(&mut self, summary: &dto::TransferSummary) -> Task<Message> {
        let select = summary.first_item.clone();
        let arrived = match summary.direction {
            dto::Direction::Download
                if self.local_pane.current_path() == Some(&summary.destination) =>
            {
                self.local_pane
                    .refresh_and_select(select)
                    .map(Message::LocalPane)
            }
            dto::Direction::Upload
                if self.device_pane.current_path() == Some(&summary.destination) =>
            {
                self.device_pane
                    .refresh_and_select(select)
                    .map(Message::DevicePane)
            }
            // Inside the Mac: what arrived is shown where it went, and a move
            // took things from wherever the pane is now (below).
            dto::Direction::Local
                if self.local_pane.current_path() == Some(&summary.destination) =>
            {
                self.local_pane
                    .refresh_and_select(select)
                    .map(Message::LocalPane)
            }
            _ => Task::none(),
        };
        // A move took files off the phone, wherever the Mac pane is now: the
        // phone pane shows them gone, read from what the explorer kept true as
        // they went, not asked of the phone one by one. The phone's own change
        // events are ignored while a transfer runs, so nothing else will.
        let gone = match &summary.removal {
            // A move to the phone put Mac items in the Trash: the Mac pane
            // shows them gone, wherever it is now.
            Some(removal) if removal.from_mac => self
                .local_pane
                .refresh_and_select(None)
                .map(Message::LocalPane),
            Some(removal) if removal.removed > 0 => self
                .device_pane
                .reload_remembered()
                .map(Message::DevicePane),
            _ => Task::none(),
        };
        Task::batch([arrived, gone])
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().map(Message::Keyboard),
            self.menu.subscription().map(Message::Menu),
            iced::event::listen_with(file_drop),
            iced::event::listen_with(pointer_released),
            iced::event::listen_with(focus_change),
            iced::event::listen_with(window_change),
            iced::event::listen_with(side_button),
            iced::system::theme_changes().map(Message::SystemTheme),
            // A disk plugged in or ejected shows in Places soon after.
            iced::time::every(PLACES_EVERY).map(|_| Message::Places(places::Message::Refresh)),
            // The close button asks first while a transfer runs.
            window::close_requests().map(|_| Message::CloseRequested),
            // Only while a column is being resized: no message per pointer
            // movement otherwise.
            if self.device_pane.is_resizing() || self.local_pane.is_resizing() {
                iced::event::listen_with(resize_pointer)
            } else {
                Subscription::none()
            },
            // Only while a lasso is held past a list's edge, so the list
            // scrolls on; no clock otherwise.
            if self.device_pane.lasso_step().is_some() || self.local_pane.lasso_step().is_some() {
                iced::time::every(LASSO_BEAT).map(|_| Message::LassoTick)
            } else {
                Subscription::none()
            },
            // Only while files are being edited on the Mac: is one saved?
            if self.transfer.has_edits() {
                iced::time::every(transfer::EDIT_CHECK)
                    .map(|_| Message::Transfer(transfer::Message::CheckEdits))
            } else {
                Subscription::none()
            },
            // Only while a drag waits over a folder that may spring open.
            if self.device_pane.spring_pending()
                || self.local_pane.spring_pending()
                || self.sidebar_spring_pending()
            {
                iced::time::every(SPRING_BEAT).map(Message::SpringTick)
            } else {
                Subscription::none()
            },
        ])
    }

    /// What the drop overlay over the phone pane says.
    fn drop_hint(&self) -> String {
        if !self.device_pane.is_open() {
            return t("app.drop_no_phone").to_owned();
        }
        let folder = self
            .device_pane
            .current_path()
            .and_then(|path| Path::new(path).file_name())
            .map_or_else(
                || t("app.top_folder").to_owned(),
                |name| name.to_string_lossy().into_owned(),
            );
        if self.transfer.is_running() || self.transfer.is_paused() {
            tf("app.drop_queue", &[("folder", &folder)])
        } else {
            tf("app.drop_upload", &[("folder", &folder)])
        }
    }

    fn on_key(&mut self, event: keyboard::Event) -> Task<Message> {
        match event {
            keyboard::Event::ModifiersChanged(modifiers) => {
                self.set_modifiers(modifiers);
                Task::none()
            }
            keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            } => {
                self.set_modifiers(modifiers);
                match keymap::shortcut(&key, physical_key, modifiers) {
                    Some(shortcut) => self.run(shortcut),
                    None => Task::none(),
                }
            }
            _ => Task::none(),
        }
    }

    fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
        self.search.set_modifiers(modifiers);
        self.device_pane.set_modifiers(modifiers);
        self.local_pane.set_modifiers(modifiers);
    }

    /// The pane shortcuts go to: the active one, unless it is hidden.
    fn target(&self) -> Side {
        if self.active == Side::Mac && !self.settings.current().show_mac_pane {
            Side::Phone
        } else {
            self.active
        }
    }

    fn pane_of(&self, side: Side) -> &explorer::Handler {
        match side {
            Side::Mac => &self.local_pane,
            Side::Phone => &self.device_pane,
        }
    }

    fn run(&mut self, shortcut: Shortcut) -> Task<Message> {
        // Quitting comes before every dialog; while it is asked about, only
        // Escape (keep going) does anything.
        if shortcut == Shortcut::Quit {
            return self.ask_to_quit();
        }
        if let Some(quitting) = self.quitting {
            if quitting == Quitting::Asking && shortcut == Shortcut::Pane(explorer::Request::Escape)
            {
                self.quitting = None;
            }
            return Task::none();
        }
        // The import and mirror dialogs cover the window; Escape closes them.
        if self.import.is_open() || self.mirror.is_open() || self.space.is_open() {
            if shortcut == Shortcut::Pane(explorer::Request::Escape) {
                self.import.close();
                self.mirror.close();
                self.space.close();
            }
            return Task::none();
        }
        // The Settings dialog covers the window; Escape or ⌘, closes it.
        if self.settings.is_open() {
            return match shortcut {
                Shortcut::Pane(explorer::Request::Escape) | Shortcut::Settings => self.update(
                    Message::Settings(settings::Message::Request(settings::Request::Close)),
                ),
                _ => Task::none(),
            };
        }
        // Help covers the window too; Escape or ⌘/ closes it.
        if self.help.is_open() {
            return match shortcut {
                Shortcut::Pane(explorer::Request::Escape) | Shortcut::Help => {
                    self.update(Message::Help(help::Message::Request(help::Request::Close)))
                }
                _ => Task::none(),
            };
        }
        // Get Info covers the window too; Escape or ⌘I closes it.
        if self.info.is_open() {
            if matches!(
                shortcut,
                Shortcut::Pane(explorer::Request::Escape) | Shortcut::GetInfo
            ) {
                self.info.close();
            }
            return Task::none();
        }
        // Escape stops a fetch for Quick Look before anything else.
        if self.preview.is_fetching() && shortcut == Shortcut::Pane(explorer::Request::Escape) {
            self.preview.cancel();
            return Task::none();
        }
        // A key closes an open right-click menu; Escape does only that.
        if self.device_pane.has_menu() || self.local_pane.has_menu() {
            self.device_pane.close_menu();
            self.local_pane.close_menu();
            if shortcut == Shortcut::Pane(explorer::Request::Escape) {
                return Task::none();
            }
        }
        if shortcut == Shortcut::Settings {
            return self.update(Message::Settings(settings::Message::Request(
                settings::Request::Open,
            )));
        }
        if shortcut == Shortcut::Help {
            return self.update(Message::Help(help::Message::Request(help::Request::Open)));
        }
        if shortcut == Shortcut::Search {
            return self.open_search();
        }
        // The search panel stands in for the phone pane: Escape closes it, and
        // ⌘A selects every result.
        if self.search.is_open() && self.target() == Side::Phone {
            match shortcut {
                Shortcut::Pane(explorer::Request::Escape) => {
                    self.search.close();
                    return Task::none();
                }
                Shortcut::Pane(explorer::Request::SelectAll) => {
                    return self
                        .search
                        .update(search::Message::Request(search::Request::SelectAll))
                        .map(Message::Search);
                }
                // The phone pane underneath is not where the keys go.
                Shortcut::Pane(_) | Shortcut::GetInfo | Shortcut::TypeSelect(_) => {
                    return Task::none();
                }
                _ => {}
            }
        }
        let side = self.target();
        // While a dialog is open only Escape reaches the pane (and ↑/↓, which
        // pick a folder in Go to Folder); the rest would act on the files
        // behind it.
        let pane = self.pane_of(side);
        let passes = match &shortcut {
            Shortcut::Pane(explorer::Request::Escape) => true,
            Shortcut::Pane(explorer::Request::Step { .. }) => pane.is_going_to(),
            _ => false,
        };
        if pane.is_editing() && !passes {
            return Task::none();
        }
        match shortcut {
            Shortcut::Pane(request) => {
                let message = explorer::Message::Request(request);
                match side {
                    Side::Mac => self.update(Message::LocalPane(message)),
                    Side::Phone => self.update(Message::DevicePane(message)),
                }
            }
            Shortcut::TypeSelect(text) => {
                let message = explorer::Message::Request(explorer::Request::TypeAhead {
                    text,
                    at: std::time::Instant::now(),
                });
                match side {
                    Side::Mac => self.update(Message::LocalPane(message)),
                    Side::Phone => self.update(Message::DevicePane(message)),
                }
            }
            // Taken at the top of `run`.
            Shortcut::Quit => Task::none(),
            Shortcut::UseGroups => {
                let now = match side {
                    Side::Mac => self.settings.current().mac_group,
                    Side::Phone => self.settings.current().phone_group,
                };
                // Finder's Use Groups: on by date, or off.
                let by = if now == dto::GroupBy::None {
                    dto::GroupBy::Date
                } else {
                    dto::GroupBy::None
                };
                self.set_group(side, by)
            }
            Shortcut::ToggleHidden => {
                let settings = self.settings.current();
                let message =
                    |show| explorer::Message::Request(explorer::Request::SetShowHidden(show));
                match side {
                    Side::Mac => {
                        self.update(Message::LocalPane(message(!settings.show_hidden_on_mac)))
                    }
                    Side::Phone => {
                        self.update(Message::DevicePane(message(!settings.show_hidden_on_phone)))
                    }
                }
            }
            // A request to the Mac pane makes it the active one.
            Shortcut::GoToMac(place) => {
                let path = place.path(&self.home);
                self.update(Message::LocalPane(explorer::Message::Request(
                    explorer::Request::GoTo(path),
                )))
            }
            Shortcut::TilesBigger => self.step_tiles(true),
            Shortcut::TilesSmaller => self.step_tiles(false),
            Shortcut::Undo => {
                // The latest change of either pane, as Finder has one Undo.
                if self.local_pane.changed_at() >= self.device_pane.changed_at() {
                    self.local_pane.undo().map(Message::LocalPane)
                } else {
                    self.device_pane.undo().map(Message::DevicePane)
                }
            }
            Shortcut::SwitchPane => {
                if self.settings.current().show_mac_pane {
                    self.active = match side {
                        Side::Mac => Side::Phone,
                        Side::Phone => Side::Mac,
                    };
                }
                Task::none()
            }
            Shortcut::Copy => {
                if let Some(selection) = self.pane_of(side).selection() {
                    self.set_clipboard(Some(Clipboard {
                        side,
                        selections: vec![selection],
                        cut: false,
                    }));
                }
                Task::none()
            }
            Shortcut::Cut => {
                // A cut on the phone moves inside it or to the Mac; a cut on
                // the Mac moves to the phone. (Moving inside the Mac is 50.)
                if let Some(selection) = self.pane_of(side).selection() {
                    self.set_clipboard(Some(Clipboard {
                        side,
                        selections: vec![selection],
                        cut: true,
                    }));
                }
                Task::none()
            }
            Shortcut::CopyToQueue => {
                if let Some(selection) = self.pane_of(side).selection() {
                    let clipboard = match self.clipboard.take() {
                        Some(mut clipboard) if clipboard.side == side && !clipboard.cut => {
                            clipboard.selections.push(selection);
                            clipboard
                        }
                        // Something from the other side, or a cut, starts a
                        // new clipboard.
                        _ => Clipboard {
                            side,
                            selections: vec![selection],
                            cut: false,
                        },
                    };
                    self.set_clipboard(Some(clipboard));
                }
                Task::none()
            }
            // Space during a drag opens the folder under it at once, as in
            // Finder, rather than looking at a file.
            Shortcut::QuickLook if self.spring_held() => Task::batch([
                self.device_pane.spring_now().map(Message::DevicePane),
                self.local_pane.spring_now().map(Message::LocalPane),
                self.spring_sidebar(Instant::now(), true),
            ]),
            Shortcut::QuickLook => self.quick_look(side),
            Shortcut::GetInfo => self.open_info(side, true),
            Shortcut::Settings | Shortcut::Help | Shortcut::Search => Task::none(),
            Shortcut::Paste => self.paste(side),
        }
    }

    /// Queues the copy or move of `from` into the phone folder `path` through
    /// the Mac, one job per source folder. A cut is used up once queued, like
    /// a move to the other side; a copy stays for another paste.
    fn queue_through_mac(
        &mut self,
        mode: dto::Paste,
        from: &[explorer::Selection],
        volume_id: u64,
        path: &str,
    ) -> Task<Message> {
        if mode == dto::Paste::Move
            && self
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.cut)
        {
            self.set_clipboard(None);
        }
        Task::batch(from.iter().map(|selection| {
            let request = dto::PasteRequest {
                mode,
                from_storage: selection.volume_id,
                from: selection.path.clone(),
                names: selection.names.clone(),
                to_storage: volume_id,
                to: path.to_owned(),
            };
            self.transfer.paste_on_phone(request).map(Message::Transfer)
        }))
    }

    /// Queues the copy or move of `from` into the Mac folder `path`, one job
    /// per source folder: the pane found it too big to do on the spot. A cut is
    /// used up once queued, like any other move.
    fn queue_on_mac(
        &mut self,
        mode: dto::Paste,
        from: &[explorer::Selection],
        path: &str,
    ) -> Task<Message> {
        if mode == dto::Paste::Move
            && self
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.cut)
        {
            self.set_clipboard(None);
        }
        Task::batch(from.iter().map(|selection| {
            let request = dto::PasteRequest {
                mode,
                from_storage: selection.volume_id,
                from: selection.path.clone(),
                names: selection.names.clone(),
                to_storage: MAC_VOLUME,
                to: path.to_owned(),
            };
            self.transfer.paste_on_mac(request).map(Message::Transfer)
        }))
    }

    /// Why the open phone can't be closed for another now, if it can't: work
    /// on it would carry on against the other phone's files.
    fn why_not_switch(&self) -> Option<String> {
        self.why_not_leave(Leaving::ToSwitch)
    }

    fn why_not_leave(&self, then: Leaving) -> Option<String> {
        let name = self.device.open_name()?;
        let said = |to_switch: &'static str, to_open: &'static str| {
            let key = match then {
                Leaving::ToSwitch => to_switch,
                Leaving::ToOpen => to_open,
            };
            Some(tf(key, &[("name", name)]))
        };
        if self.preview.is_fetching() {
            return said(key("app.wait_quick_look"), key("app.wait_quick_look_open"));
        }
        if self.transfer.has_phone_work() {
            return said(key("app.wait_transfers"), key("app.wait_transfers_open"));
        }
        if self.space.is_busy() {
            return said(key("app.wait_space"), key("app.wait_space_open"));
        }
        if self.device_pane.is_editing() || self.device_pane.is_pasting() {
            return Some(tf("app.wait_editing", &[("name", name)]));
        }
        None
    }

    /// Opens Free Up Space on the storage picked in the sidebar.
    fn open_space(&mut self) -> Task<Message> {
        let Some((storage_id, storage)) = self.device.selected_storage() else {
            return Task::none();
        };
        if self.dialog_is_open() {
            return Task::none();
        }
        self.device_pane.close_menu();
        self.space.open(storage_id, &storage).map(Message::Space)
    }

    /// Opens the import dialog for the storage picked in the sidebar, into the
    /// folder the Mac pane shows.
    fn open_import(&mut self) -> Task<Message> {
        let (Some((storage_id, storage)), Some(destination)) = (
            self.device.selected_storage(),
            self.local_pane.current_path(),
        ) else {
            return Task::none();
        };
        let destination = destination.to_owned();
        self.device_pane.close_menu();
        let folders = self.settings.current().import_folders;
        let remove = self.settings.current().import_remove;
        self.import
            .open(
                storage_id,
                &storage,
                &destination,
                &self.home,
                folders,
                remove,
            )
            .map(Message::Import)
    }

    /// Space: the selection of the pane `side` in Quick Look (phone files are
    /// fetched first), or Quick Look closed.
    fn quick_look(&mut self, side: Side) -> Task<Message> {
        // The search stands in for the phone pane: its selection is not shown.
        if side == Side::Phone && self.search.is_open() {
            return Task::none();
        }
        let Some(selection) = self.pane_of(side).selection() else {
            return Task::none();
        };
        match side {
            Side::Mac => {
                let paths = selection
                    .names
                    .iter()
                    .map(|name| {
                        std::path::Path::new(&selection.path)
                            .join(name)
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                self.preview.mac(paths).map(Message::Preview)
            }
            Side::Phone => self
                .preview
                .phone(dto::PreviewRequest {
                    storage_id: selection.volume_id,
                    folder: selection.path,
                    names: selection.names,
                })
                .map(Message::Preview),
        }
    }

    /// The Mac's share menu for the pane's selection, beside `at` (where
    /// the right-click was; `None`: the window's middle). Phone files are
    /// fetched first, as for Quick Look.
    fn share(&mut self, side: Side, at: Option<(f32, f32)>) -> Task<Message> {
        if side == Side::Phone && self.search.is_open() {
            return Task::none();
        }
        let Some(selection) = self.pane_of(side).selection() else {
            return Task::none();
        };
        match side {
            Side::Mac => {
                let paths = mac_paths(&selection);
                self.preview.mac_share(paths, at).map(Message::Preview)
            }
            Side::Phone => self
                .preview
                .phone_share(
                    dto::PreviewRequest {
                        storage_id: selection.volume_id,
                        folder: selection.path,
                        names: selection.names,
                    },
                    at,
                )
                .map(Message::Preview),
        }
    }

    /// Opens the mirror dialog for the folder the phone pane shows and the one
    /// the Mac pane shows.
    fn open_mirror(&mut self) -> Task<Message> {
        let (Some((storage_id, storage)), Some(phone_folder), Some(mac_folder)) = (
            self.device.selected_storage(),
            self.device_pane.current_path(),
            self.local_pane.current_path(),
        ) else {
            return Task::none();
        };
        let (phone_folder, mac_folder) = (phone_folder.to_owned(), mac_folder.to_owned());
        self.device_pane.close_menu();
        let task = self
            .mirror
            .open(storage_id, &storage, &phone_folder, &mac_folder, &self.home)
            .map(Message::Mirror);
        let serial = self
            .device
            .open_phone()
            .map(|(serial, _)| serial.to_owned());
        self.mirror.offer_saving(serial);
        self.sync_mirror_saved();
        task
    }

    /// Opens the mirror dialog for a saved mirror from the sidebar, as it
    /// was saved: it compares first, and runs only when Mirror is clicked.
    fn open_saved_mirror(&mut self, at: usize, storage_id: u64) -> Task<Message> {
        let Some(saved) = self.places.mirror(at).cloned() else {
            return Task::none();
        };
        if self.dialog_is_open() {
            return Task::none();
        }
        let task = self
            .mirror
            .open_saved(storage_id, &saved, &self.home)
            .map(Message::Mirror);
        self.sync_mirror_saved();
        task
    }

    /// Tells the mirror dialog whether what it shows now is saved.
    fn sync_mirror_saved(&mut self) {
        if let Some(shown) = self.mirror.to_save() {
            let saved = self.places.is_saved(&shown);
            self.mirror.set_saved(saved);
        }
    }

    /// Opens the search of the storage the phone pane shows, in its place.
    fn open_search(&mut self) -> Task<Message> {
        let Some((storage_id, name)) = self.device.selected_storage() else {
            return Task::none();
        };
        if !self.device_pane.is_open() {
            return Task::none();
        }
        self.active = Side::Phone;
        self.device_pane.close_menu();
        let hidden = self.settings.current().show_hidden_on_phone;
        self.search
            .open(storage_id, &name, hidden)
            .map(Message::Search)
    }

    /// Downloads the selected search results into the Mac pane's folder: one
    /// transfer for each folder they are in.
    fn download_search_results(&mut self) -> Task<Message> {
        let (Some(storage_id), Some(destination)) =
            (self.search.storage_id(), self.local_pane.current_path())
        else {
            return Task::none();
        };
        let destination = destination.to_owned();
        let requests: Vec<dto::DownloadRequest> = self
            .search
            .chosen_by_folder()
            .into_iter()
            .map(|(source, names)| dto::DownloadRequest {
                storage_id,
                source,
                names,
                destination: destination.clone(),
            })
            .collect();
        Task::batch(requests.into_iter().map(|request| {
            self.transfer
                .download_as(dto::Paste::Copy, request)
                .map(Message::Transfer)
        }))
    }

    /// Closes the search and shows the folder the selected result is in, with
    /// it selected.
    fn show_search_result(&mut self) -> Task<Message> {
        let Some((folder, name)) = self.search.cursor_item() else {
            return Task::none();
        };
        self.search.close();
        self.device_pane
            .show_item(folder, name)
            .map(Message::DevicePane)
    }

    /// Closes the phone pane because the phone is gone, and with it what was
    /// copied or cut there: a phone plugged in next can have the same storage
    /// id and the same folders, and a paste would act on its files.
    fn close_device_pane(&mut self) {
        self.device_pane.close();
        // What a search found was on that phone, and the import was from it.
        self.search.close();
        self.import.close();
        self.mirror.close();
        self.space.close();
        self.preview.cancel();
        // A count of that phone's folders can't go on, and its numbers are
        // about a phone that is gone.
        if self.info.is_about_the_phone() {
            self.info.close();
        }
        if self
            .clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.side == Side::Phone)
        {
            self.set_clipboard(None);
        }
    }

    /// Replaces what ⌘C or ⌘X remembered, dimming a cut in the pane it was
    /// cut in.
    fn set_clipboard(&mut self, clipboard: Option<Clipboard>) {
        let cut = clipboard.as_ref().filter(|clipboard| clipboard.cut);
        let from = |side| {
            cut.filter(|clipboard| clipboard.side == side)
                .and_then(|clipboard| clipboard.selections.first())
        };
        self.device_pane.set_cut(from(Side::Phone));
        self.local_pane.set_cut(from(Side::Mac));
        self.clipboard = clipboard;
    }

    /// ⌘V in `side`. Across the two sides a paste transfers (phone → Mac
    /// downloads, Mac → phone uploads), one queued transfer per remembered
    /// selection. Inside the phone it copies, or moves what ⌘X cut.
    fn paste(&mut self, side: Side) -> Task<Message> {
        let Some(clipboard) = self.clipboard.clone() else {
            return Task::none();
        };
        match (clipboard.side, side) {
            (Side::Phone, Side::Phone) => {
                let mode = if clipboard.cut {
                    dto::Paste::Move
                } else {
                    dto::Paste::Copy
                };
                self.device_pane
                    .paste(mode, clipboard.selections)
                    .map(Message::DevicePane)
            }
            // A cut pasted on the Mac moves: each file is copied, checked, and
            // only then removed from the phone. It is queued like any
            // transfer, so what was cut is used up now.
            (Side::Phone, Side::Mac) if clipboard.cut => {
                if self.local_pane.current_path().is_none() {
                    return Task::none();
                }
                self.set_clipboard(None);
                Task::batch(
                    clipboard
                        .selections
                        .into_iter()
                        .map(|selection| self.start_download_as(dto::Paste::Move, Some(selection))),
                )
            }
            (Side::Phone, Side::Mac) => Task::batch(
                clipboard
                    .selections
                    .into_iter()
                    .map(|selection| self.start_download(Some(selection))),
            ),
            // A cut pasted on the phone moves: each file is sent, read back from
            // the phone, and only then is its original put in the Trash. It is
            // queued like any transfer, so what was cut is used up now, but
            // only if there is a phone folder to send it to.
            (Side::Mac, Side::Phone) if clipboard.cut => {
                if self.device_pane.current_path().is_none() {
                    return Task::none();
                }
                self.set_clipboard(None);
                Task::batch(
                    clipboard
                        .selections
                        .into_iter()
                        .map(|selection| self.start_upload_as(dto::Paste::Move, Some(selection))),
                )
            }
            (Side::Mac, Side::Phone) => Task::batch(
                clipboard
                    .selections
                    .into_iter()
                    .map(|selection| self.start_upload(Some(selection))),
            ),
            // Inside the Mac, as in the phone: copies, or moves what ⌘X cut.
            (Side::Mac, Side::Mac) => {
                let mode = if clipboard.cut {
                    dto::Paste::Move
                } else {
                    dto::Paste::Copy
                };
                self.local_pane
                    .paste(mode, clipboard.selections)
                    .map(Message::LocalPane)
            }
        }
    }

    /// A right-click menu command that needs both panes or the clipboard:
    /// the same as the button or shortcut it stands for.
    /// Both panes show a folder that has been read, and no dialog or phone
    /// search covers either: one can be compared with the other.
    fn can_compare(&self) -> bool {
        !self.dialog_is_open()
            && self.settings.current().show_mac_pane
            && !self.search.is_open()
            && self.device_pane.is_listed()
            && self.local_pane.is_listed()
    }

    /// Selects, in the pane on `side`, what the other pane's folder lacks
    /// or has: one pass over each, from what both have listed.
    fn select_against(&mut self, side: Side, want: explorer::Against) {
        if !self.can_compare() {
            return;
        }
        self.active = side;
        let (pane, other) = match side {
            Side::Mac => (&mut self.local_pane, &self.device_pane),
            Side::Phone => (&mut self.device_pane, &self.local_pane),
        };
        let name = other.shown_name().unwrap_or_default();
        pane.select_against(want, other.entries(), &name);
    }

    /// Fetches the one phone file selected into a working copy and opens
    /// it in its Mac app; a saved edit is offered back in the transfers.
    fn edit_on_mac(&mut self) -> Task<Message> {
        if self.dialog_is_open() {
            return Task::none();
        }
        let (Some(phone), Some((storage_id, folder)), Some(name)) = (
            self.device.open_key(),
            self.device_pane.location(),
            self.device_pane.single_file(),
        ) else {
            return Task::none();
        };
        let request = dto::EditRequest {
            phone,
            storage_id,
            folder: folder.to_owned(),
            name,
        };
        self.transfer.edit(request).map(Message::Transfer)
    }

    /// Queues a byte-for-byte check of the selection in the pane on `side`
    /// against what has the same names in the other pane's folder: the
    /// phone's files are read, nothing is written.
    fn check_contents(&mut self, side: Side) -> Task<Message> {
        if !self.can_compare() {
            return Task::none();
        }
        let (Some(mac_folder), Some((storage_id, phone_folder))) =
            (self.local_pane.current_path(), self.device_pane.location())
        else {
            return Task::none();
        };
        let Some(selection) = self.pane_of(side).selection() else {
            return Task::none();
        };
        self.active = side;
        let request = dto::CheckRequest {
            storage_id,
            phone_folder: phone_folder.to_owned(),
            mac_folder: mac_folder.to_owned(),
            names: selection.names,
        };
        self.transfer.check(request).map(Message::Transfer)
    }

    fn menu_action(&mut self, side: Side, action: explorer::MenuAction) -> Task<Message> {
        let at = self.pane_of(side).menu_at().map(|at| (at.x, at.y));
        self.device_pane.close_menu();
        self.local_pane.close_menu();
        self.active = side;
        let shortcut = match action {
            explorer::MenuAction::Cut => Shortcut::Cut,
            explorer::MenuAction::Copy => Shortcut::Copy,
            explorer::MenuAction::CopyToQueue => Shortcut::CopyToQueue,
            explorer::MenuAction::Paste => Shortcut::Paste,
            explorer::MenuAction::Transfer => {
                let transfer = explorer::Message::Request(explorer::Request::Transfer);
                return match side {
                    Side::Mac => self.update(Message::LocalPane(transfer)),
                    Side::Phone => self.update(Message::DevicePane(transfer)),
                };
            }
            explorer::MenuAction::Pin | explorer::MenuAction::PinHere => {
                self.pin(side, action == explorer::MenuAction::Pin);
                return Task::none();
            }
            explorer::MenuAction::GetInfo | explorer::MenuAction::GetInfoHere => {
                return self.open_info(side, action == explorer::MenuAction::GetInfo);
            }
            explorer::MenuAction::Share => return self.share(side, at),
            explorer::MenuAction::SelectNotInOther => {
                self.select_against(side, explorer::Against::Missing);
                return Task::none();
            }
            explorer::MenuAction::SelectAlsoInOther => {
                self.select_against(side, explorer::Against::Same);
                return Task::none();
            }
            explorer::MenuAction::CheckAgainstOther => return self.check_contents(side),
            explorer::MenuAction::EditOnMac => return self.edit_on_mac(),
            // The pane carries out the rest itself.
            _ => return Task::none(),
        };
        self.run(shortcut)
    }

    /// Pins the pane's selected folder (`selected`) or the folder it shows
    /// to the sidebar. A phone folder is pinned to that phone, by its serial
    /// number; a phone without one can't have pins.
    fn pin(&mut self, side: Side, selected: bool) {
        let pane = self.pane_of(side);
        let Some(folder) = pane.current_path() else {
            return;
        };
        let path = match (selected, pane.selection()) {
            (true, Some(selection)) => match selection.names.as_slice() {
                [name] => explorer::child_path(folder, name),
                _ => return,
            },
            (true, None) => return,
            (false, _) => folder.to_owned(),
        };
        let pin = match side {
            Side::Mac => dto::Pin {
                side: dto::PinSide::Mac,
                label: places::pin_label(&path, english_only("Macintosh HD")),
                path,
                phone: None,
            },
            Side::Phone => {
                let (Some((serial, _)), Some((storage_id, storage_name))) =
                    (self.device.open_phone(), self.device.selected_storage())
                else {
                    return;
                };
                if serial.is_empty() {
                    self.places.refuse(t("app.pin_no_serial").to_owned());
                    return;
                }
                let serial = serial.to_owned();
                dto::Pin {
                    side: dto::PinSide::Phone,
                    label: places::pin_label(&path, &storage_name),
                    path,
                    phone: Some(dto::PinPhone {
                        serial,
                        storage_id,
                        storage_name,
                    }),
                }
            }
        };
        self.places.pin(pin);
    }

    /// Opens a pinned phone folder: on the storage shown, or after picking
    /// its storage (not while work on the phone would carry on there).
    fn go_to_phone_pin(&mut self, storage_id: u64, path: String) -> Task<Message> {
        self.active = Side::Phone;
        let go = Message::DevicePane(explorer::Message::Request(explorer::Request::GoTo(path)));
        if self.device.selected_storage().map(|(id, _)| id) == Some(storage_id) {
            return self.update(go);
        }
        if let Some(why) = self.why_not_leave(Leaving::ToOpen) {
            self.places.refuse(why);
            return Task::none();
        }
        // Picking the storage opens the pane at its top; going on to the
        // folder cancels that reading.
        let picked = self.update(Message::Device(device::Message::Request(
            device::Request::SelectStorage(storage_id),
        )));
        let gone = self.update(go);
        Task::batch([picked, gone])
    }

    /// Whether ⌘V in `side` would do something: the clipboard holds items
    /// from the other side, or from the same side (each pane copies and moves
    /// inside itself).
    fn can_paste(&self) -> bool {
        self.clipboard.is_some()
    }

    /// The open right-click menu over everything, with a backdrop that
    /// closes it when anything else is clicked. A left click outside only
    /// closes it, like a Mac menu; a right-click elsewhere closes it and
    /// lets the click through, so a menu opens there instead.
    fn with_menu<'a>(&'a self, window: Element<'a, Message>) -> Element<'a, Message> {
        let close = explorer::Message::Request(explorer::Request::CloseMenu);
        let menu = if let Some(menu) = self.device_pane.menu(self.can_paste()) {
            (menu.map(Message::DevicePane), Message::DevicePane(close))
        } else if let Some(menu) = self.local_pane.menu(self.can_paste()) {
            (menu.map(Message::LocalPane), Message::LocalPane(close))
        } else {
            return window;
        };
        let (menu, close) = menu;
        let backdrop = mouse_area(explorer::right_click(
            space().width(Length::Fill).height(Length::Fill),
            {
                let close = close.clone();
                move |_, _| close.clone()
            },
        ))
        .on_press(close);
        stack![window, backdrop, menu].into()
    }

    /// Opens Help over the window, telling it how the phone looks right now.
    /// After an update to `current`, opens Help on what is new since the
    /// version that ran before, once: the version is then remembered. A first
    /// run only remembers it; there is nothing new to someone new.
    fn greet(&mut self, current: dto::Version) {
        let last_seen = self.settings.current().last_seen_version;
        if !help::whats_new::after_update(last_seen, current).is_empty()
            && let Some(since) = last_seen
        {
            let phone = self.help_phone();
            self.help.open_whats_new(phone, since);
        }
        if last_seen != Some(current) {
            self.settings
                .change(|settings| settings.last_seen_version = Some(current));
        }
    }

    /// Get Info on the pane on `side`: its selection (`selected`), or the
    /// folder it shows.
    fn open_info(&mut self, side: Side, selected: bool) -> Task<Message> {
        let Some((folder, items)) = self.pane_of(side).info_subject() else {
            return Task::none();
        };
        let place = match side {
            Side::Mac => info::Place::Mac,
            // The pane's own storage, which the folder belongs to even while a
            // switch is under way; the device panel only names it.
            Side::Phone => match (self.device_pane.volume_id(), self.device.selected_storage()) {
                (Some(storage_id), Some((_, storage))) => info::Place::Phone {
                    storage_id,
                    storage: storage.to_owned(),
                    abilities: self.device.abilities(),
                },
                _ => return Task::none(),
            },
        };
        self.device_pane.close_menu();
        self.local_pane.close_menu();
        let items = if selected { items } else { Vec::new() };
        self.info
            .open(info::Subject {
                place,
                folder,
                items,
            })
            .map(Message::Info)
    }

    fn open_help(&mut self) {
        self.device_pane.close_menu();
        self.local_pane.close_menu();
        // One dialog at a time.
        let _ = self
            .settings
            .update(settings::Message::Request(settings::Request::Close));
        self.info.close();
        self.help.open(self.help_phone());
    }

    /// How the phone looks to Help now: the sidebar's line, what it can do,
    /// and the folder the phone pane shows (what a timed reading reads).
    fn help_phone(&self) -> help::Phone {
        help::Phone::new(self.device.status_line(), self.device.is_connected())
            .with(
                self.device.abilities(),
                self.device_pane
                    .location()
                    .map(|(storage_id, path)| (storage_id, path.to_owned())),
            )
            .with_link(self.device.link())
    }

    /// Copies Mac items into the device pane's folder.
    fn start_upload(&mut self, selection: Option<explorer::Selection>) -> Task<Message> {
        self.start_upload_as(dto::Paste::Copy, selection)
    }

    /// Sends Mac items into the phone pane's folder; a move puts each original
    /// in the Trash once its copy is read back.
    fn start_upload_as(
        &mut self,
        mode: dto::Paste,
        selection: Option<explorer::Selection>,
    ) -> Task<Message> {
        let Some(selection) = selection else {
            return Task::none();
        };
        self.upload_as(mode, mac_paths(&selection))
    }

    /// Uploads Mac paths into the device pane's folder. Without an open
    /// phone, or while another transfer runs, nothing happens; the drop
    /// overlay has already said why.
    fn upload(&mut self, sources: Vec<String>) -> Task<Message> {
        self.upload_as(dto::Paste::Copy, sources)
    }

    fn upload_as(&mut self, mode: dto::Paste, sources: Vec<String>) -> Task<Message> {
        let Some(destination) = self.device_pane.current_path() else {
            return Task::none();
        };
        let destination = destination.to_owned();
        self.upload_into(mode, sources, destination)
    }

    /// Uploads Mac paths into `destination` on the phone pane's storage.
    fn upload_into(
        &mut self,
        mode: dto::Paste,
        sources: Vec<String>,
        destination: String,
    ) -> Task<Message> {
        let Some(storage_id) = self.device_pane.volume_id() else {
            return Task::none();
        };
        self.upload_to(mode, sources, storage_id, destination)
    }

    /// Uploads Mac paths into `destination` on the storage `storage_id` of
    /// the open phone (a drop on the sidebar may name one the pane doesn't
    /// show).
    fn upload_to(
        &mut self,
        mode: dto::Paste,
        sources: Vec<String>,
        storage_id: u64,
        destination: String,
    ) -> Task<Message> {
        if sources.is_empty() {
            return Task::none();
        }
        let request = dto::UploadRequest {
            sources,
            storage_id,
            destination,
        };
        self.transfer
            .upload_as(mode, request)
            .map(Message::Transfer)
    }

    /// Copies phone items into the Mac pane's folder.
    fn start_download(&mut self, selection: Option<explorer::Selection>) -> Task<Message> {
        self.start_download_as(dto::Paste::Copy, selection)
    }

    /// Copies phone items into the Mac pane's folder, and for a move removes
    /// each original from the phone once its copy is checked.
    fn start_download_as(
        &mut self,
        mode: dto::Paste,
        selection: Option<explorer::Selection>,
    ) -> Task<Message> {
        let Some(destination) = self.local_pane.current_path() else {
            return Task::none();
        };
        let destination = destination.to_owned();
        self.download_into(mode, selection, destination)
    }

    /// Copies phone items into `destination` on the Mac (and for a move
    /// removes each original once its copy is checked).
    fn download_into(
        &mut self,
        mode: dto::Paste,
        selection: Option<explorer::Selection>,
        destination: String,
    ) -> Task<Message> {
        let Some(selection) = selection else {
            return Task::none();
        };
        let request = dto::DownloadRequest {
            storage_id: selection.volume_id,
            source: selection.path,
            names: selection.names,
            destination,
        };
        self.transfer
            .download_as(mode, request)
            .map(Message::Transfer)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let device_pane = if self.search.is_open() {
            self.search.view().map(Message::Search)
        } else if self.device_pane.is_open() {
            self.device_pane.view().map(Message::DevicePane)
        } else {
            self.device.view_main().map(Message::Device)
        };
        let device_pane = if self.drop_hovering {
            drop_overlay(device_pane, self.drop_hint())
        } else {
            device_pane
        };
        let device_pane = pane(device_pane, self.target() == Side::Phone);

        let layout = self.settings.current();
        let panes = if !layout.show_mac_pane {
            row![device_pane]
        } else {
            let local_pane = pane(
                self.local_pane.view().map(Message::LocalPane),
                self.target() == Side::Mac,
            );
            if layout.mac_pane_on_left {
                row![local_pane, device_pane]
            } else {
                row![device_pane, local_pane]
            }
        };

        let mut main = column![panes.spacing(8).height(Length::Fill)].spacing(8);
        // Below the panes, so nothing moves under the pointer.
        if self.crash.is_visible() {
            main = main.push(self.crash.view().map(Message::Crash));
        }
        if self.preview.is_visible() {
            main = main.push(self.preview.view().map(Message::Preview));
        }
        if self.transfer.is_visible() {
            main = main.push(self.transfer.view().map(Message::Transfer));
        }

        let window = row![self.sidebar(), main].padding(8).spacing(8);
        if let Some(quitting) = self.quitting {
            dimmed(window.into(), quit_view(quitting))
        } else if self.import.is_open() {
            dimmed(window.into(), self.import.view().map(Message::Import))
        } else if self.space.is_open() {
            dimmed(window.into(), self.space.view().map(Message::Space))
        } else if self.mirror.is_open() {
            dimmed(window.into(), self.mirror.view().map(Message::Mirror))
        } else if self.settings.is_open() {
            dimmed(window.into(), self.settings.view().map(Message::Settings))
        } else if self.info.is_open() {
            dimmed(window.into(), self.info.view().map(Message::Info))
        } else if self.help.is_open() {
            dimmed(window.into(), self.help.view().map(Message::Help))
        } else {
            self.with_menu(window.into())
        }
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let settings = button(
            row![
                text(t("menubar.settings")).size(13),
                space::horizontal(),
                text("⌘,").size(12).style(text::secondary)
            ]
            .align_y(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .style(button::text)
        .on_press(Message::Settings(settings::Message::Request(
            settings::Request::Open,
        )));

        let help = button(
            row![
                text(t("app.help_button")).size(13),
                space::horizontal(),
                text("⌘/").size(12).style(text::secondary)
            ]
            .align_y(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .style(button::text)
        .on_press(Message::Help(help::Message::Request(help::Request::Open)));

        let open_phone = self
            .device
            .open_phone()
            .map(|(serial, storages)| places::OpenPhone {
                serial,
                storages: storages
                    .into_iter()
                    .map(|(id, name)| (id, name.to_owned()))
                    .collect(),
            });
        let drop = self.sidebar_drop();
        let mut top = column![
            self.device
                .sidebar_top(self.device_pane.current_path(), drop.as_ref())
                .map(Message::Device),
            self.places
                .view(
                    open_phone.as_ref(),
                    self.local_pane.current_path(),
                    drop.as_ref()
                )
                .map(Message::Places),
        ]
        .spacing(20)
        .padding(16);
        if let Some(phones) = self.device.sidebar_phones() {
            top = top.push(phones.map(Message::Device));
        }
        container(column![
            container(iced::widget::scrollable(top)).height(Length::Fill),
            container(column![help, settings]).padding(8),
        ])
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .style(container::rounded_box)
        .into()
    }

    pub fn theme(&self) -> Theme {
        match (self.settings.current().theme, self.system_theme) {
            (dto::ThemeChoice::Light, _) | (dto::ThemeChoice::Auto, theme::Mode::Light) => {
                Theme::Light
            }
            _ => Theme::Dark,
        }
    }
}

/// Keeps only Finder drag-and-drop events.
/// A folder in Go › Recent Folders.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Recent {
    Mac(String),
    Phone { storage_id: u64, path: String },
}

/// How Go › Recent Folders names Mac folders: by name ("Home" for home),
/// with the folder above when two share a name.
fn mac_recent_labels(paths: &[String], home: &str) -> Vec<String> {
    let name = |path: &str| match path {
        "/" => "/".to_owned(),
        _ if path == home => t("places.home").to_owned(),
        _ => explorer::folder_name(path),
    };
    let names: Vec<String> = paths.iter().map(|path| name(path)).collect();
    names
        .iter()
        .zip(paths)
        .map(|(shown, path)| {
            if names.iter().filter(|other| *other == shown).count() < 2 {
                return shown.clone();
            }
            let above = Path::new(path)
                .parent()
                .map_or_else(|| "/".to_owned(), |parent| name(&parent.to_string_lossy()));
            format!("{shown} — {above}")
        })
        .collect()
}

/// How Go › Recent Folders names a phone folder: its name (the storage's at
/// the top) and the phone's.
fn phone_recent_label(path: &str, storage: &str, phone: &str) -> String {
    let name = if path == "/" {
        storage.to_owned()
    } else {
        explorer::folder_name(path)
    };
    format!("{name} — {phone}")
}

fn file_drop(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::FileHovered(_)) => {
            Some(Message::Drop(FileDrop::Hovered))
        }
        iced::Event::Window(window::Event::FilesHoveredLeft) => Some(Message::Drop(FileDrop::Left)),
        iced::Event::Window(window::Event::FileDropped(path)) => {
            Some(Message::Drop(FileDrop::Dropped(path)))
        }
        _ => None,
    }
}

/// The left button coming up anywhere in the window, whichever widget had it:
/// a drag onto a folder ends here, so nothing depends on which row saw it.
fn pointer_released(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left))
        | iced::Event::Touch(iced::touch::Event::FingerLifted { .. }) => {
            Some(Message::PointerReleased)
        }
        _ => None,
    }
}

/// The window coming to the front or going behind another app's.
/// Where the pointer is, while a column is being resized.
fn resize_pointer(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            Some(Message::ColumnResize(position.x))
        }
        _ => None,
    }
}

/// The mouse's side buttons go back and forward, as in Finder.
fn side_button(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Back)) => {
            Some(Message::SideButton(false))
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Forward)) => {
            Some(Message::SideButton(true))
        }
        _ => None,
    }
}

/// The window moving or changing size (full screen included).
fn window_change(
    event: iced::Event,
    _status: iced::event::Status,
    window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::Moved(_) | window::Event::Resized(_)) => {
            Some(Message::WindowChanged(window))
        }
        _ => None,
    }
}

fn focus_change(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::Focused) => Some(Message::Focused(true)),
        iced::Event::Window(window::Event::Unfocused) => Some(Message::Focused(false)),
        _ => None,
    }
}

/// "A transfer is under way": stop it and quit, or keep going; then, while
/// it stops, a way out if the phone no longer answers.
fn quit_view<'a>(quitting: Quitting) -> Element<'a, Message> {
    let card = match quitting {
        Quitting::Asking => column![
            text(t("app.quit_title")).size(18),
            text(t("app.quit_asking")).size(13),
            row![
                space::horizontal(),
                button(text(t("app.keep_going")).size(13))
                    .style(button::secondary)
                    .on_press(Message::Quit(QuitChoice::KeepGoing)),
                button(text(t("app.stop_and_quit")).size(13))
                    .style(button::primary)
                    .on_press(Message::Quit(QuitChoice::StopAndQuit)),
            ]
            .spacing(8),
        ],
        Quitting::Stopping => column![
            text(t("app.stopping")).size(18),
            text(t("app.stopping_note")).size(13),
            row![
                space::horizontal(),
                button(text(t("app.quit_now")).size(13))
                    .style(button::secondary)
                    .on_press(Message::Quit(QuitChoice::QuitNow)),
            ],
        ],
    };
    container(card.spacing(12))
        .padding([18, 22])
        .max_width(480)
        .style(container::rounded_box)
        .into()
}

/// Dims `base` and says what dropping will do.
fn drop_overlay(base: Element<'_, Message>, hint: String) -> Element<'_, Message> {
    let card = container(text(hint).size(18))
        .padding([16, 24])
        .style(container::rounded_box);
    dimmed(base, card.into())
}

/// Dims `base` and shows `top` centred over it; clicks don't reach `base`.
fn dimmed<'a>(base: Element<'a, Message>, top: Element<'a, Message>) -> Element<'a, Message> {
    stack![
        base,
        opaque(center(top).style(|_theme| {
            container::Style {
                background: Some(
                    Color {
                        a: 0.6,
                        ..Color::BLACK
                    }
                    .into(),
                ),
                ..container::Style::default()
            }
        }))
    ]
    .into()
}

/// A file pane; the active one is outlined in the accent colour.
/// A click, key or right-click in a pane, which makes it the active one.
/// A message that means the user is working in that pane, so shortcuts go
/// there. The pointer crossing rows is not: a drag from the other pane
/// passing over must leave Escape with the pane it came from.
fn is_interaction(message: &explorer::Message) -> bool {
    match message {
        explorer::Message::Request(
            explorer::Request::PointerEntered(_) | explorer::Request::PointerLeft(_),
        ) => false,
        explorer::Message::Request(_)
        | explorer::Message::ShowMenu { .. }
        | explorer::Message::PressedAt { .. } => true,
        _ => false,
    }
}

/// The Mac paths of a Mac pane selection.
fn mac_paths(selection: &explorer::Selection) -> Vec<String> {
    selection
        .names
        .iter()
        .map(|name| {
            Path::new(&selection.path)
                .join(name)
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn pane(content: Element<'_, Message>, active: bool) -> Element<'_, Message> {
    container(content)
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .style(move |theme: &Theme| {
            let style = container::bordered_box(theme);
            if !active {
                return style;
            }
            container::Style {
                border: Border {
                    color: theme.palette().primary,
                    width: 1.5,
                    ..style.border
                },
                ..style
            }
        })
        .into()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use internal::dto;

    use super::*;
    use crate::handler::error::to_error_response;
    use crate::handler::testing::{
        FakeDevice, FakeExplorer, FakeSettings, FakeTransfer, file, folder, phone, storage,
    };

    fn app() -> App {
        app_on(Arc::new(FakeExplorer::new(Ok(vec![]))))
    }

    /// An app whose phone pane works through `phone_files`, so a test can see
    /// what the pane asked of it.
    fn app_on(phone_files: Arc<FakeExplorer>) -> App {
        app_with(phone_files, Arc::new(default_transfer()))
    }

    fn default_transfer() -> FakeTransfer {
        FakeTransfer::new(Ok(dto::TransferSummary {
            files: 1,
            bytes: 3,
            renamed: 0,
            destination: "/Users/me".into(),
            first_item: Some("a.jpg".into()),
            ..Default::default()
        }))
    }

    /// Like `app_on`, with the transfer service a test can inspect too.
    fn app_with(phone_files: Arc<FakeExplorer>, transfers: Arc<FakeTransfer>) -> App {
        app_with_panes(
            phone_files,
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            transfers,
        )
    }

    /// Like `app_with`, with the Mac pane's files a test can inspect too.
    fn app_with_panes(
        phone_files: Arc<FakeExplorer>,
        mac_files: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
    ) -> App {
        app_with_search(
            phone_files,
            mac_files,
            transfers,
            Arc::new(crate::handler::testing::FakeSearch::default()),
        )
    }

    /// Like `app_with_panes`, with the phone search a test can script.
    fn app_with_search(
        phone_files: Arc<FakeExplorer>,
        mac_files: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
        search: Arc<crate::handler::testing::FakeSearch>,
    ) -> App {
        // The storage the tests connect with, so that asking the phone again
        // after a transfer finds it where it was.
        let device = Arc::new(FakeDevice::with_storages(vec![storage(5, "Internal")]));
        app_built(device, phone_files, mac_files, transfers, search)
    }

    fn app_built(
        device: Arc<FakeDevice>,
        phone_files: Arc<FakeExplorer>,
        mac_files: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
        search: Arc<crate::handler::testing::FakeSearch>,
    ) -> App {
        app_built_with(
            device,
            phone_files,
            mac_files,
            transfers,
            search,
            Arc::new(FakeSettings::default()),
        )
    }

    /// `app_built`, with settings (and where the panes were left) from
    /// `settings`.
    fn app_built_with(
        device: Arc<FakeDevice>,
        phone_files: Arc<FakeExplorer>,
        mac_files: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
        search: Arc<crate::handler::testing::FakeSearch>,
        settings_service: Arc<FakeSettings>,
    ) -> App {
        App::new(handlers_for(
            device,
            phone_files,
            mac_files,
            transfers,
            search,
            settings_service,
            Arc::new(crate::handler::testing::FakeMenuBar::default()),
            Arc::new(local::RecordingLanguages::new(&[])),
        ))
        .0
    }

    #[allow(clippy::too_many_arguments)]
    fn handlers_for(
        device: Arc<FakeDevice>,
        phone_files: Arc<FakeExplorer>,
        mac_files: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
        search: Arc<crate::handler::testing::FakeSearch>,
        settings_service: Arc<FakeSettings>,
        menubar: Arc<crate::handler::testing::FakeMenuBar>,
        languages: Arc<local::RecordingLanguages>,
    ) -> Handlers {
        Handlers {
            import: import::Handler::new(Arc::new(crate::handler::testing::FakeImport::default())),
            space: freeing::Handler::new(Arc::new(crate::handler::testing::FakeSpace::default())),
            mirror: mirror::Handler::new(Arc::new(crate::handler::testing::FakeMirror::default())),
            preview: preview::Handler::new(Arc::new(
                crate::handler::testing::FakePreview::default(),
            )),
            crash: crash::Handler::new(Arc::new(crate::handler::testing::FakeCrash::default())),
            places: places::Handler::new(Arc::new(crate::handler::testing::FakePlaces::default())),
            info: info::Handler::new(Arc::new(crate::handler::testing::FakeInfo::counting(
                dto::Tally {
                    files: 4,
                    folders: 1,
                    bytes: 4_000,
                    ..dto::Tally::default()
                },
            ))),
            search: search::Handler::new(search),
            device: device::Handler::new(device),
            device_pane: explorer::Handler::new(phone_files),
            local_pane: explorer::Handler::new(mac_files),
            local_home: "/Users/me".into(),
            settings: settings::Handler::new(settings_service),
            help: help::Handler::new(Arc::new(crate::handler::testing::FakeHelp::default())),
            transfer: transfer::Handler::new(transfers),
            menubar: menubar::Handler::new(menubar),
            languages,
        }
    }

    fn settings_request(app: &mut App, request: settings::Request) {
        let _ = app.update(Message::Settings(settings::Message::Request(request)));
    }

    /// Connects, lists `a.jpg` in the device pane and selects it.
    fn select_on_device(app: &mut App) {
        connect(app, &[(5, "Internal")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
    }

    fn summary(destination: &str) -> dto::TransferSummary {
        dto::TransferSummary {
            files: 1,
            bytes: 3,
            renamed: 0,
            destination: destination.into(),
            first_item: Some("a.jpg".into()),
            ..Default::default()
        }
    }

    /// Feeds the device handler a successful connection with these storages.
    fn connect(app: &mut App, storages: &[(u64, &str)]) {
        let storages: Vec<dto::Storage> = storages
            .iter()
            .map(|(id, name)| storage(*id, name))
            .collect();
        let _ = app.update(Message::Device(device::Message::connected(
            &phone(),
            &storages,
        )));
    }

    #[test]
    fn starts_with_the_mac_pane_at_home_and_no_device_pane() {
        let app = app();

        assert_eq!(app.local_pane.location(), Some((MAC_VOLUME, "/Users/me")));
        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
    }

    #[test]
    fn connecting_opens_the_first_storage() {
        let mut app = app();

        connect(&mut app, &[(5, "Internal")]);

        assert_eq!(app.device_pane.location(), Some((5, "/")));
    }

    #[test]
    fn an_android_phones_app_folders_say_why_they_look_empty_and_another_phones_dont() {
        let shown_in = |device: dto::Device| {
            let mut app = app();
            let _ = app.update(Message::Device(device::Message::connected(
                &device,
                &[storage(5, "Internal")],
            )));
            let _ = app.update(Message::DevicePane(explorer::Message::Listed {
                volume_id: 5,
                path: "/".into(),
                result: Ok(vec![]),
            }));
            let _ = app.update(Message::DevicePane(explorer::Message::Request(
                explorer::Request::GoTo("/Android/data".into()),
            )));
            let _ = app.update(Message::DevicePane(explorer::Message::Listed {
                volume_id: 5,
                path: "/Android/data".into(),
                result: Ok(vec![]),
            }));
            let mut ui = iced_test::simulator(app.device_pane.view());
            ui.find("This folder is empty").is_ok()
        };

        assert!(!shown_in(phone()), "Android: it says why instead");
        assert!(shown_in(dto::Device {
            android: false,
            ..phone()
        }));
    }

    #[test]
    fn picking_another_storage_reopens_the_device_pane_there() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal"), (6, "SD card")]);

        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::SelectStorage(6),
        )));

        assert_eq!(app.device_pane.location(), Some((6, "/")));
    }

    #[test]
    fn re_picking_the_same_storage_keeps_the_folder() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::GoTo("/DCIM".into()),
        )));

        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::SelectStorage(5),
        )));

        assert_eq!(app.device_pane.location(), Some((5, "/DCIM")));
    }

    #[test]
    fn disconnecting_closes_the_device_pane_but_not_the_mac_pane() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::Device(device::Message::Disconnected));

        assert!(!app.device_pane.is_open());
        assert!(app.local_pane.is_open());
    }

    #[test]
    fn the_device_pane_losing_the_device_resets_the_device_side() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::Disconnected)),
        }));

        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
    }

    #[test]
    fn a_thumbnail_the_silent_phone_never_gave_resets_the_device_side() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let lost = |path: &str| {
            Message::DevicePane(explorer::Message::ThumbnailLost {
                volume_id: 5,
                path: path.into(),
                error: to_error_response(dto::Error::Timeout),
            })
        };

        // Thumbnails asked for in another folder, answered after the pane
        // moved on: not about the phone shown now.
        let _ = app.update(lost("/DCIM"));
        assert!(app.device_pane.is_open());

        let _ = app.update(lost("/"));
        assert!(!app.device_pane.is_open());
        assert!(!app.device.is_connected(), "Reconnect is offered");

        // Reconnected: the rest of that grid's late answers change nothing.
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(lost("/DCIM"));
        assert!(app.device_pane.is_open());
        assert!(app.device.is_connected());
    }

    #[test]
    fn a_device_edit_losing_the_device_resets_the_device_side() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::DevicePane(explorer::Message::Edited {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::Disconnected)),
            undo: None,
        }));

        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
    }

    #[test]
    fn the_phone_deletes_for_good_and_the_mac_uses_the_trash() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        assert_eq!(app.device_pane.deletes_permanently(), Some(true));
        assert_eq!(app.local_pane.deletes_permanently(), Some(false));
    }

    #[test]
    fn other_device_pane_errors_stay_in_the_pane() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::PathNotFound("/DCIM".into()))),
        }));

        assert!(app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), Some((5, "Internal".into())));
    }

    #[test]
    fn mac_pane_navigation_is_independent_of_the_device_pane() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Up,
        )));

        assert_eq!(app.local_pane.location(), Some((MAC_VOLUME, "/Users")));
        assert_eq!(app.device_pane.location(), Some((5, "/")));
    }

    #[test]
    fn mac_pane_errors_never_touch_the_device() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Err(to_error_response(dto::Error::NotConnected)),
        }));

        assert_eq!(app.device.selected_storage(), Some((5, "Internal".into())));
        assert!(app.device_pane.is_open());
    }

    #[test]
    fn the_mac_pane_can_be_hidden_and_moved() {
        let mut app = app();

        settings_request(&mut app, settings::Request::SetMacPaneOnLeft(false));
        assert!(!app.settings.current().mac_pane_on_left);

        settings_request(&mut app, settings::Request::SetShowMacPane(false));
        assert!(!app.settings.current().show_mac_pane);
    }

    #[test]
    fn renders_every_layout() {
        let mut app = app();
        let _ = app.view();

        connect(&mut app, &[(5, "Internal")]);
        let _ = app.view();

        settings_request(&mut app, settings::Request::SetMacPaneOnLeft(false));
        let _ = app.view();

        settings_request(&mut app, settings::Request::SetShowMacPane(false));
        let _ = app.view();

        settings_request(&mut app, settings::Request::Open);
        let _ = app.view();
    }

    #[test]
    fn download_copies_the_device_selection_into_the_mac_folder() {
        let mut app = app();
        select_on_device(&mut app);

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        assert!(app.transfer.is_running());
    }

    #[test]
    fn download_without_a_selection_does_nothing() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        assert!(!app.transfer.is_visible());
    }

    #[test]
    fn a_finished_download_refreshes_the_mac_folder_it_went_to() {
        let mut app = app();

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Ok(summary(
            "/Users/me",
        )))));
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.jpg")]),
        }));

        assert_eq!(
            app.local_pane.selection().map(|s| s.names),
            Some(vec!["a.jpg".into()])
        );
    }

    #[test]
    fn a_download_to_another_folder_leaves_the_mac_pane_alone() {
        let mut app = app();

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Ok(summary(
            "/Users/me/Elsewhere",
        )))));

        assert_eq!(app.local_pane.selection(), None);
    }

    #[test]
    fn a_download_losing_the_device_resets_the_device_side() {
        let mut app = app();
        select_on_device(&mut app);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));

        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
    }

    #[test]
    fn renders_with_the_transfer_panel() {
        let mut app = app();
        select_on_device(&mut app);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        let _ = app.view();
    }

    /// Lists and selects `notes.txt` in the Mac pane.
    fn select_on_mac(app: &mut App) {
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        }));
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
    }

    #[test]
    fn upload_copies_the_mac_selection_into_the_phone_folder() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);
        select_on_mac(&mut app);

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        assert!(app.transfer.is_running());
    }

    #[test]
    fn upload_without_a_phone_does_nothing() {
        let mut app = app();
        select_on_mac(&mut app);

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        assert!(!app.transfer.is_visible());
    }

    #[test]
    fn upload_is_offered_while_a_phone_storage_is_open() {
        let mut app = app();
        assert_eq!(app.local_pane.transfer_action(), None);

        connect(&mut app, &[(5, "Internal")]);
        assert_eq!(app.local_pane.transfer_action(), Some(UPLOAD_ACTION));

        let _ = app.update(Message::Device(device::Message::Disconnected));
        assert_eq!(app.local_pane.transfer_action(), None);
    }

    #[test]
    fn losing_the_phone_during_a_transfer_withdraws_upload() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));

        assert_eq!(app.local_pane.transfer_action(), None);
    }

    #[test]
    fn a_finished_upload_refreshes_the_phone_folder_it_went_to() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Ok(
            dto::TransferSummary {
                direction: dto::Direction::Upload,
                ..summary("/")
            },
        ))));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));

        assert_eq!(
            app.device_pane.selection().map(|s| s.names),
            Some(vec!["a.jpg".into()])
        );
    }

    #[test]
    fn an_upload_to_another_folder_leaves_the_phone_pane_alone() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Ok(
            dto::TransferSummary {
                direction: dto::Direction::Upload,
                ..summary("/DCIM")
            },
        ))));

        assert_eq!(app.device_pane.selection(), None);
    }

    use iced::keyboard::key::{Code, Named, Physical};
    use iced::keyboard::{Key, Location};

    fn key_press(key: Key, code: Code, modifiers: Modifiers) -> Message {
        Message::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Code(code),
            location: Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    fn press(app: &mut App, key: Key, code: Code, modifiers: Modifiers) {
        let _ = app.update(key_press(key, code, modifiers));
    }

    fn command(app: &mut App, code: Code) {
        press(app, Key::Character("x".into()), code, Modifiers::COMMAND);
    }

    /// Mac pane lists notes.txt and song.mp3; phone pane lists a.jpg.
    fn two_listed_panes() -> App {
        two_listed_panes_on(Arc::new(FakeExplorer::new(Ok(vec![]))))
    }

    fn two_listed_panes_on(phone_files: Arc<FakeExplorer>) -> App {
        two_listed_panes_with(phone_files, Arc::new(default_transfer()))
    }

    fn two_listed_panes_with(phone_files: Arc<FakeExplorer>, transfers: Arc<FakeTransfer>) -> App {
        let mut app = app_with(phone_files, transfers);
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt"), file("song.mp3")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        app
    }

    #[tokio::test]
    async fn edit_on_mac_fetches_the_one_phone_file_selected_and_its_send_needs_that_phone() {
        let transfers = Arc::new(FakeTransfer {
            edit: Ok(dto::WorkingCopy {
                id: "c0ffee".into(),
                phone: "ABC123".into(),
                storage_id: 5,
                folder: "/".into(),
                name: String::new(),
                path: "/support/editing/c0ffee/a.jpg".into(),
                fetched: dto::CopyState::default(),
                phone_size: 1,
                phone_date: String::new(),
            }),
            ..default_transfer()
        });
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        assert_eq!(
            app.transfer.open_phone(),
            Some("ABC123"),
            "told which phone is open"
        );
        assert!(!app.menu_state().can_edit, "nothing selected");
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        assert!(app.menu_state().can_edit);

        let messages = sent_by(
            &mut app,
            Message::DevicePane(explorer::Message::Request(explorer::Request::Menu(
                explorer::MenuAction::EditOnMac,
            ))),
        )
        .await;
        for message in messages {
            let _ = app.update(message);
        }

        assert_eq!(
            *transfers.edit_requests.lock().unwrap(),
            [dto::EditRequest {
                phone: "ABC123".into(),
                storage_id: 5,
                folder: "/".into(),
                name: "a.jpg".into(),
            }]
        );
        assert!(app.transfer.has_edits(), "watched from now on");
        assert!(
            iced_test::simulator(app.view())
                .find("a.jpg is open for editing on the Mac")
                .is_ok()
        );

        // The phone gone, nothing can be sent: it is told so.
        let _ = app.update(Message::Device(device::Message::Disconnected));
        assert_eq!(app.transfer.open_phone(), None);
        assert!(app.transfer.has_edits(), "the copy waits for its phone");
    }

    #[test]
    fn edit_on_mac_is_not_for_a_mac_file_or_a_phone_folder() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
        assert!(!app.menu_state().can_edit, "the Mac pane is active");
        let _ = choose_from_menu(&mut app, menubar::Command::EditOnMac);
        assert!(!app.transfer.has_edits());
    }

    #[test]
    fn shortcuts_go_to_the_mac_pane_first() {
        let mut app = two_listed_panes();

        command(&mut app, Code::KeyA);

        assert_eq!(app.local_pane.selection().unwrap().names.len(), 2);
        assert_eq!(app.device_pane.selection(), None);
    }

    #[test]
    fn shift_command_g_goes_to_a_folder_picked_with_the_arrow_keys() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![
                folder("Desktop"),
                folder("Documents"),
                file("notes.txt"),
            ]),
        }));

        press(
            &mut app,
            Key::Character("g".into()),
            Code::KeyG,
            Modifiers::COMMAND | Modifiers::SHIFT,
        );
        assert!(app.local_pane.is_going_to(), "in the pane the keys go to");
        assert!(!app.device_pane.is_editing());
        press(
            &mut app,
            Key::Named(Named::ArrowDown),
            Code::ArrowDown,
            Modifiers::empty(),
        );
        press(
            &mut app,
            Key::Named(Named::ArrowDown),
            Code::ArrowDown,
            Modifiers::empty(),
        );
        // Other keys don't reach the files behind the dialog.
        press(
            &mut app,
            Key::Named(Named::Backspace),
            Code::Backspace,
            Modifiers::COMMAND,
        );
        assert!(app.local_pane.is_going_to());
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Submit,
        )));

        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Documents"));
    }

    #[tokio::test]
    async fn option_command_c_copies_the_mac_paths_of_the_selection() {
        let mut app = two_listed_panes();
        select_on_mac(&mut app);

        let task = app.update(key_press(
            Key::Character("ç".into()),
            Code::KeyC,
            Modifiers::COMMAND | Modifiers::ALT,
        ));
        let (_, copied) = crate::handler::testing::drain(task).await;

        assert_eq!(copied, ["/Users/me/notes.txt"]);
    }

    #[test]
    fn control_command_n_puts_the_selection_in_a_new_folder_in_that_pane() {
        let mut app = two_listed_panes();
        select_on_mac(&mut app);

        press(
            &mut app,
            Key::Character("n".into()),
            Code::KeyN,
            Modifiers::COMMAND | Modifiers::CTRL,
        );

        assert!(app.local_pane.is_pasting(), "the folder is being made");
        assert!(!app.device_pane.is_pasting());
    }

    #[test]
    fn a_usual_place_opens_in_the_phone_pane() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::GoToPlace("/DCIM/Camera".into()),
        )));

        assert_eq!(app.device_pane.current_path(), Some("/DCIM/Camera"));
        assert_eq!(
            app.local_pane.current_path(),
            Some("/Users/me"),
            "the Mac pane stays"
        );
    }

    #[test]
    fn clicking_a_pane_makes_it_the_one_shortcuts_go_to() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Refresh,
        )));

        command(&mut app, Code::KeyA);

        assert_eq!(app.device_pane.selection().unwrap().names, ["a.jpg"]);
        assert_eq!(app.local_pane.selection(), None);
    }

    #[test]
    fn command_one_switches_panes_unless_the_mac_pane_is_hidden() {
        let mut app = two_listed_panes();
        command(&mut app, Code::Digit1);
        assert_eq!(app.target(), Side::Phone);
        command(&mut app, Code::Digit1);
        assert_eq!(app.target(), Side::Mac);

        settings_request(&mut app, settings::Request::SetShowMacPane(false));
        assert_eq!(app.target(), Side::Phone);
        command(&mut app, Code::Digit1);
        assert_eq!(app.target(), Side::Phone);
    }

    #[test]
    fn held_modifiers_reach_both_panes() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));

        let _ = app.update(Message::Keyboard(keyboard::Event::ModifiersChanged(
            Modifiers::COMMAND,
        )));
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("song.mp3".into()),
        )));

        assert_eq!(app.local_pane.selection().unwrap().names.len(), 2);
    }

    #[test]
    fn copy_on_the_phone_and_paste_on_the_mac_downloads() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        command(&mut app, Code::KeyC);

        command(&mut app, Code::Digit1);
        command(&mut app, Code::KeyV);

        assert!(app.transfer.is_running());
    }

    #[test]
    fn copy_on_the_mac_and_paste_on_the_phone_uploads() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
        command(&mut app, Code::KeyC);

        command(&mut app, Code::Digit1);
        command(&mut app, Code::KeyV);

        assert!(app.transfer.is_running());
    }

    #[test]
    fn pasting_into_the_same_side_or_with_nothing_copied_does_nothing() {
        let mut app = two_listed_panes();
        command(&mut app, Code::KeyV);
        command(&mut app, Code::KeyC);

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
        command(&mut app, Code::KeyC);
        command(&mut app, Code::KeyV);

        assert!(!app.transfer.is_visible());
    }

    #[test]
    fn only_escape_reaches_a_pane_with_an_open_dialog() {
        let mut app = two_listed_panes();
        command(&mut app, Code::KeyN);
        assert!(app.local_pane.is_editing());

        command(&mut app, Code::KeyA);
        press(
            &mut app,
            Key::Named(Named::Backspace),
            Code::Backspace,
            Modifiers::empty(),
        );
        assert!(app.local_pane.is_editing());
        assert_eq!(app.local_pane.selection(), None);

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );
        assert!(!app.local_pane.is_editing());
    }

    #[test]
    fn unmapped_keys_and_releases_do_nothing() {
        let mut app = two_listed_panes();
        press(
            &mut app,
            Key::Character("q".into()),
            Code::KeyQ,
            Modifiers::empty(),
        );
        let _ = app.update(Message::Keyboard(keyboard::Event::KeyReleased {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::COMMAND,
        }));

        assert_eq!(app.local_pane.selection(), None);
        let _ = app.subscription();
    }

    #[test]
    fn renders_with_either_pane_active() {
        let mut app = two_listed_panes();
        let _ = app.view();
        command(&mut app, Code::Digit1);
        let _ = app.view();
    }

    #[test]
    fn hovering_files_shows_where_a_drop_will_go() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::Drop(FileDrop::Hovered));
        assert!(app.drop_hovering);
        assert_eq!(app.drop_hint(), "Drop to upload into the top folder");
        let _ = app.view();

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::GoTo("/DCIM".into()),
        )));
        assert_eq!(app.drop_hint(), "Drop to upload into DCIM");

        let _ = app.update(Message::Drop(FileDrop::Left));
        assert!(!app.drop_hovering);
    }

    #[test]
    fn the_hint_explains_when_a_drop_cannot_upload_or_will_queue() {
        let app = app();
        assert_eq!(app.drop_hint(), "Connect a phone to drop files here");

        let mut app = two_listed_panes();
        select_on_mac(&mut app);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));
        assert_eq!(
            app.drop_hint(),
            "Drop to queue an upload into the top folder"
        );

        let mut app = two_listed_panes();
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });
        for message in [
            transfer::Message::Request(transfer::Request::Pause),
            transfer::Message::Finished(Err(to_error_response(dto::Error::Cancelled))),
        ] {
            let _ = app.update(Message::Transfer(message));
        }
        assert!(app.transfer.is_paused());
        assert_eq!(
            app.drop_hint(),
            "Drop to queue an upload into the top folder",
            "it waits behind the paused one"
        );
    }

    #[test]
    fn files_of_one_drop_become_one_upload() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::Drop(FileDrop::Dropped("/Users/me/a.jpg".into())));
        let _ = app.update(Message::Drop(FileDrop::Dropped("/Users/me/b.jpg".into())));
        assert_eq!(app.dropped.len(), 2);
        assert!(!app.drop_hovering);

        let _ = app.update(Message::DropSettled);

        assert!(app.dropped.is_empty());
        assert!(app.transfer.is_running());
        assert_eq!(app.target(), Side::Phone);
    }

    #[test]
    fn a_drop_without_a_phone_does_nothing() {
        let mut app = app();

        let _ = app.update(Message::Drop(FileDrop::Dropped("/Users/me/a.jpg".into())));
        let _ = app.update(Message::DropSettled);

        assert!(!app.transfer.is_visible());
        assert!(app.dropped.is_empty());
    }

    #[test]
    fn a_settle_with_nothing_dropped_does_nothing() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::DropSettled);

        assert!(!app.transfer.is_visible());
    }

    #[test]
    fn only_file_drag_events_are_kept() {
        let hovered = iced::Event::Window(window::Event::FileHovered("/a".into()));
        let dropped = iced::Event::Window(window::Event::FileDropped("/a".into()));
        let left = iced::Event::Window(window::Event::FilesHoveredLeft);
        let other = iced::Event::Window(window::Event::Focused);
        let status = iced::event::Status::Ignored;
        let id = window::Id::unique();

        assert!(matches!(
            file_drop(hovered, status, id),
            Some(Message::Drop(FileDrop::Hovered))
        ));
        assert!(matches!(
            file_drop(dropped, status, id),
            Some(Message::Drop(FileDrop::Dropped(p))) if p.as_os_str() == "/a"
        ));
        assert!(matches!(
            file_drop(left, status, id),
            Some(Message::Drop(FileDrop::Left))
        ));
        assert!(file_drop(other, status, id).is_none());
    }

    /// Settings that load as `start`.
    struct Saved(dto::Settings);

    impl settings::Service for Saved {
        fn load(&self) -> dto::Settings {
            self.0
        }

        fn save(&self, _: &dto::Settings) -> Result<(), dto::Error> {
            Ok(())
        }
    }

    #[test]
    fn saved_settings_are_applied_at_start() {
        let mut app = app();
        app.settings = settings::Handler::new(Arc::new(Saved(dto::Settings {
            show_hidden_on_mac: true,
            folders_first: false,
            status_bar: false,
            ..dto::Settings::default()
        })));

        let _ = app.apply_settings();

        assert_eq!(app.local_pane.listing_options(), (true, false, false));
        assert_eq!(app.device_pane.listing_options(), (false, false, false));
    }

    #[test]
    fn pane_footer_toggles_become_settings() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::SetShowHidden(true),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::SetShowHidden(true),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::SetFoldersFirst(false),
        )));

        let settings = app.settings.current();
        assert!(settings.show_hidden_on_mac && settings.show_hidden_on_phone);
        assert!(!settings.folders_first);
        // Folders first is one setting for both panes.
        assert_eq!(app.local_pane.listing_options(), (true, false, true));
        assert_eq!(app.device_pane.listing_options(), (true, false, true));
    }

    #[test]
    fn the_settings_dialog_changes_the_panes() {
        let mut app = two_listed_panes();

        settings_request(&mut app, settings::Request::SetStatusBar(false));
        settings_request(&mut app, settings::Request::SetShowHiddenOnPhone(true));

        assert_eq!(app.device_pane.listing_options(), (true, true, false));
        assert_eq!(app.local_pane.listing_options(), (false, true, false));
    }

    #[test]
    fn the_row_size_from_settings_reaches_both_panes() {
        let mut app = two_listed_panes();

        settings_request(
            &mut app,
            settings::Request::SetDensity(dto::Density::Compact),
        );

        assert_eq!(app.device_pane.density(), dto::Density::Compact);
        assert_eq!(app.local_pane.density(), dto::Density::Compact);
        assert_eq!(app.settings.current().list_density, dto::Density::Compact);
    }

    #[test]
    fn command_comma_opens_settings_and_escape_closes_it() {
        let mut app = two_listed_panes();

        command(&mut app, Code::Comma);
        assert!(app.settings.is_open());
        let _ = app.view();

        command(&mut app, Code::KeyA);
        assert_eq!(
            app.local_pane.selection(),
            None,
            "shortcuts wait while Settings is open"
        );

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );
        assert!(!app.settings.is_open());

        command(&mut app, Code::Comma);
        command(&mut app, Code::Comma);
        assert!(!app.settings.is_open());
    }

    #[test]
    fn the_theme_follows_the_choice_or_the_mac() {
        let mut app = app();
        assert_eq!(app.theme(), Theme::Dark);

        let _ = app.update(Message::SystemTheme(theme::Mode::Light));
        assert_eq!(app.theme(), Theme::Light);

        settings_request(
            &mut app,
            settings::Request::SetTheme(dto::ThemeChoice::Dark),
        );
        assert_eq!(app.theme(), Theme::Dark);

        let _ = app.update(Message::SystemTheme(theme::Mode::Dark));
        settings_request(
            &mut app,
            settings::Request::SetTheme(dto::ThemeChoice::Light),
        );
        assert_eq!(app.theme(), Theme::Light);
    }

    fn phone_changed(app: &mut App) {
        let _ = app.update(Message::Device(device::Message::Changed(
            dto::ContentChange::Files,
        )));
    }

    #[test]
    fn phone_changes_refresh_the_phone_pane_once_they_settle() {
        let mut app = two_listed_panes();

        phone_changed(&mut app);
        phone_changed(&mut app);
        assert!(app.refresh_pending);
        assert!(!app.device_pane.is_loading());

        let _ = app.update(Message::PhoneChangesSettled);

        assert!(!app.refresh_pending);
        assert!(app.device_pane.is_loading());
    }

    #[test]
    fn phone_changes_during_a_transfer_wait_for_it() {
        let mut app = two_listed_panes();
        select_on_mac(&mut app);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));
        phone_changed(&mut app);

        let _ = app.update(Message::PhoneChangesSettled);

        assert!(!app.device_pane.is_loading());
    }

    #[test]
    fn the_auto_connect_setting_reaches_the_device() {
        let mut app = app();
        // The app starts by connecting; settle it in the idle state first.
        let _ = app.update(Message::Device(device::Message::Disconnected));
        settings_request(&mut app, settings::Request::SetAutoConnect(false));

        let _ = app.update(Message::Device(device::Message::Hotplug(
            dto::DeviceChange::Plugged { serial: None },
        )));

        assert!(app.device.is_idle());
    }

    #[tokio::test]
    async fn keeping_deleted_files_reaches_the_phone_pane_and_its_deletes_go_to_the_queue() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        assert!(!app.device_pane.keeps_deleted(), "off unless asked for");
        settings_request(&mut app, settings::Request::SetKeepDeleted(true));
        assert!(app.device_pane.keeps_deleted());

        let request = dto::DeleteRequest {
            storage_id: 5,
            folder: "/".into(),
            names: vec!["a.jpg".into()],
        };
        let messages = sent_by(
            &mut app,
            Message::DevicePane(explorer::Message::DeleteKeepingCopy(request.clone())),
        )
        .await;
        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(*transfers.deletes.lock().unwrap(), [request]);
        assert!(app.transfer.is_visible(), "the queue shows it");
    }

    #[test]
    fn control_command_0_groups_the_active_pane_by_date_or_not_and_is_kept() {
        let mut app = two_listed_panes();
        let press = |app: &mut App| {
            app.update(key_press(
                keyboard::Key::Character("0".into()),
                Code::Digit0,
                Modifiers::COMMAND | Modifiers::CTRL,
            ))
        };

        let _ = press(&mut app);
        assert_eq!(app.settings.current().mac_group, dto::GroupBy::Date);
        assert_eq!(app.local_pane.group_by(), dto::GroupBy::Date);
        assert_eq!(
            app.device_pane.group_by(),
            dto::GroupBy::None,
            "only the active pane"
        );
        assert_eq!(app.menu_state().group_by, dto::GroupBy::Date);
        let _ = press(&mut app);
        assert_eq!(app.settings.current().mac_group, dto::GroupBy::None);

        let _ = choose_from_menu(&mut app, menubar::Command::GroupBy(dto::GroupBy::Kind));
        assert_eq!(app.settings.current().mac_group, dto::GroupBy::Kind);
        command(&mut app, Code::Digit1);
        let _ = choose_from_menu(&mut app, menubar::Command::GroupBy(dto::GroupBy::Size));
        assert_eq!(app.settings.current().phone_group, dto::GroupBy::Size);
        assert_eq!(app.device_pane.group_by(), dto::GroupBy::Size);
    }

    #[test]
    fn a_kind_in_get_info_finds_those_files_on_the_side_it_was_about() {
        let mut app = two_listed_panes();
        let _ = app.open_info(Side::Phone, false);
        assert!(app.info.is_about_the_phone());
        let _ = app.update(Message::Info(info::Message::Request(
            info::Request::ShowKind(dto::SpaceKind::Videos),
        )));
        assert!(!app.info.is_open());
        assert!(app.search.is_open(), "the phone's search");
        assert_eq!(app.search.kind(), Some(dto::FileKind::Video));

        let mut app = two_listed_panes();
        let _ = app.open_info(Side::Mac, false);
        let _ = app.update(Message::Info(info::Message::Request(
            info::Request::ShowKind(dto::SpaceKind::Photos),
        )));
        assert_eq!(app.local_pane.filter_kind(), Some(dto::FileKind::Photo));
        assert_eq!(app.active, Side::Mac);

        // Apps and the rest have no kind to find by: the dialog just goes.
        let mut app = two_listed_panes();
        let _ = app.open_info(Side::Mac, false);
        let _ = app.update(Message::Info(info::Message::Request(
            info::Request::ShowKind(dto::SpaceKind::Apps),
        )));
        assert!(!app.info.is_open());
        assert_eq!(app.local_pane.filter_kind(), None);
    }

    #[test]
    fn the_finish_sound_setting_reaches_the_transfers() {
        let mut app = app();
        assert!(!app.transfer.plays_sound(), "off unless asked for");

        settings_request(&mut app, settings::Request::SetFinishSound(true));
        assert!(app.transfer.plays_sound());
        settings_request(&mut app, settings::Request::SetFinishSound(false));
        assert!(!app.transfer.plays_sound());
    }

    #[test]
    fn each_pane_starts_in_its_saved_view_and_the_toggle_saves() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::SetView(dto::ViewMode::Grid),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::SetView(dto::ViewMode::List),
        )));

        let settings = app.settings.current();
        assert_eq!(settings.mac_view, dto::ViewMode::Grid);
        assert_eq!(settings.phone_view, dto::ViewMode::List);
        assert_eq!(app.target(), Side::Phone);
        let _ = app.view();
    }

    #[test]
    fn the_phone_pane_starts_as_a_grid_and_the_mac_pane_as_a_list() {
        let mut app = app();
        assert_eq!(app.device_pane.view_mode(), dto::ViewMode::Grid);

        connect(&mut app, &[(5, "Internal")]);

        assert_eq!(app.device_pane.view_mode(), dto::ViewMode::Grid);
        assert_eq!(app.local_pane.view_mode(), dto::ViewMode::List);
    }

    /// Lists `folder` in the phone pane with `names`, then selects `pick`.
    fn select_on_phone(app: &mut App, folder: &str, names: &[&str], pick: &str) {
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::GoTo(folder.into()),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: folder.into(),
            result: Ok(names.iter().map(|n| file(n)).collect()),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select(pick.into()),
        )));
    }

    fn copy_to_queue(app: &mut App) {
        press(
            app,
            Key::Character("c".into()),
            Code::KeyC,
            Modifiers::COMMAND | Modifiers::SHIFT,
        );
    }

    #[test]
    fn copy_to_queue_collects_from_several_folders_and_paste_queues_each() {
        let mut app = two_listed_panes();
        select_on_phone(&mut app, "/DCIM", &["a.jpg"], "a.jpg");
        command(&mut app, Code::KeyC);
        select_on_phone(&mut app, "/Download", &["b.pdf"], "b.pdf");
        copy_to_queue(&mut app);

        let Some(Clipboard {
            side: Side::Phone,
            selections,
            cut: false,
        }) = &app.clipboard
        else {
            panic!("the clipboard holds the phone selections, copied");
        };
        assert_eq!(selections.len(), 2);

        command(&mut app, Code::Digit1);
        command(&mut app, Code::KeyV);

        assert!(app.transfer.is_running());
        let _ = app.view();
    }

    #[test]
    fn copy_to_queue_from_the_other_side_starts_a_new_clipboard() {
        let mut app = two_listed_panes();
        select_on_phone(&mut app, "/DCIM", &["a.jpg"], "a.jpg");
        copy_to_queue(&mut app);

        command(&mut app, Code::Digit1);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
        copy_to_queue(&mut app);

        assert!(matches!(
            &app.clipboard,
            Some(Clipboard { side: Side::Mac, selections, .. }) if selections.len() == 1
        ));
    }

    #[test]
    fn copy_to_queue_with_nothing_selected_does_nothing() {
        let mut app = two_listed_panes();

        copy_to_queue(&mut app);

        assert!(app.clipboard.is_none());
    }

    #[test]
    fn a_drop_while_a_transfer_runs_is_queued() {
        let mut app = two_listed_panes();
        select_on_mac(&mut app);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        let _ = app.update(Message::Drop(FileDrop::Dropped("/Users/me/a.jpg".into())));
        let _ = app.update(Message::DropSettled);

        assert!(app.transfer.is_running());
        let _ = app.view();
    }
    /// A right-click in `side`'s list, on `entry` or on empty space.
    fn right_click(app: &mut App, side: Side, entry: Option<&str>) {
        let wrap = |message| match side {
            Side::Mac => Message::LocalPane(message),
            Side::Phone => Message::DevicePane(message),
        };
        if let Some(name) = entry {
            let _ = app.update(wrap(explorer::Message::Request(explorer::Request::MenuOn(
                name.into(),
            ))));
        }
        let _ = app.update(wrap(explorer::Message::ShowMenu {
            at: iced::Point::new(300.0, 200.0),
            area: iced::Rectangle::new(iced::Point::ORIGIN, iced::Size::new(800.0, 600.0)),
        }));
    }

    fn choose(app: &mut App, side: Side, action: explorer::MenuAction) {
        let message = explorer::Message::Request(explorer::Request::Menu(action));
        let _ = app.update(match side {
            Side::Mac => Message::LocalPane(message),
            Side::Phone => Message::DevicePane(message),
        });
    }

    #[tokio::test]
    async fn share_from_the_right_click_menu_shows_beside_the_click() {
        let mut app = two_listed_panes();
        let fake = with_preview(&mut app);

        right_click(&mut app, Side::Mac, Some("notes.txt"));
        choose(&mut app, Side::Mac, explorer::MenuAction::Share);
        assert!(!app.local_pane.has_menu(), "the menu closes");

        right_click(&mut app, Side::Phone, Some("a.jpg"));
        let task = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Menu(explorer::MenuAction::Share),
        )));
        assert!(
            app.preview.is_fetching(),
            "the phone's file is fetched first"
        );
        assert_eq!(app.target(), Side::Phone);
        feed_back(&mut app, task).await;

        assert_eq!(
            *fake.shared.lock().unwrap(),
            [
                (vec!["/Users/me/notes.txt".to_owned()], Some((300.0, 200.0))),
                (vec!["/tmp/preview/a.jpg".to_owned()], Some((300.0, 200.0)))
            ]
        );
        assert_eq!(
            *fake.fetches.lock().unwrap(),
            [dto::PreviewRequest {
                storage_id: 5,
                folder: "/".into(),
                names: vec!["a.jpg".into()],
            }]
        );
        assert!(fake.looks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn file_share_shares_the_active_panes_selection_and_nothing_without_one() {
        let mut app = two_listed_panes();
        let fake = with_preview(&mut app);
        let _ = choose_from_menu(&mut app, menubar::Command::Share);
        assert!(fake.shared.lock().unwrap().is_empty(), "nothing selected");

        select(&mut app, Side::Mac, "notes.txt");
        let _ = choose_from_menu(&mut app, menubar::Command::Share);

        assert_eq!(
            *fake.shared.lock().unwrap(),
            [(vec!["/Users/me/notes.txt".to_owned()], None)],
            "no click to show beside: the window's middle"
        );
        select(&mut app, Side::Phone, "a.jpg");
        settle(
            &mut app,
            Message::DevicePane(explorer::Message::Request(explorer::Request::Search)),
        )
        .await;
        app.active = Side::Phone;
        let _ = choose_from_menu(&mut app, menubar::Command::Share);
        assert!(
            !app.preview.is_fetching(),
            "the search stands in for the phone pane: nothing of it is shown"
        );
    }

    #[test]
    fn a_right_click_makes_its_pane_active_and_closes_the_other_menu() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("notes.txt"));
        assert!(app.local_pane.has_menu());

        right_click(&mut app, Side::Phone, Some("a.jpg"));

        assert_eq!(app.target(), Side::Phone);
        assert!(app.device_pane.has_menu());
        assert!(!app.local_pane.has_menu());
        let _ = app.view();

        right_click(&mut app, Side::Mac, None);
        assert!(app.local_pane.has_menu());
        assert!(!app.device_pane.has_menu());
        let _ = app.view();
    }

    #[test]
    fn menu_copy_on_the_phone_then_menu_paste_on_the_mac_downloads() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Phone, Some("a.jpg"));
        choose(&mut app, Side::Phone, explorer::MenuAction::Copy);
        assert!(!app.device_pane.has_menu());
        assert!(app.can_paste());
        assert!(app.can_paste(), "a copy can be pasted on the phone too");

        right_click(&mut app, Side::Mac, None);
        choose(&mut app, Side::Mac, explorer::MenuAction::Paste);

        assert!(app.transfer.is_running());
        assert!(!app.local_pane.has_menu());
    }

    #[test]
    fn menu_copy_to_queue_adds_to_the_clipboard() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("notes.txt"));
        choose(&mut app, Side::Mac, explorer::MenuAction::Copy);
        right_click(&mut app, Side::Mac, Some("song.mp3"));
        choose(&mut app, Side::Mac, explorer::MenuAction::CopyToQueue);

        let Some(Clipboard {
            side: Side::Mac,
            selections,
            ..
        }) = &app.clipboard
        else {
            panic!("the clipboard holds the Mac selections");
        };
        assert_eq!(selections.len(), 2);
    }

    #[test]
    fn menu_transfer_uploads_the_right_clicked_file() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("song.mp3"));

        choose(&mut app, Side::Mac, explorer::MenuAction::Transfer);

        assert!(app.transfer.is_running());
        assert!(!app.local_pane.has_menu());
    }

    #[test]
    fn menu_transfer_on_the_phone_downloads() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Phone, Some("a.jpg"));

        choose(&mut app, Side::Phone, explorer::MenuAction::Transfer);

        assert!(app.transfer.is_running());
    }

    #[test]
    fn pane_commands_from_the_menu_reach_the_pane() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, None);

        choose(&mut app, Side::Mac, explorer::MenuAction::SelectAll);

        assert_eq!(app.local_pane.selection().unwrap().names.len(), 2);
        assert!(!app.local_pane.has_menu());
    }

    #[test]
    fn escape_only_closes_the_menu() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("notes.txt"));

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert!(!app.local_pane.has_menu());
        assert_eq!(app.local_pane.selection().unwrap().names, ["notes.txt"]);
    }

    #[test]
    fn another_shortcut_closes_the_menu_and_still_runs() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("notes.txt"));

        command(&mut app, Code::KeyA);

        assert!(!app.local_pane.has_menu());
        assert_eq!(app.local_pane.selection().unwrap().names.len(), 2);
    }

    #[test]
    fn a_click_on_the_backdrop_closes_the_menu() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Phone, Some("a.jpg"));

        let mut ui = iced_test::simulator(app.view());
        ui.point_at(iced::Point::new(5.0, 5.0));
        let _ = ui.simulate(iced_test::simulator::click());
        let messages: Vec<_> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }

        assert!(!app.device_pane.has_menu());
    }
    /// Right-clicks the text `at` (or `point`) in the whole window and
    /// applies what that sends.
    fn right_click_in_window(app: &mut App, at: Option<&str>, point: iced::Point) {
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(app.view());
            let point = match at {
                Some(label) => ui.find(label).unwrap().visible_bounds().unwrap().center(),
                None => point,
            };
            ui.point_at(point);
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
                iced::mouse::Button::Right,
            ))]);
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = app.update(message);
        }
    }

    #[test]
    fn a_right_click_elsewhere_moves_the_menu_there() {
        let mut app = two_listed_panes();
        // song.mp3 first: the menu opens below the cursor and would cover it.
        right_click_in_window(&mut app, Some("song.mp3"), iced::Point::ORIGIN);
        assert!(app.local_pane.has_menu());

        right_click_in_window(&mut app, Some("notes.txt"), iced::Point::ORIGIN);

        assert!(app.local_pane.has_menu(), "reopened on the second click");
        assert_eq!(app.local_pane.selection().unwrap().names, ["notes.txt"]);

        right_click_in_window(&mut app, Some("a.jpg"), iced::Point::ORIGIN);

        assert!(app.device_pane.has_menu());
        assert!(!app.local_pane.has_menu());
    }

    #[test]
    fn a_right_click_outside_the_panes_just_closes_the_menu() {
        let mut app = two_listed_panes();
        right_click_in_window(&mut app, Some("notes.txt"), iced::Point::ORIGIN);

        right_click_in_window(&mut app, None, iced::Point::new(5.0, 5.0));

        assert!(!app.local_pane.has_menu());
        assert!(!app.device_pane.has_menu());
    }

    #[test]
    fn a_download_where_the_phone_stops_answering_resets_the_device_side() {
        let mut app = app();
        select_on_device(&mut app);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Timeout),
        ))));

        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
        // The device panel offers to reconnect.
        let mut ui = iced_test::simulator(app.device.view_main());
        assert!(ui.find("Reconnect").is_ok());
    }

    fn help_is_open(app: &App) -> bool {
        app.help.is_open()
    }

    /// Runs the work a message started and feeds the results back in.
    async fn settle(app: &mut App, message: Message) {
        let (messages, _) = crate::handler::testing::drain(app.update(message)).await;
        for message in messages {
            let _ = app.update(message);
        }
    }

    fn help_request(request: help::Request) -> Message {
        Message::Help(help::Message::Request(request))
    }

    #[test]
    fn command_slash_opens_help_and_again_closes_it() {
        let mut app = two_listed_panes();

        command(&mut app, Code::Slash);
        assert!(help_is_open(&app));

        command(&mut app, Code::Slash);
        assert!(!help_is_open(&app));
    }

    #[test]
    fn command_question_mark_opens_help_too() {
        let mut app = two_listed_panes();

        press(
            &mut app,
            Key::Character("?".into()),
            Code::Slash,
            Modifiers::COMMAND | Modifiers::SHIFT,
        );

        assert!(help_is_open(&app));
    }

    #[test]
    fn escape_closes_help() {
        let mut app = two_listed_panes();
        command(&mut app, Code::Slash);

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert!(!help_is_open(&app));
    }

    #[test]
    fn while_help_is_open_other_shortcuts_do_nothing() {
        let mut app = two_listed_panes();
        command(&mut app, Code::Slash);

        command(&mut app, Code::KeyA);
        command(&mut app, Code::Comma);

        assert!(help_is_open(&app));
        assert_eq!(
            app.local_pane.selection(),
            None,
            "⌘A did not select behind the dialog"
        );
        assert!(!app.settings.is_open(), "and Settings did not open on top");
    }

    fn crash_note(file: Option<&str>) -> dto::CrashNote {
        dto::CrashNote {
            at_ms: 1_792_000_000_000,
            location: "internal/service/x.rs:1:1".to_owned(),
            reported: false,
            file: file.map(std::path::PathBuf::from),
        }
    }

    fn speaking(
        saved: dto::LanguageChoice,
        preferred: &[&str],
    ) -> (App, Arc<crate::handler::testing::FakeMenuBar>) {
        let settings = Arc::new(FakeSettings {
            loaded: Some(dto::Settings {
                language: saved,
                ..dto::Settings::default()
            }),
            ..FakeSettings::default()
        });
        let menubar = Arc::new(crate::handler::testing::FakeMenuBar::default());
        let handlers = handlers_for(
            Arc::new(FakeDevice::with_storages(vec![storage(5, "Internal")])),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(default_transfer()),
            Arc::new(crate::handler::testing::FakeSearch::default()),
            settings,
            menubar.clone(),
            Arc::new(local::RecordingLanguages::new(preferred)),
        );
        (App::new(handlers).0, menubar)
    }

    fn choose_language(app: &mut App, choice: dto::LanguageChoice) {
        settings_request(app, settings::Request::SetLanguage(choice));
    }

    fn in_english<Output>(work: impl FnOnce() -> Output) -> Output {
        crate::i18n::with_language(dto::Language::English, work)
    }

    fn menu_titles(installed: &[dto::Menu]) -> Vec<String> {
        installed.iter().map(|menu| menu.title.clone()).collect()
    }

    #[test]
    fn an_app_built_from_saved_settings_opens_in_the_language_saved() {
        in_english(|| {
            let _ = speaking(
                dto::LanguageChoice::Chosen(dto::Language::Japanese),
                &["th-TH"],
            );

            assert_eq!(crate::i18n::language(), dto::Language::Japanese);
        });
    }

    #[test]
    fn an_app_that_follows_the_mac_opens_in_the_first_offered_language_the_mac_lists() {
        in_english(|| {
            let _ = speaking(dto::LanguageChoice::FollowMac, &["fr-FR", "th-TH"]);
            assert_eq!(crate::i18n::language(), dto::Language::Thai);
        });
        in_english(|| {
            let _ = speaking(dto::LanguageChoice::FollowMac, &[]);
            assert_eq!(crate::i18n::language(), dto::Language::English);
        });
    }

    fn the_six_main_screens(language: dto::Language) -> Vec<(&'static str, App)> {
        let mut connect = app();
        let _ = connect.update(Message::Device(device::Message::Connected(Err(
            to_error_response(dto::Error::AllowStorageAccess).into(),
        ))));

        let folder = two_listed_panes();

        let mut transfer = app();
        select_on_device(&mut transfer);
        let _ = transfer.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));

        let mut error = two_listed_panes();
        let _ = error.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::PathNotFound("/DCIM".into()))),
        }));

        let mut settings = two_listed_panes();
        settings_request(&mut settings, settings::Request::Open);

        let mut help = two_listed_panes();
        command(&mut help, Code::Slash);
        let _ = help.update(Message::Help(help::Message::Request(help::Request::Show(
            help::Tab::Troubleshooting,
        ))));

        let mut screens = vec![
            ("connect", connect),
            ("folder", folder),
            ("transfer", transfer),
            ("error", error),
            ("settings", settings),
            ("help", help),
        ];
        for (_, app) in &mut screens {
            crate::i18n::set_language(dto::Language::English);
            choose_language(app, dto::LanguageChoice::Chosen(language));
        }
        screens
    }

    fn the_dialogs(language: dto::Language) -> Vec<(&'static str, App)> {
        let mut info = two_listed_panes();
        command(&mut info, Code::KeyI);

        let mut import = two_listed_panes();
        let _ = import.update(Message::Device(device::Message::Request(
            device::Request::Import,
        )));

        let mut mirror = two_listed_panes();
        let _ = choose_from_menu(&mut mirror, menubar::Command::Mirror);

        let mut space = two_listed_panes();
        let _ = space.update(Message::Device(device::Message::Request(
            device::Request::FreeUpSpace,
        )));

        let mut dialogs = vec![
            ("info", info),
            ("import", import),
            ("mirror", mirror),
            ("space", space),
        ];
        for (_, app) in &mut dialogs {
            crate::i18n::set_language(dto::Language::English);
            choose_language(app, dto::LanguageChoice::Chosen(language));
        }
        dialogs
    }

    #[test]
    fn language_check() {
        crate::i18n::screens::clear_folder();
        let mut problems = Vec::new();

        for language in dto::Language::ALL {
            crate::i18n::with_language(language, || {
                for (screen, app) in the_six_main_screens(language)
                    .into_iter()
                    .chain(the_dialogs(language))
                {
                    problems.extend(crate::i18n::screens::look_at(language, screen, app.view()));
                }
            });
        }

        assert!(
            problems.is_empty(),
            "{} problems (the pictures are in target/language-check):\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    #[test]
    fn the_first_launch_speaks_the_language_the_mac_lists_or_english() {
        use dto::Language::{ChineseSimplified, English, Japanese, Thai};

        let cases: [(dto::LanguageChoice, &[&str], dto::Language); 8] = [
            (dto::LanguageChoice::FollowMac, &["ja-JP"], Japanese),
            (dto::LanguageChoice::FollowMac, &["th-TH"], Thai),
            (
                dto::LanguageChoice::FollowMac,
                &["zh-Hans-CN"],
                ChineseSimplified,
            ),
            (dto::LanguageChoice::FollowMac, &["fr-FR"], English),
            (dto::LanguageChoice::FollowMac, &["zh-Hant-TW"], English),
            (dto::LanguageChoice::FollowMac, &["en-TH", "th-TH"], English),
            (dto::LanguageChoice::FollowMac, &[], English),
            (dto::LanguageChoice::Chosen(Thai), &["ja-JP"], Thai),
        ];

        for (saved, listed, spoken) in cases {
            in_english(|| {
                let _ = speaking(saved, listed);

                assert_eq!(crate::i18n::language(), spoken, "{saved:?} with {listed:?}");
            });
        }
    }

    #[test]
    fn the_language_is_set_before_the_first_view_so_nothing_flashes_in_english() {
        in_english(|| {
            let (app, _) = speaking(dto::LanguageChoice::FollowMac, &["ja-JP"]);

            let mut ui = iced_test::simulator(app.view());

            assert!(
                ui.find(crate::i18n::t("app.help_button").to_owned())
                    .is_ok()
            );
            assert!(
                ui.find("Help…").is_err(),
                "the English word is not drawn first"
            );
        });
    }

    #[test]
    fn choosing_a_language_in_settings_changes_the_language_of_the_app() {
        for language in dto::Language::ALL {
            in_english(|| {
                let (mut app, _) = speaking(dto::LanguageChoice::FollowMac, &[]);

                choose_language(&mut app, dto::LanguageChoice::Chosen(language));

                assert_eq!(crate::i18n::language(), language);
                assert_eq!(
                    app.settings.current().language,
                    dto::LanguageChoice::Chosen(language)
                );
            });
        }
    }

    #[test]
    fn following_the_mac_again_goes_to_the_language_the_mac_lists() {
        in_english(|| {
            let (mut app, _) =
                speaking(dto::LanguageChoice::Chosen(dto::Language::Thai), &["ja-JP"]);
            assert_eq!(crate::i18n::language(), dto::Language::Thai);

            choose_language(&mut app, dto::LanguageChoice::FollowMac);

            assert_eq!(crate::i18n::language(), dto::Language::Japanese);
        });
    }

    #[test]
    fn a_switch_puts_the_menu_up_again_in_the_new_language() {
        in_english(|| {
            let (mut app, menubar) = speaking(dto::LanguageChoice::FollowMac, &[]);
            let _ = app.update(Message::Focused(true));
            assert_eq!(menubar.installed.lock().unwrap().len(), 1);

            choose_language(&mut app, dto::LanguageChoice::Chosen(dto::Language::Thai));

            let installed = menubar.installed.lock().unwrap();
            assert_eq!(installed.len(), 2, "put up again");
            assert_ne!(
                menu_titles(&installed[1]),
                menu_titles(&installed[0]),
                "with the new titles"
            );
        });
    }

    #[test]
    fn a_setting_that_leaves_the_language_alone_puts_nothing_up_again() {
        in_english(|| {
            let (mut app, menubar) = speaking(dto::LanguageChoice::FollowMac, &[]);
            let _ = app.update(Message::Focused(true));

            settings_request(&mut app, settings::Request::SetShowHiddenOnMac(true));
            choose_language(&mut app, dto::LanguageChoice::FollowMac);

            assert_eq!(menubar.installed.lock().unwrap().len(), 1);
        });
    }

    #[test]
    fn the_language_is_two_clicks_from_the_main_window() {
        in_english(|| {
            let (mut app, _) = speaking(dto::LanguageChoice::FollowMac, &[]);
            let mut clicks = 0;

            click(&mut app, "Settings…");
            clicks += 1;
            assert!(app.settings.is_open());

            let title = {
                let mut ui = iced_test::simulator(app.view());
                ui.find("Language").unwrap().bounds()
            };
            let picks_thai = |message: &Message| {
                matches!(
                    message,
                    Message::Settings(settings::Message::Request(
                        settings::Request::SetLanguage(choice)
                    )) if *choice == dto::LanguageChoice::Chosen(dto::Language::Thai)
                )
            };
            let reached = (0..200).find_map(|step| {
                let mut ui = iced_test::simulator(app.view());
                ui.point_at(iced::Point::new(
                    title.x + 8.0,
                    title.y + title.height + step as f32 * 3.0,
                ));
                let _ = ui.simulate(iced_test::simulator::click());
                let messages: Vec<Message> = ui.into_messages().collect();
                messages.iter().any(picks_thai).then_some(messages)
            });
            clicks += 1;
            for message in reached.expect("one click on the dialog picks Thai") {
                let _ = app.update(message);
            }

            assert_eq!(crate::i18n::language(), dto::Language::Thai);
            assert!(clicks <= 3);
        });
    }

    /// Clicks the button `label` in what the app shows, and feeds it back.
    fn click(app: &mut App, label: &str) {
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(app.view());
            let _ = ui.click(label.to_owned()).unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = app.update(message);
        }
    }

    #[test]
    fn a_crash_an_earlier_run_left_is_said_below_the_panes_until_dismissed() {
        let fake = Arc::new(crate::handler::testing::FakeCrash {
            unreported: vec![crash_note(Some("/crashes/crash-1.txt"))],
            ..Default::default()
        });
        let mut app = app();
        assert!(!shows(&app, "Show Report"), "a quiet start says nothing");
        app.crash = crash::Handler::new(fake.clone());

        assert!(shows(&app, "Show Report"));
        click(&mut app, "Show Report");
        assert_eq!(
            *fake.opened.lock().unwrap(),
            [std::path::PathBuf::from("/crashes/crash-1.txt")]
        );
        assert!(shows(&app, "Dismiss"), "looking is not dismissing");

        click(&mut app, "Dismiss");

        assert!(!shows(&app, "Dismiss"));
        assert_eq!(fake.told.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panic_in_a_task_is_said_after_the_next_update() {
        let fake = Arc::new(crate::handler::testing::FakeCrash::default());
        let mut app = app();
        app.crash = crash::Handler::new(fake.clone());
        let _ = app.update(Message::Focused(true));
        assert!(!app.crash.is_visible());

        // A task's panic: nothing reaches the app but the count.
        fake.panic_at(1_792_000_000_000, Some("/crashes/crash-2.txt"));
        let _ = app.update(Message::Focused(true));

        assert!(app.crash.is_visible());
        assert!(shows(&app, "Show Report"));
        click(&mut app, "Dismiss");
        let _ = app.update(Message::Focused(true));
        assert!(!app.crash.is_visible(), "said once");
    }

    #[test]
    fn the_sidebar_help_button_opens_help() {
        let mut app = two_listed_panes();
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(app.view());
            let _ = ui.click("Help…".to_owned()).unwrap();
            ui.into_messages().collect()
        };

        for message in messages {
            let _ = app.update(message);
        }

        assert!(help_is_open(&app));
    }

    #[test]
    fn help_and_settings_never_stack() {
        let mut app = two_listed_panes();
        command(&mut app, Code::Comma);
        assert!(app.settings.is_open());

        let _ = app.update(help_request(help::Request::Open));
        assert!(help_is_open(&app));
        assert!(!app.settings.is_open(), "opening Help put Settings away");

        let _ = app.update(Message::Settings(settings::Message::Request(
            settings::Request::Open,
        )));
        assert!(app.settings.is_open());
        assert!(!help_is_open(&app), "opening Settings put Help away");
    }

    #[test]
    fn opening_help_puts_away_an_open_right_click_menu() {
        let mut app = two_listed_panes();
        right_click(&mut app, Side::Mac, Some("notes.txt"));
        assert!(app.local_pane.has_menu());

        let _ = app.update(help_request(help::Request::Open));

        assert!(!app.local_pane.has_menu());
    }

    #[test]
    fn the_window_draws_with_help_open_over_it() {
        let mut app = two_listed_panes();
        let _ = app.update(help_request(help::Request::Open));

        let mut ui = iced_test::simulator(app.view());

        assert!(ui.find("Troubleshooting".to_owned()).is_ok());
        assert!(ui.snapshot(&iced::Theme::Dark).is_ok());
    }

    #[tokio::test]
    async fn help_tells_its_report_how_the_phone_looks_right_now() {
        let mut app = two_listed_panes();
        let _ = app.update(help_request(help::Request::Open));

        settle(
            &mut app,
            help_request(help::Request::Show(help::Tab::About)),
        )
        .await;

        let mut ui = iced_test::simulator(app.view());
        let line = "Phone: Pixel 9, serial …C123, 1 storage";
        assert!(
            ui.find("Collecting…".to_owned()).is_err(),
            "the report finished"
        );
        let found = ui
            .find(
                move |candidate: iced_test::selector::Candidate<'_>| match candidate {
                    iced_test::selector::Candidate::Text { content, .. }
                        if content.contains(line) =>
                    {
                        Some(())
                    }
                    _ => None,
                },
            )
            .is_ok();
        assert!(found, "the report names the connected phone");
    }

    #[tokio::test]
    async fn help_opened_with_no_phone_says_not_connected() {
        let mut app = app();
        let _ = app.update(help_request(help::Request::Open));

        settle(
            &mut app,
            help_request(help::Request::Show(help::Tab::About)),
        )
        .await;

        let mut ui = iced_test::simulator(app.view());
        let found = ui
            .find(
                |candidate: iced_test::selector::Candidate<'_>| match candidate {
                    iced_test::selector::Candidate::Text { content, .. }
                        if content.contains("Phone: connecting")
                            || content.contains("Phone: not connected") =>
                    {
                        Some(())
                    }
                    _ => None,
                },
            )
            .is_ok();
        assert!(found);
    }

    #[tokio::test]
    async fn help_opened_with_a_phone_connected_says_it_is_connected() {
        let mut app = two_listed_panes();
        let _ = app.update(help_request(help::Request::Open));

        settle(
            &mut app,
            help_request(help::Request::Show(help::Tab::Troubleshooting)),
        )
        .await;

        let mut ui = iced_test::simulator(app.view());
        let found = ui
            .find(
                |candidate: iced_test::selector::Candidate<'_>| match candidate {
                    iced_test::selector::Candidate::Text { content, .. }
                        if content.starts_with("Connected: Pixel 9") =>
                    {
                        Some(())
                    }
                    _ => None,
                },
            )
            .is_ok();
        assert!(
            found,
            "the check starts from the fact that the phone is connected"
        );
    }

    #[tokio::test]
    async fn help_opened_with_no_phone_runs_the_failure_check() {
        let mut app = app();
        let _ = app.update(help_request(help::Request::Open));

        settle(
            &mut app,
            help_request(help::Request::Show(help::Tab::Troubleshooting)),
        )
        .await;

        let mut ui = iced_test::simulator(app.view());
        assert!(
            ui.find("No other app is using your phone.".to_owned())
                .is_ok()
        );
        assert!(
            ui.find(
                |candidate: iced_test::selector::Candidate<'_>| match candidate {
                    iced_test::selector::Candidate::Text { content, .. }
                        if content.starts_with("Connected:") =>
                    {
                        Some(())
                    }
                    _ => None,
                }
            )
            .is_err()
        );
    }

    /// The messages the work started by `message` sends back.
    async fn sent_by(app: &mut App, message: Message) -> Vec<Message> {
        crate::handler::testing::drain(app.update(message)).await.0
    }

    fn reloads_space(messages: &[Message]) -> bool {
        messages
            .iter()
            .any(|m| matches!(m, Message::Device(device::Message::StoragesReloaded(_))))
    }

    fn finished_ok() -> Message {
        Message::Transfer(transfer::Message::Finished(Ok(summary(
            "/Users/me/Downloads",
        ))))
    }

    #[tokio::test]
    async fn free_up_space_opens_from_the_sidebar_as_a_dialog_and_esc_closes_it() {
        let mut app = two_listed_panes();
        let mut ui = iced_test::simulator(app.view());
        let clicked = ui.click("Free Up Space…").is_ok();
        let messages = ui.into_messages().collect::<Vec<_>>();
        assert!(clicked);
        for message in messages {
            let _ = app.update(message);
        }

        assert!(app.space.is_open());
        assert!(app.menu_state().dialog_open);
        assert!(
            app.why_not_switch().is_some(),
            "looking at the phone is phone work"
        );
        assert!(
            iced_test::simulator(app.view())
                .find("Free Up Space on Internal")
                .is_ok()
        );
        let _ = app.update(key_press(
            keyboard::Key::Named(keyboard::key::Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        ));
        assert!(!app.space.is_open());
    }

    #[tokio::test]
    async fn removing_from_the_phone_reads_its_space_and_the_folder_shown_again() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::FreeUpSpace,
        )));

        let messages = sent_by(
            &mut app,
            Message::Space(freeing::Message::Removed {
                look: 1,
                result: Ok(dto::Removal {
                    removed: 1,
                    ..Default::default()
                }),
            }),
        )
        .await;

        assert!(reloads_space(&messages), "{messages:?}");
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DevicePane(explorer::Message::Listed { .. }))),
            "the folder shown is read again"
        );
        let failed = sent_by(
            &mut app,
            Message::Space(freeing::Message::Removed {
                look: 1,
                result: Err(to_error_response(dto::Error::Disconnected)),
            }),
        )
        .await;
        assert!(!reloads_space(&failed));
    }

    #[tokio::test]
    async fn a_removal_that_crashed_part_way_still_reads_the_phone_again() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::FreeUpSpace,
        )));

        // Some files may be gone by then, so what is shown can't be trusted.
        let messages = sent_by(
            &mut app,
            Message::Space(freeing::Message::Removed {
                look: 1,
                result: Err(to_error_response(dto::Error::Crashed)),
            }),
        )
        .await;

        assert!(reloads_space(&messages), "{messages:?}");
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DevicePane(explorer::Message::Listed { .. }))),
            "the folder shown is read again"
        );
    }

    #[tokio::test]
    async fn the_biggest_files_are_the_phone_search_ordered_by_size() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::FreeUpSpace,
        )));

        let _ = app.update(Message::Space(freeing::Message::Request(
            freeing::Request::BiggestFiles,
        )));

        assert!(!app.space.is_open());
        assert!(app.search.is_open());
        assert!(
            iced_test::simulator(app.view()).find("Size ▼").is_ok(),
            "largest first"
        );
    }

    #[tokio::test]
    async fn losing_the_phone_closes_free_up_space() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::FreeUpSpace,
        )));
        app.close_device_pane();
        assert!(!app.space.is_open());
    }

    #[tokio::test]
    async fn a_finished_transfer_asks_the_phone_how_full_it_is_now() {
        let mut app = two_listed_panes();

        let messages = sent_by(&mut app, finished_ok()).await;

        assert!(reloads_space(&messages), "{messages:?}");
    }

    #[tokio::test]
    async fn a_transfer_that_failed_part_way_asks_too_since_some_files_moved() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Err(to_error_response(
                dto::Error::Cancelled,
            )))),
        )
        .await;

        assert!(reloads_space(&messages));
    }

    #[tokio::test]
    async fn a_transfer_that_lost_the_phone_has_no_phone_to_ask() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Err(to_error_response(
                dto::Error::Disconnected,
            )))),
        )
        .await;

        assert!(!reloads_space(&messages));
    }

    #[tokio::test]
    async fn progress_and_other_transfer_messages_do_not_ask() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Progress(dto::TransferProgress::default())),
        )
        .await;

        assert!(!reloads_space(&messages));
    }

    fn phone_edited(
        result: Result<Option<String>, crate::handler::error::ErrorResponse>,
    ) -> Message {
        Message::DevicePane(explorer::Message::Edited {
            volume_id: 5,
            path: "/".into(),
            result,
            undo: None,
        })
    }

    #[tokio::test]
    async fn a_delete_or_new_folder_on_the_phone_asks_how_full_it_is_now() {
        let mut app = two_listed_panes();

        let deleted = sent_by(&mut app, phone_edited(Ok(None))).await;
        let made = sent_by(&mut app, phone_edited(Ok(Some("Trip".into())))).await;

        assert!(reloads_space(&deleted));
        assert!(reloads_space(&made));
    }

    #[tokio::test]
    async fn an_edit_that_failed_changed_nothing_so_nothing_is_asked() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            phone_edited(Err(to_error_response(dto::Error::ReadOnly))),
        )
        .await;

        assert!(!reloads_space(&messages));
    }

    #[tokio::test]
    async fn an_edit_on_the_mac_does_not_ask_the_phone_anything() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            Message::LocalPane(explorer::Message::Edited {
                volume_id: MAC_VOLUME,
                path: "/Users/me".into(),
                result: Ok(None),
                undo: None,
            }),
        )
        .await;

        assert!(!reloads_space(&messages));
    }

    #[tokio::test]
    async fn with_no_phone_connected_nothing_is_asked_after_a_transfer() {
        let mut app = app();

        let messages = sent_by(&mut app, finished_ok()).await;

        assert!(!reloads_space(&messages));
    }

    // ---- ⌘X, and pasting inside the phone ----

    /// The phone pane is active with `a.jpg` selected; its pastes go to `files`.
    fn on_the_phone(files: &Arc<FakeExplorer>) -> App {
        let mut app = two_listed_panes_on(Arc::clone(files));
        command(&mut app, Code::Digit1);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        app
    }

    fn phone_key(app: &mut App, code: Code) -> Task<Message> {
        app.update(key_press(
            Key::Character("x".into()),
            code,
            Modifiers::COMMAND,
        ))
    }

    async fn feed_back(app: &mut App, task: Task<Message>) {
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = app.update(message);
        }
    }

    /// Runs `task` and everything it sets going, such as the next queued
    /// transfer starting when one finishes, until nothing is left.
    async fn settle_task(app: &mut App, task: Task<Message>) {
        let mut pending = vec![task];
        while let Some(task) = pending.pop() {
            for message in crate::handler::testing::drain(task).await.0 {
                pending.push(app.update(message));
            }
        }
    }

    fn clipboard(app: &App) -> Option<(Side, usize, bool)> {
        app.clipboard
            .as_ref()
            .map(|c| (c.side, c.selections.len(), c.cut))
    }

    #[tokio::test]
    async fn a_copy_pasted_in_the_phone_makes_copies_and_can_be_pasted_again() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = on_the_phone(&files);

        command(&mut app, Code::KeyC);
        let task = phone_key(&mut app, Code::KeyV);
        feed_back(&mut app, task).await;

        let pastes = files.pastes.lock().unwrap().clone();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].mode, dto::Paste::Copy);
        assert_eq!(
            (pastes[0].from.as_str(), pastes[0].names.clone()),
            ("/", vec!["a.jpg".to_owned()])
        );
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 1, false)),
            "still there for another paste"
        );
        assert!(!app.transfer.is_running(), "nothing leaves the phone");
        let _ = app.view();
    }

    #[tokio::test]
    async fn a_cut_pasted_in_the_phone_moves_and_is_then_used_up() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = on_the_phone(&files);

        command(&mut app, Code::KeyX);
        assert_eq!(clipboard(&app), Some((Side::Phone, 1, true)));
        let _ = app.view();
        let task = phone_key(&mut app, Code::KeyV);
        feed_back(&mut app, task).await;

        assert_eq!(files.pastes.lock().unwrap()[0].mode, dto::Paste::Move);
        assert_eq!(clipboard(&app), None, "what moved can't be moved again");
        assert!(!app.can_paste());
        let _ = app.view();
    }

    #[tokio::test]
    async fn a_cut_that_was_refused_stays_cut_for_another_try() {
        let files = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::IntoItself("a.jpg".into()),
        )]));
        let mut app = on_the_phone(&files);

        command(&mut app, Code::KeyX);
        let task = phone_key(&mut app, Code::KeyV);
        feed_back(&mut app, task).await;

        assert_eq!(clipboard(&app), Some((Side::Phone, 1, true)));
        assert!(app.can_paste());
    }

    /// The phone pane has `a.jpg` selected and the Mac pane is active, so a
    /// paste there acts on the phone's clipboard; transfers go to `transfers`.
    fn phone_clipboard_pasted_on_the_mac(cut: bool) -> (App, Arc<FakeExplorer>, Arc<FakeTransfer>) {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with(Arc::clone(&files), Arc::clone(&transfers));
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        command(&mut app, Code::Digit1);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        command(&mut app, if cut { Code::KeyX } else { Code::KeyC });
        command(&mut app, Code::Digit1);
        assert_eq!(app.target(), Side::Mac);
        (app, files, transfers)
    }

    #[tokio::test]
    async fn a_cut_pasted_on_the_mac_moves_the_files_and_is_used_up() {
        let (mut app, files, transfers) = phone_clipboard_pasted_on_the_mac(true);
        assert!(app.can_paste(), "a cut can go to the Mac");

        let task = phone_key(&mut app, Code::KeyV);
        feed_back(&mut app, task).await;

        assert_eq!(*transfers.modes.lock().unwrap(), [dto::Paste::Move]);
        let requests = transfers.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            (requests[0].source.as_str(), requests[0].names.clone()),
            ("/", vec!["a.jpg".to_owned()])
        );
        assert_eq!(requests[0].destination, "/Users/me");
        assert_eq!(clipboard(&app), None, "queued, so it is used up");
        assert!(!app.device_pane.is_cut("a.jpg"), "no longer dimmed");
        assert!(
            files.pastes.lock().unwrap().is_empty(),
            "not a paste inside the phone"
        );
        let _ = app.view();
    }

    #[tokio::test]
    async fn a_copy_pasted_on_the_mac_is_still_a_plain_download() {
        let (mut app, _files, transfers) = phone_clipboard_pasted_on_the_mac(false);

        let task = phone_key(&mut app, Code::KeyV);
        feed_back(&mut app, task).await;

        assert_eq!(*transfers.modes.lock().unwrap(), [dto::Paste::Copy]);
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 1, false)),
            "a copy can be pasted again"
        );
    }

    #[tokio::test]
    async fn a_cut_with_several_folders_queues_one_move_for_each() {
        let (mut app, _files, transfers) = phone_clipboard_pasted_on_the_mac(true);
        app.clipboard
            .as_mut()
            .unwrap()
            .selections
            .push(explorer::Selection {
                volume_id: 5,
                path: "/DCIM".into(),
                names: vec!["b.jpg".into()],
            });

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        assert_eq!(
            *transfers.modes.lock().unwrap(),
            [dto::Paste::Move, dto::Paste::Move]
        );
        let sources: Vec<_> = transfers
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.source.clone())
            .collect();
        assert_eq!(sources, ["/", "/DCIM"]);
        assert_eq!(clipboard(&app), None);
    }

    #[test]
    fn a_cut_is_not_pasted_on_the_mac_without_a_folder_there() {
        let (mut app, _files, transfers) = phone_clipboard_pasted_on_the_mac(true);
        app.local_pane.close();

        let _ = phone_key(&mut app, Code::KeyV);

        assert!(transfers.modes.lock().unwrap().is_empty());
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 1, true)),
            "kept for another try"
        );
    }

    fn moved_summary(removed: usize) -> dto::TransferSummary {
        dto::TransferSummary {
            files: 1,
            bytes: 3,
            destination: "/Users/me".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(dto::Removal {
                removed,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_finished_move_shows_the_phone_pane_without_the_files_that_left() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(Arc::clone(&files));
        files.allowed.lock().unwrap().clear();

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Ok(moved_summary(1)))),
        )
        .await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DevicePane(explorer::Message::Listed { .. }))),
            "the phone pane was read again"
        );
        assert_eq!(
            *files.allowed.lock().unwrap(),
            [true],
            "from what the explorer kept true, not file by file"
        );
    }

    #[tokio::test]
    async fn a_copy_or_a_move_that_removed_nothing_leaves_the_phone_pane_alone() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(Arc::clone(&files));
        files.allowed.lock().unwrap().clear();

        for removal in [None, Some(dto::Removal::default())] {
            let summary = dto::TransferSummary {
                removal,
                ..moved_summary(0)
            };
            let _ = sent_by(
                &mut app,
                Message::Transfer(transfer::Message::Finished(Ok(summary))),
            )
            .await;
        }

        assert!(files.allowed.lock().unwrap().is_empty());
    }

    fn select_on_the_mac(app: &mut App, name: &str) {
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select(name.into()),
        )));
    }

    #[test]
    fn a_cut_on_the_mac_is_remembered_and_dims_its_row_there() {
        let mut app = two_listed_panes();
        select_on_the_mac(&mut app, "notes.txt");

        command(&mut app, Code::KeyX);

        assert_eq!(clipboard(&app), Some((Side::Mac, 1, true)));
        assert!(app.local_pane.is_cut("notes.txt"));
        assert!(!app.device_pane.is_cut("a.jpg"));
    }

    #[test]
    fn a_cut_dims_the_pane_it_was_made_in_and_a_new_one_moves_the_dimming() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = on_the_phone(&files);
        command(&mut app, Code::KeyX);
        assert!(app.device_pane.is_cut("a.jpg") && !app.local_pane.is_cut("notes.txt"));

        command(&mut app, Code::Digit1);
        select_on_the_mac(&mut app, "notes.txt");
        command(&mut app, Code::KeyX);

        assert!(app.local_pane.is_cut("notes.txt"));
        assert!(!app.device_pane.is_cut("a.jpg"), "one cut at a time");

        command(&mut app, Code::KeyC);
        assert!(!app.local_pane.is_cut("notes.txt"), "a copy ends the cut");
    }

    #[test]
    fn cutting_with_nothing_selected_does_nothing() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(files);
        command(&mut app, Code::Digit1);

        command(&mut app, Code::KeyX);

        assert_eq!(clipboard(&app), None);
    }

    #[test]
    fn a_copy_or_a_queued_copy_replaces_a_cut() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = on_the_phone(&files);

        command(&mut app, Code::KeyX);
        command(&mut app, Code::KeyC);
        assert_eq!(clipboard(&app), Some((Side::Phone, 1, false)));

        command(&mut app, Code::KeyX);
        copy_to_queue(&mut app);
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 1, false)),
            "a queued copy never joins a cut: it starts a copy"
        );
        copy_to_queue(&mut app);
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 2, false)),
            "but joins a copy"
        );
    }

    /// The Mac pane lists notes.txt and song.mp3 (selected: notes.txt); a
    /// phone pane lists a.jpg. The Mac's files and the transfers are the
    /// test's to look at.
    fn mac_app() -> (App, Arc<FakeExplorer>, Arc<FakeTransfer>) {
        let mac_files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with_panes(
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::clone(&mac_files),
            Arc::clone(&transfers),
        );
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt"), file("song.mp3")]),
        }));
        select_on_the_mac(&mut app, "notes.txt");
        (app, mac_files, transfers)
    }

    #[tokio::test]
    async fn a_copy_in_the_mac_pane_is_pasted_in_the_mac_pane() {
        let (mut app, mac_files, transfers) = mac_app();
        command(&mut app, Code::KeyC);

        assert!(app.can_paste());
        let task = app.update(key_press(
            Key::Character("v".into()),
            Code::KeyV,
            Modifiers::COMMAND,
        ));
        feed_back(&mut app, task).await;

        let pastes = mac_files.pastes.lock().unwrap();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].mode, dto::Paste::Copy);
        assert_eq!(
            (
                pastes[0].from.as_str(),
                pastes[0].to.as_str(),
                pastes[0].names.clone()
            ),
            ("/Users/me", "/Users/me", vec!["notes.txt".to_owned()])
        );
        assert_eq!(pastes[0].to_storage, MAC_VOLUME);
        assert!(!app.transfer.is_running(), "done on the spot");
        assert!(transfers.mac_pastes.lock().unwrap().is_empty());
        assert_eq!(
            clipboard(&app),
            Some((Side::Mac, 1, false)),
            "and can be pasted again"
        );
    }

    #[tokio::test]
    async fn a_cut_in_the_mac_pane_is_a_move_and_is_used_up_when_it_went() {
        let (mut app, mac_files, _) = mac_app();
        command(&mut app, Code::KeyX);
        assert!(app.local_pane.is_cut("notes.txt"));

        let task = app.update(key_press(
            Key::Character("v".into()),
            Code::KeyV,
            Modifiers::COMMAND,
        ));
        feed_back(&mut app, task).await;

        assert_eq!(mac_files.pastes.lock().unwrap()[0].mode, dto::Paste::Move);
        assert_eq!(clipboard(&app), None, "a move is done once");
        assert!(!app.local_pane.is_cut("notes.txt"));
    }

    #[tokio::test]
    async fn a_refused_paste_in_the_mac_pane_leaves_what_was_cut_for_another_go() {
        let (mut app, mac_files, _) = mac_app();
        mac_files
            .paste_results
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::IntoItself("notes.txt".into())));
        command(&mut app, Code::KeyX);

        let task = app.update(key_press(
            Key::Character("v".into()),
            Code::KeyV,
            Modifiers::COMMAND,
        ));
        feed_back(&mut app, task).await;

        assert_eq!(clipboard(&app), Some((Side::Mac, 1, true)));
        assert!(app.local_pane.is_cut("notes.txt"));
    }

    #[tokio::test]
    async fn a_paste_too_big_for_the_mac_pane_is_queued_with_progress_and_uses_the_cut_up() {
        let (mut app, mac_files, transfers) = mac_app();
        mac_files
            .paste_results
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::Unsupported));
        command(&mut app, Code::KeyX);

        let task = app.update(key_press(
            Key::Character("v".into()),
            Code::KeyV,
            Modifiers::COMMAND,
        ));
        settle_task(&mut app, task).await;

        let queued = transfers.mac_pastes.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].mode, dto::Paste::Move);
        assert_eq!(
            (queued[0].from.as_str(), queued[0].to.as_str()),
            ("/Users/me", "/Users/me")
        );
        assert_eq!(queued[0].names, ["notes.txt"]);
        assert_eq!(clipboard(&app), None, "queued once, so used up");
        assert!(!app.local_pane.is_cut("notes.txt"));
    }

    #[tokio::test]
    async fn a_copy_queued_in_the_mac_stays_ready_to_paste_again() {
        let (mut app, mac_files, transfers) = mac_app();
        mac_files
            .paste_results
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::Unsupported));
        command(&mut app, Code::KeyC);

        let task = app.update(key_press(
            Key::Character("v".into()),
            Code::KeyV,
            Modifiers::COMMAND,
        ));
        settle_task(&mut app, task).await;

        assert_eq!(transfers.mac_pastes.lock().unwrap().len(), 1);
        assert_eq!(clipboard(&app), Some((Side::Mac, 1, false)));
    }

    /// Press notes.txt in the Mac pane, drag it onto the folder Docs, let go.
    async fn drag_notes_onto_docs(app: &mut App) {
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt"), folder("Docs")]),
        }));
        for request in [
            explorer::Request::Select("notes.txt".into()),
            explorer::Request::PointerLeft("notes.txt".into()),
            explorer::Request::PointerEntered("Docs".into()),
        ] {
            let _ = app.update(Message::LocalPane(explorer::Message::Request(request)));
        }
        let task = app.update(Message::PointerReleased);
        settle_task(app, task).await;
    }

    #[tokio::test]
    async fn a_drag_in_the_mac_pane_is_dropped_by_the_app_on_the_folder_under_the_pointer() {
        let (mut app, mac_files, _) = mac_app();

        drag_notes_onto_docs(&mut app).await;

        let pastes = mac_files.pastes.lock().unwrap();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].mode, dto::Paste::Move);
        assert_eq!(
            (
                pastes[0].from.as_str(),
                pastes[0].to.as_str(),
                pastes[0].names.clone()
            ),
            ("/Users/me", "/Users/me/Docs", vec!["notes.txt".to_owned()])
        );
    }

    #[tokio::test]
    async fn a_drop_in_the_mac_pane_too_big_to_do_on_the_spot_is_queued_into_that_folder() {
        let (mut app, mac_files, transfers) = mac_app();
        mac_files
            .paste_results
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::Unsupported));

        drag_notes_onto_docs(&mut app).await;

        let queued = transfers.mac_pastes.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(
            queued[0].to, "/Users/me/Docs",
            "where it was dropped, not the folder shown"
        );
        assert_eq!(queued[0].mode, dto::Paste::Move);
    }

    #[tokio::test]
    async fn a_finished_copy_inside_the_mac_shows_the_pane_it_went_to() {
        let (mut app, _, _) = mac_app();
        let summary = dto::TransferSummary {
            direction: dto::Direction::Local,
            files: 1,
            destination: "/Users/me".into(),
            first_item: Some("notes (1).txt".into()),
            ..Default::default()
        };

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Ok(summary))),
        )
        .await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::LocalPane(explorer::Message::Listed { .. }))),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn the_menu_cuts_on_either_pane_and_pastes_inside_the_phone() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(Arc::clone(&files));

        right_click(&mut app, Side::Mac, Some("notes.txt"));
        assert!(
            menu_labels(&app, Side::Mac).contains(&"Cut".to_owned()),
            "the Mac pane's menu cuts too: that moves to the phone"
        );
        choose(&mut app, Side::Mac, explorer::MenuAction::Cut);
        assert_eq!(clipboard(&app), Some((Side::Mac, 1, true)));

        right_click(&mut app, Side::Phone, Some("a.jpg"));
        assert!(menu_labels(&app, Side::Phone).contains(&"Cut".to_owned()));
        choose(&mut app, Side::Phone, explorer::MenuAction::Cut);
        assert_eq!(clipboard(&app), Some((Side::Phone, 1, true)));

        right_click(&mut app, Side::Phone, None);
        let task = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Menu(explorer::MenuAction::Paste),
        )));
        feed_back(&mut app, task).await;

        assert_eq!(files.pastes.lock().unwrap()[0].mode, dto::Paste::Move);
    }

    /// Paste on the phone pane, answer the question about names already taken
    /// there (the fake phone says all are), and run whatever that sets going.
    async fn paste_on_the_phone(app: &mut App) {
        command(app, Code::Digit1);
        let task = phone_key(app, Code::KeyV);
        settle_task(app, task).await;
        let task = app.update(Message::Transfer(transfer::Message::Request(
            transfer::Request::Resolve(dto::Conflict::KeepBoth),
        )));
        settle_task(app, task).await;
    }

    #[tokio::test]
    async fn pasting_a_mac_cut_on_the_phone_is_a_move_and_uses_the_cut_up() {
        let transfers = Arc::new(default_transfer());
        let mut app = two_listed_panes_with(
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::clone(&transfers),
        );
        select_on_the_mac(&mut app, "notes.txt");
        command(&mut app, Code::KeyX);

        paste_on_the_phone(&mut app).await;

        assert_eq!(*transfers.upload_modes.lock().unwrap(), [dto::Paste::Move]);
        let uploads = transfers.uploads.lock().unwrap();
        assert_eq!(uploads[0].0.sources, ["/Users/me/notes.txt"]);
        assert_eq!(uploads[0].0.destination, "/");
        assert_eq!(clipboard(&app), None, "a move is queued once");
        assert!(!app.local_pane.is_cut("notes.txt"), "and the dimming ends");
    }

    #[tokio::test]
    async fn pasting_a_mac_copy_on_the_phone_is_still_a_copy_and_can_be_pasted_again() {
        let transfers = Arc::new(default_transfer());
        let mut app = two_listed_panes_with(
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::clone(&transfers),
        );
        select_on_the_mac(&mut app, "notes.txt");
        command(&mut app, Code::KeyC);

        paste_on_the_phone(&mut app).await;

        assert_eq!(*transfers.upload_modes.lock().unwrap(), [dto::Paste::Copy]);
        assert_eq!(clipboard(&app), Some((Side::Mac, 1, false)));
    }

    #[test]
    fn a_mac_cut_waits_for_a_phone_folder_instead_of_being_lost() {
        let mut app = app();
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        }));
        select_on_the_mac(&mut app, "notes.txt");
        command(&mut app, Code::KeyX);

        let _ = app.paste(Side::Phone);

        assert!(!app.transfer.is_running(), "no phone, nothing sent");
        assert_eq!(clipboard(&app), Some((Side::Mac, 1, true)), "still cut");
        assert!(app.local_pane.is_cut("notes.txt"));
    }

    #[test]
    fn a_phone_closing_does_not_take_a_mac_cut_with_it() {
        // What was cut on the Mac is the Mac's, whatever phone comes next.
        let mut app = two_listed_panes();
        select_on_the_mac(&mut app, "notes.txt");
        command(&mut app, Code::KeyX);

        app.close_device_pane();

        assert_eq!(clipboard(&app), Some((Side::Mac, 1, true)));
    }

    fn moved_to_the_phone(removed: usize, from_mac: bool) -> Message {
        Message::Transfer(transfer::Message::Finished(Ok(dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: removed,
            destination: "/".into(),
            first_item: Some("notes.txt".into()),
            removal: Some(dto::Removal {
                from_mac,
                removed,
                ..Default::default()
            }),
            ..Default::default()
        })))
    }

    #[tokio::test]
    async fn a_finished_move_to_the_phone_shows_the_mac_pane_without_what_went_to_the_trash() {
        let mut app = two_listed_panes();

        let messages = sent_by(&mut app, moved_to_the_phone(1, true)).await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::LocalPane(explorer::Message::Listed { .. }))),
            "the Mac pane was read again: {messages:?}"
        );
    }

    #[tokio::test]
    async fn a_move_inside_the_phone_does_not_read_the_mac_pane_again() {
        let mut app = two_listed_panes();

        let messages = sent_by(&mut app, moved_to_the_phone(1, false)).await;

        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Message::LocalPane(explorer::Message::Listed { .. }))),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn a_move_to_the_phone_that_trashed_nothing_still_refreshes_nothing_odd() {
        // Even when every original stayed, the Mac pane is read again: it is
        // one local listing, and nothing else tells it what the Trash did.
        let mut app = two_listed_panes();

        let messages = sent_by(&mut app, moved_to_the_phone(0, true)).await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::LocalPane(explorer::Message::Listed { .. })))
        );
    }

    fn menu_labels(app: &App, side: Side) -> Vec<String> {
        match side {
            Side::Mac => app.local_pane.menu_labels(app.can_paste()),
            Side::Phone => app.device_pane.menu_labels(app.can_paste()),
        }
    }

    fn pasted_ok() -> Message {
        Message::DevicePane(explorer::Message::Pasted {
            volume_id: 5,
            path: "/".into(),
            result: Ok(crate::handler::explorer::PasteResponse {
                select: vec!["a (1).jpg".into()],
                notice: crate::handler::explorer::NoticeResponse {
                    title: "Copied as “a (1).jpg”".into(),
                    details: vec![],
                    problem: false,
                },
                undo: None,
            }),
        })
    }

    #[tokio::test]
    async fn a_paste_that_went_through_asks_how_full_the_phone_is_now() {
        let mut app = two_listed_panes();

        let messages = sent_by(&mut app, pasted_ok()).await;

        assert!(reloads_space(&messages));
    }

    #[tokio::test]
    async fn a_paste_that_failed_changed_nothing_so_nothing_is_asked() {
        let mut app = two_listed_panes();

        let messages = sent_by(
            &mut app,
            Message::DevicePane(explorer::Message::Pasted {
                volume_id: 5,
                path: "/".into(),
                result: Err(to_error_response(dto::Error::ReadOnly)),
            }),
        )
        .await;

        assert!(!reloads_space(&messages));
    }

    #[test]
    fn a_paste_that_lost_the_phone_closes_the_pane_and_says_so() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::DevicePane(explorer::Message::Pasted {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::Disconnected)),
        }));

        assert!(!app.device_pane.is_open());
        let _ = app.view();
    }

    #[test]
    fn losing_the_phone_clears_the_dim_of_a_cut() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = on_the_phone(&files);
        command(&mut app, Code::KeyX);

        let _ = app.update(Message::DevicePane(explorer::Message::Pasted {
            volume_id: 5,
            path: "/".into(),
            result: Err(to_error_response(dto::Error::Disconnected)),
        }));

        assert!(!app.device_pane.is_open());
        assert!(!app.device_pane.is_cut("a.jpg"));
    }

    /// Everything that can take the phone away from the app.
    fn lose_the_phone(app: &mut App, how: &str) {
        match how {
            "unplugged" => {
                let _ = app.update(Message::Device(device::Message::Disconnected));
            }
            "pane lost it" => {
                let _ = app.update(Message::DevicePane(explorer::Message::Pasted {
                    volume_id: 5,
                    path: "/".into(),
                    result: Err(to_error_response(dto::Error::Disconnected)),
                }));
            }
            "transfer lost it" => {
                let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
                    to_error_response(dto::Error::Disconnected),
                ))));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn losing_the_phone_drops_what_was_copied_or_cut_there() {
        // Another phone plugged in next can have the same storage id and the
        // same folders; a paste must not act on its files.
        for how in ["unplugged", "pane lost it", "transfer lost it"] {
            for cut in [false, true] {
                let files = Arc::new(FakeExplorer::new(Ok(vec![])));
                let mut app = on_the_phone(&files);
                command(&mut app, if cut { Code::KeyX } else { Code::KeyC });
                assert!(clipboard(&app).is_some(), "{how}, cut={cut}");

                lose_the_phone(&mut app, how);

                assert_eq!(clipboard(&app), None, "{how}, cut={cut}");
                assert!(!app.device_pane.is_cut("a.jpg"), "{how}, cut={cut}");
                assert!(!app.can_paste() && !app.can_paste());
            }
        }
    }

    #[test]
    fn losing_the_phone_keeps_what_was_copied_on_the_mac() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Select("notes.txt".into()),
        )));
        command(&mut app, Code::KeyC);

        lose_the_phone(&mut app, "unplugged");

        assert_eq!(clipboard(&app), Some((Side::Mac, 1, false)));
    }

    #[test]
    fn switching_storage_keeps_a_cut_and_its_dimming() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = app_on(Arc::clone(&files));
        connect(&mut app, &[(5, "Internal"), (6, "SD card")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        command(&mut app, Code::Digit1);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        command(&mut app, Code::KeyX);
        assert!(app.device_pane.is_cut("a.jpg"));

        // Over to the SD card (a paste there would move it across), and back.
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::SelectStorage(6),
        )));
        assert_eq!(app.device_pane.location(), Some((6, "/")));
        assert!(app.can_paste(), "the cut can be pasted on the SD card");
        let _ = app.update(Message::Device(device::Message::Request(
            device::Request::SelectStorage(5),
        )));

        assert_eq!(clipboard(&app), Some((Side::Phone, 1, true)));
        assert!(
            app.device_pane.is_cut("a.jpg"),
            "still dimmed where it was cut"
        );
    }

    #[tokio::test]
    async fn a_finished_move_refreshes_the_phone_pane_even_if_the_mac_pane_moved_on() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(Arc::clone(&files));
        // The user went somewhere else on the Mac while the move ran.
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Elsewhere".into()),
        )));
        assert_ne!(app.local_pane.current_path(), Some("/Users/me"));
        files.allowed.lock().unwrap().clear();

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Ok(moved_summary(1)))),
        )
        .await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DevicePane(explorer::Message::Listed { .. }))),
            "the phone pane still shows what left"
        );
        assert_eq!(*files.allowed.lock().unwrap(), [true]);
    }

    // ---- a phone that cannot copy or move by itself ----

    /// A phone pane that answers every paste "I can't", with `a.jpg` selected
    /// and the transfer service a test can look at.
    fn phone_that_cannot_paste() -> (App, Arc<FakeExplorer>, Arc<FakeTransfer>) {
        let files = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::Unsupported,
        )]));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with(Arc::clone(&files), Arc::clone(&transfers));
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        command(&mut app, Code::Digit1);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        (app, files, transfers)
    }

    #[tokio::test]
    async fn a_copy_the_phone_cannot_do_is_queued_through_the_mac_and_can_be_pasted_again() {
        let (mut app, _files, transfers) = phone_that_cannot_paste();
        command(&mut app, Code::KeyC);

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        assert_eq!(
            *transfers.pastes.lock().unwrap(),
            [dto::PasteRequest {
                mode: dto::Paste::Copy,
                from_storage: 5,
                from: "/".into(),
                names: vec!["a.jpg".into()],
                to_storage: 5,
                to: "/".into(),
            }]
        );
        assert_eq!(
            clipboard(&app),
            Some((Side::Phone, 1, false)),
            "a copy can be pasted again"
        );
        assert!(!app.device_pane.is_pasting(), "the pane is not left busy");
        let _ = app.view();
    }

    #[tokio::test]
    async fn a_cut_the_phone_cannot_do_is_queued_as_a_move_and_used_up() {
        let (mut app, _files, transfers) = phone_that_cannot_paste();
        command(&mut app, Code::KeyX);

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        let pastes = transfers.pastes.lock().unwrap();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].mode, dto::Paste::Move);
        assert_eq!(clipboard(&app), None, "queued, so it is used up");
        assert!(!app.device_pane.is_cut("a.jpg"), "no longer dimmed");
    }

    #[tokio::test]
    async fn the_destination_is_the_folder_the_phone_pane_shows_when_the_paste_hands_over() {
        let (mut app, _files, transfers) = phone_that_cannot_paste();
        command(&mut app, Code::KeyC);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::GoTo("/Music".into()),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/Music".into(),
            result: Ok(vec![]),
        }));

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        let pastes = transfers.pastes.lock().unwrap();
        assert_eq!(
            (pastes[0].from.as_str(), pastes[0].to.as_str()),
            ("/", "/Music")
        );
    }

    #[tokio::test]
    async fn one_job_is_queued_for_each_folder_that_was_copied() {
        let (mut app, _files, transfers) = phone_that_cannot_paste();
        command(&mut app, Code::KeyC);
        app.clipboard
            .as_mut()
            .unwrap()
            .selections
            .push(explorer::Selection {
                volume_id: 5,
                path: "/DCIM".into(),
                names: vec!["b.jpg".into(), "c.jpg".into()],
            });

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        let sources: Vec<_> = transfers
            .pastes
            .lock()
            .unwrap()
            .iter()
            .map(|request| (request.from.clone(), request.names.len()))
            .collect();
        assert_eq!(sources, [("/".to_owned(), 1), ("/DCIM".to_owned(), 2)]);
    }

    #[tokio::test]
    async fn a_phone_that_can_paste_by_itself_never_involves_the_mac() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with(Arc::clone(&files), Arc::clone(&transfers));
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        command(&mut app, Code::Digit1);
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        command(&mut app, Code::KeyC);

        let task = phone_key(&mut app, Code::KeyV);
        settle_task(&mut app, task).await;

        assert_eq!(files.pastes.lock().unwrap().len(), 1, "the phone did it");
        assert!(transfers.pastes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_copy_through_the_mac_that_finished_shows_the_new_files_in_the_phone_pane() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = two_listed_panes_on(Arc::clone(&files));
        files.allowed.lock().unwrap().clear();
        let summary = dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: 1,
            destination: "/".into(),
            first_item: Some("a (1).jpg".into()),
            through_mac: true,
            ..Default::default()
        };

        let messages = sent_by(
            &mut app,
            Message::Transfer(transfer::Message::Finished(Ok(summary))),
        )
        .await;

        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DevicePane(explorer::Message::Listed { .. }))),
            "the phone pane read the folder the copies went into"
        );
        assert!(
            reloads_space(&messages),
            "and the phone's free space was asked"
        );
    }

    // ---- dragging onto a folder ----

    /// The phone pane lists a.jpg and the folder DCIM; its pastes go to `files`
    /// and its copies through the Mac to `transfers`.
    fn app_with_a_folder(files: &Arc<FakeExplorer>, transfers: &Arc<FakeTransfer>) -> App {
        let mut app = app_with(Arc::clone(files), Arc::clone(transfers));
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg"), folder("DCIM")]),
        }));
        app
    }

    fn phone_request(request: explorer::Request) -> Message {
        Message::DevicePane(explorer::Message::Request(request))
    }

    /// Press a.jpg, drag it onto DCIM, and let go.
    async fn drag_a_jpg_onto_dcim(app: &mut App) {
        for request in [
            explorer::Request::Select("a.jpg".into()),
            explorer::Request::PointerLeft("a.jpg".into()),
            explorer::Request::PointerEntered("DCIM".into()),
        ] {
            let _ = app.update(phone_request(request));
        }
        let task = app.update(Message::PointerReleased);
        settle_task(app, task).await;
    }

    #[tokio::test]
    async fn a_drag_dropped_on_a_folder_moves_the_file_into_it() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = app_with_a_folder(&files, &Arc::new(default_transfer()));

        drag_a_jpg_onto_dcim(&mut app).await;

        assert_eq!(
            *files.pastes.lock().unwrap(),
            [dto::PasteRequest {
                mode: dto::Paste::Move,
                from_storage: 5,
                from: "/".into(),
                names: vec!["a.jpg".into()],
                to_storage: 5,
                to: "/DCIM".into(),
            }]
        );
        let _ = app.view();
    }

    #[tokio::test]
    async fn holding_alt_while_letting_go_copies() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = app_with_a_folder(&files, &Arc::new(default_transfer()));
        let _ = app.update(Message::Keyboard(keyboard::Event::ModifiersChanged(
            Modifiers::ALT,
        )));

        drag_a_jpg_onto_dcim(&mut app).await;

        assert_eq!(files.pastes.lock().unwrap()[0].mode, dto::Paste::Copy);
    }

    #[tokio::test]
    async fn a_drop_on_a_folder_of_a_phone_that_cannot_move_goes_through_the_mac_into_that_folder()
    {
        let files = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::Unsupported,
        )]));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with_a_folder(&files, &transfers);

        drag_a_jpg_onto_dcim(&mut app).await;

        assert_eq!(
            *transfers.pastes.lock().unwrap(),
            [dto::PasteRequest {
                mode: dto::Paste::Move,
                from_storage: 5,
                from: "/".into(),
                names: vec!["a.jpg".into()],
                to_storage: 5,
                to: "/DCIM".into(),
            }],
            "into the folder it was dropped on, not the one shown"
        );
    }

    #[tokio::test]
    async fn letting_go_changes_which_pane_is_active_for_nobody() {
        let files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = app_with_a_folder(&files, &Arc::new(default_transfer()));
        assert_eq!(app.target(), Side::Mac);

        let task = app.update(Message::PointerReleased);
        settle_task(&mut app, task).await;

        assert_eq!(app.target(), Side::Mac, "a release is nobody's click");
        assert!(files.pastes.lock().unwrap().is_empty());
    }

    #[test]
    fn only_the_left_button_coming_up_ends_a_press() {
        use iced::mouse::{Button, Event as Mouse};
        let seen =
            |event| pointer_released(event, iced::event::Status::Ignored, window::Id::unique());

        assert!(matches!(
            seen(iced::Event::Mouse(Mouse::ButtonReleased(Button::Left))),
            Some(Message::PointerReleased)
        ));
        assert!(
            matches!(
                pointer_released(
                    iced::Event::Mouse(Mouse::ButtonReleased(Button::Left)),
                    iced::event::Status::Captured,
                    window::Id::unique()
                ),
                Some(Message::PointerReleased)
            ),
            "whichever widget had it"
        );
        assert!(seen(iced::Event::Mouse(Mouse::ButtonReleased(Button::Right))).is_none());
        assert!(seen(iced::Event::Mouse(Mouse::ButtonPressed(Button::Left))).is_none());
        assert!(
            seen(iced::Event::Mouse(Mouse::CursorMoved {
                position: iced::Point::ORIGIN
            }))
            .is_none()
        );
    }

    // ---- a phone that goes silent in the middle of an edit ----

    #[test]
    fn a_phone_that_goes_silent_during_an_edit_leaves_the_pane_and_offers_reconnect() {
        // What the explorer service ends as when the phone never answers a
        // request: `Timeout`, which the app takes for a lost phone.
        let timeout = || to_error_response(dto::Error::Timeout);
        type Ending = fn(crate::handler::error::ErrorResponse) -> explorer::Message;
        let endings: [(&str, Ending); 3] = [
            ("listing", |error| explorer::Message::Listed {
                volume_id: 5,
                path: "/".into(),
                result: Err(error),
            }),
            ("editing", |error| explorer::Message::Edited {
                volume_id: 5,
                path: "/".into(),
                result: Err(error),
                undo: None,
            }),
            ("pasting", |error| explorer::Message::Pasted {
                volume_id: 5,
                path: "/".into(),
                result: Err(error),
            }),
        ];
        for (what, ending) in endings {
            let mut app = two_listed_panes();
            // A rename is waiting on the phone with its dialog up.
            for request in [
                explorer::Request::Select("a.jpg".into()),
                explorer::Request::Rename,
                explorer::Request::Submit,
            ] {
                let _ = app.update(Message::DevicePane(explorer::Message::Request(request)));
            }
            assert!(app.device_pane.is_editing(), "{what}: the dialog is up");

            let _ = app.update(Message::DevicePane(ending(timeout())));

            assert!(
                !app.device_pane.is_open(),
                "{what}: the pane gave up waiting"
            );
            assert!(
                !app.device_pane.is_editing(),
                "{what}: and its dialog went with it"
            );
            let mut ui = iced_test::simulator(app.device.view_main());
            assert!(ui.find("Your phone stopped responding").is_ok(), "{what}");
            assert!(ui.find("Reconnect").is_ok(), "{what}");
        }
    }

    /// The phone and Mac panes listed, with a search that finds "trip a.jpg"
    /// and "trip b.jpg" in DCIM › Camera and "trip c.mp3" in Music.
    fn searching_app() -> (
        App,
        Arc<crate::handler::testing::FakeSearch>,
        Arc<FakeTransfer>,
    ) {
        let search = Arc::new(crate::handler::testing::FakeSearch::finding(&[
            ("/DCIM/Camera", "trip a.jpg", false),
            ("/DCIM/Camera", "trip b.jpg", false),
            ("/Music", "trip c.mp3", false),
        ]));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with_search(
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            transfers.clone(),
            search.clone(),
        );
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        }));
        (app, search, transfers)
    }

    /// ⇧⌘F: search the whole phone.
    fn search_key(app: &mut App) -> Task<Message> {
        app.update(key_press(
            Key::Character("f".into()),
            Code::KeyF,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ))
    }

    fn search_request(request: search::Request) -> Message {
        Message::Search(search::Message::Request(request))
    }

    /// Opens the search with ⇧⌘F, lets the walk finish and types `query`.
    async fn open_and_type(app: &mut App, query: &str) {
        let task = search_key(app);
        feed_back(app, task).await;
        let _ = app.update(search_request(search::Request::Query(query.into())));
    }

    #[tokio::test]
    async fn shift_command_f_searches_the_storage_the_phone_pane_shows() {
        let mut app = app();
        let task = search_key(&mut app);
        feed_back(&mut app, task).await;
        assert!(!app.search.is_open(), "no phone, nothing to search");

        let (mut app, search, _) = searching_app();
        app.active = Side::Mac;
        let task = search_key(&mut app);
        feed_back(&mut app, task).await;

        assert!(app.search.is_open());
        assert_eq!(app.active, Side::Phone);
        assert_eq!(app.search.storage_id(), Some(5));
        assert_eq!(*search.walks.lock().unwrap(), [(5, false)]);
        let _ = app.update(search_request(search::Request::Query("trip".into())));
        let mut ui = iced_test::simulator(app.view());
        assert!(ui.find("Look again").is_ok(), "in the phone pane's place");
        assert!(ui.find("trip c.mp3").is_ok());
    }

    #[tokio::test]
    async fn the_search_button_opens_it_and_walks_hidden_folders_when_they_are_shown() {
        let (mut app, search, _) = searching_app();
        settings_request(&mut app, settings::Request::SetShowHiddenOnPhone(true));

        settle(
            &mut app,
            Message::DevicePane(explorer::Message::Request(explorer::Request::Search)),
        )
        .await;

        assert!(app.search.is_open());
        assert_eq!(*search.walks.lock().unwrap(), [(5, true)]);
    }

    #[tokio::test]
    async fn keys_go_to_the_search_while_it_is_open() {
        let (mut app, _, _) = searching_app();
        open_and_type(&mut app, "trip").await;

        command(&mut app, Code::KeyA);
        assert_eq!(app.search.selected(), 3);
        command(&mut app, Code::KeyC);
        assert_eq!(
            clipboard(&app),
            None,
            "the phone pane underneath is not copied"
        );

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );
        assert!(!app.search.is_open());
        assert!(app.device_pane.is_open());
    }

    #[tokio::test]
    async fn downloading_results_copies_each_folders_files_into_the_mac_folder() {
        let (mut app, _, transfers) = searching_app();
        open_and_type(&mut app, "trip").await;
        command(&mut app, Code::KeyA);

        let task = app.update(search_request(search::Request::Download));
        settle_task(&mut app, task).await;

        let requests = transfers.requests.lock().unwrap();
        let asked: Vec<(u64, &str, Vec<&str>, &str)> = requests
            .iter()
            .map(|r| {
                (
                    r.storage_id,
                    r.source.as_str(),
                    r.names.iter().map(String::as_str).collect(),
                    r.destination.as_str(),
                )
            })
            .collect();
        assert_eq!(
            asked,
            [
                (
                    5,
                    "/DCIM/Camera",
                    vec!["trip a.jpg", "trip b.jpg"],
                    "/Users/me"
                ),
                (5, "/Music", vec!["trip c.mp3"], "/Users/me"),
            ]
        );
        assert!(
            transfers
                .modes
                .lock()
                .unwrap()
                .iter()
                .all(|m| *m == dto::Paste::Copy)
        );
        assert!(app.search.is_open(), "the results stay to pick more");
    }

    #[tokio::test]
    async fn show_in_folder_closes_the_search_and_selects_the_file_in_its_folder() {
        let (mut app, _, _) = searching_app();
        open_and_type(&mut app, "c.mp3").await;
        let _ = app.update(search_request(search::Request::Select(
            "/Music/trip c.mp3".into(),
        )));

        let _ = app.update(search_request(search::Request::ShowInFolder));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/Music".into(),
            result: Ok(vec![file("b.mp3"), file("trip c.mp3")]),
        }));

        assert!(!app.search.is_open());
        assert_eq!(app.device_pane.location(), Some((5, "/Music")));
        assert_eq!(
            app.device_pane.selection().map(|s| s.names),
            Some(vec!["trip c.mp3".to_owned()])
        );
    }

    #[tokio::test]
    async fn a_walk_that_loses_the_phone_closes_the_phone_side_and_offers_reconnect() {
        let (mut app, _, _) = searching_app();
        open_and_type(&mut app, "trip").await;

        let _ = app.update(Message::Search(search::Message::Walked {
            walk: 1,
            result: Err(to_error_response(dto::Error::Timeout)),
        }));

        assert!(!app.search.is_open());
        assert!(!app.device_pane.is_open());
        assert_eq!(app.device.selected_storage(), None);
        let mut ui = iced_test::simulator(app.device.view_main());
        assert!(ui.find("Reconnect").is_ok());
    }

    #[tokio::test]
    async fn losing_the_phone_closes_its_search() {
        let (mut app, _, _) = searching_app();
        let task = search_key(&mut app);
        feed_back(&mut app, task).await;

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Timeout),
        ))));

        assert!(!app.search.is_open());
        let task = search_key(&mut app);
        feed_back(&mut app, task).await;
        assert!(!app.search.is_open(), "nothing to search until it is back");
    }

    /// Pixel 9 (ABC123, what a plain connect opens) and Galaxy S24 (S24)
    /// plugged in, both with a storage 5 called "Internal"; Pixel 9 open with
    /// a.jpg listed.
    async fn two_phones() -> (App, Arc<FakeTransfer>) {
        let device = Arc::new(FakeDevice {
            phones: vec![
                crate::handler::testing::plugged("ABC123", "Pixel 9"),
                crate::handler::testing::plugged("S24", "Galaxy S24"),
            ],
            ..FakeDevice::with_storages(vec![storage(5, "Internal")])
        });
        let transfers = Arc::new(default_transfer());
        let mut app = app_built(
            device,
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            transfers.clone(),
            Arc::new(crate::handler::testing::FakeSearch::default()),
        );
        settle_task(
            &mut app,
            Task::done(Message::Device(device::Message::Request(
                device::Request::Connect,
            ))),
        )
        .await;
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        (app, transfers)
    }

    fn open_phone(key: &str) -> Message {
        Message::Device(device::Message::Request(device::Request::Open(key.into())))
    }

    fn open_phone_key(app: &App) -> Option<String> {
        app.device.opened().map(|(key, ..)| key)
    }

    fn says(element: Element<'_, device::Message>, words: &str) -> bool {
        iced_test::simulator(element).find(words).is_ok()
    }

    #[tokio::test]
    async fn switching_phones_waits_for_the_transfers_on_the_open_one() {
        let (mut app, _) = two_phones().await;
        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(app.device_pane.is_open());
        assert!(says(
            app.device.view_sidebar(),
            "Wait for the transfers on Pixel 9 to finish, or cancel them, then switch."
        ));
    }

    #[tokio::test]
    async fn a_paused_transfer_still_holds_the_open_phone() {
        let (mut app, _) = two_phones().await;
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });
        let _ = app.update(Message::Transfer(transfer::Message::Request(
            transfer::Request::Pause,
        )));
        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Cancelled),
        ))));
        assert!(app.transfer.is_paused());

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(says(
            app.device.view_sidebar(),
            "Wait for the transfers on Pixel 9 to finish, or cancel them, then switch."
        ));
    }

    #[tokio::test]
    async fn switching_phones_waits_for_an_edit_on_the_open_one() {
        let (mut app, _) = two_phones().await;
        for request in [
            explorer::Request::Select("a.jpg".into()),
            explorer::Request::Rename,
        ] {
            let _ = app.update(Message::DevicePane(explorer::Message::Request(request)));
        }

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(says(
            app.device.view_sidebar(),
            "Finish what is going on in Pixel 9’s files first."
        ));
    }

    #[tokio::test]
    async fn switching_phones_shows_the_other_phone_even_where_its_storage_looks_the_same() {
        let (mut app, _) = two_phones().await;
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        command(&mut app, Code::KeyC);
        assert!(clipboard(&app).is_some());

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("S24"));
        assert_eq!(app.device_pane.location(), Some((5, "/")));
        assert_eq!(
            app.device_pane.selection(),
            None,
            "the pane was opened afresh"
        );
        assert_eq!(clipboard(&app), None, "a.jpg was on the other phone");
    }

    fn failed_download(app: &mut App) {
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });
        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Cancelled),
        ))));
    }

    fn offers_retry(app: &App) -> bool {
        iced_test::simulator(app.transfer.view())
            .find("Retry")
            .is_ok()
    }

    #[tokio::test]
    async fn a_failed_transfer_is_not_offered_again_on_another_phone() {
        let (mut app, _) = two_phones().await;
        failed_download(&mut app);
        assert!(offers_retry(&app));

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("S24"));
        assert!(!offers_retry(&app));
    }

    #[tokio::test]
    async fn a_failed_transfer_is_still_offered_when_the_same_phone_comes_back() {
        let (mut app, _) = two_phones().await;
        failed_download(&mut app);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));
        settle_task(
            &mut app,
            Task::done(Message::Device(device::Message::Request(
                device::Request::Connect,
            ))),
        )
        .await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(offers_retry(&app));
    }

    fn download_from_phone(app: &mut App) {
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });
    }

    #[tokio::test]
    async fn a_download_cut_off_by_a_lost_phone_carries_on_when_the_same_phone_is_back() {
        let (mut app, transfers) = two_phones().await;
        download_from_phone(&mut app);

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));
        assert!(!app.device_pane.is_open(), "the phone is gone");
        assert!(app.transfer.waits_for_phone());

        settle_task(
            &mut app,
            Task::done(Message::Device(device::Message::Request(
                device::Request::Connect,
            ))),
        )
        .await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(!app.transfer.is_paused(), "it carried on and finished");
        assert_eq!(
            transfers.requests.lock().unwrap().len(),
            1,
            "run again on the phone that came back"
        );
        assert!(!offers_retry(&app), "it went through");
    }

    #[tokio::test]
    async fn a_download_waiting_for_a_lost_phone_ends_when_another_phone_opens() {
        let (mut app, transfers) = two_phones().await;
        download_from_phone(&mut app);
        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Timeout),
        ))));
        assert!(app.transfer.waits_for_phone());

        let other = dto::Device {
            key: "S24".into(),
            serial_number: "S24".into(),
            ..crate::handler::testing::phone()
        };
        let task = app.update(Message::Device(device::Message::connected(
            &other,
            &[storage(5, "Internal")],
        )));
        settle_task(&mut app, task).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("S24"));
        assert!(!app.transfer.is_paused());
        assert!(
            transfers.requests.lock().unwrap().is_empty(),
            "not run on it"
        );
        assert!(!offers_retry(&app), "its paths were the other phone's");
    }

    // Transfers kept from last time.

    fn kept_download(phone: Option<&str>) -> dto::SavedTransfer {
        dto::SavedTransfer {
            phone: phone.map(Into::into),
            job: dto::SavedJob::Download {
                request: dto::DownloadRequest {
                    storage_id: 5,
                    source: "/".into(),
                    names: vec!["a.jpg".into()],
                    destination: "/Users/me".into(),
                },
                mode: dto::Paste::Copy,
                answer: dto::SavedAnswer::Retried(dto::Conflict::KeepBoth),
            },
        }
    }

    fn kept_mac_paste() -> dto::SavedTransfer {
        dto::SavedTransfer {
            phone: None,
            job: dto::SavedJob::OnMac(dto::PasteRequest {
                mode: dto::Paste::Copy,
                from_storage: 0,
                from: "/Users/me".into(),
                names: vec!["big.mov".into()],
                to_storage: 0,
                to: "/Volumes/Backup".into(),
            }),
        }
    }

    /// Two phones plugged in (ABC123 opens) and `kept` from last time.
    async fn two_phones_with(kept: Vec<dto::SavedTransfer>) -> (App, Arc<FakeSettings>) {
        let device = Arc::new(FakeDevice {
            phones: vec![
                crate::handler::testing::plugged("ABC123", "Pixel 9"),
                crate::handler::testing::plugged("S24", "Galaxy S24"),
            ],
            ..FakeDevice::with_storages(vec![storage(5, "Internal")])
        });
        let settings = Arc::new(FakeSettings {
            transfers: std::sync::Mutex::new(kept),
            ..FakeSettings::default()
        });
        let app = app_built_with(
            device,
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(default_transfer()),
            Arc::new(crate::handler::testing::FakeSearch::default()),
            Arc::clone(&settings),
        );
        (app, settings)
    }

    async fn connect_first_phone(app: &mut App) {
        settle_task(
            app,
            Task::done(Message::Device(device::Message::Request(
                device::Request::Connect,
            ))),
        )
        .await;
    }

    #[tokio::test]
    async fn the_macs_own_transfers_are_offered_at_once_and_a_phones_when_it_opens() {
        let (mut app, settings) = two_phones_with(vec![
            kept_mac_paste(),
            kept_download(Some("ABC123")),
            kept_download(Some("S24")),
        ])
        .await;
        let offered = |app: &App| {
            iced_test::simulator(app.transfer.view())
                .find(format!("{} transfers weren't finished last time", 2).as_str())
                .is_ok()
        };
        assert!(
            iced_test::simulator(app.transfer.view())
                .find("1 transfer wasn't finished last time")
                .is_ok(),
            "the Mac's own, before any phone"
        );

        connect_first_phone(&mut app).await;

        assert!(offered(&app), "ABC123's joins it");
        assert!(!app.transfer.is_running(), "nothing runs unasked");
        let file = settings.transfers.lock().unwrap().clone();
        assert_eq!(file.len(), 3, "S24's still kept: {file:?}");
        assert!(file.contains(&kept_download(Some("S24"))));
    }

    #[tokio::test]
    async fn what_the_queue_has_to_do_is_kept_and_forgotten_once_done() {
        let (mut app, settings) = two_phones_with(Vec::new()).await;
        connect_first_phone(&mut app).await;

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });
        let _ = app.transfer.paste_on_mac(dto::PasteRequest {
            mode: dto::Paste::Copy,
            from_storage: MAC_VOLUME,
            from: "/Users/me".into(),
            names: vec!["big.mov".into()],
            to_storage: MAC_VOLUME,
            to: "/Users/me/Movies".into(),
        });
        let _ = app.update(Message::Transfer(transfer::Message::Progress(
            dto::TransferProgress::default(),
        )));
        let file = settings.transfers.lock().unwrap().clone();
        assert_eq!(file.len(), 2);
        assert_eq!(file[0].phone.as_deref(), Some("ABC123"));
        assert_eq!(file[1].phone, None, "the Mac's own copy needs no phone");

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Ok(summary(
            "/Users/me",
        )))));
        assert!(
            settings.transfers.lock().unwrap().is_empty(),
            "the download is done, and the Mac copy now running can't carry on by itself (it would number its copies), so nothing is kept"
        );
    }

    #[tokio::test]
    async fn keeping_the_queue_costs_nothing_while_it_is_unchanged() {
        let (mut app, settings) = two_phones_with(Vec::new()).await;
        connect_first_phone(&mut app).await;
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/DCIM/Camera".into(),
            names: (0..5000).map(|i| format!("IMG_2026{i:06}.jpg")).collect(),
            destination: "/Users/me".into(),
        });
        app.keep_transfers();
        assert!(matches!(
            &settings.transfers.lock().unwrap()[0].job,
            dto::SavedJob::Download { request, .. } if request.names.len() == 5000
        ));

        // Comparing 5,000 names took 134 µs a time on a release build, after
        // every message (a mouse move is one); unchanged, it is skipped.
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            app.keep_transfers();
        }
        let took = start.elapsed();
        assert!(took < Duration::from_millis(20), "{took:?} for 1,000");

        let _ = app.transfer.paste_on_mac(dto::PasteRequest {
            mode: dto::Paste::Copy,
            from_storage: MAC_VOLUME,
            from: "/Users/me".into(),
            names: vec!["big.mov".into()],
            to_storage: MAC_VOLUME,
            to: "/Users/me/Movies".into(),
        });
        app.keep_transfers();
        assert_eq!(
            settings.transfers.lock().unwrap().len(),
            2,
            "a change is kept"
        );
    }

    #[tokio::test]
    async fn another_phone_opening_keeps_what_was_offered_for_the_last_one() {
        let (mut app, settings) = two_phones_with(vec![kept_download(Some("ABC123"))]).await;
        connect_first_phone(&mut app).await;
        assert!(app.transfer.is_visible());

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert!(!app.transfer.is_visible(), "not offered on S24");
        assert_eq!(
            *settings.transfers.lock().unwrap(),
            [kept_download(Some("ABC123"))],
            "kept for when it comes back"
        );
        settle_task(&mut app, Task::done(open_phone("ABC123"))).await;
        assert!(app.transfer.is_visible(), "offered again");
    }

    #[tokio::test]
    async fn a_finished_transfer_asks_the_phone_how_charged_it_is() {
        let device = Arc::new(FakeDevice::with_storages(vec![storage(5, "Internal")]));
        *device.battery.lock().unwrap() = vec![Ok(Some(60)), Ok(Some(58))];
        let mut app = app_built(
            device.clone(),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(default_transfer()),
            Arc::new(crate::handler::testing::FakeSearch::default()),
        );
        settle_task(
            &mut app,
            Task::done(Message::Device(device::Message::Request(
                device::Request::Connect,
            ))),
        )
        .await;

        settle_task(
            &mut app,
            Task::done(Message::Transfer(transfer::Message::Finished(Ok(summary(
                "/Users/me",
            ))))),
        )
        .await;

        assert_eq!(
            device
                .battery_asks
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert!(
            iced_test::simulator(app.device.view_sidebar())
                .find("Android 15 · Battery 58%")
                .is_ok()
        );
    }

    // Undo.

    #[tokio::test]
    async fn command_z_undoes_the_latest_change_whichever_pane_made_it() {
        let phone_files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mac_files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut app = app_with_panes(
            phone_files.clone(),
            mac_files.clone(),
            Arc::new(default_transfer()),
        );
        connect(&mut app, &[(5, "Internal")]);
        let renamed = |from: &str, to: &str, folder: &str| {
            Some(crate::handler::explorer::undo::Undo::Renamed {
                folder: folder.into(),
                from: from.into(),
                to: to.into(),
            })
        };
        assert!(!app.menu_state().can_undo, "nothing done yet");
        let _ = app.update(Message::LocalPane(explorer::Message::Edited {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(Some("b.txt".into())),
            undo: renamed("a.txt", "b.txt", "/Users/me"),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Edited {
            volume_id: 5,
            path: "/".into(),
            result: Ok(Some("y.jpg".into())),
            undo: renamed("x.jpg", "y.jpg", "/"),
        }));
        assert!(app.menu_state().can_undo);

        command(&mut app, Code::KeyZ);

        assert!(
            !app.device_pane.can_undo(),
            "⌘Z took the phone's, the latest"
        );
        assert!(app.local_pane.can_undo(), "the Mac's older rename is left");
        assert!(
            !app.menu_state().can_undo,
            "as in Finder, only the latest change is undone"
        );
        assert!(mac_files.edits.lock().unwrap().is_empty());
    }

    // A folder macOS refused.

    #[test]
    fn coming_back_to_the_app_reads_a_refused_mac_folder_again() {
        let mut app = two_listed_panes();
        assert_eq!(
            app.update(Message::Focused(true)).units(),
            0,
            "nothing refused: nothing read"
        );
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Err(crate::handler::error::to_error_response(
                dto::Error::FolderAccessDenied("/Users/me/Desktop".into()),
            )),
        }));
        assert!(shows(&app, "Open Privacy Settings…"));

        let _ = app.update(Message::Focused(false));
        assert!(!app.local_pane.is_loading(), "going away reads nothing");
        assert!(app.update(Message::Focused(true)).units() > 0);
        assert!(app.local_pane.is_loading());
    }

    // Typing to select.

    fn type_letter(app: &mut App, letter: &str, code: Code) {
        press(app, Key::Character(letter.into()), code, Modifiers::empty());
    }

    #[test]
    fn typing_selects_a_name_in_the_active_pane() {
        let mut app = two_listed_panes();

        type_letter(&mut app, "s", Code::KeyS);

        assert_eq!(app.local_pane.selected_count(), 1);
        assert_eq!(
            app.local_pane.selection().map(|selection| selection.names),
            Some(vec!["song.mp3".to_owned()])
        );
    }

    #[test]
    fn typing_goes_nowhere_while_a_dialog_or_the_phone_search_is_up() {
        let mut app = two_listed_panes();
        settings_request(&mut app, settings::Request::Open);
        type_letter(&mut app, "s", Code::KeyS);
        assert_eq!(app.local_pane.selected_count(), 0, "Settings is up");
        settings_request(&mut app, settings::Request::Close);

        command(&mut app, Code::Digit1);
        let _ = app.open_search();
        type_letter(&mut app, "a", Code::KeyA);
        assert_eq!(
            app.device_pane.selected_count(),
            0,
            "the search panel stands in for the phone pane"
        );
    }

    // Back and forward.

    #[test]
    fn command_bracket_and_the_side_buttons_go_back_and_forward_in_the_active_pane() {
        let mut app = two_listed_panes();
        assert!(!app.menu_state().can_go_back, "nowhere yet");
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Desktop".into()),
        )));
        assert!(app.menu_state().can_go_back && !app.menu_state().can_go_forward);

        command(&mut app, Code::BracketLeft);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"));
        assert!(app.menu_state().can_go_forward, "Forward is offered now");

        let _ = app.update(Message::SideButton(true));
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Desktop"));
        let _ = app.update(Message::SideButton(false));
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"));
        command(&mut app, Code::BracketRight);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Desktop"));

        let _ = choose_from_menu(
            &mut app,
            menubar::Command::Key(Shortcut::Pane(explorer::Request::Back)),
        );
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"));
    }

    #[test]
    fn only_the_side_buttons_go_back_and_forward() {
        let id = window::Id::unique();
        let status = iced::event::Status::Ignored;
        let press = |button| iced::Event::Mouse(iced::mouse::Event::ButtonPressed(button));

        assert!(matches!(
            side_button(press(iced::mouse::Button::Back), status, id),
            Some(Message::SideButton(false))
        ));
        assert!(matches!(
            side_button(press(iced::mouse::Button::Forward), status, id),
            Some(Message::SideButton(true))
        ));
        assert!(side_button(press(iced::mouse::Button::Left), status, id).is_none());
    }

    // Opening where each pane was left.

    fn with_folders(
        folders: dto::LastFolders,
    ) -> (App, Arc<FakeExplorer>, Arc<FakeExplorer>, Arc<FakeSettings>) {
        let settings = Arc::new(FakeSettings {
            folders: std::sync::Mutex::new(folders),
            ..FakeSettings::default()
        });
        let phone_files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mac_files = Arc::new(FakeExplorer::new(Ok(vec![])));
        let app = app_built_with(
            Arc::new(FakeDevice::with_storages(vec![storage(5, "Internal")])),
            phone_files.clone(),
            mac_files.clone(),
            Arc::new(default_transfer()),
            Arc::new(crate::handler::testing::FakeSearch::default()),
            settings.clone(),
        );
        (app, phone_files, mac_files, settings)
    }

    #[test]
    fn each_pane_opens_where_it_was_left_and_each_phone_has_its_own() {
        let mut folders = dto::LastFolders {
            mac: Some("/Users/me/Downloads".into()),
            ..dto::LastFolders::default()
        };
        folders.keep_phone(&phone().key, 5, "/DCIM/Camera");
        folders.keep_phone("another phone", 5, "/Music");
        let (mut app, ..) = with_folders(folders);

        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Downloads"));
        connect(&mut app, &[(5, "Internal")]);
        assert_eq!(
            app.device_pane.current_path(),
            Some("/DCIM/Camera"),
            "this phone's, not the other's"
        );
    }

    #[test]
    fn where_a_pane_goes_is_kept_only_when_it_changes() {
        let (mut app, _, _, settings) = with_folders(dto::LastFolders::default());
        connect(&mut app, &[(5, "Internal")]);
        let saves = || {
            settings
                .folders_saved
                .load(std::sync::atomic::Ordering::SeqCst)
        };
        let before = saves();

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::GoTo("/DCIM".into()),
        )));
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Desktop".into()),
        )));
        let after_moves = saves();
        let _ = app.update(Message::Focused(true));
        let _ = app.update(Message::Places(places::Message::Refresh));

        assert_eq!(after_moves, before + 2, "one save for each pane that moved");
        assert_eq!(saves(), after_moves, "nothing new: nothing saved");
        let kept = settings.folders.lock().unwrap().clone();
        assert_eq!(kept.mac.as_deref(), Some("/Users/me/Desktop"));
        assert_eq!(kept.phone(&phone().key, 5), Some("/DCIM"));
    }

    /// The Mac's Home and Desktop, then this phone's Camera and its storage's
    /// top, Music on a storage it doesn't have now, and another phone's.
    fn recent_ones() -> dto::LastFolders {
        let mut folders = dto::LastFolders::default();
        folders.keep_mac("/Users/me/Desktop");
        folders.keep_mac("/Users/me");
        folders.keep_phone(&phone().key, 9, "/Music");
        folders.keep_phone(&phone().key, 5, "/DCIM/Camera");
        folders.keep_phone("another phone", 5, "/Movies");
        folders.keep_phone(&phone().key, 5, "/");
        folders
    }

    fn choose_tag(app: &mut App, tag: u32) -> Task<Message> {
        app.update(Message::Menu(menubar::Message::Chosen(tag)))
    }

    #[test]
    fn go_recent_folders_lists_the_open_phones_then_the_macs() {
        let (mut app, ..) = with_folders(recent_ones());
        assert_eq!(
            app.menu_state().recent,
            ["Home", "Desktop"],
            "no phone open: the Mac's only"
        );

        connect(&mut app, &[(5, "Internal")]);
        let name = app.device.open_name().unwrap().to_owned();

        assert_eq!(
            app.menu_state().recent,
            [
                format!("Internal — {name}"),
                format!("Camera — {name}"),
                "Home".to_owned(),
                "Desktop".to_owned()
            ],
            "Music's storage isn't there and the other phone's folder is its own"
        );
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Downloads".into()),
        )));
        assert_eq!(
            app.menu_state().recent[2],
            "Downloads",
            "a folder shown goes first at once"
        );
    }

    #[test]
    fn the_menu_bar_hears_of_a_folder_shown_in_the_same_update() {
        let (mut app, ..) = with_folders(recent_ones());
        let bar = Arc::new(crate::handler::testing::FakeMenuBar::default());
        app.menu = menubar::Handler::new(bar.clone());
        let _ = app.update(Message::Focused(true));

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Downloads".into()),
        )));

        let filled = bar.filled.lock().unwrap().clone();
        let (_, entries) = filled.last().expect("the submenu was filled again");
        assert!(
            matches!(
                entries.first(),
                Some(dto::MenuEntry::Command { title, .. }) if title == "Downloads"
            ),
            "{entries:?}"
        );
    }

    #[test]
    fn a_recent_folder_chosen_opens_in_its_pane() {
        let (mut app, ..) = with_folders(recent_ones());
        connect(&mut app, &[(5, "Internal")]);

        let _ = choose_tag(&mut app, menubar::RECENT_TAG + 3);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Desktop"));
        assert_eq!(app.target(), Side::Mac);

        let camera = app
            .menu_state()
            .recent
            .iter()
            .position(|label| label.starts_with("Camera"))
            .unwrap() as u32;
        let _ = choose_tag(&mut app, menubar::RECENT_TAG + camera);
        assert_eq!(app.device_pane.current_path(), Some("/DCIM/Camera"));
        assert_eq!(app.target(), Side::Phone);

        let _ = choose_tag(&mut app, menubar::RECENT_TAG + 99);
        assert_eq!(
            app.device_pane.current_path(),
            Some("/DCIM/Camera"),
            "none such"
        );
    }

    #[test]
    fn clear_menu_forgets_the_recent_folders_but_not_where_panes_open() {
        let (mut app, _, _, settings) = with_folders(recent_ones());
        connect(&mut app, &[(5, "Internal")]);
        let clear = app.menu.tag_of(&menubar::Command::ClearRecent).unwrap();

        let _ = choose_tag(&mut app, clear);

        assert!(app.menu_state().recent.is_empty());
        let kept = settings.folders.lock().unwrap().clone();
        assert!(kept.recent_mac.is_empty() && kept.recent_phone.is_empty());
        assert_eq!(
            kept.mac.as_deref(),
            Some("/Users/me"),
            "saved, the rest kept"
        );
        assert_eq!(kept.phone(&phone().key, 5), Some("/"));
    }

    #[test]
    fn recent_mac_folders_are_named_as_finder_names_them() {
        let paths: Vec<String> = [
            "/Users/me",
            "/Users/me/Photos/2024",
            "/Volumes/Card/2024",
            "/",
            "/Users/me/Desktop",
        ]
        .map(String::from)
        .to_vec();

        assert_eq!(
            mac_recent_labels(&paths, "/Users/me"),
            ["Home", "2024 — Photos", "2024 — Card", "/", "Desktop"]
        );
        assert_eq!(
            phone_recent_label("/", "Internal shared storage", "Pixel 9"),
            "Internal shared storage — Pixel 9"
        );
        assert_eq!(
            phone_recent_label("/DCIM/Camera", "Internal", "Pixel 9"),
            "Camera — Pixel 9"
        );
    }

    #[test]
    fn a_folder_that_has_gone_since_opens_home_or_the_top() {
        let mut folders = dto::LastFolders {
            mac: Some("/Users/me/Gone".into()),
            ..dto::LastFolders::default()
        };
        folders.keep_phone(&phone().key, 5, "/Deleted");
        let (mut app, ..) = with_folders(folders);
        let gone = |path: &str| {
            Err(crate::handler::error::to_error_response(
                dto::Error::PathNotFound(path.into()),
            ))
        };

        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me/Gone".into(),
            result: gone("/Users/me/Gone"),
        }));
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"), "Home");
        assert!(!app.local_pane.can_go_back(), "not a step to go back to");

        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/Deleted".into(),
            result: gone("/Deleted"),
        }));
        assert_eq!(app.device_pane.current_path(), Some("/"));
    }

    // Resizing columns.

    #[test]
    fn a_resized_column_is_remembered_for_its_pane_and_put_back_next_time() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::ColumnResize(400.0));
        assert_eq!(
            app.settings.current().mac_columns,
            dto::ColumnWidths::default(),
            "nothing is being resized: the pointer is not followed"
        );

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::ResizeColumn(explorer::layout::Edge::Modified, 800),
        )));
        let _ = app.update(Message::ColumnResize(600.0));
        let _ = app.update(Message::ColumnResize(560.0));
        let _ = app.update(Message::PointerReleased);

        let settings = app.settings.current();
        assert_eq!(
            settings.mac_columns.modified, 210,
            "40 points left: 40 wider"
        );
        assert_eq!(
            settings.phone_columns,
            dto::ColumnWidths::default(),
            "the other pane keeps its own"
        );

        let mut next = two_listed_panes();
        next.settings = settings::Handler::new(Arc::new(FakeSettings {
            loaded: Some(settings),
            ..FakeSettings::default()
        }));
        let _ = next.apply_settings();
        assert_eq!(next.local_pane.column_widths().modified, 210);
    }

    #[test]
    fn only_pointer_movements_are_followed_for_a_resize() {
        let id = window::Id::unique();
        let status = iced::event::Status::Ignored;
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: iced::Point::new(321.0, 9.0),
        });
        assert!(matches!(
            resize_pointer(moved, status, id),
            Some(Message::ColumnResize(x)) if x == 321.0
        ));
        assert!(resize_pointer(iced::Event::Window(window::Event::Focused), status, id).is_none());
    }

    // Remembering the window and the panes' sorts.

    #[test]
    fn a_sort_by_kind_is_remembered_for_that_pane() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::SortBy(dto::SortKey::Kind),
        )));

        assert_eq!(
            app.settings.current().mac_order,
            dto::SortOrder {
                key: dto::SortKey::Kind,
                descending: false,
            }
        );
        assert_eq!(
            app.settings.current().phone_order,
            dto::SortOrder::default()
        );
    }

    #[test]
    fn a_header_click_is_remembered_for_that_pane_and_put_back_next_time() {
        let mut app = two_listed_panes();

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::SortBy(dto::SortKey::Size),
        )));

        let biggest_first = dto::SortOrder {
            key: dto::SortKey::Size,
            descending: true,
        };
        let settings = app.settings.current();
        assert_eq!(settings.phone_order, biggest_first);
        assert_eq!(
            settings.mac_order,
            dto::SortOrder::default(),
            "the other pane keeps its own"
        );

        // The next launch.
        let mut next = two_listed_panes();
        next.settings = settings::Handler::new(Arc::new(FakeSettings {
            loaded: Some(settings),
            ..FakeSettings::default()
        }));
        let _ = next.apply_settings();
        assert_eq!(next.device_pane.sort_order(), biggest_first);
        assert_eq!(next.local_pane.sort_order(), dto::SortOrder::default());
    }

    #[test]
    fn only_the_last_window_change_of_a_drag_is_read_back() {
        let mut app = two_listed_panes();
        let id = window::Id::unique();

        for _ in 0..3 {
            let wait = app.update(Message::WindowChanged(id));
            assert_eq!(wait.units(), 1, "a settle timer for each step");
        }

        assert_eq!(
            app.update(Message::WindowSettled(1)).units(),
            0,
            "a step with a later one after it reads nothing"
        );
        assert_eq!(
            app.update(Message::WindowSettled(3)).units(),
            1,
            "the last one asks the window where it is"
        );
    }

    #[test]
    fn the_window_is_kept_where_it_settled_and_full_screen_keeps_that_place() {
        let mut app = two_listed_panes();
        assert_eq!(
            app.update(Message::WindowSettled(0)).units(),
            0,
            "no window yet"
        );

        let _ = app.update(Message::WindowPlaced {
            mode: window::Mode::Windowed,
            position: Some(iced::Point::new(140.0, 90.0)),
            size: iced::Size::new(1320.0, 840.0),
        });
        let placed = dto::WindowPlace {
            x: 140,
            y: 90,
            width: 1320,
            height: 840,
            fullscreen: false,
        };
        assert_eq!(app.settings.current().window, Some(placed));

        let _ = app.update(Message::WindowPlaced {
            mode: window::Mode::Fullscreen,
            position: Some(iced::Point::ORIGIN),
            size: iced::Size::new(1512.0, 982.0),
        });
        assert_eq!(
            app.settings.current().window,
            Some(dto::WindowPlace {
                fullscreen: true,
                ..placed
            })
        );
    }

    #[test]
    fn only_moves_and_resizes_are_window_changes() {
        let id = window::Id::unique();
        let status = iced::event::Status::Ignored;
        let moved = iced::Event::Window(window::Event::Moved(iced::Point::ORIGIN));
        let resized = iced::Event::Window(window::Event::Resized(iced::Size::new(9.0, 9.0)));

        assert!(
            matches!(window_change(moved, status, id), Some(Message::WindowChanged(w)) if w == id)
        );
        assert!(window_change(resized, status, id).is_some());
        assert!(window_change(iced::Event::Window(window::Event::Focused), status, id).is_none());
    }

    // The menu bar.

    fn choose_from_menu(app: &mut App, command: menubar::Command) -> Task<Message> {
        let tag = app.menu.tag_of(&command).expect("the command is in a menu");
        app.update(Message::Menu(menubar::Message::Chosen(tag)))
    }

    fn path_asked(messages: &[Message]) -> Option<&str> {
        messages.iter().find_map(|message| match message {
            Message::LocalPane(explorer::Message::Listed { path, .. }) => Some(path.as_str()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn finders_keys_take_the_mac_pane_home_to_the_desktop_and_to_downloads() {
        let mut app = two_listed_panes();
        command(&mut app, Code::Digit1);
        assert_eq!(app.active, Side::Phone);
        let home = app.home.clone();

        let task = app.update(key_press(
            Key::Character("d".into()),
            Code::KeyD,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ));
        assert_eq!(app.active, Side::Mac, "the Mac pane is where it goes");
        assert_eq!(
            app.local_pane.current_path(),
            Some(format!("{home}/Desktop").as_str())
        );
        let _ = crate::handler::testing::drain(task).await;

        let _ = app.update(key_press(
            Key::Character("l".into()),
            Code::KeyL,
            Modifiers::COMMAND | Modifiers::ALT,
        ));
        assert_eq!(
            app.local_pane.current_path(),
            Some(format!("{home}/Downloads").as_str())
        );
        let _ = app.update(key_press(
            Key::Character("h".into()),
            Code::KeyH,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ));
        assert_eq!(app.local_pane.current_path(), Some(home.as_str()));

        // From the menu too.
        let task = choose_from_menu(
            &mut app,
            menubar::Command::Key(Shortcut::GoToMac(keymap::MacPlace::Downloads)),
        );
        let messages = crate::handler::testing::drain(task).await.0;
        assert_eq!(
            path_asked(&messages),
            Some(format!("{home}/Downloads").as_str())
        );
    }

    #[test]
    fn shift_command_period_shows_hidden_files_in_the_active_pane() {
        let mut app = two_listed_panes();
        let _ = app.update(key_press(
            Key::Character(".".into()),
            Code::Period,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ));
        let settings = app.settings.current();
        assert!(settings.show_hidden_on_mac && !settings.show_hidden_on_phone);
        assert!(app.menu_state().show_hidden, "ticked in View");
        let _ = app.update(key_press(
            Key::Character(".".into()),
            Code::Period,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ));
        assert!(!app.settings.current().show_hidden_on_mac, "and off again");

        command(&mut app, Code::Digit1);
        let _ = app.update(key_press(
            Key::Character(".".into()),
            Code::Period,
            Modifiers::COMMAND | Modifiers::SHIFT,
        ));
        assert!(app.settings.current().show_hidden_on_phone);
    }

    #[test]
    fn home_end_and_option_arrows_select_the_ends_of_the_active_pane() {
        let mut app = two_listed_panes();
        let _ = app.update(key_press(
            Key::Named(keyboard::key::Named::End),
            Code::End,
            Modifiers::empty(),
        ));
        assert_eq!(app.local_pane.selection().unwrap().names, ["song.mp3"]);
        let _ = app.update(key_press(
            Key::Named(keyboard::key::Named::ArrowUp),
            Code::ArrowUp,
            Modifiers::ALT,
        ));
        assert_eq!(app.local_pane.selection().unwrap().names, ["notes.txt"]);
        let _ = app.update(key_press(
            Key::Named(keyboard::key::Named::PageDown),
            Code::PageDown,
            Modifiers::SHIFT,
        ));
        assert_eq!(app.local_pane.selection().unwrap().names.len(), 2);
        assert_eq!(app.device_pane.selection(), None, "only the active pane");
    }

    /// The Mac pane at /Users/me holds a.jpg and b.jpg, the phone's top
    /// folder A.JPG (the same file) and c.jpg.
    fn comparing_panes() -> App {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.jpg"), file("b.jpg")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("A.JPG"), file("c.jpg")]),
        }));
        app
    }

    #[test]
    fn edit_selects_what_the_other_pane_lacks_or_has_in_the_active_pane() {
        let mut app = comparing_panes();
        assert!(app.menu_state().can_compare);

        let _ = choose_from_menu(
            &mut app,
            menubar::Command::SelectAgainst(explorer::Against::Missing),
        );
        assert_eq!(app.local_pane.selection().unwrap().names, ["b.jpg"]);
        assert!(
            iced_test::simulator(app.local_pane.view())
                .find("Selected 1 of 2: not in “Internal”")
                .is_ok()
        );

        command(&mut app, Code::Digit1);
        let _ = choose_from_menu(
            &mut app,
            menubar::Command::SelectAgainst(explorer::Against::Same),
        );
        assert_eq!(app.device_pane.selection().unwrap().names, ["A.JPG"]);
    }

    #[test]
    fn the_right_click_menu_selects_against_the_other_pane_too() {
        let mut app = comparing_panes();
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Menu(explorer::MenuAction::SelectNotInOther),
        )));
        assert_eq!(app.active, Side::Phone);
        assert_eq!(app.device_pane.selection().unwrap().names, ["c.jpg"]);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::Menu(explorer::MenuAction::SelectAlsoInOther),
        )));
        assert_eq!(app.local_pane.selection().unwrap().names, ["a.jpg"]);
    }

    #[test]
    fn check_contents_queues_the_active_panes_selection_against_the_other_folder() {
        let mut app = comparing_panes();
        assert!(
            !menubar::is_enabled(&menubar::Command::CheckContents, &app.menu_state()),
            "nothing selected to check"
        );
        let _ = choose_from_menu(
            &mut app,
            menubar::Command::SelectAgainst(explorer::Against::Same),
        );
        assert!(menubar::is_enabled(
            &menubar::Command::CheckContents,
            &app.menu_state()
        ));

        let _ = choose_from_menu(&mut app, menubar::Command::CheckContents);

        let check = dto::CheckRequest {
            storage_id: 5,
            phone_folder: "/".into(),
            mac_folder: "/Users/me".into(),
            names: vec!["a.jpg".into()],
        };
        assert_eq!(app.transfer.saved(), [(true, dto::SavedJob::Check(check))]);
    }

    #[test]
    fn the_right_click_menu_checks_a_phone_selection_against_the_mac_folder() {
        let mut app = comparing_panes();
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("c.jpg".into()),
        )));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Menu(explorer::MenuAction::CheckAgainstOther),
        )));

        assert_eq!(app.active, Side::Phone);
        let check = dto::CheckRequest {
            storage_id: 5,
            phone_folder: "/".into(),
            mac_folder: "/Users/me".into(),
            names: vec!["c.jpg".into()],
        };
        assert_eq!(app.transfer.saved(), [(true, dto::SavedJob::Check(check))]);

        // Not while the Mac pane is hidden: there is nothing to check against.
        let mut hidden = comparing_panes();
        let _ = hidden.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("c.jpg".into()),
        )));
        settings_request(&mut hidden, settings::Request::SetShowMacPane(false));
        let _ = hidden.check_contents(Side::Phone);
        assert!(hidden.transfer.saved().is_empty());
    }

    #[test]
    fn nothing_is_compared_until_both_panes_have_read_their_folders() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.jpg")]),
        }));
        assert!(
            !app.menu_state().can_compare,
            "the phone is still being read"
        );
        let _ = choose_from_menu(
            &mut app,
            menubar::Command::SelectAgainst(explorer::Against::Missing),
        );
        assert_eq!(app.local_pane.selection(), None);

        // Nor with the Mac pane hidden, or a dialog up.
        let mut app = comparing_panes();
        settings_request(&mut app, settings::Request::SetShowMacPane(false));
        assert!(!app.menu_state().can_compare);
        let mut app = comparing_panes();
        let _ = app.open_search();
        assert!(!app.menu_state().can_compare);
    }

    // Quitting.

    fn command_q(app: &mut App) -> Task<Message> {
        app.update(key_press(
            Key::Character("q".into()),
            Code::KeyQ,
            Modifiers::COMMAND,
        ))
    }

    #[tokio::test]
    async fn with_nothing_under_way_a_quit_or_a_close_ends_the_app_at_once() {
        let mut app = two_listed_panes();
        assert!(crate::handler::testing::exits(command_q(&mut app)).await);
        assert!(crate::handler::testing::exits(app.update(Message::CloseRequested)).await);
        let quit = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::Quit));
        assert!(crate::handler::testing::exits(quit).await);
        assert_eq!(app.quitting, None);
    }

    #[tokio::test]
    async fn a_quit_during_a_transfer_asks_and_keep_going_leaves_it_running() {
        let (mut app, _) = two_phones().await;
        download_from_phone(&mut app);

        assert!(!crate::handler::testing::exits(command_q(&mut app)).await);
        assert_eq!(app.quitting, Some(Quitting::Asking));
        assert!(app.menu_state().dialog_open, "a dialog like the others");
        assert!(
            iced_test::simulator(app.view())
                .find("A transfer is under way")
                .is_ok()
        );
        let _ = app.update(Message::Quit(QuitChoice::KeepGoing));
        assert_eq!(app.quitting, None);
        assert!(app.transfer.is_running());

        // Escape is Keep Going too; the close button asks the same.
        let _ = app.update(Message::CloseRequested);
        assert_eq!(app.quitting, Some(Quitting::Asking));
        let _ = app.update(key_press(
            Key::Named(keyboard::key::Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        ));
        assert_eq!(app.quitting, None);
        assert!(app.transfer.is_running());
    }

    #[tokio::test]
    async fn stop_and_quit_waits_for_the_transfer_to_stop_then_ends_the_app() {
        let (mut app, _) = two_phones().await;
        download_from_phone(&mut app);
        let _ = command_q(&mut app);

        let stopping = app.update(Message::Quit(QuitChoice::StopAndQuit));
        assert!(!crate::handler::testing::exits(stopping).await, "not yet");
        assert_eq!(app.quitting, Some(Quitting::Stopping));
        assert!(
            iced_test::simulator(app.view())
                .find("Stopping the transfer…")
                .is_ok()
        );
        // A second ⌘Q while stopping changes nothing.
        assert!(!crate::handler::testing::exits(command_q(&mut app)).await);

        // The download stops as asked: the app ends.
        let stopped = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Cancelled),
        ))));
        assert!(crate::handler::testing::exits(stopped).await);
    }

    #[tokio::test]
    async fn quit_now_ends_the_app_while_a_silent_phone_holds_the_stop() {
        let (mut app, _) = two_phones().await;
        download_from_phone(&mut app);
        let _ = command_q(&mut app);
        let _ = app.update(Message::Quit(QuitChoice::StopAndQuit));

        let progress = app.update(Message::Transfer(transfer::Message::Progress(
            dto::TransferProgress::default(),
        )));
        assert!(
            !crate::handler::testing::exits(progress).await,
            "still stopping"
        );
        assert!(
            crate::handler::testing::exits(app.update(Message::Quit(QuitChoice::QuitNow))).await
        );

        // Answers that don't fit where the quit is do nothing.
        let mut idle = two_listed_panes();
        assert!(
            !crate::handler::testing::exits(idle.update(Message::Quit(QuitChoice::QuitNow))).await
        );
        assert!(
            !crate::handler::testing::exits(idle.update(Message::Quit(QuitChoice::StopAndQuit)))
                .await
        );
    }

    #[tokio::test]
    async fn a_quit_with_only_a_question_waiting_drops_it_and_ends_at_once() {
        let mut app = two_listed_panes();
        let _ = app.transfer.upload(dto::UploadRequest {
            sources: vec!["/Users/me/notes.txt".into()],
            storage_id: 5,
            destination: "/".into(),
        });
        let _ = app.update(Message::Transfer(transfer::Message::Checked {
            request: dto::UploadRequest {
                sources: vec!["/Users/me/notes.txt".into()],
                storage_id: 5,
                destination: "/".into(),
            },
            result: Ok(vec!["notes.txt".into()]),
        }));
        let _ = command_q(&mut app);
        assert_eq!(app.quitting, Some(Quitting::Asking));

        assert!(
            crate::handler::testing::exits(app.update(Message::Quit(QuitChoice::StopAndQuit)))
                .await
        );
    }

    #[test]
    fn calculate_folder_sizes_is_a_choice_of_the_pane_the_keys_go_to() {
        let mut app = two_listed_panes();
        assert!(!app.menu_state().folder_sizes);

        let _ = choose_from_menu(&mut app, menubar::Command::ToggleFolderSizes);

        let settings = app.settings.current();
        assert!(settings.mac_folder_sizes && !settings.phone_folder_sizes);
        assert!(app.menu_state().folder_sizes, "ticked");
        let _ = choose_from_menu(&mut app, menubar::Command::ToggleFolderSizes);
        assert!(!app.settings.current().mac_folder_sizes);
    }

    #[test]
    fn copy_from_the_menu_does_what_command_c_does() {
        let mut app = two_listed_panes();
        select_on_phone(&mut app, "/DCIM", &["a.jpg"], "a.jpg");
        let state = app.menu_state();
        assert_eq!(
            (state.selected, state.phone_open, state.can_paste),
            (1, true, false)
        );

        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::Copy));

        assert!(matches!(
            &app.clipboard,
            Some(Clipboard { side: Side::Phone, cut: false, selections }) if selections.len() == 1
        ));
        assert!(app.menu_state().can_paste, "Paste is offered now");
    }

    #[test]
    fn settings_and_help_chosen_from_the_menu_open_and_stay_open() {
        let mut app = two_listed_panes();

        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::Settings));
        assert!(app.settings.is_open());
        assert!(app.menu_state().dialog_open, "the rest is greyed meanwhile");
        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::Settings));
        assert!(
            app.settings.is_open(),
            "choosing it again does not close it"
        );

        let _ = choose_from_menu(&mut app, menubar::Command::Help(help::Tab::WhatsNew));
        assert!(
            app.help.is_open() && !app.settings.is_open(),
            "one dialog at a time"
        );
        assert_eq!(app.help.tab(), help::Tab::WhatsNew);
        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::Help));
        assert!(app.help.is_open());
    }

    #[test]
    fn hidden_files_and_the_view_change_in_the_active_pane_only() {
        let mut app = two_listed_panes();
        select_on_phone(&mut app, "/DCIM", &["a.jpg"], "a.jpg");

        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::ToggleHidden));
        let _ = choose_from_menu(&mut app, menubar::Command::ViewAs(dto::ViewMode::Grid));

        let settings = app.settings.current();
        assert!(settings.show_hidden_on_phone && !settings.show_hidden_on_mac);
        assert_eq!(
            (settings.phone_view, settings.mac_view),
            (dto::ViewMode::Grid, dto::ViewMode::List)
        );
        let state = app.menu_state();
        assert!(state.show_hidden, "ticked");
        assert_eq!(state.view, dto::ViewMode::Grid);
        let _ = choose_from_menu(&mut app, menubar::Command::Key(Shortcut::ToggleHidden));
        assert!(
            !app.settings.current().show_hidden_on_phone,
            "and off again"
        );
    }

    #[tokio::test]
    async fn import_and_mirror_from_the_menu_open_their_dialogs() {
        let mut app = two_listed_panes();
        app.import = import::Handler::new(Arc::new(crate::handler::testing::FakeImport::default()));
        let tag = app.menu.tag_of(&menubar::Command::ImportPhotos).unwrap();

        settle(&mut app, Message::Menu(menubar::Message::Chosen(tag))).await;

        assert!(app.import.is_open());
        assert!(app.menu_state().dialog_open);
        app.import.close();
        let _ = choose_from_menu(&mut app, menubar::Command::Mirror);
        assert!(app.mirror.is_open());
    }

    #[test]
    fn report_a_problem_opens_the_page_for_it() {
        let help_service = Arc::new(crate::handler::testing::FakeHelp::default());
        let mut app = two_listed_panes();
        app.help = help::Handler::new(help_service.clone());

        let _ = choose_from_menu(&mut app, menubar::Command::ReportProblem);

        assert_eq!(help_service.links.lock().unwrap().len(), 1);
        assert!(help_service.links.lock().unwrap()[0].starts_with("https://"));
    }

    #[test]
    fn the_menus_go_up_once_the_window_has_been_in_front() {
        let mut app = two_listed_panes();
        assert!(!app.menu.is_installed(), "macOS would replace them before");

        let _ = app.update(Message::Focused(false));
        assert!(!app.menu.is_installed());
        let _ = app.update(Message::Focused(true));

        assert!(app.menu.is_installed());
    }

    #[test]
    fn a_tag_no_menu_has_does_nothing() {
        let mut app = two_listed_panes();
        let _ = app.update(Message::Menu(menubar::Message::Chosen(9_999)));
        assert!(!app.settings.is_open() && !app.help.is_open());
    }

    fn import_button() -> Message {
        Message::Device(device::Message::Request(device::Request::Import))
    }

    #[tokio::test]
    async fn the_sidebar_imports_the_storage_into_the_mac_folder_shown() {
        let fake = Arc::new(crate::handler::testing::FakeImport::default());
        let mut app = two_listed_panes();
        app.import = import::Handler::new(fake.clone());
        assert!(
            iced_test::simulator(app.device.view_sidebar())
                .find("Import new photos…")
                .is_ok()
        );

        settle(&mut app, import_button()).await;

        assert!(app.import.is_open());
        assert_eq!(
            *fake.asked.lock().unwrap(),
            [dto::ImportRequest {
                storage_id: 5,
                destination: "/Users/me".into(),
                folders: dto::ImportFolders::None,
                remove_from_phone: false,
            }]
        );
        let mut ui = iced_test::simulator(app.view());
        assert!(
            ui.find("Import new photos and videos").is_ok(),
            "over the window"
        );
    }

    #[tokio::test]
    async fn the_folder_per_day_choice_is_kept_for_next_time() {
        let fake = Arc::new(crate::handler::testing::FakeImport::default());
        let mut app = two_listed_panes();
        app.import = import::Handler::new(fake.clone());
        settle(&mut app, import_button()).await;

        settle(
            &mut app,
            Message::Import(import::Message::Request(import::Request::Folders(
                dto::ImportFolders::Month,
            ))),
        )
        .await;
        app.import.close();
        settle(&mut app, import_button()).await;

        assert_eq!(
            app.settings.current().import_folders,
            dto::ImportFolders::Month
        );
        let asked: Vec<dto::ImportFolders> = fake
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.folders)
            .collect();
        assert_eq!(
            asked,
            [
                dto::ImportFolders::None,
                dto::ImportFolders::Month,
                dto::ImportFolders::Month
            ],
            "the choice is kept for the next import"
        );
    }

    #[tokio::test]
    async fn import_queues_the_import_and_closes_the_dialog() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        settle(&mut app, import_button()).await;

        let task = app.update(Message::Import(import::Message::Request(
            import::Request::Import,
        )));
        settle_task(&mut app, task).await;

        assert!(!app.import.is_open());
        assert_eq!(
            *transfers.imports.lock().unwrap(),
            [dto::ImportRequest {
                storage_id: 5,
                destination: "/Users/me".into(),
                folders: dto::ImportFolders::None,
                remove_from_phone: false,
            }]
        );
    }

    #[tokio::test]
    async fn freeing_the_phone_is_asked_of_the_import_and_kept_for_next_time() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        settle(&mut app, import_button()).await;

        settle(
            &mut app,
            Message::Import(import::Message::Request(import::Request::RemoveAfter(true))),
        )
        .await;
        assert!(app.settings.current().import_remove);
        let task = app.update(Message::Import(import::Message::Request(
            import::Request::Import,
        )));
        settle_task(&mut app, task).await;

        assert!(transfers.imports.lock().unwrap()[0].remove_from_phone);
        settle(&mut app, import_button()).await;
        assert_eq!(
            app.import
                .request()
                .map(|request| request.remove_from_phone),
            Some(true),
            "the next import starts from the same choice"
        );
    }

    #[tokio::test]
    async fn with_nothing_new_import_queues_nothing() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        app.import = import::Handler::new(Arc::new(
            crate::handler::testing::FakeImport::answering(Ok(dto::ImportPreview {
                new_files: 0,
                new_bytes: 0,
                already: 10,
            })),
        ));
        settle(&mut app, import_button()).await;

        let task = app.update(Message::Import(import::Message::Request(
            import::Request::Import,
        )));
        settle_task(&mut app, task).await;

        assert!(transfers.imports.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn escape_closes_the_import_dialog_and_other_keys_wait() {
        let mut app = two_listed_panes();
        settle(&mut app, import_button()).await;

        command(&mut app, Code::KeyA);
        assert!(
            app.device_pane.selection().is_none(),
            "the pane underneath is left alone"
        );
        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert!(!app.import.is_open());
    }

    #[tokio::test]
    async fn losing_the_phone_closes_the_import_dialog() {
        let mut app = two_listed_panes();
        settle(&mut app, import_button()).await;

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));

        assert!(!app.import.is_open());
    }

    #[test]
    fn no_phone_no_import() {
        let mut app = app();

        let _ = app.update(import_button());

        assert!(!app.import.is_open());
    }

    fn mirror_button() -> Message {
        Message::DevicePane(explorer::Message::Request(explorer::Request::Mirror))
    }

    #[tokio::test]
    async fn a_mirror_saved_from_the_dialog_opens_again_from_the_sidebar() {
        let fake = Arc::new(crate::handler::testing::FakeMirror::default());
        let mut app = two_listed_panes();
        app.mirror = mirror::Handler::new(fake.clone());
        let places = with_places(&mut app);
        let _ = app.update(Message::LocalPane(explorer::Message::Request(
            explorer::Request::GoTo("/Users/me/Phone photos".into()),
        )));
        settle(&mut app, mirror_button()).await;
        let _ = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Direction(dto::MirrorDirection::ToPhone),
        )));

        let _ = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Save,
        )));

        let kept = places
            .mirrors_saved
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].phone.serial, "ABC123");
        assert_eq!(kept[0].phone_folder, "/");
        assert_eq!(kept[0].mac_folder, "/Users/me/Phone photos");
        assert_eq!(kept[0].direction, dto::MirrorDirection::ToPhone);
        let mut ui = iced_test::simulator(app.mirror.view());
        assert!(ui.find("Saved in the sidebar").is_ok());
        drop(ui);
        let _ = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Direction(dto::MirrorDirection::ToMac),
        )));
        let mut ui = iced_test::simulator(app.mirror.view());
        assert!(
            ui.find("Save This Mirror").is_ok(),
            "the other way round is another mirror"
        );
        drop(ui);
        let _ = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Close,
        )));
        fake.asked.lock().unwrap().clear();

        let task = app.update(Message::Places(places::Message::OpenMirror {
            at: 0,
            storage_id: 5,
        }));
        feed_back(&mut app, task).await;

        assert!(app.mirror.is_open());
        assert_eq!(
            fake.asked.lock().unwrap()[0].direction,
            dto::MirrorDirection::ToPhone,
            "as it was saved"
        );
        assert!(
            fake.asked.lock().unwrap().len() == 1,
            "compared, and nothing run: Mirror is not clicked"
        );
        let task = app.update(Message::Places(places::Message::OpenMirror {
            at: 0,
            storage_id: 5,
        }));
        assert_eq!(task.units(), 0, "not over a dialog");
        let _ = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Close,
        )));
        let _ = app.update(Message::Places(places::Message::OpenMirror {
            at: 9,
            storage_id: 5,
        }));
        assert!(!app.mirror.is_open(), "no such mirror");
    }

    #[test]
    fn the_phones_usb_link_reaches_help_and_the_transfer_panel() {
        let mut device = FakeDevice::with_storages(vec![storage(5, "Internal")]);
        device.connect = Ok(dto::Device {
            link: Some(dto::UsbSpeed::High),
            ..crate::handler::testing::phone()
        });
        let mut app = app_built_with(
            Arc::new(device),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::new(default_transfer()),
            Arc::new(crate::handler::testing::FakeSearch::default()),
            Arc::new(FakeSettings::default()),
        );
        assert_eq!(app.help_phone().link, None, "no phone yet");

        let _ = app.update(Message::Device(device::Message::connected(
            &dto::Device {
                link: Some(dto::UsbSpeed::High),
                ..crate::handler::testing::phone()
            },
            &[storage(5, "Internal")],
        )));

        assert_eq!(app.help_phone().link, Some(dto::UsbSpeed::High));
        assert_eq!(app.transfer.link(), Some(dto::UsbSpeed::High));
    }

    #[test]
    fn command_plus_and_minus_size_the_active_grid_panes_tiles_and_keep_it() {
        let mut app = two_listed_panes();
        app.active = Side::Phone;
        let plus = |app: &mut App, shift: bool| {
            let modifiers = if shift {
                Modifiers::COMMAND | Modifiers::SHIFT
            } else {
                Modifiers::COMMAND
            };
            press(app, Key::Character("=".into()), Code::Equal, modifiers);
        };
        assert_eq!(app.settings.current().phone_view, dto::ViewMode::Grid);

        plus(&mut app, true);
        assert_eq!(app.settings.current().phone_tiles.points(), 160, "saved");
        assert_eq!(app.device_pane.tile().width, 160.0, "and shown");
        for _ in 0..5 {
            plus(&mut app, false);
        }
        assert_eq!(
            app.settings.current().phone_tiles.points(),
            192,
            "no bigger"
        );
        assert!(app.menu_state().tiles_biggest);
        press(
            &mut app,
            Key::Character("-".into()),
            Code::Minus,
            Modifiers::COMMAND,
        );
        assert_eq!(app.device_pane.tile().width, 160.0);

        app.active = Side::Mac;
        assert_eq!(app.settings.current().mac_view, dto::ViewMode::List);
        plus(&mut app, false);
        assert_eq!(
            app.settings.current().mac_tiles,
            dto::TileSize::default(),
            "a list has no tiles to size"
        );
        assert!(!app.menu_state().tiles_biggest && !app.menu_state().tiles_smallest);
    }

    #[tokio::test]
    async fn mirror_pairs_the_two_panes_folders_in_a_dialog_over_the_window() {
        let fake = Arc::new(crate::handler::testing::FakeMirror::default());
        let mut app = two_listed_panes();
        app.mirror = mirror::Handler::new(fake.clone());

        settle(&mut app, mirror_button()).await;

        assert!(app.mirror.is_open());
        assert_eq!(
            *fake.asked.lock().unwrap(),
            [dto::MirrorRequest {
                storage_id: 5,
                phone_folder: "/".into(),
                mac_folder: "/Users/me".into(),
                direction: dto::MirrorDirection::ToMac,
                remove_extras: false,
            }]
        );
        let mut ui = iced_test::simulator(app.view());
        assert!(ui.find("Mirror a folder").is_ok());
    }

    #[tokio::test]
    async fn starting_queues_the_mirror_and_closes_the_dialog() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        settle(&mut app, mirror_button()).await;

        let task = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Start,
        )));
        settle_task(&mut app, task).await;

        assert!(!app.mirror.is_open());
        assert_eq!(transfers.mirrors.lock().unwrap().len(), 1);
        assert_eq!(transfers.mirrors.lock().unwrap()[0].mac_folder, "/Users/me");
    }

    #[tokio::test]
    async fn with_nothing_to_mirror_nothing_is_queued() {
        let transfers = Arc::new(default_transfer());
        let mut app =
            two_listed_panes_with(Arc::new(FakeExplorer::new(Ok(vec![]))), transfers.clone());
        app.mirror = mirror::Handler::new(Arc::new(
            crate::handler::testing::FakeMirror::answering(Ok(dto::MirrorPreview::default())),
        ));
        settle(&mut app, mirror_button()).await;

        let task = app.update(Message::Mirror(mirror::Message::Request(
            mirror::Request::Start,
        )));
        settle_task(&mut app, task).await;

        assert!(transfers.mirrors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn escape_or_losing_the_phone_closes_the_mirror_dialog() {
        let mut app = two_listed_panes();
        settle(&mut app, mirror_button()).await;
        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );
        assert!(!app.mirror.is_open());

        settle(&mut app, mirror_button()).await;
        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));
        assert!(!app.mirror.is_open());
    }

    fn space(app: &mut App) -> Task<Message> {
        app.update(key_press(
            Key::Named(Named::Space),
            Code::Space,
            Modifiers::empty(),
        ))
    }

    fn with_preview(app: &mut App) -> Arc<crate::handler::testing::FakePreview> {
        let fake = Arc::new(crate::handler::testing::FakePreview::default());
        app.preview = preview::Handler::new(fake.clone());
        fake
    }

    fn select(app: &mut App, side: Side, name: &str) {
        let message = explorer::Message::Request(explorer::Request::Select(name.into()));
        let _ = match side {
            Side::Mac => app.update(Message::LocalPane(message)),
            Side::Phone => app.update(Message::DevicePane(message)),
        };
    }

    #[tokio::test]
    async fn space_looks_at_the_mac_selection_and_fetches_the_phones() {
        let mut app = two_listed_panes();
        let fake = with_preview(&mut app);

        select(&mut app, Side::Mac, "notes.txt");
        let _ = space(&mut app);
        select(&mut app, Side::Phone, "a.jpg");
        let task = space(&mut app);
        assert!(app.preview.is_fetching());
        feed_back(&mut app, task).await;

        assert_eq!(
            *fake.looks.lock().unwrap(),
            [
                vec!["/Users/me/notes.txt".to_owned()],
                vec!["/tmp/preview/a.jpg".to_owned()]
            ]
        );
        assert_eq!(
            *fake.fetches.lock().unwrap(),
            [dto::PreviewRequest {
                storage_id: 5,
                folder: "/".into(),
                names: vec!["a.jpg".into()],
            }]
        );
    }

    #[tokio::test]
    async fn space_does_nothing_over_a_dialog_or_the_search() {
        let mut app = two_listed_panes();
        let fake = with_preview(&mut app);
        select(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Rename,
        )));
        let _ = space(&mut app);
        assert!(
            fake.fetches.lock().unwrap().is_empty(),
            "the rename dialog is up"
        );

        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Escape,
        )));
        settle(
            &mut app,
            Message::DevicePane(explorer::Message::Request(explorer::Request::Search)),
        )
        .await;
        let _ = space(&mut app);
        assert!(
            fake.fetches.lock().unwrap().is_empty(),
            "the search covers the pane"
        );
        let _ = space(&mut two_listed_panes());
    }

    #[test]
    fn escape_stops_a_fetch_and_leaves_the_selection() {
        let mut app = two_listed_panes();
        with_preview(&mut app);
        select(&mut app, Side::Phone, "a.jpg");
        let _ = space(&mut app);

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert!(!app.preview.is_fetching());
        assert!(
            app.device_pane.selection().is_some(),
            "only the fetch stopped"
        );
    }

    #[tokio::test]
    async fn a_fetch_for_quick_look_holds_the_phone_like_a_transfer() {
        let (mut app, _) = two_phones().await;
        with_preview(&mut app);
        select(&mut app, Side::Phone, "a.jpg");
        let _ = space(&mut app);

        settle_task(&mut app, Task::done(open_phone("S24"))).await;

        assert_eq!(open_phone_key(&app).as_deref(), Some("ABC123"));
        assert!(says(
            app.device.view_sidebar(),
            "Wait for Quick Look to get its files from Pixel 9, or press esc, then switch."
        ));

        let _ = app.update(Message::Transfer(transfer::Message::Finished(Err(
            to_error_response(dto::Error::Disconnected),
        ))));
        assert!(!app.preview.is_fetching(), "losing the phone stops it");
    }

    #[tokio::test]
    async fn a_fetch_that_finds_the_phone_gone_says_so_in_the_device_panel() {
        let mut app = two_listed_panes();
        app.preview = preview::Handler::new(Arc::new(
            crate::handler::testing::FakePreview::fetching(Err(dto::Error::Disconnected)),
        ));
        select(&mut app, Side::Phone, "a.jpg");

        let task = space(&mut app);
        feed_back(&mut app, task).await;

        assert!(!app.device_pane.is_open());
        assert!(says(app.device.view_main(), "Reconnect"));
    }

    async fn open_on(app: &mut App, side: Side, name: &str) {
        let message = explorer::Message::Request(explorer::Request::Open(name.into()));
        let task = match side {
            Side::Mac => app.update(Message::LocalPane(message)),
            Side::Phone => app.update(Message::DevicePane(message)),
        };
        settle_task(app, task).await;
    }

    #[tokio::test]
    async fn a_file_opens_in_its_mac_app_from_either_pane_and_a_folder_in_the_pane() {
        let mut app = two_listed_panes();
        let fake = with_preview(&mut app);

        open_on(&mut app, Side::Mac, "notes.txt").await;
        open_on(&mut app, Side::Phone, "a.jpg").await;

        assert_eq!(
            *fake.opened.lock().unwrap(),
            ["/Users/me/notes.txt", "/tmp/preview/a.jpg"]
        );
        assert_eq!(
            *fake.fetches.lock().unwrap(),
            [dto::PreviewRequest {
                storage_id: 5,
                folder: "/".into(),
                names: vec!["a.jpg".into()],
            }]
        );
        assert!(fake.looks.lock().unwrap().is_empty());
    }

    #[test]
    fn command_f_puts_the_cursor_in_the_active_panes_filter_and_does_not_search() {
        let mut app = two_listed_panes();
        app.active = Side::Mac;

        let focus = app.update(key_press(
            Key::Character("f".into()),
            Code::KeyF,
            Modifiers::COMMAND,
        ));

        assert!(focus.units() > 0, "the filter is focused");
        assert!(!app.search.is_open());
    }

    fn with_places(app: &mut App) -> Arc<crate::handler::testing::FakePlaces> {
        let fake = Arc::new(crate::handler::testing::FakePlaces::default());
        app.places = places::Handler::new(fake.clone());
        fake
    }

    fn menu(app: &mut App, side: Side, action: explorer::MenuAction) {
        let message = explorer::Message::Request(explorer::Request::Menu(action));
        let _ = match side {
            Side::Mac => app.update(Message::LocalPane(message)),
            Side::Phone => app.update(Message::DevicePane(message)),
        };
    }

    fn last_saved(fake: &crate::handler::testing::FakePlaces) -> Vec<dto::Pin> {
        fake.saved
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn a_mac_place_takes_the_mac_pane_there() {
        let mut app = two_listed_panes();
        app.active = Side::Phone;

        let _ = app.update(Message::Places(places::Message::Go(places::Target::Mac(
            "/Users/me/Downloads".into(),
        ))));

        assert_eq!(app.active, Side::Mac);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Downloads"));
    }

    #[test]
    fn folders_are_pinned_from_the_menu_on_either_side_once() {
        let mut app = two_listed_panes();
        let fake = with_places(&mut app);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![folder("Work"), file("notes.txt")]),
        }));
        select(&mut app, Side::Mac, "Work");

        menu(&mut app, Side::Mac, explorer::MenuAction::Pin);
        menu(&mut app, Side::Mac, explorer::MenuAction::Pin);
        menu(&mut app, Side::Phone, explorer::MenuAction::PinHere);

        assert_eq!(
            last_saved(&fake),
            [
                dto::Pin {
                    side: dto::PinSide::Mac,
                    path: "/Users/me/Work".into(),
                    label: "Work".into(),
                    phone: None,
                },
                dto::Pin {
                    side: dto::PinSide::Phone,
                    path: "/".into(),
                    label: "Internal".into(),
                    phone: Some(dto::PinPhone {
                        serial: "ABC123".into(),
                        storage_id: 5,
                        storage_name: "Internal".into(),
                    }),
                },
            ]
        );
    }

    #[test]
    fn a_phone_without_a_serial_number_cannot_pin() {
        let mut app = app();
        let fake = with_places(&mut app);
        let _ = app.update(Message::Device(device::Message::connected(
            &dto::Device {
                abilities: dto::Abilities::default(),
                serial_number: String::new(),
                ..phone()
            },
            &[storage(5, "Internal")],
        )));

        menu(&mut app, Side::Phone, explorer::MenuAction::PinHere);

        assert!(fake.saved.lock().unwrap().is_empty());
        assert!(says_places(
            &app,
            "This phone gives no serial number, so its folders can't be pinned: they could be another phone's next time."
        ));
    }

    fn says_places(app: &App, words: &str) -> bool {
        let open = app
            .device
            .open_phone()
            .map(|(serial, storages)| places::OpenPhone {
                serial,
                storages: storages
                    .into_iter()
                    .map(|(id, n)| (id, n.to_owned()))
                    .collect(),
            });
        iced_test::simulator(app.places.view(open.as_ref(), None, None))
            .find(words)
            .is_ok()
    }

    #[test]
    fn a_phone_pin_on_another_storage_picks_it_then_goes_to_the_folder() {
        let mut app = app();
        connect(&mut app, &[(5, "Internal"), (6, "SD card")]);

        let _ = app.update(Message::Places(places::Message::Go(
            places::Target::Phone {
                storage_id: 6,
                path: "/Music".into(),
            },
        )));

        assert_eq!(app.device.selected_storage().map(|(id, _)| id), Some(6));
        assert_eq!(app.device_pane.location(), Some((6, "/Music")));
        assert_eq!(app.active, Side::Phone);
        // The storage's top was read first; its answer, arriving late, is
        // not shown in place of the pinned folder.
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 6,
            path: "/".into(),
            result: Ok(vec![file("top.jpg")]),
        }));
        assert_eq!(app.device_pane.location(), Some((6, "/Music")));
        assert!(app.device_pane.is_loading(), "still reading /Music");

        let _ = app.update(Message::Places(places::Message::Go(
            places::Target::Phone {
                storage_id: 6,
                path: "/DCIM".into(),
            },
        )));
        assert_eq!(
            app.device_pane.location(),
            Some((6, "/DCIM")),
            "the same storage"
        );
    }

    #[test]
    fn a_phone_pin_on_another_storage_waits_for_the_transfers_on_the_phone() {
        let mut app = app();
        let fake = with_places(&mut app);
        connect(&mut app, &[(5, "Internal"), (6, "SD card")]);
        let _ = app.transfer.download(dto::DownloadRequest {
            storage_id: 5,
            source: "/".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me".into(),
        });

        let _ = app.update(Message::Places(places::Message::Go(
            places::Target::Phone {
                storage_id: 6,
                path: "/Music".into(),
            },
        )));

        assert_eq!(app.device.selected_storage().map(|(id, _)| id), Some(5));
        assert!(says_places(
            &app,
            "Wait for the transfers on Pixel 9 to finish, or cancel them, then open it."
        ));
        assert!(fake.saved.lock().unwrap().is_empty());
    }

    #[test]
    fn the_sidebar_holds_the_phone_the_places_and_the_pins() {
        let mut app = two_listed_panes();
        with_places(&mut app);
        let _ = app.update(Message::Places(places::Message::Places(vec![dto::Place {
            label: "Downloads".into(),
            path: "/Users/me/Downloads".into(),
            volume: false,
            space: None,
        }])));

        let mut ui = iced_test::simulator(app.view());

        assert!(ui.find("Pixel 9").is_ok());
        assert!(ui.find("Places").is_ok());
        assert!(ui.find("Downloads").is_ok());
    }

    // Dragging from one pane onto the other, and the lasso through the app.

    struct Across {
        app: App,
        phone: Arc<FakeExplorer>,
        mac: Arc<FakeExplorer>,
        transfers: Arc<FakeTransfer>,
    }

    /// The Mac pane at /Users/me with a Photos folder and notes.txt, the
    /// phone pane at its top with a.jpg and DCIM.
    fn across() -> Across {
        let phone = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mac = Arc::new(FakeExplorer::new(Ok(vec![])));
        let transfers = Arc::new(default_transfer());
        let mut app = app_with_panes(Arc::clone(&phone), Arc::clone(&mac), Arc::clone(&transfers));
        connect(&mut app, &[(5, "Internal")]);
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok(vec![folder("Photos"), file("notes.txt")]),
        }));
        let _ = app.update(Message::DevicePane(explorer::Message::Listed {
            volume_id: 5,
            path: "/".into(),
            result: Ok(vec![file("a.jpg"), folder("DCIM")]),
        }));
        Across {
            app,
            phone,
            mac,
            transfers,
        }
    }

    fn on_pane(side: Side, request: explorer::Request) -> Message {
        let message = explorer::Message::Request(request);
        match side {
            Side::Mac => Message::LocalPane(message),
            Side::Phone => Message::DevicePane(message),
        }
    }

    fn hover(side: Side, inside: bool) -> Message {
        match side {
            Side::Mac => Message::LocalPane(explorer::Message::Hover(inside)),
            Side::Phone => Message::DevicePane(explorer::Message::Hover(inside)),
        }
    }

    /// Presses `name` in the pane on `side` and drags it out of its row.
    fn start_dragging(app: &mut App, side: Side, name: &str) {
        let _ = app.update(on_pane(side, explorer::Request::Select(name.into())));
        let _ = app.update(on_pane(side, explorer::Request::PointerLeft(name.into())));
    }

    fn hold(app: &mut App, modifiers: Modifiers) {
        let _ = app.update(Message::Keyboard(keyboard::Event::ModifiersChanged(
            modifiers,
        )));
    }

    #[tokio::test]
    async fn a_phone_drag_dropped_on_a_mac_folder_copies_into_it() {
        let Across {
            mut app,
            phone,
            transfers,
            ..
        } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        assert_eq!(app.local_pane.drop_into(), None, "not over the Mac yet");

        let _ = app.update(hover(Side::Mac, true));
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::PointerEntered("Photos".into()),
        ));
        assert_eq!(app.target(), Side::Phone, "Escape still goes to the drag");
        let task = app.update(Message::PointerReleased);
        feed_back(&mut app, task).await;

        let requests = transfers.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].destination, "/Users/me/Photos");
        assert_eq!(
            (requests[0].source.as_str(), requests[0].names.clone()),
            ("/", vec!["a.jpg".to_owned()])
        );
        assert_eq!(*transfers.modes.lock().unwrap(), [dto::Paste::Copy]);
        assert!(
            phone.pastes.lock().unwrap().is_empty(),
            "nothing dropped on the phone"
        );
        assert_eq!(app.target(), Side::Mac, "the pane it went to");
        assert_eq!(app.local_pane.drop_into(), None, "the drag is over");
        assert_eq!(app.device_pane.dragging(), None);
    }

    #[tokio::test]
    async fn command_makes_a_drop_on_the_other_pane_a_move() {
        let Across {
            mut app, transfers, ..
        } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        hold(&mut app, Modifiers::COMMAND);

        let task = app.update(Message::PointerReleased);
        feed_back(&mut app, task).await;

        assert_eq!(*transfers.modes.lock().unwrap(), [dto::Paste::Move]);
        assert_eq!(
            transfers.requests.lock().unwrap()[0].destination,
            "/Users/me",
            "over no folder: the folder shown"
        );
    }

    #[tokio::test]
    async fn a_mac_drag_dropped_on_the_phone_uploads_there() {
        let Across {
            mut app,
            mac,
            transfers,
            ..
        } = across();
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(hover(Side::Phone, true));
        let _ = app.update(on_pane(
            Side::Phone,
            explorer::Request::PointerEntered("DCIM".into()),
        ));
        assert_eq!(app.local_pane.dragging(), Some(1));
        assert_eq!(app.device_pane.drop_into().as_deref(), Some("/DCIM"));

        let task = app.update(Message::PointerReleased);
        settle_task(&mut app, task).await;
        // The fake phone says the name is taken: the usual question first.
        let task = app.update(Message::Transfer(transfer::Message::Request(
            transfer::Request::Resolve(dto::Conflict::KeepBoth),
        )));
        settle_task(&mut app, task).await;

        let uploads = transfers.uploads.lock().unwrap().clone();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0.sources, ["/Users/me/notes.txt"]);
        assert_eq!(uploads[0].0.destination, "/DCIM");
        assert_eq!(uploads[0].0.storage_id, 5);
        assert_eq!(*transfers.upload_modes.lock().unwrap(), [dto::Paste::Copy]);
        assert!(
            mac.pastes.lock().unwrap().is_empty(),
            "nothing dropped on the Mac"
        );
    }

    #[tokio::test]
    async fn let_go_anywhere_else_nothing_moves_and_the_other_pane_forgets() {
        let Across {
            mut app, transfers, ..
        } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        let _ = app.update(hover(Side::Mac, false));

        let task = app.update(Message::PointerReleased);
        feed_back(&mut app, task).await;

        assert!(transfers.requests.lock().unwrap().is_empty());
        let _ = app.update(hover(Side::Mac, true));
        assert_eq!(app.local_pane.drop_into(), None, "no drag any more");
    }

    /// A beat of the drag's clock, long after any folder was first held over.
    fn spring_beat(app: &mut App) -> Task<Message> {
        app.update(Message::SpringTick(
            Instant::now() + explorer::SPRING_DELAY * 2,
        ))
    }

    #[tokio::test]
    async fn a_phone_drag_held_over_a_mac_folder_opens_it_and_lands_there() {
        let Across {
            mut app, transfers, ..
        } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::PointerEntered("Photos".into()),
        ));
        assert!(app.local_pane.spring_pending(), "the clock runs");

        let _ = spring_beat(&mut app);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Photos"));
        let task = app.update(Message::PointerReleased);
        feed_back(&mut app, task).await;

        let requests = transfers.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].destination, "/Users/me/Photos");
        assert_eq!(requests[0].names, ["a.jpg"]);
        assert_eq!(
            app.local_pane.current_path(),
            Some("/Users/me/Photos"),
            "the Mac pane stays where it was dropped"
        );
    }

    #[tokio::test]
    async fn a_drag_that_opened_its_own_panes_folder_carries_what_it_started_with() {
        let Across {
            mut app, transfers, ..
        } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(on_pane(
            Side::Phone,
            explorer::Request::PointerEntered("DCIM".into()),
        ));
        let _ = spring_beat(&mut app);
        assert_eq!(app.device_pane.current_path(), Some("/DCIM"));
        assert_eq!(
            app.device_pane.selection(),
            None,
            "nothing selected in DCIM"
        );

        let _ = app.update(hover(Side::Phone, false));
        let _ = app.update(hover(Side::Mac, true));
        let task = app.update(Message::PointerReleased);
        feed_back(&mut app, task).await;

        let requests = transfers.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            (requests[0].source.as_str(), requests[0].names.clone()),
            ("/", vec!["a.jpg".to_owned()])
        );
        assert_eq!(requests[0].destination, "/Users/me");
        assert_eq!(
            app.device_pane.current_path(),
            Some("/"),
            "the folder it opened closes"
        );
    }

    #[tokio::test]
    async fn space_during_a_drag_opens_the_folder_rather_than_looking() {
        let Across { mut app, .. } = across();
        let fake = with_preview(&mut app);
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::PointerEntered("Photos".into()),
        ));

        let task = space(&mut app);
        feed_back(&mut app, task).await;

        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Photos"));
        assert!(fake.looks.lock().unwrap().is_empty());
        assert!(fake.fetches.lock().unwrap().is_empty());
        assert!(!app.preview.is_fetching());
    }

    #[test]
    fn escape_closes_the_folders_a_drag_opened_in_the_other_pane() {
        let Across { mut app, .. } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::PointerEntered("Photos".into()),
        ));
        let _ = spring_beat(&mut app);
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Photos"));

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert_eq!(app.local_pane.current_path(), Some("/Users/me"));
        assert!(!app.local_pane.spring_pending());
    }

    #[test]
    fn escape_ends_the_drag_for_both_panes() {
        let Across { mut app, .. } = across();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        assert!(app.local_pane.drop_into().is_some());

        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );

        assert_eq!(app.device_pane.dragging(), None);
        assert_eq!(app.local_pane.drop_into(), None);
    }

    #[test]
    fn a_drag_is_offered_only_to_a_pane_that_is_shown() {
        // The Mac pane hidden: a phone drag has nowhere to go.
        let Across { mut app, .. } = across();
        settings_request(&mut app, settings::Request::SetShowMacPane(false));
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Mac, true));
        assert_eq!(app.local_pane.drop_into(), None);

        // The search in the phone pane's place: a Mac drag has nowhere to go.
        let Across { mut app, .. } = across();
        let _ = app.open_search();
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(hover(Side::Phone, true));
        assert_eq!(app.device_pane.drop_into(), None);
    }

    // Dropping onto the sidebar.

    /// `across`, with the phone's SD card as a second storage.
    fn across_with_a_card() -> Across {
        let mut across = across();
        connect(&mut across.app, &[(5, "Internal"), (6, "SD card")]);
        let _ = across
            .app
            .update(Message::DevicePane(explorer::Message::Listed {
                volume_id: 5,
                path: "/".into(),
                result: Ok(vec![file("a.jpg"), folder("DCIM")]),
            }));
        across
    }

    fn over_mac_place(path: &str, over: bool) -> Message {
        Message::Places(places::Message::DropHover(Spot::Mac(path.into()), over))
    }

    fn over_storage(storage_id: u64, path: &str, over: bool) -> Message {
        Message::Device(device::Message::DropHover(
            Spot::Phone {
                storage_id,
                path: path.into(),
            },
            over,
        ))
    }

    async fn let_go(app: &mut App) {
        let task = app.update(Message::PointerReleased);
        settle_task(app, task).await;
    }

    #[tokio::test]
    async fn a_mac_drag_let_go_on_a_mac_place_moves_it_there_and_onto_another_disk_copies() {
        let Across { mut app, mac, .. } = across();
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(hover(Side::Mac, false));
        let _ = app.update(over_mac_place("/Users/me/Desktop", true));
        assert_eq!(
            app.sidebar_drop().unwrap().over,
            Some(Spot::Mac("/Users/me/Desktop".into()))
        );

        let_go(&mut app).await;

        {
            let pastes = mac.pastes.lock().unwrap();
            assert_eq!(pastes.len(), 1);
            assert_eq!(
                (
                    pastes[0].mode,
                    pastes[0].from.as_str(),
                    pastes[0].to.as_str()
                ),
                (dto::Paste::Move, "/Users/me", "/Users/me/Desktop")
            );
            assert_eq!(pastes[0].names, ["notes.txt"]);
        }
        assert_eq!(app.sidebar_over, None, "forgotten with the drag");
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"));

        // Another disk: a copy, and ⌘ makes it a move.
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(over_mac_place("/Volumes/Backup", true));
        let_go(&mut app).await;
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(over_mac_place("/Volumes/Backup", true));
        hold(&mut app, Modifiers::COMMAND);
        let_go(&mut app).await;
        let modes: Vec<dto::Paste> = mac.pastes.lock().unwrap().iter().map(|p| p.mode).collect();
        assert_eq!(
            modes,
            [dto::Paste::Move, dto::Paste::Copy, dto::Paste::Move]
        );
    }

    #[tokio::test]
    async fn a_mac_drag_let_go_on_a_phone_storage_uploads_it_to_that_storage() {
        let Across {
            mut app, transfers, ..
        } = across_with_a_card();
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(over_storage(6, "/", true));

        let_go(&mut app).await;
        // The fake phone says the name is taken: the usual question first.
        let task = app.update(Message::Transfer(transfer::Message::Request(
            transfer::Request::Resolve(dto::Conflict::KeepBoth),
        )));
        settle_task(&mut app, task).await;

        let uploads = transfers.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            (uploads[0].0.storage_id, uploads[0].0.destination.as_str()),
            (6, "/"),
            "the card, though the pane shows the internal storage"
        );
        assert_eq!(uploads[0].0.sources, ["/Users/me/notes.txt"]);
        assert_eq!(*transfers.upload_modes.lock().unwrap(), [dto::Paste::Copy]);
        assert_eq!(app.active, Side::Phone);
    }

    #[tokio::test]
    async fn a_phone_drag_let_go_on_a_mac_place_downloads_it_and_on_its_own_storage_moves_it() {
        let Across {
            mut app,
            phone,
            transfers,
            ..
        } = across_with_a_card();
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(hover(Side::Phone, false));
        let _ = app.update(over_mac_place("/Users/me/Downloads", true));
        let_go(&mut app).await;
        {
            let requests = transfers.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                (requests[0].destination.as_str(), requests[0].names.clone()),
                ("/Users/me/Downloads", vec!["a.jpg".to_owned()])
            );
            assert_eq!(*transfers.modes.lock().unwrap(), [dto::Paste::Copy]);
        }

        // Its own storage: a move inside the phone, ⌥ a copy.
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(over_storage(5, "/Download", true));
        let_go(&mut app).await;
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(over_storage(5, "/Download", true));
        hold(&mut app, Modifiers::ALT);
        let_go(&mut app).await;
        hold(&mut app, Modifiers::empty());
        let pastes = phone.pastes.lock().unwrap().clone();
        assert_eq!(pastes.len(), 2);
        assert_eq!(
            (pastes[0].mode, pastes[0].to.as_str(), pastes[1].mode),
            (dto::Paste::Move, "/Download", dto::Paste::Copy)
        );

        // Another storage of the phone takes nothing from it.
        start_dragging(&mut app, Side::Phone, "a.jpg");
        let _ = app.update(over_storage(6, "/", true));
        let_go(&mut app).await;
        assert_eq!(phone.pastes.lock().unwrap().len(), 2);
        assert_eq!(transfers.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn leaving_a_spot_after_coming_onto_the_next_keeps_the_next() {
        let Across { mut app, .. } = across();
        let _ = app.update(over_mac_place("/Users/me", true));
        assert_eq!(app.sidebar_over, None, "no drag, nothing kept");

        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(over_mac_place("/Users/me/Desktop", true));
        let _ = app.update(over_mac_place("/Users/me/Downloads", true));
        let _ = app.update(over_mac_place("/Users/me/Desktop", false));
        assert_eq!(
            app.sidebar_drop().unwrap().over,
            Some(Spot::Mac("/Users/me/Downloads".into()))
        );
        let _ = app.update(over_mac_place("/Users/me/Downloads", false));
        assert_eq!(app.sidebar_drop().unwrap().over, None);

        let _ = app.update(over_mac_place("/Users/me/Desktop", true));
        let _ = app.update(on_pane(Side::Mac, explorer::Request::Escape));
        assert_eq!(app.sidebar_drop(), None);
        assert_eq!(app.sidebar_over, None, "called off: forgotten");
    }

    #[test]
    fn the_windows_sidebar_reports_a_drag_over_a_storage_and_nothing_without_one() {
        let pass_over = |app: &App| -> Vec<Message> {
            let mut ui = iced_test::simulator(app.view());
            let at = ui.find("SD card").unwrap().bounds().center();
            ui.point_at(at);
            ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: at,
            })]);
            ui.into_messages().collect()
        };
        let Across { mut app, .. } = across_with_a_card();
        assert!(
            !pass_over(&app)
                .iter()
                .any(|m| matches!(m, Message::Device(device::Message::DropHover(..)))),
            "moving over the sidebar publishes nothing"
        );

        start_dragging(&mut app, Side::Mac, "notes.txt");
        let heard = pass_over(&app);
        let card = Spot::Phone {
            storage_id: 6,
            path: "/".into(),
        };
        assert!(
            heard.iter().any(|m| matches!(
                m,
                Message::Device(device::Message::DropHover(spot, true)) if *spot == card
            )),
            "{heard:?}"
        );
    }

    #[tokio::test]
    async fn a_drag_held_over_a_sidebar_spot_opens_it_in_its_pane_once() {
        let Across { mut app, .. } = across_with_a_card();
        start_dragging(&mut app, Side::Mac, "notes.txt");
        let _ = app.update(hover(Side::Mac, false));
        let _ = app.update(over_mac_place("/Users/me/Desktop", true));
        assert!(app.sidebar_spring_pending(), "the clock beats");
        let since = app.sidebar_over.as_ref().unwrap().1;

        let _ = app.update(Message::SpringTick(
            since + explorer::SPRING_DELAY - Duration::from_millis(1),
        ));
        assert_eq!(app.local_pane.current_path(), Some("/Users/me"), "not yet");
        let _ = app.update(Message::SpringTick(since + explorer::SPRING_DELAY));
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Desktop"));
        assert!(!app.sidebar_spring_pending(), "once");
        assert!(app.local_pane.dragging().is_some(), "the drag goes on");
        let _ = app.update(over_mac_place("/Users/me/Desktop", true));
        assert!(
            !app.sidebar_spring_pending(),
            "coming onto it again is not a new spot"
        );
        assert_eq!(
            app.sidebar_over.as_ref().unwrap().1,
            since,
            "nor a new clock"
        );

        // Let go in the pane, it lands in the folder that opened.
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me/Desktop".into(),
            result: Ok(vec![]),
        }));
        let _ = app.update(over_mac_place("/Users/me/Desktop", false));
        let _ = app.update(hover(Side::Mac, true));
        let_go(&mut app).await;
        assert_eq!(app.local_pane.current_path(), Some("/Users/me/Desktop"));

        // The phone pane springs only on the storage it shows; Space opens
        // at once.
        start_dragging(&mut app, Side::Mac, "notes.txt");
        // (A pin of a folder on the card.)
        let _ = app.update(over_storage(6, "/Music", true));
        let _ = app.spring_sidebar(Instant::now(), true);
        assert_eq!(app.device_pane.current_path(), Some("/"));
        assert_eq!(app.device_pane.volume_id(), Some(5));
        let _ = app.update(over_storage(5, "/DCIM", true));
        let _ = app.update(key_press(
            Key::Named(keyboard::key::Named::Space),
            Code::Space,
            Modifiers::empty(),
        ));
        assert_eq!(app.device_pane.current_path(), Some("/DCIM"));
        let _ = app.view();
    }

    #[test]
    fn a_press_beside_a_name_in_the_window_selects_that_row_for_a_lasso() {
        let Across { mut app, .. } = across();
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::Select("Photos".into()),
        ));
        let _ = app.update(Message::PointerReleased);

        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(app.view());
            let name = ui.find("notes.txt").unwrap().visible_bounds().unwrap();
            ui.point_at(iced::Point::new(
                name.x + name.width + 40.0,
                name.center_y(),
            ));
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
                iced::mouse::Button::Left,
            ))]);
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = app.update(message);
        }

        assert_eq!(
            app.local_pane.selection().map(|selection| selection.names),
            Some(vec!["notes.txt".to_owned()])
        );
        assert_eq!(app.local_pane.dragging(), None, "a lasso, not a drag");
        assert_eq!(app.target(), Side::Mac);
    }

    #[test]
    fn a_lasso_held_past_the_edge_is_scrolled_on_by_the_apps_clock() {
        let Across { mut app, .. } = across();
        let quiet = app.subscription();

        let _ = app.update(Message::LocalPane(explorer::Message::PressedAt {
            at: iced::Point::new(100.0, 5.0),
            size: iced::Size::new(400.0, 100.0),
            taken: false,
        }));
        let _ = app.update(Message::LocalPane(explorer::Message::LassoMoved {
            at: iced::Point::new(100.0, 400.0),
            size: iced::Size::new(400.0, 100.0),
        }));
        assert!(
            app.local_pane.lasso_step().is_none(),
            "two rows fit: nowhere to scroll"
        );
        // Forty rows in a 50 px list: room to scroll.
        let _ = app.update(Message::LocalPane(explorer::Message::Listed {
            volume_id: MAC_VOLUME,
            path: "/Users/me".into(),
            result: Ok((0..40).map(|n| file(&format!("f{n}.txt"))).collect()),
        }));
        let _ = app.update(Message::LocalPane(explorer::Message::PressedAt {
            at: iced::Point::new(100.0, 5.0),
            size: iced::Size::new(400.0, 50.0),
            taken: false,
        }));
        let _ = app.update(Message::LocalPane(explorer::Message::LassoMoved {
            at: iced::Point::new(100.0, 400.0),
            size: iced::Size::new(400.0, 50.0),
        }));
        assert!(app.local_pane.lasso_step().is_some());
        let ticking = app.subscription();
        let _ = app.update(Message::LassoTick);
        assert!(
            app.local_pane
                .selection()
                .map_or(0, |selection| selection.names.len())
                > 2,
            "the list scrolled on and the box took in more"
        );

        let _ = app.update(Message::PointerReleased);
        assert!(app.local_pane.lasso_step().is_none());
        drop((quiet, ticking));
    }

    // Get Info.

    fn shows(app: &App, words: &str) -> bool {
        iced_test::simulator(app.view()).find(words).is_ok()
    }

    #[test]
    fn command_i_tells_about_the_selection_on_the_mac() {
        let Across { mut app, .. } = across();
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::Select("notes.txt".into()),
        ));

        command(&mut app, Code::KeyI);

        assert!(app.info.is_open());
        assert!(!app.info.is_about_the_phone());
        assert!(shows(&app, "TXT file"));
        assert!(shows(&app, "/Users/me"));
    }

    #[tokio::test]
    async fn command_i_on_the_phone_counts_the_selected_folder() {
        let Across { mut app, .. } = across();
        command(&mut app, Code::Digit1);
        assert_eq!(app.target(), Side::Phone);
        let _ = app.update(on_pane(
            Side::Phone,
            explorer::Request::Select("DCIM".into()),
        ));

        let task = phone_key(&mut app, Code::KeyI);
        feed_back(&mut app, task).await;

        assert!(app.info.is_about_the_phone());
        assert!(shows(&app, "Internal on the phone"));
        assert!(
            shows(&app, "4 files, 2 folders"),
            "DCIM and what the fake counts in it"
        );
    }

    #[test]
    fn with_nothing_selected_or_from_empty_space_it_is_the_folder_shown() {
        let Across { mut app, .. } = across();
        command(&mut app, Code::KeyI);
        assert!(shows(&app, "me"), "the Mac folder /Users/me");
        assert!(shows(&app, "Counting… 0 files, 0 B so far") || shows(&app, "Folder"));
        command(&mut app, Code::KeyI);
        assert!(!app.info.is_open(), "⌘I again closes it");

        // A right-click on empty space: the folder, whatever is selected.
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::Select("notes.txt".into()),
        ));
        menu(&mut app, Side::Mac, explorer::MenuAction::GetInfoHere);
        assert!(app.info.is_open());
        assert!(!shows(&app, "TXT file"), "not the selected file");
        assert!(shows(&app, "Folder"));
    }

    #[test]
    fn help_and_get_info_hear_what_the_open_phone_can_do() {
        let mut app = two_listed_panes();

        let phone = app.help_phone();
        assert_eq!(phone.abilities, Some(dto::Abilities::default()));
        assert_eq!(
            phone.folder,
            Some((5, "/".to_owned())),
            "what a timed reading reads"
        );

        command(&mut app, Code::Digit1);
        command(&mut app, Code::KeyI);
        assert!(app.info.is_open());
        assert!(
            shows(&app, "Can't"),
            "a phone folder says what the phone can't do"
        );
    }

    #[test]
    fn get_info_from_the_menu_on_an_item_is_about_the_selection() {
        let Across { mut app, .. } = across();
        let _ = app.update(on_pane(
            Side::Mac,
            explorer::Request::Select("notes.txt".into()),
        ));

        menu(&mut app, Side::Mac, explorer::MenuAction::GetInfo);

        assert!(shows(&app, "TXT file"));
    }

    #[test]
    fn get_info_covers_the_window_until_escape_and_is_one_dialog_of_many() {
        let Across { mut app, .. } = across();
        command(&mut app, Code::KeyI);

        command(&mut app, Code::KeyA);
        assert_eq!(
            app.local_pane.selection(),
            None,
            "keys wait while it is open"
        );
        press(
            &mut app,
            Key::Named(Named::Escape),
            Code::Escape,
            Modifiers::empty(),
        );
        assert!(!app.info.is_open());

        command(&mut app, Code::KeyI);
        settings_request(&mut app, settings::Request::Open);
        assert!(!app.info.is_open(), "Settings put it away");
        settings_request(&mut app, settings::Request::Close);

        command(&mut app, Code::KeyI);
        let _ = app.update(Message::Help(help::Message::Request(help::Request::Open)));
        assert!(!app.info.is_open(), "Help put it away");
    }

    #[test]
    fn a_phones_info_goes_with_the_phone_and_the_macs_stays() {
        let Across { mut app, .. } = across();
        command(&mut app, Code::Digit1);
        let _ = phone_key(&mut app, Code::KeyI);
        assert!(app.info.is_about_the_phone());

        let _ = app.update(Message::Device(device::Message::Disconnected));
        assert!(!app.info.is_open(), "a lost phone's count is put away");

        let Across { mut app, .. } = across();
        command(&mut app, Code::KeyI);
        let _ = app.update(Message::Device(device::Message::Disconnected));
        assert!(app.info.is_open(), "the Mac's stays");
    }

    #[test]
    fn command_i_over_the_search_panel_does_nothing() {
        let Across { mut app, .. } = across();
        let _ = app.open_search();
        assert_eq!(app.target(), Side::Phone, "the search is where keys go");

        let _ = phone_key(&mut app, Code::KeyI);

        assert!(!app.info.is_open());
    }

    // The Dock and notifications, through the app.

    #[test]
    fn the_window_says_when_it_comes_to_the_front_or_goes_behind() {
        let id = window::Id::unique();
        let status = iced::event::Status::Ignored;
        assert!(matches!(
            focus_change(iced::Event::Window(window::Event::Unfocused), status, id),
            Some(Message::Focused(false))
        ));
        assert!(matches!(
            focus_change(iced::Event::Window(window::Event::Focused), status, id),
            Some(Message::Focused(true))
        ));
        assert!(focus_change(iced::Event::Window(window::Event::Closed), status, id).is_none());
    }

    #[tokio::test]
    async fn a_download_ending_while_the_app_is_behind_is_notified_and_the_dock_follows() {
        let transfers = Arc::new(default_transfer());
        let mut app = two_listed_panes_with(
            Arc::new(FakeExplorer::new(Ok(vec![]))),
            Arc::clone(&transfers),
        );
        let _ = app.update(Message::Focused(false));
        let _ = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Select("a.jpg".into()),
        )));

        let task = app.update(Message::DevicePane(explorer::Message::Request(
            explorer::Request::Transfer,
        )));
        settle_task(&mut app, task).await;

        assert_eq!(transfers.notes.lock().unwrap().len(), 1);
        let dock = transfers.dock.lock().unwrap();
        assert_eq!(dock.first(), Some(&Some(0)));
        assert_eq!(dock.last(), Some(&None), "the bar goes when it is done");
    }

    // What's new, once after an update.

    fn version(text: &str) -> dto::Version {
        dto::Version::parse(text).unwrap()
    }

    #[test]
    fn a_first_run_only_remembers_the_version() {
        let mut app = app();
        assert!(!app.help.is_open(), "App::new greeted a first run quietly");
        assert_eq!(
            app.settings.current().last_seen_version,
            Some(help::whats_new::current())
        );

        app.settings.change(|s| s.last_seen_version = None);
        app.greet(version("0.1.0"));
        assert!(!app.help.is_open());
        assert_eq!(
            app.settings.current().last_seen_version,
            Some(version("0.1.0"))
        );
    }

    #[test]
    fn after_an_update_whats_new_shows_once() {
        let mut app = app();
        app.settings
            .change(|s| s.last_seen_version = Some(version("0.0.9")));

        app.greet(help::whats_new::current());
        assert!(app.help.is_open(), "what is new since 0.0.9");
        assert_eq!(
            app.settings.current().last_seen_version,
            Some(help::whats_new::current()),
            "remembered, so it is not shown again"
        );

        let _ = app.update(Message::Help(help::Message::Request(help::Request::Close)));
        app.greet(help::whats_new::current());
        assert!(!app.help.is_open(), "the same version again: nothing");
    }

    #[test]
    fn an_older_version_shows_nothing_and_remembers_itself() {
        let mut app = app();
        app.settings
            .change(|s| s.last_seen_version = Some(version("9.0.0")));

        app.greet(help::whats_new::current());

        assert!(!app.help.is_open());
        assert_eq!(
            app.settings.current().last_seen_version,
            Some(help::whats_new::current()),
            "so going forward again shows what is new"
        );
    }
}
