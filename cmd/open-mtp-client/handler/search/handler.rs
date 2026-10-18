//! Searching the whole phone: a panel over the phone pane.
//!
//! Opening it walks the storage once (`Service::walk`), and what the walk sees
//! arrives as a catalogue, piece by piece. Typing, or choosing a kind, only
//! looks through that catalogue: no keystroke asks the phone anything. Results
//! are rows of a list built only near the screen, as in the panes, so a search
//! that finds 50,000 photos costs no more to draw than one that finds three.
//! Results are selected by their full path (two folders can hold the same
//! name); the app downloads them, one transfer for each folder they are in.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::keyboard::Modifiers;
use iced::widget::scrollable::{AbsoluteOffset, Viewport};
use iced::widget::text::Wrapping;
use iced::widget::{
    Id, button, column, container, mouse_area, operation, pick_list, row, rule, scrollable, space,
    text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use internal::dto::{self, FileKind};

use super::mapper::{self, Progress};
use super::request::Request;
use super::response::ResultRow;
use crate::handler::error::{self, ErrorResponse};
use crate::handler::explorer::selection::{Click, Selection};
use crate::handler::explorer::window;

/// Rows have a fixed height, so only the ones near the screen are built.
pub(super) const ROW_HEIGHT: f32 = 32.0;
const NAME_COLUMN: u16 = 5;
const PLACE_COLUMN: u16 = 4;
const SIZE_COLUMN: u16 = 2;
const MODIFIED_COLUMN: u16 = 2;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Starts a walk of the whole storage; `cancel` stops it between folders.
    async fn walk(
        &self,
        storage_id: u64,
        include_hidden: bool,
        cancel: dto::Cancel,
    ) -> Result<Box<dyn Walking>, dto::Error>;
}

/// A walk being read piece by piece.
#[async_trait::async_trait]
pub trait Walking: Send {
    async fn next(&mut self) -> Option<Result<dto::WalkUpdate, dto::Error>>;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    /// A piece of the walk with this number; pieces of an older walk are
    /// dropped.
    Walked {
        walk: u64,
        result: Result<dto::WalkUpdate, ErrorResponse>,
    },
    Scrolled(Viewport),
}

impl Message {
    /// The error this message carries if it means the phone is gone.
    pub fn device_lost(&self) -> Option<&ErrorResponse> {
        match self {
            Message::Walked {
                result: Err(error), ..
            } if error.device_lost => Some(error),
            _ => None,
        }
    }
}

/// The storage being searched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    storage_id: u64,
    name: String,
    include_hidden: bool,
}

/// A date in the search's menu, as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateChoice(Option<dto::DateRange>);

impl std::fmt::Display for DateChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::handler::format::date_label(self.0))
    }
}

pub struct Handler {
    service: Arc<dyn Service>,
    scope: Option<Scope>,
    query: String,
    kind: Option<FileKind>,
    date: Option<dto::DateRange>,
    /// Today, for the date: the Mac's clock, a fixed day in tests.
    today: fn() -> chrono::NaiveDate,
    /// Everything the walk has seen.
    catalogue: Vec<dto::Found>,
    /// Each catalogue name in lower case, made once, for ordering by name.
    lower: Vec<String>,
    /// The column the results are ordered by, and whether downwards; `None`
    /// for the order the walk found them in.
    order: Option<(dto::SortKey, bool)>,
    /// What matches the query: places in the catalogue, and their keys.
    matches: Vec<usize>,
    keys: Vec<String>,
    selection: Selection,
    /// The selected keys, to tell a row is selected without searching.
    chosen: HashSet<String>,
    walk: u64,
    cancel: Option<dto::Cancel>,
    progress: Progress,
    error: Option<ErrorResponse>,
    /// Scrolled offset and visible height of the results.
    viewport: (f32, f32),
    list: Id,
    input: Id,
    modifiers: Modifiers,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            scope: None,
            query: String::new(),
            kind: None,
            date: None,
            today: crate::handler::format::local_today,
            catalogue: Vec::new(),
            lower: Vec::new(),
            order: None,
            matches: Vec::new(),
            keys: Vec::new(),
            selection: Selection::default(),
            chosen: HashSet::new(),
            walk: 0,
            cancel: None,
            progress: Progress::default(),
            error: None,
            viewport: (0.0, 0.0),
            list: Id::unique(),
            input: Id::unique(),
            modifiers: Modifiers::default(),
        }
    }

    /// The kind the results keep.
    #[cfg(test)]
    pub fn kind(&self) -> Option<FileKind> {
        self.kind
    }

    pub fn is_open(&self) -> bool {
        self.scope.is_some()
    }

    /// The storage being searched.
    pub fn storage_id(&self) -> Option<u64> {
        self.scope.as_ref().map(|scope| scope.storage_id)
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    /// Opens the search of storage `storage_id` (called `name`), with the
    /// cursor in the search field. The phone is looked through unless the
    /// search is already open on it.
    pub fn open(&mut self, storage_id: u64, name: &str, include_hidden: bool) -> Task<Message> {
        let scope = Scope {
            storage_id,
            name: name.to_owned(),
            include_hidden,
        };
        let focus = operation::focus(self.input.clone());
        if self.scope.as_ref() == Some(&scope) && self.error.is_none() {
            return focus;
        }
        self.scope = Some(scope);
        Task::batch([self.start_walk(), focus])
    }

    /// Closes the panel: the walk stops, and what it saw is let go.
    pub fn close(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.walk += 1;
        self.scope = None;
        self.catalogue = Vec::new();
        self.lower = Vec::new();
        self.matches = Vec::new();
        self.keys = Vec::new();
        self.selection.clear();
        self.chosen.clear();
        self.error = None;
        self.progress = Progress::default();
    }

    /// The selected results, by the folder they are in: one download each.
    pub fn chosen_by_folder(&self) -> Vec<(String, Vec<String>)> {
        let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, &at) in self.keys.iter().zip(&self.matches) {
            if self.chosen.contains(key) {
                let found = &self.catalogue[at];
                folders
                    .entry(found.folder.clone())
                    .or_default()
                    .push(found.entry.name.clone());
            }
        }
        folders.into_iter().collect()
    }

    /// The result the cursor is on: the folder it is in, and its name.
    pub fn cursor_item(&self) -> Option<(String, String)> {
        let at = self.selection.cursor_index(&self.keys)?;
        let found = &self.catalogue[self.matches[at]];
        Some((found.folder.clone(), found.entry.name.clone()))
    }

    fn start_walk(&mut self) -> Task<Message> {
        let Some(scope) = &self.scope else {
            return Task::none();
        };
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.walk += 1;
        let cancel = dto::Cancel::new();
        self.cancel = Some(cancel.clone());
        self.catalogue.clear();
        self.lower.clear();
        self.matches.clear();
        self.keys.clear();
        self.selection.clear();
        self.chosen.clear();
        self.error = None;
        self.progress = Progress::default();
        let walk = self.walk;
        Task::run(
            walk_pieces(
                Arc::clone(&self.service),
                scope.storage_id,
                scope.include_hidden,
                cancel,
            ),
            move |result| Message::Walked { walk, result },
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(request) => self.handle(request),
            Message::Scrolled(viewport) => {
                self.viewport = (viewport.absolute_offset().y, viewport.bounds().height);
                Task::none()
            }
            Message::Walked { walk, result } => {
                if walk != self.walk {
                    return Task::none();
                }
                match result {
                    Ok(update) => self.take_in(update),
                    Err(error) => {
                        self.cancel = None;
                        self.progress.done = true;
                        self.error = Some(error);
                    }
                }
                Task::none()
            }
        }
    }

    /// A piece of the walk: what is new goes into the catalogue, and what of it
    /// matches goes onto the end of the results.
    fn take_in(&mut self, update: dto::WalkUpdate) {
        if update.restart {
            self.catalogue.clear();
            self.lower.clear();
            self.matches.clear();
            self.keys.clear();
            self.selection.clear();
            self.chosen.clear();
        }
        let matcher = self.query().matcher();
        let first = self.catalogue.len();
        self.lower.extend(
            update
                .found
                .iter()
                .map(|found| found.entry.name.to_lowercase()),
        );
        self.catalogue.extend(update.found);
        let mut new: Vec<usize> = (first..self.catalogue.len())
            .filter(|&at| self.wanted(&matcher, at))
            .collect();
        if self.order.is_some() {
            // Ordered: the new results are ordered among themselves and
            // merged in, one pass, rather than all of them ordered again.
            new.sort_by(|&a, &b| self.compare(a, b));
            let old_matches = std::mem::take(&mut self.matches);
            let mut old_keys = std::mem::take(&mut self.keys).into_iter();
            let mut old = old_matches.into_iter().peekable();
            let mut fresh = new.into_iter().peekable();
            while let (Some(&a), Some(&b)) = (old.peek(), fresh.peek()) {
                if self.compare(b, a) == Ordering::Less {
                    self.push_match(b, None);
                    fresh.next();
                } else {
                    self.push_match(a, old_keys.next());
                    old.next();
                }
            }
            for a in old {
                self.push_match(a, old_keys.next());
            }
            for b in fresh {
                self.push_match(b, None);
            }
        } else {
            for at in new {
                self.push_match(at, None);
            }
        }
        self.progress = Progress {
            folders_read: update.folders_read,
            items: update.items,
            done: update.done,
            truncated: update.truncated,
        };
        if update.done {
            self.cancel = None;
        }
    }

    /// Adds a result, with its key when it is already made.
    fn push_match(&mut self, at: usize, key: Option<String>) {
        self.matches.push(at);
        self.keys
            .push(key.unwrap_or_else(|| mapper::key_of(&self.catalogue[at])));
    }

    /// Whether the catalogue item `at` is a result. With nothing typed and no
    /// kind, ordering the results lists every file: what fills the phone.
    fn wanted(&self, matcher: &dto::Matcher, at: usize) -> bool {
        let entry = &self.catalogue[at].entry;
        if self.query().is_empty() {
            self.order.is_some() && !entry.is_folder
        } else {
            matcher.matches(entry)
        }
    }

    /// How two results are ordered by the chosen column; ties by name, then
    /// by folder, so the order is stable.
    fn compare(&self, a: usize, b: usize) -> Ordering {
        let Some((key, descending)) = self.order else {
            return a.cmp(&b);
        };
        let (x, y) = (&self.catalogue[a], &self.catalogue[b]);
        let by_key = match key {
            dto::SortKey::Size => x.entry.size_bytes.cmp(&y.entry.size_bytes),
            dto::SortKey::Modified => x.entry.modified.cmp(&y.entry.modified),
            dto::SortKey::Name | dto::SortKey::Kind => Ordering::Equal,
        };
        let by_key = if descending { by_key.reverse() } else { by_key };
        let by_name = self.lower[a].cmp(&self.lower[b]);
        let by_name = if descending && matches!(key, dto::SortKey::Name) {
            by_name.reverse()
        } else {
            by_name
        };
        by_key.then(by_name).then_with(|| x.folder.cmp(&y.folder))
    }

    fn query(&self) -> dto::SearchQuery {
        let today = (self.today)();
        dto::SearchQuery {
            text: self.query.clone(),
            kind: self.kind,
            date: self
                .date
                .map(|range| crate::handler::format::date_span(range, today)),
        }
    }

    /// Looks through the catalogue again for what is asked now.
    fn refilter(&mut self) -> Task<Message> {
        let matcher = self.query().matcher();
        let mut matches: Vec<usize> = (0..self.catalogue.len())
            .filter(|&at| self.wanted(&matcher, at))
            .collect();
        if self.order.is_some() {
            matches.sort_by(|&a, &b| self.compare(a, b));
        }
        self.matches = matches;
        self.keys = self
            .matches
            .iter()
            .map(|&at| mapper::key_of(&self.catalogue[at]))
            .collect();
        self.selection.clear();
        self.chosen.clear();
        self.viewport.0 = 0.0;
        operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y: 0.0 })
    }

    fn handle(&mut self, request: Request) -> Task<Message> {
        match request {
            Request::Query(query) => {
                self.query = query;
                self.refilter()
            }
            Request::Kind(kind) => {
                self.kind = kind;
                self.refilter()
            }
            Request::Date(date) => {
                self.date = date;
                self.refilter()
            }
            Request::SortBy(key) => {
                self.order = Some(mapper::next_order(self.order, key));
                self.refilter()
            }
            Request::Select(key) => {
                let click = if self.modifiers.command() {
                    Click::Toggle
                } else if self.modifiers.shift() {
                    Click::Range
                } else {
                    Click::Only
                };
                self.selection.click(&self.keys, &key, click);
                self.chosen = self.selection.names().iter().cloned().collect();
                Task::none()
            }
            Request::SelectAll => {
                self.selection.all(&self.keys);
                self.chosen = self.keys.iter().cloned().collect();
                Task::none()
            }
            Request::Refresh => self.start_walk(),
            Request::Close => {
                self.close();
                Task::none()
            }
            // The app carries these out: they need the other pane.
            Request::Download | Request::ShowInFolder => Task::none(),
        }
    }

    /// How many results are selected.
    pub fn selected(&self) -> usize {
        self.chosen.len()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(scope) = &self.scope else {
            return space().into();
        };
        let field = text_input("Search the phone by name", &self.query)
            .id(self.input.clone())
            .on_input(|query| Message::Request(Request::Query(query)))
            .padding([6, 10])
            .size(15);
        let top = row![
            field,
            button(text("Look again").size(13))
                .style(button::text)
                .on_press(Message::Request(Request::Refresh)),
            button(text("Close").size(13))
                .style(button::text)
                .on_press(Message::Request(Request::Close)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut kinds = row![].spacing(6);
        for kind in std::iter::once(None).chain(FileKind::ALL.into_iter().map(Some)) {
            let chosen = self.kind == kind;
            kinds = kinds.push(
                button(text(mapper::kind_label(kind)).size(13))
                    .padding([3, 10])
                    .style(if chosen {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::Request(Request::Kind(kind))),
            );
        }
        let dates: Vec<DateChoice> = std::iter::once(None)
            .chain(dto::DateRange::ALL.into_iter().map(Some))
            .map(DateChoice)
            .collect();
        let kinds = kinds.push(space::horizontal()).push(
            pick_list(dates, Some(DateChoice(self.date)), |choice| {
                Message::Request(Request::Date(choice.0))
            })
            .text_size(13)
            .padding([3, 8]),
        );

        let status = match &self.error {
            Some(error) => text(format!("{} {}", error.title, error.hint))
                .size(12)
                .style(text::danger),
            None => text(mapper::status(
                self.progress,
                !self.query().is_empty() || self.order.is_some(),
                self.matches.len(),
                &scope.name,
            ))
            .size(12)
            .style(text::secondary),
        };

        let sortable = |title, key, portion| {
            button(text(mapper::order_label(title, key, self.order)).size(13))
                .style(button::text)
                .padding(0)
                .width(Length::FillPortion(portion))
                .on_press(Message::Request(Request::SortBy(key)))
        };
        let header = row![
            sortable("Name", dto::SortKey::Name, NAME_COLUMN),
            heading("Where", PLACE_COLUMN),
            sortable("Size", dto::SortKey::Size, SIZE_COLUMN),
            sortable("Modified", dto::SortKey::Modified, MODIFIED_COLUMN),
        ]
        .spacing(8)
        .padding([0, 10]);

        let selected = self.selected();
        let footer = row![
            text(match selected {
                0 => String::new(),
                1 => "1 selected".to_owned(),
                n => format!("{} selected", crate::handler::format::format_count(n)),
            })
            .size(12)
            .style(text::secondary)
            .width(Length::Fill),
            button(text("Show in folder").size(13))
                .style(button::secondary)
                .on_press_maybe((selected == 1).then_some(Message::Request(Request::ShowInFolder))),
            button(text(mapper::download_label(selected)).size(13))
                .style(button::primary)
                .on_press_maybe((selected > 0).then_some(Message::Request(Request::Download))),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        container(
            column![
                top,
                kinds,
                status,
                rule::horizontal(1),
                header,
                self.results(),
                rule::horizontal(1),
                footer
            ]
            .spacing(8),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// The results: one fixed-height row each, built only near the screen;
    /// spacers of the same height stand in for the rest.
    fn results(&self) -> Element<'_, Message> {
        let range = window::rows(
            self.viewport.0,
            self.viewport.1,
            ROW_HEIGHT,
            self.matches.len(),
        );
        let above = range.start as f32 * ROW_HEIGHT;
        let below = (self.matches.len() - range.end) as f32 * ROW_HEIGHT;
        let today = chrono::Local::now().date_naive();
        let mut rows = column![space().height(above)];
        for &at in &self.matches[range] {
            let row = mapper::to_row(&self.catalogue[at], today);
            let selected = self.chosen.contains(&row.key);
            rows = rows.push(result_row(row, selected));
        }
        rows = rows.push(space().height(below));
        scrollable(rows.width(Length::Fill))
            .id(self.list.clone())
            .on_scroll(Message::Scrolled)
            .height(Length::Fill)
            .into()
    }
}

fn heading(label: &str, portion: u16) -> Element<'_, Message> {
    text(label)
        .size(13)
        .style(text::secondary)
        .width(Length::FillPortion(portion))
        .into()
}

fn result_row(row: ResultRow, selected: bool) -> Element<'static, Message> {
    // Each cell is cut at its own column: a long name must not run on under
    // the next one.
    let cell = |content: String, size: u32, portion: u16| {
        container(text(content).size(size).wrapping(Wrapping::None))
            .width(Length::FillPortion(portion))
            .clip(true)
    };
    let cells = row![
        cell(row.name, 15, NAME_COLUMN),
        cell(row.place, 13, PLACE_COLUMN),
        cell(row.size, 13, SIZE_COLUMN),
        cell(row.modified, 13, MODIFIED_COLUMN),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let key = row.key;
    mouse_area(
        container(cells)
            .padding([0, 10])
            .height(ROW_HEIGHT)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .style(move |theme: &iced::Theme| {
                if selected {
                    let palette = theme.extended_palette();
                    container::Style {
                        background: Some(palette.primary.base.color.into()),
                        text_color: Some(palette.primary.base.text),
                        ..container::Style::default()
                    }
                } else {
                    container::Style::default()
                }
            }),
    )
    .on_press(Message::Request(Request::Select(key)))
    .into()
}

/// The walk, as messages.
fn walk_pieces(
    service: Arc<dyn Service>,
    storage_id: u64,
    include_hidden: bool,
    cancel: dto::Cancel,
) -> BoxStream<'static, Result<dto::WalkUpdate, ErrorResponse>> {
    enum State {
        Opening(Arc<dyn Service>),
        Walking(Box<dyn Walking>),
        Over,
    }
    stream::unfold(State::Opening(service), move |state| {
        let cancel = cancel.clone();
        async move {
            let mut walking = match state {
                State::Opening(service) => {
                    match service.walk(storage_id, include_hidden, cancel).await {
                        Ok(walking) => walking,
                        Err(err) => {
                            return Some((Err(error::to_error_response(err)), State::Over));
                        }
                    }
                }
                State::Walking(walking) => walking,
                State::Over => return None,
            };
            match walking.next().await? {
                Ok(update) => Some((Ok(update), State::Walking(walking))),
                Err(err) => Some((Err(error::to_error_response(err)), State::Over)),
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeSearch, drain};

    fn found(folder: &str, name: &str, is_folder: bool) -> dto::Found {
        dto::Found {
            folder: folder.into(),
            entry: dto::Entry {
                id: 1,
                name: name.into(),
                is_folder,
                size_bytes: 10,
                modified: None,
            },
        }
    }

    fn piece(found: Vec<dto::Found>, done: bool) -> dto::WalkUpdate {
        let items = found.len();
        dto::WalkUpdate {
            found,
            folders_read: 1,
            items,
            done,
            ..dto::WalkUpdate::default()
        }
    }

    /// A panel open on storage 1, whose walk has not answered yet.
    fn open(search: FakeSearch) -> (Handler, Arc<FakeSearch>) {
        let service = Arc::new(search);
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(1, "Internal", false);
        (handler, service)
    }

    fn walked(handler: &mut Handler, update: dto::WalkUpdate) {
        let walk = handler.walk;
        let _ = handler.update(Message::Walked {
            walk,
            result: Ok(update),
        });
    }

    fn ask(handler: &mut Handler, request: Request) {
        let _ = handler.update(Message::Request(request));
    }

    fn result_names(handler: &Handler) -> Vec<String> {
        handler
            .matches
            .iter()
            .map(|&at| handler.catalogue[at].entry.name.clone())
            .collect()
    }

    /// DCIM/Camera with a.jpg, beach.mp4 and Beach day.jpg; Music/beach.mp3.
    fn a_phone() -> Vec<dto::Found> {
        vec![
            found("/", "DCIM", true),
            found("/DCIM", "Camera", true),
            found("/DCIM/Camera", "a.jpg", false),
            found("/DCIM/Camera", "beach.mp4", false),
            found("/DCIM/Camera", "Beach day.jpg", false),
            found("/Music", "beach.mp3", false),
        ]
    }

    #[tokio::test]
    async fn opening_walks_the_storage_once_with_what_is_hidden_as_asked() {
        let service = Arc::new(FakeSearch::finding(&[("/", "a.jpg", false)]));
        let mut handler = Handler::new(service.clone());

        let task = handler.open(1, "Internal", true);
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
        let again = handler.open(1, "Internal", true);
        let _ = drain(again).await;

        assert!(handler.is_open());
        assert_eq!(
            *service.walks.lock().unwrap(),
            [(1, true)],
            "not walked again"
        );
        assert_eq!(handler.catalogue.len(), 1);
        assert!(handler.progress.done);
    }

    fn mid_june() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
    }

    fn dated(folder: &str, name: &str, month: u8, day: u8) -> dto::Found {
        let mut item = found(folder, name, false);
        item.entry.modified = Some(dto::Timestamp {
            year: 2026,
            month,
            day,
            hour: 8,
            minute: 0,
        });
        item
    }

    #[test]
    fn a_date_finds_what_changed_then_with_or_without_words_and_kind() {
        let (mut handler, _) = open(FakeSearch::default());
        handler.today = mid_june;
        walked(
            &mut handler,
            piece(
                vec![
                    dated("/DCIM/Camera", "today.jpg", 6, 15),
                    dated("/DCIM/Camera", "monday.mp4", 6, 10),
                    dated("/DCIM/Camera", "may.jpg", 5, 2),
                    found("/DCIM/Camera", "undated.jpg", false),
                    found("/DCIM", "Camera", true),
                ],
                true,
            ),
        );

        ask(&mut handler, Request::Date(Some(dto::DateRange::LastWeek)));
        assert_eq!(result_names(&handler), ["today.jpg", "monday.mp4"]);
        ask(&mut handler, Request::Kind(Some(FileKind::Photo)));
        assert_eq!(result_names(&handler), ["today.jpg"]);
        ask(&mut handler, Request::Kind(None));
        ask(&mut handler, Request::Date(Some(dto::DateRange::Today)));
        ask(&mut handler, Request::Query("mon".into()));
        assert!(result_names(&handler).is_empty(), "words and date together");
        ask(&mut handler, Request::Query(String::new()));
        ask(&mut handler, Request::Date(Some(dto::DateRange::ThisYear)));
        assert_eq!(
            result_names(&handler).len(),
            3,
            "no undated file, no folder"
        );
        ask(&mut handler, Request::Date(None));
        assert!(
            result_names(&handler).is_empty(),
            "nothing asked: nothing listed"
        );

        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Any date").is_err(), "a menu draws its own words");
    }

    #[test]
    fn typing_looks_through_what_the_walk_saw_without_walking_again() {
        let (mut handler, _) = open(FakeSearch::default());
        walked(&mut handler, piece(a_phone(), true));
        let walk = handler.walk;

        ask(&mut handler, Request::Query("beach".into()));
        assert_eq!(
            result_names(&handler),
            ["beach.mp4", "Beach day.jpg", "beach.mp3"]
        );
        ask(&mut handler, Request::Query("beach day".into()));
        assert_eq!(result_names(&handler), ["Beach day.jpg"]);
        ask(&mut handler, Request::Query("beach".into()));
        ask(&mut handler, Request::Kind(Some(FileKind::Video)));
        assert_eq!(result_names(&handler), ["beach.mp4"]);
        ask(&mut handler, Request::Query(String::new()));
        assert_eq!(result_names(&handler), ["beach.mp4"], "every video");

        assert_eq!(handler.walk, walk, "no new walk");
        assert!(handler.progress.done);
    }

    #[test]
    fn results_grow_as_the_walk_goes_and_a_restart_starts_them_over() {
        let (mut handler, _) = open(FakeSearch::default());
        ask(&mut handler, Request::Query("beach".into()));

        walked(&mut handler, piece(a_phone()[..4].to_vec(), false));
        assert_eq!(result_names(&handler), ["beach.mp4"]);
        assert!(!handler.progress.done);
        walked(&mut handler, piece(a_phone()[4..].to_vec(), true));
        assert_eq!(
            result_names(&handler),
            ["beach.mp4", "Beach day.jpg", "beach.mp3"]
        );

        walked(
            &mut handler,
            dto::WalkUpdate {
                restart: true,
                ..piece(vec![found("/", "beach.wav", false)], false)
            },
        );
        assert_eq!(result_names(&handler), ["beach.wav"]);
    }

    #[test]
    fn a_piece_of_an_older_walk_is_dropped() {
        let (mut handler, _) = open(FakeSearch::default());
        let old = handler.walk;
        let _ = handler.update(Message::Request(Request::Refresh));

        let _ = handler.update(Message::Walked {
            walk: old,
            result: Ok(piece(a_phone(), true)),
        });

        assert!(handler.catalogue.is_empty());
        assert!(!handler.progress.done, "the new walk is still on");
    }

    #[test]
    fn only_a_walk_that_lost_the_phone_says_so() {
        let failed = |error| Message::Walked {
            walk: 1,
            result: Err(error::to_error_response(error)),
        };

        assert!(failed(dto::Error::Timeout).device_lost().is_some());
        assert!(failed(dto::Error::Disconnected).device_lost().is_some());
        assert!(failed(dto::Error::ReadOnly).device_lost().is_none());
        assert!(
            Message::Walked {
                walk: 1,
                result: Ok(dto::WalkUpdate::default()),
            }
            .device_lost()
            .is_none()
        );
        assert!(Message::Request(Request::Close).device_lost().is_none());
    }

    #[test]
    fn a_failed_walk_says_why() {
        let (mut handler, _) = open(FakeSearch::default());
        let walk = handler.walk;

        let _ = handler.update(Message::Walked {
            walk,
            result: Err(error::to_error_response(dto::Error::Disconnected)),
        });

        assert!(handler.error.is_some());
        assert!(handler.progress.done);
        let _ = handler.view();
    }

    #[test]
    fn results_are_picked_by_full_path_and_handed_over_by_folder() {
        let (mut handler, _) = open(FakeSearch::default());
        walked(
            &mut handler,
            piece(
                vec![
                    found("/DCIM/Camera", "a.jpg", false),
                    found("/Pictures", "a.jpg", false),
                    found("/DCIM/Camera", "b.jpg", false),
                ],
                true,
            ),
        );
        ask(&mut handler, Request::Query("jpg".into()));

        ask(&mut handler, Request::Select("/DCIM/Camera/a.jpg".into()));
        assert_eq!(handler.selected(), 1, "the other a.jpg is not picked");
        assert_eq!(
            handler.cursor_item(),
            Some(("/DCIM/Camera".into(), "a.jpg".into()))
        );
        handler.set_modifiers(Modifiers::COMMAND);
        ask(&mut handler, Request::Select("/DCIM/Camera/b.jpg".into()));
        ask(&mut handler, Request::Select("/Pictures/a.jpg".into()));

        assert_eq!(
            handler.chosen_by_folder(),
            [
                (
                    "/DCIM/Camera".to_owned(),
                    vec!["a.jpg".to_owned(), "b.jpg".to_owned()]
                ),
                ("/Pictures".to_owned(), vec!["a.jpg".to_owned()]),
            ]
        );
        ask(&mut handler, Request::Query("b".into()));
        assert_eq!(handler.selected(), 0, "a new search starts a new selection");
        ask(&mut handler, Request::SelectAll);
        assert_eq!(handler.selected(), 1);
    }

    #[test]
    fn closing_stops_the_walk_and_lets_go_of_what_it_saw() {
        let (mut handler, _) = open(FakeSearch::default());
        walked(&mut handler, piece(a_phone(), false));
        let cancel = handler.cancel.clone().unwrap();

        ask(&mut handler, Request::Close);

        assert!(!handler.is_open());
        assert!(cancel.is_cancelled());
        assert!(handler.catalogue.is_empty() && handler.matches.is_empty());
        assert_eq!(handler.storage_id(), None);
    }

    /// A panel whose search found `n` photos, in one folder.
    fn photos(n: usize) -> Handler {
        let (mut handler, _) = open(FakeSearch::default());
        let found = (0..n)
            .map(|i| found("/DCIM/Camera", &format!("IMG_{i:05}.jpg"), false))
            .collect();
        walked(&mut handler, piece(found, true));
        ask(&mut handler, Request::Kind(Some(FileKind::Photo)));
        handler
    }

    #[test]
    fn a_search_that_finds_thousands_builds_only_the_rows_near_the_screen() {
        let handler = photos(5_000);
        assert_eq!(handler.matches.len(), 5_000);

        let mut ui = iced_test::simulator(handler.view());
        let built: Vec<usize> = (0..5_000)
            .filter(|i| ui.find(format!("IMG_{i:05}.jpg")).is_ok())
            .collect();

        assert!(built.contains(&0));
        assert!(built.len() < 100, "built {} of 5,000 rows", built.len());
    }

    #[test]
    fn looking_through_200_000_items_for_a_word_is_quick() {
        let (mut handler, _) = open(FakeSearch::default());
        let found = (0..200_000)
            .map(|i| found("/DCIM/Camera", &format!("IMG_{i:06}.jpg"), false))
            .collect();
        walked(&mut handler, piece(found, true));
        let started = std::time::Instant::now();

        ask(&mut handler, Request::Query("123".into()));

        eprintln!("200,000 items looked through in {:?}", started.elapsed());
        assert_eq!(handler.matches.len(), 1_599);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    // What fills the phone: results in order.

    fn sized(folder: &str, name: &str, size: u64, day: Option<u8>) -> dto::Found {
        let mut item = found(folder, name, false);
        item.entry.size_bytes = size;
        item.entry.modified = day.map(|day| dto::Timestamp {
            year: 2026,
            month: 10,
            day,
            hour: 9,
            minute: 0,
        });
        item
    }

    fn a_full_phone() -> Vec<dto::Found> {
        vec![
            found("/", "DCIM", true),
            sized("/DCIM/Camera", "trip.mp4", 3_000_000_000, Some(3)),
            sized("/DCIM/Camera", "a.jpg", 4_000_000, Some(9)),
            sized("/Movies", "film.mkv", 0xFFFF_FFFF, Some(1)),
            sized("/Download", "notes.txt", 2_000, None),
            sized("/Music", "Beach.mp3", 9_000_000, Some(5)),
        ]
    }

    #[test]
    fn with_nothing_typed_ordering_by_size_lists_every_file_largest_first() {
        let (mut handler, _) = open(FakeSearch::default());
        walked(&mut handler, piece(a_full_phone(), true));
        assert!(result_names(&handler).is_empty(), "nothing asked yet");

        ask(&mut handler, Request::SortBy(dto::SortKey::Size));

        assert_eq!(
            result_names(&handler),
            ["film.mkv", "trip.mp4", "Beach.mp3", "a.jpg", "notes.txt"],
            "largest first, as the sizes read; no folder"
        );
        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("Size ▼").is_ok());
            assert!(ui.find("5 found on Internal").is_ok());
        }

        ask(&mut handler, Request::SortBy(dto::SortKey::Size));
        assert_eq!(
            result_names(&handler)[0],
            "notes.txt",
            "again: smallest first"
        );
    }

    #[test]
    fn results_order_by_name_or_date_and_keep_the_kind() {
        let (mut handler, _) = open(FakeSearch::default());
        walked(&mut handler, piece(a_full_phone(), true));

        ask(&mut handler, Request::SortBy(dto::SortKey::Modified));
        assert_eq!(
            result_names(&handler),
            ["a.jpg", "Beach.mp3", "trip.mp4", "film.mkv", "notes.txt"],
            "newest first, undated last"
        );
        ask(&mut handler, Request::SortBy(dto::SortKey::Name));
        assert_eq!(
            result_names(&handler),
            ["a.jpg", "Beach.mp3", "film.mkv", "notes.txt", "trip.mp4"],
            "A to Z, any case"
        );
        ask(&mut handler, Request::SortBy(dto::SortKey::Name));
        assert_eq!(result_names(&handler)[0], "trip.mp4", "Z to A");

        ask(&mut handler, Request::Kind(Some(FileKind::Video)));
        ask(&mut handler, Request::SortBy(dto::SortKey::Size));
        assert_eq!(result_names(&handler), ["film.mkv", "trip.mp4"]);
    }

    #[test]
    fn results_arriving_while_ordered_take_their_place() {
        let (mut handler, _) = open(FakeSearch::default());
        ask(&mut handler, Request::SortBy(dto::SortKey::Size));
        walked(
            &mut handler,
            piece(
                vec![
                    sized("/a", "middle.jpg", 500, None),
                    sized("/a", "small.jpg", 10, None),
                ],
                false,
            ),
        );
        walked(
            &mut handler,
            piece(
                vec![
                    sized("/b", "big.mp4", 9_000, None),
                    sized("/b", "tiny.txt", 1, None),
                    sized("/b", "medium.jpg", 200, None),
                ],
                true,
            ),
        );

        assert_eq!(
            result_names(&handler),
            [
                "big.mp4",
                "middle.jpg",
                "medium.jpg",
                "small.jpg",
                "tiny.txt"
            ]
        );
        let keys: Vec<String> = handler
            .matches
            .iter()
            .map(|&at| mapper::key_of(&handler.catalogue[at]))
            .collect();
        assert_eq!(handler.keys, keys, "each key beside its result");
    }

    #[test]
    fn ordering_200_000_items_as_they_arrive_stays_quick() {
        let (mut handler, _) = open(FakeSearch::default());
        ask(&mut handler, Request::SortBy(dto::SortKey::Size));
        let started = std::time::Instant::now();
        for chunk in 0..400u64 {
            let found = (0..500u64)
                .map(|i| {
                    let n = chunk * 500 + i;
                    sized(
                        "/DCIM",
                        &format!("IMG_{n:06}.jpg"),
                        (n * 7919) % 100_000,
                        None,
                    )
                })
                .collect();
            walked(&mut handler, piece(found, chunk == 399));
        }

        eprintln!(
            "200,000 items taken in, ordered by size, in {:?}",
            started.elapsed()
        );
        assert_eq!(handler.matches.len(), 200_000);
        let size = |at: usize| handler.catalogue[at].entry.size_bytes;
        assert!(handler.matches.windows(2).all(|w| size(w[0]) >= size(w[1])));
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[test]
    fn a_click_on_a_row_picks_it_and_the_buttons_follow() {
        let mut handler = photos(3);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("IMG_00001.jpg".to_owned()).is_ok());

        let row = ui
            .find("IMG_00001.jpg".to_owned())
            .unwrap()
            .bounds()
            .center();
        ui.point_at(row);
        let _ = ui.simulate(iced_test::simulator::click());
        for message in ui.into_messages() {
            let _ = handler.update(message);
        }

        assert_eq!(handler.selected(), 1);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Show in folder".to_owned()).is_ok());
        assert!(ui.find("Download to Mac".to_owned()).is_ok());
    }
}
