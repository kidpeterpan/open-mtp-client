use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::widget::{button, column, container, progress_bar, row, rule, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::Request;
use super::response::{DeviceResponse, PhoneResponse, StorageResponse};
use crate::handler::diagnosis::{self, DiagnosisResponse};
use crate::handler::drop_spot::{SidebarDrop, Spot, drop_spot};
use crate::handler::error::{self, ErrorResponse};
use crate::i18n::{count, english_only, t, tf};

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Opens `prefer` (a phone's key) if it is plugged in, else the only
    /// phone; the open one is closed first when another is asked for.
    async fn connect(&self, prefer: Option<String>) -> Result<dto::Device, dto::Error>;
    /// The phones plugged in, without opening any.
    async fn phones(&self) -> Result<Vec<dto::Phone>, dto::Error>;
    /// The open phone's battery level in percent; `None` if it does not say.
    async fn battery(&self) -> Result<Option<u8>, dto::Error>;
    async fn storages(&self) -> Result<Vec<dto::Storage>, dto::Error>;
    async fn disconnect(&self) -> Result<(), dto::Error>;
    /// Phones being plugged in and unplugged; never ends.
    fn watch(&self) -> Result<BoxStream<'static, dto::DeviceChange>, dto::Error>;
    /// The next change on the connected phone; an error once it is gone.
    async fn next_change(&self) -> Result<dto::ContentChange, dto::Error>;
    /// What may be behind a failed connect: apps holding the phone, or a phone
    /// that is plugged in without offering file transfer.
    async fn diagnose(&self, failure: &dto::Error) -> dto::Diagnosis;
    /// Quits `holders` and waits for them to be gone.
    async fn release(&self, holders: &[dto::Holder]);
    /// After something kept the phone: ends macOS's camera services when they
    /// are all that could have, saying whether it did.
    async fn free_from_camera(&self) -> bool {
        false
    }
    /// Which of `paths` are folders on storage `storage_id` of the open phone.
    async fn folders_present(
        &self,
        _storage_id: u64,
        _paths: Vec<String>,
    ) -> Result<Vec<String>, dto::Error> {
        Ok(Vec::new())
    }
}

/// At most this many automatic connects in `RETRY_WINDOW`,
/// so a phone stuck re-enumerating can't keep the app busy.
const MAX_AUTO_CONNECTS: usize = 6;
const RETRY_WINDOW: Duration = Duration::from_secs(15);

type Connection = (DeviceResponse, Vec<StorageResponse>);

/// A connect that did not work: what to tell the user, and what was found out
/// about why. The diagnosis is made in the same task as the connect, so it is
/// never about an attempt the user has already moved on from.
#[derive(Debug, Clone)]
pub struct Failure {
    error: ErrorResponse,
    /// Several phones are plugged in and none was picked: the user chooses.
    choose: bool,
    /// Boxed: a failure travels through `Result`s and should stay small.
    diagnosis: Option<Box<Diagnosed>>,
}

#[derive(Debug, Clone)]
struct Diagnosed {
    response: DiagnosisResponse,
    source: dto::Diagnosis,
    /// What "Quit … and try again" quits.
    holders: Vec<dto::Holder>,
}

impl Diagnosed {
    fn of(diagnosis: &dto::Diagnosis) -> Self {
        Self {
            response: diagnosis::to_diagnosis_response(diagnosis),
            source: diagnosis.clone(),
            holders: diagnosis.holders.clone().unwrap_or_default(),
        }
    }
}

impl Failure {
    fn reworded(&self) -> Failure {
        let owner_seen = self
            .diagnosis
            .as_ref()
            .is_some_and(|diagnosed| diagnosed.source.owner_seen);
        let mut error = self.error.reworded();
        if owner_seen {
            error.hint.clear();
        }
        Failure {
            error,
            choose: self.choose,
            diagnosis: self
                .diagnosis
                .as_ref()
                .map(|diagnosed| Box::new(Diagnosed::of(&diagnosed.source))),
        }
    }
}

impl From<ErrorResponse> for Failure {
    fn from(error: ErrorResponse) -> Self {
        Self {
            error,
            choose: false,
            diagnosis: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Connected(Result<Connection, Failure>),
    Disconnected,
    /// A phone was plugged in or unplugged.
    Hotplug(dto::DeviceChange),
    /// Something changed on the connected phone.
    Changed(dto::ContentChange),
    StoragesReloaded(Result<Vec<StorageResponse>, ErrorResponse>),
    /// The open phone's battery level, asked again.
    Battery(Result<Option<u8>, ErrorResponse>),
    /// The phones plugged in now.
    Phones(Vec<PhoneResponse>),
    /// How a switch to another phone went.
    Switched(Switch),
    /// Which of the usual places are on the storage `storage_id` of the phone
    /// `key`.
    Places {
        key: String,
        storage_id: u64,
        result: Result<Vec<String>, ErrorResponse>,
    },
    /// A drag came onto (`true`) or left a storage or a usual place
    /// (`app.rs` keeps it).
    DropHover(Spot, bool),
}

/// How a switch to another phone went.
#[derive(Debug, Clone)]
pub enum Switch {
    Opened(Connection),
    /// The phone asked for is no longer plugged in; the open one was kept.
    Gone,
    /// The open phone was closed and the other would not open.
    Failed(Failure),
}

impl Message {
    /// A successful connection, as the connect task would report it.
    #[cfg(test)]
    pub fn connected(device: &dto::Device, storages: &[dto::Storage]) -> Self {
        Message::Connected(Ok((
            mapper::to_device_response(device),
            storages.iter().map(mapper::to_storage_response).collect(),
        )))
    }
}

#[allow(clippy::large_enum_variant)]
enum State {
    Idle,
    Connecting,
    Connected {
        device: DeviceResponse,
        storages: Vec<StorageResponse>,
        selected: Option<u64>,
    },
    Switching {
        to: Option<String>,
        from: Box<State>,
    },
    Failed(Failure),
}

impl State {
    fn reword(&mut self) {
        match self {
            State::Connected {
                device, storages, ..
            } => {
                let level = device.battery_level;
                *device = mapper::to_device_response(&device.source);
                device.battery_level = level;
                device.battery = mapper::battery_label(level);
                *storages = storages
                    .iter()
                    .map(|storage| mapper::to_storage_response(&storage.source))
                    .collect();
            }
            State::Switching { from, .. } => from.reword(),
            State::Failed(failure) => *failure = failure.reworded(),
            State::Idle | State::Connecting => {}
        }
    }
}

fn to_name(name: Option<&str>) -> String {
    name.map_or_else(|| t("device.that_phone").to_owned(), str::to_owned)
}

pub struct Handler {
    service: Arc<dyn Service>,
    state: State,
    auto_connect: bool,
    attempts: Attempts,
    /// Every phone plugged in, open or not.
    phones: Vec<PhoneResponse>,
    /// The phone last opened or picked: a reconnect opens it again rather
    /// than asking which, while it is plugged in.
    preferred: Option<String>,
    /// Why the last switch did not happen, until the next one.
    notice: Option<String>,
    /// A phone left without saying which: the next list of phones tells
    /// whether it was the open one.
    check_unplugged: bool,
    /// The usual places found on the open storage (label, path), and which
    /// phone and storage they are about.
    places: Vec<(String, String)>,
    places_for: Option<(String, u64)>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            state: State::Idle,
            auto_connect: true,
            attempts: Attempts::default(),
            phones: Vec::new(),
            preferred: None,
            notice: None,
            check_unplugged: false,
            places: Vec::new(),
            places_for: None,
        }
    }

    pub fn set_auto_connect(&mut self, on: bool) {
        self.auto_connect = on;
    }

    /// Starts listening for phones being plugged in and unplugged.
    pub fn watch(&self) -> Task<Message> {
        match self.service.watch() {
            Ok(changes) => Task::run(changes, Message::Hotplug),
            // Without a watch the Connect button still works.
            Err(_) => Task::none(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.route(message);
        Task::batch([task, self.follow_places()])
    }

    /// Asks again which usual places are there whenever another phone or
    /// storage is open; forgets them when none is.
    fn follow_places(&mut self) -> Task<Message> {
        let now = self.opened().map(|(key, id, _)| (key, id));
        if now == self.places_for {
            return Task::none();
        }
        self.places.clear();
        self.places_for.clone_from(&now);
        let Some((key, storage_id)) = now else {
            return Task::none();
        };
        let service = Arc::clone(&self.service);
        Task::perform(
            async move {
                let result = service
                    .folders_present(storage_id, mapper::place_candidates())
                    .await
                    .map_err(error::to_error_response);
                (key, storage_id, result)
            },
            |(key, storage_id, result)| Message::Places {
                key,
                storage_id,
                result,
            },
        )
    }

    fn route(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DropHover(..) => Task::none(),
            Message::Places {
                key,
                storage_id,
                result,
            } => {
                // About what is open now, and found: shown. A refusal or a
                // lost phone shows nothing here; the panel hears of the loss
                // from the requests that need the phone.
                if self.places_for.as_ref() == Some(&(key, storage_id))
                    && let Ok(present) = result
                {
                    self.places = mapper::to_places(&present);
                }
                Task::none()
            }
            Message::Request(Request::Connect) => self.connect(),
            Message::Request(Request::QuitHolders) => self.quit_holders(),
            Message::Request(Request::Disconnect) => self.disconnect(),
            Message::Request(Request::SelectStorage(id)) => {
                if let State::Connected { selected, .. } = &mut self.state {
                    *selected = Some(id);
                }
                Task::none()
            }
            Message::Request(Request::Open(key)) => self.open(key),
            // The import dialog and the phone pane are `app.rs`'s.
            Message::Request(Request::Import | Request::FreeUpSpace | Request::GoToPlace(_)) => {
                Task::none()
            }
            Message::Connected(Ok(connection)) | Message::Switched(Switch::Opened(connection)) => {
                let (device, storages) = connection;
                let selected = storages.first().map(|storage| storage.id);
                self.preferred = Some(device.key.clone());
                self.state = State::Connected {
                    device,
                    storages,
                    selected,
                };
                Task::batch([
                    Task::run(changes(Arc::clone(&self.service)), Message::Changed),
                    self.list_phones(),
                ])
            }
            Message::Connected(Err(failure)) | Message::Switched(Switch::Failed(failure)) => {
                self.state = State::Failed(failure);
                self.list_phones()
            }
            Message::Switched(Switch::Gone) => {
                if let State::Switching { to, from } =
                    std::mem::replace(&mut self.state, State::Idle)
                {
                    self.notice = Some(tf(
                        "device.no_longer_plugged",
                        &[("to", &to_name(to.as_deref()))],
                    ));
                    self.state = *from;
                }
                self.preferred = self.open_key();
                self.list_phones()
            }
            Message::Phones(phones) => {
                self.phones = phones;
                let gone = std::mem::take(&mut self.check_unplugged)
                    && self
                        .open_key()
                        .is_some_and(|key| !self.phones.iter().any(|phone| phone.key == key));
                if gone { self.unplugged() } else { Task::none() }
            }
            Message::Disconnected => {
                self.state = State::Idle;
                Task::none()
            }
            Message::Hotplug(dto::DeviceChange::Plugged { .. }) => {
                let waiting = matches!(self.state, State::Idle | State::Failed(_));
                let connect = if waiting && self.auto_connect && self.attempts.allow(Instant::now())
                {
                    self.connect()
                } else {
                    Task::none()
                };
                Task::batch([connect, self.list_phones()])
            }
            Message::Hotplug(dto::DeviceChange::Unplugged { serial }) => {
                let open = match &self.state {
                    State::Connected { device, .. } => Some(device.serial.as_str()),
                    _ => None,
                };
                match (open, serial.as_deref()) {
                    (Some(open), Some(serial)) if !open.is_empty() && open == serial => {
                        self.unplugged()
                    }
                    (Some(open), Some(_)) if !open.is_empty() => self.list_phones(),
                    // The event can't say which phone left (no serial on one
                    // side): the phones still plugged in can.
                    (Some(_), _) => {
                        self.check_unplugged = true;
                        self.list_phones()
                    }
                    (None, _) => self.list_phones(),
                }
            }
            Message::Changed(dto::ContentChange::Storages) => self.reload_storages(),
            Message::Changed(dto::ContentChange::Battery) => self.reload_battery(),
            Message::Battery(Ok(level)) => {
                if let State::Connected { device, .. } = &mut self.state {
                    device.battery_level = level;
                    device.battery = mapper::battery_label(level);
                }
                Task::none()
            }
            Message::Battery(Err(error)) => {
                // Only about the phone open now: one that comes back during a
                // switch is about the phone being closed.
                if error.device_lost && self.is_connected() {
                    self.state = State::Idle;
                }
                Task::none()
            }
            // The file pane refreshes; `app.rs` sees this message too.
            Message::Changed(dto::ContentChange::Files) => Task::none(),
            Message::StoragesReloaded(Ok(fresh)) => {
                if let State::Connected {
                    storages, selected, ..
                } = &mut self.state
                {
                    if !fresh.iter().any(|storage| Some(storage.id) == *selected) {
                        *selected = fresh.first().map(|storage| storage.id);
                    }
                    *storages = fresh;
                }
                Task::none()
            }
            Message::StoragesReloaded(Err(error)) => {
                // As for the battery: not about a phone being switched from.
                if error.device_lost && self.is_connected() {
                    self.state = State::Idle;
                }
                Task::none()
            }
        }
    }

    /// Asks the phone how full its storages are now, so the bars in the sidebar
    /// follow what was just copied, deleted or made instead of waiting for the
    /// phone to announce it (not every phone does). Nothing happens unless a
    /// phone is connected.
    pub fn reload_storages(&mut self) -> Task<Message> {
        if !matches!(self.state, State::Connected { .. }) {
            return Task::none();
        }
        let service = Arc::clone(&self.service);
        let storages = Task::perform(
            async move {
                service
                    .storages()
                    .await
                    .map(|storages| storages.iter().map(mapper::to_storage_response).collect())
                    .map_err(error::to_error_response)
            },
            Message::StoragesReloaded,
        );
        // Not every phone says when its battery changes; a transfer is a
        // good moment to look (one request).
        Task::batch([storages, self.reload_battery()])
    }

    /// Asks the open phone how charged it is.
    fn reload_battery(&self) -> Task<Message> {
        if !self.is_connected() {
            return Task::none();
        }
        let service = Arc::clone(&self.service);
        Task::perform(
            async move { service.battery().await.map_err(error::to_error_response) },
            Message::Battery,
        )
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected { .. })
    }

    /// The key of the phone open now.
    pub fn open_key(&self) -> Option<String> {
        match &self.state {
            State::Connected { device, .. } => Some(device.key.clone()),
            _ => None,
        }
    }

    /// Which phone is open and which of its storages is picked: (phone key,
    /// storage id, storage name). Two phones can have storages with the same
    /// id and name, so this, not the storage alone, tells a switch apart.
    pub fn opened(&self) -> Option<(String, u64, String)> {
        let key = self.open_key()?;
        let (id, name) = self.selected_storage()?;
        Some((key, id, name))
    }

    /// The name of the phone open now, for messages about it.
    pub fn open_name(&self) -> Option<&str> {
        match &self.state {
            State::Connected { device, .. } => Some(&device.name),
            _ => None,
        }
    }

    /// Says in the sidebar why the phone was not switched.
    pub fn refuse_switch(&mut self, why: String) {
        self.notice = Some(why);
    }

    /// Opens the phone `key`: a switch if another is open (its storages and
    /// what was shown of it go), else a connect to that phone.
    fn open(&mut self, key: String) -> Task<Message> {
        self.notice = None;
        if self.open_key().as_deref() == Some(key.as_str()) {
            return Task::none();
        }
        if matches!(self.state, State::Connecting | State::Switching { .. }) {
            return Task::none();
        }
        self.preferred = Some(key.clone());
        if !self.is_connected() {
            return self.connect();
        }
        let to = self
            .phones
            .iter()
            .find(|phone| phone.key == key)
            .map(|phone| phone.name.clone());
        let from = std::mem::replace(&mut self.state, State::Idle);
        self.state = State::Switching {
            to,
            from: Box::new(from),
        };
        Task::perform(switch(Arc::clone(&self.service), key), Message::Switched)
    }

    /// The open phone was unplugged. Unplugging is deliberate: straight back
    /// to "Connect a device", not an error; the session is closed behind it.
    fn unplugged(&mut self) -> Task<Message> {
        if !self.auto_connect {
            return self.list_phones();
        }
        self.state = State::Idle;
        Task::batch([self.disconnect(), self.list_phones()])
    }

    /// Asks which phones are plugged in, for the list in the sidebar.
    fn list_phones(&self) -> Task<Message> {
        let service = Arc::clone(&self.service);
        Task::perform(
            async move {
                service
                    .phones()
                    .await
                    .map(|phones| mapper::to_phone_responses(&phones))
                    .unwrap_or_default()
            },
            Message::Phones,
        )
    }

    /// How the phone looks to the app, in a line, for a support report. The
    /// serial number is cut to its last four characters.
    pub fn language_changed(&mut self) {
        self.notice = None;
        self.state.reword();
        let phones: Vec<dto::Phone> = self
            .phones
            .iter()
            .map(|phone| phone.source.clone())
            .collect();
        self.phones = mapper::to_phone_responses(&phones);
        self.places = mapper::relabelled(&self.places);
    }

    pub fn status_line(&self) -> String {
        match &self.state {
            State::Idle => english_only("not connected".to_owned()),
            State::Connecting | State::Switching { .. } => english_only("connecting".to_owned()),
            State::Connected {
                device, storages, ..
            } => {
                let serial: Vec<char> = device.serial.chars().collect();
                let tail: String = serial[serial.len().saturating_sub(4)..].iter().collect();
                let serial = if tail.is_empty() {
                    String::new()
                } else {
                    english_only(format!(", serial …{tail}"))
                };
                let count = storages.len();
                let plural = if count == 1 { "" } else { "s" };
                let link = device
                    .link
                    .map(|link| format!(", {}", crate::handler::format::link_label(link)))
                    .unwrap_or_default();
                english_only(format!(
                    "{}{serial}, {count} storage{plural}{link}",
                    device.name
                ))
            }
            State::Failed(failed) => english_only(format!(
                "not connected (last problem: {})",
                error::english_title(&failed.error.source)
            )),
        }
    }

    /// The open phone's USB link; `None` with no phone open or not known.
    pub fn link(&self) -> Option<dto::UsbSpeed> {
        match &self.state {
            State::Connected { device, .. } => device.link,
            _ => None,
        }
    }

    /// What the open phone says it can do; `None` with no phone open.
    /// The open phone is Android.
    pub fn is_android(&self) -> bool {
        matches!(&self.state, State::Connected { device, .. } if device.android)
    }

    pub fn abilities(&self) -> Option<dto::Abilities> {
        match &self.state {
            State::Connected { device, .. } => Some(device.abilities),
            _ => None,
        }
    }

    /// Id and name of the storage picked in the sidebar.
    pub fn selected_storage(&self) -> Option<(u64, String)> {
        let State::Connected {
            storages, selected, ..
        } = &self.state
        else {
            return None;
        };
        storages
            .iter()
            .find(|storage| Some(storage.id) == *selected)
            .map(|storage| (storage.id, storage.name.clone()))
    }

    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Called when another handler finds the device gone mid-operation.
    pub fn lose_device(&mut self, error: ErrorResponse) {
        self.state = State::Failed(error.into());
    }

    pub fn connect(&mut self) -> Task<Message> {
        self.state = State::Connecting;
        self.notice = None;
        Task::perform(
            load_connection(Arc::clone(&self.service), self.preferred.clone()),
            Message::Connected,
        )
    }

    /// "Quit … and try again": lets go of what holds the phone, then connects.
    fn quit_holders(&mut self) -> Task<Message> {
        let State::Failed(Failure {
            diagnosis: Some(diagnosed),
            ..
        }) = &self.state
        else {
            return Task::none();
        };
        if diagnosed.holders.is_empty() {
            return Task::none();
        }
        let holders = diagnosed.holders.clone();
        let service = Arc::clone(&self.service);
        let prefer = self.preferred.clone();
        self.state = State::Connecting;
        Task::perform(
            async move {
                service.release(&holders).await;
                load_connection(service, prefer).await
            },
            Message::Connected,
        )
    }

    fn disconnect(&mut self) -> Task<Message> {
        let service = Arc::clone(&self.service);
        // A failed close still leaves us without a usable session, so the UI
        // returns to idle either way.
        Task::perform(
            async move {
                let _ = service.disconnect().await;
            },
            |()| Message::Disconnected,
        )
    }

    /// The device's part of the sidebar on its own (`app.rs` puts the places
    /// between `sidebar_top` and `sidebar_phones`).
    #[cfg(test)]
    pub fn view_sidebar(&self) -> Element<'_, Message> {
        let mut sidebar = column![self.sidebar_top(None, None)]
            .spacing(20)
            .padding(16);
        if let Some(phones) = self.sidebar_phones() {
            sidebar = sidebar.push(phones);
        }
        iced::widget::scrollable(sidebar).into()
    }

    /// The open phone's serial number and storages (id, name), for what the
    /// sidebar shows of it.
    pub fn open_phone(&self) -> Option<(&str, Vec<(u64, &str)>)> {
        match &self.state {
            State::Connected {
                device, storages, ..
            } => Some((
                device.serial.as_str(),
                storages.iter().map(|s| (s.id, s.name.as_str())).collect(),
            )),
            _ => None,
        }
    }

    /// The top of the sidebar: the app's name, the phone and its storages.
    /// The usual places on the open storage, `shown` marking the folder the
    /// phone pane shows.
    fn place_list(
        &self,
        shown: Option<&str>,
        storage: Option<&StorageResponse>,
        drop: Option<&SidebarDrop>,
    ) -> Option<Element<'_, Message>> {
        if self.places.is_empty() {
            return None;
        }
        // A read-only storage takes no drop.
        let drop = drop.filter(|_| storage.is_some_and(|storage| !storage.read_only));
        let rows = self.places.iter().map(|(label, path)| {
            let go = button(text(label).size(14))
                .width(Length::Fill)
                .padding([4, 10])
                .style(if shown == Some(path.as_str()) {
                    button::secondary
                } else {
                    button::subtle
                })
                .on_press(Message::Request(Request::GoToPlace(path.clone())));
            match storage {
                Some(storage) => {
                    let spot = Spot::Phone {
                        storage_id: storage.id,
                        path: path.clone(),
                    };
                    drop_spot(go, spot, drop, Message::DropHover)
                }
                None => go.into(),
            }
        });
        Some(
            column![
                text(t("device.sidebar.on_the_phone"))
                    .size(13)
                    .style(text::secondary),
                column(rows).spacing(2),
            ]
            .spacing(8)
            .into(),
        )
    }

    /// While a drag is on (`drop`), each storage and usual place it can be
    /// dropped on says when the pointer is over it.
    pub fn sidebar_top(
        &self,
        phone_path: Option<&str>,
        drop: Option<&SidebarDrop>,
    ) -> Element<'_, Message> {
        let header = text("open-mtp-client").size(22);

        let body: Element<'_, Message> = match &self.state {
            State::Connected {
                device,
                storages,
                selected,
            } => {
                let mut body = column![
                    device_card(device),
                    rule::horizontal(1),
                    text(t("device.sidebar.storage"))
                        .size(13)
                        .style(text::secondary),
                    column(storages.iter().map(|storage| {
                        let row = storage_row(storage, *selected == Some(storage.id));
                        if storage.read_only {
                            return row;
                        }
                        let spot = Spot::Phone {
                            storage_id: storage.id,
                            path: "/".into(),
                        };
                        drop_spot(row, spot, drop, Message::DropHover)
                    }))
                    .spacing(6),
                ]
                .spacing(14);
                let open = storages
                    .iter()
                    .find(|storage| *selected == Some(storage.id));
                if let Some(places) = self.place_list(phone_path, open, drop) {
                    body = body.push(places);
                }
                body.into()
            }
            State::Connecting => text(t("device.sidebar.looking"))
                .style(text::secondary)
                .into(),
            State::Switching { to, .. } => text(tf(
                "device.sidebar.opening",
                &[("to", &to_name(to.as_deref()))],
            ))
            .style(text::secondary)
            .into(),
            State::Idle | State::Failed(_) => text(t("error.not_connected.title"))
                .style(text::secondary)
                .into(),
        };

        column![header, body].spacing(20).into()
    }

    /// The other phones plugged in, one click from being opened. With a
    /// single phone open there is nothing to choose, so nothing is shown.
    pub fn sidebar_phones(&self) -> Option<Element<'_, Message>> {
        let open = self.open_key();
        let others = self
            .phones
            .iter()
            .any(|phone| Some(&phone.key) != open.as_ref());
        (others && (open.is_some() || self.phones.len() > 1))
            .then(|| self.phone_list(open.as_deref()))
    }

    /// "Phones": every phone plugged in, the open one marked, the others a
    /// click from being opened; and why the last switch did not happen.
    fn phone_list(&self, open: Option<&str>) -> Element<'_, Message> {
        let rows = self.phones.iter().map(|phone| {
            let is_open = open == Some(phone.key.as_str());
            let label = row![text(&phone.name).size(14)]
                .push(if is_open {
                    // Inherits the button's colour: text::secondary is
                    // invisible on the selected row's background.
                    text(t("device.sidebar.open_marker")).size(11)
                } else {
                    text("")
                })
                .spacing(8)
                .align_y(Alignment::Center);
            button(label)
                .width(Length::Fill)
                .padding([6, 10])
                .style(if is_open {
                    button::secondary
                } else {
                    button::subtle
                })
                // Opening the open phone does nothing; a button without a press
                // would be drawn greyed out, as if that phone were unusable.
                .on_press(Message::Request(Request::Open(phone.key.clone())))
                .into()
        });
        let mut list = column![
            rule::horizontal(1),
            text(t("device.sidebar.phones"))
                .size(13)
                .style(text::secondary),
            column(rows).spacing(4),
        ]
        .spacing(10);
        if let Some(notice) = &self.notice {
            list = list.push(text(notice).size(12).style(text::secondary));
        }
        list.into()
    }

    /// The main pane while there is nothing to browse yet: onboarding,
    /// progress, or what went wrong and how to fix it.
    pub fn view_main(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match &self.state {
            State::Idle if self.phones.len() > 1 => self.picker(),
            State::Idle => onboarding(),
            State::Connecting => column![
                text(t("device.main.connecting")).size(24),
                text(t("device.main.opening_session")).style(text::secondary),
            ]
            .spacing(8)
            .align_x(Alignment::Center)
            .into(),
            State::Switching { to, .. } => column![
                text(tf(
                    "device.sidebar.opening",
                    &[("to", &to_name(to.as_deref()))],
                ))
                .size(24),
                text(t("device.main.closing_first")).style(text::secondary),
            ]
            .spacing(8)
            .align_x(Alignment::Center)
            .into(),
            State::Failed(failed) if failed.choose && !self.phones.is_empty() => self.picker(),
            State::Failed(failed) => failure(failed),
            State::Connected { .. } => text(t("device.main.pick_storage")).into(),
        };

        container(content).center(Length::Fill).padding(32).into()
    }
}

/// Changes on the connected phone, until it is gone.
fn changes(service: Arc<dyn Service>) -> BoxStream<'static, dto::ContentChange> {
    stream::unfold(service, |service| async move {
        let change = service.next_change().await.ok()?;
        Some((change, service))
    })
    .boxed()
}

/// Recent automatic connects, to keep within `MAX_AUTO_CONNECTS`.
#[derive(Default)]
struct Attempts(VecDeque<Instant>);

impl Attempts {
    /// Records an attempt at `now` if one is allowed.
    fn allow(&mut self, now: Instant) -> bool {
        while self
            .0
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RETRY_WINDOW)
        {
            self.0.pop_front();
        }
        if self.0.len() >= MAX_AUTO_CONNECTS {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

/// Opens the device and reads its storages, ready for display. A failure
/// comes back with a diagnosis of what may be behind it.
async fn load_connection(
    service: Arc<dyn Service>,
    prefer: Option<String>,
) -> Result<Connection, Failure> {
    match connect_freeing(&service, prefer).await {
        Ok(connection) => Ok(connection),
        Err(err) => Err(diagnosed(&service, err).await),
    }
}

/// Connects; when the phone is kept by something and macOS's camera services
/// are all that could be keeping it, they are ended and the phone opened
/// again at once (they relaunch, so nothing is waited for), once. Anything
/// else is the doctor's to show, as before.
async fn connect_freeing(
    service: &Arc<dyn Service>,
    prefer: Option<String>,
) -> Result<Connection, dto::Error> {
    match connect(service, prefer.clone()).await {
        Err(dto::Error::DeviceInUse) if service.free_from_camera().await => {
            connect(service, prefer).await
        }
        other => other,
    }
}

/// Opens the phone `key` in place of the open one. A phone that has gone
/// leaves the open one as it was.
async fn switch(service: Arc<dyn Service>, key: String) -> Switch {
    match connect_freeing(&service, Some(key)).await {
        Ok(connection) => Switch::Opened(connection),
        Err(dto::Error::PhoneUnplugged) => Switch::Gone,
        Err(err) => Switch::Failed(diagnosed(&service, err).await),
    }
}

async fn connect(
    service: &Arc<dyn Service>,
    prefer: Option<String>,
) -> Result<Connection, dto::Error> {
    let device = service.connect(prefer).await?;
    let storages = service.storages().await?;
    let battery = service.battery().await?;
    let mut device = mapper::to_device_response(&device);
    device.battery_level = battery;
    device.battery = mapper::battery_label(battery);
    Ok((
        device,
        storages.iter().map(mapper::to_storage_response).collect(),
    ))
}

async fn diagnosed(service: &Arc<dyn Service>, err: dto::Error) -> Failure {
    let choose = err == dto::Error::MultipleDevices;
    let diagnosis = service.diagnose(&err).await;
    let diagnosed = Diagnosed::of(&diagnosis);
    let mut error = error::to_error_response(err);
    if diagnosis.owner_seen {
        // macOS named who has it: guesses such as "Image Capture or Photos"
        // would only blur that.
        error.hint.clear();
    }
    Failure {
        error,
        choose,
        diagnosis: (!diagnosed.response.checks.is_empty()).then(|| Box::new(diagnosed)),
    }
}

impl Handler {
    /// Several phones plugged in and none open: which one?
    fn picker(&self) -> Element<'_, Message> {
        let buttons = self.phones.iter().map(|phone| {
            button(text(&phone.name).size(16))
                .width(Length::Fill)
                .padding([10, 24])
                .style(button::secondary)
                .on_press(Message::Request(Request::Open(phone.key.clone())))
                .into()
        });
        column![
            text(count("device.picker.title", self.phones.len(), &[])).size(28),
            text(t("device.picker.hint")).style(text::secondary),
            column(buttons).spacing(8).max_width(320),
        ]
        .spacing(16)
        .max_width(460)
        .into()
    }
}

fn device_card(device: &DeviceResponse) -> Element<'_, Message> {
    // "Android 15 · Battery 76% · USB 2", whichever the phone tells.
    let link = device.link.map(crate::handler::format::link_label);
    let state = [
        Some(device.version.as_str()),
        device.battery.as_deref(),
        link,
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    column![
        text(&device.name).size(18),
        text(&device.details).size(13).style(text::secondary),
    ]
    .push((!state.is_empty()).then(|| text(state).size(13)))
    .push(
        button(text(t("device.card.import")).size(13))
            .style(button::text)
            .padding(0)
            .on_press(Message::Request(Request::Import)),
    )
    .push(
        button(text(t("device.card.free_up")).size(13))
            .style(button::text)
            .padding(0)
            .on_press(Message::Request(Request::FreeUpSpace)),
    )
    .push(
        button(text(t("label.disconnect")).size(13))
            .style(button::text)
            .padding(0)
            .on_press(Message::Request(Request::Disconnect)),
    )
    .spacing(4)
    .into()
}

fn storage_row(storage: &StorageResponse, is_selected: bool) -> Element<'_, Message> {
    let mut name = row![text(&storage.name).size(15)]
        .spacing(6)
        .align_y(Alignment::Center);
    if storage.read_only {
        name = name.push(
            text(t("device.storage.read_only"))
                .size(11)
                .style(text::secondary),
        );
    }

    let content = column![
        name,
        progress_bar(0.0..=1.0, storage.used_fraction).girth(4),
        // Inherits the button's text colour; text::secondary is invisible on
        // the selected-row background.
        text(&storage.usage).size(12),
    ]
    .spacing(6);

    button(content)
        .width(Length::Fill)
        .padding(10)
        .style(if is_selected {
            button::secondary
        } else {
            button::subtle
        })
        .on_press(Message::Request(Request::SelectStorage(storage.id)))
        .into()
}

fn onboarding<'a>() -> Element<'a, Message> {
    let steps = column![
        text(t("device.onboarding.step_1")),
        text(t("device.onboarding.step_2")),
        text(t("device.onboarding.step_3")),
    ]
    .spacing(8);

    column![
        text(t("device.onboarding.title")).size(28),
        steps,
        space().height(8),
        button(text(t("label.connect")).size(16))
            .padding([10, 24])
            .style(button::primary)
            .on_press(Message::Request(Request::Connect)),
    ]
    .spacing(16)
    .max_width(460)
    .into()
}

fn failure(failed: &Failure) -> Element<'_, Message> {
    let error = &failed.error;
    let mut content = column![text(&error.title).size(24)]
        .spacing(12)
        .max_width(460);
    if !error.hint.is_empty() {
        content = content.push(text(&error.hint).style(text::secondary));
    }

    // A phone that went away or stopped answering is reconnected; anything
    // else is simply tried again.
    let again = if error.device_lost {
        t("label.reconnect")
    } else {
        t("label.try_again")
    };
    let try_again = |style: fn(&iced::Theme, button::Status) -> button::Style| {
        button(text(again).size(16))
            .padding([10, 24])
            .style(style)
            .on_press(Message::Request(Request::Connect))
    };
    let mut buttons = row![].spacing(12);
    match &failed.diagnosis {
        Some(diagnosed) => {
            content = content.push(diagnosis::checklist(&diagnosed.response.checks));
            match &diagnosed.response.quit_label {
                Some(label) => {
                    buttons = buttons
                        .push(
                            button(text(label).size(16))
                                .padding([10, 24])
                                .style(button::primary)
                                .on_press(Message::Request(Request::QuitHolders)),
                        )
                        .push(try_again(button::secondary));
                }
                None => buttons = buttons.push(try_again(button::primary)),
            }
        }
        None => buttons = buttons.push(try_again(button::primary)),
    }
    content.push(buttons).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeDevice, phone, storage};

    fn handler() -> Handler {
        Handler::new(Arc::new(FakeDevice::with_storages(vec![storage(
            1, "Internal",
        )])))
    }

    fn connected(storages: &[dto::Storage]) -> Message {
        Message::connected(&phone(), storages)
    }

    fn failed(err: dto::Error) -> Message {
        Message::Connected(Err(error::to_error_response(err).into()))
    }

    // The phone's usual places.

    async fn settle_task(handler: &mut Handler, task: Task<Message>) {
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    fn sidebar_says(element: Element<'_, Message>, words: &str) -> bool {
        iced_test::simulator(element).find(words).is_ok()
    }

    /// Moves the pointer onto the words `on` and then well below everything.
    fn pass_over(element: Element<'_, Message>, on: &str) -> Vec<Message> {
        use iced::{Event, Point, mouse};
        let mut ui = iced_test::simulator(element);
        let at = ui.find(on).unwrap().bounds().center();
        let away = Point::new(at.x, at.y + 5_000.0);
        for point in [at, away] {
            ui.point_at(point);
            ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: point })]);
        }
        ui.into_messages().collect()
    }

    #[tokio::test]
    async fn while_a_drag_is_on_a_storage_or_a_usual_place_says_when_the_pointer_is_over_it() {
        use crate::handler::drop_spot::{Origin, SidebarDrop};
        let service = Arc::new(FakeDevice {
            present: vec!["/DCIM/Camera".into()],
            ..FakeDevice::with_storages(vec![])
        });
        let mut handler = Handler::new(service);
        let read_only = dto::Storage {
            is_writable: false,
            ..storage(3, "Cards")
        };
        let asked = handler.update(connected(&[
            storage(1, "Internal"),
            storage(2, "SD card"),
            read_only,
        ]));
        settle_task(&mut handler, asked).await;
        let drop = SidebarDrop {
            over: None,
            from: Origin::Mac {
                folder: "/Users/me".into(),
            },
        };
        let hovers = |messages: Vec<Message>| -> Vec<(Spot, bool)> {
            messages
                .into_iter()
                .filter_map(|message| match message {
                    Message::DropHover(spot, over) => Some((spot, over)),
                    _ => None,
                })
                .collect()
        };

        let sd = Spot::Phone {
            storage_id: 2,
            path: "/".into(),
        };
        assert_eq!(
            hovers(pass_over(handler.sidebar_top(None, Some(&drop)), "SD card")),
            [(sd.clone(), true), (sd, false)],
            "a storage is its top folder"
        );
        let camera = Spot::Phone {
            storage_id: 1,
            path: "/DCIM/Camera".into(),
        };
        assert_eq!(
            hovers(pass_over(handler.sidebar_top(None, Some(&drop)), "Camera")),
            [(camera.clone(), true), (camera, false)],
            "a usual place, on the storage open"
        );
        assert!(
            hovers(pass_over(handler.sidebar_top(None, Some(&drop)), "Cards")).is_empty(),
            "a read-only storage takes no drop"
        );
        assert!(hovers(pass_over(handler.sidebar_top(None, None), "SD card")).is_empty());

        // The storage open is read-only: its usual places take nothing.
        let service = Arc::new(FakeDevice {
            present: vec!["/DCIM/Camera".into()],
            ..FakeDevice::with_storages(vec![])
        });
        let mut cards = Handler::new(service);
        let asked = cards.update(connected(&[dto::Storage {
            is_writable: false,
            ..storage(3, "Cards")
        }]));
        settle_task(&mut cards, asked).await;
        assert!(sidebar_says(cards.sidebar_top(None, None), "Camera"));
        assert!(hovers(pass_over(cards.sidebar_top(None, Some(&drop)), "Camera")).is_empty());
        let _ = handler.update(Message::DropHover(
            Spot::Phone {
                storage_id: 2,
                path: "/".into(),
            },
            true,
        ));
    }

    #[tokio::test]
    async fn the_usual_places_found_on_the_storage_are_offered_under_it() {
        let service = Arc::new(FakeDevice {
            present: vec!["/DCIM/Camera".into(), "/Pictures/Screenshots".into()],
            ..FakeDevice::with_storages(vec![])
        });
        let mut handler = Handler::new(service.clone());
        assert!(!sidebar_says(
            handler.sidebar_top(None, None),
            "On the phone"
        ));

        let asked = handler.update(connected(&[storage(1, "Internal"), storage(2, "SD card")]));
        settle_task(&mut handler, asked).await;

        assert_eq!(
            *service.places_asked.lock().unwrap(),
            [1],
            "the storage open"
        );
        assert!(sidebar_says(
            handler.sidebar_top(None, None),
            "On the phone"
        ));
        assert!(sidebar_says(handler.sidebar_top(None, None), "Camera"));
        assert!(sidebar_says(handler.sidebar_top(None, None), "Screenshots"));
        assert!(
            !sidebar_says(handler.sidebar_top(None, None), "Music"),
            "not on this phone"
        );

        let mut ui = iced_test::simulator(handler.sidebar_top(None, None));
        let _ = ui.click("Camera").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::Request(Request::GoToPlace(path))] if path == "/DCIM/Camera"
        ));
    }

    #[tokio::test]
    async fn another_storage_is_asked_again_and_an_old_answer_is_dropped() {
        let service = Arc::new(FakeDevice {
            present: vec!["/Download".into()],
            ..FakeDevice::with_storages(vec![])
        });
        let mut handler = Handler::new(service.clone());
        let first = handler.update(connected(&[storage(1, "Internal"), storage(2, "SD card")]));

        let second = handler.update(Message::Request(Request::SelectStorage(2)));
        settle_task(&mut handler, first).await;
        assert!(
            !sidebar_says(handler.sidebar_top(None, None), "Downloads"),
            "about storage 1: dropped"
        );
        settle_task(&mut handler, second).await;
        assert!(sidebar_says(handler.sidebar_top(None, None), "Downloads"));
        assert_eq!(*service.places_asked.lock().unwrap(), [1, 2]);

        let _ = handler.update(Message::Disconnected);
        assert!(handler.places.is_empty(), "no phone, no places");
    }

    #[tokio::test]
    async fn a_place_that_could_not_be_looked_up_shows_nothing() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(Message::Places {
            key: handler.places_for.clone().unwrap().0,
            storage_id: 1,
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });

        assert!(!sidebar_says(
            handler.sidebar_top(None, None),
            "On the phone"
        ));
    }

    #[test]
    fn starts_idle_with_no_storage() {
        let handler = handler();

        assert!(matches!(handler.state, State::Idle));
        assert_eq!(handler.selected_storage(), None);
    }

    #[test]
    fn connect_shows_progress() {
        let mut handler = handler();

        let _ = handler.connect();

        assert!(matches!(handler.state, State::Connecting));
    }

    #[test]
    fn connect_request_starts_connecting() {
        let mut handler = handler();

        let _ = handler.update(Message::Request(Request::Connect));

        assert!(matches!(handler.state, State::Connecting));
    }

    #[test]
    fn connecting_selects_the_first_storage() {
        let mut handler = handler();

        let _ = handler.update(connected(&[storage(7, "Internal"), storage(9, "SD card")]));

        assert_eq!(handler.selected_storage(), Some((7, "Internal".into())));
    }

    #[test]
    fn a_device_without_storages_selects_nothing() {
        let mut handler = handler();

        let _ = handler.update(connected(&[]));

        assert_eq!(handler.selected_storage(), None);
    }

    #[test]
    fn picking_a_storage_selects_it() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(7, "Internal"), storage(9, "SD card")]));

        let _ = handler.update(Message::Request(Request::SelectStorage(9)));

        assert_eq!(handler.selected_storage(), Some((9, "SD card".into())));
    }

    #[test]
    fn picking_a_storage_while_disconnected_does_nothing() {
        let mut handler = handler();

        let _ = handler.update(Message::Request(Request::SelectStorage(9)));

        assert!(matches!(handler.state, State::Idle));
    }

    fn in_thai<Output>(work: impl FnOnce() -> Output) -> Output {
        crate::i18n::with_language(dto::Language::Thai, work)
    }

    fn phone_named(key: &str, name: &str) -> PhoneResponse {
        PhoneResponse {
            key: key.into(),
            name: name.into(),
            source: dto::Phone {
                key: key.into(),
                manufacturer: String::new(),
                model: name.into(),
                serial: String::new(),
            },
        }
    }

    #[test]
    fn a_language_switch_rewords_the_phone_card_and_the_storage_lines() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Battery(Ok(Some(76))));
        let seen = |handler: &Handler| match &handler.state {
            State::Connected {
                device, storages, ..
            } => (
                device.battery.clone(),
                storages[0].usage.clone(),
                device.name.clone(),
            ),
            _ => panic!("not connected"),
        };
        assert_eq!(
            seen(&handler),
            (
                Some("Battery 76%".into()),
                "16.0 GB free of 64.0 GB".into(),
                "Pixel 9".into()
            )
        );

        in_thai(|| handler.language_changed());

        assert_eq!(
            seen(&handler),
            in_thai(|| (
                Some(tf("device.battery", &[("level", "76")])),
                tf(
                    "device.usage",
                    &[("free", "16.0 GB"), ("capacity", "64.0 GB")]
                ),
                "Pixel 9".into()
            ))
        );
    }

    #[test]
    fn a_language_switch_says_a_failed_connect_again_with_its_checklist() {
        let mut handler = handler();
        let _ = handler.update(failed(dto::Error::NoDevice));
        let diagnosis = dto::Diagnosis {
            phones: Some(Vec::new()),
            ..Default::default()
        };
        let mut failure = Failure::from(error::to_error_response(dto::Error::NoDevice));
        failure.diagnosis = Some(Box::new(Diagnosed::of(&diagnosis)));
        let _ = handler.update(Message::Connected(Err(failure)));

        in_thai(|| handler.language_changed());

        let State::Failed(failed) = &handler.state else {
            panic!("still failed");
        };
        let said = in_thai(|| error::describe_again(&dto::Error::NoDevice).title);
        assert_ne!(said, error::describe_again(&dto::Error::NoDevice).title);
        assert_eq!(failed.error.title, said);
        let checks = &failed.diagnosis.as_ref().unwrap().response.checks;
        assert_eq!(
            checks[0].text,
            in_thai(|| diagnosis::to_diagnosis_response(&diagnosis).checks[0]
                .text
                .clone())
        );
        assert_ne!(
            checks[0].text,
            "No phone seen on USB. Try another cable (some only charge), another port, and no USB hub."
        );
    }

    #[test]
    fn a_language_switch_rewords_the_usual_places() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Places {
            key: handler.places_for.clone().unwrap().0,
            storage_id: 1,
            result: Ok(vec!["/DCIM/Camera".into(), "/Music".into()]),
        });
        let labels = |handler: &Handler| -> Vec<String> {
            handler
                .places
                .iter()
                .map(|(label, _)| label.clone())
                .collect()
        };
        assert_eq!(labels(&handler), ["Camera", "Music"]);

        in_thai(|| handler.language_changed());

        assert_eq!(
            labels(&handler),
            in_thai(|| [t("device.place.camera"), t("device.place.music")])
        );
        assert_eq!(handler.places[0].1, "/DCIM/Camera");
    }

    #[test]
    fn a_language_switch_clears_the_notice_and_names_an_unnamed_phone_again() {
        let mut handler = handler();
        handler.notice = Some("Pixel 7 is no longer plugged in.".into());
        let unnamed = dto::Phone {
            key: "usb@1".into(),
            manufacturer: String::new(),
            model: String::new(),
            serial: String::new(),
        };
        let _ = handler.update(Message::Phones(mapper::to_phone_responses(&[unnamed])));
        assert_eq!(handler.phones[0].name, "Phone");

        in_thai(|| handler.language_changed());

        assert!(handler.notice.is_none());
        assert_eq!(
            handler.phones[0].name,
            in_thai(|| t("device.phone_fallback"))
        );
        assert_ne!(handler.phones[0].name, "Phone");
    }

    #[test]
    fn a_language_switch_in_the_middle_of_a_switch_keeps_the_phone_it_came_from() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        in_thai(|| handler.language_changed());

        assert!(handler.is_connected());
        assert_eq!(handler.selected_storage(), Some((1, "Internal".to_owned())));
    }

    #[test]
    fn a_failed_connect_shows_the_error() {
        let mut handler = handler();

        let _ = handler.update(failed(dto::Error::NoDevice));

        assert!(
            matches!(&handler.state, State::Failed(failed) if failed.error.title == "No device found")
        );
        assert_eq!(handler.selected_storage(), None);
    }

    #[test]
    fn disconnecting_returns_to_idle() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(Message::Request(Request::Disconnect));
        let _ = handler.update(Message::Disconnected);

        assert!(matches!(handler.state, State::Idle));
    }

    #[test]
    fn losing_the_device_shows_why() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        handler.lose_device(error::to_error_response(dto::Error::Disconnected));

        assert!(matches!(&handler.state, State::Failed(failed) if failed.error.device_lost));
        assert_eq!(handler.selected_storage(), None);
    }

    #[tokio::test]
    async fn load_connection_formats_device_and_storages() {
        let service = Arc::new(FakeDevice::with_storages(vec![storage(1, "Internal")]));

        let (device, storages) = load_connection(service, None).await.unwrap();

        assert_eq!(device.name, "Pixel 9");
        assert_eq!(storages[0].usage, "16.0 GB free of 64.0 GB");
    }

    #[tokio::test]
    async fn load_connection_reports_a_connect_failure() {
        let service = Arc::new(FakeDevice {
            connect: Err(dto::Error::MultipleDevices),
            ..FakeDevice::with_storages(vec![])
        });

        let error = load_connection(service, None).await.unwrap_err();

        assert_eq!(error.error.title, "Several phones are plugged in");
    }

    #[tokio::test]
    async fn a_phone_kept_by_the_camera_services_alone_is_freed_and_opened_again_once() {
        let service = Arc::new(FakeDevice {
            cameras_only: true,
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        });
        service
            .first_connects
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::DeviceInUse));

        let (device, _) = load_connection(service.clone(), None).await.unwrap();

        assert_eq!(device.name, "Pixel 9");
        assert_eq!(
            service
                .cameras_ended
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            service.asked.lock().unwrap().len(),
            2,
            "opened again at once"
        );
    }

    #[tokio::test]
    async fn the_phone_is_freed_once_and_anything_else_is_the_doctors_to_show() {
        // Still kept after the camera services were ended: no second go.
        let stubborn = Arc::new(FakeDevice {
            cameras_only: true,
            connect: Err(dto::Error::DeviceInUse),
            ..FakeDevice::with_storages(vec![])
        });
        let failure = load_connection(stubborn.clone(), None).await.unwrap_err();
        assert_eq!(
            failure.error.title,
            error::to_error_response(dto::Error::DeviceInUse).title
        );
        assert_eq!(
            stubborn
                .cameras_ended
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(stubborn.asked.lock().unwrap().len(), 2);

        // Something else could be keeping it: nothing ended, asked as before.
        let others = Arc::new(FakeDevice {
            connect: Err(dto::Error::DeviceInUse),
            ..FakeDevice::with_storages(vec![])
        });
        let _ = load_connection(others.clone(), None).await.unwrap_err();
        assert_eq!(others.asked.lock().unwrap().len(), 1, "not opened again");

        // Another failure never ends anything.
        let lost = Arc::new(FakeDevice {
            cameras_only: true,
            connect: Err(dto::Error::NoDevice),
            ..FakeDevice::with_storages(vec![])
        });
        let _ = load_connection(lost.clone(), None).await.unwrap_err();
        assert_eq!(
            lost.cameras_ended
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        // Switching to a phone the camera services keep frees it too.
        let switching = Arc::new(FakeDevice {
            cameras_only: true,
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        });
        switching
            .first_connects
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::DeviceInUse));
        assert!(matches!(
            switch(switching.clone(), "ABC123".into()).await,
            Switch::Opened(_)
        ));
    }

    #[tokio::test]
    async fn load_connection_reports_a_storage_failure() {
        let service = Arc::new(FakeDevice {
            storages: Err(dto::Error::AllowStorageAccess),
            ..FakeDevice::with_storages(vec![])
        });

        let error = load_connection(service, None).await.unwrap_err();

        assert_eq!(error.error.title, "Allow access on your phone");
    }

    #[test]
    fn every_state_renders() {
        let mut handler = handler();
        let _ = handler.view_sidebar();
        let _ = handler.view_main();

        let _ = handler.connect();
        let _ = handler.view_sidebar();
        let _ = handler.view_main();

        let read_only = dto::Storage {
            is_writable: false,
            ..storage(9, "SD card")
        };
        let _ = handler.update(connected(&[storage(1, "Internal"), read_only]));
        let _ = handler.view_sidebar();
        let _ = handler.view_main();

        let _ = handler.update(failed(dto::Error::Timeout));
        let _ = handler.view_sidebar();
        let _ = handler.view_main();
    }

    #[test]
    fn an_error_without_a_hint_still_renders() {
        let error = ErrorResponse {
            title: "Oops".into(),
            hint: String::new(),
            device_lost: false,
            privacy: None,
            cancelled: false,
            source: dto::Error::NoDevice,
        };

        let _ = failure(&error.into());
    }

    fn plugged() -> Message {
        Message::Hotplug(dto::DeviceChange::Plugged {
            serial: Some("ABC123".into()),
        })
    }

    fn unplugged(serial: Option<&str>) -> Message {
        Message::Hotplug(dto::DeviceChange::Unplugged {
            serial: serial.map(str::to_owned),
        })
    }

    #[test]
    fn plugging_in_a_phone_connects_it() {
        let mut handler = handler();

        let _ = handler.update(plugged());

        assert!(matches!(handler.state, State::Connecting));
    }

    #[test]
    fn plugging_in_after_a_failure_tries_again() {
        let mut handler = handler();
        let _ = handler.update(failed(dto::Error::NoDevice));

        let _ = handler.update(plugged());

        assert!(matches!(handler.state, State::Connecting));
    }

    #[test]
    fn plugging_in_while_connected_or_with_auto_connect_off_does_nothing() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(plugged());
        assert!(matches!(handler.state, State::Connected { .. }));

        let mut handler = self::handler();
        handler.set_auto_connect(false);
        let _ = handler.update(plugged());
        assert!(handler.is_idle());
    }

    #[test]
    fn unplugging_the_connected_phone_goes_back_to_idle() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(unplugged(Some("ABC123")));

        assert!(handler.is_idle());
    }

    #[tokio::test]
    async fn a_phone_leaving_without_a_serial_is_the_open_one_only_if_it_is_no_longer_listed() {
        // The open phone is the only one: it is the one that left.
        let mut alone = handler();
        settle(&mut alone, connected(&[storage(1, "Internal")])).await;
        settle(&mut alone, unplugged(None)).await;
        assert!(alone.is_idle());

        // Another phone is plugged in, and the open one is still listed.
        let mut two = Handler::new(two_plugged());
        settle(&mut two, connected(&[storage(1, "Internal")])).await;
        settle(&mut two, unplugged(None)).await;
        assert!(two.is_connected(), "the other phone left");

        // A phone that reports no serial can't be matched by the event's.
        let nameless = dto::Device {
            abilities: dto::Abilities::default(),
            serial_number: String::new(),
            ..phone()
        };
        let mut handler = Handler::new(two_plugged());
        settle(
            &mut handler,
            Message::connected(&nameless, &[storage(1, "Internal")]),
        )
        .await;
        settle(&mut handler, unplugged(Some("S24"))).await;
        assert!(handler.is_connected());
    }

    #[test]
    fn unplugging_another_phone_changes_nothing() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(unplugged(Some("OTHER")));
        assert!(matches!(handler.state, State::Connected { .. }));

        let mut idle = self::handler();
        let _ = idle.update(unplugged(Some("ABC123")));
        assert!(idle.is_idle());
    }

    #[test]
    fn unplugging_with_auto_connect_off_waits_for_the_user() {
        let mut handler = handler();
        handler.set_auto_connect(false);
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(unplugged(Some("ABC123")));

        assert!(matches!(handler.state, State::Connected { .. }));
    }

    #[test]
    fn reloaded_storages_keep_the_selection_when_it_is_still_there() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal"), storage(2, "SD card")]));
        let _ = handler.update(Message::Request(Request::SelectStorage(2)));
        let _ = handler.update(Message::Changed(dto::ContentChange::Storages));

        let _ = handler.update(Message::StoragesReloaded(Ok(vec![
            mapper::to_storage_response(&storage(1, "Internal")),
            mapper::to_storage_response(&storage(2, "SD card")),
        ])));
        assert_eq!(handler.selected_storage(), Some((2, "SD card".into())));

        // The SD card is taken out.
        let _ = handler.update(Message::StoragesReloaded(Ok(vec![
            mapper::to_storage_response(&storage(1, "Internal")),
        ])));
        assert_eq!(handler.selected_storage(), Some((1, "Internal".into())));
    }

    #[test]
    fn a_failed_storage_reload_only_matters_if_the_phone_is_gone() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let _ = handler.update(Message::StoragesReloaded(Err(error::to_error_response(
            dto::Error::ReadOnly,
        ))));
        assert!(matches!(handler.state, State::Connected { .. }));

        let _ = handler.update(Message::StoragesReloaded(Err(error::to_error_response(
            dto::Error::Disconnected,
        ))));
        assert!(handler.is_idle());
    }

    #[test]
    fn changes_while_idle_or_to_files_leave_the_sidebar_alone() {
        let mut handler = handler();
        let _ = handler.update(Message::Changed(dto::ContentChange::Storages));
        let _ = handler.update(Message::StoragesReloaded(Ok(vec![])));
        assert!(handler.is_idle());

        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Changed(dto::ContentChange::Files));
        assert_eq!(handler.selected_storage(), Some((1, "Internal".into())));
    }

    #[test]
    fn a_watch_that_cannot_start_is_not_fatal() {
        let handler = Handler::new(Arc::new(FakeDevice {
            watch_fails: true,
            ..FakeDevice::with_storages(vec![])
        }));

        let _ = handler.watch();

        assert!(handler.is_idle());
    }

    #[tokio::test]
    async fn the_change_stream_ends_when_the_phone_is_gone() {
        let service = FakeDevice::with_storages(vec![]);
        service
            .changes
            .lock()
            .unwrap()
            .extend([dto::ContentChange::Files, dto::ContentChange::Storages]);

        let seen: Vec<dto::ContentChange> = changes(Arc::new(service)).collect().await;

        assert_eq!(
            seen,
            [dto::ContentChange::Files, dto::ContentChange::Storages]
        );
    }

    #[test]
    fn automatic_connects_are_limited_per_window() {
        let mut attempts = Attempts::default();
        let start = Instant::now();

        for i in 0..MAX_AUTO_CONNECTS {
            assert!(attempts.allow(start + Duration::from_secs(i as u64)));
        }
        assert!(!attempts.allow(start + Duration::from_secs(10)));
        assert!(attempts.allow(start + RETRY_WINDOW + Duration::from_secs(1)));
    }
    fn holder(name: &str, restarts: bool) -> dto::Holder {
        dto::Holder {
            name: name.to_owned(),
            restarts,
        }
    }

    /// A phone that cannot connect because `holders` have it.
    fn held_by(holders: Vec<dto::Holder>) -> Arc<FakeDevice> {
        Arc::new(FakeDevice {
            connect: Err(dto::Error::DeviceInUse),
            diagnosis: dto::Diagnosis {
                holders: Some(holders),
                phones: None,
                ..Default::default()
            },
            ..FakeDevice::with_storages(vec![])
        })
    }

    fn diagnosed_holders(handler: &Handler) -> Vec<String> {
        let State::Failed(Failure {
            diagnosis: Some(diagnosed),
            ..
        }) = &handler.state
        else {
            panic!("expected a diagnosed failure");
        };
        diagnosed.holders.iter().map(|h| h.name.clone()).collect()
    }

    #[tokio::test]
    async fn a_failed_connect_comes_back_with_what_was_found() {
        let service = held_by(vec![holder("Photos", false)]);

        let failed = load_connection(service, None).await.unwrap_err();

        assert_eq!(failed.error.title, "Another app is using your phone");
        assert!(
            !failed.error.hint.is_empty(),
            "only guesses: the usual hint stays"
        );
        let diagnosed = failed.diagnosis.unwrap();
        assert_eq!(
            diagnosed.response.quit_label.as_deref(),
            Some("Quit Photos and try again")
        );
        assert_eq!(diagnosed.holders, [holder("Photos", false)]);
    }

    #[tokio::test]
    async fn a_phone_held_by_a_program_the_app_does_not_know_says_so_without_a_quit_button() {
        let service = Arc::new(FakeDevice {
            connect: Err(dto::Error::DeviceInUse),
            diagnosis: dto::Diagnosis {
                holders: Some(vec![]),
                owner_seen: true,
                unknown_owners: vec!["SyncMate".into()],
                ..Default::default()
            },
            ..FakeDevice::with_storages(vec![])
        });
        let failed = load_connection(Arc::clone(&service) as Arc<dyn Service>, None)
            .await
            .unwrap_err();
        assert_eq!(failed.error.hint, "", "macOS named it: no guessing at apps");
        let mut handler = Handler::new(service as Arc<dyn Service>);
        let _ = handler.update(Message::Connected(Err(failed)));

        let mut ui = iced_test::simulator(handler.view_main());
        assert!(ui.find("macOS says “SyncMate” has your phone. Quit it (Activity Monitor finds it by that name), or restart your Mac if you don't know it, then try again.").is_ok());
        assert!(ui.find("Quit SyncMate and try again").is_err());
        drop(ui);
        assert!(diagnosed_holders(&handler).is_empty());
        let _ = handler.update(Message::Request(Request::QuitHolders));
        assert!(
            matches!(handler.state, State::Failed(_)),
            "nothing to quit: the failure stays"
        );
    }

    #[tokio::test]
    async fn a_failure_with_nothing_to_check_has_no_diagnosis() {
        let service = Arc::new(FakeDevice {
            connect: Err(dto::Error::AllowStorageAccess),
            ..FakeDevice::with_storages(vec![])
        });

        let failed = load_connection(service, None).await.unwrap_err();

        assert!(failed.diagnosis.is_none());
    }

    #[tokio::test]
    async fn a_storage_failure_is_diagnosed_too() {
        let service = Arc::new(FakeDevice {
            storages: Err(dto::Error::Timeout),
            diagnosis: dto::Diagnosis {
                holders: Some(vec![]),
                phones: None,
                ..Default::default()
            },
            ..FakeDevice::with_storages(vec![])
        });

        let failed = load_connection(service, None).await.unwrap_err();

        let checks = failed.diagnosis.unwrap().response.checks;
        assert_eq!(checks[0].text, "No other app is using your phone.");
    }

    #[test]
    fn the_diagnosis_is_kept_with_the_failure() {
        let mut handler = handler();
        let failure = Failure {
            choose: false,
            error: error::to_error_response(dto::Error::DeviceInUse),
            diagnosis: Some(Box::new(Diagnosed {
                response: DiagnosisResponse::default(),
                source: dto::Diagnosis::default(),
                holders: vec![holder("Preview", false)],
            })),
        };

        let _ = handler.update(Message::Connected(Err(failure)));

        assert_eq!(diagnosed_holders(&handler), ["Preview"]);
    }

    #[tokio::test]
    async fn quit_and_try_again_quits_the_holders_then_connects() {
        let service = Arc::new(FakeDevice {
            diagnosis: dto::Diagnosis {
                holders: Some(vec![holder("Photos", false)]),
                phones: None,
                ..Default::default()
            },
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        });
        let mut handler = Handler::new(Arc::clone(&service) as Arc<dyn Service>);
        let _ = handler.update(Message::Connected(Err(Failure {
            choose: false,
            error: error::to_error_response(dto::Error::DeviceInUse),
            diagnosis: Some(Box::new(Diagnosed {
                response: DiagnosisResponse::default(),
                source: dto::Diagnosis::default(),
                holders: vec![holder("Photos", false), holder("Preview", false)],
            })),
        })));

        let _ = handler.update(Message::Request(Request::QuitHolders));

        assert!(matches!(handler.state, State::Connecting));
        // The task the handler built does exactly this.
        service
            .release(&[holder("Photos", false), holder("Preview", false)])
            .await;
        assert_eq!(*service.released.lock().unwrap(), [["Photos", "Preview"]]);
        let connection = load_connection(service, None).await.unwrap();
        assert_eq!(connection.0.name, "Pixel 9");
    }

    #[test]
    fn quitting_needs_a_diagnosed_failure_with_holders() {
        let mut handler = handler();
        let _ = handler.update(Message::Request(Request::QuitHolders));
        assert!(handler.is_idle(), "nothing failed yet");

        let _ = handler.update(failed(dto::Error::Timeout));
        let _ = handler.update(Message::Request(Request::QuitHolders));
        assert!(matches!(handler.state, State::Failed(_)), "nothing to quit");

        let _ = handler.update(Message::Connected(Err(Failure {
            choose: false,
            error: error::to_error_response(dto::Error::DeviceInUse),
            diagnosis: Some(Box::new(Diagnosed {
                response: DiagnosisResponse::default(),
                source: dto::Diagnosis::default(),
                holders: vec![],
            })),
        })));
        let _ = handler.update(Message::Request(Request::QuitHolders));
        assert!(
            matches!(handler.state, State::Failed(_)),
            "empty holder list"
        );
    }

    fn failure_with(response: DiagnosisResponse, holders: Vec<dto::Holder>) -> Failure {
        Failure {
            choose: false,
            error: error::to_error_response(dto::Error::DeviceInUse),
            diagnosis: Some(Box::new(Diagnosed {
                response,
                source: dto::Diagnosis::default(),
                holders,
            })),
        }
    }

    #[test]
    fn the_failure_screen_lists_what_was_checked_and_offers_the_quit() {
        let response = diagnosis::to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![holder("Photos", false)]),
            phones: Some(vec!["Pixel 7".to_owned()]),
            ..Default::default()
        });
        let failed = failure_with(response, vec![holder("Photos", false)]);

        let mut ui = iced_test::simulator(failure(&failed));

        assert!(ui.find("Seen on USB: Pixel 7.").is_ok());
        assert!(ui.find("Photos is using your phone.").is_ok());
        assert!(ui.find("Try again").is_ok());
        let messages: Vec<Message> = {
            let _ = ui.click("Quit Photos and try again").unwrap();
            ui.into_messages().collect()
        };
        assert!(
            matches!(
                messages.as_slice(),
                [Message::Request(Request::QuitHolders)]
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn without_holders_the_only_button_is_try_again() {
        let response = diagnosis::to_diagnosis_response(&dto::Diagnosis {
            holders: None,
            phones: Some(vec![]),
            ..Default::default()
        });
        let failed = failure_with(response, vec![]);

        let mut ui = iced_test::simulator(failure(&failed));

        assert!(ui.find("No phone seen on USB. Try another cable (some only charge), another port, and no USB hub.").is_ok());
        assert!(ui.find("Try again").is_ok());
        assert!(ui.find("Quit them and try again").is_err());
        let _ = ui.click("Try again").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::Request(Request::Connect)]
        ));
    }

    #[test]
    fn the_failure_screen_draws_in_both_themes() {
        let response = diagnosis::to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![
                holder("macOS's camera service", true),
                holder("Photos", false),
            ]),
            phones: Some(vec!["Pixel 7".to_owned()]),
            ..Default::default()
        });
        let failed = failure_with(response, vec![holder("Photos", false)]);

        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let mut ui = iced_test::simulator(failure(&failed));
            assert!(ui.snapshot(&theme).is_ok());
        }
    }

    #[test]
    fn a_phone_that_went_quiet_is_reconnected_not_tried_again() {
        let mut handler = handler();
        handler.lose_device(error::to_error_response(dto::Error::Timeout));

        let mut ui = iced_test::simulator(handler.view_main());

        assert!(ui.find("Your phone stopped responding").is_ok());
        assert!(ui.find("Reconnect").is_ok());
        assert!(ui.find("Try again").is_err());
        let _ = ui.click("Reconnect").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Request(Request::Connect)]),
            "{messages:?}"
        );
    }

    #[test]
    fn other_failures_still_say_try_again() {
        let mut handler = handler();
        let _ = handler.update(failed(dto::Error::MultipleDevices));

        let mut ui = iced_test::simulator(handler.view_main());

        assert!(ui.find("Try again").is_ok());
        assert!(ui.find("Reconnect").is_err());
    }

    #[test]
    fn a_lost_phone_is_reconnected_too() {
        let mut handler = handler();
        handler.lose_device(error::to_error_response(dto::Error::Disconnected));

        let mut ui = iced_test::simulator(handler.view_main());

        assert!(ui.find("Reconnect").is_ok());
    }

    fn connected_as(serial: &str, storages: &[dto::Storage]) -> Handler {
        let mut handler = handler();
        let device = dto::Device {
            abilities: dto::Abilities::default(),
            serial_number: serial.into(),
            ..phone()
        };
        let _ = handler.update(Message::connected(&device, storages));
        handler
    }

    #[test]
    fn the_status_line_says_what_state_the_phone_is_in() {
        let mut handler = handler();
        assert_eq!(handler.status_line(), "not connected");

        let _ = handler.connect();
        assert_eq!(handler.status_line(), "connecting");

        let _ = handler.update(failed(dto::Error::MultipleDevices));
        assert_eq!(
            handler.status_line(),
            "not connected (last problem: Several phones are plugged in)"
        );
    }

    #[test]
    fn a_connected_phone_is_named_with_the_end_of_its_serial_and_its_storages() {
        let handler = connected_as("ABC123456789", &[storage(1, "Internal"), storage(2, "SD")]);

        assert_eq!(handler.status_line(), "Pixel 9, serial …6789, 2 storages");
    }

    #[test]
    fn the_status_line_never_holds_more_of_the_serial_than_four_characters() {
        let handler = connected_as("SECRET-SERIAL-NUMBER-0042", &[storage(1, "Internal")]);

        let line = handler.status_line();

        assert!(line.contains("…0042"), "{line}");
        assert!(!line.contains("SECRET"), "{line}");
        assert!(line.ends_with("1 storage"), "{line}");
    }

    #[test]
    fn short_odd_and_missing_serials_are_handled() {
        assert_eq!(
            connected_as("AB", &[storage(1, "Internal")]).status_line(),
            "Pixel 9, serial …AB, 1 storage"
        );
        assert_eq!(
            connected_as("", &[]).status_line(),
            "Pixel 9, 0 storages",
            "no serial, no gap"
        );
        // The last four characters, not bytes: cutting a Thai serial by bytes
        // would split a character (and panic).
        assert_eq!(
            connected_as("เลขที่หนึ่งสองสาม", &[storage(1, "Internal")]).status_line(),
            "Pixel 9, serial …งสาม, 1 storage"
        );
    }

    #[test]
    fn only_a_connected_phone_counts_as_connected() {
        let mut handler = handler();
        assert!(!handler.is_connected());

        let _ = handler.connect();
        assert!(!handler.is_connected());

        let _ = handler.update(failed(dto::Error::Timeout));
        assert!(!handler.is_connected());

        let _ = handler.update(connected(&[storage(1, "Internal")]));
        assert!(handler.is_connected());

        let _ = handler.update(Message::Disconnected);
        assert!(!handler.is_connected());
    }

    /// A storage with `free` bytes left of 64 GB.
    fn storage_with_free(id: u64, free: u64) -> dto::Storage {
        dto::Storage {
            free_bytes: free,
            ..storage(id, "Internal")
        }
    }

    async fn run_to_the_end(handler: &mut Handler, task: Task<Message>) {
        let (messages, _) = crate::handler::testing::drain(task).await;
        for message in messages {
            let _ = handler.update(message);
        }
    }

    #[tokio::test]
    async fn reloading_the_storages_updates_how_full_the_sidebar_says_they_are() {
        // The phone now has 5 GB free; the sidebar was shown 16 GB.
        let service = Arc::new(FakeDevice::with_storages(vec![storage_with_free(
            1,
            5_000_000_000,
        )]));
        let mut handler = Handler::new(service);
        let _ = handler.update(Message::connected(
            &phone(),
            &[storage_with_free(1, 16_000_000_000)],
        ));
        {
            let mut ui = iced_test::simulator(handler.view_sidebar());
            assert!(ui.find("16.0 GB free of 64.0 GB".to_owned()).is_ok());
        }

        let task = handler.reload_storages();
        run_to_the_end(&mut handler, task).await;

        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("5.0 GB free of 64.0 GB".to_owned()).is_ok());
        assert!(ui.find("16.0 GB free of 64.0 GB".to_owned()).is_err());
    }

    #[tokio::test]
    async fn the_storage_picked_in_the_sidebar_survives_a_reload() {
        let service = Arc::new(FakeDevice::with_storages(vec![
            storage_with_free(1, 5_000_000_000),
            storage_with_free(2, 9_000_000_000),
        ]));
        let mut handler = Handler::new(service);
        let _ = handler.update(Message::connected(
            &phone(),
            &[
                storage_with_free(1, 16_000_000_000),
                storage_with_free(2, 20_000_000_000),
            ],
        ));
        let _ = handler.update(Message::Request(Request::SelectStorage(2)));

        let task = handler.reload_storages();
        run_to_the_end(&mut handler, task).await;

        assert_eq!(handler.selected_storage().map(|(id, _)| id), Some(2));
    }

    #[tokio::test]
    async fn there_is_nothing_to_reload_without_a_connected_phone() {
        let mut handler = handler();
        for before in [0, 1, 2] {
            match before {
                1 => {
                    let _ = handler.connect();
                }
                2 => {
                    let _ = handler.update(failed(dto::Error::Timeout));
                }
                _ => {}
            }

            let (messages, _) = crate::handler::testing::drain(handler.reload_storages()).await;

            assert!(messages.is_empty(), "state {before}");
        }
    }

    #[tokio::test]
    async fn a_reload_that_finds_the_phone_gone_goes_back_to_idle() {
        let service = Arc::new(FakeDevice {
            storages: Err(dto::Error::Disconnected),
            ..FakeDevice::with_storages(vec![])
        });
        let mut handler = Handler::new(service);
        let _ = handler.update(connected(&[storage(1, "Internal")]));

        let task = handler.reload_storages();
        run_to_the_end(&mut handler, task).await;

        assert!(handler.is_idle());
    }

    /// Pixel 9 ("ABC123", what a plain connect opens) and Galaxy S24
    /// ("S24") plugged in at once.
    fn two_plugged() -> Arc<FakeDevice> {
        Arc::new(FakeDevice {
            phones: vec![
                crate::handler::testing::plugged("ABC123", "Pixel 9"),
                crate::handler::testing::plugged("S24", "Galaxy S24"),
            ],
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        })
    }

    /// Runs what a message started and feeds back what it said.
    async fn settle(handler: &mut Handler, message: Message) {
        let mut pending = vec![handler.update(message)];
        while let Some(task) = pending.pop() {
            for message in crate::handler::testing::drain(task).await.0 {
                pending.push(handler.update(message));
            }
        }
    }

    fn open_request(key: &str) -> Message {
        Message::Request(Request::Open(key.into()))
    }

    #[tokio::test]
    async fn several_phones_and_none_picked_asks_which_and_opens_the_one_clicked() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        let service_failure = Failure {
            choose: true,
            ..Failure::from(error::to_error_response(dto::Error::MultipleDevices))
        };
        settle(&mut handler, Message::Connected(Err(service_failure))).await;

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view_main());
            assert!(ui.find("2 phones are plugged in").is_ok());
            let galaxy = ui.find("Galaxy S24").unwrap().bounds().center();
            ui.point_at(galaxy);
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };
        for message in clicked {
            settle(&mut handler, message).await;
        }

        assert_eq!(handler.open_key().as_deref(), Some("S24"));
        assert_eq!(
            service.asked.lock().unwrap().last(),
            Some(&Some("S24".to_owned()))
        );
        assert_eq!(handler.open_name(), Some("Galaxy S24"));
    }

    #[tokio::test]
    async fn a_plain_connect_with_several_plugged_in_comes_back_as_a_choice() {
        let service = Arc::new(FakeDevice {
            connect: Err(dto::Error::MultipleDevices),
            ..FakeDevice::with_storages(vec![])
        });
        let failed = load_connection(service, None).await.unwrap_err();

        assert!(failed.choose);
        let other = load_connection(
            Arc::new(FakeDevice {
                connect: Err(dto::Error::NoDevice),
                ..FakeDevice::with_storages(vec![])
            }),
            None,
        )
        .await
        .unwrap_err();
        assert!(!other.choose);
    }

    #[tokio::test]
    async fn switching_closes_the_storages_shown_and_opens_the_other_phone() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        assert_eq!(handler.phones.len(), 2, "listed once connected");

        let task = handler.update(open_request("S24"));
        assert_eq!(
            handler.selected_storage(),
            None,
            "nothing to browse meanwhile"
        );
        {
            let mut ui = iced_test::simulator(handler.view_main());
            assert!(ui.find("Opening Galaxy S24…").is_ok());
            let _ = handler.view_sidebar();
        }
        for message in crate::handler::testing::drain(task).await.0 {
            settle(&mut handler, message).await;
        }

        assert_eq!(handler.opened(), Some(("S24".into(), 1, "Internal".into())));
        assert_eq!(handler.preferred.as_deref(), Some("S24"));
    }

    #[tokio::test]
    async fn a_switch_to_a_phone_that_has_gone_keeps_the_open_one_and_says_so() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        let _ = handler.update(open_request("S24"));

        settle(&mut handler, Message::Switched(Switch::Gone)).await;

        assert_eq!(
            handler.opened(),
            Some(("ABC123".into(), 1, "Internal".into()))
        );
        assert_eq!(handler.preferred.as_deref(), Some("ABC123"));
        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Galaxy S24 is no longer plugged in.").is_ok());
    }

    #[tokio::test]
    async fn a_switch_whose_phone_will_not_open_says_why() {
        let mut handler = Handler::new(two_plugged());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        let _ = handler.update(open_request("S24"));

        let _ = handler.update(Message::Switched(Switch::Failed(
            error::to_error_response(dto::Error::DeviceInUse).into(),
        )));

        assert!(
            matches!(&handler.state, State::Failed(f) if f.error.title == "Another app is using your phone")
        );
        assert_eq!(
            handler.preferred.as_deref(),
            Some("S24"),
            "Try again tries it again"
        );
    }

    #[tokio::test]
    async fn the_open_phone_or_a_switch_under_way_is_not_opened_again() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        let asked = service.asked.lock().unwrap().len();

        let _ = handler.update(open_request("ABC123"));
        assert!(handler.is_connected());
        let _ = handler.update(open_request("S24"));
        let _ = handler.update(open_request("ABC123"));

        assert!(
            matches!(&handler.state, State::Switching { to, .. } if to.as_deref() == Some("Galaxy S24"))
        );
        assert_eq!(service.asked.lock().unwrap().len(), asked, "no connect yet");
    }

    #[tokio::test]
    async fn a_reconnect_opens_the_phone_last_used() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, open_request("S24")).await;
        assert_eq!(handler.open_key().as_deref(), Some("S24"));
        handler.lose_device(error::to_error_response(dto::Error::Disconnected));

        settle(&mut handler, Message::Request(Request::Connect)).await;

        assert_eq!(
            service.asked.lock().unwrap().last(),
            Some(&Some("S24".to_owned()))
        );
        assert_eq!(handler.open_key().as_deref(), Some("S24"));
    }

    #[tokio::test]
    async fn plugging_in_another_phone_lists_it_without_leaving_the_open_one() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        handler.phones.clear();
        let asked = service.asked.lock().unwrap().len();

        settle(
            &mut handler,
            Message::Hotplug(dto::DeviceChange::Plugged {
                serial: Some("S24".into()),
            }),
        )
        .await;

        assert_eq!(handler.phones.len(), 2);
        assert_eq!(service.asked.lock().unwrap().len(), asked);
        {
            let mut ui = iced_test::simulator(handler.view_sidebar());
            assert!(ui.find("Phones").is_ok());
            assert!(ui.find("open").is_ok(), "the open phone is marked");
        }

        settle(
            &mut handler,
            Message::Hotplug(dto::DeviceChange::Unplugged {
                serial: Some("S24".into()),
            }),
        )
        .await;
        assert!(handler.is_connected(), "another phone left, not this one");
    }

    #[tokio::test]
    async fn a_reconnect_opens_the_phone_that_was_open_even_if_it_was_never_picked() {
        let service = two_plugged();
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        handler.lose_device(error::to_error_response(dto::Error::Disconnected));

        settle(&mut handler, Message::Request(Request::Connect)).await;

        assert_eq!(
            service.asked.lock().unwrap().last(),
            Some(&Some("ABC123".to_owned()))
        );
    }

    #[tokio::test]
    async fn a_switch_tells_a_phone_that_has_gone_from_one_that_will_not_open() {
        let gone = switch(two_plugged(), "unplugged".into()).await;
        let opened = switch(two_plugged(), "S24".into()).await;
        let failing = Arc::new(FakeDevice {
            connect: Err(dto::Error::DeviceInUse),
            ..FakeDevice::with_storages(vec![])
        });
        let failed = switch(failing, "S24".into()).await;

        assert!(matches!(gone, Switch::Gone));
        assert!(matches!(opened, Switch::Opened((device, _)) if device.key == "S24"));
        assert!(matches!(failed, Switch::Failed(f) if !f.choose));
    }

    #[test]
    fn with_only_the_open_phone_plugged_in_no_list_is_shown() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Phones(vec![phone_named("ABC123", "Pixel 9")]));

        let mut ui = iced_test::simulator(handler.view_sidebar());

        assert!(ui.find("Phones").is_err());
    }

    #[test]
    fn a_refused_switch_says_why_in_the_sidebar_until_the_next_try() {
        let mut handler = handler();
        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Phones(vec![
            phone_named("ABC123", "Pixel 9"),
            phone_named("S24", "Galaxy S24"),
        ]));

        handler.refuse_switch("Wait for the transfers.".into());
        {
            let mut ui = iced_test::simulator(handler.view_sidebar());
            assert!(ui.find("Wait for the transfers.").is_ok());
        }

        let _ = handler.update(open_request("S24"));
        assert_eq!(handler.notice, None);
        assert_eq!(handler.status_line(), "connecting");
    }

    /// A phone whose battery answers `levels` in turn.
    fn charging(levels: Vec<Result<Option<u8>, dto::Error>>) -> Arc<FakeDevice> {
        let device = FakeDevice::with_storages(vec![storage(1, "Internal")]);
        *device.battery.lock().unwrap() = levels;
        Arc::new(device)
    }

    fn asks(service: &FakeDevice) -> usize {
        service
            .battery_asks
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn the_open_phone_says_whether_it_is_android() {
        let mut handler = Handler::new(Arc::new(FakeDevice::with_storages(vec![])));
        assert!(!handler.is_android(), "no phone open");
        let _ = handler.update(Message::connected(
            &crate::handler::testing::phone(),
            &[storage(1, "Internal")],
        ));
        assert!(handler.is_android());
        let _ = handler.update(Message::connected(
            &dto::Device {
                android: false,
                ..crate::handler::testing::phone()
            },
            &[storage(1, "Internal")],
        ));
        assert!(!handler.is_android());
    }

    #[tokio::test]
    async fn connecting_reads_the_battery_and_the_sidebar_shows_it_with_the_android_version() {
        let service = charging(vec![Ok(Some(76))]);
        let mut handler = Handler::new(service.clone());

        settle(&mut handler, Message::Request(Request::Connect)).await;

        assert_eq!(asks(&service), 1);
        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Android 15 · Battery 76%").is_ok());
    }

    #[tokio::test]
    async fn the_sidebar_and_the_report_line_say_how_fast_the_usb_link_is() {
        let mut device = FakeDevice::with_storages(vec![storage(1, "Internal")]);
        device.connect = Ok(dto::Device {
            link: Some(dto::UsbSpeed::High),
            ..phone()
        });
        *device.battery.lock().unwrap() = vec![Ok(Some(76))];
        let mut handler = Handler::new(Arc::new(device));
        assert_eq!(handler.link(), None, "no phone yet");

        settle(&mut handler, Message::Request(Request::Connect)).await;

        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Android 15 · Battery 76% · USB 2").is_ok());
        assert_eq!(handler.link(), Some(dto::UsbSpeed::High));
        assert!(
            handler.status_line().ends_with("1 storage, USB 2"),
            "{}",
            handler.status_line()
        );

        let mut unknown = Handler::new(charging(vec![Ok(Some(76))]));
        settle(&mut unknown, Message::Request(Request::Connect)).await;
        assert_eq!(unknown.link(), None);
        assert!(
            unknown.status_line().ends_with("1 storage"),
            "not known: not said"
        );
    }

    #[tokio::test]
    async fn the_level_follows_the_phone_when_it_says_its_battery_changed() {
        let service = charging(vec![Ok(Some(76)), Ok(Some(77))]);
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, Message::Request(Request::Connect)).await;

        settle(&mut handler, Message::Changed(dto::ContentChange::Battery)).await;

        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Android 15 · Battery 77%").is_ok());
    }

    #[tokio::test]
    async fn the_level_is_asked_again_with_the_storages_after_a_transfer() {
        let service = charging(vec![Ok(Some(76)), Ok(Some(70))]);
        let mut handler = Handler::new(service.clone());
        settle(&mut handler, Message::Request(Request::Connect)).await;

        let task = handler.reload_storages();
        for message in crate::handler::testing::drain(task).await.0 {
            settle(&mut handler, message).await;
        }

        assert_eq!(asks(&service), 2);
        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Android 15 · Battery 70%").is_ok());
    }

    #[tokio::test]
    async fn a_phone_that_does_not_say_shows_only_its_version_and_one_that_says_nothing_shows_no_line()
     {
        let mut handler = Handler::new(charging(vec![Ok(None)]));
        settle(&mut handler, Message::Request(Request::Connect)).await;
        {
            let mut ui = iced_test::simulator(handler.view_sidebar());
            assert!(ui.find("Android 15").is_ok());
            assert!(ui.find("Android 15 · ").is_err());
        }

        let mut quiet = handler_with_device(dto::Device {
            abilities: dto::Abilities::default(),
            firmware_version: String::new(),
            ..phone()
        });
        let _ = quiet.update(Message::connected(
            &dto::Device {
                abilities: dto::Abilities::default(),
                firmware_version: String::new(),
                ..phone()
            },
            &[storage(1, "Internal")],
        ));
        let mut ui = iced_test::simulator(quiet.view_sidebar());
        assert!(ui.find("Android 15").is_err(), "nothing to say, no line");
        assert!(ui.find("Pixel 9").is_ok());
    }

    fn handler_with_device(device: dto::Device) -> Handler {
        Handler::new(Arc::new(FakeDevice {
            connect: Ok(device),
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        }))
    }

    #[tokio::test]
    async fn the_open_phone_says_what_it_can_do() {
        let abilities = dto::Abilities {
            copy: true,
            prop_list: true,
            ..dto::Abilities::default()
        };
        let mut handler = handler_with_device(dto::Device {
            abilities,
            ..crate::handler::testing::phone()
        });
        assert_eq!(handler.abilities(), None, "no phone yet");

        settle(&mut handler, Message::Request(Request::Connect)).await;

        assert_eq!(handler.abilities(), Some(abilities));
    }

    #[tokio::test]
    async fn losing_the_phone_while_asking_its_battery_is_losing_it() {
        let lost = charging(vec![Err(dto::Error::Disconnected)]);
        let failed = load_connection(lost, None).await.unwrap_err();
        assert!(failed.error.device_lost, "the connect did not work");

        let service = charging(vec![Ok(Some(50)), Err(dto::Error::Timeout)]);
        let mut handler = Handler::new(service);
        settle(&mut handler, Message::Request(Request::Connect)).await;
        settle(&mut handler, Message::Changed(dto::ContentChange::Battery)).await;
        assert!(handler.is_idle());
    }

    #[test]
    fn a_battery_question_that_fails_otherwise_keeps_the_phone_and_none_is_asked_without_one() {
        let service = charging(vec![Ok(None)]);
        let mut handler = Handler::new(service.clone());
        let _ = handler.update(Message::Changed(dto::ContentChange::Battery));
        assert_eq!(handler.reload_battery().units(), 0, "no phone, no question");

        let _ = handler.update(connected(&[storage(1, "Internal")]));
        let _ = handler.update(Message::Battery(Err(error::to_error_response(
            dto::Error::General("busy".into()),
        ))));
        assert!(handler.is_connected());
        let _ = handler.update(Message::Battery(Ok(Some(9))));
        let mut ui = iced_test::simulator(handler.view_sidebar());
        assert!(ui.find("Android 15 · Battery 9%").is_ok());
    }

    #[tokio::test]
    async fn a_late_answer_from_the_phone_being_closed_does_not_undo_the_switch() {
        let service = Arc::new(FakeDevice {
            phones: vec![
                crate::handler::testing::plugged("ABC123", "Pixel 9"),
                crate::handler::testing::plugged("S24", "Galaxy S24"),
            ],
            ..FakeDevice::with_storages(vec![storage(1, "Internal")])
        });
        let mut handler = Handler::new(service);
        settle(&mut handler, connected(&[storage(1, "Internal")])).await;
        let _ = handler.update(open_request("S24"));
        let lost = || error::to_error_response(dto::Error::Disconnected);

        let _ = handler.update(Message::Battery(Err(lost())));
        let _ = handler.update(Message::StoragesReloaded(Err(lost())));
        let _ = handler.update(Message::Switched(Switch::Gone));

        assert_eq!(
            handler.opened(),
            Some(("ABC123".into(), 1, "Internal".into())),
            "the first phone, still open, is shown again"
        );
    }
}
