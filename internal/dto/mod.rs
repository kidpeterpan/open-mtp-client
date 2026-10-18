//! Business entities and errors. Plain data only: no serde, no UI or protocol types.

mod check;
mod crash;
mod device;
mod doctor;
mod editing;
mod entry;
mod error;
mod import;
mod listing;
mod menu;
mod mirror;
mod paste;
mod places;
mod preview;
mod queue;
mod rename;
mod search;
mod settings;
mod sort;
mod sound;
mod space;
mod storage;
mod tally;
mod transfer;

pub use check::{CheckRequest, Compared};
pub use crash::CrashNote;
pub use device::{Abilities, ContentChange, Device, DeviceChange, Phone, UsbSpeed};
pub use doctor::{Diagnosis, Holder, MacInfo};
pub use editing::{CopyState, EditRequest, MOST_EDITS, WorkingCopy};
pub use entry::{Entry, THUMBNAIL_SIZE, Thumbnail, Timestamp};
pub use error::{Error, NameProblem, PrivacyPane};
pub use import::{ImportFolders, ImportPreview, ImportRequest};
pub use listing::{ListingUpdate, ReadTiming};
pub use menu::{Menu, MenuEntry, MenuKeys, MenuRole, MenuState, SystemAction};
pub use mirror::{
    MIRROR_SAMPLE, MirrorDirection, MirrorPreview, MirrorRequest, MirrorSamples, Mirrored,
};
pub use paste::{Paste, PasteRequest, PasteSummary};
pub use places::{
    DiskSpace, MOST_MIRRORS, MOST_PINS, MOST_VOLUMES, Pin, PinPhone, PinSide, Place, SavedMirror,
};
pub use preview::{Fetched, Look, PREVIEW_CACHE, PREVIEW_LARGEST, PREVIEW_MOST, PreviewRequest};
pub use queue::{SavedAnswer, SavedJob, SavedTransfer};
pub use rename::{RenamePattern, RenameProblem, Renaming};
pub use search::{DateRange, Day, DaySpan, FileKind, Found, Matcher, SearchQuery, WalkUpdate};
pub use settings::{
    ColumnWidths, Density, LastFolders, MOST_PHONE_FOLDERS, MOST_RECENT, MOST_RECENT_PHONES,
    PhoneFolder, Screen, Settings, ThemeChoice, TileSize, Version, ViewMode, WindowPlace,
    limit_recent,
};
pub use sort::{GroupBy, Sort, SortKey, SortOrder};
pub use sound::Sound;
pub use space::{DuplicateGroup, FreeUpRequest, SpaceFile, SpaceProgress, SpaceReport};
pub use storage::Storage;
pub use tally::{SpaceKind, Tally};
pub use transfer::{
    Arrival, Cancel, Conflict, DeleteRequest, Direction, DownloadRequest, FileFailure, FileOutcome,
    FileReport, KEEP_MOST, Kept, Original, REPORT_MOST, Removal, TransferProgress, TransferSummary,
    UploadRequest, Why,
};
