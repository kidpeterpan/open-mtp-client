use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::stream::{self, BoxStream, StreamExt};
use iced::keyboard::Modifiers;
use iced::widget::image::Handle as Picture;
use iced::widget::scrollable::{AbsoluteOffset, Viewport};
use iced::widget::text::Wrapping;
use iced::widget::{
    Id, button, center, column, container, image, mouse_area, opaque, operation, pick_list, pin,
    responsive, row, rule, scrollable, space, stack, text, text_input, toggler, tooltip,
};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Size, Task, mouse};
use internal::dto;

use super::compare;
use super::go_to;
use super::grid;
use super::groups;
use super::history::History;
use super::lasso::lasso;
use super::layout::{self, Fit};
use super::mapper;
use super::menu;
use super::middle_text::{middle_text, probably_cut};
use super::rename_many::{self, Many};
use super::request::{Jump, Request};
use super::response::{Badge, BadgeKind, EntryResponse, NoticeResponse, PasteResponse};
use super::right_click::right_click;
use super::selection::{self as rows, Click, Sweep};
use super::undo::{self, Undo};
use super::window;
use crate::handler::error::{self, ErrorResponse};
use crate::handler::format::format_count;
use crate::i18n::{t, tf};

/// How wide the filter's kind menu is: room for "Documents".
const KIND_WIDTH: f32 = 104.0;

/// A kind in the filter's menu, as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KindChoice(Option<dto::FileKind>);

impl std::fmt::Display for KindChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            None => t("explorer.filter.all_kinds"),
            kind => crate::handler::format::kind_label(kind),
        })
    }
}

/// How wide the filter's date menu is: room for "Last 30 days".
const DATE_WIDTH: f32 = 112.0;

/// A date in the filter's menu, as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateChoice(Option<dto::DateRange>);

impl std::fmt::Display for DateChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::handler::format::date_label(self.0))
    }
}

/// Typing after a pause this long starts a new name, as in Finder.
const TYPING_PAUSE: Duration = Duration::from_secs(1);

/// What a list row is like at a density. Every row of a list is as tall as
/// the others, so the rows on screen follow from the scroll offset alone.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RowStyle {
    height: f32,
    /// Text size of the name, and of the other columns.
    name: u32,
    detail: u32,
    /// The side of the square the folder or file glyph is drawn in.
    glyph: f32,
}

fn row_style(density: dto::Density) -> RowStyle {
    match density {
        // Finder's own list.
        dto::Density::Compact => RowStyle {
            height: 22.0,
            name: 13,
            detail: 12,
            glyph: 14.0,
        },
        dto::Density::Standard => RowStyle {
            height: 28.0,
            name: 14,
            detail: 12,
            glyph: 16.0,
        },
        dto::Density::Large => RowStyle {
            height: 36.0,
            name: 16,
            detail: 14,
            glyph: 20.0,
        },
    }
}

/// Every other row is shaded, as in Finder. Counted from the top of the
/// folder, not of what is built, so the shading stays put while scrolling.
fn striped(index: usize) -> bool {
    index % 2 == 1
}

/// Lists one volume: a storage on the phone (its MTP storage id) or the Mac
/// (which has a single volume, so the id is ignored).
#[async_trait::async_trait]
pub trait Service: Send + Sync {
    async fn list(
        &self,
        volume_id: u64,
        path: &str,
        show_hidden: bool,
    ) -> Result<Vec<dto::Entry>, dto::Error>;

    fn sort(&self, entries: &mut [dto::Entry], sort: dto::Sort);

    /// Returns the name the folder was given.
    async fn make_folder(
        &self,
        volume_id: u64,
        parent: &str,
        name: &str,
    ) -> Result<String, dto::Error>;

    /// Returns the new name.
    async fn rename(
        &self,
        volume_id: u64,
        parent: &str,
        old: &str,
        new: &str,
    ) -> Result<String, dto::Error>;

    /// What is in the folder `path`, everything below it counted (hidden
    /// items too), for its size in the list. Stops between pieces on `cancel`.
    async fn folder_size(
        &self,
        _volume_id: u64,
        _path: &str,
        _cancel: dto::Cancel,
    ) -> Result<dto::Tally, dto::Error> {
        Err(dto::Error::Unsupported)
    }

    /// The folders this pane knows to be in `path`, to offer as a path is
    /// typed. The phone answers from what it remembers, never by asking.
    async fn folder_names(&self, _volume_id: u64, _path: &str) -> Vec<String> {
        Vec::new()
    }

    /// The new names of `items` (name, is a folder) renamed by `pattern` in a
    /// folder holding `folder`, and what would stop each. Asked as the user
    /// types, so it asks nothing of the phone.
    fn plan_renames(
        &self,
        items: &[(String, bool)],
        folder: &[String],
        pattern: &dto::RenamePattern,
    ) -> Vec<dto::Renaming>;

    async fn delete(
        &self,
        volume_id: u64,
        parent: &str,
        names: &[String],
    ) -> Result<(), dto::Error>;

    /// Copies or moves items inside one volume. Only the phone can, so the
    /// default says it is unsupported.
    async fn paste(&self, _request: &dto::PasteRequest) -> Result<dto::PasteSummary, dto::Error> {
        Err(dto::Error::Unsupported)
    }

    /// A small preview of `entry` in `folder`, or `None` when there is none.
    async fn thumbnail(
        &self,
        volume_id: u64,
        folder: &str,
        entry: &dto::Entry,
    ) -> Result<Option<dto::Thumbnail>, dto::Error>;

    /// Opens Finder with `path` selected; only asked of volumes that
    /// `shows_in_finder`.
    fn reveal(&self, path: &str);

    /// Opens System Settings on the pane that lets the app into a folder
    /// macOS refused. Only the Mac's folders are refused so; the default
    /// does nothing.
    fn open_privacy_settings(&self, _pane: dto::PrivacyPane) {}

    /// Starts listing a folder in pieces, for volumes where that matters: a
    /// phone describes its files one round trip each, so a big folder is
    /// better shown as it arrives. `cancel` stops it between files. The
    /// default hands over the whole folder at once.
    ///
    /// With `allow_remembered`, a phone folder that still holds as many items
    /// as when it was last read in full may be handed over from memory,
    /// instead of asking the phone about every file again. It is false when
    /// the user asked to see the phone as it is now.
    async fn open_listing(
        &self,
        volume_id: u64,
        path: &str,
        show_hidden: bool,
        _cancel: dto::Cancel,
        _allow_remembered: bool,
    ) -> Result<Box<dyn Listing>, dto::Error> {
        let entries = self.list(volume_id, path, show_hidden).await?;
        Ok(Box::new(Whole(Some(entries))))
    }
}

/// A folder being read piece by piece.
#[async_trait::async_trait]
pub trait Listing: Send {
    /// The next piece; `None` when there are no more (it ended or was
    /// cancelled).
    async fn next(&mut self) -> Option<Result<dto::ListingUpdate, dto::Error>>;
}

/// A folder that is already complete: one finished piece.
struct Whole(Option<Vec<dto::Entry>>);

#[async_trait::async_trait]
impl Listing for Whole {
    async fn next(&mut self) -> Option<Result<dto::ListingUpdate, dto::Error>> {
        let entries = self.0.take()?;
        Some(Ok(dto::ListingUpdate {
            loaded: entries.len(),
            total: entries.len(),
            entries,
            restart: false,
            done: true,
        }))
    }
}

/// The most thumbnails asked for at once, so a fast scroll can't queue
/// hundreds of USB reads.
const THUMBNAILS_PER_BATCH: usize = 24;

/// The most thumbnails kept at once. A camera folder holds thousands of
/// photos, and a decoded thumbnail is tens of kilobytes; without a limit,
/// scrolling through one filled gigabytes of memory.
const THUMBNAIL_CACHE: usize = 400;

#[derive(Debug, Clone)]
enum Thumb {
    Loading,
    Ready(Picture),
    /// Nothing to show; the tile shows the file's kind instead.
    None,
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    /// What a delete of `names` would copy to the Trash first (`None`: not
    /// every size is known).
    DeleteCounted {
        names: Vec<String>,
        bytes: Option<u64>,
    },
    /// A confirmed phone delete that keeps a copy: queued by `app.rs`.
    DeleteKeepingCopy(dto::DeleteRequest),
    /// The left button went down over the list at `at` (in the list's own
    /// coordinates); `taken` says a row, tile or the scrollbar handled it.
    /// Where nothing (or a row away from its name) did, a lasso starts.
    PressedAt {
        at: Point,
        size: Size,
        taken: bool,
    },
    /// The pointer moved while a lasso runs: where, and how big the list is.
    LassoMoved {
        at: Point,
        size: Size,
    },
    /// A beat of the clock while a lasso is held past the top or bottom of
    /// the list: it scrolls on.
    LassoTick,
    /// The pointer is at `x` in the window while a column is being resized.
    ResizeMoved(f32),
    /// A drag from the other pane came over this list (`true`) or left it.
    Hover(bool),
    /// A file was opened (double-click, ↵, or the menu): `app.rs` opens it in
    /// its Mac app, fetching it from the phone first.
    OpenFile(String),
    Listed {
        volume_id: u64,
        path: String,
        result: Result<Vec<dto::Entry>, ErrorResponse>,
    },
    /// A folder was made, renamed or deleted; `Ok` holds the name to select.
    Edited {
        volume_id: u64,
        path: String,
        result: Result<Option<String>, ErrorResponse>,
        /// How to undo it; `None` for a delete, which can't be.
        undo: Option<Undo>,
    },
    /// The folder for New Folder with Selection was made (`Ok` holds its
    /// name) in `path`, for `names` to move into.
    GroupMade {
        volume_id: u64,
        path: String,
        names: Vec<String>,
        result: Result<String, ErrorResponse>,
    },
    /// A beat while a drag is held over a folder: it opens once held for
    /// `SPRING_DELAY`.
    SpringTick(Instant),
    /// A folder's size was counted (`run` says which counting it was).
    FolderSized {
        run: u64,
        name: String,
        result: Result<dto::Tally, ErrorResponse>,
    },
    /// The folders known in `parent`, for Go to Folder.
    FolderNames {
        parent: String,
        names: Vec<String>,
    },
    /// Renaming several: how many are done so far.
    RenamedSome(usize),
    /// Renaming several in `path` ended, `asked` of them.
    RenamedMany {
        volume_id: u64,
        path: String,
        outcome: rename_many::Outcome,
        asked: usize,
    },
    /// An undo (⌘Z) of `what` ended: the folder to show and what to select
    /// there, or why nothing was undone.
    Undone {
        volume_id: u64,
        what: &'static str,
        result: Result<(String, Vec<String>), ErrorResponse>,
    },
    /// A paste into `path` ended: what to select and tell, or why it could not
    /// even start.
    Pasted {
        volume_id: u64,
        path: String,
        result: Result<PasteResponse, ErrorResponse>,
    },
    /// The phone can't copy or move by itself (it says `Unsupported`), so the
    /// paste of `from` into `path` is for the app to do through the Mac, as a
    /// queued transfer. Nothing was changed on the phone.
    PasteViaMac {
        mode: dto::Paste,
        from: Vec<Selection>,
        volume_id: u64,
        path: String,
    },
    /// Part of a folder has arrived: everything so far, and how far along the
    /// phone is. The finished folder comes as `Listed`.
    Loading {
        volume_id: u64,
        path: String,
        entries: Vec<dto::Entry>,
        loaded: usize,
        total: usize,
    },
    /// The file list scrolled.
    Scrolled(Viewport),
    /// The file list was right-clicked at `at`; `area` is the list's bounds,
    /// which the menu stays inside. Both are in window coordinates.
    ShowMenu {
        at: Point,
        area: Rectangle,
    },
    Thumbnail {
        volume_id: u64,
        path: String,
        name: String,
        picture: Option<Picture>,
    },
    /// A thumbnail for the folder `path` on `volume_id` could not be had
    /// because the phone is gone or stopped answering: the app shows that, as
    /// for a listing, if the pane still shows that folder.
    ThumbnailLost {
        volume_id: u64,
        path: String,
        error: ErrorResponse,
    },
}

impl Message {
    /// A paste the phone could not do by itself: what to paste, and where.
    pub fn via_mac(&self) -> Option<(dto::Paste, &[Selection], u64, &str)> {
        match self {
            Message::PasteViaMac {
                mode,
                from,
                volume_id,
                path,
            } => Some((*mode, from, *volume_id, path)),
            _ => None,
        }
    }

    /// The error this message carries if it means the device is gone.
    pub fn device_lost(&self) -> Option<&ErrorResponse> {
        let error = match self {
            Message::Listed {
                result: Err(error), ..
            }
            | Message::Edited {
                result: Err(error), ..
            }
            | Message::Pasted {
                result: Err(error), ..
            }
            | Message::ThumbnailLost { error, .. } => error,
            Message::RenamedMany { outcome, .. } => outcome.stopped.as_ref()?,
            Message::FolderSized {
                result: Err(error), ..
            } => error,
            _ => return None,
        };
        error.device_lost.then_some(error)
    }
}

/// What a pane browses: a storage on the phone, or the Mac.
#[derive(Debug, Clone)]
pub struct Volume {
    pub id: u64,
    /// Labels the root breadcrumb.
    pub name: String,
    /// Shown as a single "Home" breadcrumb while browsing inside it.
    pub home: Option<String>,
    /// The phone deletes for good, so it asks first; the Mac uses the Trash.
    pub deletes_permanently: bool,
    /// Label of the button that copies the selection to the other side.
    pub transfer_action: Option<&'static str>,
    /// Its files are on the Mac, so the menu offers "Show in Finder".
    pub shows_in_finder: bool,
    /// Items can be cut and pasted inside it.
    pub pastes_within: bool,
    /// The phone pane: the toolbar offers the tools for the whole phone
    /// (Search) and for its folder with the Mac pane's (Mirror).
    pub phone_tools: bool,
    /// An Android phone's storage: Android keeps `Android/data` and
    /// `Android/obb` from computers, so an empty or refused one there says
    /// why.
    pub hides_app_folders: bool,
}

/// What a transfer from this pane should copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub volume_id: u64,
    pub path: String,
    pub names: Vec<String>,
}

/// A change waiting for the user to confirm it.
#[derive(Debug, Clone, PartialEq)]
enum Change {
    NewFolder,
    Rename {
        original: String,
    },
    /// Several selected items, renamed by one pattern.
    RenameMany(Box<Many>),
    /// Going to a folder whose path is typed (in the editor's name).
    GoTo(GoTo),
    Delete {
        names: Vec<String>,
        keep: Keep,
    },
}

/// Whether a phone delete keeps a copy in the Mac's Trash first, as its
/// question says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Deleted for good: the setting is off (or the Mac, which uses the Trash
    /// anyway).
    No,
    /// Counting what would be copied, before the question can be answered.
    Counting,
    /// This much goes to the Trash first.
    Copy(u64),
    /// More than `KEEP_MOST`: deleted for good, as before.
    TooMuch(u64),
    /// The phone doesn't give every size: no copy can be promised.
    Unknown,
}

/// The names Go to Folder offers: the folders known in `parent`, those that
/// go on from what is typed (worked out when either changes, never per
/// redraw), and which one ↑/↓ picked.
#[derive(Debug, Clone, PartialEq)]
struct GoTo {
    parent: String,
    names: Vec<String>,
    offered: Vec<String>,
    chosen: Option<usize>,
}

impl GoTo {
    fn offer(&mut self, typed: &str) {
        let (_, partial) = go_to::split(typed);
        self.offered = go_to::suggestions(&self.names, partial)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if self.chosen.is_some_and(|at| at >= self.offered.len()) {
            self.chosen = None;
        }
    }
}

/// What is known of a folder's size.
#[derive(Debug, Clone, PartialEq)]
enum FolderSize {
    Counting,
    Known(dto::Tally),
}

struct Editor {
    change: Change,
    name: String,
    error: Option<ErrorResponse>,
    busy: bool,
}

struct Location {
    volume: Volume,
    path: String,
}

/// The open right-click menu.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OpenMenu {
    at: Point,
    area: Rectangle,
    /// About the selection, rather than the folder (empty space was clicked).
    on_entry: bool,
}

pub struct Handler {
    service: Arc<dyn Service>,
    location: Option<Location>,
    show_hidden: bool,
    sort: dto::Sort,
    /// Sorted listing; `rows` is the display form of what the filter leaves
    /// of it (`shown` says which of `entries` each row is), rebuilt whenever
    /// either changes.
    entries: Vec<dto::Entry>,
    rows: Vec<EntryResponse>,
    shown: Vec<usize>,
    /// Every entry's display form, so typing in the filter only picks rows.
    all_rows: Vec<EntryResponse>,
    /// Where each entry is in `entries`, by name: built with the rows, so the
    /// selection's size is a lookup per selected name, never a pass over the
    /// folder.
    by_name: std::collections::HashMap<String, usize>,
    /// What is typed in the filter; empty shows everything.
    filter: String,
    /// The kind the filter keeps, beside its words.
    filter_kind: Option<dto::FileKind>,
    /// The dates the filter keeps.
    filter_date: Option<dto::DateRange>,
    /// Today, for the date filter: the Mac's clock, a fixed day in tests.
    today: fn() -> chrono::NaiveDate,
    filter_input: Id,
    selection: rows::Selection,
    /// Held keys, so a click can toggle (⌘) or extend (⇧) the selection.
    modifiers: Modifiers,
    /// An item to select and scroll to once the folder being opened is read
    /// (a search's "Show in folder").
    reveal: Option<String>,
    /// Back and forward through the folders this pane has shown.
    history: History,
    /// Where to go if the folder the pane was opened at can't be read.
    fallback: Option<String>,
    /// How wide the list's columns are, and a resize under way: which edge,
    /// the widths when it started, and where the pointer first was.
    columns: dto::ColumnWidths,
    resizing: Option<Resizing>,
    /// The pane's last change (its mark, which orders changes across both
    /// panes), and how to undo it if it can be.
    changed_at: u64,
    last_undo: Option<(u64, Undo)>,
    /// What has been typed to select a name, and when the last of it was.
    typed: String,
    typed_at: Option<Instant>,
    /// The footer with counts and listing toggles.
    status_bar: bool,
    list: Id,
    /// Scroll offset, visible height and width of the file list.
    viewport: (f32, f32, f32),
    view: dto::ViewMode,
    density: dto::Density,
    /// How big the grid's tiles are.
    tile: grid::Tile,
    thumbnails: std::collections::HashMap<String, Thumb>,
    loading: bool,
    /// How many items the phone has described and how many it has, while a
    /// big folder is still arriving.
    progress: Option<(usize, usize)>,
    /// Stops the listing in progress, between files, when another starts.
    listing: Option<dto::Cancel>,
    error: Option<ErrorResponse>,
    editor: Option<Editor>,
    name_input: Id,
    menu: Option<OpenMenu>,
    /// The entry a right-click landed on, until the menu opens for it.
    menu_target: Option<String>,
    /// A paste is under way; edits wait for it.
    pasting: Option<dto::Paste>,
    /// Stops a run of renames between items.
    renaming: Option<dto::Cancel>,
    /// The folder row a drag is held over, and since when: held long enough,
    /// it opens.
    spring: Option<(String, Instant)>,
    /// Where the pane was before a drag opened folders in it, to go back to
    /// if nothing is dropped.
    spring_origin: Option<String>,
    /// Folder sizes are shown (View › Calculate Folder Sizes).
    sizes_on: bool,
    /// What is known of each folder's size here, by name.
    sizes: std::collections::HashMap<String, FolderSize>,
    /// Folders still to count, in the order shown.
    size_queue: std::collections::VecDeque<String>,
    /// The count under way, to stop it.
    sizing: Option<dto::Cancel>,
    /// Which counting is current: an older one's answer is dropped.
    size_run: u64,
    /// A folder just made around the selection, which the move under way
    /// goes into: (the folder it is in, its name).
    grouping: Option<(String, String)>,
    /// What the last paste said, until it is dismissed or the folder changes.
    notice: Option<NoticeResponse>,
    /// Items cut from this pane that are still waiting for a paste, drawn dim.
    cut: Option<Cut>,
    /// A button is down on a row: it may turn into a drag onto a folder.
    drag: Option<Drag>,
    /// What the last press landed on, until the list says where it was.
    pending_press: Option<Pressed>,
    /// A box being drawn to select.
    lasso: Option<Sweeping>,
    /// The other pane is dragging its selection, maybe to here.
    incoming: Option<Incoming>,
    /// The other pane is shown, so a drag can go there.
    other_pane: bool,
    /// A phone delete keeps a copy in the Trash first (the setting).
    keep_deleted: bool,
    /// What the rows are grouped by under headers.
    group_by: dto::GroupBy,
    /// The groups of `rows`, in order; empty when not grouped.
    groups: Vec<groups::Group>,
}

/// Which part of a row a press landed on. A press on a name (or a tile) can
/// start dragging the item; anywhere else on a row starts a lasso, as in
/// Finder's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pressed {
    Name,
    Row,
}

/// A lasso being drawn.
struct Sweeping {
    /// Where it started, in the coordinates of the whole list (scrolled
    /// rows included), so it stays put while the list scrolls.
    anchor: Point,
    /// Where the pointer is, in the visible list's coordinates.
    pointer: Point,
    /// How big the visible list is.
    size: Size,
    /// What was selected when it started, for ⌘ and ⇧.
    base: std::collections::HashSet<String>,
    sweep: Sweep,
}

/// A drag of the other pane's selection.
struct Incoming {
    count: usize,
    /// The pointer is over this list.
    inside: bool,
    /// The row it is over.
    over: Option<String>,
}

/// The most a lasso held past the edge scrolls in one beat (16 ms).
const LASSO_STEP: f32 = 24.0;

/// A press on a row that may turn into dragging the selection onto a folder.
struct Drag {
    /// The row the button went down on.
    pressed: String,
    /// The pointer has left that row with the button still down: a drag.
    active: bool,
    /// A plain press on one of several selected rows keeps them all, in case
    /// they are being dragged; if it was only a click, the selection narrows
    /// to that row when the button comes up.
    narrow_on_release: bool,
    /// The row the pointer is over.
    over: Option<String>,
    /// What is being dragged, from where: kept once the drag starts, since a
    /// spring-loaded folder shows somewhere else before the drop.
    source: Option<Selection>,
    /// The pointer is over this pane's list (said while the drag is on).
    inside: bool,
}

/// Holding a drag over a folder this long opens it (Finder's spring-loading).
pub const SPRING_DELAY: Duration = Duration::from_millis(800);

/// Items marked by ⌘X: where they are, and which.
struct Cut {
    volume_id: u64,
    path: String,
    names: std::collections::HashSet<String>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            location: None,
            show_hidden: false,
            sort: dto::Sort::default(),
            entries: Vec::new(),
            rows: Vec::new(),
            shown: Vec::new(),
            all_rows: Vec::new(),
            by_name: std::collections::HashMap::new(),
            filter: String::new(),
            filter_kind: None,
            filter_date: None,
            today: crate::handler::format::local_today,
            filter_input: Id::unique(),
            selection: rows::Selection::default(),
            modifiers: Modifiers::default(),
            reveal: None,
            history: History::default(),
            typed: String::new(),
            typed_at: None,
            changed_at: 0,
            last_undo: None,
            columns: dto::ColumnWidths::default(),
            resizing: None,
            fallback: None,
            status_bar: true,
            list: Id::unique(),
            viewport: (0.0, 0.0, 0.0),
            view: dto::ViewMode::List,
            density: dto::Density::Standard,
            tile: grid::Tile::default(),
            thumbnails: std::collections::HashMap::new(),
            loading: false,
            progress: None,
            listing: None,
            error: None,
            editor: None,
            name_input: Id::unique(),
            menu: None,
            menu_target: None,
            pasting: None,
            renaming: None,
            spring: None,
            spring_origin: None,
            sizes_on: false,
            sizes: std::collections::HashMap::new(),
            size_queue: std::collections::VecDeque::new(),
            sizing: None,
            size_run: 0,
            grouping: None,
            notice: None,
            cut: None,
            drag: None,
            pending_press: None,
            lasso: None,
            incoming: None,
            other_pane: false,
            keep_deleted: false,
            group_by: dto::GroupBy::None,
            groups: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.location.is_some()
    }

    /// Storage id and folder being shown.
    pub fn location(&self) -> Option<(u64, &str)> {
        self.location
            .as_ref()
            .map(|location| (location.volume.id, location.path.as_str()))
    }

    #[cfg(test)]
    pub fn deletes_permanently(&self) -> Option<bool> {
        self.location
            .as_ref()
            .map(|location| location.volume.deletes_permanently)
    }

    /// Copies or moves `from` (items on this phone, from one or several
    /// folders) into the folder shown, the way ⌘V does inside the phone. The
    /// answer arrives as `Pasted`.
    pub fn paste(&mut self, mode: dto::Paste, from: Vec<Selection>) -> Task<Message> {
        let Some(into) = self.location.as_ref().map(|location| location.path.clone()) else {
            return Task::none();
        };
        self.paste_into(mode, from, into)
    }

    /// `paste`, into the folder `into` (which may be one inside the folder
    /// shown: a drop on a folder, or anywhere on this volume: a drop on the
    /// sidebar).
    pub fn paste_into(
        &mut self,
        mode: dto::Paste,
        from: Vec<Selection>,
        into: String,
    ) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        if !location.volume.pastes_within || self.pasting.is_some() || self.editor.is_some() {
            return Task::none();
        }
        let (volume_id, path) = (location.volume.id, into);
        let asked = from.clone();
        let requests: Vec<dto::PasteRequest> = from
            .into_iter()
            .filter(|selection| !selection.names.is_empty())
            .map(|selection| dto::PasteRequest {
                mode,
                from_storage: selection.volume_id,
                from: selection.path,
                names: selection.names,
                to_storage: volume_id,
                to: path.clone(),
            })
            .collect();
        if requests.is_empty() {
            return Task::none();
        }
        self.notice = None;
        self.pasting = Some(mode);

        let paste = paste_items(Arc::clone(&self.service), requests);
        Task::perform(paste, move |end| match end {
            PasteEnd::Done(result) => Message::Pasted {
                volume_id,
                path,
                result,
            },
            PasteEnd::ViaMac => Message::PasteViaMac {
                mode,
                from: asked,
                volume_id,
                path,
            },
        })
    }

    /// A paste is under way.
    pub fn is_pasting(&self) -> bool {
        self.pasting.is_some()
    }

    /// Dims the items cut in this pane; `None` once they are pasted or the
    /// clipboard moves on.
    pub fn set_cut(&mut self, cut: Option<&Selection>) {
        self.cut = cut.map(|selection| Cut {
            volume_id: selection.volume_id,
            path: selection.path.clone(),
            names: selection.names.iter().cloned().collect(),
        });
    }

    /// The mouse button came up. A click that did not turn into a drag narrows
    /// the selection, if it was waiting to; a drag is dropped on the folder the
    /// pointer is over, if the selection can go there: moved, or copied with ⌥.
    fn release(&mut self) -> Task<Message> {
        self.lasso = None;
        self.pending_press = None;
        let Some(drag) = self.drag.take() else {
            return Task::none();
        };
        if !drag.active {
            if drag.narrow_on_release {
                self.selection.only(&drag.pressed);
            }
            return Task::none();
        }
        self.spring = None;
        let source = drag.source.clone();
        let target = drag
            .over
            .clone()
            .filter(|name| self.can_drop_on_with(name, source.as_ref()));
        let (Some(from), Some(location)) = (source.or_else(|| self.selection()), &self.location)
        else {
            return self.spring_back();
        };
        let into = match target {
            Some(target) => mapper::child_path(&location.path, &target),
            // Let go in a folder a drag opened: that is where it goes.
            None if drag.inside && self.spring_origin.is_some() && from.path != location.path => {
                location.path.clone()
            }
            // Not dropped on a folder here (let go elsewhere, or over the
            // other pane, which the app handles): folders it opened close.
            None => return self.spring_back(),
        };
        self.spring_origin = None;
        let mode = if self.modifiers.alt() {
            dto::Paste::Copy
        } else {
            dto::Paste::Move
        };
        self.paste_into(mode, vec![from], into)
    }

    /// Whether the selection can be dropped on the entry `name`: a folder, and
    /// not one of the things being dragged.
    fn can_drop_on(&self, name: &str) -> bool {
        let source = self.drag.as_ref().and_then(|drag| drag.source.as_ref());
        self.can_drop_on_with(name, source)
    }

    /// `can_drop_on`, for a drag carrying `source`: a folder that is not one
    /// of the things being dragged (which are in `source`'s folder).
    fn can_drop_on_with(&self, name: &str, source: Option<&Selection>) -> bool {
        let is_folder = self
            .by_name
            .get(name)
            .is_some_and(|&at| self.entries[at].is_folder);
        let dragged = match (source, &self.location) {
            (Some(source), Some(location)) => {
                source.path == location.path && source.names.iter().any(|n| n == name)
            }
            _ => self.selection.contains(name),
        };
        is_folder && !dragged
    }

    /// What the drag in progress says, if there is one.
    fn drag_hint(&self) -> Option<String> {
        let drag = self.drag.as_ref().filter(|drag| drag.active)?;
        let sprung = self
            .spring_origin
            .as_ref()
            .filter(|_| drag.inside)
            .and(self.location.as_ref())
            .map(|location| mapper::folder_name(&location.path));
        let target = drag
            .over
            .as_deref()
            .filter(|name| self.can_drop_on(name))
            .or(sprung.as_deref());
        Some(mapper::drag_hint(
            self.dragging().unwrap_or(0),
            target,
            self.modifiers.alt(),
            self.other_pane,
        ))
    }

    /// What dropping the other pane's drag here would do, while it is over
    /// this list.
    fn incoming_hint(&self) -> Option<String> {
        let incoming = self.incoming.as_ref().filter(|incoming| incoming.inside)?;
        let folder = incoming.over.as_deref().filter(|name| self.is_folder(name));
        Some(mapper::incoming_hint(
            incoming.count,
            folder,
            self.modifiers.command(),
        ))
    }

    fn is_folder(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.name == name && entry.is_folder)
    }

    /// The folder row the other pane's drag is over: it is where a drop lands.
    fn incoming_over(&self, entry: &EntryResponse) -> bool {
        entry.is_folder
            && self.incoming.as_ref().is_some_and(|incoming| {
                incoming.inside && incoming.over.as_deref() == Some(entry.name.as_str())
            })
    }

    /// How many items this pane is dragging, once the drag has started.
    pub fn dragging(&self) -> Option<usize> {
        let drag = self.drag.as_ref().filter(|drag| drag.active)?;
        Some(
            drag.source
                .as_ref()
                .map_or(self.selection.len(), |source| source.names.len()),
        )
    }

    /// What the drag in progress carries, from where: the selection it
    /// started with, even after a folder sprang open in this pane.
    pub fn dragged(&self) -> Option<Selection> {
        let drag = self.drag.as_ref().filter(|drag| drag.active)?;
        drag.source.clone().or_else(|| self.selection())
    }

    /// Where the drag in progress comes from (volume, folder), without
    /// copying what it carries: read on every redraw while a drag is on.
    pub fn dragged_from(&self) -> Option<(u64, &str)> {
        let drag = self.drag.as_ref().filter(|drag| drag.active)?;
        match &drag.source {
            Some(source) => Some((source.volume_id, source.path.as_str())),
            None => self
                .location
                .as_ref()
                .map(|location| (location.volume.id, location.path.as_str())),
        }
    }

    /// A drag is held over a folder that will open: the app sends beats.
    pub fn spring_pending(&self) -> bool {
        self.spring.is_some()
    }

    /// Space while dragging: opens the folder the drag is over at once.
    pub fn spring_now(&mut self) -> Task<Message> {
        match self.spring.clone() {
            Some((name, _)) => self.spring_into(&name),
            None => Task::none(),
        }
    }

    /// Opens the folder `name` under a drag, which goes on: the pane shows it
    /// and the drag can be dropped in it or held over a folder inside.
    fn spring_into(&mut self, name: &str) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        let path = mapper::child_path(&location.path, name);
        self.spring_origin
            .get_or_insert_with(|| location.path.clone());
        let drag = self.drag.take();
        let task = self.go(path);
        self.drag = drag.map(|drag| Drag {
            inside: true,
            ..drag
        });
        self.forget_pointer();
        task
    }

    /// Opens `path` under a drag held over the sidebar's spot for it: this
    /// pane's own drag goes on as after a folder sprang open, and the other
    /// pane's drag can then be dropped in it; if the drag ends anywhere but
    /// in this pane, it goes back. Nothing when this pane has no part in a
    /// drag, or shows `path` already.
    pub fn spring_to(&mut self, path: String) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        let dragging = self.dragging().is_some() || self.incoming.is_some();
        if !dragging || location.path == path {
            return Task::none();
        }
        self.spring_origin
            .get_or_insert_with(|| location.path.clone());
        let drag = self.drag.take();
        let task = self.go(path);
        self.drag = drag;
        self.forget_pointer();
        task
    }

    /// Goes back to where the pane was before a drag opened folders in it.
    fn spring_back(&mut self) -> Task<Message> {
        self.spring = None;
        match self.spring_origin.take() {
            Some(origin) => self.go(origin),
            None => Task::none(),
        }
    }

    /// The other pane is dragging `count` items (or stopped: `None`; `dropped`
    /// when the button came up rather than the drag being called off). Rows
    /// then say what the pointer is over, so a drop can land on a folder.
    pub fn set_incoming(&mut self, count: Option<usize>, dropped: bool) -> Task<Message> {
        match (count, &mut self.incoming) {
            (None, Some(_)) => {
                let ended = self.incoming.take();
                self.spring = None;
                // Let go over this pane: it was dropped here, where the pane
                // shows now. Called off, or let go elsewhere: folders it
                // opened close.
                if dropped && ended.is_some_and(|incoming| incoming.inside) {
                    self.spring_origin = None;
                } else {
                    return self.spring_back();
                }
            }
            (None, None) => {}
            (Some(count), Some(incoming)) => incoming.count = count,
            (Some(count), None) => {
                self.incoming = Some(Incoming {
                    count,
                    inside: false,
                    over: None,
                })
            }
        }
        Task::none()
    }

    /// Whether the other pane is shown, so a drag can be dropped there.
    pub fn set_other_pane(&mut self, shown: bool) {
        self.other_pane = shown;
    }

    /// Where the other pane's drag would land if let go now: the folder row
    /// it is over, else the folder shown; `None` while it is not over this
    /// list.
    pub fn drop_into(&self) -> Option<String> {
        let incoming = self.incoming.as_ref().filter(|incoming| incoming.inside)?;
        let location = self.location.as_ref()?;
        Some(
            match incoming.over.as_deref().filter(|name| self.is_folder(name)) {
                Some(name) => mapper::child_path(&location.path, name),
                None => location.path.clone(),
            },
        )
    }

    /// How far a lasso held past the top (negative) or bottom of the list
    /// would scroll it in one beat; `None` when it isn't, or can't go further.
    pub fn lasso_step(&self) -> Option<f32> {
        let lasso = self.lasso.as_ref()?;
        // Before the list has a size, nothing is past its edge.
        if lasso.size.height <= 0.0 {
            return None;
        }
        let y = lasso.pointer.y;
        let past = if y < 0.0 {
            y
        } else if y > lasso.size.height {
            y - lasso.size.height
        } else {
            return None;
        };
        let step = (past / 3.0).clamp(-LASSO_STEP, LASSO_STEP);
        let step = if step < 0.0 {
            step.min(-2.0)
        } else {
            step.max(2.0)
        };
        let end = (self.content_height(lasso.size.width) - lasso.size.height).max(0.0);
        let to = (self.viewport.0 + step).clamp(0.0, end);
        ((to - self.viewport.0).abs() > 0.01).then_some(to - self.viewport.0)
    }

    /// The height of everything in a list `width` wide, scrolled away or not
    /// (the grid's columns follow the width, as the lasso's do).
    fn content_height(&self, width: f32) -> f32 {
        if !self.groups.is_empty() {
            return match self.view {
                dto::ViewMode::List => {
                    groups::list_lines(&self.groups, self.rows.len()) as f32 * self.row_height()
                }
                dto::ViewMode::Grid => self.grid_frame(width).total(),
            };
        }
        match self.view {
            dto::ViewMode::List => self.rows.len() as f32 * self.row_height(),
            dto::ViewMode::Grid => {
                self.rows.len().div_ceil(grid::columns(width, self.tile)) as f32
                    * self.tile.row_height()
            }
        }
    }

    /// The grouped grid's lines for a list `width` wide.
    fn grid_frame(&self, width: f32) -> groups::GridFrame {
        groups::GridFrame::new(
            &self.groups,
            grid::columns(width, self.tile),
            self.tile.row_height(),
            GRID_HEADER,
        )
    }

    /// The rows a lasso from `a` to `b` (content coordinates) touches while
    /// grouped: the lines it crosses, headers left out; in the grid, the
    /// tiles of those lines within its columns.
    fn grouped_touched(&self, a: Point, b: Point, width: f32) -> std::collections::HashSet<usize> {
        let mut touched = std::collections::HashSet::new();
        match self.view {
            dto::ViewMode::List => {
                let lines = groups::list_lines(&self.groups, self.rows.len());
                for line in window::rows_between(a.y, b.y, self.row_height(), lines) {
                    if let groups::Line::Row(row) = groups::list_line(&self.groups, line) {
                        touched.insert(row);
                    }
                }
            }
            dto::ViewMode::Grid => {
                let columns = grid::columns(width, self.tile);
                let frame = self.grid_frame(width);
                let pitch = self.tile.pitch();
                let (left, right) = (a.x.min(b.x), a.x.max(b.x));
                let first = (left.max(0.0) / pitch).floor() as usize;
                let last =
                    ((right.max(0.0) / pitch).floor() as usize).min(columns.saturating_sub(1));
                let (top, bottom) = (a.y.min(b.y), a.y.max(b.y));
                let lines = if bottom < 0.0 || frame.lines == 0 {
                    0..0
                } else {
                    frame.line_at(top.max(0.0))..frame.line_at(bottom) + 1
                };
                for line in lines {
                    if let groups::GridLine::Tiles { first: from, end } =
                        groups::grid_line(&self.groups, columns, &frame.starts, line)
                    {
                        touched.extend((from..end).filter(|row| {
                            let column = (row - from) % columns.max(1);
                            column >= first && column <= last
                        }));
                    }
                }
            }
        }
        touched
    }

    /// A click on group `g`'s header: selects its rows (⇧ adds them, ⌘
    /// toggles them: all of them go if all were selected).
    fn select_group(&mut self, g: usize) -> Task<Message> {
        let Some(group) = self.groups.get(g) else {
            return Task::none();
        };
        let names: Vec<String> = self.rows[group.start..group.start + group.len]
            .iter()
            .map(|row| row.name.clone())
            .collect();
        let chosen: Vec<String> = if self.modifiers.command() {
            let all = names.iter().all(|name| self.selection.contains(name));
            let mut kept: Vec<String> = self
                .selection
                .names()
                .iter()
                .filter(|name| !all || !names.contains(name))
                .cloned()
                .collect();
            if !all {
                kept.extend(names);
            }
            kept
        } else if self.modifiers.shift() {
            let mut kept = self.selection.names().to_vec();
            kept.extend(names);
            kept
        } else {
            names
        };
        self.selection.set(&chosen);
        let rows = self.row_names();
        self.selection.sort_like(&rows);
        Task::none()
    }

    /// The left button went down over the list. What the press landed on
    /// was said just before (a name, a row) or nothing was.
    fn press_at(&mut self, at: Point, size: Size, taken: bool) -> Task<Message> {
        match self.pending_press.take() {
            // The name: the item may be dragged.
            Some(Pressed::Name) => return Task::none(),
            Some(Pressed::Row) => {}
            // Empty space: a lasso, which first clears the selection.
            None if !taken => {}
            // The scrollbar, or something else that wanted the press.
            None => return Task::none(),
        }
        if self.location.is_none() {
            return Task::none();
        }
        self.drag = None;
        let sweep = Sweep::from_keys(self.modifiers.command(), self.modifiers.shift());
        let base = match sweep {
            Sweep::Replace => std::collections::HashSet::new(),
            Sweep::Toggle | Sweep::Add => self.selection.names().iter().cloned().collect(),
        };
        self.lasso = Some(Sweeping {
            anchor: Point::new(at.x, at.y + self.viewport.0),
            pointer: at,
            size,
            base,
            sweep,
        });
        self.sweep_lasso();
        Task::none()
    }

    /// Selects what the lasso touches, combined with what was selected when
    /// it started. One pass over the rows.
    fn sweep_lasso(&mut self) {
        let Some(lasso) = &self.lasso else {
            return;
        };
        let a = lasso.anchor;
        let b = Point::new(lasso.pointer.x, lasso.pointer.y + self.viewport.0);
        let names = self.rows.iter().map(|row| row.name.as_str());
        let chosen = if self.groups.is_empty() {
            None
        } else {
            Some(self.grouped_touched(a, b, lasso.size.width))
        };
        let chosen = match (chosen, self.view) {
            (Some(touched), _) => rows::swept(
                names,
                |index| touched.contains(&index),
                &lasso.base,
                lasso.sweep,
            ),
            (None, dto::ViewMode::List) => {
                let touched = window::rows_between(a.y, b.y, self.row_height(), self.rows.len());
                rows::swept(
                    names,
                    |index| touched.contains(&index),
                    &lasso.base,
                    lasso.sweep,
                )
            }
            (None, dto::ViewMode::Grid) => {
                let columns = grid::columns(lasso.size.width, self.tile);
                let touched = grid::tiles_touched(
                    (a.x, a.y),
                    (b.x, b.y),
                    columns,
                    self.rows.len(),
                    self.tile,
                );
                rows::swept(
                    names,
                    |index| touched.binary_search(&index).is_ok(),
                    &lasso.base,
                    lasso.sweep,
                )
            }
        };
        self.selection.set(&chosen);
    }

    /// The lasso's box in the visible list's coordinates, once it is more
    /// than a click.
    fn lasso_box(&self) -> Option<Rectangle> {
        let lasso = self.lasso.as_ref()?;
        let a = Point::new(lasso.anchor.x, lasso.anchor.y - self.viewport.0);
        let b = lasso.pointer;
        let (width, height) = ((a.x - b.x).abs(), (a.y - b.y).abs());
        (width > 3.0 || height > 3.0).then(|| Rectangle {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            width,
            height,
        })
    }

    /// One beat of a lasso held past the edge: the list scrolls on and the
    /// box takes in what came into view.
    fn lasso_tick(&mut self) -> Task<Message> {
        let Some(step) = self.lasso_step() else {
            return Task::none();
        };
        let y = self.viewport.0 + step;
        self.viewport.0 = y;
        self.sweep_lasso();
        operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y })
    }

    /// What a row listens for: nothing, unless a button is down on one.
    fn pointer(&self) -> Pointer {
        if self.incoming.is_some() {
            return Pointer::Dragging;
        }
        match &self.drag {
            None => Pointer::Idle,
            Some(drag) if drag.active => Pointer::Dragging,
            Some(_) => Pointer::Armed,
        }
    }

    /// How a row or tile is drawn and what it listens for.
    fn look(&self, entry: &EntryResponse) -> Look {
        let dragging = self.drag.as_ref().is_some_and(|drag| drag.active);
        let selected = self.selection.contains(&entry.name);
        let over = self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.over.as_deref() == Some(entry.name.as_str()));
        Look {
            selected,
            // What was cut is drawn dim. (What is being dragged is not: the
            // banner says what it is, and grey on the selection is unreadable.)
            dim: self.is_cut(&entry.name),
            target: (dragging && entry.is_folder && !selected && over) || self.incoming_over(entry),
            pointer: self.pointer(),
        }
    }

    /// Whether `message` is about what this pane shows now: a thumbnail's
    /// word arriving after the pane moved on, or after a reconnect opened it
    /// afresh, is about a folder (and maybe a phone) it no longer shows.
    pub fn is_about_now(&self, message: &Message) -> bool {
        match message {
            Message::ThumbnailLost {
                volume_id, path, ..
            } => self
                .location
                .as_ref()
                .is_some_and(|location| location.volume.id == *volume_id && location.path == *path),
            _ => true,
        }
    }

    /// Whether `name` here is waiting to be moved.
    pub fn is_cut(&self, name: &str) -> bool {
        match (&self.cut, &self.location) {
            (Some(cut), Some(location)) => {
                cut.volume_id == location.volume.id
                    && cut.path == location.path
                    && cut.names.contains(name)
            }
            _ => false,
        }
    }

    /// How many items are selected, without copying their names.
    pub fn selected_count(&self) -> usize {
        self.selection.names().len()
    }

    /// The selected entries and where they are.
    pub fn selection(&self) -> Option<Selection> {
        let location = self.location.as_ref()?;
        if self.selection.is_empty() {
            return None;
        }
        Some(Selection {
            volume_id: location.volume.id,
            path: location.path.clone(),
            names: self.selection.names().to_vec(),
        })
    }

    #[cfg(test)]
    pub fn transfer_action(&self) -> Option<&'static str> {
        self.location
            .as_ref()
            .and_then(|location| location.volume.transfer_action)
    }

    pub fn volume_id(&self) -> Option<u64> {
        self.location.as_ref().map(|location| location.volume.id)
    }

    /// Shows or hides the button that copies to the other side, e.g. only
    /// while there is a phone to upload to.
    pub fn set_transfer_action(&mut self, label: Option<&'static str>) {
        if let Some(location) = &mut self.location {
            location.volume.transfer_action = label;
        }
    }

    pub fn current_path(&self) -> Option<&str> {
        self.location
            .as_ref()
            .map(|location| location.path.as_str())
    }

    /// Opens the folder `path` and, once it is read, selects `name` there and
    /// scrolls it into view.
    pub fn show_item(&mut self, path: String, name: String) -> Task<Message> {
        if self.location.is_none() {
            return Task::none();
        }
        let task = self.navigate(path);
        self.reveal = Some(name);
        task
    }

    /// Reloads the folder, selecting `name` once it shows up.
    pub fn refresh_and_select(&mut self, name: Option<String>) -> Task<Message> {
        if self.location.is_none() {
            return Task::none();
        }
        match name {
            Some(name) => self.selection.only(&name),
            None => self.selection.clear(),
        }
        // A transfer changed the folder: read it as it is now.
        self.load(false)
    }

    #[cfg(test)]
    pub fn view_mode(&self) -> dto::ViewMode {
        self.view
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Reloads the folder keeping the selection, e.g. after the phone said
    /// something changed.
    pub fn reload(&mut self) -> Task<Message> {
        // The phone said something changed; trust nothing remembered.
        self.load(false)
    }

    /// Reloads the folder after a move took files off the phone: the explorer
    /// kept what it remembers of the folder true as each one went, so this
    /// needn't ask the phone about every file left.
    pub fn reload_remembered(&mut self) -> Task<Message> {
        self.load(true)
    }

    /// Hidden files, folders first, status bar: what settings last applied.
    #[cfg(test)]
    pub fn listing_options(&self) -> (bool, bool, bool) {
        (self.show_hidden, self.sort.folders_first, self.status_bar)
    }

    /// Applies the hidden-files setting, reloading only if it changed.
    pub fn set_show_hidden(&mut self, show_hidden: bool) -> Task<Message> {
        if self.show_hidden == show_hidden {
            return Task::none();
        }
        self.show_hidden = show_hidden;
        // The same folder, shown differently: the remembered reading has it all.
        self.load(true)
    }

    /// The column the pane is sorted by and which way, to be remembered.
    pub fn sort_order(&self) -> dto::SortOrder {
        dto::SortOrder {
            key: self.sort.key,
            descending: self.sort.descending,
        }
    }

    /// Sorts the pane as remembered, re-sorting only if that changes it.
    pub fn set_sort_order(&mut self, order: dto::SortOrder) {
        if self.sort_order() != order {
            self.sort.key = order.key;
            self.sort.descending = order.descending;
            self.resort();
        }
    }

    pub fn set_folders_first(&mut self, folders_first: bool) {
        if self.sort.folders_first != folders_first {
            self.sort.folders_first = folders_first;
            self.resort();
        }
    }

    pub fn set_status_bar(&mut self, status_bar: bool) {
        self.status_bar = status_bar;
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    /// A dialog is open, so keys belong to it rather than to the list.
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    /// Go to Folder is open: ↑/↓ pick among the folders it offers.
    pub fn is_going_to(&self) -> bool {
        matches!(
            self.editor,
            Some(Editor {
                change: Change::GoTo(_),
                ..
            })
        )
    }

    pub fn has_menu(&self) -> bool {
        self.menu.is_some()
    }

    /// Where the right-click menu was opened, in window coordinates.
    pub fn menu_at(&self) -> Option<Point> {
        self.menu.as_ref().map(|menu| menu.at)
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
        self.menu_target = None;
    }

    /// What the menu offers here; `can_paste` comes from the app's clipboard.
    fn menu_context(&self, location: &Location, on_entry: bool, can_paste: bool) -> menu::Context {
        let single_folder = self.selection.single().is_some_and(|name| {
            self.entries
                .iter()
                .any(|entry| entry.name == name && entry.is_folder)
        });
        menu::Context {
            on_entry,
            selected: self.selection.names().len(),
            single_folder,
            transfer: location.volume.transfer_action,
            can_paste,
            on_mac: location.volume.shows_in_finder,
            deletes_permanently: location.volume.deletes_permanently,
            other_pane: self.other_pane,
            edit_on_mac: location
                .volume
                .phone_tools
                .then(|| self.single_file().is_some()),
        }
    }

    /// The one file selected, if the selection is exactly that. One lookup
    /// (the menu state asks after every message, whatever the folder holds).
    pub fn single_file(&self) -> Option<String> {
        let name = self.selection.single()?;
        let &index = self.by_name.get(name)?;
        (!self.entries[index].is_folder).then(|| name.to_owned())
    }

    /// What the folder holds, as listed.
    pub fn entries(&self) -> &[dto::Entry] {
        &self.entries
    }

    /// The folder is listed and readable: there is something to compare.
    pub fn is_listed(&self) -> bool {
        self.location.is_some() && !self.loading && self.error.is_none()
    }

    /// The folder shown, by name (a phone's top folder by its storage's).
    pub fn shown_name(&self) -> Option<String> {
        let location = self.location.as_ref()?;
        Some(if location.path == "/" {
            location.volume.name.clone()
        } else {
            mapper::folder_name(&location.path)
        })
    }

    /// Selects, among the rows shown, what the other pane's folder (`other`,
    /// called `other_name`) lacks or has (`want`), and says how many.
    pub fn select_against(
        &mut self,
        want: compare::Against,
        other: &[dto::Entry],
        other_name: &str,
    ) {
        let index = compare::Other::new(other);
        let picked = compare::pick(self.shown.iter().map(|&at| &self.entries[at]), &index, want);
        self.selection.set(&picked);
        self.notice = Some(mapper::against_notice(
            want,
            picked.len(),
            self.shown.len(),
            other_name,
        ));
    }

    /// The labels of the open menu's commands, section by section flattened.
    #[cfg(test)]
    pub fn menu_labels(&self, can_paste: bool) -> Vec<String> {
        let (Some(open), Some(location)) = (self.menu, self.location.as_ref()) else {
            return Vec::new();
        };
        menu::sections(&self.menu_context(location, open.on_entry, can_paste))
            .into_iter()
            .flatten()
            .map(|item| item.label)
            .collect()
    }

    /// The open right-click menu, placed in window coordinates; `app.rs`
    /// draws it above everything else.
    pub fn menu(&self, can_paste: bool) -> Option<Element<'_, Message>> {
        let (open, location) = (self.menu?, self.location.as_ref()?);
        let sections = menu::sections(&self.menu_context(location, open.on_entry, can_paste));
        let at = menu::place(open.at, menu::size(&sections), open.area);
        Some(
            pin(menu_box(sections))
                .position(at)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        )
    }

    /// Shows `start` on `volume`, or `fallback` if `start` can't be read:
    /// how a pane opens where it was left, which may have gone since.
    pub fn open_with_fallback(
        &mut self,
        volume: Volume,
        start: String,
        fallback: String,
    ) -> Task<Message> {
        let task = self.open(volume, start);
        self.fallback = Some(fallback);
        task
    }

    /// Shows `start` on `volume`. Back and forward start again: another
    /// volume's paths name other folders.
    pub fn open(&mut self, volume: Volume, start: String) -> Task<Message> {
        self.history.clear();
        self.fallback = None;
        self.editor = None;
        self.drag = None;
        self.spring = None;
        self.spring_origin = None;
        self.notice = None;
        self.close_menu();
        self.location = Some(Location {
            volume,
            path: start.clone(),
        });
        self.navigate(start)
    }

    pub fn close(&mut self) {
        self.location = None;
        self.history.clear();
        self.lasso = None;
        self.filter.clear();
        self.filter_kind = None;
        self.filter_date = None;
        self.forget_sizes();
        self.set_entries(Vec::new());
        self.selection.clear();
        self.stop_listing();
        self.loading = false;
        self.error = None;
        self.editor = None;
        self.pasting = None;
        self.notice = None;
        self.cut = None;
        self.drag = None;
        self.spring = None;
        self.spring_origin = None;
        self.close_menu();
    }

    /// Stops reading a folder the pane no longer wants.
    fn stop_listing(&mut self) {
        if let Some(cancel) = self.listing.take() {
            cancel.cancel();
        }
        self.progress = None;
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(request) => self.handle(request),
            Message::DeleteCounted { names, bytes } => {
                if let Some(Editor {
                    change:
                        Change::Delete {
                            names: asked,
                            keep: keep @ Keep::Counting,
                        },
                    ..
                }) = &mut self.editor
                    && *asked == names
                {
                    *keep = match bytes {
                        Some(bytes) if bytes <= dto::KEEP_MOST => Keep::Copy(bytes),
                        Some(bytes) => Keep::TooMuch(bytes),
                        None => Keep::Unknown,
                    };
                }
                Task::none()
            }
            // `app.rs` queues it.
            Message::DeleteKeepingCopy(_) => Task::none(),
            // `app.rs` opens the file.
            Message::OpenFile(_) => Task::none(),
            Message::Listed {
                volume_id,
                path,
                result,
            } => {
                // Ignore answers for a folder the user already left.
                let is_current = self.location.as_ref().is_some_and(|location| {
                    location.volume.id == volume_id && location.path == path
                });
                if !is_current {
                    return Task::none();
                }

                self.loading = false;
                self.progress = None;
                self.listing = None;
                // The folder the pane was opened at can't be read now (gone,
                // or refused): it goes to the fallback instead, once.
                let fallback = self.fallback.take();
                if result.is_err()
                    && let Some(fallback) = fallback
                    && fallback != path
                {
                    return self.go(fallback);
                }
                let mut shown = Task::none();
                match result {
                    Ok(entries) => {
                        self.error = None;
                        self.set_entries(entries);
                        let names = self.row_names();
                        self.selection.retain(&names);
                        let present: std::collections::HashSet<&String> = names.iter().collect();
                        self.thumbnails.retain(|name, _| present.contains(name));
                        if let Some(name) = self.reveal.take()
                            && present.contains(&name)
                        {
                            self.selection.only(&name);
                            shown = self.scroll_to_cursor();
                        }
                    }
                    Err(error) => {
                        self.reveal = None;
                        self.set_entries(Vec::new());
                        self.error = Some(error);
                    }
                }
                Task::batch([shown, self.request_thumbnails(), self.count_sizes()])
            }
            Message::Loading {
                volume_id,
                path,
                entries,
                loaded,
                total,
            } => {
                let is_current = self.loading
                    && self.location.as_ref().is_some_and(|location| {
                        location.volume.id == volume_id && location.path == path
                    });
                if is_current {
                    self.progress = Some((loaded, total));
                    self.set_entries(entries);
                }
                Task::none()
            }
            Message::ShowMenu { at, area } => {
                let target = self.menu_target.take();
                if self.location.is_none() || self.editor.is_some() {
                    return Task::none();
                }
                // Like Finder, a right-click on empty space drops the selection.
                if target.is_none() {
                    self.selection.clear();
                }
                self.menu = Some(OpenMenu {
                    at,
                    area,
                    on_entry: target.is_some(),
                });
                Task::none()
            }
            Message::Scrolled(viewport) => {
                self.forget_pointer();
                let bounds = viewport.bounds();
                self.viewport = (viewport.absolute_offset().y, bounds.height, bounds.width);
                // Scrolled under a lasso: the box now reaches other rows.
                self.sweep_lasso();
                self.request_thumbnails()
            }
            Message::PressedAt { at, size, taken } => self.press_at(at, size, taken),
            Message::LassoMoved { at, size } => {
                if let Some(lasso) = &mut self.lasso {
                    lasso.pointer = at;
                    lasso.size = size;
                    self.sweep_lasso();
                }
                Task::none()
            }
            Message::ResizeMoved(x) => {
                if let Some(resizing) = &mut self.resizing {
                    let from = *resizing.anchor.get_or_insert(x);
                    let fit = layout::for_width(resizing.width, resizing.start);
                    self.columns = layout::resized(
                        resizing.start,
                        resizing.edge,
                        x - from,
                        resizing.width,
                        fit,
                    );
                }
                Task::none()
            }
            Message::LassoTick => self.lasso_tick(),
            Message::Hover(inside) => {
                if let Some(incoming) = &mut self.incoming {
                    incoming.inside = inside;
                    if !inside {
                        incoming.over = None;
                    }
                }
                if let Some(drag) = &mut self.drag {
                    drag.inside = inside;
                }
                if !inside {
                    self.spring = None;
                }
                Task::none()
            }
            Message::Thumbnail {
                volume_id,
                path,
                name,
                picture,
            } => {
                let is_current = self.location.as_ref().is_some_and(|location| {
                    location.volume.id == volume_id && location.path == path
                });
                if is_current {
                    let thumb = picture.map_or(Thumb::None, Thumb::Ready);
                    self.thumbnails.insert(name, thumb);
                    self.trim_thumbnails();
                }
                Task::none()
            }
            // `app.rs` sees the phone is lost; the pane is closed with it.
            Message::ThumbnailLost { .. } => Task::none(),
            // The app has queued the copy through the Mac; the pane has nothing
            // more to wait for.
            Message::PasteViaMac { .. } => {
                self.pasting = None;
                match self.grouping.take() {
                    // The items go in later, by the queue; the folder is there.
                    Some((folder, name)) => self.name_the_group(&folder, &name),
                    None => Task::none(),
                }
            }
            Message::Pasted {
                volume_id,
                path,
                result,
            } => {
                self.pasting = None;
                // One paste at a time: this is the move into the new folder.
                if let Some((folder, name)) = self.grouping.take() {
                    return self.grouped(volume_id, folder, name, result);
                }
                let is_current = self.location.as_ref().is_some_and(|location| {
                    location.volume.id == volume_id && location.path == path
                });
                // The banner is about what the user asked for, wherever they
                // are now; the folder only needs reading if it is the one shown.
                if let Ok(response) = &result
                    && !response.select.is_empty()
                {
                    self.changed(volume_id, response.undo.clone());
                }
                let response = result.unwrap_or_else(|error| mapper::to_paste_failure(&error));
                let mut notice = response.notice;
                // A drop on a folder, or a paste that ended after the user went
                // elsewhere: say where it went.
                if !is_current && !response.select.is_empty() {
                    notice.title = tf(
                        "explorer.paste.to_folder",
                        &[
                            ("title", &notice.title),
                            ("folder", &mapper::folder_name(&path)),
                        ],
                    );
                }
                self.notice = Some(notice);
                if response.select.is_empty() {
                    return Task::none();
                }
                // What was pasted is selected where it landed; wherever it
                // landed, what is shown may have changed (a move empties it).
                if is_current {
                    self.selection.set(&response.select);
                }
                // The service kept what it remembered of both folders in
                // step with the paste, so this needn't read every file.
                self.load(true)
            }
            Message::GroupMade {
                volume_id,
                path,
                names,
                result,
            } => {
                self.pasting = None;
                match result {
                    Ok(name) => {
                        let into = mapper::child_path(&path, &name);
                        self.grouping = Some((path.clone(), name));
                        self.paste_into(
                            dto::Paste::Move,
                            vec![Selection {
                                volume_id,
                                path,
                                names,
                            }],
                            into,
                        )
                    }
                    Err(error) => {
                        self.notice = Some(NoticeResponse {
                            title: error.title,
                            details: vec![error.hint],
                            problem: true,
                        });
                        Task::none()
                    }
                }
            }
            Message::SpringTick(now) => {
                let Some((name, since)) = self.spring.clone() else {
                    return Task::none();
                };
                if now.saturating_duration_since(since) < SPRING_DELAY {
                    return Task::none();
                }
                self.spring_into(&name)
            }
            Message::FolderSized { run, name, result } => {
                if run != self.size_run {
                    return Task::none();
                }
                self.sizing = None;
                match result {
                    Ok(tally) => {
                        self.sizes.insert(name.clone(), FolderSize::Known(tally));
                    }
                    Err(error) => {
                        self.sizes.remove(&name);
                        if error.device_lost {
                            self.size_queue.clear();
                        }
                    }
                }
                if self.sort.key == dto::SortKey::Size {
                    self.resort();
                } else {
                    self.show_size(&name);
                }
                self.next_size()
            }
            Message::FolderNames { parent, names } => {
                if let Some(Editor {
                    change: Change::GoTo(go),
                    name,
                    ..
                }) = &mut self.editor
                    && go.parent == parent
                {
                    go.names = names;
                    go.offer(name);
                }
                Task::none()
            }
            Message::RenamedSome(done) => {
                if let Some(Editor {
                    change: Change::RenameMany(many),
                    ..
                }) = &mut self.editor
                {
                    many.done = Some(done);
                }
                Task::none()
            }
            Message::RenamedMany {
                volume_id,
                path,
                outcome,
                asked,
            } => {
                self.renaming = None;
                if !outcome.done.is_empty() {
                    self.changed(
                        volume_id,
                        Some(Undo::RenamedMany {
                            folder: path.clone(),
                            renames: outcome.done.clone(),
                        }),
                    );
                }
                let is_current = self.location.as_ref().is_some_and(|location| {
                    location.volume.id == volume_id && location.path == path
                });
                if matches!(
                    self.editor,
                    Some(Editor {
                        change: Change::RenameMany(_),
                        ..
                    })
                ) {
                    self.editor = None;
                }
                self.notice = Some(rename_many::notice(&outcome, asked));
                if !is_current {
                    return Task::none();
                }
                // Everything that was selected stays so, under its new name.
                let mut select: Vec<String> =
                    outcome.done.iter().map(|(_, now)| now.clone()).collect();
                select.extend(outcome.failures.iter().map(|(name, _)| name.clone()));
                self.selection.set(&select);
                self.load(true)
            }
            Message::Undone {
                volume_id,
                what,
                result,
            } => match result {
                Ok((folder, select)) => {
                    self.notice = Some(NoticeResponse {
                        title: t(what).to_owned(),
                        details: Vec::new(),
                        problem: false,
                    });
                    let here = self.location.as_ref().is_some_and(|location| {
                        location.volume.id == volume_id && location.path == folder
                    });
                    if !here {
                        return Task::none();
                    }
                    self.selection.set(&select);
                    self.load(true)
                }
                Err(error) => {
                    self.notice = Some(NoticeResponse {
                        title: error.title,
                        details: vec![error.hint],
                        problem: true,
                    });
                    Task::none()
                }
            },
            Message::Edited {
                volume_id,
                path,
                result,
                undo,
            } => {
                if result.is_ok() {
                    self.changed(volume_id, undo);
                }
                let is_current = self.location.as_ref().is_some_and(|location| {
                    location.volume.id == volume_id && location.path == path
                });
                if !is_current {
                    return Task::none();
                }
                match result {
                    Ok(select) => {
                        self.editor = None;
                        match select {
                            Some(name) => self.selection.only(&name),
                            None => self.selection.clear(),
                        }
                        // The service kept what it remembered of the folder in
                        // step with the edit, so this needn't read every file.
                        self.load(true)
                    }
                    Err(error) => {
                        if let Some(editor) = &mut self.editor {
                            editor.busy = false;
                            editor.error = Some(error);
                        }
                        Task::none()
                    }
                }
            }
        }
    }

    fn handle(&mut self, request: Request) -> Task<Message> {
        // A press is said by its row and then by the list; any other request
        // in between means the list's word is about something else.
        self.pending_press = None;
        let Some(location) = &self.location else {
            return Task::none();
        };

        match request {
            Request::PressRow(name) => {
                self.pending_press = Some(Pressed::Row);
                // ⇧ selects up to here first, as a ⇧-click does, and the box
                // adds to that.
                if self.modifiers.shift() && !self.modifiers.command() {
                    let names = self.row_names();
                    self.selection.click(&names, &name, Click::Range);
                }
                Task::none()
            }
            Request::Select(name) => {
                self.pending_press = Some(Pressed::Name);
                let plain = !self.modifiers.command() && !self.modifiers.shift();
                // Only a pane that can move things inside itself has anywhere
                // to drag to.
                let can_drag = location.volume.pastes_within;
                // Pressing on one of several selected rows must not narrow the
                // selection yet there: it may be the start of dragging all of
                // them. (Where nothing can be dragged, it narrows at once.)
                let narrow_on_release =
                    can_drag && plain && self.selection.len() > 1 && self.selection.contains(&name);
                if !narrow_on_release {
                    let click = if self.modifiers.command() {
                        Click::Toggle
                    } else if self.modifiers.shift() {
                        Click::Range
                    } else {
                        Click::Only
                    };
                    let names = self.row_names();
                    self.selection.click(&names, &name, click);
                }
                self.drag = can_drag.then(|| Drag {
                    over: Some(name.clone()),
                    pressed: name,
                    active: false,
                    narrow_on_release,
                    source: None,
                    inside: true,
                });
                Task::none()
            }
            Request::PointerEntered(name) => {
                if let Some(incoming) = &mut self.incoming {
                    incoming.over = Some(name.clone());
                }
                if let Some(drag) = &mut self.drag {
                    drag.over = Some(name.clone());
                }
                // A drag held over a folder starts its clock; another row
                // starts it again.
                // Another pane's drag may go into any folder here; this
                // pane's own, not into what it is carrying.
                let can_open = if self.incoming.is_some() {
                    self.is_folder(&name)
                } else {
                    self.drag.as_ref().is_some_and(|drag| drag.active) && self.can_drop_on(&name)
                };
                if self.spring.as_ref().map(|(over, _)| over) != Some(&name) {
                    self.spring = can_open.then(|| (name, Instant::now()));
                }
                Task::none()
            }
            Request::PointerLeft(name) => {
                if let Some(incoming) = &mut self.incoming
                    && incoming.over.as_deref() == Some(name.as_str())
                {
                    incoming.over = None;
                }
                if self.spring.as_ref().is_some_and(|(over, _)| *over == name) {
                    self.spring = None;
                }
                let source = self.selection();
                if let Some(drag) = &mut self.drag {
                    if drag.over.as_deref() == Some(name.as_str()) {
                        drag.over = None;
                    }
                    // Out of the row the button went down on, button still
                    // down: it is a drag, not a click.
                    if drag.pressed == name && !drag.active {
                        drag.active = true;
                        drag.source = source;
                    }
                }
                Task::none()
            }
            Request::PointerReleased => {
                self.resizing = None;
                self.release()
            }
            Request::ResizeColumn(edge, width) => {
                self.resizing = Some(Resizing {
                    edge,
                    start: self.columns,
                    anchor: None,
                    width: f32::from(width),
                });
                Task::none()
            }
            Request::ResetColumns => {
                self.resizing = None;
                self.columns = dto::ColumnWidths::default();
                Task::none()
            }
            Request::SelectGroup(g) => self.select_group(g),
            Request::SelectAll => {
                let names = self.row_names();
                self.selection.all(&names);
                Task::none()
            }
            Request::Step { delta, .. }
                if matches!(
                    self.editor,
                    Some(Editor {
                        change: Change::GoTo(_),
                        ..
                    })
                ) =>
            {
                self.choose_suggestion(delta);
                Task::none()
            }
            Request::Step { delta, extend } => {
                // In the grid, up and down move a whole row.
                let delta = match self.view {
                    dto::ViewMode::List => delta,
                    dto::ViewMode::Grid => delta * self.columns() as isize,
                };
                let names = self.row_names();
                self.selection.step(&names, delta, extend);
                self.scroll_to_cursor()
            }
            Request::Jump { to, extend } => {
                let names = self.row_names();
                match to {
                    Jump::First => self.selection.go_to(&names, 0, extend),
                    Jump::Last => self.selection.go_to(&names, usize::MAX, extend),
                    Jump::PageUp => self.selection.step(&names, -self.page(), extend),
                    Jump::PageDown => self.selection.step(&names, self.page(), extend),
                }
                self.scroll_to_cursor()
            }
            Request::Sideways { delta, extend } => {
                if self.view != dto::ViewMode::Grid {
                    return Task::none();
                }
                let names = self.row_names();
                self.selection.step(&names, delta, extend);
                self.scroll_to_cursor()
            }
            // Saved as a setting by `app.rs`, which then calls `set_view`.
            Request::SetView(_) => Task::none(),
            Request::FocusFilter => operation::focus(self.filter_input.clone()),
            Request::Filter(_) | Request::FilterKind(_) | Request::FilterDate(_) => {
                match request {
                    Request::Filter(text) => self.filter = text,
                    Request::FilterKind(kind) => self.filter_kind = kind,
                    Request::FilterDate(date) => self.filter_date = date,
                    _ => {}
                }
                self.apply_filter();
                self.viewport.0 = 0.0;
                Task::batch([
                    operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y: 0.0 }),
                    self.request_thumbnails(),
                ])
            }
            Request::SubmitFilter => {
                let only = (self.rows.len() == 1).then(|| self.rows[0].name.clone());
                let chosen = self.selection.single().map(str::to_owned);
                match only.or(chosen) {
                    Some(name) => self.handle(Request::Open(name)),
                    None => match self.rows.first() {
                        Some(first) => {
                            let first = first.name.clone();
                            self.selection.only(&first);
                            self.scroll_to_cursor()
                        }
                        None => Task::none(),
                    },
                }
            }
            Request::OpenSelected => match self.selection.single() {
                Some(name) => {
                    let name = name.to_owned();
                    self.handle(Request::Open(name))
                }
                None => Task::none(),
            },
            Request::Escape => {
                if self.lasso.take().is_some() {
                    // The box goes; what it selected stays, as in Finder.
                } else if self.drag.as_ref().is_some_and(|drag| drag.active) {
                    // Changed their mind mid-drag: nothing is dropped. (A press
                    // that has not started dragging is not in the way of the
                    // rest of what Escape does.) Folders it opened close.
                    self.drag = None;
                    return self.spring_back();
                } else if self.menu.is_some() {
                    self.close_menu();
                } else if self.editor.as_ref().is_some_and(|editor| !editor.busy) {
                    self.editor = None;
                } else if self.editor.is_none() && self.is_filtering() {
                    // The filter goes first, as Finder's search does.
                    self.filter.clear();
                    self.filter_kind = None;
                    self.filter_date = None;
                    self.apply_filter();
                } else if self.editor.is_none() {
                    self.selection.clear();
                }
                Task::none()
            }
            Request::Open(name) => {
                let Some(entry) = self.entries.iter().find(|entry| entry.name == name) else {
                    return Task::none();
                };
                if !entry.is_folder {
                    return Task::done(Message::OpenFile(name));
                }
                let path = mapper::child_path(&location.path, &name);
                self.navigate(path)
            }
            Request::GoTo(path) => self.navigate(path),
            Request::Up => match mapper::parent_path(&location.path) {
                Some(parent) => self.navigate(parent),
                None => Task::none(),
            },
            Request::TypeAhead { text, at } => self.type_ahead(&text, at),
            Request::Back => match self.history.back(&location.path) {
                Some(path) => self.go(path),
                None => Task::none(),
            },
            Request::Forward => match self.history.forward(&location.path) {
                Some(path) => self.go(path),
                None => Task::none(),
            },
            // The user asked to see the phone as it is now.
            Request::Refresh => self.load(false),
            Request::OpenPrivacySettings(pane) => {
                self.service.open_privacy_settings(pane);
                Task::none()
            }
            Request::SetShowHidden(show_hidden) => {
                self.show_hidden = show_hidden;
                self.load(true)
            }
            Request::SortBy(key) => {
                self.sort = mapper::next_sort(self.sort, key);
                self.resort();
                Task::none()
            }
            Request::SetFoldersFirst(folders_first) => {
                self.sort.folders_first = folders_first;
                self.resort();
                Task::none()
            }
            // The phone is busy with a paste; edits wait their turn.
            Request::NewFolder | Request::Rename | Request::Delete if self.pasting.is_some() => {
                Task::none()
            }
            Request::NewFolder => {
                self.start(Change::NewFolder, mapper::new_folder_name().to_owned());
                Task::batch([
                    operation::focus(self.name_input.clone()),
                    operation::select_all(self.name_input.clone()),
                ])
            }
            Request::CopyPath => {
                if self.selection.is_empty() {
                    return Task::none();
                }
                let names = self.selection.names();
                let paths: Vec<String> = names
                    .iter()
                    .map(|name| mapper::child_path(&location.path, name))
                    .collect();
                // A name from the phone (or the Mac), the item's or a folder's
                // above it, can hold a line break: pasted, that would be a line
                // more than was copied, maybe a command. Nothing is copied
                // then, and the banner says why.
                if let Some(odd) = paths.iter().find(|path| mapper::has_control(path)) {
                    self.notice = Some(NoticeResponse {
                        title: t("explorer.paths_not_copied").to_owned(),
                        details: vec![mapper::odd_name_hint(odd)],
                        problem: true,
                    });
                    return Task::none();
                }
                self.notice = Some(NoticeResponse {
                    title: mapper::copied_paths(names),
                    details: Vec::new(),
                    problem: false,
                });
                iced::clipboard::write(paths.join("\n"))
            }
            Request::NewFolderWithItems => {
                if self.selection.is_empty() || !location.volume.pastes_within {
                    return Task::none();
                }
                let name = mapper::free_folder_name(
                    mapper::folder_with_items(),
                    self.entries.iter().map(|entry| entry.name.as_str()),
                );
                let names = self.selection.names().to_vec();
                let (service, volume_id, path) = (
                    Arc::clone(&self.service),
                    location.volume.id,
                    location.path.clone(),
                );
                self.pasting = Some(dto::Paste::Move);
                Task::perform(
                    async move {
                        let result = service
                            .make_folder(volume_id, &path, &name)
                            .await
                            .map_err(error::to_error_response);
                        (path, result)
                    },
                    move |(path, result)| Message::GroupMade {
                        volume_id,
                        path,
                        names: names.clone(),
                        result,
                    },
                )
            }
            Request::GoToFolder => {
                let typed = match location.path.as_str() {
                    "/" => "/".to_owned(),
                    path => format!("{path}/"),
                };
                self.start(
                    Change::GoTo(GoTo {
                        parent: String::new(),
                        names: Vec::new(),
                        offered: Vec::new(),
                        chosen: None,
                    }),
                    typed,
                );
                let names = self.look_for_folders();
                Task::batch([
                    names,
                    operation::focus(self.name_input.clone()),
                    operation::move_cursor_to_end(self.name_input.clone()),
                ])
            }
            Request::ChooseFolder(name) => {
                let Some(editor) = &mut self.editor else {
                    return Task::none();
                };
                editor.name = go_to::complete(&editor.name, &name);
                if let Change::GoTo(go) = &mut editor.change {
                    go.chosen = None;
                }
                let names = self.look_for_folders();
                Task::batch([
                    names,
                    operation::focus(self.name_input.clone()),
                    operation::move_cursor_to_end(self.name_input.clone()),
                ])
            }
            Request::Rename if self.selection.len() > 1 => {
                // In the order shown, which is what the numbers follow.
                let items: Vec<(String, bool)> = self
                    .shown
                    .iter()
                    .map(|&at| &self.entries[at])
                    .filter(|entry| self.selection.contains(&entry.name))
                    .map(|entry| (entry.name.clone(), entry.is_folder))
                    .collect();
                let folder = self
                    .entries
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect();
                let mut many = Many::new(items, folder);
                many.replan(self.service.as_ref());
                self.start(Change::RenameMany(Box::new(many)), String::new());
                operation::focus(self.name_input.clone())
            }
            Request::Rename => {
                let Some(original) = self.selection.single().map(str::to_owned) else {
                    return Task::none();
                };
                let stem = mapper::stem_len(&original);
                self.start(
                    Change::Rename {
                        original: original.clone(),
                    },
                    original,
                );
                // Like Finder: the name is selected, the extension is not.
                Task::batch([
                    operation::focus(self.name_input.clone()),
                    operation::select_range(self.name_input.clone(), 0, stem),
                ])
            }
            Request::Delete => {
                if self.selection.is_empty() {
                    return Task::none();
                }
                let names = self.selection.names().to_vec();
                let confirm = location.volume.deletes_permanently;
                let (volume_id, path) = (location.volume.id, location.path.clone());
                let keep = if confirm && self.keep_deleted {
                    Keep::Counting
                } else {
                    Keep::No
                };
                self.start(
                    Change::Delete {
                        names: names.clone(),
                        keep,
                    },
                    String::new(),
                );
                if !confirm {
                    return self.submit();
                }
                if keep == Keep::Counting {
                    return self.count_for_keeping(volume_id, &path, names);
                }
                Task::none()
            }
            Request::EditName(name) => {
                if let Some(editor) = &mut self.editor
                    && !editor.busy
                {
                    editor.name = name;
                    editor.error = None;
                    if let Change::GoTo(go) = &mut editor.change {
                        go.chosen = None;
                        return self.look_for_folders();
                    }
                }
                Task::none()
            }
            Request::RenameMode(_) | Request::RenameField(..) | Request::RenameAtEnd(_) => {
                if let Some(Editor {
                    change: Change::RenameMany(many),
                    busy: false,
                    ..
                }) = &mut self.editor
                {
                    match request {
                        Request::RenameMode(mode) => many.mode = mode,
                        Request::RenameField(field, value) => many.set(field, value),
                        Request::RenameAtEnd(at_end) => many.at_end = at_end,
                        _ => {}
                    }
                    many.replan(self.service.as_ref());
                }
                Task::none()
            }
            Request::Submit => self.submit(),
            // Carried out by `app.rs`, which knows both panes.
            Request::Transfer | Request::Search | Request::Mirror => Task::none(),
            Request::MenuOn(name) => {
                if self.editor.is_none() {
                    if !self.selection.contains(&name) {
                        self.selection.only(&name);
                    }
                    self.menu_target = Some(name);
                }
                Task::none()
            }
            Request::CloseMenu => {
                self.close_menu();
                Task::none()
            }
            Request::DismissNotice => {
                self.notice = None;
                Task::none()
            }
            Request::Menu(action) => {
                self.close_menu();
                self.choose(action)
            }
            Request::Cancel => {
                if self.editor.as_ref().is_some_and(|editor| !editor.busy) {
                    self.editor = None;
                } else if let Some(cancel) = &self.renaming {
                    // Stops between renames; what is done stays done.
                    cancel.cancel();
                }
                Task::none()
            }
        }
    }

    /// Carries out a menu command that stays within this pane; the rest are
    /// `app.rs`'s (`Action::is_app_wide`).
    fn choose(&mut self, action: menu::Action) -> Task<Message> {
        let request = match action {
            menu::Action::Open => Request::OpenSelected,
            menu::Action::NewFolder => Request::NewFolder,
            menu::Action::NewFolderWithItems => Request::NewFolderWithItems,
            menu::Action::CopyPath => Request::CopyPath,
            menu::Action::Rename => Request::Rename,
            menu::Action::Delete => Request::Delete,
            menu::Action::SelectAll => Request::SelectAll,
            menu::Action::Refresh => Request::Refresh,
            menu::Action::ShowInFinder => {
                if let Some(location) = &self.location
                    && location.volume.shows_in_finder
                    && let Some(name) = self.selection.names().first()
                {
                    self.service
                        .reveal(&mapper::child_path(&location.path, name));
                }
                return Task::none();
            }
            menu::Action::Transfer
            | menu::Action::Cut
            | menu::Action::Copy
            | menu::Action::CopyToQueue
            | menu::Action::Paste
            | menu::Action::Pin
            | menu::Action::PinHere
            | menu::Action::GetInfo
            | menu::Action::GetInfoHere
            | menu::Action::Share
            | menu::Action::SelectNotInOther
            | menu::Action::SelectAlsoInOther
            | menu::Action::CheckAgainstOther
            | menu::Action::EditOnMac => {
                return Task::none();
            }
        };
        self.handle(request)
    }

    /// The selection's move into the folder made for it ended: one undo for
    /// both, the folder selected with its name ready to change. A move that
    /// moved nothing leaves no empty folder behind.
    fn grouped(
        &mut self,
        volume_id: u64,
        folder: String,
        name: String,
        result: Result<PasteResponse, ErrorResponse>,
    ) -> Task<Message> {
        let moved = result
            .as_ref()
            .is_ok_and(|response| !response.select.is_empty());
        if !moved {
            let response = result.unwrap_or_else(|error| mapper::to_paste_failure(&error));
            self.notice = Some(response.notice);
            let service = Arc::clone(&self.service);
            let (path, names) = (folder.clone(), vec![name]);
            return Task::perform(
                async move {
                    // Empty: nothing went in. Gone again, as if never made.
                    let _ = service.delete(volume_id, &path, &names).await;
                },
                |()| Message::Request(Request::Refresh),
            );
        }
        let Ok(response) = result else {
            return Task::none();
        };
        let undo = match response.undo {
            Some(Undo::Moved(steps)) => Some(Undo::Grouped {
                folder: folder.clone(),
                name: name.clone(),
                moved: steps,
            }),
            _ => None,
        };
        self.changed(volume_id, undo);
        let mut notice = response.notice;
        notice.title = tf(
            "explorer.paste.into_folder",
            &[("title", &notice.title), ("name", &name)],
        );
        self.notice = Some(notice);
        self.name_the_group(&folder, &name)
    }

    /// Selects the folder made around the selection, if it is in the folder
    /// shown, and opens its name for editing, as Finder does.
    fn name_the_group(&mut self, folder: &str, name: &str) -> Task<Message> {
        let here = self
            .location
            .as_ref()
            .is_some_and(|location| location.path == folder);
        if !here {
            return Task::none();
        }
        self.selection.only(name);
        let stem = name.chars().count();
        self.start(
            Change::Rename {
                original: name.to_owned(),
            },
            name.to_owned(),
        );
        Task::batch([
            self.load(true),
            operation::focus(self.name_input.clone()),
            operation::select_range(self.name_input.clone(), 0, stem),
        ])
    }

    /// Finds the folder names to offer for what Go to Folder has typed: the
    /// folder shown answers from the pane itself, any other is asked of the
    /// service once.
    fn look_for_folders(&mut self) -> Task<Message> {
        let (Some(location), Some(editor)) = (&self.location, &mut self.editor) else {
            return Task::none();
        };
        let Change::GoTo(go) = &mut editor.change else {
            return Task::none();
        };
        let (folder, _) = go_to::split(&editor.name);
        let parent = go_to::resolve(&location.path, folder, location.volume.home.as_deref());
        if parent == go.parent {
            go.offer(&editor.name);
            return Task::none();
        }
        go.parent.clone_from(&parent);
        if parent == location.path {
            go.names = self
                .entries
                .iter()
                .filter(|entry| entry.is_folder)
                .map(|entry| entry.name.clone())
                .collect();
            go.offer(&editor.name);
            return Task::none();
        }
        go.names.clear();
        go.offer(&editor.name);
        let service = Arc::clone(&self.service);
        let volume_id = location.volume.id;
        Task::perform(
            async move {
                let names = service.folder_names(volume_id, &parent).await;
                (parent, names)
            },
            |(parent, names)| Message::FolderNames { parent, names },
        )
    }

    /// ↑/↓ in Go to Folder: picks the next or previous name offered.
    fn choose_suggestion(&mut self, delta: isize) {
        let Some(Editor {
            change: Change::GoTo(go),
            ..
        }) = &mut self.editor
        else {
            return;
        };
        let count = go.offered.len() as isize;
        if count == 0 {
            go.chosen = None;
            return;
        }
        let next = match go.chosen {
            None if delta > 0 => 0,
            None => count - 1,
            Some(at) => (at as isize + delta).rem_euclid(count),
        };
        go.chosen = Some(next as usize);
    }

    fn start(&mut self, change: Change, name: String) {
        self.editor = Some(Editor {
            change,
            name,
            error: None,
            busy: false,
        });
    }

    /// Sends the open change to the service; the answer arrives as `Edited`.
    fn submit(&mut self) -> Task<Message> {
        if let Some(Editor {
            change: Change::GoTo(go),
            name,
            ..
        }) = &self.editor
        {
            let typed = match go.chosen.and_then(|at| go.offered.get(at)) {
                Some(folder) => go_to::complete(name, folder),
                None => name.clone(),
            };
            let Some(location) = &self.location else {
                return Task::none();
            };
            let target = go_to::resolve(&location.path, &typed, location.volume.home.as_deref());
            self.editor = None;
            // The folder's own error if it isn't there, as for any folder.
            return self.navigate(target);
        }
        let (Some(location), Some(editor)) = (&self.location, &mut self.editor) else {
            return Task::none();
        };
        if editor.busy {
            return Task::none();
        }
        if let Change::RenameMany(many) = &mut editor.change {
            if !many.ready() {
                return Task::none();
            }
            editor.busy = true;
            many.done = Some(0);
            let renames = many.renames();
            let cancel = dto::Cancel::new();
            self.renaming = Some(cancel.clone());
            return rename_run(
                Arc::clone(&self.service),
                location.volume.id,
                location.path.clone(),
                renames,
                cancel,
            );
        }
        editor.busy = true;
        editor.error = None;

        let volume_id = location.volume.id;
        let path = location.path.clone();
        let change = match &editor.change {
            Change::NewFolder => Action::MakeFolder(editor.name.clone()),
            Change::Rename { original } => Action::Rename {
                old: original.clone(),
                new: editor.name.clone(),
            },
            Change::Delete {
                names,
                keep: Keep::Copy(_),
            } => {
                // The queue does it: copy, Trash, then delete.
                let request = dto::DeleteRequest {
                    storage_id: volume_id,
                    folder: location.path.clone(),
                    names: names.clone(),
                };
                self.editor = None;
                return Task::done(Message::DeleteKeepingCopy(request));
            }
            Change::Delete {
                keep: Keep::Counting,
                ..
            } => {
                editor.busy = false;
                return Task::none();
            }
            Change::Delete { names, .. } => Action::Delete(names.clone()),
            Change::RenameMany(_) | Change::GoTo(_) => return Task::none(),
        };
        // Undone with the opposite change: a rename back, the new folder
        // removed (while empty). A delete can't be undone.
        let renamed_from = match &change {
            Action::Rename { old, .. } => Some(old.clone()),
            _ => None,
        };
        let made_folder = matches!(change, Action::MakeFolder(_));
        let apply = apply(Arc::clone(&self.service), volume_id, path.clone(), change);

        Task::perform(apply, move |result| {
            let undo = match (&result, renamed_from) {
                (Ok(Some(name)), Some(from)) => Some(Undo::Renamed {
                    folder: path.clone(),
                    from,
                    to: name.clone(),
                }),
                (Ok(Some(name)), None) if made_folder => Some(Undo::MadeFolder {
                    folder: path.clone(),
                    name: name.clone(),
                }),
                _ => None,
            };
            Message::Edited {
                volume_id,
                path,
                result,
                undo,
            }
        })
    }

    fn set_entries(&mut self, entries: Vec<dto::Entry>) {
        self.entries = entries;
        self.resort();
    }

    /// Forgets the row a drag was over. The rows can change under a pointer
    /// that has not moved (the list scrolled, a piece of the folder arrived,
    /// the order or the view changed), and the next movement says again which
    /// one is there; until then a drop has nowhere to land.
    fn forget_pointer(&mut self) {
        self.spring = None;
        if let Some(drag) = &mut self.drag {
            drag.over = None;
        }
        if let Some(incoming) = &mut self.incoming {
            incoming.over = None;
        }
    }

    fn resort(&mut self) {
        self.forget_pointer();
        self.service.sort(&mut self.entries, self.sort);
        // Ordered by size, a folder counts as what is in it, once counted.
        if self.sort.key == dto::SortKey::Size && !self.sizes.is_empty() {
            let sizes = &self.sizes;
            let size = |entry: &dto::Entry| match sizes.get(&entry.name) {
                Some(FolderSize::Known(tally)) if entry.is_folder => tally.bytes,
                _ if entry.is_folder => 0,
                _ => entry.size_bytes,
            };
            let (folders_first, descending) = (self.sort.folders_first, self.sort.descending);
            // Stable: the service's order stays among equal sizes.
            self.entries.sort_by(|a, b| {
                let group = if folders_first {
                    b.is_folder.cmp(&a.is_folder)
                } else {
                    std::cmp::Ordering::Equal
                };
                let by_size = size(a).cmp(&size(b));
                group.then(if descending {
                    by_size.reverse()
                } else {
                    by_size
                })
            });
        }
        let today = chrono::Local::now().date_naive();
        let mut all_rows: Vec<EntryResponse> = self
            .entries
            .iter()
            .map(|entry| mapper::to_entry_response(entry, today))
            .collect();
        if !self.sizes.is_empty() {
            for (row, entry) in all_rows.iter_mut().zip(&self.entries) {
                if entry.is_folder
                    && let Some(text) = self.size_text(&entry.name)
                {
                    row.size = text;
                }
            }
        }
        self.all_rows = all_rows;
        self.by_name = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.name.clone(), index))
            .collect();
        self.apply_filter();
    }

    /// Counts what a delete would copy to the Trash first: the files' known
    /// sizes, and each folder's contents (counted as Calculate Folder Sizes
    /// counts, one at a time: the phone has one session).
    fn count_for_keeping(&self, volume_id: u64, path: &str, names: Vec<String>) -> Task<Message> {
        let mut bytes: u64 = 0;
        let mut unknown = false;
        let mut folders = Vec::new();
        for name in &names {
            match self.by_name.get(name).map(|&index| &self.entries[index]) {
                Some(entry) if entry.is_folder => {
                    folders.push(format!("{}/{name}", path.trim_end_matches('/')));
                }
                Some(entry) if entry.size_bytes == u64::from(u32::MAX) => unknown = true,
                Some(entry) => bytes = bytes.saturating_add(entry.size_bytes),
                None => unknown = true,
            }
        }
        let service = Arc::clone(&self.service);
        Task::perform(
            async move {
                for folder in folders {
                    match service
                        .folder_size(volume_id, &folder, dto::Cancel::new())
                        .await
                    {
                        Ok(tally)
                            if tally.unknown_sizes == 0
                                && tally.unreadable == 0
                                && !tally.incomplete =>
                        {
                            bytes = bytes.saturating_add(tally.bytes);
                        }
                        _ => unknown = true,
                    }
                }
                (names, (!unknown).then_some(bytes))
            },
            |(names, bytes)| Message::DeleteCounted { names, bytes },
        )
    }

    /// The selected entries, as the phone or the Mac described them.
    pub fn selected_entries(&self) -> Vec<dto::Entry> {
        self.selection
            .names()
            .iter()
            .filter_map(|name| self.by_name.get(name))
            .map(|&index| self.entries[index].clone())
            .collect()
    }

    /// What Get Info is about here: the folder shown and the selected items in
    /// it (none: the folder itself).
    pub fn info_subject(&self) -> Option<(String, Vec<dto::Entry>)> {
        let location = self.location.as_ref()?;
        Some((location.path.clone(), self.selected_entries()))
    }

    /// The selected files' bytes (where known) and how many folders are
    /// selected: one lookup per selected name.
    fn selected_size(&self) -> (u64, usize) {
        self.selection
            .names()
            .iter()
            .filter_map(|name| self.by_name.get(name))
            .map(|&index| &self.entries[index])
            .fold((0, 0), |(bytes, folders), entry| {
                if entry.is_folder {
                    (bytes, folders + 1)
                } else if entry.size_bytes == 0xFFFF_FFFF {
                    (bytes, folders)
                } else {
                    (bytes + entry.size_bytes, folders)
                }
            })
    }

    /// Words are typed in the filter or a kind is picked.
    fn is_filtering(&self) -> bool {
        !self.filter.is_empty() || self.filter_kind.is_some() || self.filter_date.is_some()
    }

    /// Picks the rows the filter leaves. What it hides is no longer selected:
    /// a delete or a copy must never act on something not on the screen.
    fn apply_filter(&mut self) {
        self.forget_pointer();
        let today = (self.today)();
        let query = dto::SearchQuery {
            text: self.filter.clone(),
            kind: self.filter_kind,
            date: self
                .filter_date
                .map(|range| crate::handler::format::date_span(range, today)),
        };
        let matcher = query.matcher();
        self.shown = if query.is_empty() {
            (0..self.entries.len()).collect()
        } else {
            (0..self.entries.len())
                .filter(|&at| matcher.matches(&self.entries[at]))
                .collect()
        };
        // Grouped: the rows go in group order, each group as it was sorted.
        // Once per listing, sort or filter change; never per redraw.
        self.groups = if self.group_by == dto::GroupBy::None {
            Vec::new()
        } else {
            let entries = &self.entries;
            let keys: Vec<(u32, String)> = self
                .shown
                .iter()
                .map(|&at| groups::bucket(self.group_by, &entries[at], today))
                .collect();
            let sizes: Vec<u64> = self
                .shown
                .iter()
                .map(|&at| match &entries[at] {
                    entry if entry.is_folder || entry.size_bytes == u64::from(u32::MAX) => 0,
                    entry => entry.size_bytes,
                })
                .collect();
            let (order, groups) = groups::arrange(&keys, &sizes);
            self.shown = order.iter().map(|&at| self.shown[at]).collect();
            groups
        };
        self.rows = self
            .shown
            .iter()
            .map(|&at| self.all_rows[at].clone())
            .collect();
        let names = self.row_names();
        self.selection.retain(&names);
        let names = self.row_names();
        self.selection.sort_like(&names);
    }

    /// Goes to `path`, which Back then returns from.
    fn navigate(&mut self, path: String) -> Task<Message> {
        self.fallback = None;
        if let Some(location) = &self.location {
            self.history.went(&location.path, &path);
        }
        self.go(path)
    }

    /// Adds `text`, typed `at`, to what is being typed (or starts again
    /// after a pause) and selects the first row, in the order shown, whose
    /// name starts with it. One pass over the rows; nothing is asked of the
    /// phone. No row starting so: the selection stays.
    fn type_ahead(&mut self, text: &str, at: Instant) -> Task<Message> {
        let paused = self
            .typed_at
            .is_none_or(|last| at.saturating_duration_since(last) > TYPING_PAUSE);
        if paused {
            self.typed.clear();
        }
        self.typed.push_str(text);
        self.typed_at = Some(at);
        let Some(name) = self
            .rows
            .iter()
            .find(|row| mapper::starts_with_typed(&row.name, &self.typed))
            .map(|row| row.name.clone())
        else {
            return Task::none();
        };
        self.selection.only(&name);
        self.scroll_to_cursor()
    }

    /// A change was made in this pane: it is the latest, and `undo` (if any)
    /// is how to undo it; an earlier change's undo is gone, as in Finder.
    fn changed(&mut self, volume_id: u64, undo: Option<Undo>) {
        self.changed_at = undo::next_mark();
        self.last_undo = undo.map(|undo| (volume_id, undo));
    }

    /// When this pane last changed something (0: never), to tell which
    /// pane's change ⌘Z undoes.
    pub fn changed_at(&self) -> u64 {
        self.changed_at
    }

    /// The pane's last change can be undone now.
    pub fn can_undo(&self) -> bool {
        self.last_undo.is_some() && self.editor.is_none() && self.pasting.is_none()
    }

    /// Undoes the pane's last change (once: there is no redo); the answer
    /// arrives as `Undone`.
    pub fn language_changed(&mut self) {
        self.notice = None;
        self.error = self.error.as_ref().map(ErrorResponse::reworded);
        if let Some(editor) = &mut self.editor {
            editor.error = editor.error.as_ref().map(ErrorResponse::reworded);
        }
        if self.location.is_some() {
            self.resort();
        }
    }

    pub fn undo(&mut self) -> Task<Message> {
        if !self.can_undo() {
            return Task::none();
        }
        let Some((volume_id, undo)) = self.last_undo.take() else {
            return Task::none();
        };
        let what = undo.banner();
        let service = Arc::clone(&self.service);
        Task::perform(
            async move {
                undo::undo(service, volume_id, undo)
                    .await
                    .map_err(error::to_error_response)
            },
            move |result| Message::Undone {
                volume_id,
                what,
                result,
            },
        )
    }

    /// How wide the list's columns are, to be remembered.
    pub fn column_widths(&self) -> dto::ColumnWidths {
        self.columns
    }

    /// The widths remembered for this pane.
    pub fn set_column_widths(&mut self, columns: dto::ColumnWidths) {
        if self.resizing.is_none() {
            self.columns = columns;
        }
    }

    /// A column is being resized: the app then tells where the pointer is.
    pub fn is_resizing(&self) -> bool {
        self.resizing.is_some()
    }

    /// Reads the folder again if macOS refused it: the user may have just
    /// allowed it in System Settings and come back. Nothing otherwise.
    pub fn read_again_if_refused(&mut self) -> Task<Message> {
        if self
            .error
            .as_ref()
            .is_some_and(|error| error.privacy.is_some())
        {
            self.load(false)
        } else {
            Task::none()
        }
    }

    /// Whether Back, and Forward, have somewhere to go.
    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    /// Shows `path`. Going up to a folder holding the one left (Up, a
    /// crumb, Back) selects the folder that leads back down, as Finder does.
    fn go(&mut self, path: String) -> Task<Message> {
        let came_from = self
            .location
            .as_ref()
            .and_then(|location| mapper::child_toward(&path, &location.path));
        self.close_menu();
        self.notice = None;
        self.drag = None;
        // A box drawn in the folder left behind has nothing to select here.
        self.lasso = None;
        if let Some(location) = &mut self.location {
            location.path = path;
        }
        self.filter.clear();
        self.filter_kind = None;
        self.filter_date = None;
        self.forget_sizes();
        self.set_entries(Vec::new());
        self.selection.clear();
        self.reveal = came_from;
        self.typed.clear();
        self.thumbnails.clear();
        self.viewport.0 = 0.0;
        self.load(true)
    }

    /// Shows the files as a list or as tiles, fetching thumbnails for tiles.
    pub fn set_view(&mut self, view: dto::ViewMode) -> Task<Message> {
        if self.view == view {
            return Task::none();
        }
        self.view = view;
        self.forget_pointer();
        // Sizes are for the list's Size column; tiles have none.
        let sizes = if view == dto::ViewMode::List {
            self.count_sizes()
        } else {
            self.stop_sizes();
            Task::none()
        };
        Task::batch([self.request_thumbnails(), sizes])
    }

    /// Shows (or stops showing) each folder's size in the list.
    /// The kind the filter keeps.
    #[cfg(test)]
    pub fn filter_kind(&self) -> Option<dto::FileKind> {
        self.filter_kind
    }

    /// What the rows are grouped by.
    #[cfg(test)]
    pub fn group_by(&self) -> dto::GroupBy {
        self.group_by
    }

    /// Groups the rows under headers by `by` (or not), keeping the selection.
    pub fn set_group_by(&mut self, by: dto::GroupBy) {
        if self.group_by == by {
            return;
        }
        self.group_by = by;
        self.apply_filter();
    }

    /// Whether a phone delete keeps a copy in the Trash first.
    #[cfg(test)]
    pub fn keeps_deleted(&self) -> bool {
        self.keep_deleted
    }

    /// Whether a phone delete keeps a copy in the Trash first (the setting).
    pub fn set_keep_deleted(&mut self, on: bool) {
        self.keep_deleted = on;
    }

    pub fn set_folder_sizes(&mut self, on: bool) -> Task<Message> {
        if self.sizes_on == on {
            return Task::none();
        }
        self.sizes_on = on;
        if on {
            self.count_sizes()
        } else {
            self.forget_sizes();
            self.resort();
            Task::none()
        }
    }

    /// Stops the counting and forgets every size (another folder, or its
    /// contents may have changed).
    fn forget_sizes(&mut self) {
        self.stop_sizes();
        self.sizes.clear();
    }

    /// Stops the counting, keeping the sizes already known.
    fn stop_sizes(&mut self) {
        if let Some(cancel) = self.sizing.take() {
            cancel.cancel();
        }
        self.size_run += 1;
        self.size_queue.clear();
        let stopped: Vec<String> = self
            .sizes
            .iter()
            .filter(|(_, size)| **size == FolderSize::Counting)
            .map(|(name, _)| name.clone())
            .collect();
        for name in stopped {
            self.sizes.remove(&name);
            self.show_size(&name);
        }
    }

    /// Starts counting the folders shown whose size isn't known, one at a
    /// time (the phone has one session), in the order shown.
    fn count_sizes(&mut self) -> Task<Message> {
        if !self.sizes_on
            || self.view != dto::ViewMode::List
            || self.loading
            || self.sizing.is_some()
            || self.location.is_none()
        {
            return Task::none();
        }
        self.size_queue = self
            .entries
            .iter()
            .filter(|entry| entry.is_folder && !self.sizes.contains_key(&entry.name))
            .map(|entry| entry.name.clone())
            .collect();
        self.next_size()
    }

    /// Counts the next folder in the queue that is still here.
    fn next_size(&mut self) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        let (volume_id, folder) = (location.volume.id, location.path.clone());
        while let Some(name) = self.size_queue.pop_front() {
            let present = self
                .by_name
                .get(&name)
                .is_some_and(|&at| self.entries[at].is_folder);
            if !present {
                continue;
            }
            self.sizes.insert(name.clone(), FolderSize::Counting);
            self.show_size(&name);
            let cancel = dto::Cancel::new();
            self.sizing = Some(cancel.clone());
            let (service, run) = (Arc::clone(&self.service), self.size_run);
            let path = mapper::child_path(&folder, &name);
            return Task::perform(
                async move {
                    service
                        .folder_size(volume_id, &path, cancel)
                        .await
                        .map_err(error::to_error_response)
                },
                move |result| Message::FolderSized {
                    run,
                    name: name.clone(),
                    result,
                },
            );
        }
        Task::none()
    }

    /// What a folder's Size cell says now, if anything is known.
    fn size_text(&self, name: &str) -> Option<String> {
        match self.sizes.get(name)? {
            FolderSize::Counting => Some(mapper::counting().to_owned()),
            FolderSize::Known(tally) => Some(mapper::folder_size(tally)),
        }
    }

    /// Puts what is known of one folder's size into its row.
    fn show_size(&mut self, name: &str) {
        let Some(&at) = self.by_name.get(name) else {
            return;
        };
        let text = self
            .size_text(name)
            .unwrap_or_else(|| mapper::MISSING.to_owned());
        self.all_rows[at].size.clone_from(&text);
        if let Some(row) = self.shown.iter().position(|&shown| shown == at) {
            self.rows[row].size = text;
        }
    }

    /// Makes the list's rows taller or shorter. The row at the top of the
    /// list stays at the top: the offset is scaled with the rows.
    pub fn set_density(&mut self, density: dto::Density) -> Task<Message> {
        if self.density == density {
            return Task::none();
        }
        let before = self.row_height();
        self.density = density;
        self.forget_pointer();
        if self.view != dto::ViewMode::List {
            return Task::none();
        }
        let y = self.viewport.0 / before * self.row_height();
        self.viewport.0 = y;
        operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y })
    }

    fn row_height(&self) -> f32 {
        row_style(self.density).height
    }

    /// Makes the grid's tiles `size`. In Grid view the first tile in view
    /// stays in view, and thumbnails are asked for any tiles that come into
    /// it.
    pub fn set_tile_size(&mut self, size: dto::TileSize) -> Task<Message> {
        let tile = grid::Tile::of(size);
        if self.tile == tile {
            return Task::none();
        }
        let (columns_before, row_before) = (self.columns(), self.tile.row_height());
        self.tile = tile;
        self.forget_pointer();
        if self.view != dto::ViewMode::Grid {
            return Task::none();
        }
        let first = (self.viewport.0 / row_before).floor() as usize * columns_before;
        let y = (first / self.columns()) as f32 * self.tile.row_height();
        self.viewport.0 = y;
        Task::batch([
            operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y }),
            self.request_thumbnails(),
        ])
    }

    #[cfg(test)]
    pub fn tile(&self) -> grid::Tile {
        self.tile
    }

    #[cfg(test)]
    pub fn density(&self) -> dto::Density {
        self.density
    }

    fn columns(&self) -> usize {
        grid::columns(self.viewport.2, self.tile)
    }

    /// The rows whose tiles are on screen (grouped: from the first line in
    /// view to the last, headers left out).
    fn visible_tiles(&self) -> std::ops::Range<usize> {
        let (offset, height, _) = self.viewport;
        if self.groups.is_empty() {
            return grid::visible(offset, height, self.columns(), self.shown.len(), self.tile);
        }
        let columns = self.columns();
        let frame = self.grid_frame(self.viewport.2);
        let window = frame.window(offset, height);
        let mut rows = window.clone().filter_map(|line| {
            match groups::grid_line(&self.groups, columns, &frame.starts, line) {
                groups::GridLine::Tiles { first, end } if first < end => Some((first, end)),
                _ => None,
            }
        });
        let Some((start, mut end)) = rows.next() else {
            return 0..0;
        };
        if let Some((_, last)) = rows.next_back() {
            end = last;
        }
        start..end.min(self.shown.len())
    }

    /// Lets go of thumbnails far from the screen once there are too many; they
    /// are fetched again if the user scrolls back to them.
    fn trim_thumbnails(&mut self) {
        if self.thumbnails.len() <= THUMBNAIL_CACHE {
            return;
        }
        let on_screen = self.visible_tiles();
        let slack = THUMBNAIL_CACHE / 2;
        let keep =
            on_screen.start.saturating_sub(slack)..(on_screen.end + slack).min(self.shown.len());
        let near: std::collections::HashSet<&str> = self.shown[keep]
            .iter()
            .map(|&at| self.entries[at].name.as_str())
            .collect();
        self.thumbnails
            .retain(|name, thumb| matches!(thumb, Thumb::Loading) || near.contains(name.as_str()));
    }

    /// Asks for thumbnails of the files on screen that don't have one yet.
    fn request_thumbnails(&mut self) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        // The phone is busy describing files; thumbnails would only slow that
        // down, and the tiles show their kind until it is done.
        if self.view != dto::ViewMode::Grid || self.loading {
            return Task::none();
        }
        let range = self.visible_tiles();
        let wanted: Vec<dto::Entry> = self.shown[range]
            .iter()
            .map(|&at| &self.entries[at])
            .filter(|entry| !entry.is_folder && !self.thumbnails.contains_key(&entry.name))
            .take(THUMBNAILS_PER_BATCH)
            .cloned()
            .collect();

        let volume_id = location.volume.id;
        let path = location.path.clone();
        let tasks: Vec<Task<Message>> = wanted
            .into_iter()
            .map(|entry| {
                self.thumbnails.insert(entry.name.clone(), Thumb::Loading);
                let service = Arc::clone(&self.service);
                Task::perform(
                    fetch_thumbnail(service, volume_id, path.clone(), entry),
                    move |(path, name, picture)| match picture {
                        Ok(picture) => Message::Thumbnail {
                            volume_id,
                            path,
                            name,
                            picture,
                        },
                        Err(error) => Message::ThumbnailLost {
                            volume_id,
                            path,
                            error,
                        },
                    },
                )
            })
            .collect();
        Task::batch(tasks)
    }

    fn row_names(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.name.clone()).collect()
    }

    /// How many items Page Up or Page Down moves: the whole rows that fit
    /// the list's height (whole rows of tiles in the grid), at least one.
    fn page(&self) -> isize {
        let height = self.viewport.1;
        let rows = |row_height: f32| ((height / row_height).floor() as isize).max(1);
        match self.view {
            dto::ViewMode::List => rows(self.row_height()),
            dto::ViewMode::Grid => rows(self.tile.row_height()) * self.columns() as isize,
        }
    }

    /// Scrolls the list just enough to show the cursor row.
    fn scroll_to_cursor(&self) -> Task<Message> {
        let Some(index) = self.selection.cursor_index(&self.row_names()) else {
            return Task::none();
        };
        let (offset, height, _) = self.viewport;
        let (index, row_height) = match (self.groups.is_empty(), self.view) {
            (true, dto::ViewMode::List) => (index, self.row_height()),
            (true, dto::ViewMode::Grid) => (index / self.columns(), self.tile.row_height()),
            (false, dto::ViewMode::List) => {
                (groups::list_line_of(&self.groups, index), self.row_height())
            }
            (false, dto::ViewMode::Grid) => {
                // Lines of two heights: scrolled to by where the line is.
                let frame = self.grid_frame(self.viewport.2);
                let (line, _) =
                    groups::grid_place(&self.groups, self.columns(), &frame.starts, index);
                let (top, bottom) = (frame.top(line), frame.top(line) + frame.height(line));
                let to = if top < offset {
                    Some(top)
                } else if bottom > offset + height {
                    Some(bottom - height)
                } else {
                    None
                };
                return match to {
                    Some(y) => {
                        operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y })
                    }
                    None => Task::none(),
                };
            }
        };
        match rows::scroll_to_show(index, row_height, offset, height) {
            Some(y) => operation::scroll_to(self.list.clone(), AbsoluteOffset { x: 0.0, y }),
            None => Task::none(),
        }
    }

    /// Starts reading the folder; `allow_remembered` says whether a remembered
    /// reading of a phone folder will do (see `Service::open_listing`).
    fn load(&mut self, allow_remembered: bool) -> Task<Message> {
        let Some(location) = &self.location else {
            return Task::none();
        };
        let volume_id = location.volume.id;
        let path = location.path.clone();

        // The folder being read is no longer wanted; it stops between files.
        self.stop_listing();
        // Read afresh (Refresh, a change on the phone, after a transfer): what
        // is inside the folders may have changed too.
        if !allow_remembered {
            self.forget_sizes();
        }
        let cancel = dto::Cancel::new();
        self.listing = Some(cancel.clone());
        self.loading = true;
        self.error = None;

        let updates = list_updates(
            Arc::clone(&self.service),
            volume_id,
            path.clone(),
            self.show_hidden,
            cancel,
            allow_remembered,
        );
        Task::run(updates, move |piece| piece.into_message(volume_id, &path))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(location) = &self.location else {
            return space().into();
        };
        let pane = self.view_pane(location);
        match &self.editor {
            Some(editor) => modal(pane, self.view_editor(editor, location)),
            None => pane,
        }
    }

    /// The pane, laid out for the width it actually gets.
    fn view_pane<'a>(&'a self, location: &'a Location) -> Element<'a, Message> {
        responsive(move |size| {
            self.view_fitted(location, layout::for_width(size.width, self.columns))
        })
        .into()
    }

    fn view_fitted<'a>(&'a self, location: &'a Location, fit: Fit) -> Element<'a, Message> {
        // Android keeps these from computers: say so rather than "empty".
        let app_folder =
            location.volume.hides_app_folders && mapper::in_app_folders(&location.path);
        let body: Element<'_, Message> = if let Some(error) = &self.error {
            failure(error, app_folder.then_some(mapper::app_folders_note()))
        } else if self.loading && self.rows.is_empty() {
            centered(text(loading_text(self.progress)).style(text::secondary))
        } else if self.rows.is_empty() && !self.entries.is_empty() {
            centered(
                text(tf(
                    "explorer.empty.no_match",
                    &[("filter", self.filter.trim())],
                ))
                .style(text::secondary),
            )
        } else if self.rows.is_empty() && app_folder {
            centered(
                column![
                    text(t("explorer.empty.app_folders")).size(18),
                    text(mapper::app_folders_note()).style(text::secondary),
                ]
                .spacing(8)
                .max_width(460),
            )
        } else if self.rows.is_empty() {
            centered(text(t("explorer.empty.folder")).style(text::secondary))
        } else if self.view == dto::ViewMode::Grid {
            responsive(move |size| self.tiles(grid::columns(size.width, self.tile), size.height))
                .into()
        } else {
            responsive(move |size| self.list_rows(fit, size.height)).into()
        };

        let mut body = lasso(body, |at, size, taken| Message::PressedAt {
            at,
            size,
            taken,
        })
        .rectangle(self.lasso_box());
        if self.lasso.is_some() {
            body = body.on_move(|at, size| Message::LassoMoved { at, size });
        }
        if self.incoming.is_some() || self.dragging().is_some() {
            body = body.on_hover(Message::Hover);
        }
        let body = right_click(body, |at, area| Message::ShowMenu { at, area });

        let mut content =
            column![self.toolbar(location, fit.width), rule::horizontal(1)].spacing(8);
        if let Some(banner) = self.banner() {
            content = content.push(banner);
        }
        content = content.push(self.table_header(fit)).push(body);
        // The drag's hint is said down here, below the list, so that nothing
        // above it moves when a drag starts: a row sliding under a pointer
        // that has not moved would be dropped on by mistake. The footer is
        // shown for the length of a drag if it is turned off.
        if self.status_bar || self.drag_hint().is_some() || self.incoming_hint().is_some() {
            content = content.push(rule::horizontal(1)).push(self.status_bar(fit));
        }

        container(content)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// What a paste is doing or just did, across the top of the list.
    fn banner(&self) -> Option<Element<'_, Message>> {
        let on_the_mac = self
            .location
            .as_ref()
            .is_some_and(|location| location.volume.shows_in_finder);
        let (title, details, problem, dismiss) = match (&self.pasting, &self.notice) {
            (Some(mode), _) => (
                match (mode, on_the_mac) {
                    (dto::Paste::Copy, false) => t("explorer.busy.copying_phone"),
                    (dto::Paste::Move, false) => t("explorer.busy.moving_phone"),
                    (dto::Paste::Copy, true) => t("explorer.busy.copying"),
                    (dto::Paste::Move, true) => t("explorer.busy.moving"),
                },
                &[][..],
                false,
                false,
            ),
            (None, Some(notice)) => (
                notice.title.as_str(),
                notice.details.as_slice(),
                notice.problem,
                true,
            ),
            (None, None) => return None,
        };

        // A problem is told by its red title; a loud fill would make the
        // grey detail lines under it hard to read.
        let title = text(title)
            .size(13)
            .style(if problem { text::danger } else { text::default });
        let mut lines = column![title].spacing(2);
        for detail in details {
            lines = lines.push(text(detail).size(12).style(text::secondary));
        }
        let mut content = row![lines.width(Length::Fill)]
            .spacing(8)
            .align_y(Alignment::Center);
        if dismiss {
            content = content.push(
                button(text("✕").size(12))
                    .style(button::text)
                    .padding([2, 6])
                    .on_press(Message::Request(Request::DismissNotice)),
            );
        }
        Some(
            container(content)
                .padding([6, 10])
                .width(Length::Fill)
                .style(container::bordered_box)
                .into(),
        )
    }

    fn toolbar<'a>(&'a self, location: &'a Location, width: u16) -> Element<'a, Message> {
        let back = icon_button(
            "‹",
            t("explorer.toolbar.back"),
            self.history
                .can_go_back()
                .then_some(Message::Request(Request::Back)),
        );
        let forward = icon_button(
            "›",
            t("explorer.toolbar.forward"),
            self.history
                .can_go_forward()
                .then_some(Message::Request(Request::Forward)),
        );
        let up = icon_button(
            "↑",
            t("explorer.toolbar.up"),
            mapper::parent_path(&location.path).map(|_| Message::Request(Request::Up)),
        );
        let refresh = icon_button(
            "↻",
            t("label.refresh"),
            Some(Message::Request(Request::Refresh)),
        );

        let crumbs = mapper::compact_breadcrumbs(mapper::breadcrumbs(
            &location.volume.name,
            location.volume.home.as_deref(),
            &location.path,
        ));
        let last = crumbs.len() - 1;
        let mut trail = row![].spacing(2).align_y(Alignment::Center);
        for (index, crumb) in crumbs.into_iter().enumerate() {
            if index > 0 {
                trail = trail.push(text("›").style(text::secondary));
            }
            let label = text(crumb.label).size(15).wrapping(Wrapping::None);
            trail = if index == last {
                trail.push(container(label).padding([4, 6]))
            } else {
                trail.push(
                    button(label)
                        .style(button::text)
                        .padding([4, 6])
                        .on_press(Message::Request(Request::GoTo(crumb.path))),
                )
            };
        }

        let action = |label, request: Option<Request>| {
            button(text(label).size(13).wrapping(Wrapping::None))
                .style(button::text)
                .padding([4, 6])
                .on_press_maybe(request.map(Message::Request))
        };
        let has_selection = !self.selection.is_empty();

        // Navigation and actions get a row each so neither is squeezed out
        // when the window is narrow and the pane is half of it.
        let view_button = |label, view| {
            button(text(label).size(14))
                .padding([3, 8])
                .style(if self.view == view {
                    button::secondary
                } else {
                    button::text
                })
                .on_press(Message::Request(Request::SetView(view)))
        };
        let views = row![
            view_button("☰", dto::ViewMode::List),
            view_button("▦", dto::ViewMode::Grid)
        ];
        // Always there, so nothing moves when it is used.
        let filter = text_input(t("explorer.filter.placeholder"), &self.filter)
            .id(self.filter_input.clone())
            .size(13)
            .padding([3, 8])
            .on_input(|text| Message::Request(Request::Filter(text)))
            .on_submit(Message::Request(Request::SubmitFilter));
        // Beside the words, the kind to keep. Its width follows the pane's,
        // never what is picked, so picking one moves nothing.
        let kinds: Vec<KindChoice> = std::iter::once(None)
            .chain(dto::FileKind::ALL.into_iter().map(Some))
            .map(KindChoice)
            .collect();
        let kind = pick_list(kinds, Some(KindChoice(self.filter_kind)), |choice| {
            Message::Request(Request::FilterKind(choice.0))
        })
        .text_size(13)
        .padding([3, 6])
        .width(Length::Fill);
        let dates: Vec<DateChoice> = std::iter::once(None)
            .chain(dto::DateRange::ALL.into_iter().map(Some))
            .map(DateChoice)
            .collect();
        let date = pick_list(dates, Some(DateChoice(self.filter_date)), |choice| {
            Message::Request(Request::FilterDate(choice.0))
        })
        .text_size(13)
        .padding([3, 6])
        .width(Length::Fill);
        // In a wide pane the path and the filter share what is left of the
        // row; a narrow one gives the filter a row of its own below, so
        // neither is squeezed to nothing.
        let inline = layout::filters_inline(width);
        let menus_beside = layout::menus_beside_filter(width);
        let filters: Element<'_, Message> = if inline || menus_beside {
            row![
                container(filter)
                    .width(Length::FillPortion(if inline { 2 } else { 3 }))
                    .max_width(if inline { 140.0 } else { f32::INFINITY }),
                container(date)
                    .width(Length::FillPortion(2))
                    .max_width(DATE_WIDTH),
                container(kind)
                    .width(Length::FillPortion(2))
                    .max_width(KIND_WIDTH),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        } else {
            column![
                filter,
                row![
                    container(date).width(Length::Fill),
                    container(kind).width(Length::Fill),
                ]
                .spacing(8),
            ]
            .spacing(4)
            .into()
        };
        let mut navigation = row![
            row![back, forward].spacing(2),
            up,
            refresh,
            container(trail).width(Length::FillPortion(3)).clip(true),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut below = None;
        if inline {
            navigation = navigation.push(
                container(filters)
                    .width(Length::FillPortion(6))
                    .max_width(140.0 + DATE_WIDTH + KIND_WIDTH + 16.0),
            );
        } else {
            below = Some(filters);
        }
        let navigation = navigation.push(views);
        let mut actions = row![
            action(t("explorer.action.new_folder"), Some(Request::NewFolder)),
            action(t("label.rename"), has_selection.then_some(Request::Rename)),
            action(t("label.delete"), has_selection.then_some(Request::Delete)),
        ]
        .spacing(4);
        if location.volume.phone_tools {
            actions = actions.push(action(t("label.search"), Some(Request::Search)));
            actions = actions.push(action(t("explorer.action.mirror"), Some(Request::Mirror)));
        }
        // In a narrow pane the actions go on to a second line rather than
        // pushing the transfer button out of the pane.
        let mut actions = row![container(actions.wrap().vertical_spacing(2)).width(Length::Fill)]
            .align_y(Alignment::Start);
        if let Some(action) = location.volume.transfer_action {
            actions = actions.push(
                button(text(t(action)).size(13).wrapping(Wrapping::None))
                    .style(button::primary)
                    .padding([4, 10])
                    .on_press_maybe(has_selection.then_some(Message::Request(Request::Transfer))),
            );
        }

        let mut toolbar = column![navigation].spacing(4);
        if let Some(filters) = below {
            toolbar = toolbar.push(filters);
        }
        toolbar.push(actions).into()
    }

    fn view_editor<'a>(
        &'a self,
        editor: &'a Editor,
        location: &'a Location,
    ) -> Element<'a, Message> {
        let (title, confirm) = match &editor.change {
            Change::NewFolder => (
                t("explorer.action.new_folder").to_owned(),
                t("label.create"),
            ),
            Change::Rename { original } => (
                tf("explorer.editor.rename_title", &[("original", original)]),
                t("label.rename"),
            ),
            Change::RenameMany(many) => (rename_many::title(many), t("label.rename_all")),
            Change::GoTo(_) => (t("explorer.editor.go_to_title").to_owned(), t("label.go")),
            Change::Delete { names, .. } => (
                mapper::delete_title(names, location.volume.deletes_permanently),
                if location.volume.deletes_permanently {
                    t("label.delete")
                } else {
                    t("label.move_to_trash")
                },
            ),
        };

        let mut content = column![text(title).size(18)].spacing(12);
        content = match &editor.change {
            Change::NewFolder | Change::Rename { .. } => content.push(
                text_input(t("explorer.editor.name_placeholder"), &editor.name)
                    .id(self.name_input.clone())
                    .padding(8)
                    .on_input_maybe(
                        (!editor.busy).then_some(|name| Message::Request(Request::EditName(name))),
                    )
                    .on_submit(Message::Request(Request::Submit)),
            ),
            Change::Delete { keep, .. } if location.volume.deletes_permanently => {
                content.push(text(mapper::delete_note(*keep)).style(text::secondary))
            }
            Change::GoTo(go) => {
                content.push(go_to_fields(go, &editor.name, self.name_input.clone()))
            }
            Change::RenameMany(many) => content.push(rename_many::fields(
                many,
                editor.busy,
                self.name_input.clone(),
            )),
            Change::Delete { .. } => content,
        };
        if let Some(error) = &editor.error {
            content = content.push(
                column![
                    text(&error.title).style(text::danger),
                    text(&error.hint).size(13).style(text::secondary),
                ]
                .spacing(4),
            );
        }

        let confirm_style = if matches!(editor.change, Change::Delete { .. }) {
            button::danger
        } else {
            button::primary
        };
        // Renaming several: Cancel stops a run between items, and Rename waits
        // until every new name can be used.
        let (can_cancel, can_confirm, width) = match &editor.change {
            Change::RenameMany(many) => (true, !editor.busy && many.ready(), 520),
            // The question can't be answered before it knows what it asks.
            Change::Delete {
                keep: Keep::Counting,
                ..
            } => (!editor.busy, false, 400),
            _ => (!editor.busy, !editor.busy, 400),
        };
        let buttons = row![
            space::horizontal(),
            button(text(t("label.cancel")))
                .style(button::secondary)
                .on_press_maybe(can_cancel.then_some(Message::Request(Request::Cancel))),
            button(text(if editor.busy {
                t("label.working")
            } else {
                confirm
            }))
            .style(confirm_style)
            .on_press_maybe(can_confirm.then_some(Message::Request(Request::Submit))),
        ]
        .spacing(8);

        container(content.push(buttons))
            .padding(20)
            .max_width(width)
            .style(container::rounded_box)
            .into()
    }

    fn table_header(&self, fit: Fit) -> Element<'_, Message> {
        let sortable = |title, key, width: Length| {
            button(text(mapper::header_label(title, key, self.sort)).size(12))
                .style(button::text)
                .padding(0)
                .width(width)
                .on_press(Message::Request(Request::SortBy(key)))
        };
        // The gap between two columns is the handle that resizes the one on
        // its right; double-click it for the usual widths.
        let handle = |edge| {
            mouse_area(
                container(rule::vertical(1))
                    .width(f32::from(layout::GAP))
                    .height(14)
                    .align_x(Alignment::Center),
            )
            .interaction(mouse::Interaction::ResizingHorizontally)
            .on_press(Message::Request(Request::ResizeColumn(edge, fit.width)))
            .on_double_click(Message::Request(Request::ResetColumns))
        };

        let mut header = row![sortable(
            t("label.column.name"),
            dto::SortKey::Name,
            Length::Fill
        )];
        if fit.kind {
            header = header.push(handle(layout::Edge::Kind)).push(sortable(
                t("label.column.kind"),
                dto::SortKey::Kind,
                Length::Fixed(f32::from(fit.kind_width)),
            ));
        }
        if fit.size {
            header = header.push(handle(layout::Edge::Size)).push(sortable(
                t("label.column.size"),
                dto::SortKey::Size,
                Length::Fixed(f32::from(fit.size_width)),
            ));
        }
        header
            .push(handle(layout::Edge::Modified))
            .push(sortable(
                t("label.column.modified"),
                dto::SortKey::Modified,
                Length::Fixed(f32::from(fit.modified_width)),
            ))
            .align_y(Alignment::Center)
            .padding([0, 10])
            .into()
    }

    /// Status on the left, listing options on the right, so the toolbar
    /// above stays navigation only and fits a half-width pane.
    fn status_bar(&self, fit: Fit) -> Element<'_, Message> {
        let selected = self.selection.len();
        let hint = self.drag_hint().or_else(|| self.incoming_hint());
        let status = match &hint {
            Some(hint) => hint.clone(),
            None if self.loading => loading_text(self.progress),
            None if self.shown.len() < self.entries.len() => {
                mapper::filtered_status(self.shown.len(), self.entries.len(), selected)
            }
            None if selected > 0 => {
                let (bytes, folders) = self.selected_size();
                mapper::selected_status(selected, self.entries.len(), bytes, folders)
            }
            None => mapper::status(&self.entries),
        };

        let folders_first = toggler(self.sort.folders_first)
            .label(t("settings.folders_first"))
            .text_size(12)
            .on_toggle(|on| Message::Request(Request::SetFoldersFirst(on)));
        let hidden = toggler(self.show_hidden)
            .label(t("label.hidden"))
            .text_size(12)
            .on_toggle(|show| Message::Request(Request::SetShowHidden(show)));

        let mut footer = row![
            text(status)
                .size(12)
                // A drag's hint is what to read now; the counts are background.
                .style(if hint.is_some() {
                    text::default
                } else {
                    text::secondary
                })
                .wrapping(Wrapping::None),
            space::horizontal(),
        ];
        if fit.footer_toggles {
            footer = footer.push(folders_first).push(hidden);
        }
        // A long hint in a narrow pane is cut at the pane's edge rather than
        // written over the next pane.
        footer
            .spacing(12)
            .align_y(Alignment::Center)
            .clip(true)
            .into()
    }
}

/// "Loading…", or how far along a big folder is: "Loading 1,200 of 3,400…".
fn loading_text(progress: Option<(usize, usize)>) -> String {
    match progress {
        Some((loaded, total)) if total > 0 => tf(
            "explorer.loading_of",
            &[
                ("loaded", &format_count(loaded.min(total))),
                ("total", &format_count(total)),
            ],
        ),
        _ => t("explorer.loading").to_owned(),
    }
}

/// A compact toolbar button that names itself on hover, so the folder
/// trail keeps the room in a half-width pane.
fn icon_button<'a>(
    icon: &'a str,
    label: &'a str,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    tooltip(
        button(text(icon).size(15))
            .style(button::secondary)
            .padding([3, 10])
            .on_press_maybe(on_press),
        container(text(label).size(12))
            .padding([4, 8])
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .into()
}

/// Go to Folder's field and the folder names it could go on with.
fn go_to_fields<'a>(go: &'a GoTo, typed: &'a str, input: Id) -> Element<'a, Message> {
    let mut names = column![].spacing(2);
    for (at, name) in go.offered.iter().enumerate() {
        let chosen = go.chosen == Some(at);
        names = names.push(
            button(row![glyph(true, None, chosen, 14.0), text(name).size(13)].spacing(6))
                .style(if chosen {
                    button::primary
                } else {
                    button::text
                })
                .padding([2, 6])
                .width(Length::Fill)
                .on_press(Message::Request(Request::ChooseFolder(name.clone()))),
        );
    }
    column![
        text_input("/DCIM/Camera", typed)
            .id(input)
            .padding(8)
            .on_input(|typed| Message::Request(Request::EditName(typed)))
            .on_submit(Message::Request(Request::Submit)),
        names,
        text(t(go_to::HINT)).size(12).style(text::secondary),
    ]
    .spacing(8)
    .into()
}

/// Renames each pair in `path` in turn, telling how many are done as it
/// goes, and ends with `RenamedMany`.
fn rename_run(
    service: Arc<dyn Service>,
    volume_id: u64,
    path: String,
    renames: Vec<(String, String)>,
    cancel: dto::Cancel,
) -> Task<Message> {
    let asked = renames.len();
    Task::run(
        iced::stream::channel(
            16,
            async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
                let progress = std::sync::Mutex::new(output.clone());
                let outcome = rename_many::rename_each(
                    service,
                    volume_id,
                    path.clone(),
                    renames,
                    cancel,
                    |done| {
                        // Best effort: a dropped count is replaced by the next.
                        if let Ok(mut sender) = progress.lock() {
                            let _ = sender.try_send(Message::RenamedSome(done));
                        }
                    },
                )
                .await;
                let _ = iced::futures::SinkExt::send(
                    &mut output,
                    Message::RenamedMany {
                        volume_id,
                        path,
                        outcome,
                        asked,
                    },
                )
                .await;
            },
        ),
        |message| message,
    )
}

/// A change sent to the service.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    MakeFolder(String),
    Rename { old: String, new: String },
    Delete(Vec<String>),
}

/// Applies one change in `path`; `Ok` holds the name to select afterwards.
async fn apply(
    service: Arc<dyn Service>,
    volume_id: u64,
    path: String,
    action: Action,
) -> Result<Option<String>, ErrorResponse> {
    let result = match action {
        Action::MakeFolder(name) => service.make_folder(volume_id, &path, &name).await.map(Some),
        Action::Rename { old, new } => service.rename(volume_id, &path, &old, &new).await.map(Some),
        Action::Delete(names) => service
            .delete(volume_id, &path, &names)
            .await
            .map(|()| None),
    };
    result.map_err(error::to_error_response)
}

/// Pastes each request in turn (one per folder the items came from) and
/// reports them as one. The banner and the selection come from the answer.
///
/// A request the phone refuses (a folder into itself, say) is counted among
/// the failures so the others still go ahead; the refusal itself is returned
/// when it is the only request, where its hint is worth reading in full, or
/// when it means the phone is gone.
async fn paste_items(service: Arc<dyn Service>, requests: Vec<dto::PasteRequest>) -> PasteEnd {
    let alone = requests.len() == 1;
    let mut total: Option<dto::PasteSummary> = None;
    // Each request with what it did, for undoing a move; `None` once any
    // request was refused outright.
    let mut done: Option<Vec<(dto::PasteRequest, dto::PasteSummary)>> = Some(Vec::new());

    for request in requests {
        let summary = match service.paste(&request).await {
            Ok(summary) => {
                if let Some(done) = &mut done {
                    done.push((request.clone(), summary.clone()));
                }
                summary
            }
            // A phone that can't copy or move at all says so before it touches
            // anything: the copy goes through the Mac instead.
            Err(dto::Error::Unsupported) if total.is_none() => return PasteEnd::ViaMac,
            Err(refusal) => {
                let response = error::to_error_response(refusal.clone());
                if alone || response.device_lost {
                    return PasteEnd::Done(Err(response));
                }
                done = None;
                dto::PasteSummary {
                    mode: request.mode,
                    done: 0,
                    renamed: 0,
                    into: request.to.clone(),
                    names: Vec::new(),
                    failures: request
                        .names
                        .iter()
                        .map(|name| dto::FileFailure {
                            label: name.clone(),
                            error: refusal.clone(),
                        })
                        .collect(),
                }
            }
        };
        match &mut total {
            None => total = Some(summary),
            Some(total) => {
                total.done += summary.done;
                total.renamed += summary.renamed;
                total.names.extend(summary.names);
                total.failures.extend(summary.failures);
            }
        }
    }
    let total = total.expect("a paste has at least one request");
    let mut response = mapper::to_paste_response(&total);
    response.undo = done.as_deref().and_then(undo::of_paste);
    PasteEnd::Done(Ok(response))
}

/// How asking the phone to paste ended.
enum PasteEnd {
    /// It was done, or refused: what to tell the user.
    Done(Result<PasteResponse, ErrorResponse>),
    /// The phone can't do it by itself; the app does it through the Mac.
    ViaMac,
}

#[cfg(test)]
impl PasteEnd {
    /// What the user is told; panics if the paste went through the Mac.
    fn done(self) -> Result<PasteResponse, ErrorResponse> {
        match self {
            PasteEnd::Done(result) => result,
            PasteEnd::ViaMac => panic!("the paste was handed over to the Mac"),
        }
    }
}

/// Dims `base` and shows `dialog` on top; the dim layer swallows clicks so
/// the pane behind can't change while a dialog is open.
fn modal<'a>(base: Element<'a, Message>, dialog: Element<'a, Message>) -> Element<'a, Message> {
    stack![
        base,
        opaque(center(opaque(dialog)).style(|_theme| {
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

/// Fetches one thumbnail; a failure just means the tile shows its kind.
async fn fetch_thumbnail(
    service: Arc<dyn Service>,
    volume_id: u64,
    path: String,
    entry: dto::Entry,
) -> (String, String, Result<Option<Picture>, ErrorResponse>) {
    let picture = match service.thumbnail(volume_id, &path, &entry).await {
        Ok(thumbnail) => Ok(thumbnail.map(mapper::to_picture)),
        // A phone that is gone or silent is worth saying; anything else just
        // means no picture.
        Err(err) => {
            let error = error::to_error_response(err);
            if error.device_lost {
                Err(error)
            } else {
                Ok(None)
            }
        }
    };
    (path, entry.name, picture)
}

/// Lists one folder; the handler sorts it.
/// What a folder listing has to say, as the pane hears it.
enum Piece {
    /// Part of the folder: everything so far, and how far along the phone is.
    Partial {
        entries: Vec<dto::Entry>,
        loaded: usize,
        total: usize,
    },
    /// The whole folder, or why it can't be shown.
    Finished(Result<Vec<dto::Entry>, ErrorResponse>),
}

impl Piece {
    fn into_message(self, volume_id: u64, path: &str) -> Message {
        match self {
            Piece::Partial {
                entries,
                loaded,
                total,
            } => Message::Loading {
                volume_id,
                path: path.to_owned(),
                entries,
                loaded,
                total,
            },
            Piece::Finished(result) => Message::Listed {
                volume_id,
                path: path.to_owned(),
                result,
            },
        }
    }
}

/// Where a folder listing is up to.
enum Reading {
    Opening {
        service: Arc<dyn Service>,
        volume_id: u64,
        path: String,
        show_hidden: bool,
        cancel: dto::Cancel,
        allow_remembered: bool,
    },
    Receiving {
        listing: Box<dyn Listing>,
        so_far: Vec<dto::Entry>,
    },
    Over,
}

/// A folder as it arrives. Each piece carries everything received so far, so
/// a late piece from a listing the pane has moved on from can never add rows
/// twice; the last piece is the whole folder. The stream ends quietly when the
/// listing is cancelled.
fn list_updates(
    service: Arc<dyn Service>,
    volume_id: u64,
    path: String,
    show_hidden: bool,
    cancel: dto::Cancel,
    allow_remembered: bool,
) -> BoxStream<'static, Piece> {
    let start = Reading::Opening {
        service,
        volume_id,
        path,
        show_hidden,
        cancel,
        allow_remembered,
    };
    stream::unfold(start, |reading| async move {
        let (mut listing, so_far) = match reading {
            Reading::Opening {
                service,
                volume_id,
                path,
                show_hidden,
                cancel,
                allow_remembered,
            } => match service
                .open_listing(volume_id, &path, show_hidden, cancel, allow_remembered)
                .await
            {
                Ok(listing) => (listing, Vec::new()),
                Err(err) => {
                    let failed = Piece::Finished(Err(error::to_error_response(err)));
                    return Some((failed, Reading::Over));
                }
            },
            Reading::Receiving { listing, so_far } => (listing, so_far),
            Reading::Over => return None,
        };
        let mut so_far = so_far;

        match listing.next().await? {
            Err(err) => Some((
                Piece::Finished(Err(error::to_error_response(err))),
                Reading::Over,
            )),
            Ok(update) => {
                if update.restart {
                    so_far.clear();
                }
                so_far.extend(update.entries);
                if update.done {
                    return Some((Piece::Finished(Ok(so_far)), Reading::Over));
                }
                let piece = Piece::Partial {
                    entries: so_far.clone(),
                    loaded: update.loaded,
                    total: update.total,
                };
                Some((piece, Reading::Receiving { listing, so_far }))
            }
        }
    })
    .boxed()
}

impl Handler {
    /// The list: one fixed-height row per file. Only the rows near the screen
    /// are built; spacers of the same total height stand in for the rest.
    fn list_rows(&self, fit: Fit, height: f32) -> Element<'_, Message> {
        if !self.groups.is_empty() {
            return self.grouped_rows(fit, height);
        }
        let style = row_style(self.density);
        let range = window::rows(self.viewport.0, height, style.height, self.rows.len());
        let above = range.start as f32 * style.height;
        let below = (self.rows.len() - range.end) as f32 * style.height;
        let first = range.start;
        let shown = self.rows[range].iter().enumerate().map(|(at, entry)| {
            let index = first + at;
            let look = self.look(entry);
            let joined = self.joined(index, look);
            entry_row(entry, look, fit, style, striped(index), joined)
        });
        scrollable(
            column![space().height(above)]
                .extend(shown)
                .push(space().height(below)),
        )
        .id(self.list.clone())
        .on_scroll(Message::Scrolled)
        .height(Length::Fill)
        .into()
    }

    /// The list grouped: its lines near the screen, a header (a row's
    /// height, a click selecting its group) or a row.
    fn grouped_rows(&self, fit: Fit, height: f32) -> Element<'_, Message> {
        let style = row_style(self.density);
        let lines = groups::list_lines(&self.groups, self.rows.len());
        let range = window::rows(self.viewport.0, height, style.height, lines);
        let above = range.start as f32 * style.height;
        let below = (lines - range.end) as f32 * style.height;
        let shown = range.map(|line| match groups::list_line(&self.groups, line) {
            groups::Line::Header(g) => group_header(g, &self.groups[g], style.height),
            groups::Line::Row(index) => {
                let entry = &self.rows[index];
                let look = self.look(entry);
                let joined = self.joined(index, look);
                entry_row(entry, look, fit, style, striped(index), joined)
            }
        });
        scrollable(
            column![space().height(above)]
                .extend(shown)
                .push(space().height(below)),
        )
        .id(self.list.clone())
        .on_scroll(Message::Scrolled)
        .height(Length::Fill)
        .into()
    }

    /// Selected rows next to each other make one block, as in Finder: the
    /// corners they share are square. Asked only of the rows being built.
    fn joined(&self, index: usize, look: Look) -> Joined {
        let selected_at = |index: Option<usize>| {
            index
                .and_then(|index| self.rows.get(index))
                .is_some_and(|row| self.selection.contains(&row.name))
        };
        // A header between two rows parts them.
        let starts_group = |index: usize| self.groups.iter().any(|group| group.start == index);
        Joined {
            above: look.selected && !starts_group(index) && selected_at(index.checked_sub(1)),
            below: look.selected && !starts_group(index + 1) && selected_at(index.checked_add(1)),
        }
    }

    /// The grid: fixed-size tiles, `columns` to a row, built like the list.
    /// Each row of tiles is padded to `self.tile.row_height()`, so a row's place
    /// is exactly its index times that.
    fn tiles(&self, columns: usize, height: f32) -> Element<'_, Message> {
        if !self.groups.is_empty() {
            return self.grouped_tiles(columns, height);
        }
        let total_rows = self.rows.len().div_ceil(columns);
        let window = window::rows(self.viewport.0, height, self.tile.row_height(), total_rows);
        let first_tile = (window.start * columns).min(self.rows.len());
        let last_tile = (window.end * columns).min(self.rows.len());

        let shown = self.rows[first_tile..last_tile]
            .chunks(columns)
            .map(|chunk| {
                container(
                    row(chunk.iter().map(|entry| {
                        let thumb = self.thumbnails.get(&entry.name);
                        tile(entry, thumb, self.look(entry), self.tile)
                    }))
                    .spacing(grid::SPACING),
                )
                .height(self.tile.row_height())
                .into()
            });
        let above = window.start as f32 * self.tile.row_height();
        let below = (total_rows - window.end) as f32 * self.tile.row_height();
        // Fill, so the scrollbar sits at the pane's edge, not beside the tiles.
        scrollable(
            column![space().height(above)]
                .extend(shown)
                .push(space().height(below)),
        )
        .id(self.list.clone())
        .on_scroll(Message::Scrolled)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Handler {
    /// The grid grouped: each group starts a new line of tiles under its
    /// header, a line of the grid's height; only lines near the screen are
    /// built.
    fn grouped_tiles(&self, columns: usize, height: f32) -> Element<'_, Message> {
        let row_height = self.tile.row_height();
        let frame = groups::GridFrame::new(&self.groups, columns, row_height, GRID_HEADER);
        let window = frame.window(self.viewport.0, height);
        let above = frame.top(window.start);
        let below = frame.total() - frame.top(window.end);
        let shown = window.map(|line| {
            match groups::grid_line(&self.groups, columns, &frame.starts, line) {
                groups::GridLine::Header(g) => group_header(g, &self.groups[g], GRID_HEADER),
                groups::GridLine::Tiles { first, end } => container(
                    row(self.rows[first..end].iter().map(|entry| {
                        let thumb = self.thumbnails.get(&entry.name);
                        tile(entry, thumb, self.look(entry), self.tile)
                    }))
                    .spacing(grid::SPACING),
                )
                .height(row_height)
                .into(),
            }
        });
        scrollable(
            column![space().height(above)]
                .extend(shown)
                .push(space().height(below)),
        )
        .id(self.list.clone())
        .on_scroll(Message::Scrolled)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

/// How tall a group's header is in the grid (in the list, a row's height).
const GRID_HEADER: f32 = 32.0;

/// A group's header: its label and what it holds; a click selects the
/// group (⇧ adds it, ⌘ toggles it).
fn group_header(g: usize, group: &groups::Group, height: f32) -> Element<'_, Message> {
    button(
        row![
            text(&group.label).size(13),
            text(group.summary()).size(12).style(text::secondary),
        ]
        .spacing(8)
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .style(button::text)
    .padding([0, 4])
    .height(height)
    .width(Length::Fill)
    .on_press(Message::Request(Request::SelectGroup(g)))
    .into()
}

/// A file's badge on a tile with no thumbnail: its extension on its kind's
/// colour, and the kind's word under it.
fn badge_tile(badge: &Badge) -> Element<'_, Message> {
    let colour = badge_colour(badge.kind);
    let label = if badge.label.is_empty() {
        "?".to_owned()
    } else {
        badge.label.clone()
    };
    column![
        container(
            text(label)
                .size(15)
                .style(move |theme: &iced::Theme| text::Style {
                    color: Some(match colour {
                        Some(_) => Color::WHITE,
                        None => theme.extended_palette().background.base.text,
                    }),
                })
        )
        .padding([4, 8])
        .style(move |theme: &iced::Theme| container::Style {
            background: Some(
                colour
                    .unwrap_or(theme.extended_palette().background.strong.color)
                    .into()
            ),
            border: iced::Border {
                radius: 4.0.into(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        }),
        text(badge.kind.word()).size(11).style(text::secondary),
    ]
    .spacing(6)
    .align_x(Alignment::Center)
    .into()
}

/// A column being resized: which edge, the widths when it started, where the
/// pointer first was (its first report), and the list's width then.
struct Resizing {
    edge: layout::Edge,
    start: dto::ColumnWidths,
    anchor: Option<f32>,
    width: f32,
}

/// A grid tile: the thumbnail, or the file's kind, over its name.
fn tile<'a>(
    entry: &'a EntryResponse,
    thumb: Option<&'a Thumb>,
    look: Look,
    size: grid::Tile,
) -> Element<'a, Message> {
    let preview: Element<'a, Message> = match thumb {
        Some(Thumb::Ready(picture)) => image(picture.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        _ => match &entry.badge {
            Some(badge) => center(badge_tile(badge)).into(),
            None => center(glyph(true, None, false, 44.0)).into(),
        },
    };

    let content = column![
        container(preview)
            .width(Length::Fill)
            .height(Length::Fixed(size.height() - 40.0))
            .style(container::bordered_box),
        with_whole_name(
            middle_text(&entry.name, 12, name_style(look.dim)),
            &entry.name,
            12.0,
            size.width - 12.0,
        ),
    ]
    .spacing(6)
    .align_x(Alignment::Center);

    let cell = container(content)
        .padding(6)
        .width(Length::Fixed(size.width))
        .height(Length::Fixed(size.height()))
        .clip(true)
        .style(cell_style(look));

    pressable(mouse_area(cell), entry, look).into()
}

/// What a tile or row is filled with: the colour of a drop target beats the
/// selection's, so it is plain where the drop would land.
fn cell_style(look: Look) -> fn(&iced::Theme) -> container::Style {
    if look.target {
        container::success
    } else if look.selected {
        container::primary
    } else {
        container::transparent
    }
}

/// What a button on a row or tile does, and, while one is down, what the
/// pointer crossing rows means (a drag to a folder).
fn pressable<'a>(
    area: iced::widget::MouseArea<'a, Message>,
    entry: &EntryResponse,
    look: Look,
) -> iced::widget::MouseArea<'a, Message> {
    tracked(
        area.on_press(Message::Request(Request::Select(entry.name.clone()))),
        entry,
        look,
    )
}

/// What a row or tile does besides a press: opens on a double click, says
/// it was right-clicked, and, while a button is down, what the pointer
/// crossing it means (a drag to a folder).
fn tracked<'a>(
    area: iced::widget::MouseArea<'a, Message>,
    entry: &EntryResponse,
    look: Look,
) -> iced::widget::MouseArea<'a, Message> {
    let name = || entry.name.clone();
    let area = area
        .on_double_click(Message::Request(Request::Open(name())))
        .on_right_press(Message::Request(Request::MenuOn(name())));
    match look.pointer {
        Pointer::Idle => area,
        // Only while a button is down are rows asked to report the pointer
        // coming and going, so moving over a list costs nothing otherwise.
        Pointer::Armed => area
            .on_enter(Message::Request(Request::PointerEntered(name())))
            .on_exit(Message::Request(Request::PointerLeft(name()))),
        // Mid-drag every movement says which row it is over, not only crossing
        // into one: the rows under a still pointer can change (a scroll), and
        // the next movement must put the target right.
        Pointer::Dragging => {
            let over = entry.name.clone();
            area.on_enter(Message::Request(Request::PointerEntered(name())))
                .on_exit(Message::Request(Request::PointerLeft(name())))
                .on_move(move |_| Message::Request(Request::PointerEntered(over.clone())))
        }
    }
}

/// What a row listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pointer {
    /// No button is down on a row: nothing about the pointer is reported.
    Idle,
    /// A button is down on a row: leaving it starts a drag.
    Armed,
    /// A drag is under way.
    Dragging,
}

/// How a row or tile is drawn.
#[derive(Debug, Clone, Copy)]
struct Look {
    selected: bool,
    /// Cut: drawn dim.
    dim: bool,
    /// The folder a drop would land in.
    target: bool,
    pointer: Pointer,
}

/// Items waiting to be moved are drawn dim, as Finder does for a cut.
fn name_style(is_cut: bool) -> fn(&iced::Theme) -> text::Style {
    if is_cut {
        text::secondary
    } else {
        text::default
    }
}

/// The colour of a row's text. On the selection or a drop target it takes
/// the fill's own text colour (an explicit grey there would be unreadable);
/// elsewhere the name is plain, and the other columns and a cut are grey.
fn row_text(look: Look, detail: bool) -> fn(&iced::Theme) -> text::Style {
    if look.selected || look.target {
        text::default
    } else {
        name_style(look.dim || detail)
    }
}

/// Whether the rows above and below a selected row are selected too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Joined {
    above: bool,
    below: bool,
}

/// A row's fill: a drop target beats the selection, which beats the stripe.
/// Rounded like Finder's, with the fill's own text colour; a run of selected
/// rows is one block, rounded only at its ends.
fn row_fill(
    look: Look,
    striped: bool,
    joined: Joined,
) -> impl Fn(&iced::Theme) -> container::Style {
    move |theme| {
        let palette = theme.extended_palette();
        let (fill, text_color) = if look.target {
            (palette.success.base.color, Some(palette.success.base.text))
        } else if look.selected {
            (palette.primary.base.color, Some(palette.primary.base.text))
        } else if striped {
            (stripe(palette), None)
        } else {
            return container::Style::default();
        };
        let joined = if look.selected && !look.target {
            joined
        } else {
            Joined::default()
        };
        let corner = |shared: bool| if shared { 0.0 } else { 5.0 };
        container::Style {
            background: Some(fill.into()),
            text_color,
            border: iced::Border {
                radius: iced::border::Radius {
                    top_left: corner(joined.above),
                    top_right: corner(joined.above),
                    bottom_right: corner(joined.below),
                    bottom_left: corner(joined.below),
                },
                ..iced::Border::default()
            },
            ..container::Style::default()
        }
    }
}

/// The shade of every other row: a little off the pane's own background,
/// darker in light mode and lighter in dark mode.
fn stripe(palette: &iced::theme::palette::Extended) -> Color {
    let toward = if palette.is_dark {
        Color::WHITE
    } else {
        Color::BLACK
    };
    Color { a: 0.045, ..toward }
}

/// Finder's folder blue.
const FOLDER_BLUE: Color = Color::from_rgb(0.33, 0.65, 0.95);

/// Each kind's colour, the same in light and dark: a badge and a page's band.
fn badge_colour(kind: BadgeKind) -> Option<Color> {
    Some(match kind {
        BadgeKind::Photo => Color::from_rgb(0.30, 0.69, 0.40),
        BadgeKind::Video => Color::from_rgb(0.62, 0.40, 0.90),
        BadgeKind::Music => Color::from_rgb(0.95, 0.55, 0.20),
        BadgeKind::Document => Color::from_rgb(0.30, 0.56, 0.96),
        BadgeKind::Archive => Color::from_rgb(0.72, 0.55, 0.32),
        BadgeKind::App => Color::from_rgb(0.20, 0.74, 0.62),
        BadgeKind::Other => return None,
    })
}

/// A small folder or page drawn from boxes, always in a square of `side`, so
/// names line up whichever a row holds. A page is an outline, drawn in the
/// row's text colour on the selection so it shows on the fill, with a band
/// across its foot in its kind's colour (none for a file of no known kind).
fn glyph<'a>(
    is_folder: bool,
    badge: Option<&Badge>,
    on_fill: bool,
    side: f32,
) -> Element<'a, Message> {
    let band = badge.and_then(|badge| badge_colour(badge.kind));
    let shape: Element<'a, Message> = if is_folder {
        let part = |width: f32, height: f32| {
            container(space())
                .width(width)
                .height(height)
                .style(|_: &iced::Theme| container::Style {
                    background: Some(FOLDER_BLUE.into()),
                    border: iced::Border {
                        radius: 1.5.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                })
        };
        column![
            part((side * 0.4).round(), (side * 0.15).round()),
            part(side, (side * 0.6).round())
        ]
        .into()
    } else {
        let width = (side * 0.72).round();
        let height = (side * 0.9).round();
        let page = match band {
            Some(colour) => column![
                space().height(Length::Fill),
                container(space())
                    .width(Length::Fill)
                    .height((height * 0.38).round())
                    .style(move |_: &iced::Theme| container::Style {
                        background: Some(colour.into()),
                        border: iced::Border {
                            radius: iced::border::Radius::default().bottom(2.0),
                            ..iced::Border::default()
                        },
                        ..container::Style::default()
                    })
            ]
            .into(),
            None => Element::from(space()),
        };
        container(page)
            .width(width)
            .height(height)
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let line = if on_fill {
                    Color {
                        a: 0.85,
                        ..palette.primary.base.text
                    }
                } else {
                    palette.background.strong.color
                };
                container::Style {
                    border: iced::Border {
                        color: line,
                        width: 1.0,
                        radius: 2.0.into(),
                    },
                    ..container::Style::default()
                }
            })
            .into()
    };
    container(shape)
        .width(side)
        .height(side)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

fn entry_row(
    entry: &EntryResponse,
    look: Look,
    fit: Fit,
    style: RowStyle,
    striped: bool,
    joined: Joined,
) -> Element<'_, Message> {
    // Each cell is cut at its own column: a long name must not run on under
    // the next one. The name is cut in the middle (`middle_text`), the other
    // cells at their edge.
    let cell = |content: String, width: u16| {
        container(
            text(content)
                .size(style.detail)
                .wrapping(Wrapping::None)
                .style(row_text(look, true)),
        )
        .width(f32::from(width))
        .clip(true)
    };
    let name = row![
        glyph(
            entry.is_folder,
            entry.badge.as_ref(),
            look.selected || look.target,
            style.glyph
        ),
        with_whole_name(
            middle_text(&entry.name, style.name, row_text(look, false)),
            &entry.name,
            style.name as f32,
            name_room(fit, style),
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    // The name (and its glyph) is what drags the item; the rest of the row
    // starts a lasso, as in Finder's list.
    let name = mouse_area(name)
        .on_press(Message::Request(Request::Select(entry.name.clone())))
        .on_double_click(Message::Request(Request::Open(entry.name.clone())));
    let mut cells = row![container(name).width(Length::Fill).clip(true)];
    if fit.kind {
        cells = cells.push(cell(entry.kind.clone(), fit.kind_width));
    }
    if fit.size {
        cells = cells.push(cell(entry.size.clone(), fit.size_width));
    }
    let cells = cells
        .push(cell(entry.modified.clone(), fit.modified_width))
        .spacing(8)
        .align_y(Alignment::Center);

    // Inset a little, so the rounded fill doesn't touch the pane's sides; the
    // text still starts where the header's does.
    let cell = container(
        container(cells)
            .padding([0, 6])
            .height(Length::Fill)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .style(row_fill(look, striped, joined)),
    )
    .padding([0, 4])
    .height(style.height)
    .width(Length::Fill);

    // A button would swallow the clicks, so the row uses a mouse area to get
    // both single (select) and double (open) clicks.
    let press_row = Message::Request(Request::PressRow(entry.name.clone()));
    tracked(mouse_area(cell).on_press(press_row), entry, look)
        .interaction(match (look.pointer, entry.is_folder) {
            (Pointer::Dragging, _) => mouse::Interaction::Grabbing,
            (_, true) => mouse::Interaction::Pointer,
            (_, false) => mouse::Interaction::default(),
        })
        .into()
}

/// The room a list row's name has: the pane's width less the row's insets,
/// the glyph and the other columns shown.
fn name_room(fit: Fit, style: RowStyle) -> f32 {
    let columns = [
        (fit.kind, fit.kind_width),
        (fit.size, fit.size_width),
        (true, fit.modified_width),
    ]
    .iter()
    .filter(|(shown, _)| *shown)
    .map(|(_, width)| f32::from(*width) + 8.0)
    .sum::<f32>();
    f32::from(fit.width) - 20.0 - style.glyph - 6.0 - columns
}

/// `name`'s cell, showing the whole name on hover when it is probably cut.
fn with_whole_name<'a>(
    cell: impl Into<Element<'a, Message>>,
    name: &'a str,
    size: f32,
    room: f32,
) -> Element<'a, Message> {
    if !probably_cut(name, size, room) {
        return cell.into();
    }
    tooltip(
        cell,
        container(text(name).size(12))
            .padding([4, 8])
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The right-click menu: rows of commands with their shortcuts, a line
/// between sections. Disabled commands are dimmed and can't be clicked.
fn menu_box<'a>(sections: Vec<Vec<menu::Item>>) -> Element<'a, Message> {
    let mut rows = column![];
    for (index, section) in sections.into_iter().enumerate() {
        if index > 0 {
            rows = rows.push(
                container(rule::horizontal(1))
                    .padding([4, 6])
                    .height(menu::SEPARATOR_HEIGHT),
            );
        }
        for item in section {
            let label = row![
                text(item.label).size(13).width(Length::Fill),
                text(item.shortcut).size(13).style(text::secondary),
            ]
            .align_y(Alignment::Center);
            rows = rows.push(
                button(label)
                    .padding([4, 10])
                    .height(menu::ITEM_HEIGHT)
                    .width(Length::Fill)
                    .style(menu_item)
                    .on_press_maybe(
                        item.enabled
                            .then_some(Message::Request(Request::Menu(item.action))),
                    ),
            );
        }
    }
    // Opaque, so clicks between the rows don't reach the backdrop behind
    // the menu and close it.
    opaque(
        container(rows)
            .padding(menu::PADDING)
            .width(menu::WIDTH)
            .style(|theme: &iced::Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(palette.background.base.color.into()),
                    border: iced::Border {
                        color: palette.background.strong.color,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    shadow: iced::Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.25),
                        offset: iced::Vector::new(0.0, 4.0),
                        blur_radius: 16.0,
                    },
                    ..container::Style::default()
                }
            }),
    )
}

/// A menu row highlights under the cursor like a Mac menu.
fn menu_item(theme: &iced::Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = button::Style {
        background: None,
        text_color: palette.background.base.text,
        border: iced::Border {
            radius: 4.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    };
    match status {
        button::Status::Hovered | button::Status::Pressed => button::Style {
            background: Some(palette.primary.base.color.into()),
            text_color: palette.primary.base.text,
            ..base
        },
        button::Status::Disabled => button::Style {
            text_color: palette.background.base.text.scale_alpha(0.35),
            ..base
        },
        button::Status::Active => base,
    }
}

/// The pane's error. `why`, when the folder's own nature explains a
/// refusal (Android's app folders), takes the place of the usual title and
/// advice, which would send the user unplugging a phone that is fine; a lost
/// phone is still said as one.
fn failure<'a>(error: &'a ErrorResponse, why: Option<&'static str>) -> Element<'a, Message> {
    let (title, hint) = match why {
        Some(why) if !error.device_lost => (mapper::app_folders_refused(), why),
        _ => (error.title.as_str(), error.hint.as_str()),
    };
    let try_again = button(text(t("label.try_again"))).on_press(Message::Request(Request::Refresh));
    let go_to_top = button(text(t("label.go_to_top")))
        .style(button::secondary)
        .on_press(Message::Request(Request::GoTo("/".to_owned())));
    // macOS refused the folder: the way in is a switch in System Settings,
    // so that comes first.
    let buttons = match error.privacy {
        Some(pane) => row![
            button(text(t("label.open_privacy_settings")))
                .style(button::primary)
                .on_press(Message::Request(Request::OpenPrivacySettings(pane))),
            try_again.style(button::secondary),
            go_to_top,
        ],
        None => row![try_again.style(button::primary), go_to_top],
    };
    let message = column![text(title).size(22), text(hint).style(text::secondary)]
        .spacing(12)
        .padding([0, 12]);
    let content = column![
        container(scrollable(message)).center_y(Length::Fill),
        container(buttons.spacing(8).wrap().vertical_spacing(8)).padding([0, 12]),
    ]
    .spacing(12)
    .max_width(460);
    container(content)
        .center_x(Length::Fill)
        .padding([12, 0])
        .into()
}

fn centered<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).center(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::explorer::request::{ManyField, ManyMode};
    use crate::handler::testing::{FakeExplorer, drain, file, folder, piece};

    fn handler() -> Handler {
        Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))))
    }

    fn storage() -> Volume {
        Volume {
            id: 1,
            name: "Internal".into(),
            home: None,
            deletes_permanently: true,
            transfer_action: Some("label.download"),
            shows_in_finder: false,
            phone_tools: true,
            hides_app_folders: false,
            pastes_within: true,
        }
    }

    fn mac() -> Volume {
        Volume {
            id: 0,
            name: "Mac".into(),
            home: Some("/Users/me".into()),
            deletes_permanently: false,
            transfer_action: None,
            shows_in_finder: true,
            phone_tools: false,
            hides_app_folders: false,
            pastes_within: true,
        }
    }

    fn listed(path: &str, entries: &[dto::Entry]) -> Message {
        Message::Listed {
            volume_id: 1,
            path: path.into(),
            result: Ok(entries.to_vec()),
        }
    }

    fn sized(name: &str, size_bytes: u64) -> dto::Entry {
        dto::Entry {
            size_bytes,
            ..file(name)
        }
    }

    /// A handler showing `path` on storage 1 with DCIM/ and notes.txt in it.
    fn browsing(path: &str) -> Handler {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(Message::Request(Request::GoTo(path.into())));
        let _ = handler.update(listed(path, &[file("notes.txt"), folder("DCIM")]));
        handler
    }

    fn path(handler: &Handler) -> &str {
        &handler.location.as_ref().unwrap().path
    }

    fn names(handler: &Handler) -> Vec<&str> {
        handler.rows.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn opening_a_volume_loads_its_start_folder() {
        let mut handler = handler();

        let _ = handler.open(storage(), "/".into());

        assert!(handler.is_open());
        assert_eq!(path(&handler), "/");
        assert!(handler.loading);
    }

    #[test]
    fn a_listing_for_the_current_folder_is_shown_sorted() {
        let handler = browsing("/");

        assert_eq!(names(&handler), ["DCIM", "notes.txt"]);
        assert!(!handler.loading);
        assert!(handler.error.is_none());
    }

    #[test]
    fn a_listing_for_a_folder_already_left_is_ignored() {
        let mut handler = browsing("/DCIM");

        let _ = handler.update(listed("/", &[file("old.txt")]));

        assert_eq!(names(&handler), ["DCIM", "notes.txt"]);
    }

    #[test]
    fn a_listing_for_another_storage_is_ignored() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Listed {
            volume_id: 2,
            path: "/".into(),
            result: Ok(vec![]),
        });

        assert_eq!(names(&handler), ["DCIM", "notes.txt"]);
    }

    #[test]
    fn a_failed_listing_shows_the_error_and_no_entries() {
        let mut handler = browsing("/");
        let _ = handler.update(Message::Request(Request::Refresh));

        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Err(error::to_error_response(dto::Error::PathNotFound(
                "/".into(),
            ))),
        });

        assert!(handler.rows.is_empty());
        assert_eq!(handler.error.as_ref().unwrap().title, "This folder is gone");
    }

    #[test]
    fn opening_a_folder_enters_it() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::Open("DCIM".into())));

        assert_eq!(path(&handler), "/DCIM");
        assert!(handler.rows.is_empty());
        assert!(handler.loading);
    }

    #[test]
    fn opening_a_file_stays_put() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::Open("notes.txt".into())));

        assert_eq!(path(&handler), "/");
    }

    #[test]
    fn up_goes_to_the_parent_folder() {
        let mut handler = browsing("/DCIM/Camera");

        let _ = handler.update(Message::Request(Request::Up));

        assert_eq!(path(&handler), "/DCIM");
    }

    #[test]
    fn up_at_the_root_stays_put() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::Up));

        assert_eq!(path(&handler), "/");
        assert!(!handler.loading);
    }

    #[test]
    fn selecting_highlights_an_entry() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::Select("notes.txt".into())));

        assert_eq!(handler.selection.single(), Some("notes.txt"));
    }

    #[test]
    fn selection_survives_a_refresh_that_keeps_the_entry() {
        let mut handler = browsing("/");
        let _ = handler.update(Message::Request(Request::Select("notes.txt".into())));

        let _ = handler.update(Message::Request(Request::Refresh));
        let _ = handler.update(listed("/", &[file("notes.txt")]));

        assert_eq!(handler.selection.single(), Some("notes.txt"));
    }

    #[test]
    fn selection_is_dropped_when_the_entry_disappears() {
        let mut handler = browsing("/");
        let _ = handler.update(Message::Request(Request::Select("notes.txt".into())));

        let _ = handler.update(Message::Request(Request::Refresh));
        let _ = handler.update(listed("/", &[folder("DCIM")]));

        assert!(handler.selection.is_empty());
    }

    #[test]
    fn toggling_hidden_files_reloads() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::SetShowHidden(true)));

        assert!(handler.show_hidden);
        assert!(handler.loading);
    }

    #[test]
    fn sorting_by_size_shows_largest_first_without_reloading() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[sized("small", 1), sized("big", 9)]));

        let _ = handler.update(Message::Request(Request::SortBy(dto::SortKey::Size)));

        assert_eq!(names(&handler), ["big", "small"]);
        assert!(!handler.loading);
    }

    #[test]
    fn sorting_the_same_column_again_reverses_it() {
        let mut handler = browsing("/");

        let _ = handler.update(Message::Request(Request::SortBy(dto::SortKey::Name)));

        assert!(handler.sort.descending);
        assert_eq!(names(&handler), ["DCIM", "notes.txt"]);
    }

    #[test]
    fn turning_off_folders_first_mixes_them_with_files() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[folder("Zoo"), file("apple.txt")]));

        let _ = handler.update(Message::Request(Request::SetFoldersFirst(false)));

        assert_eq!(names(&handler), ["apple.txt", "Zoo"]);
    }

    #[test]
    fn the_sort_is_kept_across_folders() {
        let mut handler = browsing("/");
        let _ = handler.update(Message::Request(Request::SortBy(dto::SortKey::Size)));

        let _ = handler.update(Message::Request(Request::GoTo("/DCIM".into())));
        let _ = handler.update(listed("/DCIM", &[sized("small", 1), sized("big", 9)]));

        assert_eq!(names(&handler), ["big", "small"]);
    }

    #[test]
    fn requests_without_an_open_storage_are_ignored() {
        let mut handler = handler();

        let _ = handler.update(Message::Request(Request::Refresh));
        let _ = handler.update(listed("/", &[file("a.txt")]));

        assert!(!handler.is_open());
        assert!(!handler.loading);
        assert!(handler.rows.is_empty());
    }

    #[test]
    fn closing_forgets_the_folder() {
        let mut handler = browsing("/DCIM");

        handler.close();

        assert!(!handler.is_open());
        assert!(handler.rows.is_empty());
        assert!(handler.selection.is_empty());
    }

    #[test]
    fn every_state_renders() {
        let mut handler = handler();
        let _ = handler.view();

        let _ = handler.open(storage(), "/".into());
        let _ = handler.view();

        let _ = handler.update(listed("/", &[]));
        let _ = handler.view();

        let mut handler = browsing("/DCIM/Camera");
        let _ = handler.update(Message::Request(Request::Select("notes.txt".into())));
        let _ = handler.view();

        let mut mac_pane = self::handler();
        let _ = mac_pane.open(mac(), "/Users/me/Music/Albums/2026/Live".into());
        let _ = mac_pane.view();

        let _ = handler.update(Message::Request(Request::Refresh));
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/DCIM/Camera".into(),
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });
        let _ = handler.view();
    }

    #[test]
    fn a_volume_can_start_below_its_root() {
        let mut handler = handler();

        let _ = handler.open(mac(), "/Users/me".into());

        assert_eq!(path(&handler), "/Users/me");
        let _ = handler.update(Message::Request(Request::Up));
        assert_eq!(path(&handler), "/Users");
    }

    fn request(handler: &mut Handler, request: Request) {
        let _ = handler.update(Message::Request(request));
    }

    fn edited(path: &str, result: Result<Option<String>, ErrorResponse>) -> Message {
        Message::Edited {
            volume_id: 1,
            path: path.into(),
            result,
            undo: None,
        }
    }

    fn change(handler: &Handler) -> Option<&Change> {
        handler.editor.as_ref().map(|editor| &editor.change)
    }

    #[test]
    fn new_folder_opens_a_dialog_with_finders_default_name() {
        let mut handler = browsing("/");

        request(&mut handler, Request::NewFolder);

        let editor = handler.editor.as_ref().unwrap();
        assert_eq!(editor.change, Change::NewFolder);
        assert_eq!(editor.name, "untitled folder");
        assert!(!editor.busy);
    }

    #[test]
    fn typing_updates_the_name_and_clears_an_old_error() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::Submit);
        let _ = handler.update(edited(
            "/",
            Err(error::to_error_response(dto::Error::NameTaken("x".into()))),
        ));

        request(&mut handler, Request::EditName("Trip".into()));

        let editor = handler.editor.as_ref().unwrap();
        assert_eq!(editor.name, "Trip");
        assert!(editor.error.is_none());
    }

    #[test]
    fn submitting_marks_the_dialog_busy_and_ignores_more_input() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);

        request(&mut handler, Request::Submit);
        request(&mut handler, Request::EditName("changed".into()));
        request(&mut handler, Request::Cancel);
        let _ = handler.update(Message::Request(Request::Submit));

        let editor = handler.editor.as_ref().unwrap();
        assert!(editor.busy);
        assert_eq!(editor.name, "untitled folder");
    }

    #[test]
    fn a_successful_edit_closes_the_dialog_selects_and_reloads() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::Submit);

        let _ = handler.update(edited("/", Ok(Some("Trip".into()))));
        let _ = handler.update(listed("/", &[folder("Trip"), file("notes.txt")]));

        assert!(handler.editor.is_none());
        assert_eq!(handler.selection.single(), Some("Trip"));
        assert_eq!(names(&handler), ["Trip", "notes.txt"]);
    }

    #[test]
    fn a_failed_edit_keeps_the_dialog_open_with_the_reason() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::Submit);

        let _ = handler.update(edited(
            "/",
            Err(error::to_error_response(dto::Error::NameTaken(
                "DCIM".into(),
            ))),
        ));

        let editor = handler.editor.as_ref().unwrap();
        assert!(!editor.busy);
        assert_eq!(editor.error.as_ref().unwrap().title, "That name is taken");
    }

    #[test]
    fn an_edit_answer_for_another_folder_is_ignored() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::Submit);

        let _ = handler.update(edited("/DCIM", Ok(None)));

        assert!(handler.editor.as_ref().unwrap().busy);
    }

    #[test]
    fn a_failure_after_the_dialog_closed_is_ignored() {
        let mut handler = browsing("/");

        let _ = handler.update(edited(
            "/",
            Err(error::to_error_response(dto::Error::Timeout)),
        ));

        assert!(handler.editor.is_none());
    }

    #[test]
    fn cancel_closes_the_dialog() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);

        request(&mut handler, Request::Cancel);

        assert!(handler.editor.is_none());
    }

    #[test]
    fn rename_and_delete_need_a_selection() {
        let mut handler = browsing("/");

        request(&mut handler, Request::Rename);
        request(&mut handler, Request::Delete);

        assert!(handler.editor.is_none());
    }

    #[test]
    fn rename_starts_from_the_selected_name() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("notes.txt".into()));

        request(&mut handler, Request::Rename);

        let editor = handler.editor.as_ref().unwrap();
        assert_eq!(
            editor.change,
            Change::Rename {
                original: "notes.txt".into()
            }
        );
        assert_eq!(editor.name, "notes.txt");
    }

    #[test]
    fn delete_on_the_phone_asks_first() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("notes.txt".into()));

        request(&mut handler, Request::Delete);

        assert_eq!(
            change(&handler),
            Some(&Change::Delete {
                names: vec!["notes.txt".into()],
                keep: Keep::No,
            })
        );
        assert!(!handler.editor.as_ref().unwrap().busy);
    }

    #[test]
    fn delete_on_the_mac_goes_straight_to_the_trash() {
        let mut handler = handler();
        let _ = handler.open(mac(), "/Users/me".into());
        let _ = handler.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt")]),
        });
        request(&mut handler, Request::Select("notes.txt".into()));

        request(&mut handler, Request::Delete);

        assert!(handler.editor.as_ref().unwrap().busy);
    }

    #[test]
    fn opening_another_volume_or_closing_drops_the_dialog() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        let _ = handler.open(storage(), "/".into());
        assert!(handler.editor.is_none());

        request(&mut handler, Request::NewFolder);
        handler.close();
        assert!(handler.editor.is_none());
    }

    #[test]
    fn device_lost_is_reported_for_listings_and_edits_only() {
        let lost = error::to_error_response(dto::Error::Disconnected);
        let other = error::to_error_response(dto::Error::ReadOnly);

        assert!(edited("/", Err(lost.clone())).device_lost().is_some());
        assert!(
            Message::Listed {
                volume_id: 1,
                path: "/".into(),
                result: Err(lost),
            }
            .device_lost()
            .is_some()
        );
        assert!(edited("/", Err(other)).device_lost().is_none());
        assert!(edited("/", Ok(None)).device_lost().is_none());
        assert!(Message::Request(Request::Refresh).device_lost().is_none());
    }

    #[tokio::test]
    async fn apply_sends_each_change_to_the_service() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));

        let made = apply(
            service.clone(),
            1,
            "/".into(),
            Action::MakeFolder("Trip".into()),
        )
        .await;
        let renamed = apply(
            service.clone(),
            1,
            "/".into(),
            Action::Rename {
                old: "a".into(),
                new: "b".into(),
            },
        )
        .await;
        let deleted = apply(
            service.clone(),
            1,
            "/".into(),
            Action::Delete(vec!["b".into()]),
        )
        .await;

        assert_eq!(made.unwrap(), Some("Trip".into()));
        assert_eq!(renamed.unwrap(), Some("b".into()));
        assert_eq!(deleted.unwrap(), None);
        assert_eq!(
            *service.edits.lock().unwrap(),
            ["mkdir 1 / Trip", "rename 1 / a -> b", "delete 1 / b"]
        );
    }

    #[tokio::test]
    async fn apply_reports_failures() {
        let service = Arc::new(FakeExplorer::failing_edits(dto::Error::ReadOnly));

        let error = apply(service, 1, "/".into(), Action::MakeFolder("x".into()))
            .await
            .unwrap_err();

        assert_eq!(error.title, "This storage is read-only");
    }

    #[test]
    fn every_dialog_renders() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("notes.txt".into()));

        request(&mut handler, Request::NewFolder);
        let _ = handler.view();
        request(&mut handler, Request::Submit);
        let _ = handler.view();
        let _ = handler.update(edited(
            "/",
            Err(error::to_error_response(dto::Error::ReadOnly)),
        ));
        let _ = handler.view();
        request(&mut handler, Request::Cancel);

        request(&mut handler, Request::Rename);
        let _ = handler.view();
        request(&mut handler, Request::Cancel);

        request(&mut handler, Request::Delete);
        let _ = handler.view();

        let mut mac_pane = self::handler();
        let _ = mac_pane.open(mac(), "/Users/me".into());
        let _ = mac_pane.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.txt")]),
        });
        request(&mut mac_pane, Request::Select("a.txt".into()));
        request(&mut mac_pane, Request::Delete);
        let _ = mac_pane.view();
    }

    #[test]
    fn selection_names_the_selected_entry_and_its_folder() {
        let mut handler = browsing("/DCIM");
        assert_eq!(handler.selection(), None);

        request(&mut handler, Request::Select("notes.txt".into()));

        assert_eq!(
            handler.selection(),
            Some(Selection {
                volume_id: 1,
                path: "/DCIM".into(),
                names: vec!["notes.txt".into()],
            })
        );
        assert_eq!(handler.current_path(), Some("/DCIM"));
    }

    #[test]
    fn a_closed_pane_has_no_selection_or_path() {
        let handler = handler();

        assert_eq!(handler.selection(), None);
        assert_eq!(handler.current_path(), None);
    }

    #[test]
    fn transfer_is_left_to_the_app() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("notes.txt".into()));

        request(&mut handler, Request::Transfer);

        assert!(handler.editor.is_none());
        assert!(!handler.loading);
    }

    #[test]
    fn refresh_and_select_reloads_and_selects_once_listed() {
        let mut handler = browsing("/");

        let _ = handler.refresh_and_select(Some("new.txt".into()));
        assert!(handler.loading);
        let _ = handler.update(listed("/", &[file("new.txt")]));

        assert_eq!(handler.selection.single(), Some("new.txt"));
    }

    #[test]
    fn show_item_opens_the_folder_and_selects_the_item_once_read() {
        let mut handler = browsing("/");

        let _ = handler.show_item("/DCIM/Camera".into(), "b.jpg".into());
        assert_eq!(
            handler.location().map(|(_, path)| path),
            Some("/DCIM/Camera")
        );
        let _ = handler.update(listed("/DCIM/Camera", &[file("a.jpg"), file("b.jpg")]));

        assert_eq!(handler.selection.single(), Some("b.jpg"));
        // Only once: a later reading of the folder leaves the selection be.
        let _ = handler.update(Message::Request(Request::Select("a.jpg".into())));
        let _ = handler.update(listed("/DCIM/Camera", &[file("a.jpg"), file("b.jpg")]));
        assert_eq!(handler.selection.single(), Some("a.jpg"));
    }

    // Kind at a glance.

    #[test]
    fn a_tile_without_a_thumbnail_shows_its_kind_and_a_folder_its_glyph() {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed(
            "/",
            &[
                folder("Camera"),
                file("trip.mp4"),
                file("WhatsApp.apk"),
                file("README"),
            ],
        ));
        let _ = handler.set_view(dto::ViewMode::Grid);

        let mut ui = iced_test::simulator(handler.view());

        for shown in ["MP4", "Video", "APK", "Android app", "?", "File"] {
            assert!(ui.find(shown).is_ok(), "{shown}");
        }
        assert!(ui.find("Folder").is_err(), "a folder is drawn, not written");
    }

    // Resizing columns.

    #[test]
    fn dragging_a_header_edge_resizes_the_column_until_the_button_comes_up() {
        let mut handler = browsing("/");
        assert!(!handler.is_resizing());

        request(&mut handler, Request::ResizeColumn(layout::Edge::Kind, 800));
        assert!(handler.is_resizing());
        let _ = handler.update(Message::ResizeMoved(500.0));
        assert_eq!(
            handler.column_widths(),
            dto::ColumnWidths::default(),
            "where it started"
        );
        let _ = handler.update(Message::ResizeMoved(470.0));
        assert_eq!(
            handler.column_widths().kind,
            150,
            "30 points left: Kind 30 wider"
        );
        handler.set_column_widths(dto::ColumnWidths::default());
        assert_eq!(
            handler.column_widths().kind,
            150,
            "settings don't pull the edge back from under the pointer"
        );

        request(&mut handler, Request::PointerReleased);
        assert!(!handler.is_resizing());
        let _ = handler.update(Message::ResizeMoved(300.0));
        assert_eq!(handler.column_widths().kind, 150, "the drag is over");

        request(&mut handler, Request::ResetColumns);
        assert_eq!(handler.column_widths(), dto::ColumnWidths::default());
    }

    #[test]
    fn the_gap_left_of_a_column_is_its_handle() {
        let mut handler = browsing("/");
        let pressed: Vec<Message> = {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(800.0, 400.0),
                handler.view(),
            );
            let kind = ui.find("Kind").unwrap().bounds();
            ui.point_at(iced::Point::new(kind.x - 4.0, kind.center_y()));
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };

        assert!(
            pressed.iter().any(|m| matches!(
                m,
                Message::Request(Request::ResizeColumn(layout::Edge::Kind, 800))
            )),
            "{pressed:?}"
        );
        for message in pressed {
            let _ = handler.update(message);
        }
        assert!(handler.is_resizing());
    }

    #[test]
    fn rows_and_header_line_up_at_the_widths_given() {
        let mut handler = browsing("/");
        handler.set_column_widths(dto::ColumnWidths {
            kind: 200,
            size: 60,
            modified: 150,
        });
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(900.0, 400.0),
            handler.view(),
        );
        let header = ui.find("Kind").unwrap().bounds();
        let cell = ui.find("TXT file").unwrap().bounds();

        assert!((header.x - cell.x).abs() < 1.0, "{header:?} {cell:?}");
    }

    // Undo.

    /// Runs a task and feeds back what it says.
    async fn settle_task(handler: &mut Handler, task: Task<Message>) {
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    #[tokio::test]
    async fn a_rename_is_kept_to_be_undone_and_undoing_it_renames_back() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("notes.txt")]));
        assert!(!handler.can_undo() && handler.changed_at() == 0);

        request(&mut handler, Request::Select("notes.txt".into()));
        request(&mut handler, Request::Rename);
        request(&mut handler, Request::EditName("plan.txt".into()));
        let submit = handler.update(Message::Request(Request::Submit));
        settle_task(&mut handler, submit).await;

        assert!(handler.can_undo());
        let first = handler.changed_at();
        assert!(first > 0);
        let undo = handler.undo();
        settle_task(&mut handler, undo).await;

        let edits = service.edits.lock().unwrap().clone();
        assert_eq!(
            edits.last().map(String::as_str),
            Some("rename 1 / plan.txt -> notes.txt"),
            "{edits:?}"
        );
        assert!(!handler.can_undo(), "once: there is no redo");
        let notice = handler.notice.as_ref().unwrap();
        assert_eq!(notice.title, "Undid the rename");
        assert!(!notice.problem);
    }

    #[test]
    fn a_delete_or_a_copy_leaves_nothing_to_undo_and_a_move_can_be() {
        let mut handler = browsing("/");
        let renamed = Undo::Renamed {
            folder: "/".into(),
            from: "a".into(),
            to: "b".into(),
        };
        let _ = handler.update(Message::Edited {
            volume_id: 1,
            path: "/".into(),
            result: Ok(Some("b".into())),
            undo: Some(renamed),
        });
        assert!(handler.can_undo());
        let after_rename = handler.changed_at();

        let _ = handler.update(Message::Edited {
            volume_id: 1,
            path: "/".into(),
            result: Ok(None),
            undo: None,
        });
        assert!(
            !handler.can_undo(),
            "the last change was a delete: can't undo"
        );
        assert!(
            handler.changed_at() > after_rename,
            "but it is the latest change"
        );

        let pasted = |undo| Message::Pasted {
            volume_id: 1,
            path: "/".into(),
            result: Ok(PasteResponse {
                select: vec!["a.jpg".into()],
                notice: NoticeResponse {
                    title: "Moved 1 item".into(),
                    details: Vec::new(),
                    problem: false,
                },
                undo,
            }),
        };
        let _ = handler.update(pasted(Some(Undo::Moved(Vec::new()))));
        assert!(handler.can_undo(), "a whole move can be undone");
        let _ = handler.update(pasted(None));
        assert!(!handler.can_undo(), "a copy can't");

        let failed = Message::Edited {
            volume_id: 1,
            path: "/".into(),
            result: Err(error::to_error_response(dto::Error::NameTaken("b".into()))),
            undo: None,
        };
        let before = handler.changed_at();
        let _ = handler.update(failed);
        assert_eq!(
            handler.changed_at(),
            before,
            "a refused change is no change"
        );
    }

    #[test]
    fn an_undo_that_finds_things_changed_says_so_and_nothing_else_happens() {
        let mut handler = browsing("/");
        let _ = handler.update(Message::Undone {
            volume_id: 1,
            what: "explorer.undo.moved",
            result: Err(error::to_error_response(dto::Error::ChangedSince(
                "a.jpg".into(),
            ))),
        });

        let notice = handler.notice.as_ref().unwrap();
        assert!(notice.problem);
        assert_eq!(notice.title, "Can't undo: it has changed since");
        assert!(notice.details[0].contains("a.jpg"));
        assert!(!handler.is_loading());

        let elsewhere = handler.update(Message::Undone {
            volume_id: 1,
            what: "explorer.undo.moved",
            result: Ok(("/Elsewhere".into(), vec!["a.jpg".into()])),
        });
        assert_eq!(elsewhere.units(), 0, "another folder: only the banner");
        assert_eq!(handler.notice.as_ref().unwrap().title, "Undid the move");
        let here = handler.update(Message::Undone {
            volume_id: 1,
            what: "explorer.undo.moved",
            result: Ok(("/".into(), vec!["notes.txt".into()])),
        });
        assert!(
            here.units() > 0 && handler.is_loading(),
            "this folder is read again"
        );
        assert_eq!(handler.selection.single(), Some("notes.txt"));
    }

    // A folder macOS refused.

    fn refused(path: &str) -> Message {
        Message::Listed {
            volume_id: 1,
            path: path.into(),
            result: Err(error::to_error_response(dto::Error::FolderAccessDenied(
                format!("/Users/me{path}"),
            ))),
        }
    }

    #[test]
    fn a_refused_folder_offers_the_settings_pane_that_lets_the_app_in() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(storage(), "/Desktop".into());
        let _ = handler.update(refused("/Desktop"));

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("Open Privacy Settings…").unwrap();
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert_eq!(
            *service.edits.lock().unwrap(),
            ["privacy settings FilesAndFolders"]
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Try again").is_ok() && ui.find("Go to top").is_ok());
    }

    #[test]
    fn a_refused_folder_is_read_again_when_the_app_comes_back_and_nothing_else_is() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(storage(), "/Desktop".into());
        let _ = handler.update(refused("/Desktop"));
        assert!(!handler.is_loading());

        assert!(handler.read_again_if_refused().units() > 0);
        assert!(handler.is_loading(), "asked again");

        let _ = handler.update(listed("/Desktop", &[file("a.txt")]));
        assert_eq!(
            handler.read_again_if_refused().units(),
            0,
            "a folder that opened is left alone"
        );

        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/Desktop".into(),
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });
        assert_eq!(handler.read_again_if_refused().units(), 0, "only a refusal");
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find("Open Privacy Settings…").is_err(),
            "nothing to allow"
        );
    }

    // Typing to select.

    fn typed(handler: &mut Handler, text: &str, at: Instant) {
        request(
            handler,
            Request::TypeAhead {
                text: text.into(),
                at,
            },
        );
    }

    #[test]
    fn typing_selects_the_first_name_that_starts_so_and_narrows_as_it_goes() {
        let mut handler = browsing("/");
        let _ = handler.update(listed(
            "/",
            &[
                file("Contacts.vcf"),
                file("camera.jpg"),
                file("Cat.png"),
                file("dog.png"),
            ],
        ));
        let start = Instant::now();
        let soon = |ms| start + Duration::from_millis(ms);

        typed(&mut handler, "c", start);
        assert_eq!(
            handler.selection.single(),
            Some("camera.jpg"),
            "in the order shown, either case"
        );
        typed(&mut handler, "a", soon(300));
        typed(&mut handler, "t", soon(600));
        assert_eq!(handler.selection.single(), Some("Cat.png"));

        typed(&mut handler, "d", soon(1_700));
        assert_eq!(
            handler.selection.single(),
            Some("dog.png"),
            "after a pause, a new name"
        );
        typed(&mut handler, "z", soon(1_900));
        assert_eq!(
            handler.selection.single(),
            Some("dog.png"),
            "no name starts \"dz\": the selection stays"
        );
    }

    #[test]
    fn typing_follows_the_sort_and_skips_what_the_filter_hides() {
        let mut handler = browsing("/");
        let mut small = file("photo-small.jpg");
        small.size_bytes = 10;
        let mut big = file("photo-big.jpg");
        big.size_bytes = 9_000;
        let _ = handler.update(listed("/", &[small, big, file("plan.pdf")]));
        request(&mut handler, Request::SortBy(dto::SortKey::Size));

        typed(&mut handler, "photo", Instant::now());
        assert_eq!(
            handler.selection.single(),
            Some("photo-big.jpg"),
            "biggest first, as shown"
        );

        request(&mut handler, Request::Filter("small".into()));
        typed(&mut handler, "p", Instant::now() + Duration::from_secs(5));
        assert_eq!(
            handler.selection.single(),
            Some("photo-small.jpg"),
            "only what is on screen"
        );
    }

    #[test]
    fn going_to_another_folder_starts_the_typing_again() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[folder("DCIM"), file("notes.txt")]));
        let now = Instant::now();
        typed(&mut handler, "d", now);
        request(&mut handler, Request::OpenSelected);
        let _ = handler.update(listed("/DCIM", &[file("a.jpg"), file("Camera")]));

        typed(&mut handler, "c", now + Duration::from_millis(100));

        assert_eq!(
            handler.selection.single(),
            Some("Camera"),
            "\"c\", not \"dc\""
        );
    }

    #[test]
    fn typing_in_a_big_folder_is_one_pass_over_the_rows() {
        let mut handler = browsing("/DCIM/Camera");
        let entries: Vec<dto::Entry> = (0..5_000)
            .map(|n| file(&format!("IMG_{n:04}.jpg")))
            .collect();
        let _ = handler.update(listed("/DCIM/Camera", &entries));
        let now = Instant::now();

        let started = Instant::now();
        for (i, letter) in ["i", "m", "g", "_", "4", "9", "9", "9"].iter().enumerate() {
            typed(
                &mut handler,
                letter,
                now + Duration::from_millis(i as u64 * 100),
            );
        }
        let took = started.elapsed();

        assert_eq!(handler.selection.single(), Some("IMG_4999.jpg"));
        assert!(took < Duration::from_secs(1), "{took:?} for eight keys");
        let _ = handler.view();
    }

    // Opening where the pane was left.

    #[test]
    fn only_the_folder_it_was_opened_at_falls_back_and_only_once() {
        let mut handler = handler();
        let _ = handler.open_with_fallback(storage(), "/Gone".into(), "/".into());
        let gone = |path: &str| Message::Listed {
            volume_id: 1,
            path: path.into(),
            result: Err(error::to_error_response(dto::Error::PathNotFound(
                path.into(),
            ))),
        };

        let _ = handler.update(gone("/Gone"));
        assert_eq!(handler.current_path(), Some("/"));
        let _ = handler.update(gone("/"));
        assert_eq!(
            handler.current_path(),
            Some("/"),
            "the fallback's own error stays"
        );
        assert!(handler.error.is_some());

        let _ = handler.open_with_fallback(storage(), "/DCIM".into(), "/".into());
        let _ = handler.update(listed("/DCIM", &[file("a.jpg")]));
        request(&mut handler, Request::GoTo("/Music".into()));
        let _ = handler.update(gone("/Music"));
        assert_eq!(
            handler.current_path(),
            Some("/Music"),
            "a folder gone later says so; it doesn't jump to the top"
        );

        let _ = handler.open_with_fallback(storage(), "/DCIM".into(), "/".into());
        request(&mut handler, Request::GoTo("/Music".into()));
        let _ = handler.update(gone("/Music"));
        assert_eq!(
            handler.current_path(),
            Some("/Music"),
            "moving on before the remembered folder answered drops the fallback"
        );
    }

    // Back and forward.

    fn path_of(handler: &Handler) -> Option<&str> {
        handler.location().map(|(_, path)| path)
    }

    #[test]
    fn back_and_forward_walk_the_folders_the_pane_went_through() {
        let mut handler = browsing("/");
        assert!(!handler.can_go_back() && !handler.can_go_forward());
        let _ = handler.update(listed("/", &[folder("DCIM"), folder("Music")]));
        request(&mut handler, Request::Open("DCIM".into()));
        let _ = handler.update(listed("/DCIM", &[folder("Camera")]));
        request(&mut handler, Request::Open("Camera".into()));
        request(&mut handler, Request::Refresh);
        assert!(handler.can_go_back(), "a refresh is not a step");

        request(&mut handler, Request::Back);
        assert_eq!(path_of(&handler), Some("/DCIM"));
        let _ = handler.update(listed("/DCIM", &[folder("Camera")]));
        assert_eq!(
            handler.selection.single(),
            Some("Camera"),
            "back up, the folder left is selected"
        );
        request(&mut handler, Request::Back);
        assert_eq!(path_of(&handler), Some("/"));
        request(&mut handler, Request::Back);
        assert_eq!(path_of(&handler), Some("/"), "nothing before the first");
        assert!(handler.can_go_forward() && !handler.can_go_back());

        request(&mut handler, Request::Forward);
        request(&mut handler, Request::Forward);
        assert_eq!(path_of(&handler), Some("/DCIM/Camera"));
        assert!(!handler.can_go_forward());

        request(&mut handler, Request::Back);
        request(&mut handler, Request::GoTo("/Music".into()));
        assert!(!handler.can_go_forward(), "a new way drops what was ahead");
        request(&mut handler, Request::Back);
        assert_eq!(path_of(&handler), Some("/DCIM"));
    }

    #[test]
    fn up_and_a_crumb_select_the_folder_left_but_going_down_selects_nothing() {
        let mut handler = browsing("/DCIM/Camera/2026");

        request(&mut handler, Request::GoTo("/DCIM".into()));
        let _ = handler.update(listed("/DCIM", &[folder("Camera"), folder("Other")]));
        assert_eq!(handler.selection.single(), Some("Camera"));

        request(&mut handler, Request::Open("Other".into()));
        let _ = handler.update(listed("/DCIM/Other", &[folder("Camera")]));
        assert_eq!(handler.selection.single(), None);

        request(&mut handler, Request::Up);
        let _ = handler.update(listed("/DCIM", &[folder("Camera"), folder("Other")]));
        assert_eq!(handler.selection.single(), Some("Other"));
    }

    #[test]
    fn another_volume_starts_a_history_of_its_own() {
        let mut handler = browsing("/");
        request(&mut handler, Request::GoTo("/DCIM".into()));
        assert!(handler.can_go_back());

        let _ = handler.open(storage(), "/".into());

        assert!(!handler.can_go_back() && !handler.can_go_forward());
        request(&mut handler, Request::GoTo("/DCIM".into()));
        handler.close();
        assert!(!handler.can_go_back(), "a closed pane goes nowhere");
    }

    #[test]
    fn the_toolbar_offers_back_and_forward_only_when_there_is_somewhere_to_go() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[folder("DCIM")]));
        let clicked = |handler: &Handler, label: &str| -> Vec<Message> {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click(label);
            ui.into_messages().collect()
        };
        assert!(
            clicked(&handler, "‹").is_empty(),
            "nothing to go back to: the button does nothing"
        );

        request(&mut handler, Request::GoTo("/DCIM".into()));
        let messages = clicked(&handler, "‹");
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Request(Request::Back)))
        );
        for message in messages {
            let _ = handler.update(message);
        }
        assert_eq!(path_of(&handler), Some("/"));
        assert!(
            clicked(&handler, "›")
                .iter()
                .any(|m| matches!(m, Message::Request(Request::Forward)))
        );
    }

    #[test]
    fn going_back_up_into_a_big_folder_finds_the_folder_left_at_once() {
        let mut handler = browsing("/DCIM/Camera");
        request(&mut handler, Request::Up);
        let mut entries: Vec<dto::Entry> = (0..5_000)
            .map(|n| file(&format!("IMG_{n:04}.jpg")))
            .collect();
        entries.push(folder("Camera"));

        let started = std::time::Instant::now();
        let _ = handler.update(listed("/DCIM", &entries));
        let took = started.elapsed();

        assert_eq!(handler.selection.single(), Some("Camera"));
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
        let _ = handler.view();
    }

    #[test]
    fn show_item_forgets_the_item_when_the_user_goes_elsewhere_or_it_is_gone() {
        let mut handler = browsing("/");
        let _ = handler.show_item("/DCIM".into(), "gone.jpg".into());
        let _ = handler.update(listed("/DCIM", &[file("a.jpg")]));
        assert_eq!(handler.selection.single(), None);

        let _ = handler.show_item("/DCIM".into(), "a.jpg".into());
        let _ = handler.update(Message::Request(Request::GoTo("/Music".into())));
        let _ = handler.update(listed("/Music", &[file("a.jpg")]));
        assert_eq!(
            handler.selection.single(),
            None,
            "not picked in another folder"
        );

        let mut closed = super::tests::handler();
        let _ = closed.show_item("/DCIM".into(), "a.jpg".into());
        assert!(!closed.loading);
    }

    #[test]
    fn refresh_and_select_on_a_closed_pane_does_nothing() {
        let mut handler = handler();

        let _ = handler.refresh_and_select(Some("x".into()));

        assert!(!handler.loading);
    }

    #[test]
    fn only_the_phone_pane_offers_search_and_mirror() {
        let mut phone = handler();
        let _ = phone.open(storage(), "/".into());
        let mut mac = handler();
        let _ = mac.open(super::tests::mac(), "/".into());

        let offers =
            |pane: &Handler, label: &str| iced_test::simulator(pane.view()).find(label).is_ok();

        assert!(offers(&phone, "Search") && offers(&phone, "Mirror…"));
        assert!(!offers(&mac, "Search") && !offers(&mac, "Mirror…"));
        let task = phone.update(Message::Request(Request::Mirror));
        assert_eq!(task.units(), 0, "app.rs opens the dialog");
    }

    #[test]
    fn the_transfer_button_can_come_and_go() {
        let mut handler = handler();
        handler.set_transfer_action(Some("label.download"));
        let _ = handler.open(mac(), "/Users/me".into());

        handler.set_transfer_action(Some("label.upload"));
        assert_eq!(
            handler.location.as_ref().unwrap().volume.transfer_action,
            Some("label.upload")
        );
        let _ = handler.view();

        handler.set_transfer_action(None);
        assert_eq!(
            handler.location.as_ref().unwrap().volume.transfer_action,
            None
        );
        assert_eq!(handler.volume_id(), Some(0));
    }

    /// A pane on "/" listing a.txt, b.txt, c.txt and the folder DCIM.
    #[test]
    fn the_kind_header_sorts_by_kind_like_the_others() {
        let mut handler = browsing("/");
        let _ = handler.set_view(dto::ViewMode::List);
        let _ = handler.update(listed(
            "/",
            &[file("b.txt"), file("a.mp4"), file("c.jpg"), folder("DCIM")],
        ));
        let messages: Vec<Message> = {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(900.0, 500.0),
                handler.view(),
            );
            let _ = ui.click("Kind").unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(handler.sort_order().key, dto::SortKey::Kind);
        assert!(!handler.sort_order().descending);
        let names: Vec<&str> = handler.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["DCIM", "c.jpg", "a.mp4", "b.txt"]);
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(900.0, 500.0),
            handler.view(),
        );
        assert!(ui.find("Kind ▲").is_ok());
    }

    fn four_rows() -> Handler {
        let mut handler = browsing("/");
        let _ = handler.update(listed(
            "/",
            &[file("a.txt"), file("b.txt"), file("c.txt"), folder("DCIM")],
        ));
        handler
    }

    fn selected(handler: &Handler) -> Vec<&str> {
        handler
            .selection
            .names()
            .iter()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn command_click_toggles_and_shift_click_extends() {
        let mut handler = four_rows();
        request(&mut handler, Request::Select("a.txt".into()));

        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("c.txt".into()));
        assert_eq!(selected(&handler), ["a.txt", "c.txt"]);

        handler.set_modifiers(Modifiers::SHIFT);
        request(&mut handler, Request::Select("DCIM".into()));
        assert_eq!(selected(&handler), ["DCIM", "a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn select_all_and_arrows() {
        let mut handler = four_rows();

        request(&mut handler, Request::SelectAll);
        assert_eq!(handler.selection.len(), 4);

        request(&mut handler, Request::Escape);
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: true,
            },
        );
        assert_eq!(selected(&handler), ["DCIM", "a.txt"]);
    }

    #[test]
    fn return_opens_the_one_selected_folder_only() {
        let mut handler = four_rows();
        request(&mut handler, Request::Select("a.txt".into()));
        request(&mut handler, Request::OpenSelected);
        assert_eq!(path(&handler), "/");

        request(&mut handler, Request::SelectAll);
        request(&mut handler, Request::OpenSelected);
        assert_eq!(path(&handler), "/");

        // A click on one of several selected rows narrows to it when the
        // button comes up (not before: it might be the start of a drag).
        request(&mut handler, Request::Select("DCIM".into()));
        request(&mut handler, Request::PointerReleased);
        request(&mut handler, Request::OpenSelected);
        assert_eq!(path(&handler), "/DCIM");
    }

    #[test]
    fn escape_closes_a_dialog_before_it_clears_the_selection() {
        let mut handler = four_rows();
        request(&mut handler, Request::Select("a.txt".into()));
        request(&mut handler, Request::NewFolder);

        request(&mut handler, Request::Escape);
        assert!(handler.editor.is_none());
        assert_eq!(selected(&handler), ["a.txt"]);

        request(&mut handler, Request::Escape);
        assert!(handler.selection.is_empty());
    }

    #[test]
    fn escape_waits_while_a_change_is_being_made() {
        let mut handler = four_rows();
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::Submit);

        request(&mut handler, Request::Escape);

        assert!(handler.is_editing());
    }

    // Folder sizes.

    fn sizing(sizes: &[(&str, u64)]) -> (Handler, Arc<FakeExplorer>) {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.folder_sizes = sizes.iter().map(|(p, b)| ((*p).to_owned(), *b)).collect();
        let service = Arc::new(service);
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(storage(), "/".into());
        let _ = handler.set_view(dto::ViewMode::List);
        (handler, service)
    }

    fn size_of<'a>(handler: &'a Handler, name: &str) -> &'a str {
        &handler
            .rows
            .iter()
            .find(|row| row.name == name)
            .unwrap()
            .size
    }

    fn listing_with_folders() -> Message {
        listed(
            "/",
            &[
                folder("Small"),
                folder("Big"),
                folder("Empty"),
                file("a.jpg"),
            ],
        )
    }

    #[tokio::test]
    async fn folder_sizes_are_counted_one_folder_at_a_time_and_shown_as_they_arrive() {
        let (mut handler, service) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000)]);
        let set = handler.set_folder_sizes(true);
        assert!(
            crate::handler::testing::drain(set).await.0.is_empty(),
            "nothing listed yet"
        );

        let first = handler.update(listing_with_folders());
        assert_eq!(
            size_of(&handler, "Big"),
            "Counting…",
            "the first in the order shown"
        );
        assert_eq!(size_of(&handler, "Small"), "—", "one at a time");
        settle_all(&mut handler, first).await;

        assert_eq!(size_of(&handler, "Big"), "9.0 MB");
        assert_eq!(size_of(&handler, "Small"), "2.0 KB");
        assert_eq!(size_of(&handler, "Empty"), "—", "it couldn't be counted");
        assert_eq!(size_of(&handler, "a.jpg"), "1.0 KB", "files as ever");
        assert_eq!(*service.sized.lock().unwrap(), ["/Big", "/Empty", "/Small"]);
        assert!(
            shows(&handler, "4 items · 3 folders, 1 file · 1.0 KB"),
            "the total is the files'"
        );
    }

    #[tokio::test]
    async fn ordered_by_size_the_biggest_folders_come_first() {
        let (mut handler, _) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000), ("/Empty", 0)]);
        let _ = handler.set_folder_sizes(true);
        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;

        request(&mut handler, Request::SortBy(dto::SortKey::Size));

        let names: Vec<&str> = handler.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            ["Big", "Small", "Empty", "a.jpg"],
            "largest first, folders first"
        );
    }

    #[tokio::test]
    async fn leaving_the_folder_or_the_list_stops_the_count_and_a_late_answer_is_dropped() {
        let (mut handler, service) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000)]);
        let _ = handler.set_folder_sizes(true);
        let first = handler.update(listing_with_folders());

        let _ = handler.set_view(dto::ViewMode::Grid);
        for message in crate::handler::testing::drain(first).await.0 {
            let _ = handler.update(message);
        }
        assert_eq!(
            size_of(&handler, "Big"),
            "—",
            "the answer came for a stopped count"
        );

        let resumed = handler.set_view(dto::ViewMode::List);
        settle_all(&mut handler, resumed).await;
        assert_eq!(size_of(&handler, "Big"), "9.0 MB", "the list counts again");
        let asked = service.sized.lock().unwrap().len();

        request(&mut handler, Request::GoTo("/Music".into()));
        let _ = handler.update(listed("/Music", &[folder("Big")]));
        assert_eq!(
            size_of(&handler, "Big"),
            "Counting…",
            "another folder starts afresh"
        );
        assert!(service.sized.lock().unwrap().len() >= asked);
    }

    #[tokio::test]
    async fn turning_sizes_off_forgets_them_and_a_refresh_counts_again() {
        let (mut handler, service) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000)]);
        let _ = handler.set_folder_sizes(true);
        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;
        let counted = service.sized.lock().unwrap().len();

        // An edit reads the folder from memory: what was counted stays.
        let kept = handler.load(true);
        assert!(crate::handler::testing::drain(kept).await.0.len() <= 1);
        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;
        assert_eq!(
            service.sized.lock().unwrap().len(),
            counted + 1,
            "only the uncountable again"
        );

        let refreshed = handler.update(Message::Request(Request::Refresh));
        let _ = refreshed;
        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;
        assert!(
            service.sized.lock().unwrap().len() >= counted * 2,
            "read afresh: counted afresh"
        );

        let _ = handler.set_folder_sizes(false);
        assert_eq!(size_of(&handler, "Big"), "—");
    }

    #[tokio::test]
    async fn tiles_have_no_size_column_so_nothing_is_counted() {
        let (mut handler, service) = sizing(&[("/Big", 9_000_000)]);
        let _ = handler.set_view(dto::ViewMode::Grid);
        let _ = handler.set_folder_sizes(true);

        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;

        assert!(service.sized.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_second_reading_during_a_count_starts_no_second_count() {
        let (mut handler, _) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000)]);
        let _ = handler.set_folder_sizes(true);
        let _first = handler.update(listing_with_folders());

        let second = handler.update(listing_with_folders());
        let messages = crate::handler::testing::drain(second).await.0;

        assert!(
            !messages
                .iter()
                .any(|message| matches!(message, Message::FolderSized { .. })),
            "one count at a time"
        );
    }

    #[tokio::test]
    async fn ordered_by_size_the_other_way_the_smallest_folders_come_first() {
        let (mut handler, _) = sizing(&[("/Small", 2_000), ("/Big", 9_000_000), ("/Empty", 0)]);
        let _ = handler.set_folder_sizes(true);
        let task = handler.update(listing_with_folders());
        settle_all(&mut handler, task).await;

        request(&mut handler, Request::SortBy(dto::SortKey::Size));
        request(&mut handler, Request::SortBy(dto::SortKey::Size));

        let names: Vec<&str> = handler.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Empty", "Small", "Big", "a.jpg"]);
    }

    #[test]
    fn a_folder_part_of_which_could_not_be_counted_says_so() {
        let some = dto::Tally {
            bytes: 5_000_000,
            unknown_sizes: 1,
            ..dto::Tally::default()
        };
        assert_eq!(mapper::folder_size(&some), "5.0 MB+");
        let all = dto::Tally {
            bytes: 5_000_000,
            ..dto::Tally::default()
        };
        assert_eq!(mapper::folder_size(&all), "5.0 MB");
        for odd in [
            dto::Tally {
                unreadable: 1,
                ..all
            },
            dto::Tally {
                incomplete: true,
                ..all
            },
        ] {
            assert!(mapper::folder_size(&odd).ends_with('+'));
        }
    }

    #[tokio::test]
    async fn a_lost_phone_ends_the_counting_and_says_so() {
        let (mut handler, _) = sizing(&[]);
        let _ = handler.set_folder_sizes(true);
        let _ = handler.update(listing_with_folders());
        let run = handler.size_run;
        let lost = Message::FolderSized {
            run,
            name: "Big".into(),
            result: Err(error::to_error_response(dto::Error::Disconnected)),
        };
        assert!(lost.device_lost().is_some());

        let next = handler.update(lost);

        assert!(
            crate::handler::testing::drain(next).await.0.is_empty(),
            "nothing more asked"
        );
        assert_eq!(size_of(&handler, "Small"), "—");
    }

    #[tokio::test]
    async fn sizes_for_fifty_folders_beside_five_thousand_files_stay_quick() {
        let sizes: Vec<(String, u64)> = (0..50).map(|n| (format!("/F{n:02}"), n * 1_000)).collect();
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.folder_sizes = sizes;
        let mut handler = Handler::new(Arc::new(service));
        let _ = handler.open(storage(), "/".into());
        let _ = handler.set_view(dto::ViewMode::List);
        let _ = handler.set_folder_sizes(true);
        let mut entries: Vec<dto::Entry> = (0..50).map(|n| folder(&format!("F{n:02}"))).collect();
        entries.extend((0..5_000).map(|n| file(&format!("IMG_{n:04}.jpg"))));
        let started = Instant::now();

        let task = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        settle_all(&mut handler, task).await;

        assert_eq!(size_of(&handler, "F49"), "49.0 KB");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    // Copy Path.

    #[tokio::test]
    async fn copy_path_puts_each_selected_items_path_on_the_clipboard_in_the_order_shown() {
        let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.open(storage(), "/DCIM/Camera".into());
        let _ = handler.update(listed(
            "/DCIM/Camera",
            &[file("b.jpg"), file("a.jpg"), file("c.jpg")],
        ));
        request(&mut handler, Request::Select("c.jpg".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("a.jpg".into()));
        handler.set_modifiers(Modifiers::empty());

        let task = handler.update(Message::Request(Request::CopyPath));
        let (_, copied) = crate::handler::testing::drain(task).await;

        assert_eq!(copied, ["/DCIM/Camera/a.jpg\n/DCIM/Camera/c.jpg"]);
        assert_eq!(handler.notice.as_ref().unwrap().title, "Copied 2 paths");

        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[folder("DCIM")]));
        request(&mut handler, Request::Select("DCIM".into()));
        let task = handler.update(Message::Request(Request::CopyPath));
        let (_, copied) = crate::handler::testing::drain(task).await;
        assert_eq!(copied, ["/DCIM"], "at the top, one slash");
        assert_eq!(
            handler.notice.as_ref().unwrap().title,
            "Copied the path of “DCIM”"
        );

        request(&mut handler, Request::Escape);
        let task = handler.update(Message::Request(Request::CopyPath));
        assert!(
            crate::handler::testing::drain(task).await.1.is_empty(),
            "nothing selected"
        );
    }

    #[tokio::test]
    async fn a_name_with_a_line_break_or_other_control_character_copies_nothing() {
        for odd in [
            "a.jpg\nrm -rf ~",
            "tab\there.txt",
            "esc\u{1b}[2J.txt",
            "c1\u{85}.txt",
            "line\u{2028}sep.txt",
            "gpj.\u{202E}exe.txt",
            "isolate\u{2066}x.txt",
        ] {
            let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
            let _ = handler.open(storage(), "/".into());
            let _ = handler.update(listed("/", &[file("fine.jpg"), file(odd)]));
            request(&mut handler, Request::SelectAll);

            let task = handler.update(Message::Request(Request::CopyPath));
            let (_, copied) = crate::handler::testing::drain(task).await;

            assert!(copied.is_empty(), "{odd:?}");
            let notice = handler.notice.as_ref().unwrap();
            assert!(notice.problem);
            assert!(
                !mapper::has_control(&notice.details[0]),
                "shown escaped: {}",
                notice.details[0]
            );
        }
    }

    #[tokio::test]
    async fn a_folder_above_with_a_line_break_copies_nothing_either() {
        let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.open(storage(), "/odd\nfolder".into());
        let _ = handler.update(listed("/odd\nfolder", &[file("fine.jpg")]));
        request(&mut handler, Request::SelectAll);

        let task = handler.update(Message::Request(Request::CopyPath));
        let (_, copied) = crate::handler::testing::drain(task).await;

        assert!(copied.is_empty());
        assert!(handler.notice.as_ref().unwrap().details[0].contains("/odd\\nfolder/fine.jpg"));
    }

    #[test]
    fn ordinary_names_in_any_script_are_fine() {
        for name in [
            "IMG_0001.jpg",
            "café.jpg",
            "ภาพถ่าย.jpg",
            "写真.png",
            "a b (1).jpg",
            "🙂.heic",
        ] {
            assert!(!mapper::has_control(name), "{name}");
        }
    }

    #[tokio::test]
    async fn copying_five_thousand_paths_is_one_pass() {
        let mut handler = browsing("/");
        let entries: Vec<dto::Entry> = (0..5_000)
            .map(|n| file(&format!("IMG_{n:04}.jpg")))
            .collect();
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        request(&mut handler, Request::SelectAll);
        let started = Instant::now();

        let task = handler.update(Message::Request(Request::CopyPath));
        let (_, copied) = crate::handler::testing::drain(task).await;

        assert_eq!(copied[0].lines().count(), 5_000);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    // New Folder with Selection.

    /// Settles `task` and every task its messages start in turn.
    async fn settle_all(handler: &mut Handler, task: Task<Message>) {
        let mut tasks = vec![task];
        while let Some(task) = tasks.pop() {
            for message in crate::handler::testing::drain(task).await.0 {
                tasks.push(handler.update(message));
            }
        }
    }

    fn grouping_two(service: Arc<FakeExplorer>, also: &[dto::Entry]) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/".into());
        let mut entries = vec![file("a.jpg"), file("b.jpg"), file("c.jpg")];
        entries.extend_from_slice(also);
        let _ = handler.update(listed("/", &entries));
        request(&mut handler, Request::Select("a.jpg".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("b.jpg".into()));
        handler.set_modifiers(Modifiers::empty());
        handler
    }

    #[tokio::test]
    async fn the_selection_goes_into_a_new_folder_whose_name_is_ready_to_change() {
        // The folder read again afterwards: the new folder, and what stayed.
        let service = Arc::new(FakeExplorer::new(Ok(vec![
            folder("New Folder With Items"),
            file("c.jpg"),
        ])));
        let mut handler = grouping_two(service.clone(), &[]);

        let task = handler.update(Message::Request(Request::NewFolderWithItems));
        assert!(handler.is_pasting(), "edits wait");
        settle_all(&mut handler, task).await;

        assert_eq!(
            service.edits.lock().unwrap().first().map(String::as_str),
            Some("mkdir 1 / New Folder With Items")
        );
        let pastes = service.pastes.lock().unwrap().clone();
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0].mode, dto::Paste::Move);
        assert_eq!(pastes[0].names, ["a.jpg", "b.jpg"]);
        assert_eq!(pastes[0].to, "/New Folder With Items");
        assert_eq!(selected(&handler), ["New Folder With Items"]);
        assert!(matches!(
            change(&handler),
            Some(Change::Rename { original }) if original == "New Folder With Items"
        ));
        assert!(
            handler
                .notice
                .as_ref()
                .unwrap()
                .title
                .ends_with("into “New Folder With Items”")
        );
        assert!(
            handler.can_undo() || handler.is_editing(),
            "undo waits for the rename dialog"
        );
        assert!(matches!(
            handler.last_undo,
            Some((1, Undo::Grouped { ref name, .. })) if name == "New Folder With Items"
        ));
    }

    #[tokio::test]
    async fn a_taken_name_is_numbered_and_nothing_selected_makes_nothing() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = grouping_two(service.clone(), &[folder("NEW folder WITH items")]);
        let task = handler.update(Message::Request(Request::NewFolderWithItems));
        settle_all(&mut handler, task).await;
        assert_eq!(
            service.edits.lock().unwrap().first().map(String::as_str),
            Some("mkdir 1 / New Folder With Items (1)"),
            "ignoring case"
        );

        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut none = Handler::new(service.clone());
        let _ = none.open(storage(), "/".into());
        let _ = none.update(listed("/", &[file("a.jpg")]));
        let task = none.update(Message::Request(Request::NewFolderWithItems));
        settle_all(&mut none, task).await;
        assert!(service.edits.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_made_says_why_and_moves_nothing() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.edit = Err(dto::Error::PermissionDenied);
        let service = Arc::new(service);
        let mut handler = grouping_two(service.clone(), &[]);

        let task = handler.update(Message::Request(Request::NewFolderWithItems));
        settle_all(&mut handler, task).await;

        assert!(service.pastes.lock().unwrap().is_empty());
        assert!(handler.notice.as_ref().unwrap().problem);
        assert!(!handler.is_pasting() && !handler.is_editing());
    }

    #[tokio::test]
    async fn a_move_that_moved_nothing_leaves_no_empty_folder() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::Disconnected,
        )]));
        let mut handler = grouping_two(service.clone(), &[]);

        let task = handler.update(Message::Request(Request::NewFolderWithItems));
        settle_all(&mut handler, task).await;

        let edits = service.edits.lock().unwrap().clone();
        assert_eq!(
            edits.last().map(String::as_str),
            Some("delete 1 / New Folder With Items")
        );
        assert!(handler.notice.as_ref().unwrap().problem);
        assert!(!handler.can_undo());
    }

    #[tokio::test]
    async fn a_phone_that_cant_move_goes_through_the_mac_into_the_new_folder() {
        let mut service = FakeExplorer::answering_pastes(vec![Err(dto::Error::Unsupported)]);
        service.result = Ok(vec![
            folder("New Folder With Items"),
            file("a.jpg"),
            file("b.jpg"),
        ]);
        let mut handler = grouping_two(Arc::new(service), &[]);

        let made = handler.update(Message::Request(Request::NewFolderWithItems));
        let mut via = None;
        for message in crate::handler::testing::drain(made).await.0 {
            let task = handler.update(message);
            for message in crate::handler::testing::drain(task).await.0 {
                if let Some((mode, _, _, path)) = message.via_mac() {
                    via = Some((mode, path.to_owned()));
                }
                let _ = handler.update(message);
            }
        }

        assert_eq!(
            via,
            Some((dto::Paste::Move, "/New Folder With Items".to_owned()))
        );
        assert_eq!(selected(&handler), ["New Folder With Items"]);
        assert!(matches!(change(&handler), Some(Change::Rename { .. })));
    }

    #[tokio::test]
    async fn one_undo_puts_the_items_back_and_removes_the_folder() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let result = undo::undo(
            service.clone(),
            1,
            Undo::Grouped {
                folder: "/".into(),
                name: "Trip".into(),
                moved: vec![undo::MovedStep {
                    from: "/".into(),
                    into: "/Trip".into(),
                    items: vec![("a.jpg".into(), "a.jpg".into())],
                }],
            },
        )
        .await;

        assert_eq!(result, Ok(("/".into(), vec!["a.jpg".into()])));
        let pastes = service.pastes.lock().unwrap().clone();
        assert_eq!(
            (pastes[0].from.as_str(), pastes[0].to.as_str()),
            ("/Trip", "/")
        );
        assert_eq!(
            service.edits.lock().unwrap().last().map(String::as_str),
            Some("delete 1 / Trip"),
            "then the folder, empty again"
        );
        assert_eq!(
            Undo::Grouped {
                folder: "/".into(),
                name: "x".into(),
                moved: Vec::new()
            }
            .banner(),
            "explorer.undo.new_folder"
        );
    }

    // Filtering by kind.

    fn mixed() -> Handler {
        let mut handler = browsing("/");
        let _ = handler.set_view(dto::ViewMode::List);
        let _ = handler.update(listed(
            "/",
            &[
                folder("Trip videos"),
                file("trip.mp4"),
                file("beach.MOV"),
                file("trip.jpg"),
                file("notes.txt"),
                file("song.mp3"),
            ],
        ));
        handler
    }

    fn row_names_of(handler: &Handler) -> Vec<&str> {
        handler.rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn picking_a_kind_leaves_only_those_files_and_select_all_takes_just_them() {
        let mut handler = mixed();

        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Video)),
        );

        assert_eq!(
            row_names_of(&handler),
            ["beach.MOV", "trip.mp4"],
            "no folder, any case"
        );
        request(&mut handler, Request::SelectAll);
        assert_eq!(selected(&handler), ["beach.MOV", "trip.mp4"]);
        assert!(shows(&handler, "2 selected · 2 of 6 shown"));

        request(&mut handler, Request::Filter("trip".into()));
        assert_eq!(row_names_of(&handler), ["trip.mp4"], "with the words");
        assert_eq!(
            selected(&handler),
            ["trip.mp4"],
            "what is hidden leaves the selection"
        );
    }

    #[test]
    fn escape_or_another_folder_clears_the_kind_with_the_words_and_a_new_reading_keeps_it() {
        let mut handler = mixed();
        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Photo)),
        );
        let _ = handler.update(listed(
            "/",
            &[file("trip.jpg"), file("new.png"), file("a.txt")],
        ));
        assert_eq!(
            row_names_of(&handler),
            ["new.png", "trip.jpg"],
            "the same folder read again"
        );

        request(&mut handler, Request::Escape);
        assert_eq!(row_names_of(&handler).len(), 3);
        assert_eq!(handler.filter_kind, None);

        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Music)),
        );
        request(&mut handler, Request::GoTo("/Music".into()));
        let _ = handler.update(listed("/Music", &[file("a.mp3"), file("cover.jpg")]));
        assert_eq!(
            row_names_of(&handler).len(),
            2,
            "another folder: everything"
        );
    }

    fn mid_june() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
    }

    fn changed_on(name: &str, year: u16, month: u8, day: u8) -> dto::Entry {
        dto::Entry {
            modified: Some(dto::Timestamp {
                year,
                month,
                day,
                hour: 12,
                minute: 0,
            }),
            ..file(name)
        }
    }

    /// A folder of photos and a video from today back to two years ago, an
    /// undated file and a folder, on 15 June 2026.
    fn dated() -> Handler {
        let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.today = mid_june;
        let _ = handler.open(storage(), "/".into());
        let mut entries = vec![
            changed_on("today.jpg", 2026, 6, 15),
            changed_on("yesterday.jpg", 2026, 6, 14),
            changed_on("tuesday.mp4", 2026, 6, 9),
            changed_on("may.jpg", 2026, 5, 20),
            changed_on("january.jpg", 2026, 1, 2),
            changed_on("old.jpg", 2024, 6, 1),
            file("undated.jpg"),
        ];
        entries.push(folder("Camera"));
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        handler
    }

    #[test]
    fn grouped_by_date_the_rows_go_under_finders_headers_newest_first() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        assert_eq!(handler.group_by(), dto::GroupBy::Date);

        let labels: Vec<&str> = handler.groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Today",
                "Yesterday",
                "Previous 7 Days",
                "Previous 30 Days",
                "January 2026",
                "2024",
                "No Date"
            ]
        );
        assert_eq!(
            shown(&handler),
            [
                "today.jpg",
                "yesterday.jpg",
                "tuesday.mp4",
                "may.jpg",
                "january.jpg",
                "old.jpg",
                "Camera",
                "undated.jpg"
            ],
            "the folder without a date with the undated, folders first as sorted"
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Previous 7 Days").is_ok());
        assert!(ui.find("1 item, 1.0 KB").is_ok());
        drop(ui);

        handler.set_group_by(dto::GroupBy::None);
        assert!(handler.groups.is_empty());
        assert_eq!(shown(&handler)[0], "Camera", "back to the pane's own order");
    }

    #[test]
    fn a_header_click_selects_its_group_shift_adds_and_command_toggles() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        let today = handler
            .groups
            .iter()
            .position(|g| g.label == "Today")
            .unwrap();
        let none = handler.groups.len() - 1;

        request(&mut handler, Request::SelectGroup(none));
        assert_eq!(selected(&handler), ["Camera", "undated.jpg"]);
        handler.set_modifiers(Modifiers::SHIFT);
        request(&mut handler, Request::SelectGroup(today));
        assert_eq!(selected(&handler), ["today.jpg", "Camera", "undated.jpg"]);
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::SelectGroup(none));
        assert_eq!(selected(&handler), ["today.jpg"], "all selected: all go");
        request(&mut handler, Request::SelectGroup(none));
        assert_eq!(selected(&handler), ["today.jpg", "Camera", "undated.jpg"]);
        handler.set_modifiers(Modifiers::empty());
        request(&mut handler, Request::SelectGroup(99));
        assert_eq!(selected(&handler).len(), 3, "no such group: nothing");

        // A click on the header itself, in the view, sends the request.
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.click("Yesterday").is_ok());
        let sent: Vec<Message> = ui.into_messages().collect();
        assert!(
            sent.iter()
                .any(|m| matches!(m, Message::Request(Request::SelectGroup(1)))),
            "{sent:?}"
        );
    }

    #[test]
    fn arrows_cross_headers_and_the_cursor_is_scrolled_to_by_its_line() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        request(&mut handler, Request::Select("today.jpg".into()));
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        assert_eq!(
            selected(&handler),
            ["yesterday.jpg"],
            "over the header between"
        );
        // Rows, with a header before each group: yesterday.jpg (row 1) is on
        // line 3.
        assert_eq!(groups::list_line_of(&handler.groups, 1), 3);
        handler.viewport = (0.0, handler.row_height() * 2.5, 600.0);
        assert!(
            handler.scroll_to_cursor().units() > 0,
            "line 3 is below a 2.5-line view"
        );
    }

    #[test]
    fn a_header_parts_a_block_of_selected_rows() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        request(&mut handler, Request::Select("today.jpg".into()));
        handler.set_modifiers(Modifiers::SHIFT);
        request(&mut handler, Request::Select("tuesday.mp4".into()));
        handler.set_modifiers(Modifiers::empty());
        assert_eq!(
            selected(&handler),
            ["today.jpg", "yesterday.jpg", "tuesday.mp4"]
        );

        // Rows 0, 1 and 2 are in three groups: each has a header above it.
        let joined = |index: usize| {
            let look = handler.look(&handler.rows[index]);
            handler.joined(index, look)
        };
        assert!(!joined(0).below && !joined(1).above && !joined(1).below && !joined(2).above);
    }

    #[test]
    fn a_lasso_over_a_header_takes_the_rows_round_it() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        let height = handler.row_height();
        handler.lasso = Some(Sweeping {
            anchor: Point::new(10.0, 0.5 * height),
            pointer: Point::new(10.0, 3.5 * height),
            size: Size::new(600.0, 400.0),
            base: std::collections::HashSet::new(),
            sweep: Sweep::Replace,
        });
        handler.sweep_lasso();
        // Lines 0..=3: Today's header, today.jpg, Yesterday's header,
        // yesterday.jpg.
        assert_eq!(selected(&handler), ["today.jpg", "yesterday.jpg"]);
    }

    #[test]
    fn a_grouped_grid_starts_each_group_on_a_new_line_under_its_header() {
        let mut handler = dated();
        handler.set_group_by(dto::GroupBy::Date);
        let _ = handler.set_view(dto::ViewMode::Grid);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Today").is_ok());
        assert!(
            ui.find("2024").is_err(),
            "lines below the screen aren't built"
        );
        drop(ui);
        handler.viewport = (0.0, 2_000.0, 600.0);
        assert_eq!(
            handler.visible_tiles(),
            0..8,
            "every tile, headers left out"
        );
        let frame = handler.grid_frame(600.0);
        // Seven short headers and a tile line a group.
        assert_eq!(frame.lines, 14);
        assert_eq!(
            handler.content_height(600.0),
            7.0 * GRID_HEADER + 7.0 * handler.tile.row_height()
        );
        // The cursor is scrolled to by its line's place, headers short.
        request(&mut handler, Request::Select("undated.jpg".into()));
        handler.viewport = (0.0, 200.0, 600.0);
        assert!(handler.scroll_to_cursor().units() > 0);
        handler.viewport = (frame.top(13), 400.0, 600.0);
        assert_eq!(handler.scroll_to_cursor().units(), 0, "already in view");
    }

    #[test]
    fn grouped_five_thousand_files_still_build_only_what_is_on_screen() {
        let mut handler = folder_of(5_000, dto::ViewMode::List, 0.0);
        handler.set_group_by(dto::GroupBy::Size);
        // Every file is 1 KB: one group, "Under 100 KB", and its header.
        assert_eq!(handler.groups.len(), 1);
        let built = rows_built(&handler, 5_000);
        assert!(!built.is_empty() && built.len() < 100, "{}", built.len());
        handler.viewport.0 = 4_000.0 * handler.row_height();
        let built = rows_built(&handler, 5_000);
        assert!(
            built.iter().all(|&i| (3_900..4_100).contains(&i)),
            "{built:?}"
        );
        let started = std::time::Instant::now();
        handler.set_group_by(dto::GroupBy::Kind);
        handler.set_group_by(dto::GroupBy::Date);
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn a_date_keeps_the_files_changed_then_and_never_a_folder() {
        let mut handler = dated();
        let keeps = |handler: &mut Handler, range| {
            request(handler, Request::FilterDate(Some(range)));
            let mut names: Vec<String> = row_names_of(handler)
                .into_iter()
                .map(str::to_owned)
                .collect();
            names.sort();
            names
        };

        assert_eq!(keeps(&mut handler, dto::DateRange::Today), ["today.jpg"]);
        assert_eq!(
            keeps(&mut handler, dto::DateRange::Yesterday),
            ["yesterday.jpg"]
        );
        assert_eq!(
            keeps(&mut handler, dto::DateRange::LastWeek),
            ["today.jpg", "tuesday.mp4", "yesterday.jpg"]
        );
        assert_eq!(
            keeps(&mut handler, dto::DateRange::LastMonth),
            ["may.jpg", "today.jpg", "tuesday.mp4", "yesterday.jpg"]
        );
        assert_eq!(keeps(&mut handler, dto::DateRange::ThisYear).len(), 5);
        assert_eq!(
            keeps(&mut handler, dto::DateRange::OlderThanAYear),
            ["old.jpg"]
        );

        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Photo)),
        );
        request(
            &mut handler,
            Request::FilterDate(Some(dto::DateRange::LastWeek)),
        );
        let mut both: Vec<String> = row_names_of(&handler)
            .into_iter()
            .map(str::to_owned)
            .collect();
        both.sort();
        assert_eq!(both, ["today.jpg", "yesterday.jpg"], "with the kind");
        request(&mut handler, Request::SelectAll);
        assert_eq!(selected(&handler).len(), 2, "⌘A selects only those");
        let _ = handler.view();
    }

    #[test]
    fn escape_or_another_folder_clears_the_date_with_the_rest() {
        let mut handler = dated();
        request(
            &mut handler,
            Request::FilterDate(Some(dto::DateRange::Today)),
        );
        assert!(handler.is_filtering());

        request(&mut handler, Request::Escape);
        assert_eq!(handler.filter_date, None);
        assert_eq!(row_names_of(&handler).len(), 8);

        request(
            &mut handler,
            Request::FilterDate(Some(dto::DateRange::Today)),
        );
        request(&mut handler, Request::GoTo("/Camera".into()));
        assert_eq!(handler.filter_date, None, "another folder: any date");
        request(
            &mut handler,
            Request::FilterDate(Some(dto::DateRange::Today)),
        );
        handler.close();
        assert_eq!(handler.filter_date, None);
    }

    #[test]
    fn a_narrow_pane_gives_the_filter_a_row_of_its_own_under_the_path() {
        assert!(layout::filters_inline(layout::FILTERS_INLINE));
        assert!(!layout::filters_inline(layout::FILTERS_INLINE - 1));

        let handler = dated();
        let views_y = |width: f32| {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(width, 420.0),
                handler.view(),
            );
            let views = ui.find("☰").unwrap().bounds();
            let new_folder = ui.find("New folder").unwrap().bounds();
            (views.center_y(), new_folder.y)
        };
        let (wide_views, wide_actions) = views_y(900.0);
        let (narrow_views, narrow_actions) = views_y(480.0);
        assert!(
            (wide_views - narrow_views).abs() < 1.0,
            "the path row stays put"
        );
        assert!(
            narrow_actions > wide_actions + 20.0,
            "the filter's own row comes between the path and the actions"
        );
    }

    #[test]
    fn the_date_is_picked_from_a_menu_beside_the_kind() {
        let mut handler = dated();
        let size = iced::Size::new(900.0, 500.0);
        let mut ui =
            iced_test::Simulator::with_size(iced::Settings::default(), size, handler.view());
        // Just left of the kind menu; its rows about 23 points apart, the
        // first just under it, "Last 7 days" fourth.
        let views = ui.find("☰").unwrap().bounds();
        let at = iced::Point::new(
            views.x - 8.0 - 8.0 - KIND_WIDTH - 8.0 - DATE_WIDTH / 2.0,
            views.center_y(),
        );
        ui.point_at(at);
        let _ = ui.simulate(iced_test::simulator::click());
        let last_week = iced::Point::new(at.x, at.y + 23.0 * 4.0);
        ui.point_at(last_week);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: last_week,
        })]);
        let _ = ui.simulate(iced_test::simulator::click());
        for message in ui.into_messages().collect::<Vec<_>>() {
            let _ = handler.update(message);
        }

        assert_eq!(handler.filter_date, Some(dto::DateRange::LastWeek));
        assert_eq!(row_names_of(&handler).len(), 3);
    }

    #[tokio::test]
    async fn a_date_over_five_thousand_files_is_one_pass_with_no_phone_request() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        handler.today = mid_june;
        let _ = handler.open(storage(), "/".into());
        let entries: Vec<dto::Entry> = (0..5_000)
            .map(|n| {
                changed_on(
                    &format!("IMG_{n:04}.jpg"),
                    2026,
                    6,
                    if n % 2 == 0 { 15 } else { 1 },
                )
            })
            .collect();
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        let listed = service.calls.lock().unwrap().len();
        let opened = service.opened.lock().unwrap().len();

        let task = handler.update(Message::Request(Request::FilterDate(Some(
            dto::DateRange::Today,
        ))));
        let _ = crate::handler::testing::drain(task).await;

        assert_eq!(handler.rows.len(), 2_500);
        assert_eq!(
            service.calls.lock().unwrap().len(),
            listed,
            "nothing asked of the phone"
        );
        assert_eq!(service.opened.lock().unwrap().len(), opened);
    }

    /// Where the kind menu is: just left of the view buttons.
    fn kind_menu_at(ui: &mut iced_test::Simulator<'_, Message>) -> iced::Point {
        let list = ui.find("☰").unwrap().bounds();
        iced::Point::new(list.x - 8.0 - 8.0 - KIND_WIDTH / 2.0, list.center_y())
    }

    #[test]
    fn the_kind_is_picked_from_a_menu_beside_the_filter() {
        let mut handler = mixed();
        let size = iced::Size::new(900.0, 500.0);
        let mut ui =
            iced_test::Simulator::with_size(iced::Settings::default(), size, handler.view());
        // The menu draws its own words (`find` can't see them): its rows are
        // about 23 points apart, the first just under it, "Documents" fifth.
        let at = kind_menu_at(&mut ui);
        ui.point_at(at);
        let _ = ui.simulate(iced_test::simulator::click());
        let documents = iced::Point::new(at.x, at.y + 23.0 * 5.0);
        ui.point_at(documents);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: documents,
        })]);
        let _ = ui.simulate(iced_test::simulator::click());
        for message in ui.into_messages().collect::<Vec<_>>() {
            let _ = handler.update(message);
        }

        assert_eq!(row_names_of(&handler), ["notes.txt"]);
        assert_eq!(handler.filter_kind, Some(dto::FileKind::Document));
    }

    #[test]
    fn a_closed_pane_forgets_its_kind() {
        let mut handler = mixed();
        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Video)),
        );

        handler.close();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("a.jpg"), file("b.mp4")]));

        assert_eq!(
            row_names_of(&handler).len(),
            2,
            "the next phone shows everything"
        );
    }

    #[test]
    fn a_kind_over_five_thousand_files_is_one_pass() {
        let mut handler = browsing("/");
        let entries: Vec<dto::Entry> = (0..5_000)
            .map(|n| {
                file(&format!(
                    "IMG_{n:04}.{}",
                    if n % 2 == 0 { "jpg" } else { "mp4" }
                ))
            })
            .collect();
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        let started = Instant::now();

        request(
            &mut handler,
            Request::FilterKind(Some(dto::FileKind::Video)),
        );

        assert_eq!(handler.rows.len(), 2_500);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    // Go to Folder.

    fn in_dcim(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/DCIM".into());
        let _ = handler.update(listed(
            "/DCIM",
            &[folder("Camera"), folder("Screenshots"), file("a.jpg")],
        ));
        handler
    }

    /// Whether Go to Folder's dialog, alone, shows `words`.
    fn offers(handler: &Handler, words: &str) -> bool {
        let editor = handler.editor.as_ref().unwrap();
        let dialog = handler.view_editor(editor, handler.location.as_ref().unwrap());
        iced_test::simulator(dialog).find(words).is_ok()
    }

    fn path_typed(handler: &Handler) -> &str {
        &handler.editor.as_ref().unwrap().name
    }

    #[test]
    fn go_to_folder_starts_at_the_folder_shown_and_offers_its_folders() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = in_dcim(service.clone());

        request(&mut handler, Request::GoToFolder);

        assert!(handler.is_going_to() && handler.is_editing());
        assert_eq!(path_typed(&handler), "/DCIM/");
        assert!(offers(&handler, "Go to Folder"));
        assert!(offers(&handler, "Camera") && offers(&handler, "Screenshots"));
        assert!(!offers(&handler, "a.jpg"), "folders only");
        assert!(
            service.folders_asked.lock().unwrap().is_empty(),
            "known here"
        );

        request(&mut handler, Request::EditName("/DCIM/sc".into()));
        assert!(offers(&handler, "Screenshots") && !offers(&handler, "Camera"));
        request(&mut handler, Request::Escape);
        assert!(!handler.is_editing());
    }

    #[tokio::test]
    async fn another_folder_is_asked_for_once_as_its_names_are_typed() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.folders = vec!["Rock".into(), "Jazz".into()];
        let service = Arc::new(service);
        let mut handler = in_dcim(service.clone());
        request(&mut handler, Request::GoToFolder);

        let asked = handler.update(Message::Request(Request::EditName("/Music/".into())));
        settle_task(&mut handler, asked).await;
        assert!(offers(&handler, "Jazz") && offers(&handler, "Rock"));
        let again = handler.update(Message::Request(Request::EditName("/Music/J".into())));
        settle_task(&mut handler, again).await;

        assert_eq!(*service.folders_asked.lock().unwrap(), ["/Music"]);
        assert!(offers(&handler, "Jazz") && !offers(&handler, "Rock"));
        // A late answer about a folder no longer typed is dropped.
        let _ = handler.update(Message::FolderNames {
            parent: "/Elsewhere".into(),
            names: vec!["Jumble".into()],
        });
        assert!(
            !offers(&handler, "Jumble"),
            "a J name, so only the folder check keeps it out"
        );
        assert!(offers(&handler, "Jazz"));
    }

    #[test]
    fn return_goes_to_what_is_typed_or_to_the_folder_picked() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = in_dcim(service.clone());
        request(&mut handler, Request::GoToFolder);
        request(&mut handler, Request::EditName("Camera".into()));
        request(&mut handler, Request::Submit);
        assert!(!handler.is_editing());
        assert_eq!(path_of(&handler), Some("/DCIM/Camera"), "from here");

        let mut handler = in_dcim(service);
        request(&mut handler, Request::GoToFolder);
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        assert!(offers(&handler, "Screenshots"));
        request(&mut handler, Request::Submit);
        assert_eq!(
            path_of(&handler),
            Some("/DCIM/Screenshots"),
            "the second offered"
        );
        assert!(handler.can_go_back(), "a step Back undoes");

        let mut wrapped = in_dcim(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut wrapped, Request::GoToFolder);
        request(
            &mut wrapped,
            Request::Step {
                delta: -1,
                extend: false,
            },
        );
        request(&mut wrapped, Request::Submit);
        assert_eq!(
            path_of(&wrapped),
            Some("/DCIM/Screenshots"),
            "↑ from none: the last"
        );
        let mut none = in_dcim(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut none, Request::GoToFolder);
        request(&mut none, Request::EditName("/DCIM/x".into()));
        request(
            &mut none,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        request(&mut none, Request::Submit);
        assert_eq!(path_of(&none), Some("/DCIM/x"), "nothing offered: as typed");
    }

    #[tokio::test]
    async fn a_name_clicked_is_filled_in_and_its_folders_are_offered_next() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.folders = vec!["2026".into()];
        let service = Arc::new(service);
        let mut handler = in_dcim(service.clone());
        request(&mut handler, Request::GoToFolder);

        let mut clicked_messages = Vec::new();
        {
            let editor = handler.editor.as_ref().unwrap();
            let dialog = handler.view_editor(editor, handler.location.as_ref().unwrap());
            let mut ui = iced_test::simulator(dialog);
            let _ = ui.click("Camera");
            clicked_messages.extend(ui.into_messages());
        }
        for message in clicked_messages {
            let task = handler.update(message);
            settle_task(&mut handler, task).await;
        }

        assert_eq!(path_typed(&handler), "/DCIM/Camera/");
        assert_eq!(*service.folders_asked.lock().unwrap(), ["/DCIM/Camera"]);
        assert!(offers(&handler, "2026"));
    }

    #[test]
    fn a_folder_that_is_not_there_says_so_where_it_was_asked_for() {
        let mut handler = in_dcim(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::GoToFolder);
        request(&mut handler, Request::EditName("/Nope".into()));
        request(&mut handler, Request::Submit);
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/Nope".into(),
            result: Err(error::to_error_response(dto::Error::PathNotFound(
                "/Nope".into(),
            ))),
        });

        assert_eq!(path_of(&handler), Some("/Nope"));
        assert!(handler.error.is_some());
        request(&mut handler, Request::Back);
        assert_eq!(path_of(&handler), Some("/DCIM"));
    }

    #[test]
    fn a_folder_of_five_thousand_folders_offers_a_few_and_redraws_without_sorting() {
        let mut handler = browsing("/");
        let folders: Vec<dto::Entry> = (0..5_000)
            .map(|n| folder(&format!("Album {n:04}")))
            .collect();
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(folders),
        });
        let started = Instant::now();
        request(&mut handler, Request::GoToFolder);
        request(&mut handler, Request::EditName("/album 12".into()));
        let Some(Editor {
            change: Change::GoTo(go),
            ..
        }) = &handler.editor
        else {
            panic!("not going to a folder");
        };
        assert_eq!(go.offered.len(), go_to::SUGGESTIONS);
        assert_eq!(go.offered[0], "Album 1200");
        for _ in 0..50 {
            let _ = handler.view();
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    // Renaming several at once.

    fn many(handler: &Handler) -> &Many {
        match handler.editor.as_ref().map(|editor| &editor.change) {
            Some(Change::RenameMany(many)) => many,
            other => panic!("not renaming several: {other:?}"),
        }
    }

    fn shows(handler: &Handler, words: &str) -> bool {
        iced_test::simulator(handler.view()).find(words).is_ok()
    }

    // Selecting against the other pane's folder.

    #[test]
    fn what_the_other_folder_lacks_or_has_is_selected_among_the_rows_shown() {
        let mut handler = browsing("/");
        let _ = handler.update(listed(
            "/",
            &[
                folder("Trip"),
                file("a.jpg"),
                file("b.jpg"),
                sized("c.jpg", 5),
                file("d.png"),
            ],
        ));
        // d.png is not backed up, but the filter hides it.
        let backup = [file("A.JPG"), sized("c.jpg", 6), folder("trip")];
        request(&mut handler, Request::Filter("jpg".into()));

        handler.select_against(compare::Against::Missing, &backup, "Backup");
        assert_eq!(
            selected(&handler),
            ["b.jpg", "c.jpg"],
            "only what the filter shows"
        );
        assert!(shows(&handler, "Selected 2 of 3: not in “Backup”"));

        request(&mut handler, Request::Filter(String::new()));
        handler.select_against(compare::Against::Same, &backup, "Backup");
        assert_eq!(selected(&handler), ["Trip", "a.jpg"]);
        assert!(shows(&handler, "Selected 2 of 5: also in “Backup”"));

        handler.select_against(compare::Against::Missing, &[], "Empty");
        assert_eq!(
            handler.selection.len(),
            5,
            "everything is missing from nothing"
        );
        handler.select_against(compare::Against::Same, &[], "Empty");
        assert!(
            handler.selection.is_empty(),
            "nothing selected for nothing found"
        );
        assert!(shows(&handler, "Nothing here is in “Empty” as it is here"));
    }

    #[test]
    fn five_thousand_rows_are_compared_with_four_thousand_at_once() {
        let mut handler = five_thousand_rows();
        let backup: Vec<dto::Entry> = (0..4_000)
            .map(|i| file(&format!("IMG_{i:04}.jpg")))
            .collect();

        let started = Instant::now();
        handler.select_against(compare::Against::Missing, &backup, "Backup");
        let took = started.elapsed();

        assert_eq!(handler.selection.len(), 1_000);
        eprintln!("5,000 rows against 4,000 in the pane in {took:?}");
        assert!(took < Duration::from_millis(300), "{took:?}");
        let _ = handler.view();
    }

    #[test]
    fn a_folder_is_ready_to_compare_once_read_and_is_named_as_shown() {
        let mut handler = handler();
        assert!(!handler.is_listed() && handler.shown_name().is_none());
        let _ = handler.open(storage(), "/".into());
        assert!(!handler.is_listed(), "still reading");
        let _ = handler.update(listed("/", &[file("a.jpg")]));
        assert!(handler.is_listed());
        assert_eq!(
            handler.shown_name().as_deref(),
            Some("Internal"),
            "the top: the storage"
        );
        assert_eq!(handler.entries().len(), 1);

        let _ = handler.update(Message::Request(Request::GoTo("/DCIM/Camera".into())));
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/DCIM/Camera".into(),
            result: Err(error::to_error_response(dto::Error::PathNotFound(
                "/DCIM/Camera".into(),
            ))),
        });
        assert!(!handler.is_listed(), "a folder that can't be read");
        assert_eq!(handler.shown_name().as_deref(), Some("Camera"));
    }

    fn clicked(handler: &Handler, label: &str) -> Vec<Message> {
        let mut ui = iced_test::simulator(handler.view());
        let _ = ui.click(label);
        ui.into_messages().collect()
    }

    #[test]
    fn rename_with_several_selected_renames_them_together_in_the_order_shown() {
        let mut handler = four_rows();
        request(&mut handler, Request::SelectAll);

        request(&mut handler, Request::Rename);

        let items: Vec<&str> = many(&handler)
            .items
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(
            items,
            ["DCIM", "a.txt", "b.txt", "c.txt"],
            "folders first, as shown"
        );
        assert!(many(&handler).items[0].1, "DCIM is a folder");
        assert!(shows(&handler, "Rename 4 items"));
        assert!(shows(&handler, "Nothing would change yet."));
        assert!(
            clicked(&handler, "Rename All").is_empty(),
            "nothing to do yet"
        );
    }

    #[test]
    fn the_new_names_are_shown_as_they_are_typed_and_a_clash_holds_back_the_lot() {
        let mut handler = four_rows();
        request(&mut handler, Request::Select("a.txt".into()));
        handler.set_modifiers(Modifiers::SHIFT);
        request(&mut handler, Request::Select("c.txt".into()));
        handler.set_modifiers(Modifiers::empty());
        request(&mut handler, Request::Rename);

        request(&mut handler, Request::RenameMode(ManyMode::Add));
        request(
            &mut handler,
            Request::RenameField(ManyField::Text, "Trip ".into()),
        );
        assert!(shows(&handler, "Trip a.txt"));
        assert!(shows(&handler, "3 items will be renamed."));
        request(&mut handler, Request::RenameAtEnd(true));
        assert!(shows(&handler, "aTrip .txt"), "straight after the name");

        request(&mut handler, Request::RenameMode(ManyMode::Replace));
        request(
            &mut handler,
            Request::RenameField(ManyField::Find, "a".into()),
        );
        request(
            &mut handler,
            Request::RenameField(ManyField::With, "b".into()),
        );
        assert!(!many(&handler).ready());
        assert!(shows(&handler, "b.txt — taken in this folder"));
        assert!(shows(
            &handler,
            "1 item can't be renamed so: change the pattern to rename any."
        ));
        assert!(clicked(&handler, "Rename All").is_empty(), "held back");
        request(&mut handler, Request::Submit);
        assert!(
            handler.editor.as_ref().is_some_and(|editor| !editor.busy),
            "Return in a field doesn't start it either"
        );

        request(&mut handler, Request::RenameMode(ManyMode::Number));
        request(
            &mut handler,
            Request::RenameField(ManyField::Base, "Day".into()),
        );
        assert!(shows(&handler, "Day 001.txt"));
        assert!(many(&handler).ready());
        assert!(matches!(
            clicked(&handler, "Rename All").as_slice(),
            [Message::Request(Request::Submit)]
        ));
        request(
            &mut handler,
            Request::RenameField(ManyField::Start, "x".into()),
        );
        assert!(shows(&handler, "Start at a whole number."));
        assert!(!many(&handler).ready());
    }

    fn renaming_two(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("a.jpg"), file("b.jpg"), file("c.jpg")]));
        request(&mut handler, Request::Select("a.jpg".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("b.jpg".into()));
        handler.set_modifiers(Modifiers::empty());
        request(&mut handler, Request::Rename);
        request(&mut handler, Request::RenameMode(ManyMode::Number));
        request(
            &mut handler,
            Request::RenameField(ManyField::Base, "Trip".into()),
        );
        handler
    }

    #[tokio::test]
    async fn several_are_renamed_one_by_one_and_one_undo_puts_them_all_back() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = renaming_two(service.clone());

        let submit = handler.update(Message::Request(Request::Submit));
        assert!(shows(&handler, "Working…"));
        assert!(clicked(&handler, "Working…").is_empty(), "once");
        settle_task(&mut handler, submit).await;

        let edits = service.edits.lock().unwrap().clone();
        assert_eq!(
            edits,
            [
                "rename 1 / a.jpg -> Trip 001.jpg",
                "rename 1 / b.jpg -> Trip 002.jpg"
            ]
        );
        assert!(handler.editor.is_none());
        assert_eq!(handler.notice.as_ref().unwrap().title, "Renamed 2 items");
        assert!(handler.can_undo());

        let undo = handler.undo();
        settle_task(&mut handler, undo).await;
        let edits = service.edits.lock().unwrap().clone();
        assert_eq!(
            &edits[2..],
            [
                "rename 1 / Trip 002.jpg -> b.jpg",
                "rename 1 / Trip 001.jpg -> a.jpg"
            ],
            "the last first"
        );
        assert_eq!(handler.notice.as_ref().unwrap().title, "Undid the rename");
    }

    #[tokio::test]
    async fn refused_renames_are_named_and_leave_nothing_to_undo() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.edit = Err(dto::Error::NameTaken("x".into()));
        let mut handler = renaming_two(Arc::new(service));

        let submit = handler.update(Message::Request(Request::Submit));
        settle_task(&mut handler, submit).await;

        let notice = handler.notice.as_ref().unwrap();
        assert_eq!(notice.title, "Renamed 0 of 2 items");
        assert!(notice.problem);
        assert_eq!(notice.details.len(), 2);
        assert!(!handler.can_undo());
        assert_eq!(selected(&handler), ["a.jpg", "b.jpg"], "still selected");
    }

    #[tokio::test]
    async fn a_lost_phone_stops_the_renames_and_says_so() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        service.edit = Err(dto::Error::Disconnected);
        let service = Arc::new(service);
        let mut handler = renaming_two(Arc::clone(&service));

        let submit = handler.update(Message::Request(Request::Submit));
        let messages = crate::handler::testing::drain(submit).await.0;

        assert!(messages.iter().any(|m| m.device_lost().is_some()));
        assert_eq!(service.edits.lock().unwrap().len(), 1, "not asked again");
    }

    #[tokio::test]
    async fn cancel_stops_a_run_between_renames() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = renaming_two(service.clone());
        let submit = handler.update(Message::Request(Request::Submit));
        let _ = handler.update(Message::RenamedSome(1));
        assert!(shows(&handler, "Renaming 1 of 2…"));
        assert!(handler.is_editing());

        for message in clicked(&handler, "Cancel") {
            let _ = handler.update(message);
        }
        settle_task(&mut handler, submit).await;

        assert!(
            service.edits.lock().unwrap().is_empty(),
            "stopped before the first"
        );
        let notice = handler.notice.as_ref().unwrap();
        assert_eq!(notice.title, "Renamed 0 of 2 items");
        assert!(handler.editor.is_none());
    }

    #[tokio::test]
    async fn undoing_several_renames_says_which_could_not_go_back() {
        for (refusal, said) in [
            (dto::Error::NameTaken("a.jpg".into()), "Can't undo"),
            (dto::Error::Disconnected, "Your phone was disconnected"),
        ] {
            let mut service = FakeExplorer::new(Ok(vec![]));
            service.edit = Err(refusal);
            let mut handler = Handler::new(Arc::new(service));
            let _ = handler.open(storage(), "/".into());
            let _ = handler.update(Message::RenamedMany {
                volume_id: 1,
                path: "/".into(),
                outcome: rename_many::Outcome {
                    done: vec![("a.jpg".into(), "x.jpg".into())],
                    failures: Vec::new(),
                    stopped: None,
                },
                asked: 1,
            });

            let undo = handler.undo();
            settle_task(&mut handler, undo).await;

            let notice = handler.notice.as_ref().unwrap();
            assert!(notice.problem);
            assert!(notice.title.starts_with(said), "{}", notice.title);
        }
    }

    #[tokio::test]
    async fn an_undo_that_put_some_names_back_says_which_kept_its_new_one() {
        let mut service = FakeExplorer::new(Ok(vec![]));
        // y.jpg goes back; x.jpg is refused.
        service.refuse_rename = Some("x.jpg");
        let renames: Vec<(String, String)> = vec![
            ("a.jpg".into(), "x.jpg".into()),
            ("b.jpg".into(), "y.jpg".into()),
        ];

        let result = undo::undo(
            Arc::new(service),
            1,
            Undo::RenamedMany {
                folder: "/".into(),
                renames,
            },
        )
        .await;

        assert_eq!(result, Err(dto::Error::PartlyUndone("x.jpg".into())));
    }

    #[test]
    fn renaming_five_thousand_shows_a_few_rows_and_plans_quickly() {
        let mut handler = browsing("/");
        let names: Vec<String> = (0..5_000).map(|n| format!("IMG_{n:05}.jpg")).collect();
        let entries: Vec<dto::Entry> = names.iter().map(|n| file(n)).collect();
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/".into(),
            result: Ok(entries),
        });
        request(&mut handler, Request::SelectAll);
        request(&mut handler, Request::Rename);
        let started = Instant::now();

        request(
            &mut handler,
            Request::RenameField(ManyField::Find, "IMG_".into()),
        );
        request(
            &mut handler,
            Request::RenameField(ManyField::With, "Trip ".into()),
        );
        assert!(shows(&handler, "and 4,994 more"));

        assert!(shows(&handler, "5,000 items will be renamed."));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "two keystrokes and a look: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn delete_takes_every_selected_item() {
        let mut handler = four_rows();
        request(&mut handler, Request::Select("a.txt".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("c.txt".into()));

        request(&mut handler, Request::Delete);

        assert_eq!(
            change(&handler),
            Some(&Change::Delete {
                names: vec!["a.txt".into(), "c.txt".into()],
                keep: Keep::No,
            })
        );
        assert_eq!(handler.selection().unwrap().names, ["a.txt", "c.txt"]);
    }

    fn jump(handler: &mut Handler, to: Jump, extend: bool) -> Task<Message> {
        handler.update(Message::Request(Request::Jump { to, extend }))
    }

    /// 5,000 photos in a list ten rows high.
    fn five_thousand_rows() -> Handler {
        let mut handler = browsing("/");
        let files: Vec<dto::Entry> = (0..5_000)
            .map(|i| file(&format!("IMG_{i:04}.jpg")))
            .collect();
        let _ = handler.update(listed("/", &files));
        handler.viewport = (0.0, handler.row_height() * 10.5, 600.0);
        handler
    }

    #[test]
    fn home_and_end_select_the_first_and_last_row_and_bring_it_into_view() {
        let mut handler = five_thousand_rows();

        let started = Instant::now();
        let scroll = jump(&mut handler, Jump::Last, false);
        let took = started.elapsed();
        assert_eq!(selected(&handler), ["IMG_4999.jpg"]);
        assert!(scroll.units() > 0, "scrolled to the bottom");
        eprintln!("End on 5,000 rows in {took:?}");
        assert!(took < Duration::from_millis(200), "{took:?}");

        // Scrolled to the bottom, as the list now is.
        handler.viewport.0 = handler.row_height() * 4_990.0;
        let scroll = jump(&mut handler, Jump::First, false);
        assert_eq!(selected(&handler), ["IMG_0000.jpg"]);
        assert!(scroll.units() > 0, "and back to the top");
        handler.viewport.0 = 0.0;
        assert_eq!(
            jump(&mut handler, Jump::First, false).units(),
            0,
            "in view already"
        );

        // With nothing selected yet, the first is the first, not the last.
        let mut fresh = five_thousand_rows();
        let _ = jump(&mut fresh, Jump::First, false);
        assert_eq!(selected(&fresh), ["IMG_0000.jpg"]);
        let _ = handler.view();
    }

    #[test]
    fn shift_with_home_or_end_selects_everything_from_the_anchor() {
        let mut handler = five_thousand_rows();
        request(&mut handler, Request::Select("IMG_4997.jpg".into()));
        request(&mut handler, Request::PointerReleased);

        let _ = jump(&mut handler, Jump::Last, true);
        assert_eq!(
            selected(&handler),
            ["IMG_4997.jpg", "IMG_4998.jpg", "IMG_4999.jpg"]
        );

        let _ = jump(&mut handler, Jump::First, true);
        assert_eq!(
            handler.selection.len(),
            4_998,
            "from the anchor up to the top"
        );
        assert_eq!(handler.selection.cursor(), Some("IMG_0000.jpg"));
    }

    #[test]
    fn page_up_and_down_move_a_screen_of_whole_rows() {
        let mut handler = five_thousand_rows();
        request(&mut handler, Request::Select("IMG_0000.jpg".into()));
        request(&mut handler, Request::PointerReleased);

        let scroll = jump(&mut handler, Jump::PageDown, false);
        assert_eq!(selected(&handler), ["IMG_0010.jpg"], "ten whole rows fit");
        assert!(scroll.units() > 0);
        let _ = jump(&mut handler, Jump::PageDown, true);
        assert_eq!(handler.selection.len(), 11, "from IMG_0010 to IMG_0020");
        let _ = jump(&mut handler, Jump::PageUp, false);
        assert_eq!(selected(&handler), ["IMG_0010.jpg"]);
        let _ = jump(&mut handler, Jump::PageUp, true);
        assert_eq!(handler.selection.len(), 11, "from IMG_0010 up to IMG_0000");
        assert_eq!(handler.selection.cursor(), Some("IMG_0000.jpg"));
        let _ = jump(&mut handler, Jump::PageDown, false);
        let _ = jump(&mut handler, Jump::PageUp, false);
        let _ = jump(&mut handler, Jump::PageUp, false);
        assert_eq!(selected(&handler), ["IMG_0000.jpg"], "stops at the top");

        // Before the list has a height, a page is one row.
        handler.viewport = (0.0, 0.0, 0.0);
        let _ = jump(&mut handler, Jump::PageDown, false);
        assert_eq!(selected(&handler), ["IMG_0001.jpg"]);
    }

    #[test]
    fn in_the_grid_a_page_is_whole_rows_of_tiles_and_the_ends_are_the_ends() {
        let (mut handler, _) = in_grid(&photos(400));
        let _ = handler.set_view(dto::ViewMode::Grid);
        handler.viewport = (0.0, 600.0, 560.0);
        let page = (600.0 / handler.tile.row_height()).floor() as usize * handler.columns();
        assert!(page > handler.columns(), "more than one row of tiles");
        let names = handler.row_names();
        request(&mut handler, Request::Select(names[0].clone()));
        request(&mut handler, Request::PointerReleased);

        let _ = jump(&mut handler, Jump::PageDown, false);
        assert_eq!(selected(&handler), [names[page].clone()]);
        let _ = jump(&mut handler, Jump::Last, false);
        assert_eq!(selected(&handler), [names[399].clone()]);
        let _ = jump(&mut handler, Jump::First, false);
        assert_eq!(selected(&handler), [names[0].clone()]);
    }

    #[test]
    fn a_jump_in_an_empty_folder_does_nothing() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[]));
        for to in [Jump::First, Jump::Last, Jump::PageUp, Jump::PageDown] {
            let _ = jump(&mut handler, to, false);
        }
        assert!(handler.selection.is_empty());
    }

    #[test]
    fn a_refresh_keeps_only_selected_items_that_are_still_there() {
        let mut handler = four_rows();
        request(&mut handler, Request::SelectAll);

        let _ = handler.update(listed("/", &[file("b.txt"), file("new.txt")]));

        assert_eq!(selected(&handler), ["b.txt"]);
    }

    #[test]
    fn the_list_scroll_position_is_tracked_and_reset_on_navigation() {
        let mut handler = four_rows();
        handler.viewport = (64.0, 100.0, 0.0);

        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        assert_eq!(handler.selection.cursor(), Some("DCIM"));

        request(&mut handler, Request::GoTo("/DCIM".into()));
        assert_eq!(handler.viewport.0, 0.0);
    }

    #[test]
    fn settings_change_hidden_files_folders_first_and_the_status_bar() {
        let mut handler = browsing("/");
        let _ = handler.update(listed("/", &[folder("Zoo"), file("apple.txt")]));

        let _ = handler.set_show_hidden(true);
        assert!(handler.show_hidden && handler.loading);
        let _ = handler.update(listed("/", &[folder("Zoo"), file("apple.txt")]));
        let _ = handler.set_show_hidden(true);
        assert!(!handler.loading, "an unchanged setting doesn't reload");

        handler.set_folders_first(false);
        assert_eq!(names(&handler), ["apple.txt", "Zoo"]);
        handler.set_folders_first(false);
        assert_eq!(names(&handler), ["apple.txt", "Zoo"]);

        handler.set_status_bar(false);
        let _ = handler.view();
        assert!(!handler.status_bar);
    }

    fn photos(count: usize) -> Vec<dto::Entry> {
        (0..count).map(|i| file(&format!("p{i:02}.jpg"))).collect()
    }

    fn in_grid(entries: &[dto::Entry]) -> (Handler, Arc<FakeExplorer>) {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", entries));
        (handler, service)
    }

    #[test]
    fn switching_to_the_grid_asks_for_thumbnails_of_files_only() {
        let (mut handler, _) = in_grid(&[folder("DCIM"), file("a.jpg"), file("notes.txt")]);

        let _ = handler.set_view(dto::ViewMode::Grid);

        assert!(matches!(
            handler.thumbnails.get("a.jpg"),
            Some(Thumb::Loading)
        ));
        assert!(matches!(
            handler.thumbnails.get("notes.txt"),
            Some(Thumb::Loading)
        ));
        assert!(!handler.thumbnails.contains_key("DCIM"));
        let _ = handler.set_view(dto::ViewMode::Grid);
        let _ = handler.view();
    }

    #[test]
    fn thumbnails_are_asked_for_in_batches() {
        let (mut handler, _) = in_grid(&photos(40));
        handler.viewport = (0.0, 10_000.0, 560.0);

        let _ = handler.set_view(dto::ViewMode::Grid);

        assert_eq!(handler.thumbnails.len(), THUMBNAILS_PER_BATCH);
    }

    #[test]
    fn a_thumbnail_arrives_and_is_shown() {
        let (mut handler, _) = in_grid(&[file("a.jpg"), file("notes.txt")]);
        let _ = handler.set_view(dto::ViewMode::Grid);
        let picture = mapper::to_picture(dto::Thumbnail {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        });

        let _ = handler.update(Message::Thumbnail {
            volume_id: 1,
            path: "/".into(),
            name: "a.jpg".into(),
            picture: Some(picture),
        });
        let _ = handler.update(Message::Thumbnail {
            volume_id: 1,
            path: "/".into(),
            name: "notes.txt".into(),
            picture: None,
        });

        assert!(matches!(
            handler.thumbnails.get("a.jpg"),
            Some(Thumb::Ready(_))
        ));
        assert!(matches!(
            handler.thumbnails.get("notes.txt"),
            Some(Thumb::None)
        ));
        let _ = handler.view();
    }

    #[test]
    fn a_thumbnail_for_a_folder_already_left_is_dropped() {
        let (mut handler, _) = in_grid(&[file("a.jpg")]);
        let _ = handler.set_view(dto::ViewMode::Grid);
        request(&mut handler, Request::GoTo("/DCIM".into()));
        assert!(handler.thumbnails.is_empty());

        let _ = handler.update(Message::Thumbnail {
            volume_id: 1,
            path: "/".into(),
            name: "a.jpg".into(),
            picture: None,
        });

        assert!(handler.thumbnails.is_empty());
    }

    #[test]
    fn scrolling_the_grid_asks_for_newly_visible_thumbnails() {
        let (mut handler, _) = in_grid(&photos(40));
        handler.viewport = (0.0, 100.0, 560.0);
        let _ = handler.set_view(dto::ViewMode::Grid);
        let before = handler.thumbnails.len();

        handler.viewport = (790.0, 100.0, 560.0);
        let _ = handler.request_thumbnails();

        assert!(handler.thumbnails.len() > before);
    }

    #[test]
    fn a_refresh_keeps_thumbnails_of_files_still_there() {
        let (mut handler, _) = in_grid(&[file("a.jpg"), file("b.jpg")]);
        let _ = handler.set_view(dto::ViewMode::Grid);

        let _ = handler.update(listed("/", &[file("a.jpg")]));

        assert!(handler.thumbnails.contains_key("a.jpg"));
        assert!(!handler.thumbnails.contains_key("b.jpg"));
    }

    #[test]
    fn the_list_view_never_asks_for_thumbnails() {
        let (mut handler, service) = in_grid(&[file("a.jpg")]);

        let _ = handler.request_thumbnails();

        assert!(handler.thumbnails.is_empty());
        assert!(service.edits.lock().unwrap().is_empty());
    }

    #[test]
    fn arrows_in_the_grid_move_by_rows_and_tiles() {
        let (mut handler, _) = in_grid(&photos(12));
        let _ = handler.set_view(dto::ViewMode::Grid);
        handler.viewport = (0.0, 600.0, 560.0); // 4 columns

        request(&mut handler, Request::Select("p00.jpg".into()));
        request(
            &mut handler,
            Request::Step {
                delta: 1,
                extend: false,
            },
        );
        assert_eq!(handler.selection.single(), Some("p04.jpg"));

        request(
            &mut handler,
            Request::Sideways {
                delta: 1,
                extend: false,
            },
        );
        assert_eq!(handler.selection.single(), Some("p05.jpg"));
    }

    #[test]
    fn left_and_right_do_nothing_in_the_list() {
        let (mut handler, _) = in_grid(&photos(3));
        request(&mut handler, Request::Select("p00.jpg".into()));

        request(
            &mut handler,
            Request::Sideways {
                delta: 1,
                extend: false,
            },
        );

        assert_eq!(handler.selection.single(), Some("p00.jpg"));
    }

    #[test]
    fn the_view_toggle_is_left_to_the_app() {
        let (mut handler, _) = in_grid(&photos(1));

        request(&mut handler, Request::SetView(dto::ViewMode::Grid));

        assert_eq!(handler.view, dto::ViewMode::List);
    }

    #[test]
    fn the_cursor_row_is_scrolled_to_in_the_grid() {
        let (mut handler, _) = in_grid(&photos(40));
        let _ = handler.set_view(dto::ViewMode::Grid);
        handler.viewport = (0.0, 158.0, 560.0);

        request(&mut handler, Request::Select("p00.jpg".into()));
        request(
            &mut handler,
            Request::Step {
                delta: 3,
                extend: false,
            },
        );

        assert_eq!(handler.selection.single(), Some("p12.jpg"));
    }

    #[tokio::test]
    async fn fetch_thumbnail_turns_the_answer_into_a_picture_or_nothing() {
        let service: Arc<dyn Service> = Arc::new(FakeExplorer::new(Ok(vec![])));

        let (path, name, picture) =
            fetch_thumbnail(service.clone(), 1, "/DCIM".into(), file("a.jpg")).await;
        let (_, _, none) = fetch_thumbnail(service, 1, "/DCIM".into(), file("notes.txt")).await;

        assert_eq!((path.as_str(), name.as_str()), ("/DCIM", "a.jpg"));
        assert!(matches!(picture, Ok(Some(_))));
        assert!(matches!(none, Ok(None)));
    }

    #[tokio::test]
    async fn a_thumbnail_the_phone_never_gave_says_the_phone_is_lost_and_others_say_nothing() {
        let silent: Arc<dyn Service> = Arc::new(FakeExplorer {
            thumbnail_error: Some(dto::Error::Timeout),
            ..FakeExplorer::new(Ok(vec![]))
        });
        let refused: Arc<dyn Service> = Arc::new(FakeExplorer {
            thumbnail_error: Some(dto::Error::Unsupported),
            ..FakeExplorer::new(Ok(vec![]))
        });

        let (_, _, lost) = fetch_thumbnail(silent, 1, "/".into(), file("a.jpg")).await;
        let (_, _, nothing) = fetch_thumbnail(refused, 1, "/".into(), file("a.jpg")).await;

        let Err(lost) = lost else {
            panic!("a silent phone is said")
        };
        assert!(lost.device_lost);
        assert!(
            Message::ThumbnailLost {
                volume_id: 1,
                path: "/".into(),
                error: lost
            }
            .device_lost()
            .is_some(),
            "and the app sees it as a lost phone"
        );
        assert!(matches!(nothing, Ok(None)));
    }

    #[test]
    fn tiles_render_in_rows_with_every_kind_of_preview() {
        let (mut handler, _) =
            in_grid(&[folder("DCIM"), file("a.jpg"), file("b.jpg"), file("c.txt")]);
        let _ = handler.set_view(dto::ViewMode::Grid);
        let _ = handler.update(Message::Thumbnail {
            volume_id: 1,
            path: "/".into(),
            name: "a.jpg".into(),
            picture: Some(mapper::to_picture(dto::Thumbnail {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            })),
        });
        request(&mut handler, Request::Select("b.jpg".into()));

        let _ = handler.tiles(3, 600.0);
        let _ = handler.tiles(1, 600.0);
    }

    #[test]
    fn the_list_renders_at_every_width() {
        let mut handler = browsing("/DCIM");
        request(&mut handler, Request::Select("notes.txt".into()));
        let location = handler.location.as_ref().unwrap();

        for width in [900.0, 560.0, 480.0, 300.0] {
            let _ = handler.view_fitted(location, layout::for_width(width, handler.columns));
        }
    }
    const AREA: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 600.0,
        height: 400.0,
    };

    fn show_menu(handler: &mut Handler) {
        let _ = handler.update(Message::ShowMenu {
            at: Point::new(100.0, 100.0),
            area: AREA,
        });
    }

    fn right_click(handler: &mut Handler, name: &str) {
        request(handler, Request::MenuOn(name.into()));
        show_menu(handler);
    }

    #[test]
    fn a_right_click_selects_the_entry_and_opens_its_menu() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("DCIM".into()));

        right_click(&mut handler, "notes.txt");

        assert_eq!(selected(&handler), ["notes.txt"]);
        assert_eq!(
            handler.menu,
            Some(OpenMenu {
                at: Point::new(100.0, 100.0),
                area: AREA,
                on_entry: true
            })
        );
        assert!(handler.menu(false).is_some());
    }

    #[test]
    fn a_right_click_inside_the_selection_keeps_it() {
        let mut handler = browsing("/");
        request(&mut handler, Request::SelectAll);

        right_click(&mut handler, "DCIM");

        assert_eq!(selected(&handler).len(), 2);
    }

    #[test]
    fn a_right_click_on_empty_space_drops_the_selection() {
        let mut handler = browsing("/");
        request(&mut handler, Request::Select("notes.txt".into()));

        show_menu(&mut handler);

        assert!(selected(&handler).is_empty());
        assert!(handler.menu.is_some_and(|menu| !menu.on_entry));
    }

    #[test]
    fn escape_closes_the_menu_before_touching_the_selection() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");

        request(&mut handler, Request::Escape);
        assert!(!handler.has_menu());
        assert_eq!(selected(&handler), ["notes.txt"]);

        request(&mut handler, Request::Escape);
        assert!(selected(&handler).is_empty());
    }

    #[test]
    fn close_menu_closes_it() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");

        request(&mut handler, Request::CloseMenu);

        assert!(!handler.has_menu());
        assert!(handler.menu(false).is_none());
    }

    #[test]
    fn menu_open_enters_the_folder() {
        let mut handler = browsing("/");
        right_click(&mut handler, "DCIM");

        request(&mut handler, Request::Menu(menu::Action::Open));

        assert_eq!(path(&handler), "/DCIM");
        assert!(!handler.has_menu());
    }

    #[test]
    fn menu_edits_open_their_dialogs() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");
        request(&mut handler, Request::Menu(menu::Action::Rename));
        assert!(matches!(change(&handler), Some(Change::Rename { .. })));
        assert!(!handler.has_menu());
        request(&mut handler, Request::Cancel);

        show_menu(&mut handler);
        request(&mut handler, Request::Menu(menu::Action::NewFolder));
        assert_eq!(change(&handler), Some(&Change::NewFolder));
        request(&mut handler, Request::Cancel);

        right_click(&mut handler, "notes.txt");
        request(&mut handler, Request::Menu(menu::Action::Delete));
        assert!(matches!(change(&handler), Some(Change::Delete { .. })));
    }

    #[test]
    fn menu_select_all_and_refresh() {
        let mut handler = browsing("/");
        show_menu(&mut handler);
        request(&mut handler, Request::Menu(menu::Action::SelectAll));
        assert_eq!(selected(&handler).len(), 2);

        show_menu(&mut handler);
        request(&mut handler, Request::Menu(menu::Action::Refresh));
        assert!(handler.is_loading());
    }

    fn mac_browsing(fake: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(fake);
        let _ = handler.open(mac(), "/Users/me".into());
        let _ = handler.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("notes.txt"), file("song.mp3")]),
        });
        handler
    }

    #[test]
    fn show_in_finder_reveals_the_first_selected_item() {
        let fake = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = mac_browsing(Arc::clone(&fake));
        request(&mut handler, Request::SelectAll);
        right_click(&mut handler, "notes.txt");

        request(&mut handler, Request::Menu(menu::Action::ShowInFinder));

        assert_eq!(*fake.edits.lock().unwrap(), ["reveal /Users/me/notes.txt"]);
        assert!(!handler.has_menu());
    }

    #[test]
    fn show_in_finder_does_nothing_on_the_phone() {
        let fake = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(Arc::clone(&fake) as Arc<dyn Service>);
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("notes.txt")]));
        right_click(&mut handler, "notes.txt");

        request(&mut handler, Request::Menu(menu::Action::ShowInFinder));

        assert!(fake.edits.lock().unwrap().is_empty());
    }

    #[test]
    fn app_wide_commands_only_close_the_menu_here() {
        for action in [
            menu::Action::Transfer,
            menu::Action::Copy,
            menu::Action::CopyToQueue,
            menu::Action::Paste,
        ] {
            let mut handler = browsing("/");
            right_click(&mut handler, "notes.txt");

            request(&mut handler, Request::Menu(action));

            assert!(!handler.has_menu(), "{action:?}");
            assert_eq!(selected(&handler), ["notes.txt"], "{action:?}");
            assert!(handler.editor.is_none(), "{action:?}");
        }
    }

    #[test]
    fn no_menu_while_a_dialog_is_open() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);

        right_click(&mut handler, "notes.txt");

        assert!(!handler.has_menu());
        assert!(handler.menu_target.is_none());
        assert!(selected(&handler).is_empty(), "the dialog keeps its folder");
    }

    #[test]
    fn no_menu_without_an_open_volume() {
        let mut handler = handler();

        show_menu(&mut handler);

        assert!(!handler.has_menu());
    }

    #[test]
    fn leaving_the_folder_or_closing_drops_the_menu() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");
        request(&mut handler, Request::GoTo("/DCIM".into()));
        assert!(!handler.has_menu());

        right_click(&mut handler, "notes.txt");
        handler.close();
        assert!(!handler.has_menu());
    }

    use iced::advanced::mouse as advanced_mouse;
    use iced_test::simulator;

    #[test]
    fn right_clicking_a_row_in_the_pane_opens_the_menu_for_it() {
        let mut handler = browsing("/");
        let messages: Vec<Message> = {
            let mut ui = simulator(handler.view());
            let row = ui.find("notes.txt").unwrap();
            ui.point_at(row.visible_bounds().unwrap().center());
            let _ = ui.simulate([iced::Event::Mouse(advanced_mouse::Event::ButtonPressed(
                advanced_mouse::Button::Right,
            ))]);
            ui.into_messages().collect()
        };
        assert!(
            matches!(
                messages.as_slice(),
                [
                    Message::Request(Request::MenuOn(name)),
                    Message::ShowMenu { .. }
                ] if name == "notes.txt"
            ),
            "{messages:?}"
        );

        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(selected(&handler), ["notes.txt"]);
        assert!(handler.menu.is_some_and(|menu| menu.on_entry));
    }

    #[test]
    fn right_clicking_a_tile_in_the_grid_opens_the_menu_for_it() {
        let mut handler = browsing("/");
        let _ = handler.set_view(dto::ViewMode::Grid);
        let messages: Vec<Message> = {
            let mut ui = simulator(handler.view());
            let tile = ui.find("DCIM").unwrap();
            ui.point_at(tile.visible_bounds().unwrap().center());
            let _ = ui.simulate([iced::Event::Mouse(advanced_mouse::Event::ButtonPressed(
                advanced_mouse::Button::Right,
            ))]);
            ui.into_messages().collect()
        };

        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(selected(&handler), ["DCIM"]);
        assert!(handler.has_menu());
    }

    fn click_in_menu(handler: &Handler, can_paste: bool, label: &str) -> Vec<Message> {
        let mut ui = simulator(handler.menu(can_paste).unwrap());
        let _ = ui.click(label).unwrap();
        ui.into_messages().collect()
    }

    #[test]
    fn clicking_a_command_in_the_menu_sends_it() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");

        let messages = click_in_menu(&handler, false, "Rename…");

        assert!(
            matches!(
                messages.as_slice(),
                [Message::Request(Request::Menu(menu::Action::Rename))]
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn disabled_commands_cannot_be_clicked() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");

        assert!(click_in_menu(&handler, false, "Paste").is_empty());
        assert_eq!(click_in_menu(&handler, true, "Paste").len(), 1);
        // A file opens in its Mac app now.
        assert_eq!(click_in_menu(&handler, false, "Open").len(), 1);
    }

    #[tokio::test]
    async fn opening_a_file_hands_it_to_the_app_and_a_folder_is_still_gone_into() {
        let mut handler = browsing("/");

        let by_name = handler.update(Message::Request(Request::Open("notes.txt".into())));
        let _ = handler.update(Message::Request(Request::Select("notes.txt".into())));
        let by_enter = handler.update(Message::Request(Request::OpenSelected));
        let missing = handler.update(Message::Request(Request::Open("gone.txt".into())));

        for task in [by_name, by_enter] {
            let messages = drain(task).await.0;
            assert!(
                matches!(messages.as_slice(), [Message::OpenFile(name)] if name == "notes.txt"),
                "{messages:?}"
            );
        }
        assert_eq!(missing.units(), 0);
        assert_eq!(
            handler
                .update(Message::OpenFile("notes.txt".into()))
                .units(),
            0
        );
        let _ = handler.update(Message::Request(Request::Open("DCIM".into())));
        assert_eq!(handler.location().map(|(_, path)| path), Some("/DCIM"));
    }

    #[test]
    fn the_menu_draws_in_both_themes_with_a_hovered_row() {
        let mut handler = browsing("/");
        right_click(&mut handler, "notes.txt");

        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let mut ui = simulator(handler.menu(false).unwrap());
            let copy = ui.find("Copy").unwrap();
            ui.point_at(copy.visible_bounds().unwrap().center());
            assert!(ui.snapshot(&theme).is_ok());
        }
    }
    /// A handler on storage 1 holding `n` photos, in `view`, with the list
    /// scrolled `offset` pixels down.
    fn folder_of(n: usize, view: dto::ViewMode, offset: f32) -> Handler {
        dense_folder_of(n, view, dto::Density::Standard, offset)
    }

    const DENSITIES: [dto::Density; 3] = [
        dto::Density::Compact,
        dto::Density::Standard,
        dto::Density::Large,
    ];

    fn dense_folder_of(
        n: usize,
        view: dto::ViewMode,
        density: dto::Density,
        offset: f32,
    ) -> Handler {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.set_view(view);
        let _ = handler.set_density(density);
        let entries: Vec<_> = (0..n).map(|i| file(&format!("IMG_{i:05}.jpg"))).collect();
        let _ = handler.update(listed("/", &entries));
        handler.viewport.0 = offset;
        handler
    }

    #[test]
    fn a_long_name_is_cut_in_the_middle_keeping_its_end_and_never_runs_under_the_next() {
        // A user's folder of game files once drew "...Switch NSP Update
        // v524288.nsp" straight over "NSP file", and then, clipped at the
        // column, showed only the start: two names differing at the end
        // looked the same. Cut in the middle, the end shows and the middle
        // goes. (Long enough to pass the Name column, which takes what the
        // fixed columns leave of a 1100-point pane.)
        let middle = |filler: &str| {
            format!(
                "FINAL FANTASY TACTICS - {filler} Switch NSP Edition 2025 Deluxe Collector's Bundle with every expansion Update v524288.nsp"
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let hash = dir.path().join("row");
        let looks = |name: String| {
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let _ = handler.update(listed("/", &[sized(&name, 7_300_000_000)]));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(1100.0, 300.0),
                handler.view(),
            );
            assert!(ui.find(name.as_str()).is_ok(), "found by its whole name");
            ui.snapshot(&iced::Theme::Dark)
                .unwrap()
                .matches_hash(&hash)
                .unwrap()
        };

        assert!(
            looks(middle("The Ivalice Chronicles")),
            "writes the first look"
        );
        assert!(
            looks(middle(
                "The Ivalice Chronicles Remastered And Expanded Further Still"
            )),
            "names that differ only in the middle look the same: the middle is cut"
        );
        assert!(
            !looks(middle("The Ivalice Chronicles").replace("v524288", "v999999")),
            "names that differ at the end look different: the end shows"
        );
    }

    fn rows_built(handler: &Handler, n: usize) -> Vec<usize> {
        let built_in = |language| {
            crate::i18n::with_language(language, || {
                let mut ui = iced_test::simulator(handler.view());
                (0..n)
                    .filter(|i| ui.find(format!("IMG_{i:05}.jpg")).is_ok())
                    .collect::<Vec<usize>>()
            })
        };
        let in_english = built_in(dto::Language::English);
        for language in dto::Language::ALL {
            assert_eq!(
                built_in(language),
                in_english,
                "{language:?} builds other rows than English"
            );
        }
        in_english
    }

    fn filter(handler: &mut Handler, text: &str) {
        let _ = handler.update(Message::Request(Request::Filter(text.into())));
    }

    fn shown(handler: &Handler) -> Vec<String> {
        handler.rows.iter().map(|row| row.name.clone()).collect()
    }

    fn in_thai<Output>(work: impl FnOnce() -> Output) -> Output {
        crate::i18n::with_language(dto::Language::Thai, work)
    }

    fn trip() -> Handler {
        let mut handler = handler();
        let _ = handler.open(storage(), "/DCIM".into());
        let photo = dto::Entry {
            modified: Some(dto::Timestamp {
                year: 2020,
                month: 1,
                day: 5,
                hour: 9,
                minute: 5,
            }),
            ..sized("写真.jpg", 1_500)
        };
        let _ = handler.update(listed(
            "/DCIM",
            &[folder("รูปทริป"), photo, file("notes.txt")],
        ));
        handler
    }

    #[test]
    fn a_language_switch_rewords_the_rows_and_keeps_the_place() {
        let mut handler = trip();
        let _ = handler.update(Message::Request(Request::Select("写真.jpg".into())));
        let row = |handler: &Handler, name: &str| {
            let at = handler
                .rows
                .iter()
                .position(|row| row.name == name)
                .unwrap();
            (
                handler.rows[at].kind.clone(),
                handler.rows[at].modified.clone(),
            )
        };
        assert_eq!(
            row(&handler, "写真.jpg"),
            ("JPG file".into(), "5 Jan 2020".into())
        );

        in_thai(|| {
            handler.language_changed();

            let kind = tf("explorer.kind.extension", &[("extension", "JPG")]);
            let modified = tf(
                "date.other_year",
                &[("day", "5"), ("month", t("month.1")), ("year", "2020")],
            );
            assert_eq!(row(&handler, "写真.jpg"), (kind, modified));
            assert_eq!(row(&handler, "รูปทริป").0, t("explorer.kind.folder"));
        });
        assert_ne!(row(&handler, "写真.jpg").0, "JPG file");
        assert_eq!(shown(&handler), ["รูปทริป", "notes.txt", "写真.jpg"]);
        assert_eq!(handler.location(), Some((1, "/DCIM")));
        assert_eq!(handler.selection().unwrap().names, ["写真.jpg"]);
    }

    #[test]
    fn a_language_switch_keeps_the_filter_and_the_sort() {
        let mut handler = trip();
        filter(&mut handler, "jpg");
        let _ = handler.update(Message::Request(Request::SortBy(dto::SortKey::Size)));
        let sort = handler.sort;

        in_thai(|| handler.language_changed());

        assert_eq!(handler.filter, "jpg");
        assert_eq!(handler.sort, sort);
        assert_eq!(shown(&handler), ["写真.jpg"]);
        assert_eq!(
            handler.rows[0].kind,
            in_thai(|| tf("explorer.kind.extension", &[("extension", "JPG")]))
        );
    }

    #[test]
    fn a_language_switch_clears_the_banner_and_rewords_the_error() {
        let mut handler = trip();
        handler.notice = Some(NoticeResponse {
            title: "Copied “a.jpg”".into(),
            details: Vec::new(),
            problem: false,
        });
        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/DCIM".into(),
            result: Err(error::to_error_response(dto::Error::PathNotFound(
                "/DCIM".into(),
            ))),
        });
        assert_eq!(handler.error.as_ref().unwrap().title, "This folder is gone");

        in_thai(|| handler.language_changed());

        assert!(handler.notice.is_none());
        let said = in_thai(|| t("error.path_not_found.title"));
        assert_ne!(said, "This folder is gone");
        assert_eq!(handler.error.as_ref().unwrap().title, said);
        in_thai(|| {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find(said).is_ok());
        });
    }

    #[test]
    fn a_language_switch_rewords_the_group_headers() {
        let mut handler = trip();
        handler.set_group_by(dto::GroupBy::Kind);
        let labels = |handler: &Handler| -> Vec<String> {
            handler
                .groups
                .iter()
                .map(|group| group.label.clone())
                .collect()
        };
        assert_eq!(labels(&handler), ["Folders", "Photos", "Documents"]);

        in_thai(|| handler.language_changed());

        assert_eq!(
            labels(&handler),
            in_thai(|| [
                t("explorer.group.folders"),
                t("label.kind.photos"),
                t("label.kind.documents"),
            ])
        );
    }

    #[test]
    fn a_language_switch_with_nothing_open_changes_nothing() {
        let mut handler = handler();

        in_thai(|| handler.language_changed());

        assert!(handler.location().is_none());
        assert!(handler.rows.is_empty());
    }

    /// Five names: two beach photos, a beach video, a note and a folder.
    fn holiday() -> Handler {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed(
            "/",
            &[
                file("Beach day.jpg"),
                file("beach night.jpg"),
                file("beach.mp4"),
                file("notes.txt"),
                folder("Beach trip"),
            ],
        ));
        handler
    }

    #[test]
    fn typing_shows_only_names_holding_every_word_in_any_case() {
        let mut handler = holiday();

        filter(&mut handler, "beach");
        assert_eq!(
            shown(&handler),
            [
                "Beach trip",
                "Beach day.jpg",
                "beach night.jpg",
                "beach.mp4"
            ]
        );
        filter(&mut handler, "BEACH jpg");
        assert_eq!(shown(&handler), ["Beach day.jpg", "beach night.jpg"]);
        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("2 of 5 shown").is_ok());
        }

        filter(&mut handler, "  ");
        assert_eq!(shown(&handler).len(), 5, "nothing typed, nothing hidden");
    }

    #[test]
    fn what_the_filter_hides_is_not_selected_so_nothing_acts_on_it() {
        let mut handler = holiday();
        let _ = handler.update(Message::Request(Request::SelectAll));
        assert_eq!(handler.selection.len(), 5);

        filter(&mut handler, "night");

        assert_eq!(handler.selection.names(), ["beach night.jpg"]);
        let _ = handler.update(Message::Request(Request::Delete));
        assert!(matches!(
            &handler.editor,
            Some(Editor { change: Change::Delete { names, .. }, .. }) if names == &["beach night.jpg"]
        ));
        let _ = handler.update(Message::Request(Request::Cancel));
        filter(&mut handler, "beach");
        let _ = handler.update(Message::Request(Request::SelectAll));
        assert_eq!(
            handler.selection.len(),
            4,
            "everything shown, and only that"
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("4 selected · 4 of 5 shown").is_ok());
    }

    #[test]
    fn escape_clears_the_filter_before_the_selection() {
        let mut handler = holiday();
        filter(&mut handler, "beach");
        let _ = handler.update(Message::Request(Request::Select("beach.mp4".into())));

        let _ = handler.update(Message::Request(Request::Escape));
        assert_eq!(shown(&handler).len(), 5);
        assert_eq!(handler.selection.names(), ["beach.mp4"], "still selected");

        let _ = handler.update(Message::Request(Request::Escape));
        assert!(handler.selection.is_empty());
    }

    #[test]
    fn a_new_folder_clears_the_filter_and_a_new_reading_keeps_it() {
        let mut handler = holiday();
        filter(&mut handler, "beach");

        let _ = handler.update(listed(
            "/",
            &[
                file("beach.mp4"),
                file("beach new.jpg"),
                file("notes.txt"),
                folder("Beach trip"),
            ],
        ));
        assert_eq!(
            shown(&handler),
            ["Beach trip", "beach new.jpg", "beach.mp4"]
        );

        let _ = handler.update(Message::Request(Request::Open("Beach trip".into())));
        assert_eq!(handler.filter, "");
    }

    #[tokio::test]
    async fn return_in_the_filter_opens_a_single_match_or_picks_the_first() {
        let mut handler = holiday();
        filter(&mut handler, "beach");
        let _ = handler.update(Message::Request(Request::SubmitFilter));
        assert_eq!(
            handler.selection.names(),
            ["Beach trip"],
            "the first is picked"
        );

        filter(&mut handler, "notes");
        let opened = handler.update(Message::Request(Request::SubmitFilter));
        let messages = drain(opened).await.0;
        assert!(
            matches!(messages.as_slice(), [Message::OpenFile(name)] if name == "notes.txt"),
            "{messages:?}"
        );

        filter(&mut handler, "trip");
        let _ = handler.update(Message::Request(Request::SubmitFilter));
        assert_eq!(
            handler.location().map(|(_, path)| path),
            Some("/Beach trip")
        );

        filter(&mut handler, "nothing like it");
        let none = handler.update(Message::Request(Request::SubmitFilter));
        assert_eq!(none.units(), 0);
    }

    fn android_storage() -> Volume {
        Volume {
            hides_app_folders: true,
            ..storage()
        }
    }

    /// The pane on `volume` showing `path` empty.
    fn empty_at(volume: Volume, path: &str) -> Handler {
        let mut handler = handler();
        let _ = handler.open(volume, path.into());
        let _ = handler.update(listed(path, &[]));
        handler
    }

    #[test]
    fn androids_app_folders_say_why_they_look_empty() {
        for path in [
            "/Android/data",
            "/android/OBB",
            "/Android/data/com.example.app",
        ] {
            let handler = empty_at(android_storage(), path);
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find(mapper::app_folders_note()).is_ok(), "{path}");
            assert!(ui.find("Nothing here can be seen from a Mac").is_ok());
            assert!(ui.find("This folder is empty").is_err(), "{path}");
        }

        // Another empty folder on the same phone is just empty, and so is
        // that folder on a phone that is not Android.
        for (volume, path) in [
            (android_storage(), "/Android/media"),
            (android_storage(), "/Music"),
            (storage(), "/Android/data"),
        ] {
            let handler = empty_at(volume, path);
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("This folder is empty").is_ok(), "{path}");
            assert!(ui.find(mapper::app_folders_note()).is_err(), "{path}");
        }
    }

    #[test]
    fn a_refused_read_in_an_app_folder_says_why_under_the_error() {
        let refused = |path: &str| {
            let mut handler = handler();
            let _ = handler.open(android_storage(), path.into());
            let _ = handler.update(Message::Listed {
                volume_id: 1,
                path: path.into(),
                result: Err(crate::handler::error::to_error_response(
                    dto::Error::PermissionDenied,
                )),
            });
            handler
        };

        let inside = refused("/Android/data/com.example.app");
        let mut ui = iced_test::simulator(inside.view());
        assert!(ui.find(mapper::app_folders_refused()).is_ok());
        assert!(ui.find(mapper::app_folders_note()).is_ok());
        let usual = crate::handler::error::to_error_response(dto::Error::PermissionDenied);
        assert!(
            ui.find(usual.hint.as_str()).is_err(),
            "no unplugging a phone that is fine"
        );
        assert!(ui.find("Try again").is_ok(), "the error's own buttons stay");

        // A phone lost while in there is said as lost, Reconnect and all.
        let mut lost = handler();
        let _ = lost.open(android_storage(), "/Android/data".into());
        let gone = crate::handler::error::to_error_response(dto::Error::Disconnected);
        let _ = lost.update(Message::Listed {
            volume_id: 1,
            path: "/Android/data".into(),
            result: Err(gone.clone()),
        });
        let mut ui = iced_test::simulator(lost.view());
        assert!(ui.find(gone.title.as_str()).is_ok());
        assert!(ui.find(mapper::app_folders_note()).is_err());

        let elsewhere = refused("/DCIM");
        let mut ui = iced_test::simulator(elsewhere.view());
        assert!(ui.find(mapper::app_folders_note()).is_err());
    }

    /// The phone pane at / with notes.txt (12 B), Trip (a folder of 40 B)
    /// and big.mov (3 GB), keeping copies of deletes when `keep` says so.
    async fn deleting(keep: bool, folder_bytes: u64) -> Handler {
        let fake = FakeExplorer {
            folder_sizes: vec![("/Trip".into(), folder_bytes)],
            ..FakeExplorer::new(Ok(vec![]))
        };
        let mut handler = Handler::new(Arc::new(fake));
        handler.set_keep_deleted(keep);
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed(
            "/",
            &[
                sized("notes.txt", 12),
                folder("Trip"),
                sized("big.mov", 3_000_000_000),
            ],
        ));
        handler
    }

    async fn ask_to_delete(handler: &mut Handler, names: &[&str]) {
        let _ = handler.update(Message::Request(Request::Select(names[0].into())));
        handler.set_modifiers(Modifiers::COMMAND);
        for name in &names[1..] {
            let _ = handler.update(Message::Request(Request::Select((*name).into())));
        }
        handler.set_modifiers(Modifiers::empty());
        let task = handler.update(Message::Request(Request::Delete));
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    fn keep_of(handler: &Handler) -> Option<Keep> {
        match &handler.editor.as_ref()?.change {
            Change::Delete { keep, .. } => Some(*keep),
            _ => None,
        }
    }

    #[tokio::test]
    async fn keeping_deletes_counts_first_and_hands_the_delete_to_the_queue() {
        let mut handler = deleting(true, 40).await;

        ask_to_delete(&mut handler, &["notes.txt", "Trip"]).await;

        assert_eq!(
            keep_of(&handler),
            Some(Keep::Copy(52)),
            "12 B and the folder's 40 B"
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find(mapper::delete_note(Keep::Copy(52)).as_str())
                .is_ok()
        );
        drop(ui);
        let task = handler.update(Message::Request(Request::Submit));
        let messages = drain(task).await.0;
        assert!(
            matches!(
                messages.as_slice(),
                [Message::DeleteKeepingCopy(request)]
                    if request.storage_id == 1
                        && request.folder == "/"
                        && request.names == ["Trip", "notes.txt"]
            ),
            "{messages:?}"
        );
        assert!(handler.editor.is_none());
    }

    #[tokio::test]
    async fn over_the_limit_or_of_an_unknown_size_the_delete_is_for_good_and_says_so() {
        let mut handler = deleting(true, 40).await;
        ask_to_delete(&mut handler, &["big.mov", "notes.txt"]).await;
        assert_eq!(keep_of(&handler), Some(Keep::TooMuch(3_000_000_012)));
        let note = mapper::delete_note(Keep::TooMuch(3_000_000_012));
        assert!(note.contains("for good"), "{note}");
        assert!(
            iced_test::simulator(handler.view())
                .find(note.as_str())
                .is_ok()
        );
        let messages = drain(handler.update(Message::Request(Request::Submit)))
            .await
            .0;
        assert!(
            messages
                .iter()
                .all(|m| !matches!(m, Message::DeleteKeepingCopy(_))),
            "the pane deletes it, as before"
        );

        // A size the phone doesn't give ("4 GB or more") can't be promised one.
        let mut huge = deleting(true, 40).await;
        let _ = huge.update(listed("/", &[sized("film.mkv", u64::from(u32::MAX))]));
        ask_to_delete(&mut huge, &["film.mkv"]).await;
        assert_eq!(keep_of(&huge), Some(Keep::Unknown));

        // Nor a folder holding a file the phone gives no size for.
        let fake = FakeExplorer {
            folder_sizes: vec![("/Trip".into(), 40)],
            partly_sized: vec!["/Trip".into()],
            ..FakeExplorer::new(Ok(vec![]))
        };
        let mut partly = Handler::new(Arc::new(fake));
        partly.set_keep_deleted(true);
        let _ = partly.open(storage(), "/".into());
        let _ = partly.update(listed("/", &[folder("Trip")]));
        ask_to_delete(&mut partly, &["Trip"]).await;
        assert_eq!(keep_of(&partly), Some(Keep::Unknown));

        // A folder whose count fails can't be promised a copy.
        let mut unknown = deleting(true, 40).await;
        let _ = unknown.update(listed("/", &[folder("Elsewhere")]));
        ask_to_delete(&mut unknown, &["Elsewhere"]).await;
        assert_eq!(keep_of(&unknown), Some(Keep::Unknown));
    }

    #[tokio::test]
    async fn while_counting_the_question_waits_and_without_the_setting_it_is_as_before() {
        let mut counting = deleting(true, 40).await;
        let _ = counting.update(Message::Request(Request::Select("Trip".into())));
        let _ = counting.update(Message::Request(Request::Delete));
        assert_eq!(keep_of(&counting), Some(Keep::Counting));
        let mut ui = iced_test::simulator(counting.view());
        assert!(
            ui.find(mapper::delete_note(Keep::Counting).as_str())
                .is_ok()
        );
        drop(ui);
        assert!(
            drain(counting.update(Message::Request(Request::Submit)))
                .await
                .0
                .is_empty()
        );
        assert_eq!(keep_of(&counting), Some(Keep::Counting), "not answered yet");
        // A count for another question is not this one's.
        let _ = counting.update(Message::DeleteCounted {
            names: vec!["notes.txt".into()],
            bytes: Some(1),
        });
        assert_eq!(keep_of(&counting), Some(Keep::Counting));

        // The Mac uses the Trash anyway: its delete is as it was.
        let mut on_mac = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        on_mac.set_keep_deleted(true);
        let _ = on_mac.open(mac(), "/Users/me".into());
        let _ = on_mac.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("old.txt")]),
        });
        let _ = on_mac.update(Message::Request(Request::Select("old.txt".into())));
        let _ = on_mac.update(Message::Request(Request::Delete));
        assert!(
            on_mac.editor.as_ref().is_none_or(|editor| editor.busy),
            "no question and no count: straight to the Trash"
        );

        let mut off = deleting(false, 40).await;
        ask_to_delete(&mut off, &["notes.txt"]).await;
        assert_eq!(keep_of(&off), Some(Keep::No));
        assert!(
            iced_test::simulator(off.view())
                .find("This deletes it from your phone for good. It can't be undone.")
                .is_ok()
        );
    }

    #[test]
    fn the_one_file_selected_is_found_with_one_lookup_in_a_big_folder() {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let mut entries: Vec<dto::Entry> = (0..5_000)
            .map(|i| file(&format!("IMG_{i:05}.jpg")))
            .collect();
        entries.push(folder("Camera"));
        let _ = handler.update(listed("/", &entries));
        assert_eq!(handler.single_file(), None, "nothing selected");
        let _ = handler.update(Message::Request(Request::Select("IMG_04999.jpg".into())));

        let started = std::time::Instant::now();
        for _ in 0..1_000 {
            assert_eq!(handler.single_file().as_deref(), Some("IMG_04999.jpg"));
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "{:?}",
            started.elapsed()
        );

        let _ = handler.update(Message::Request(Request::Select("Camera".into())));
        assert_eq!(handler.single_file(), None, "a folder isn't a file");
    }

    #[test]
    fn a_pane_opened_again_on_another_phone_or_storage_shows_everything() {
        let mut handler = holiday();
        filter(&mut handler, "beach");

        handler.close();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("notes.txt"), file("a.jpg")]));

        assert_eq!(handler.filter, "");
        assert_eq!(shown(&handler).len(), 2);
    }

    #[test]
    fn a_filter_that_leaves_nothing_says_so() {
        let mut handler = holiday();

        filter(&mut handler, "xyz");

        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("Nothing here has “xyz” in its name").is_ok());
            assert!(ui.find("0 of 5 shown").is_ok());
            assert!(ui.find("This folder is empty").is_err());
        }
        assert!(
            handler
                .update(Message::Request(Request::FocusFilter))
                .units()
                > 0
        );
    }

    #[test]
    fn thumbnails_are_asked_only_for_the_tiles_the_filter_shows() {
        let mut handler = folder_of(300, dto::ViewMode::Grid, 0.0);
        handler.viewport = (0.0, 600.0, 800.0);
        handler.thumbnails.clear();

        filter(&mut handler, "IMG_002");

        assert!(!handler.thumbnails.is_empty());
        assert!(
            handler
                .thumbnails
                .keys()
                .all(|name| name.starts_with("IMG_002")),
            "{:?}",
            handler.thumbnails.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn filtering_twenty_thousand_names_is_quick_and_builds_only_rows_near_the_screen() {
        let mut handler = folder_of(20_000, dto::ViewMode::List, 0.0);

        let started = std::time::Instant::now();
        filter(&mut handler, "IMG_1");
        let took = started.elapsed();
        eprintln!("a keystroke over 20,000 names: {took:?}");

        assert_eq!(handler.rows.len(), 10_000, "IMG_1xxxx");
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
        let mut ui = iced_test::simulator(handler.view());
        let built = (10_000..20_000)
            .filter(|i| ui.find(format!("IMG_{i:05}.jpg")).is_ok())
            .count();
        assert!(built < 100, "built {built} rows");
    }

    #[test]
    fn a_long_list_builds_only_the_rows_near_the_screen() {
        for density in DENSITIES {
            let handler = dense_folder_of(5_000, dto::ViewMode::List, density, 0.0);

            let built = rows_built(&handler, 5_000);

            // What fits on the screen, and a row each side: never the folder.
            let fit = (window_size().1 / row_style(density).height).ceil() as usize;
            assert!(built.contains(&0), "the first row is built");
            assert!(
                built.len() <= fit + 2,
                "{density:?}: built {} of 5,000 rows",
                built.len()
            );
            assert!(!built.contains(&4_999));
        }
    }

    #[test]
    fn scrolling_moves_the_window_with_it() {
        for density in DENSITIES {
            let height = row_style(density).height;
            let handler = dense_folder_of(5_000, dto::ViewMode::List, density, 100.0 * height);

            let built = rows_built(&handler, 5_000);

            assert!(
                built.contains(&100),
                "{density:?}: the row at the top of the screen"
            );
            assert!(built.contains(&110), "{density:?}: a row on screen");
            assert!(!built.contains(&0));
            assert!(!built.contains(&4_999));
        }
    }

    /// Where `name`'s text sits below the top of the list, in the layout.
    fn text_top(handler: &Handler, name: &str) -> f32 {
        let mut ui = iced_test::simulator(handler.view());
        let list = ui.find(handler.list.clone()).unwrap().bounds();
        let text = ui.find(name.to_owned()).unwrap();
        text.bounds().y - list.y
    }

    #[test]
    fn spacers_keep_every_row_exactly_where_a_full_list_would_put_it() {
        for density in DENSITIES {
            let height = row_style(density).height;
            let top = dense_folder_of(5_000, dto::ViewMode::List, density, 0.0);
            let within_row = text_top(&top, "IMG_00000.jpg");

            let scrolled = dense_folder_of(5_000, dto::ViewMode::List, density, 100.0 * height);

            // Row 100 is 100 rows down whatever the window cut away above it.
            let at = text_top(&scrolled, "IMG_00100.jpg");
            assert!(
                (at - (100.0 * height + within_row)).abs() < 0.5,
                "{density:?}: row 100 is {at} px down"
            );
        }
    }

    #[test]
    fn every_other_row_is_shaded_counting_from_the_top_of_the_folder() {
        // Rows that all read the same, so only the shading tells two scroll
        // positions apart. Counted from the top of what is built, the top row
        // would look the same however far down the list is.
        let dir = tempfile::tempdir().unwrap();
        let hash = dir.path().join("rows");
        let looks = |rows_down: usize| {
            let offset = rows_down as f32 * row_style(dto::Density::Standard).height;
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let same: Vec<_> = (0..5_000).map(|_| file("same.jpg")).collect();
            let _ = handler.update(listed("/", &same));
            handler.viewport.0 = offset;
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(600.0, 400.0),
                handler.view(),
            );
            let list = ui.find(handler.list.clone()).unwrap().bounds();
            ui.point_at(list.center());
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -offset },
            })]);
            ui.snapshot(&iced::Theme::Light)
                .unwrap()
                .matches_hash(&hash)
                .unwrap()
        };

        assert!(looks(100), "writes the first look");
        assert!(looks(102), "two rows further, shaded the same way");
        assert!(!looks(101), "one row further, the other way round");
    }

    #[test]
    fn a_new_row_size_keeps_the_same_row_at_the_top() {
        let standard = row_style(dto::Density::Standard).height;
        let compact = row_style(dto::Density::Compact).height;
        let mut handler = folder_of(5_000, dto::ViewMode::List, 100.0 * standard);

        let scrolled = handler.set_density(dto::Density::Compact);

        assert_eq!(handler.density(), dto::Density::Compact);
        assert!((handler.viewport.0 - 100.0 * compact).abs() < 0.01);
        assert!(scrolled.units() > 0, "the list is scrolled there");
        assert!(rows_built(&handler, 5_000).contains(&100));
        assert_eq!(
            handler.set_density(dto::Density::Compact).units(),
            0,
            "the same size again changes nothing"
        );

        let mut grid = folder_of(5_000, dto::ViewMode::Grid, 900.0);
        assert_eq!(grid.set_density(dto::Density::Large).units(), 0);
        assert_eq!(grid.viewport.0, 900.0, "tiles don't change size");
    }

    #[test]
    fn compact_rows_are_finders_and_each_size_is_roomier_than_the_last() {
        let spacing = |density| {
            let handler = dense_folder_of(50, dto::ViewMode::List, density, 0.0);
            text_top(&handler, "IMG_00001.jpg") - text_top(&handler, "IMG_00000.jpg")
        };
        let [compact, standard, large] = DENSITIES.map(spacing);

        assert!(
            (compact - 22.0).abs() < 0.5,
            "compact rows are {compact} px"
        );
        assert!(
            compact < standard && standard < large,
            "{compact} {standard} {large}"
        );
        assert!(
            standard < 32.0,
            "even the usual rows are tighter than before ({standard} px)"
        );
    }

    #[test]
    fn folder_and_file_names_start_at_the_same_place_at_every_size() {
        for density in DENSITIES {
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let _ = handler.set_density(density);
            let _ = handler.update(listed("/", &[folder("DCIM"), file("a.jpg")]));
            let mut ui = iced_test::simulator(handler.view());

            let folder_at = ui.find("DCIM").unwrap().bounds();
            let file_at = ui.find("a.jpg").unwrap().bounds();
            assert_eq!(folder_at.x, file_at.x, "{density:?}");
            assert!(
                (file_at.y - folder_at.y - row_style(density).height).abs() < 0.5,
                "{density:?}: one row apart"
            );
        }
    }

    #[test]
    fn row_text_stays_readable_on_the_selection_and_is_grey_elsewhere() {
        let theme = iced::Theme::Dark;
        let look = |selected, dim, target| Look {
            selected,
            dim,
            target,
            pointer: Pointer::Idle,
        };
        let colour = |look: Look, detail| row_text(look, detail)(&theme).color;
        let grey = text::secondary(&theme).color;

        assert_eq!(colour(look(false, false, false), false), None, "a name");
        assert_eq!(
            colour(look(false, false, false), true),
            grey,
            "a size or date"
        );
        assert_eq!(colour(look(false, true, false), false), grey, "a cut name");
        for detail in [false, true] {
            assert_eq!(colour(look(true, false, false), detail), None, "selected");
            assert_eq!(
                colour(look(true, true, false), detail),
                None,
                "selected and cut"
            );
            assert_eq!(
                colour(look(false, false, true), detail),
                None,
                "a drop target"
            );
        }
    }

    #[test]
    fn a_drop_target_beats_the_selection_which_beats_the_stripe() {
        let theme = iced::Theme::Light;
        let palette = theme.extended_palette();
        let look = |selected, target| Look {
            selected,
            dim: false,
            target,
            pointer: Pointer::Idle,
        };
        let fill = |look: Look, striped: bool| {
            row_fill(look, striped, Joined::default())(&theme)
                .background
                .map(|background| match background {
                    iced::Background::Color(colour) => colour,
                    _ => Color::TRANSPARENT,
                })
        };

        assert_eq!(
            fill(look(true, true), true),
            Some(palette.success.base.color)
        );
        assert_eq!(
            fill(look(true, false), true),
            Some(palette.primary.base.color)
        );
        assert_eq!(fill(look(false, false), true), Some(stripe(palette)));
        assert_eq!(fill(look(false, false), false), None);
        assert_eq!(
            row_fill(look(true, false), false, Joined::default())(&theme).text_color,
            Some(palette.primary.base.text),
            "the selection says what colour its text is"
        );
        assert_ne!(
            stripe(iced::Theme::Dark.extended_palette()),
            stripe(palette),
            "lighter in dark mode, darker in light mode"
        );
    }

    #[test]
    fn selected_rows_next_to_each_other_are_one_block() {
        let theme = iced::Theme::Light;
        let look = |selected, target| Look {
            selected,
            dim: false,
            target,
            pointer: Pointer::Idle,
        };
        let radius = |look: Look, above, below| {
            let r = row_fill(look, false, Joined { above, below })(&theme)
                .border
                .radius;
            (r.top_left, r.top_right, r.bottom_right, r.bottom_left)
        };

        assert_eq!(
            radius(look(true, false), false, false),
            (5.0, 5.0, 5.0, 5.0)
        );
        assert_eq!(radius(look(true, false), true, false), (0.0, 0.0, 5.0, 5.0));
        assert_eq!(radius(look(true, false), false, true), (5.0, 5.0, 0.0, 0.0));
        assert_eq!(
            radius(look(true, true), true, true),
            (5.0, 5.0, 5.0, 5.0),
            "a drop target stands alone"
        );
    }

    #[test]
    fn a_run_of_selected_rows_is_joined_and_nothing_else_is() {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"];
        let entries: Vec<_> = names.iter().map(|name| file(name)).collect();
        let _ = handler.update(listed("/", &entries));
        let _ = handler.update(Message::Request(Request::Select("b.jpg".into())));
        handler.set_modifiers(Modifiers::SHIFT);
        let _ = handler.update(Message::Request(Request::Select("c.jpg".into())));
        handler.set_modifiers(Modifiers::COMMAND);
        let _ = handler.update(Message::Request(Request::Select("e.jpg".into())));

        let joins: Vec<(bool, bool)> = (0..names.len())
            .map(|index| {
                let joined = handler.joined(index, handler.look(&handler.rows[index]));
                (joined.above, joined.below)
            })
            .collect();
        assert_eq!(
            joins,
            [
                (false, false),
                (false, true),
                (true, false),
                (false, false),
                (false, false)
            ]
        );
    }

    #[test]
    fn a_long_grid_builds_only_the_rows_of_tiles_near_the_screen() {
        let handler = folder_of(5_000, dto::ViewMode::Grid, 0.0);

        let built = rows_built(&handler, 5_000);

        assert!(built.contains(&0));
        assert!(built.len() < 120, "built {} of 5,000 tiles", built.len());
        assert!(!built.contains(&4_999));
    }

    #[test]
    fn bigger_tiles_build_fewer_and_smaller_ones_more_never_the_whole_folder() {
        let sized = |size: dto::TileSize| {
            let mut handler = folder_of(5_000, dto::ViewMode::Grid, 0.0);
            let _ = handler.set_tile_size(size);
            rows_built(&handler, 300).len()
        };
        let usual = sized(dto::TileSize::default());
        let biggest = sized(dto::TileSize::default().bigger().bigger());
        let smallest = sized(dto::TileSize::default().smaller().smaller());

        assert!(
            biggest < usual && usual < smallest,
            "{biggest} {usual} {smallest}"
        );
        assert!(
            smallest < 300,
            "built {smallest} tiles: only those near the screen"
        );
    }

    #[test]
    fn tiles_are_drawn_at_the_panes_size() {
        let mut handler = folder_of(20, dto::ViewMode::Grid, 0.0);
        let biggest = dto::TileSize::default().bigger().bigger();
        let _ = handler.set_tile_size(biggest);
        let mut ui = iced_test::simulator(handler.view());
        let first = ui.find("IMG_00000.jpg").unwrap().bounds().center_x();
        let second = ui.find("IMG_00001.jpg").unwrap().bounds().center_x();

        assert!(
            ((second - first) - grid::Tile::of(biggest).pitch()).abs() < 0.5,
            "{} apart",
            second - first
        );
    }

    #[test]
    fn a_new_tile_size_keeps_the_first_tile_in_view() {
        let mut handler = folder_of(5_000, dto::ViewMode::Grid, 0.0);
        handler.viewport.2 = 560.0;
        // Row 10 of 4 columns at the top: tile 40.
        handler.viewport.0 = 10.0 * grid::Tile::default().row_height();

        let biggest = dto::TileSize::default().bigger().bigger();
        let task = handler.set_tile_size(biggest);

        assert_eq!(handler.tile().width, 192.0);
        // Two columns of 192: tile 40 starts row 20.
        assert_eq!(handler.viewport.0, 20.0 * handler.tile().row_height());
        assert!(task.units() > 0, "scrolled there, and thumbnails asked for");
        assert_eq!(
            handler.set_tile_size(biggest).units(),
            0,
            "the same size: nothing"
        );
        let _ = handler.view();

        // Smaller tiles bring more into view: their thumbnails are asked for.
        let mut grid = folder_of(300, dto::ViewMode::Grid, 0.0);
        grid.viewport = (0.0, 600.0, 800.0);
        grid.thumbnails.clear();
        let _ = grid.set_tile_size(dto::TileSize::default().smaller().smaller());
        assert!(
            !grid.thumbnails.is_empty(),
            "thumbnails asked for what is in view"
        );

        let mut list = folder_of(100, dto::ViewMode::List, 50.0);
        let _ = list.set_tile_size(dto::TileSize::default().bigger());
        assert_eq!(list.tile().width, 160.0, "kept for when it shows a grid");
        assert_eq!(list.viewport.0, 50.0, "a list does not move");
    }

    #[test]
    fn grid_tiles_stay_where_a_full_grid_would_put_them() {
        let top = folder_of(5_000, dto::ViewMode::Grid, 0.0);
        let within_tile = text_top(&top, "IMG_00000.jpg");
        let columns = grid::columns(iced::Size::from(window_size()).width, grid::Tile::default());

        // Scroll to the start of a row of tiles well down the folder.
        let row = 40;
        let scrolled = folder_of(
            5_000,
            dto::ViewMode::Grid,
            row as f32 * grid::Tile::default().row_height(),
        );
        let tile = row * columns;
        let at = text_top(&scrolled, &format!("IMG_{tile:05}.jpg"));

        assert!(
            (at - (row as f32 * grid::Tile::default().row_height() + within_tile)).abs() < 0.5,
            "tile {tile} is {at} px down"
        );
    }

    /// The simulator's default window, which the pane fills.
    fn window_size() -> (f32, f32) {
        (1024.0, 768.0)
    }

    #[test]
    fn a_short_folder_is_built_in_full() {
        let list = folder_of(12, dto::ViewMode::List, 0.0);
        assert_eq!(rows_built(&list, 12).len(), 12);

        let grid = folder_of(12, dto::ViewMode::Grid, 0.0);
        assert_eq!(rows_built(&grid, 12).len(), 12);
    }

    #[test]
    fn the_bottom_of_a_long_list_is_built_too() {
        // 5,000 rows, scrolled to the very end.
        let handler = folder_of(
            5_000,
            dto::ViewMode::List,
            5_000.0 * row_style(dto::Density::Standard).height,
        );

        let built = rows_built(&handler, 5_000);

        assert!(built.contains(&4_999), "the last row");
        assert!(!built.contains(&0));
    }

    #[test]
    fn clicking_a_row_of_a_long_list_selects_it() {
        let mut handler = folder_of(5_000, dto::ViewMode::List, 0.0);
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("IMG_00005.jpg").unwrap();
            ui.into_messages().collect()
        };

        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(selected(&handler), ["IMG_00005.jpg"]);
    }

    #[test]
    fn clicking_a_tile_of_a_long_grid_selects_it() {
        let mut handler = folder_of(5_000, dto::ViewMode::Grid, 0.0);
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("IMG_00003.jpg").unwrap();
            ui.into_messages().collect()
        };

        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(selected(&handler), ["IMG_00003.jpg"]);
    }
    async fn pieces_of(fake: &Arc<FakeExplorer>) -> Vec<Piece> {
        list_updates(
            Arc::clone(fake) as Arc<dyn Service>,
            1,
            "/DCIM".into(),
            false,
            dto::Cancel::new(),
            true,
        )
        .collect()
        .await
    }

    fn partial(piece: &Piece) -> (Vec<&str>, usize, usize) {
        match piece {
            Piece::Partial {
                entries,
                loaded,
                total,
            } => (
                entries.iter().map(|e| e.name.as_str()).collect(),
                *loaded,
                *total,
            ),
            Piece::Finished(_) => panic!("expected a partial piece"),
        }
    }

    fn finished(piece: &Piece) -> Result<Vec<&str>, &str> {
        match piece {
            Piece::Finished(Ok(entries)) => Ok(entries.iter().map(|e| e.name.as_str()).collect()),
            Piece::Finished(Err(error)) => Err(error.title.as_str()),
            Piece::Partial { .. } => panic!("expected the end"),
        }
    }

    #[tokio::test]
    async fn a_folder_handed_over_whole_is_one_finished_piece_with_the_options_passed_on() {
        let fake = Arc::new(FakeExplorer::new(Ok(vec![folder("DCIM"), file("a.jpg")])));

        let pieces = list_updates(
            Arc::clone(&fake) as Arc<dyn Service>,
            3,
            "/x".into(),
            true,
            dto::Cancel::new(),
            true,
        )
        .collect::<Vec<_>>()
        .await;

        assert_eq!(pieces.len(), 1);
        assert_eq!(finished(&pieces[0]), Ok(vec!["DCIM", "a.jpg"]));
        assert_eq!(*fake.calls.lock().unwrap(), [(3, "/x".to_owned(), true)]);
    }

    #[tokio::test]
    async fn a_failed_listing_is_one_finished_error() {
        let fake = Arc::new(FakeExplorer::new(Err(dto::Error::Disconnected)));

        let pieces = pieces_of(&fake).await;

        assert_eq!(pieces.len(), 1);
        let Piece::Finished(Err(error)) = &pieces[0] else {
            panic!("expected an error");
        };
        assert!(error.device_lost);
    }

    #[tokio::test]
    async fn a_listing_that_cannot_start_is_a_finished_error() {
        let fake = Arc::new(FakeExplorer {
            open_fails: Some(dto::Error::PathNotFound("/DCIM".into())),
            ..FakeExplorer::new(Ok(vec![]))
        });

        let pieces = pieces_of(&fake).await;

        assert_eq!(finished(&pieces[0]), Err("This folder is gone"));
        assert_eq!(pieces.len(), 1);
    }

    #[tokio::test]
    async fn pieces_carry_everything_received_so_far() {
        let fake = Arc::new(FakeExplorer::arriving(vec![
            Ok(piece(&["a.jpg", "b.jpg"], 2, 5, false)),
            Ok(piece(&["c.jpg"], 3, 5, false)),
            Ok(piece(&["d.jpg", "e.jpg"], 5, 5, true)),
        ]));

        let pieces = pieces_of(&fake).await;

        assert_eq!(pieces.len(), 3);
        assert_eq!(partial(&pieces[0]), (vec!["a.jpg", "b.jpg"], 2, 5));
        assert_eq!(partial(&pieces[1]), (vec!["a.jpg", "b.jpg", "c.jpg"], 3, 5));
        assert_eq!(
            finished(&pieces[2]),
            Ok(vec!["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"])
        );
    }

    #[tokio::test]
    async fn a_restart_forgets_what_came_before() {
        let restart = dto::ListingUpdate {
            restart: true,
            total: 4,
            ..dto::ListingUpdate::default()
        };
        let fake = Arc::new(FakeExplorer::arriving(vec![
            Ok(piece(&["old1.jpg", "old2.jpg"], 2, 4, false)),
            Ok(restart),
            Ok(piece(&["new1.jpg"], 1, 4, false)),
            Ok(piece(&["new2.jpg"], 2, 4, true)),
        ]));

        let pieces = pieces_of(&fake).await;

        assert_eq!(partial(&pieces[1]), (vec![], 0, 4));
        assert_eq!(partial(&pieces[2]), (vec!["new1.jpg"], 1, 4));
        assert_eq!(finished(&pieces[3]), Ok(vec!["new1.jpg", "new2.jpg"]));
    }

    #[tokio::test]
    async fn an_error_part_way_ends_the_listing() {
        let fake = Arc::new(FakeExplorer::arriving(vec![
            Ok(piece(&["a.jpg"], 1, 3, false)),
            Err(dto::Error::Timeout),
            Ok(piece(&["never.jpg"], 3, 3, true)),
        ]));

        let pieces = pieces_of(&fake).await;

        assert_eq!(pieces.len(), 2, "nothing after the error");
        assert_eq!(finished(&pieces[1]), Err("Your phone stopped responding"));
    }

    #[tokio::test]
    async fn a_cancelled_listing_ends_without_a_word() {
        // The listing stops handing out pieces, as it does once cancelled.
        let fake = Arc::new(FakeExplorer::arriving(vec![Ok(piece(
            &["a.jpg"],
            1,
            3,
            false,
        ))]));

        let pieces = pieces_of(&fake).await;

        assert_eq!(pieces.len(), 1);
        assert!(
            matches!(pieces[0], Piece::Partial { .. }),
            "no Finished piece"
        );
    }

    #[test]
    fn pieces_become_the_messages_the_pane_understands() {
        let partial = Piece::Partial {
            entries: vec![file("a.jpg")],
            loaded: 1,
            total: 9,
        }
        .into_message(4, "/DCIM");
        assert!(matches!(
            partial,
            Message::Loading { volume_id: 4, ref path, loaded: 1, total: 9, ref entries }
                if path == "/DCIM" && entries.len() == 1
        ));

        let done = Piece::Finished(Ok(vec![])).into_message(4, "/DCIM");
        assert!(
            matches!(done, Message::Listed { volume_id: 4, ref path, result: Ok(_) } if path == "/DCIM")
        );
    }

    fn loading(path: &str, names: &[&str], loaded: usize, total: usize) -> Message {
        Message::Loading {
            volume_id: 1,
            path: path.into(),
            entries: names.iter().map(|name| file(name)).collect(),
            loaded,
            total,
        }
    }

    /// A handler that has started loading `/DCIM`.
    fn loading_handler() -> Handler {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[folder("DCIM")]));
        let _ = handler.update(Message::Request(Request::GoTo("/DCIM".into())));
        assert!(handler.is_loading());
        handler
    }

    #[test]
    fn what_has_arrived_is_shown_with_how_far_along_it_is() {
        let mut handler = loading_handler();

        let _ = handler.update(loading("/DCIM", &["b.jpg", "a.jpg"], 2, 9));

        assert_eq!(names(&handler), ["a.jpg", "b.jpg"], "sorted as usual");
        assert!(handler.is_loading(), "still arriving");
        assert_eq!(handler.progress, Some((2, 9)));
    }

    #[test]
    fn each_piece_replaces_the_last() {
        let mut handler = loading_handler();
        let _ = handler.update(loading("/DCIM", &["a.jpg"], 1, 3));

        let _ = handler.update(loading("/DCIM", &["a.jpg", "b.jpg", "c.jpg"], 3, 3));

        assert_eq!(names(&handler), ["a.jpg", "b.jpg", "c.jpg"]);
    }

    #[test]
    fn pieces_for_another_folder_or_after_the_end_are_ignored() {
        let mut handler = loading_handler();

        let _ = handler.update(loading("/Elsewhere", &["x.jpg"], 1, 1));
        assert!(names(&handler).is_empty());

        let _ = handler.update(listed("/DCIM", &[file("a.jpg")]));
        let _ = handler.update(loading("/DCIM", &["late.jpg"], 1, 9));
        assert_eq!(
            names(&handler),
            ["a.jpg"],
            "a late piece can't undo the end"
        );
        assert_eq!(handler.progress, None);
    }

    #[test]
    fn the_finished_folder_ends_the_progress() {
        let mut handler = loading_handler();
        let _ = handler.update(loading("/DCIM", &["a.jpg"], 1, 3));
        assert!(handler.listing.is_some());

        let _ = handler.update(listed("/DCIM", &[file("a.jpg"), file("b.jpg")]));

        assert!(!handler.is_loading());
        assert_eq!(handler.progress, None);
        assert!(handler.listing.is_none());
    }

    #[test]
    fn a_failure_after_some_pieces_shows_the_error_and_no_files() {
        let mut handler = loading_handler();
        let _ = handler.update(loading("/DCIM", &["a.jpg"], 1, 3));

        let _ = handler.update(Message::Listed {
            volume_id: 1,
            path: "/DCIM".into(),
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });

        assert!(names(&handler).is_empty());
        assert!(handler.error.is_some());
        assert!(!handler.is_loading());
    }

    #[test]
    fn starting_another_listing_stops_the_one_in_progress() {
        let mut handler = loading_handler();
        let first = handler.listing.clone().unwrap();
        assert!(!first.is_cancelled());

        let _ = handler.update(Message::Request(Request::Refresh));

        assert!(first.is_cancelled(), "the old listing is told to stop");
        let second = handler.listing.clone().unwrap();
        assert!(!second.is_cancelled());
    }

    #[test]
    fn leaving_the_folder_or_closing_stops_the_listing() {
        let mut handler = loading_handler();
        let first = handler.listing.clone().unwrap();
        let _ = handler.update(Message::Request(Request::Up));
        assert!(first.is_cancelled());

        let second = handler.listing.clone().unwrap();
        handler.close();

        assert!(second.is_cancelled());
        assert!(handler.listing.is_none());
        assert_eq!(handler.progress, None);
    }

    #[test]
    fn loading_says_how_far_along_a_big_folder_is() {
        assert_eq!(loading_text(None), "Loading…");
        assert_eq!(loading_text(Some((0, 0))), "Loading…");
        assert_eq!(
            loading_text(Some((1_200, 3_400))),
            "Loading 1,200 of 3,400…"
        );
        assert_eq!(
            loading_text(Some((9, 5))),
            "Loading 5 of 5…",
            "never past the total"
        );
    }

    #[test]
    fn progress_is_shown_in_the_pane_with_and_without_rows() {
        let mut handler = loading_handler();
        let _ = handler.update(loading("/DCIM", &[], 0, 3_400));
        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("Loading 0 of 3,400…").is_ok());
        }

        let _ = handler.update(loading("/DCIM", &["a.jpg"], 1_200, 3_400));
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("a.jpg").is_ok());
        assert!(ui.find("Loading 1,200 of 3,400…").is_ok(), "in the footer");
    }

    #[test]
    fn thumbnails_wait_until_the_folder_has_arrived() {
        let mut handler = loading_handler();
        let _ = handler.set_view(dto::ViewMode::Grid);

        let _ = handler.update(loading("/DCIM", &["a.jpg", "b.jpg"], 2, 9));
        assert!(handler.thumbnails.is_empty(), "the phone is busy listing");

        let _ = handler.update(listed("/DCIM", &[file("a.jpg"), file("b.jpg")]));
        assert_eq!(handler.thumbnails.len(), 2, "now they are asked for");
    }

    fn thumbnail(name: &str) -> Message {
        Message::Thumbnail {
            volume_id: 1,
            path: "/DCIM".into(),
            name: name.into(),
            picture: Some(Picture::from_rgba(1, 1, vec![0; 4])),
        }
    }

    #[test]
    fn far_off_thumbnails_are_let_go_past_the_cache_limit() {
        let mut handler = loading_handler();
        let _ = handler.set_view(dto::ViewMode::Grid);
        let many: Vec<_> = (0..3_000)
            .map(|i| file(&format!("IMG_{i:05}.jpg")))
            .collect();
        let _ = handler.update(listed("/DCIM", &many));
        handler.thumbnails.clear();
        // The screen is at the top; thumbnails pile up all the way down.
        for i in 0..(THUMBNAIL_CACHE + 200) {
            let _ = handler.update(thumbnail(&format!("IMG_{i:05}.jpg")));
        }

        assert!(
            handler.thumbnails.len() <= THUMBNAIL_CACHE,
            "{}",
            handler.thumbnails.len()
        );
        assert!(
            matches!(
                handler.thumbnails.get("IMG_00000.jpg"),
                Some(Thumb::Ready(_))
            ),
            "what is on screen stays"
        );
        assert!(
            !handler
                .thumbnails
                .contains_key(&format!("IMG_{:05}.jpg", THUMBNAIL_CACHE + 150)),
            "the far end went"
        );
    }

    #[test]
    fn thumbnails_being_fetched_are_never_dropped_by_the_limit() {
        let mut handler = loading_handler();
        let _ = handler.set_view(dto::ViewMode::Grid);
        let many: Vec<_> = (0..3_000)
            .map(|i| file(&format!("IMG_{i:05}.jpg")))
            .collect();
        let _ = handler.update(listed("/DCIM", &many));
        handler.thumbnails.clear();
        handler
            .thumbnails
            .insert("IMG_02999.jpg".into(), Thumb::Loading);

        for i in 0..(THUMBNAIL_CACHE + 50) {
            let _ = handler.update(thumbnail(&format!("IMG_{i:05}.jpg")));
        }

        assert!(matches!(
            handler.thumbnails.get("IMG_02999.jpg"),
            Some(Thumb::Loading)
        ));
    }

    #[test]
    fn a_small_cache_is_left_alone() {
        let mut handler = loading_handler();
        let _ = handler.set_view(dto::ViewMode::Grid);
        let few: Vec<_> = (0..50).map(|i| file(&format!("IMG_{i:05}.jpg"))).collect();
        let _ = handler.update(listed("/DCIM", &few));
        handler.thumbnails.clear();

        for i in 0..50 {
            let _ = handler.update(thumbnail(&format!("IMG_{i:05}.jpg")));
        }

        assert_eq!(handler.thumbnails.len(), 50);
    }

    #[test]
    fn the_scroll_area_spans_the_pane_so_the_scrollbar_sits_at_its_edge() {
        for view in [dto::ViewMode::List, dto::ViewMode::Grid] {
            let handler = folder_of(5_000, view, 0.0);
            // 500 px wide: three tile columns (about 410 px) leave room to spare.
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(500.0, 800.0),
                handler.view(),
            );

            let scroll = ui.find(handler.list.clone()).unwrap().bounds();

            // The pane is 500 wide less its 12 px padding either side.
            assert!(scroll.width > 470.0, "{view:?}: {} px wide", scroll.width);
        }
    }

    /// The best of several builds and layouts of the pane, in seconds. Seven,
    /// because the other tests run alongside and a slow moment in all of
    /// three tries once made a healthy pane look slow.
    fn build_seconds(handler: &Handler) -> f64 {
        (0..7)
            .map(|_| {
                let started = std::time::Instant::now();
                drop(iced_test::simulator(handler.view()));
                started.elapsed().as_secs_f64()
            })
            .fold(f64::MAX, f64::min)
    }

    #[test]
    fn a_name_that_fits_is_shown_whole() {
        // Cut, these two would both read "sh…rt.txt".
        let dir = tempfile::tempdir().unwrap();
        let hash = dir.path().join("short");
        let looks = |name: &str| {
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let _ = handler.update(listed("/", &[file(name)]));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(700.0, 300.0),
                handler.view(),
            );
            ui.snapshot(&iced::Theme::Light)
                .unwrap()
                .matches_hash(&hash)
                .unwrap()
        };

        assert!(looks("short.txt"), "writes the first look");
        assert!(!looks("shart.txt"), "whole names: the middle letter shows");
    }

    #[test]
    fn cutting_long_names_costs_only_the_rows_on_screen() {
        drop(iced_test::simulator(
            folder_of(10, dto::ViewMode::List, 0.0).view(),
        ));
        let long = |n: usize| {
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let entries: Vec<_> = (0..n)
                .map(|i| {
                    file(&format!(
                        "PXL_20260614_{i:06}_PORTRAIT_NIGHT_SIGHT_ASTROPHOTOGRAPHY_ORIGINAL.jpg"
                    ))
                })
                .collect();
            let _ = handler.update(listed("/", &entries));
            handler
        };
        let small = build_seconds(&long(1_000));
        let large = build_seconds(&long(5_000));
        eprintln!("long names: 1,000 in {small:.3}s, 5,000 in {large:.3}s");

        assert!(
            large < small.max(0.020) * 5.0,
            "1,000 long names took {small:.3}s, 5,000 took {large:.3}s"
        );
    }

    #[test]
    fn a_cut_name_shows_whole_on_hover_and_a_short_one_has_nothing_to_show() {
        let dir = tempfile::tempdir().unwrap();
        let hovered_differs = |name: &str| {
            let hash = dir.path().join(format!("{}", name.len()));
            let mut handler = handler();
            let _ = handler.open(storage(), "/".into());
            let _ = handler.update(listed("/", &[file(name)]));
            let size = iced::Size::new(520.0, 300.0);
            let shot = |hover: bool| {
                let mut ui = iced_test::Simulator::with_size(
                    iced::Settings::default(),
                    size,
                    handler.view(),
                );
                if hover {
                    let at = ui.find(name).unwrap().bounds().center();
                    ui.point_at(at);
                    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                        position: at,
                    })]);
                }
                ui.snapshot(&iced::Theme::Light)
                    .unwrap()
                    .matches_hash(&hash)
                    .unwrap()
            };
            assert!(shot(false), "writes the plain look");
            !shot(true)
        };

        assert!(hovered_differs(
            "PXL_20260614_082133_PORTRAIT_NIGHT_SIGHT_ASTROPHOTOGRAPHY_ORIGINAL.jpg"
        ));
        assert!(!hovered_differs("a.jpg"), "short: no tooltip");
    }

    #[test]
    fn view_cost_does_not_grow_with_the_folder() {
        // Warm up: the first layout in a process loads fonts.
        drop(iced_test::simulator(
            folder_of(10, dto::ViewMode::List, 0.0).view(),
        ));

        for language in dto::Language::ALL {
            crate::i18n::with_language(language, || {
                for view in [dto::ViewMode::List, dto::ViewMode::Grid] {
                    let small = build_seconds(&folder_of(1_000, view, 0.0));
                    let large = build_seconds(&folder_of(20_000, view, 0.0));

                    // 20 times the files must not cost anything like 20 times as much
                    // (it did, before only the rows near the screen were built). The
                    // floor keeps a very fast small build from making the ratio silly.
                    assert!(
                        large < small.max(0.020) * 5.0,
                        "{language:?} {view:?}: 1,000 files took {small:.3}s, 20,000 took {large:.3}s"
                    );
                }
            });
        }
    }

    /// Runs the messages a task produces, which is how a test sees what
    /// `load` actually asks the service for.
    async fn run(task: Task<Message>) -> Vec<Message> {
        let Some(stream) = iced_test::runtime::task::into_stream(task) else {
            return Vec::new();
        };
        stream
            .filter_map(|action| async move {
                match action {
                    iced_test::runtime::Action::Output(message) => Some(message),
                    _ => None,
                }
            })
            .collect()
            .await
    }

    /// A pane on storage 1 at `/DCIM` and the fake behind it.
    fn pane_with_a_fake() -> (Handler, Arc<FakeExplorer>) {
        let fake = Arc::new(FakeExplorer::new(Ok(vec![file("a.jpg")])));
        let mut handler = Handler::new(Arc::clone(&fake) as Arc<dyn Service>);
        let _ = handler.open(storage(), "/DCIM".into());
        (handler, fake)
    }

    /// Whether the listing a pane action starts may use what was remembered.
    async fn may_use_memory(action: impl FnOnce(&mut Handler) -> Task<Message>) -> bool {
        let (mut handler, fake) = pane_with_a_fake();
        fake.allowed.lock().unwrap().clear();

        run(action(&mut handler)).await;

        let allowed = fake.allowed.lock().unwrap().clone();
        assert_eq!(allowed.len(), 1, "one listing was started: {allowed:?}");
        allowed[0]
    }

    #[tokio::test]
    async fn going_into_a_folder_may_use_what_was_remembered() {
        assert!(
            may_use_memory(|h| h.update(Message::Request(Request::GoTo("/DCIM/Camera".into()))))
                .await
        );
        assert!(may_use_memory(|h| h.update(Message::Request(Request::Up))).await);
        assert!(may_use_memory(|h| h.open(storage(), "/".into())).await);
    }

    #[tokio::test]
    async fn the_refresh_button_and_shortcut_read_the_phone_for_real() {
        assert!(!may_use_memory(|h| h.update(Message::Request(Request::Refresh))).await);
    }

    #[tokio::test]
    async fn a_change_the_phone_reported_reads_for_real() {
        assert!(!may_use_memory(|h| h.reload()).await);
    }

    #[tokio::test]
    async fn after_a_transfer_the_folder_is_read_for_real() {
        assert!(!may_use_memory(|h| h.refresh_and_select(Some("new.jpg".into()))).await);
    }

    #[tokio::test]
    async fn showing_hidden_files_may_use_what_was_remembered() {
        assert!(may_use_memory(|h| h.set_show_hidden(true)).await);
        assert!(may_use_memory(|h| h.update(Message::Request(Request::SetShowHidden(true)))).await);
    }

    #[tokio::test]
    async fn the_refresh_after_an_edit_may_use_what_the_service_kept_in_step() {
        assert!(may_use_memory(|h| h.update(edited("/DCIM", Ok(Some("Trip".into()))))).await);
    }

    #[tokio::test]
    async fn the_pane_lists_a_folder_the_same_whether_or_not_memory_may_be_used() {
        // The fake ignores the flag; what matters is that both reach `Listed`.
        for allow in [true, false] {
            let (mut handler, _fake) = pane_with_a_fake();
            let task = if allow {
                handler.set_show_hidden(true)
            } else {
                handler.reload()
            };

            for message in run(task).await {
                let _ = handler.update(message);
            }

            assert_eq!(names(&handler), ["a.jpg"], "allow = {allow}");
            assert!(!handler.is_loading());
        }
    }

    #[test]
    fn applying_a_big_listing_to_the_pane_is_quick() {
        // Sorting and formatting every entry happens on the UI thread each time
        // a piece of a folder arrives; it must stay a fraction of a frame per
        // thousand files.
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let entries: Vec<_> = (0..20_000u64)
            .map(|i| file(&format!("IMG_{:05}.jpg", (i * 7919) % 20_000)))
            .collect();

        let started = std::time::Instant::now();
        let _ = handler.update(listed("/", &entries));
        let took = started.elapsed();

        assert_eq!(handler.rows.len(), 20_000);
        // About 0.23 s in a debug build and 15 ms in a release one (almost all
        // of it sorting 20,000 shuffled names); the bound only catches a
        // change that makes it go quadratic.
        assert!(took < std::time::Duration::from_secs(3), "{took:?}");
    }

    // ---- pasting inside the phone ----

    fn cut_from(path: &str, names: &[&str]) -> Selection {
        Selection {
            volume_id: 1,
            path: path.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    fn pasted(path: &str, result: Result<PasteResponse, ErrorResponse>) -> Message {
        Message::Pasted {
            volume_id: 1,
            path: path.into(),
            result,
        }
    }

    fn arrived(names: &[&str]) -> PasteResponse {
        PasteResponse {
            select: names.iter().map(|n| (*n).to_owned()).collect(),
            notice: NoticeResponse {
                title: "Copied 1 item".into(),
                details: Vec::new(),
                problem: false,
            },
            undo: None,
        }
    }

    /// A phone pane showing `/` that pastes through `service`.
    fn pasting_pane(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[file("notes.txt"), folder("DCIM")]));
        handler
    }

    #[tokio::test]
    async fn a_paste_says_it_is_on_the_phone_only_on_the_phone() {
        for (volume, path, copying, moving) in [
            (
                storage(),
                "/",
                "Copying on the phone…",
                "Moving on the phone…",
            ),
            (mac(), "/Users/me", "Copying…", "Moving…"),
        ] {
            let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
            let _ = handler.open(volume, path.into());
            let _ = handler.update(Message::Listed {
                volume_id: if path == "/" { 1 } else { 0 },
                path: path.into(),
                result: Ok(vec![file("notes.txt")]),
            });

            for (mode, says) in [(dto::Paste::Copy, copying), (dto::Paste::Move, moving)] {
                handler.pasting = Some(mode);
                let mut ui = iced_test::simulator(handler.view());
                assert!(ui.find(says.to_owned()).is_ok(), "{path}: {says}");
            }
        }
    }

    #[tokio::test]
    async fn a_paste_asks_the_service_for_the_folder_shown_and_says_it_is_busy() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = pasting_pane(service.clone());

        let task = handler.paste(
            dto::Paste::Copy,
            vec![cut_from("/DCIM", &["a.jpg", "b.jpg"])],
        );

        assert!(handler.is_pasting());
        let _ = handler.view();
        let (messages, _) = crate::handler::testing::drain(task).await;
        assert_eq!(
            *service.pastes.lock().unwrap(),
            [dto::PasteRequest {
                mode: dto::Paste::Copy,
                from_storage: 1,
                from: "/DCIM".into(),
                names: vec!["a.jpg".into(), "b.jpg".into()],
                to_storage: 1,
                to: "/".into(),
            }]
        );
        let [
            Message::Pasted {
                volume_id,
                path,
                result,
            },
        ] = messages.as_slice()
        else {
            panic!("one Pasted message, got {messages:?}");
        };
        assert_eq!((*volume_id, path.as_str()), (1, "/"));
        let response = result.as_ref().unwrap();
        assert_eq!(response.notice.title, "Copied 2 items");
        assert_eq!(response.select, ["a.jpg", "b.jpg"]);
    }

    #[tokio::test]
    async fn a_move_is_asked_for_as_a_move() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = pasting_pane(service.clone());

        let task = handler.paste(dto::Paste::Move, vec![cut_from("/DCIM", &["a.jpg"])]);
        let _ = crate::handler::testing::drain(task).await;

        assert_eq!(service.pastes.lock().unwrap()[0].mode, dto::Paste::Move);
    }

    #[tokio::test]
    async fn a_whole_move_can_be_undone_and_a_copy_or_a_partial_move_cannot() {
        let paste = |mode, failing: bool| async move {
            let service = Arc::new(FakeExplorer::new(Ok(vec![])));
            if failing {
                service
                    .paste_results
                    .lock()
                    .unwrap()
                    .push_back(Ok(dto::PasteSummary {
                        mode,
                        done: 1,
                        renamed: 0,
                        into: "/".into(),
                        names: vec!["a.jpg".into()],
                        failures: vec![dto::FileFailure {
                            label: "b.jpg".into(),
                            error: dto::Error::NameTaken("b.jpg".into()),
                        }],
                    }));
            }
            let mut handler = pasting_pane(service);
            let task = handler.paste(mode, vec![cut_from("/DCIM", &["a.jpg", "b.jpg"])]);
            for message in crate::handler::testing::drain(task).await.0 {
                let _ = handler.update(message);
            }
            handler.last_undo.clone().map(|(_, undo)| undo)
        };

        assert_eq!(
            paste(dto::Paste::Move, false).await,
            Some(Undo::Moved(vec![undo::MovedStep {
                from: "/DCIM".into(),
                into: "/".into(),
                items: vec![
                    ("a.jpg".into(), "a.jpg".into()),
                    ("b.jpg".into(), "b.jpg".into())
                ],
            }]))
        );
        assert_eq!(paste(dto::Paste::Copy, false).await, None);
        assert_eq!(paste(dto::Paste::Move, true).await, None, "b.jpg stayed");

        // From two folders: the first refused outright, the second whole.
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        service
            .paste_results
            .lock()
            .unwrap()
            .push_back(Err(dto::Error::IntoItself("DCIM".into())));
        let mut handler = pasting_pane(service);
        let task = handler.paste(
            dto::Paste::Move,
            vec![
                cut_from("/DCIM", &["a.jpg"]),
                cut_from("/Music", &["b.mp3"]),
            ],
        );
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
        assert_eq!(
            handler.last_undo.clone(),
            None,
            "half a move is not undone by halves"
        );
    }

    #[tokio::test]
    async fn a_new_folder_is_kept_to_be_undone() {
        let mut handler = browsing("/");
        request(&mut handler, Request::NewFolder);
        request(&mut handler, Request::EditName("Trip".into()));
        let submit = handler.update(Message::Request(Request::Submit));
        for message in crate::handler::testing::drain(submit).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            handler.last_undo.clone().map(|(_, undo)| undo),
            Some(Undo::MadeFolder {
                folder: "/".into(),
                name: "Trip".into(),
            })
        );
    }

    #[test]
    fn what_arrived_is_selected_told_and_read_back_from_memory() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/DCIM", &["a.jpg"])]);

        let _ = handler.update(pasted("/", Ok(arrived(&["a (1).jpg", "b.jpg"]))));

        assert!(!handler.is_pasting());
        assert_eq!(
            handler.notice.as_ref().map(|n| n.title.as_str()),
            Some("Copied 1 item")
        );
        assert_eq!(handler.selection.names(), ["a (1).jpg", "b.jpg"]);
        assert!(handler.is_loading(), "the folder is read again");
        let _ = handler.view();
    }

    #[tokio::test]
    async fn the_reload_after_a_paste_may_use_what_the_service_remembers() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = pasting_pane(service.clone());
        let _ = crate::handler::testing::drain(handler.update(Message::Request(Request::Refresh)))
            .await;
        service.allowed.lock().unwrap().clear();

        let task = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));
        let _ = crate::handler::testing::drain(task).await;

        assert_eq!(*service.allowed.lock().unwrap(), [true]);
    }

    #[test]
    fn a_paste_that_ended_in_another_folder_still_tells_but_leaves_this_one_alone() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/DCIM", &["a.jpg"])]);
        let _ = handler.update(Message::Request(Request::GoTo("/Music".into())));
        handler.selection.only("keep");

        let _ = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));

        assert!(!handler.is_pasting());
        assert!(handler.notice.is_some());
        assert_eq!(handler.selection.names(), ["keep"], "selection untouched");
    }

    #[test]
    fn a_refused_paste_is_a_problem_banner_and_changes_nothing_else() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.selection.only("notes.txt");
        let _ = handler.paste(dto::Paste::Move, vec![cut_from("/", &["DCIM"])]);
        let refusal = error::to_error_response(dto::Error::IntoItself("DCIM".into()));

        let _ = handler.update(pasted("/", Err(refusal.clone())));

        assert!(!handler.is_pasting());
        let notice = handler.notice.as_ref().unwrap();
        assert_eq!(notice.title, refusal.title);
        assert_eq!(notice.details, [refusal.hint]);
        assert!(notice.problem);
        assert_eq!(handler.selection.names(), ["notes.txt"]);
        assert!(!handler.is_loading());
        let _ = handler.view();
    }

    #[test]
    fn a_paste_that_lost_the_phone_says_so_to_the_app() {
        let lost = error::to_error_response(dto::Error::Disconnected);
        let message = pasted("/", Err(lost));
        assert!(message.device_lost().is_some());

        let refused = error::to_error_response(dto::Error::ReadOnly);
        assert!(pasted("/", Err(refused)).device_lost().is_none());
        assert!(pasted("/", Ok(arrived(&[]))).device_lost().is_none());
    }

    #[test]
    fn edits_wait_while_a_paste_is_under_way() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/DCIM", &["a.jpg"])]);
        handler.selection.only("notes.txt");

        for edit in [Request::NewFolder, Request::Rename, Request::Delete] {
            request(&mut handler, edit);
            assert!(handler.editor.is_none(), "{:?}", handler.editor.is_some());
        }

        // And once it is over they go ahead again.
        let _ = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));
        request(&mut handler, Request::NewFolder);
        assert!(handler.editor.is_some());
    }

    #[tokio::test]
    async fn a_paste_is_ignored_when_it_cannot_go_ahead() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let from = || vec![cut_from("/DCIM", &["a.jpg"])];

        // A volume that cannot paste inside itself ignores a paste.
        let mut closed = Handler::new(service.clone());
        let _ = closed.open(
            Volume {
                pastes_within: false,
                ..mac()
            },
            "/Users/me".into(),
        );
        let _ = crate::handler::testing::drain(closed.paste(dto::Paste::Copy, from())).await;
        assert!(!closed.is_pasting());

        let mut handler = pasting_pane(service.clone());
        // Nothing to paste.
        let _ = crate::handler::testing::drain(handler.paste(dto::Paste::Copy, vec![])).await;
        let _ = crate::handler::testing::drain(
            handler.paste(dto::Paste::Copy, vec![cut_from("/DCIM", &[])]),
        )
        .await;
        assert!(!handler.is_pasting());
        // A dialog is open.
        request(&mut handler, Request::NewFolder);
        let _ = crate::handler::testing::drain(handler.paste(dto::Paste::Copy, from())).await;
        assert!(!handler.is_pasting());
        request(&mut handler, Request::Cancel);
        // One at a time.
        let _ = handler.paste(dto::Paste::Copy, from());
        let _ = crate::handler::testing::drain(handler.paste(dto::Paste::Move, from())).await;

        assert!(
            service.pastes.lock().unwrap().is_empty(),
            "no task was run yet"
        );
        assert!(handler.is_pasting());
    }

    #[test]
    fn the_banner_goes_when_dismissed_or_when_the_folder_changes() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));
        assert!(handler.notice.is_some());
        request(&mut handler, Request::DismissNotice);
        assert!(handler.notice.is_none());

        let _ = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));
        request(&mut handler, Request::Open("DCIM".into()));
        assert!(handler.notice.is_none(), "a new folder, a clean pane");

        let _ = handler.update(pasted("/DCIM", Ok(arrived(&["a.jpg"]))));
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/", &["notes.txt"])]);
        assert!(
            handler.notice.is_none(),
            "a new paste replaces the old banner"
        );
    }

    #[test]
    fn closing_the_pane_forgets_the_paste_and_the_cut_and_reopening_keeps_the_cut() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.paste(dto::Paste::Move, vec![cut_from("/DCIM", &["a.jpg"])]);
        handler.set_cut(Some(&cut_from("/", &["notes.txt"])));

        handler.close();

        assert!(!handler.is_pasting());
        assert!(handler.notice.is_none() && handler.cut.is_none());

        // Opening another storage (or the same one again) forgets the banner
        // but not the cut: the app still holds it and a paste would still
        // move it, so it must still look cut where it is.
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_cut(Some(&cut_from("/", &["notes.txt"])));
        let _ = handler.update(pasted("/", Ok(arrived(&["a.jpg"]))));
        let _ = handler.open(storage(), "/".into());
        assert!(handler.notice.is_none());
        assert!(handler.is_cut("notes.txt"));
    }

    #[test]
    fn cut_items_are_dim_only_in_the_folder_they_were_cut_from() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_cut(Some(&cut_from("/", &["notes.txt"])));

        assert!(handler.is_cut("notes.txt"));
        assert!(!handler.is_cut("DCIM"), "not cut");

        let _ = handler.update(Message::Request(Request::GoTo("/DCIM".into())));
        assert!(!handler.is_cut("notes.txt"), "same name, another folder");
        let _ = handler.update(Message::Request(Request::GoTo("/".into())));
        assert!(handler.is_cut("notes.txt"), "back where it was cut");

        handler.set_cut(None);
        assert!(!handler.is_cut("notes.txt"));
        // Another storage's folder of the same name is not it either.
        handler.set_cut(Some(&Selection {
            volume_id: 2,
            ..cut_from("/", &["notes.txt"])
        }));
        assert!(!handler.is_cut("notes.txt"));
    }

    #[test]
    fn cut_items_render_dim_in_the_list_and_in_the_grid() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_cut(Some(&cut_from("/", &["notes.txt"])));
        let _ = handler.view();

        let _ = handler.set_view(dto::ViewMode::Grid);
        let _ = handler.view();
    }

    #[test]
    fn a_pane_pastes_inside_itself_only_where_its_volume_can() {
        let from = vec![cut_from("/", &["notes.txt"])];
        let mut handler = handler();
        let _ = handler.paste(dto::Paste::Move, from.clone());
        assert!(!handler.is_pasting(), "no folder open");

        let _ = handler.open(
            Volume {
                pastes_within: false,
                ..mac()
            },
            "/Users/me".into(),
        );
        let _ = handler.paste(dto::Paste::Move, from.clone());
        assert!(
            !handler.is_pasting(),
            "a volume that can't paste inside itself"
        );

        let _ = handler.open(mac(), "/Users/me".into());
        let _ = handler.paste(dto::Paste::Move, from.clone());
        assert!(handler.is_pasting(), "the Mac pane can, like the phone");

        let _ = handler.open(storage(), "/".into());
        let _ = handler.paste(dto::Paste::Move, from);
        assert!(handler.is_pasting());
    }

    #[tokio::test]
    async fn several_folders_are_pasted_in_turn_and_reported_as_one() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let requests = vec![
            paste_request(dto::Paste::Copy, "/DCIM", &["a.jpg", "b.jpg"]),
            paste_request(dto::Paste::Copy, "/Download", &["c.pdf"]),
        ];

        let response = paste_items(service.clone(), requests).await.done().unwrap();

        let asked = service.pastes.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            (asked[0].from.as_str(), asked[1].from.as_str()),
            ("/DCIM", "/Download")
        );
        assert_eq!(response.notice.title, "Copied 3 items");
        assert_eq!(response.select, ["a.jpg", "b.jpg", "c.pdf"]);
    }

    fn paste_request(mode: dto::Paste, from: &str, names: &[&str]) -> dto::PasteRequest {
        dto::PasteRequest {
            mode,
            from_storage: 1,
            from: from.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            to_storage: 1,
            to: "/".into(),
        }
    }

    fn summary_of(done: &[&str], renamed: usize) -> dto::PasteSummary {
        dto::PasteSummary {
            mode: dto::Paste::Copy,
            done: done.len(),
            renamed,
            into: "/".into(),
            names: done.iter().map(|n| (*n).to_owned()).collect(),
            failures: Vec::new(),
        }
    }

    #[tokio::test]
    async fn the_totals_of_several_pastes_add_up_including_renames_and_failures() {
        let failing = dto::PasteSummary {
            failures: vec![dto::FileFailure {
                label: "big.mov".into(),
                error: dto::Error::ReadOnly,
            }],
            ..summary_of(&["c"], 0)
        };
        let service = Arc::new(FakeExplorer::answering_pastes(vec![
            Ok(summary_of(&["a (1)", "b"], 1)),
            Ok(failing),
        ]));
        let requests = vec![
            paste_request(dto::Paste::Copy, "/one", &["a", "b"]),
            paste_request(dto::Paste::Copy, "/two", &["c", "big.mov"]),
        ];

        let response = paste_items(service, requests).await.done().unwrap();

        assert_eq!(response.notice.title, "Copied 3 of 4 items");
        assert!(response.notice.problem);
        assert_eq!(response.select, ["a (1)", "b", "c"]);
        assert!(
            response.notice.details[0].contains("renamed"),
            "{:?}",
            response.notice.details
        );
        assert!(response.notice.details[1].starts_with("“big.mov”"));
    }

    #[tokio::test]
    async fn one_refused_folder_does_not_stop_the_others_but_is_counted() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![
            Err(dto::Error::IntoItself("Trip".into())),
            Ok(summary_of(&["c.pdf"], 0)),
        ]));
        let requests = vec![
            paste_request(dto::Paste::Move, "/", &["Trip"]),
            paste_request(dto::Paste::Move, "/Download", &["c.pdf"]),
        ];

        let response = paste_items(service.clone(), requests).await.done().unwrap();

        assert_eq!(service.pastes.lock().unwrap().len(), 2);
        assert_eq!(response.notice.title, "Moved 1 of 2 items");
        assert!(response.notice.problem);
        assert_eq!(response.select, ["c.pdf"]);
        assert!(response.notice.details[0].starts_with("“Trip”"));
    }

    #[tokio::test]
    async fn a_lone_refusal_is_returned_with_its_full_hint() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::ReadOnly,
        )]));

        let error = paste_items(service, vec![paste_request(dto::Paste::Copy, "/a", &["x"])])
            .await
            .done()
            .unwrap_err();

        assert_eq!(error.title, "This storage is read-only");
    }

    #[tokio::test]
    async fn losing_the_phone_stops_the_rest_and_is_returned() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![
            Ok(summary_of(&["a"], 0)),
            Err(dto::Error::Disconnected),
            Ok(summary_of(&["never"], 0)),
        ]));
        let requests = vec![
            paste_request(dto::Paste::Copy, "/one", &["a"]),
            paste_request(dto::Paste::Copy, "/two", &["b"]),
            paste_request(dto::Paste::Copy, "/three", &["c"]),
        ];

        let error = paste_items(service.clone(), requests)
            .await
            .done()
            .unwrap_err();

        assert!(error.device_lost);
        assert_eq!(
            service.pastes.lock().unwrap().len(),
            2,
            "the third was never asked"
        );
    }

    #[test]
    fn the_banner_view_cost_does_not_depend_on_the_folder() {
        // A banner is a handful of widgets however long the folder.
        let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.open(storage(), "/".into());
        let entries: Vec<_> = (0..5_000)
            .map(|i| file(&format!("IMG_{i:05}.jpg")))
            .collect();
        let _ = handler.update(listed("/", &entries));
        let many = NoticeResponse {
            title: "Copied 2 of 9 items".into(),
            details: (0..4).map(|i| format!("“f{i}”: read-only")).collect(),
            problem: true,
        };
        let _ = handler.update(pasted(
            "/",
            Ok(PasteResponse {
                select: vec![],
                notice: many,
                undo: None,
            }),
        ));

        let started = std::time::Instant::now();
        for _ in 0..20 {
            let _ = handler.view();
        }

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    // ---- a phone that cannot copy or move by itself ----

    #[tokio::test]
    async fn a_phone_that_cannot_paste_hands_the_paste_over_to_the_app() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::Unsupported,
        )]));
        let mut handler = pasting_pane(service.clone());
        let from = vec![cut_from("/DCIM", &["a.jpg", "b.jpg"])];

        let task = handler.paste(dto::Paste::Move, from.clone());
        let (messages, _) = crate::handler::testing::drain(task).await;

        let [message] = messages.as_slice() else {
            panic!("one message, got {messages:?}");
        };
        assert_eq!(
            message.via_mac(),
            Some((dto::Paste::Move, from.as_slice(), 1, "/")),
            "what to paste, and where: the folder shown"
        );
        assert!(
            !matches!(message, Message::Pasted { .. }),
            "no error banner"
        );
        assert_eq!(
            service.pastes.lock().unwrap().len(),
            1,
            "the phone was asked once"
        );
    }

    #[test]
    fn the_pane_stops_waiting_once_the_app_has_taken_the_paste_over() {
        let mut handler = pasting_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/DCIM", &["a.jpg"])]);
        assert!(handler.is_pasting());

        let _ = handler.update(Message::PasteViaMac {
            mode: dto::Paste::Copy,
            from: vec![cut_from("/DCIM", &["a.jpg"])],
            volume_id: 1,
            path: "/".into(),
        });

        assert!(!handler.is_pasting());
        assert!(
            handler.notice.is_none(),
            "the transfer panel tells the rest"
        );
        request(&mut handler, Request::NewFolder);
        assert!(handler.editor.is_some(), "edits can go ahead again");
    }

    #[tokio::test]
    async fn every_folder_is_handed_over_when_the_phone_cannot_paste_at_all() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::Unsupported,
        )]));
        let requests = vec![
            paste_request(dto::Paste::Copy, "/one", &["a"]),
            paste_request(dto::Paste::Copy, "/two", &["b"]),
        ];

        let end = paste_items(service.clone(), requests).await;

        assert!(matches!(end, PasteEnd::ViaMac));
        assert_eq!(
            service.pastes.lock().unwrap().len(),
            1,
            "the second was never asked: the phone had said it cannot"
        );
    }

    #[tokio::test]
    async fn unsupported_after_something_was_done_is_a_failure_like_any_other() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![
            Ok(summary_of(&["a"], 0)),
            Err(dto::Error::Unsupported),
        ]));
        let requests = vec![
            paste_request(dto::Paste::Copy, "/one", &["a"]),
            paste_request(dto::Paste::Copy, "/two", &["b"]),
        ];

        let response = paste_items(service, requests).await.done().unwrap();

        assert_eq!(response.notice.title, "Copied 1 of 2 items");
        assert!(response.notice.problem);
    }

    #[tokio::test]
    async fn another_refusal_is_not_handed_over() {
        let service = Arc::new(FakeExplorer::answering_pastes(vec![Err(
            dto::Error::ReadOnly,
        )]));

        let end = paste_items(service, vec![paste_request(dto::Paste::Copy, "/a", &["x"])]).await;

        assert!(matches!(end, PasteEnd::Done(Err(_))));
    }

    // ---- dragging onto a folder ----

    /// A phone pane at `/` holding a.jpg, b.jpg and the folders DCIM and
    /// Music, pasting through `service`.
    fn dragging_pane(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed(
            "/",
            &[
                file("a.jpg"),
                file("b.jpg"),
                folder("DCIM"),
                folder("Music"),
            ],
        ));
        handler
    }

    /// Press on `name`, leave it with the button down, and come over `over`.
    fn drag(handler: &mut Handler, name: &str, over: Option<&str>) {
        request(handler, Request::Select(name.into()));
        request(handler, Request::PointerLeft(name.into()));
        if let Some(over) = over {
            request(handler, Request::PointerEntered(over.into()));
        }
    }

    async fn release(handler: &mut Handler) -> Vec<Message> {
        let task = handler.update(Message::Request(Request::PointerReleased));
        crate::handler::testing::drain(task).await.0
    }

    fn dropped(service: &FakeExplorer) -> Vec<dto::PasteRequest> {
        service.pastes.lock().unwrap().clone()
    }

    #[test]
    fn leaving_the_pressed_row_with_the_button_down_starts_a_drag_that_says_where_it_would_go() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));
        assert_eq!(handler.drag_hint(), None, "a press is not a drag yet");

        request(&mut handler, Request::PointerLeft("a.jpg".into()));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Dragging 1 item: drop on a folder · ⌥ copies · Esc cancels")
        );
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Move 1 item into “DCIM” · hold ⌥ to copy")
        );
        request(&mut handler, Request::PointerEntered("b.jpg".into()));
        assert!(
            handler.drag_hint().unwrap().starts_with("Dragging"),
            "a file is no place to drop"
        );
        let _ = handler.view();
    }

    #[test]
    fn leaving_a_row_that_was_not_pressed_is_not_a_drag() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));

        request(&mut handler, Request::PointerLeft("b.jpg".into()));

        assert_eq!(handler.drag_hint(), None);
    }

    #[tokio::test]
    async fn letting_go_over_a_folder_moves_the_selection_into_it() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("DCIM"));

        let messages = release(&mut handler).await;

        assert_eq!(
            dropped(&service),
            [dto::PasteRequest {
                mode: dto::Paste::Move,
                from_storage: 1,
                from: "/".into(),
                names: vec!["a.jpg".into()],
                to_storage: 1,
                to: "/DCIM".into(),
            }]
        );
        assert!(matches!(
            messages.as_slice(),
            [Message::Pasted { path, .. }] if path == "/DCIM"
        ));
        assert!(handler.is_pasting());
        assert_eq!(handler.drag_hint(), None, "the drag is over");
    }

    #[tokio::test]
    async fn holding_alt_copies_instead() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("Music"));
        handler.set_modifiers(Modifiers::ALT);
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Copy 1 item into “Music”")
        );

        let _ = release(&mut handler).await;

        assert_eq!(dropped(&service)[0].mode, dto::Paste::Copy);
        assert_eq!(dropped(&service)[0].to, "/Music");
    }

    #[tokio::test]
    async fn nothing_is_dropped_on_a_file_on_nothing_or_on_what_is_being_dragged() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));

        // On a file.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("b.jpg"));
        let _ = release(&mut handler).await;
        // On nothing: out in the empty part of the pane, or another pane.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", None);
        let _ = release(&mut handler).await;
        // A folder on itself.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "DCIM", Some("DCIM"));
        let _ = release(&mut handler).await;

        assert!(dropped(&service).is_empty());
    }

    #[tokio::test]
    async fn a_click_is_not_a_drag() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        request(&mut handler, Request::Select("DCIM".into()));

        let messages = release(&mut handler).await;

        assert!(messages.is_empty());
        assert!(dropped(&service).is_empty());
        assert_eq!(selected(&handler), ["DCIM"]);
    }

    /// a.jpg and b.jpg selected, with nothing pressed.
    fn two_selected(handler: &mut Handler) {
        request(handler, Request::Select("a.jpg".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(handler, Request::Select("b.jpg".into()));
        handler.set_modifiers(Modifiers::empty());
        request(handler, Request::PointerReleased);
        assert_eq!(selected(handler), ["a.jpg", "b.jpg"]);
    }

    #[tokio::test]
    async fn dragging_one_of_several_selected_rows_drags_them_all() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        two_selected(&mut handler);

        request(&mut handler, Request::Select("a.jpg".into()));
        assert_eq!(
            selected(&handler),
            ["a.jpg", "b.jpg"],
            "the press keeps both"
        );
        request(&mut handler, Request::PointerLeft("a.jpg".into()));
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Move 2 items into “DCIM” · hold ⌥ to copy")
        );
        let _ = release(&mut handler).await;

        assert_eq!(dropped(&service)[0].names, ["a.jpg", "b.jpg"]);
    }

    #[test]
    fn a_click_on_one_of_several_selected_rows_narrows_to_it_when_the_button_comes_up() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        two_selected(&mut handler);

        request(&mut handler, Request::Select("b.jpg".into()));
        assert_eq!(selected(&handler), ["a.jpg", "b.jpg"], "not yet");
        request(&mut handler, Request::PointerReleased);

        assert_eq!(selected(&handler), ["b.jpg"]);
    }

    #[test]
    fn a_modified_click_still_changes_the_selection_at_once() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        two_selected(&mut handler);

        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("a.jpg".into()));

        assert_eq!(selected(&handler), ["b.jpg"], "⌘-click takes it out");
    }

    #[tokio::test]
    async fn escape_calls_off_a_drag_that_has_started() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("DCIM"));

        request(&mut handler, Request::Escape);
        let _ = release(&mut handler).await;

        assert!(dropped(&service).is_empty());
        assert_eq!(selected(&handler), ["a.jpg"], "the selection is as it was");
    }

    #[test]
    fn escape_on_a_press_that_has_not_started_dragging_does_what_it_always_did() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));

        request(&mut handler, Request::Escape);

        assert!(selected(&handler).is_empty());
    }

    #[tokio::test]
    async fn a_folder_in_the_mac_pane_takes_a_drop_like_one_on_the_phone() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = Handler::new(service.clone());
        let _ = handler.open(mac(), "/Users/me".into());
        let _ = handler.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.txt"), folder("Docs")]),
        });

        drag(&mut handler, "a.txt", Some("Docs"));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Move 1 item into “Docs” · hold ⌥ to copy")
        );
        let _ = release(&mut handler).await;

        let requests = dropped(&service);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, dto::Paste::Move);
        assert_eq!(
            (requests[0].from.as_str(), requests[0].to.as_str()),
            ("/Users/me", "/Users/me/Docs")
        );
        assert_eq!(requests[0].names, ["a.txt"]);
    }

    #[tokio::test]
    async fn a_drop_while_another_paste_is_running_waits_for_nothing_and_does_nothing() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        let _ = handler.paste(dto::Paste::Copy, vec![cut_from("/", &["b.jpg"])]);
        drag(&mut handler, "a.jpg", Some("DCIM"));

        let messages = release(&mut handler).await;

        assert!(messages.is_empty());
        assert!(handler.is_pasting());
    }

    #[test]
    fn a_release_with_nothing_pressed_does_nothing() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));

        request(&mut handler, Request::PointerReleased);
        request(&mut handler, Request::PointerLeft("a.jpg".into()));
        request(&mut handler, Request::PointerEntered("DCIM".into()));

        assert_eq!(handler.drag_hint(), None);
        assert!(selected(&handler).is_empty());
    }

    #[test]
    fn what_a_drop_did_is_said_with_where_it_went_and_the_folder_is_read_again() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("b.jpg".into()));
        request(&mut handler, Request::PointerReleased);

        let _ = handler.update(pasted("/DCIM", Ok(arrived(&["a.jpg"]))));

        assert_eq!(
            handler.notice.as_ref().map(|n| n.title.as_str()),
            Some("Copied 1 item to “DCIM”")
        );
        assert_eq!(
            selected(&handler),
            ["b.jpg"],
            "this folder's selection is its own"
        );
        assert!(handler.is_loading(), "a move empties what is shown here");
    }

    #[test]
    fn the_folders_name_comes_from_the_end_of_the_path() {
        assert_eq!(mapper::folder_name("/DCIM/Camera"), "Camera");
        assert_eq!(mapper::folder_name("/DCIM/Camera/"), "Camera");
        assert_eq!(mapper::folder_name("/"), "the top folder");
    }

    #[test]
    fn the_hint_names_the_destination_and_what_alt_does() {
        assert_eq!(
            mapper::drag_hint(3, Some("Camera"), false, true),
            "Move 3 items into “Camera” · hold ⌥ to copy"
        );
        assert_eq!(
            mapper::drag_hint(1, Some("Camera"), true, true),
            "Copy 1 item into “Camera”"
        );
        assert_eq!(
            mapper::drag_hint(2, None, false, false),
            "Dragging 2 items: drop on a folder · ⌥ copies · Esc cancels"
        );
        assert_eq!(
            mapper::drag_hint(2, None, false, true),
            "Dragging 2 items: drop on a folder or the other pane · ⌥ copies · Esc cancels"
        );
    }

    #[test]
    fn a_drag_from_the_other_pane_copies_here_or_into_a_folder_and_moves_with_command() {
        assert_eq!(
            mapper::incoming_hint(3, None, false),
            "Copy 3 items here · hold ⌘ to move"
        );
        assert_eq!(
            mapper::incoming_hint(1, Some("Camera"), false),
            "Copy 1 item into “Camera” · hold ⌘ to move"
        );
        assert_eq!(
            mapper::incoming_hint(2, Some("Camera"), true),
            "Move 2 items into “Camera”"
        );
    }

    // The rows themselves: they report the pointer only while a button is down,
    // so moving over a list costs nothing the rest of the time.

    fn pointer_messages(handler: &Handler, path: &[&str]) -> Vec<Request> {
        let mut ui = iced_test::simulator(handler.view());
        for label in path {
            let at = ui.find(*label).unwrap().visible_bounds().unwrap().center();
            ui.point_at(at);
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: at,
            })]);
        }
        ui.into_messages()
            .filter_map(|message| match message {
                Message::Request(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn with_a_button_down_the_rows_report_the_pointer_crossing_them() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));

        let seen = pointer_messages(&handler, &["a.jpg", "DCIM"]);

        assert!(
            seen.contains(&Request::PointerLeft("a.jpg".into())),
            "{seen:?}"
        );
        assert!(
            seen.contains(&Request::PointerEntered("DCIM".into())),
            "{seen:?}"
        );
    }

    #[test]
    fn with_no_button_down_moving_over_the_rows_reports_nothing() {
        let handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));

        let seen = pointer_messages(&handler, &["a.jpg", "DCIM", "b.jpg"]);

        assert!(seen.is_empty(), "{seen:?}");
    }

    #[test]
    fn a_drag_draws_in_both_themes_and_in_the_grid_with_its_target_marked() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        drag(&mut handler, "a.jpg", Some("DCIM"));

        for view in [dto::ViewMode::List, dto::ViewMode::Grid] {
            let _ = handler.set_view(view);
            for theme in [iced::Theme::Light, iced::Theme::Dark] {
                let mut ui = iced_test::simulator(handler.view());
                assert!(ui.snapshot(&theme).is_ok(), "{view:?}");
            }
        }
        // Changing the view forgot the target; the pointer moves, and it is back.
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        // The folder under the pointer is the target.
        let look = |name: &str| {
            let row = handler.rows.iter().find(|row| row.name == name).unwrap();
            handler.look(row)
        };
        assert!(!look("a.jpg").dim, "what is dragged stays readable");
        assert!(
            look("DCIM").target && !look("Music").target,
            "DCIM is under the pointer"
        );
        assert!(!look("a.jpg").target, "a file is never a target");
    }

    #[test]
    fn dragging_in_a_big_folder_costs_no_more_to_draw_than_in_a_small_one() {
        drop(iced_test::simulator(
            folder_of(10, dto::ViewMode::List, 0.0).view(),
        ));
        let dragging = |n: usize| {
            let mut handler = folder_of(n, dto::ViewMode::List, 0.0);
            request(&mut handler, Request::Select("IMG_00001.jpg".into()));
            request(&mut handler, Request::PointerLeft("IMG_00001.jpg".into()));
            request(
                &mut handler,
                Request::PointerEntered("IMG_00002.jpg".into()),
            );
            handler
        };

        let small = build_seconds(&dragging(1_000));
        let large = build_seconds(&dragging(5_000));

        assert!(
            large < small.max(0.020) * 5.0,
            "1,000 files took {small:.3}s, 5,000 took {large:.3}s"
        );
        let built = rows_built(&dragging(5_000), 5_000);
        assert!(built.len() < 100, "{} rows were built", built.len());
    }

    #[test]
    fn where_nothing_can_be_dragged_a_click_on_a_selected_row_narrows_at_once() {
        let mut handler = Handler::new(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.open(
            Volume {
                pastes_within: false,
                ..mac()
            },
            "/Users/me".into(),
        );
        let _ = handler.update(Message::Listed {
            volume_id: 0,
            path: "/Users/me".into(),
            result: Ok(vec![file("a.txt"), file("b.txt"), file("c.txt")]),
        });
        request(&mut handler, Request::SelectAll);

        request(&mut handler, Request::Select("b.txt".into()));

        assert_eq!(
            selected(&handler),
            ["b.txt"],
            "no release is coming to do it"
        );
    }

    // ---- what the hint must not do, and what can go stale mid-drag ----

    /// Where the rows `labels` are drawn.
    fn rows_at(handler: &Handler, labels: &[&str]) -> Vec<iced::Rectangle> {
        let mut ui = iced_test::simulator(handler.view());
        labels
            .iter()
            .map(|label| ui.find(*label).unwrap().visible_bounds().unwrap())
            .collect()
    }

    #[test]
    fn starting_a_drag_moves_no_row_under_the_pointer() {
        // The hint appearing above the list would slide every row down by its
        // height, and a still pointer would then be over another row: a drop
        // into the wrong folder.
        for status_bar in [true, false] {
            let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
            handler.set_status_bar(status_bar);
            let before = rows_at(&handler, &["a.jpg", "DCIM", "Music"]);

            request(&mut handler, Request::Select("a.jpg".into()));
            assert_eq!(
                rows_at(&handler, &["a.jpg", "DCIM", "Music"]),
                before,
                "pressed"
            );
            request(&mut handler, Request::PointerLeft("a.jpg".into()));
            assert_eq!(
                rows_at(&handler, &["a.jpg", "DCIM", "Music"]),
                before,
                "dragging, status bar {status_bar}"
            );
            request(&mut handler, Request::PointerEntered("DCIM".into()));
            assert_eq!(
                rows_at(&handler, &["a.jpg", "DCIM", "Music"]),
                before,
                "over a folder"
            );
        }
    }

    #[test]
    fn the_hint_is_said_in_the_footer_even_when_the_footer_is_turned_off() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_status_bar(false);
        drag(&mut handler, "a.jpg", Some("DCIM"));

        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(
                ui.find("Move 1 item into “DCIM” · hold ⌥ to copy").is_ok(),
                "shown for the length of the drag"
            );
        }

        request(&mut handler, Request::PointerReleased);
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find("Folders first").is_err(),
            "and gone again with it, as the footer was off"
        );
    }

    /// A pane with a folder first and enough files below it to scroll.
    fn long_pane(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = Handler::new(service);
        let _ = handler.open(storage(), "/".into());
        let mut entries = vec![folder("DCIM")];
        entries.extend((0..80).map(|i| file(&format!("IMG_{i:03}.jpg"))));
        let _ = handler.update(listed("/", &entries));
        handler
    }

    #[tokio::test]
    async fn a_target_found_before_the_list_scrolled_is_not_dropped_on() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = long_pane(service.clone());
        drag(&mut handler, "IMG_001.jpg", Some("DCIM"));
        // The wheel turns with the pointer where it is: other rows slide under it.
        let mut ui = iced_test::simulator(handler.view());
        let at = ui.find("DCIM").unwrap().visible_bounds().unwrap().center();
        ui.point_at(at);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
            delta: iced::mouse::ScrollDelta::Lines { x: 0.0, y: -3.0 },
        })]);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.iter().any(|m| matches!(m, Message::Scrolled(_))),
            "the list scrolled: {messages:?}"
        );
        for message in messages {
            let _ = handler.update(message);
        }

        let released = release(&mut handler).await;

        assert!(released.is_empty(), "no target until the pointer moves");
        assert!(dropped(&service).is_empty());
    }

    #[tokio::test]
    async fn a_target_is_forgotten_whenever_the_rows_can_change_under_the_pointer() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let changes: [fn(&mut Handler); 4] = [
            |handler| {
                let _ = handler.update(listed("/", &[folder("DCIM"), file("a.jpg")]));
            },
            |handler| {
                let _ = handler.update(Message::Request(Request::SortBy(dto::SortKey::Size)));
            },
            |handler| {
                let _ = handler.set_view(dto::ViewMode::Grid);
            },
            |handler| {
                let _ = handler.update(Message::Loading {
                    volume_id: 1,
                    path: "/".into(),
                    entries: vec![folder("DCIM"), file("a.jpg")],
                    loaded: 2,
                    total: 9,
                });
            },
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut handler = dragging_pane(service.clone());
            // `Loading` only counts for a folder that is loading.
            handler.loading = true;
            drag(&mut handler, "a.jpg", Some("DCIM"));

            change(&mut handler);
            let released = release(&mut handler).await;

            assert!(released.is_empty(), "change {index}");
        }
        assert!(dropped(&service).is_empty());
    }

    #[test]
    fn mid_drag_every_movement_inside_a_row_says_which_row_it_is_over() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        drag(&mut handler, "a.jpg", None);
        let mut ui = iced_test::simulator(handler.view());
        let row = ui.find("DCIM").unwrap().visible_bounds().unwrap();

        for step in [0.25, 0.5, 0.75] {
            let at = iced::Point::new(row.x + row.width * step, row.center_y());
            ui.point_at(at);
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: at,
            })]);
        }
        let seen: Vec<Request> = ui
            .into_messages()
            .filter_map(|message| match message {
                Message::Request(request) => Some(request),
                _ => None,
            })
            .filter(|request| *request == Request::PointerEntered("DCIM".into()))
            .collect();

        assert!(
            seen.len() >= 3,
            "{} reports for three movements",
            seen.len()
        );
    }

    #[tokio::test]
    async fn opening_a_folder_leaves_no_drag_behind_in_the_new_one() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        // The first press of a double click on a folder arms a drag.
        request(&mut handler, Request::Select("DCIM".into()));
        request(&mut handler, Request::Open("DCIM".into()));
        assert!(handler.drag.is_none());

        let _ = handler.update(listed("/DCIM", &[folder("Camera"), file("x.jpg")]));
        request(&mut handler, Request::PointerEntered("Camera".into()));
        let released = release(&mut handler).await;

        assert!(released.is_empty());
        assert!(dropped(&service).is_empty());
    }

    // The lasso: drag a box to select.

    /// The messages a left press at `at` (window coordinates) sends.
    fn press_messages(handler: &Handler, at: Point) -> Vec<Message> {
        let mut ui = iced_test::simulator(handler.view());
        ui.point_at(at);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
            iced::mouse::Button::Left,
        ))]);
        ui.into_messages().collect()
    }

    fn swept_names(handler: &Handler) -> Vec<String> {
        handler.selection.names().to_vec()
    }

    fn feed(handler: &mut Handler, messages: Vec<Message>) {
        for message in messages {
            let _ = handler.update(message);
        }
    }

    #[test]
    fn a_press_on_a_name_can_drag_and_elsewhere_on_the_row_starts_a_lasso() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let name = {
            let mut ui = iced_test::simulator(handler.view());
            ui.find("b.jpg").unwrap().visible_bounds().unwrap()
        };

        // On the name: the row says which, then the list where.
        let on_name = press_messages(&handler, name.center());
        assert!(
            matches!(
                on_name.as_slice(),
                [
                    Message::Request(Request::Select(row)),
                    Message::PressedAt { taken: true, .. }
                ] if row == "b.jpg"
            ),
            "{on_name:?}"
        );
        feed(&mut handler, on_name);
        assert_eq!(handler.pointer(), Pointer::Armed, "it may be dragged");
        assert!(handler.lasso.is_none());

        // On the same row, far to the right of the name.
        let _ = handler.update(Message::Request(Request::PointerReleased));
        let beside = Point::new(name.x + 600.0, name.center_y());
        let on_row = press_messages(&handler, beside);
        assert!(
            matches!(
                on_row.as_slice(),
                [
                    Message::Request(Request::PressRow(row)),
                    Message::PressedAt { taken: true, .. }
                ] if row == "b.jpg"
            ),
            "{on_row:?}"
        );
        feed(&mut handler, on_row);
        assert!(handler.lasso.is_some(), "a lasso");
        assert_eq!(handler.pointer(), Pointer::Idle, "not a drag");
        assert_eq!(swept_names(&handler), ["b.jpg"], "the row it started on");
    }

    #[test]
    fn a_press_on_empty_space_clears_the_selection_and_the_scrollbar_does_nothing() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));
        let _ = handler.update(Message::Request(Request::PointerReleased));
        let list = {
            let mut ui = iced_test::simulator(handler.view());
            ui.find(handler.list.clone()).unwrap().bounds()
        };

        let empty = press_messages(
            &handler,
            Point::new(list.center_x(), list.y + list.height - 20.0),
        );
        assert!(
            matches!(empty.as_slice(), [Message::PressedAt { taken: false, .. }]),
            "{empty:?}"
        );
        feed(&mut handler, empty);
        assert!(swept_names(&handler).is_empty());
        let _ = handler.update(Message::Request(Request::PointerReleased));

        // A long list has a scrollbar at its right edge: pressing it is not a lasso.
        let mut long = folder_of(5_000, dto::ViewMode::List, 0.0);
        request(&mut long, Request::Select("IMG_00003.jpg".into()));
        let _ = long.update(Message::Request(Request::PointerReleased));
        let list = {
            let mut ui = iced_test::simulator(long.view());
            ui.find(long.list.clone()).unwrap().bounds()
        };
        let on_bar = press_messages(&long, Point::new(list.x + list.width - 3.0, list.y + 40.0));
        // The track scrolls the list, and the press is the scrollbar's.
        assert!(
            matches!(on_bar.last(), Some(Message::PressedAt { taken: true, .. })),
            "{on_bar:?}"
        );
        feed(&mut long, on_bar);
        assert!(long.lasso.is_none());
        assert_eq!(swept_names(&long), ["IMG_00003.jpg"]);
    }

    #[test]
    fn a_word_from_a_row_is_forgotten_by_the_next_request() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        // A row says it was pressed, but another request comes before the
        // list's word (here, Select All).
        request(&mut handler, Request::PressRow("a.jpg".into()));
        request(&mut handler, Request::SelectAll);

        // The scrollbar's press comes with no row before it.
        let _ = handler.update(Message::PressedAt {
            at: Point::new(5.0, 5.0),
            size: Size::new(600.0, 400.0),
            taken: true,
        });
        assert!(handler.lasso.is_none());
        assert_eq!(swept_names(&handler).len(), 4, "Select All stands");
    }

    /// A lasso started on row `from` and moved to row `to` of a list of
    /// `row_height` rows, with the keys held.
    fn sweep(handler: &mut Handler, from: usize, to: usize, keys: Modifiers) {
        let height = handler.row_height();
        let size = Size::new(600.0, 400.0);
        handler.set_modifiers(keys);
        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, from as f32 * height + 5.0 - handler.viewport.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(320.0, to as f32 * height + 5.0 - handler.viewport.0),
            size,
        });
    }

    #[test]
    fn a_lasso_selects_the_rows_it_spans_and_keys_add_or_toggle() {
        let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg", "f.jpg"];
        let entries: Vec<_> = names.iter().map(|name| file(name)).collect();
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &entries));

        sweep(&mut handler, 3, 1, Modifiers::empty());
        assert_eq!(
            swept_names(&handler),
            ["b.jpg", "c.jpg", "d.jpg"],
            "drawn upwards"
        );
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(320.0, 3.0 * handler.row_height() + 5.0),
            size: Size::new(600.0, 400.0),
        });
        assert_eq!(
            swept_names(&handler),
            ["d.jpg"],
            "shrunk back to where it started"
        );
        request(&mut handler, Request::PointerReleased);
        assert!(handler.lasso.is_none());
        assert_eq!(swept_names(&handler), ["d.jpg"], "the selection stays");

        sweep(&mut handler, 2, 4, Modifiers::COMMAND);
        assert_eq!(swept_names(&handler), ["c.jpg", "e.jpg"], "⌘ turns over");
        request(&mut handler, Request::PointerReleased);

        sweep(&mut handler, 0, 0, Modifiers::SHIFT);
        assert_eq!(swept_names(&handler), ["a.jpg", "c.jpg", "e.jpg"], "⇧ adds");
        request(&mut handler, Request::Escape);
        assert!(handler.lasso.is_none(), "Escape lets go of the box");
        assert_eq!(swept_names(&handler), ["a.jpg", "c.jpg", "e.jpg"]);
    }

    #[test]
    fn a_lasso_in_a_scrolled_list_starts_on_the_row_under_the_pointer() {
        let height = row_style(dto::Density::Standard).height;
        let mut handler = folder_of(5_000, dto::ViewMode::List, 100.0 * height);

        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, 5.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        assert_eq!(swept_names(&handler), ["IMG_00100.jpg"]);

        let _ = handler.update(Message::LassoMoved {
            at: Point::new(300.0, height + 5.0),
            size: Size::new(600.0, 400.0),
        });
        assert_eq!(swept_names(&handler), ["IMG_00100.jpg", "IMG_00101.jpg"]);
    }

    #[test]
    fn shift_on_a_row_selects_up_to_it_first_as_a_shift_click_does() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));
        request(&mut handler, Request::PointerReleased);

        handler.set_modifiers(Modifiers::SHIFT);
        request(&mut handler, Request::PressRow("DCIM".into()));
        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, 2.0 * handler.row_height() + 5.0),
            size: Size::new(600.0, 400.0),
            taken: true,
        });

        assert_eq!(
            swept_names(&handler).len(),
            3,
            "a.jpg to DCIM: {:?}",
            swept_names(&handler)
        );
    }

    #[test]
    fn scrolling_the_list_under_a_lasso_takes_in_the_rows_that_come_under_it() {
        let mut handler = folder_of(5_000, dto::ViewMode::List, 0.0);
        let height = handler.row_height();
        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, 5.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(300.0, 3.0 * height + 5.0),
            size: Size::new(600.0, 400.0),
        });
        assert_eq!(swept_names(&handler).len(), 4);

        // The wheel turns with the button held: the list reports its scroll.
        let scrolled: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let list = ui.find(handler.list.clone()).unwrap().bounds();
            ui.point_at(list.center());
            let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                delta: iced::mouse::ScrollDelta::Pixels {
                    x: 0.0,
                    y: -10.0 * height,
                },
            })]);
            ui.into_messages()
                .filter(|message| matches!(message, Message::Scrolled(_)))
                .collect()
        };
        assert!(!scrolled.is_empty());
        feed(&mut handler, scrolled);

        assert_eq!(swept_names(&handler).len(), 14, "rows 0 to 13");
    }

    #[test]
    fn the_box_is_drawn_once_it_is_more_than_a_click_and_stays_put_when_scrolled() {
        let mut handler = folder_of(5_000, dto::ViewMode::List, 0.0);
        let _ = handler.update(Message::PressedAt {
            at: Point::new(100.0, 50.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        assert_eq!(handler.lasso_box(), None, "a click draws nothing");

        let _ = handler.update(Message::LassoMoved {
            at: Point::new(150.0, 130.0),
            size: Size::new(600.0, 400.0),
        });
        assert_eq!(
            handler.lasso_box(),
            Some(Rectangle {
                x: 100.0,
                y: 50.0,
                width: 50.0,
                height: 80.0
            })
        );

        // The list scrolls 30 px under the pointer: the box's start goes with it.
        handler.viewport.0 = 30.0;
        assert_eq!(
            handler.lasso_box(),
            Some(Rectangle {
                x: 100.0,
                y: 20.0,
                width: 50.0,
                height: 110.0
            })
        );
    }

    #[test]
    fn a_lasso_held_below_the_list_scrolls_it_and_takes_in_what_comes_into_view() {
        let mut handler = folder_of(5_000, dto::ViewMode::List, 0.0);
        let height = handler.row_height();
        let size = Size::new(600.0, 400.0);
        let _ = handler.update(Message::PressedAt {
            at: Point::new(100.0, 5.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(100.0, 395.0),
            size,
        });
        assert_eq!(handler.lasso_step(), None, "inside the list it stays");
        let inside = swept_names(&handler).len();

        let _ = handler.update(Message::LassoMoved {
            at: Point::new(100.0, 460.0),
            size,
        });
        let step = handler.lasso_step().unwrap();
        assert!(step > 0.0 && step <= LASSO_STEP, "{step}");
        let scrolled = handler.update(Message::LassoTick);
        assert!(scrolled.units() > 0, "the list is scrolled");
        assert!((handler.viewport.0 - step).abs() < 0.01);
        assert!(swept_names(&handler).len() > inside, "more rows in the box");
        assert_eq!(
            swept_names(&handler).len(),
            ((460.0 + step) / height).floor() as usize + 1
        );

        // Far past the edge, it goes no faster than the most.
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(100.0, 5_000.0),
            size,
        });
        assert_eq!(handler.lasso_step(), Some(LASSO_STEP));

        // Above the top, at the top already: nowhere to go.
        handler.viewport.0 = 0.0;
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(100.0, -40.0),
            size,
        });
        assert_eq!(handler.lasso_step(), None);
        assert_eq!(handler.update(Message::LassoTick).units(), 0);

        // At the bottom of the list, below it: nowhere to go either.
        handler.viewport.0 = 5_000.0 * height - 400.0;
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(100.0, 460.0),
            size,
        });
        assert_eq!(handler.lasso_step(), None);
        request(&mut handler, Request::PointerReleased);
        assert_eq!(handler.lasso_step(), None, "no lasso, no scrolling");
    }

    #[test]
    fn a_lasso_held_below_a_narrow_grid_scrolls_it_by_the_columns_it_has() {
        // Two columns of 40 tiles are 20 rows; before the grid has scrolled
        // the pane has not said its width, and four columns (10 rows, which
        // would fit) must not be assumed.
        let mut handler = folder_of(40, dto::ViewMode::Grid, 0.0);
        let size = Size::new(
            2.0 * (grid::TILE_WIDTH + grid::SPACING),
            10.0 * grid::Tile::default().row_height(),
        );
        let _ = handler.update(Message::PressedAt {
            at: Point::new(20.0, 20.0),
            size,
            taken: false,
        });
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(20.0, size.height + 30.0),
            size,
        });

        assert!(handler.lasso_step().is_some());
    }

    #[test]
    fn a_double_click_beside_a_name_opens_the_folder_and_draws_no_box() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let beside = {
            let mut ui = iced_test::simulator(handler.view());
            let name = ui.find("DCIM").unwrap().visible_bounds().unwrap();
            Point::new(name.x + name.width + 40.0, name.center_y())
        };
        let press =
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left));
        let mut ui = iced_test::simulator(handler.view());
        ui.point_at(beside);
        let _ = ui.simulate([press.clone()]);
        let first: Vec<Message> = ui.into_messages().collect();
        feed(&mut handler, first);
        // The app tells the pane the button came up between the clicks.
        request(&mut handler, Request::PointerReleased);
        let mut ui = iced_test::simulator(handler.view());
        ui.point_at(beside);
        let _ = ui.simulate([press.clone(), press]);
        let second: Vec<Message> = ui.into_messages().collect();
        assert!(
            second.iter().any(
                |message| matches!(message, Message::Request(Request::Open(name)) if name == "DCIM")
            ),
            "{second:?}"
        );
        feed(&mut handler, second);

        assert_eq!(handler.location().map(|(_, path)| path), Some("/DCIM"));
        assert!(handler.lasso.is_none(), "no box in the folder it opened");
    }

    #[test]
    fn a_lasso_over_the_grid_selects_the_tiles_it_touches() {
        let mut handler = folder_of(40, dto::ViewMode::Grid, 0.0);
        request(&mut handler, Request::Select("IMG_00000.jpg".into()));
        request(&mut handler, Request::PointerReleased);
        let size = Size::new(4.0 * (grid::TILE_WIDTH + grid::SPACING), 600.0);
        let pitch = grid::TILE_WIDTH + grid::SPACING;

        // In the gap after the first tile: nothing.
        let _ = handler.update(Message::PressedAt {
            at: Point::new(grid::TILE_WIDTH + 3.0, 20.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        assert!(swept_names(&handler).is_empty());
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(pitch + 10.0, grid::Tile::default().row_height() + 10.0),
            size,
        });

        // From the gap after tile 0 to inside tile 5: tiles 1 and 5, not 0 or 4.
        assert_eq!(swept_names(&handler), ["IMG_00001.jpg", "IMG_00005.jpg"]);
    }

    #[test]
    fn a_lasso_starting_moves_no_row_under_the_pointer() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_status_bar(false);
        let before = rows_at(&handler, &["a.jpg", "DCIM", "Music"]);

        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, 5.0),
            size: Size::new(600.0, 400.0),
            taken: false,
        });
        let _ = handler.update(Message::LassoMoved {
            at: Point::new(320.0, 70.0),
            size: Size::new(600.0, 400.0),
        });

        assert_eq!(rows_at(&handler, &["a.jpg", "DCIM", "Music"]), before);
    }

    #[test]
    fn a_click_before_the_list_has_a_size_never_scrolls() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        assert_eq!(handler.viewport, (0.0, 0.0, 0.0), "nothing scrolled yet");

        let _ = handler.update(Message::PressedAt {
            at: Point::new(300.0, 40.0),
            size: Size::ZERO,
            taken: false,
        });

        assert!(handler.lasso.is_some());
        assert_eq!(handler.lasso_step(), None);
        assert_eq!(handler.update(Message::LassoTick).units(), 0);
    }

    // A drag from the other pane.

    #[test]
    fn a_drag_from_the_other_pane_lands_on_the_folder_it_is_over_or_the_folder_shown() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("DCIM".into()));
        request(&mut handler, Request::PointerReleased);
        assert_eq!(handler.pointer(), Pointer::Idle);

        let _ = handler.set_incoming(Some(3), false);
        assert_eq!(handler.pointer(), Pointer::Dragging, "rows say where it is");
        assert_eq!(handler.drop_into(), None, "not over this list yet");

        let _ = handler.update(Message::Hover(true));
        assert_eq!(
            handler.drop_into().as_deref(),
            Some("/"),
            "the folder shown"
        );
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        assert_eq!(handler.drop_into().as_deref(), Some("/DCIM"));
        let dcim = handler
            .rows
            .iter()
            .find(|row| row.name == "DCIM")
            .unwrap()
            .clone();
        assert!(
            handler.look(&dcim).target,
            "a target even though it is selected here"
        );
        assert_eq!(
            handler.incoming_hint().as_deref(),
            Some("Copy 3 items into “DCIM” · hold ⌘ to move")
        );
        handler.set_modifiers(Modifiers::COMMAND);
        assert_eq!(
            handler.incoming_hint().as_deref(),
            Some("Move 3 items into “DCIM”")
        );

        request(&mut handler, Request::PointerEntered("a.jpg".into()));
        assert_eq!(
            handler.drop_into().as_deref(),
            Some("/"),
            "a file is no folder"
        );
        request(&mut handler, Request::PointerLeft("a.jpg".into()));
        let _ = handler.update(Message::Hover(false));
        assert_eq!(handler.drop_into(), None);
        assert_eq!(handler.incoming_hint(), None);

        let _ = handler.set_incoming(None, false);
        assert_eq!(handler.pointer(), Pointer::Idle);
    }

    #[test]
    fn rows_moving_under_an_incoming_drag_forget_the_folder_it_was_over() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.set_incoming(Some(1), false);
        let _ = handler.update(Message::Hover(true));
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        assert_eq!(handler.drop_into().as_deref(), Some("/DCIM"));

        // A sort moves rows under a pointer that has not moved.
        request(&mut handler, Request::SortBy(dto::SortKey::Size));

        assert_eq!(
            handler.drop_into().as_deref(),
            Some("/"),
            "the folder shown"
        );
    }

    #[test]
    fn a_drag_from_the_other_pane_is_said_in_the_footer_and_moves_no_row() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        handler.set_status_bar(false);
        let before = rows_at(&handler, &["a.jpg", "DCIM", "Music"]);

        let _ = handler.set_incoming(Some(2), false);
        let _ = handler.update(Message::Hover(true));

        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Copy 2 items here · hold ⌘ to move").is_ok());
        drop(ui);
        assert_eq!(rows_at(&handler, &["a.jpg", "DCIM", "Music"]), before);
    }

    #[test]
    fn the_list_says_when_a_drag_from_the_other_pane_comes_in() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.set_incoming(Some(1), false);
        let list = {
            let mut ui = iced_test::simulator(handler.view());
            ui.find(handler.list.clone()).unwrap().bounds()
        };

        let mut ui = iced_test::simulator(handler.view());
        let inside = list.center();
        ui.point_at(inside);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: inside,
        })]);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Message::Hover(true))),
            "{messages:?}"
        );
        feed(&mut handler, messages);
        assert!(handler.drop_into().is_some());
    }

    // Spring-loaded folders: a drag held over a folder opens it.

    /// A beat of the drag's clock `after` the folder was first held over.
    fn spring_beat(handler: &mut Handler, after: Duration) -> Task<Message> {
        let since = handler.spring.as_ref().map(|(_, since)| *since).unwrap();
        handler.update(Message::SpringTick(since + after))
    }

    /// a.jpg dragged and held over DCIM long enough: DCIM is shown, holding
    /// Camera and x.jpg.
    fn sprung_into_dcim(service: Arc<FakeExplorer>) -> Handler {
        let mut handler = dragging_pane(service);
        drag(&mut handler, "a.jpg", Some("DCIM"));
        let _ = spring_beat(&mut handler, SPRING_DELAY);
        let _ = handler.update(listed("/DCIM", &[folder("Camera"), file("x.jpg")]));
        handler
    }

    #[test]
    fn a_drag_held_over_a_folder_opens_it_after_a_moment_and_goes_on() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        drag(&mut handler, "a.jpg", Some("DCIM"));
        assert!(handler.spring_pending(), "the app sends beats now");

        let _ = spring_beat(&mut handler, SPRING_DELAY - Duration::from_millis(1));
        assert_eq!(handler.current_path(), Some("/"), "not yet");

        let _ = spring_beat(&mut handler, SPRING_DELAY);
        assert_eq!(handler.current_path(), Some("/DCIM"));
        assert!(!handler.spring_pending());
        assert_eq!(handler.dragging(), Some(1), "still dragging");
        let dragged = handler.dragged().unwrap();
        assert_eq!(
            (dragged.path.as_str(), dragged.names),
            ("/", vec!["a.jpg".to_owned()]),
            "what it started with, though nothing is selected here"
        );
        assert!(handler.selection.is_empty());
        let _ = handler.update(listed("/DCIM", &[folder("Camera")]));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Move 1 item into “DCIM” · hold ⌥ to copy"),
            "let go here, it goes here"
        );
        let _ = handler.view();
    }

    #[test]
    fn only_a_folder_held_over_long_enough_opens() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        // A file.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("b.jpg"));
        assert!(!handler.spring_pending());
        // What is being dragged.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "DCIM", Some("DCIM"));
        assert!(!handler.spring_pending());
        // A folder passed over.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", Some("DCIM"));
        request(&mut handler, Request::PointerLeft("DCIM".into()));
        assert!(!handler.spring_pending());
        let _ = handler.update(Message::SpringTick(Instant::now() + SPRING_DELAY * 2));
        assert_eq!(handler.current_path(), Some("/"));
        // Another folder starts the clock again.
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        let first = handler.spring.clone().unwrap().1;
        request(&mut handler, Request::PointerEntered("Music".into()));
        let (over, since) = handler.spring.clone().unwrap();
        assert_eq!(over, "Music");
        assert!(since >= first);
        // Out of the list.
        let _ = handler.update(Message::Hover(false));
        assert!(!handler.spring_pending());
        // A press that is not a drag yet.
        let mut handler = dragging_pane(service);
        request(&mut handler, Request::Select("a.jpg".into()));
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        assert!(!handler.spring_pending());
    }

    #[tokio::test]
    async fn dropped_on_a_folder_inside_a_sprung_one_moves_what_was_dragged() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = sprung_into_dcim(service.clone());
        request(&mut handler, Request::PointerEntered("Camera".into()));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Move 1 item into “Camera” · hold ⌥ to copy")
        );

        let _ = release(&mut handler).await;

        assert_eq!(
            dropped(&service),
            [dto::PasteRequest {
                mode: dto::Paste::Move,
                from_storage: 1,
                from: "/".into(),
                names: vec!["a.jpg".into()],
                to_storage: 1,
                to: "/DCIM/Camera".into(),
            }]
        );
        assert_eq!(handler.current_path(), Some("/DCIM"), "stays where it went");
    }

    #[tokio::test]
    async fn let_go_in_a_sprung_folder_it_goes_there() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = sprung_into_dcim(service.clone());
        request(&mut handler, Request::PointerEntered("x.jpg".into()));

        let _ = release(&mut handler).await;

        assert_eq!(dropped(&service).len(), 1);
        assert_eq!(dropped(&service)[0].to, "/DCIM");
        assert_eq!(dropped(&service)[0].from, "/");
        assert_eq!(handler.current_path(), Some("/DCIM"));
    }

    #[tokio::test]
    async fn called_off_or_let_go_outside_the_folders_it_opened_close() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        // Escape.
        let mut handler = sprung_into_dcim(service.clone());
        request(&mut handler, Request::PointerEntered("Camera".into()));
        let _ = spring_beat(&mut handler, SPRING_DELAY);
        assert_eq!(handler.current_path(), Some("/DCIM/Camera"));
        request(&mut handler, Request::Escape);
        assert_eq!(handler.current_path(), Some("/"), "all the way back");
        assert_eq!(handler.dragging(), None);
        // Let go outside the list (the other pane, which the app handles).
        let mut handler = sprung_into_dcim(service.clone());
        let _ = handler.update(Message::Hover(false));
        assert_eq!(
            handler.drag_hint().as_deref(),
            Some("Dragging 1 item: drop on a folder · ⌥ copies · Esc cancels")
        );
        let _ = release(&mut handler).await;
        assert_eq!(handler.current_path(), Some("/"));
        assert!(dropped(&service).is_empty());
        // A drag that sprang nothing goes nowhere when let go on nothing.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", None);
        let _ = release(&mut handler).await;
        assert_eq!(handler.current_path(), Some("/"));
        assert!(dropped(&service).is_empty());
    }

    #[test]
    fn space_opens_the_folder_at_once() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.spring_now();
        assert_eq!(handler.current_path(), Some("/"), "over no folder");

        drag(&mut handler, "a.jpg", Some("Music"));
        let _ = handler.spring_now();

        assert_eq!(handler.current_path(), Some("/Music"));
        assert_eq!(handler.dragging(), Some(1));
    }

    #[test]
    fn a_drag_from_the_other_pane_springs_folders_open_too() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        // Selected here, but it isn't what the other pane is dragging.
        request(&mut handler, Request::Select("DCIM".into()));
        request(&mut handler, Request::PointerReleased);
        let _ = handler.set_incoming(Some(2), false);
        let _ = handler.update(Message::Hover(true));
        request(&mut handler, Request::PointerEntered("DCIM".into()));
        let _ = spring_beat(&mut handler, SPRING_DELAY);
        assert_eq!(handler.current_path(), Some("/DCIM"));
        assert_eq!(handler.drop_into().as_deref(), Some("/DCIM"));

        // Dropped here: the pane stays.
        let _ = handler.set_incoming(None, true);
        assert_eq!(handler.current_path(), Some("/DCIM"));

        // Called off (or let go elsewhere): back where it was.
        let mut handler = dragging_pane(service.clone());
        let _ = handler.set_incoming(Some(1), false);
        let _ = handler.update(Message::Hover(true));
        request(&mut handler, Request::PointerEntered("Music".into()));
        let _ = spring_beat(&mut handler, SPRING_DELAY);
        assert_eq!(handler.current_path(), Some("/Music"));
        let _ = handler.set_incoming(None, false);
        assert_eq!(handler.current_path(), Some("/"));

        // Dropped, but over the pane it came from.
        let mut handler = dragging_pane(service);
        let _ = handler.set_incoming(Some(1), false);
        let _ = handler.update(Message::Hover(true));
        request(&mut handler, Request::PointerEntered("Music".into()));
        let _ = spring_beat(&mut handler, SPRING_DELAY);
        let _ = handler.update(Message::Hover(false));
        let _ = handler.set_incoming(None, true);
        assert_eq!(handler.current_path(), Some("/"));
    }

    #[tokio::test]
    async fn the_sidebar_springs_a_folder_open_for_the_pane_that_drags() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        // No drag here: nothing opens.
        let mut handler = dragging_pane(service.clone());
        let _ = handler.spring_to("/Music".into());
        assert_eq!(handler.current_path(), Some("/"));

        // Its own drag, held over a sidebar place: the place opens and the
        // drag goes on with what it started with.
        drag(&mut handler, "a.jpg", None);
        let _ = handler.update(Message::Hover(false));
        let shown = handler.spring_to("/".into());
        assert!(
            crate::handler::testing::drain(shown).await.0.is_empty(),
            "shown already: not even read again"
        );
        let _ = handler.spring_to("/Music".into());
        assert_eq!(handler.current_path(), Some("/Music"));
        assert_eq!(handler.dragging(), Some(1));
        let _ = handler.update(listed("/Music", &[folder("Albums")]));
        // Then let go in the pane: it goes into the folder shown.
        let _ = handler.update(Message::Hover(true));
        let _ = release(&mut handler).await;
        assert_eq!(dropped(&service).len(), 1);
        assert_eq!(
            (
                dropped(&service)[0].from.as_str(),
                dropped(&service)[0].to.as_str()
            ),
            ("/", "/Music")
        );
        assert_eq!(handler.current_path(), Some("/Music"), "stays");

        // Let go anywhere else (the sidebar, which the app handles): back.
        let mut handler = dragging_pane(service.clone());
        drag(&mut handler, "a.jpg", None);
        let _ = handler.update(Message::Hover(false));
        let _ = handler.spring_to("/Music".into());
        let _ = release(&mut handler).await;
        assert_eq!(handler.current_path(), Some("/"));
        assert_eq!(dropped(&service).len(), 1, "nothing more");

        // The other pane's drag: opened here, and kept if dropped here.
        let mut handler = dragging_pane(service.clone());
        let _ = handler.set_incoming(Some(1), false);
        let _ = handler.spring_to("/DCIM".into());
        assert_eq!(handler.current_path(), Some("/DCIM"));
        let _ = handler.update(Message::Hover(true));
        assert_eq!(handler.drop_into().as_deref(), Some("/DCIM"));
        let _ = handler.set_incoming(None, true);
        assert_eq!(handler.current_path(), Some("/DCIM"));
        let _ = handler.view();
    }

    #[tokio::test]
    async fn a_paste_can_go_into_any_folder_of_the_volume() {
        let service = Arc::new(FakeExplorer::new(Ok(vec![])));
        let mut handler = dragging_pane(service.clone());
        let from = Selection {
            volume_id: 1,
            path: "/".into(),
            names: vec!["a.jpg".into()],
        };

        let task = handler.paste_into(dto::Paste::Copy, vec![from], "/Music/Albums".into());
        let _ = crate::handler::testing::drain(task).await;

        assert_eq!(dropped(&service).len(), 1);
        assert_eq!(dropped(&service)[0].to, "/Music/Albums");
        assert_eq!(dropped(&service)[0].mode, dto::Paste::Copy);
    }

    #[test]
    fn another_volume_forgets_where_a_drag_sprang_from() {
        let mut handler = sprung_into_dcim(Arc::new(FakeExplorer::new(Ok(vec![]))));
        let _ = handler.open(storage(), "/Music".into());
        let _ = handler.update(listed("/Music", &[file("song.mp3"), folder("Albums")]));
        // A later drag let go on nothing has nowhere of its own to go back to.
        drag(&mut handler, "song.mp3", None);
        request(&mut handler, Request::PointerReleased);
        assert_eq!(handler.current_path(), Some("/Music"));
        handler.close();
        assert!(!handler.spring_pending());
    }

    #[test]
    fn the_list_reports_the_pointer_leaving_it_during_a_drag_of_its_own() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        drag(&mut handler, "a.jpg", Some("DCIM"));
        let list = {
            let mut ui = iced_test::simulator(handler.view());
            ui.find(handler.list.clone()).unwrap().bounds()
        };

        let mut ui = iced_test::simulator(handler.view());
        let outside = iced::Point::new(list.x + list.width + 50.0, list.y - 50.0);
        ui.point_at(outside);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: outside,
        })]);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Message::Hover(false))),
            "{messages:?}"
        );
        feed(&mut handler, messages);
        assert!(!handler.spring_pending());
    }

    #[test]
    fn only_a_started_drag_is_offered_to_the_other_pane() {
        let mut handler = dragging_pane(Arc::new(FakeExplorer::new(Ok(vec![]))));
        request(&mut handler, Request::Select("a.jpg".into()));
        assert_eq!(handler.dragging(), None, "a press is not a drag");
        request(&mut handler, Request::PointerLeft("a.jpg".into()));
        assert_eq!(handler.dragging(), Some(1));
        handler.set_other_pane(true);
        assert!(handler.drag_hint().unwrap().contains("the other pane"));
        request(&mut handler, Request::Escape);
        assert_eq!(handler.dragging(), None);
    }

    // Sizes of the selection, and what Get Info is about.

    #[test]
    fn the_footer_says_how_big_the_selected_files_are() {
        let mut handler = handler();
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed(
            "/",
            &[
                sized("a.jpg", 2_000_000),
                sized("b.jpg", 500_000),
                // A folder's size from the phone means nothing: never added.
                dto::Entry {
                    size_bytes: 9_000_000,
                    ..folder("DCIM")
                },
            ],
        ));

        request(&mut handler, Request::Select("a.jpg".into()));
        handler.set_modifiers(Modifiers::COMMAND);
        request(&mut handler, Request::Select("b.jpg".into()));
        assert!(
            iced_test::simulator(handler.view())
                .find("2 of 3 selected · 2.5 MB")
                .is_ok()
        );

        request(&mut handler, Request::Select("DCIM".into()));
        assert!(
            iced_test::simulator(handler.view())
                .find("3 of 3 selected · 2.5 MB + 1 folder · ⌘I to count")
                .is_ok()
        );
    }

    #[test]
    fn get_info_is_about_the_selected_entries_or_the_folder_itself() {
        let mut handler = handler();
        assert_eq!(handler.info_subject(), None, "nothing open");
        let _ = handler.open(storage(), "/".into());
        let _ = handler.update(listed("/", &[sized("a.jpg", 7), folder("DCIM")]));

        assert_eq!(handler.info_subject(), Some(("/".to_owned(), Vec::new())));
        request(&mut handler, Request::SelectAll);
        let (folder, items) = handler.info_subject().unwrap();
        assert_eq!(folder, "/");
        let mut names: Vec<_> = items
            .iter()
            .map(|item| (item.name.as_str(), item.size_bytes))
            .collect();
        names.sort();
        assert_eq!(
            names,
            [("DCIM", 0), ("a.jpg", 7)],
            "as the phone described them"
        );
    }
}
