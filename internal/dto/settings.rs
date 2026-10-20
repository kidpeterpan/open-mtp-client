use super::sort::SortOrder;

/// App preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub show_hidden_on_mac: bool,
    pub show_hidden_on_phone: bool,
    pub folders_first: bool,
    pub status_bar: bool,
    pub show_mac_pane: bool,
    pub mac_pane_on_left: bool,
    pub mac_view: ViewMode,
    /// Grid by default: a phone is mostly photos.
    pub phone_view: ViewMode,
    /// Connect when a phone is plugged in and let go when it is unplugged.
    /// On by default, as plugging in is the point.
    pub auto_connect: bool,
    /// Where an import puts each photo: a folder a day, month or year, or none.
    pub import_folders: super::ImportFolders,
    /// An import removes from the phone what it copied and checked.
    pub import_remove: bool,
    /// A sound when the transfer queue empties. Off unless asked for.
    pub finish_sound: bool,
    /// A phone delete keeps a copy of what it deletes in the Mac's Trash
    /// first (up to `KEEP_MOST`). Off unless asked for.
    pub keep_deleted: bool,
    /// How tall the rows of a list are, in both panes.
    pub list_density: Density,
    pub language: super::LanguageChoice,
    /// The version of the app that last ran: after an update, what is new
    /// since then is shown once. `None` before the first run.
    pub last_seen_version: Option<Version>,
    /// Where the window was and how big, to open it there again. `None`
    /// until it has been placed once.
    pub window: Option<WindowPlace>,
    /// How each pane is sorted.
    pub mac_order: SortOrder,
    pub phone_order: SortOrder,
    /// Each pane shows its folders' sizes (counted in the background).
    pub mac_folder_sizes: bool,
    pub phone_folder_sizes: bool,
    /// What each pane's rows are grouped by (View › Use Groups).
    pub mac_group: super::GroupBy,
    pub phone_group: super::GroupBy,
    /// How wide each pane's list columns are.
    pub mac_columns: ColumnWidths,
    pub phone_columns: ColumnWidths,
    /// How big each pane's grid tiles are.
    pub mac_tiles: TileSize,
    pub phone_tiles: TileSize,
}

/// How big a grid's tiles are: one of a few steps, so a tile's thumbnail is
/// always made at the largest and only ever shrunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileSize(u8);

impl TileSize {
    /// The steps, in points across a tile.
    pub const STEPS: [u16; 5] = [96, 112, 132, 160, 192];

    /// A tile's width in points.
    pub fn points(self) -> u16 {
        Self::STEPS[usize::from(self.0)]
    }

    /// One step bigger, or the same at the biggest.
    pub fn bigger(self) -> TileSize {
        TileSize((self.0 + 1).min(Self::STEPS.len() as u8 - 1))
    }

    /// One step smaller, or the same at the smallest.
    pub fn smaller(self) -> TileSize {
        TileSize(self.0.saturating_sub(1))
    }

    pub fn is_biggest(self) -> bool {
        usize::from(self.0) == Self::STEPS.len() - 1
    }

    pub fn is_smallest(self) -> bool {
        self.0 == 0
    }

    /// The step that is `points` wide; anything else (a hand-edited file)
    /// is the usual size.
    pub fn from_points(points: u16) -> TileSize {
        Self::STEPS
            .iter()
            .position(|&step| step == points)
            .map_or_else(TileSize::default, |at| TileSize(at as u8))
    }
}

impl Default for TileSize {
    /// 132 points, the size tiles always had.
    fn default() -> Self {
        TileSize(2)
    }
}

/// The widths of a list's columns, in points; Name takes what is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnWidths {
    pub kind: u16,
    pub size: u16,
    pub modified: u16,
}

impl ColumnWidths {
    /// The narrowest and widest a column may be made.
    pub const LEAST: u16 = 50;
    pub const MOST: u16 = 600;
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self {
            kind: 120,
            size: 90,
            modified: 170,
        }
    }
}

/// The window's place on the screens, in points from the top left of the
/// main screen, as macOS lays windows out; and whether it was full screen
/// (its place is then the one it goes back to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
}

/// A screen's bounds, in the same points as `WindowPlace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// An app version, `major.minor.patch`, ordered as versions are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Reads "1.2.3"; anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let mut number = || parts.next()?.parse::<u32>().ok();
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
        };
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Auto,
            show_hidden_on_mac: false,
            show_hidden_on_phone: false,
            folders_first: true,
            status_bar: true,
            show_mac_pane: true,
            mac_pane_on_left: true,
            auto_connect: true,
            mac_view: ViewMode::List,
            phone_view: ViewMode::Grid,
            import_folders: super::ImportFolders::None,
            import_remove: false,
            finish_sound: false,
            keep_deleted: false,
            list_density: Density::Standard,
            language: super::LanguageChoice::FollowMac,
            last_seen_version: None,
            window: None,
            mac_order: SortOrder::default(),
            phone_order: SortOrder::default(),
            mac_columns: ColumnWidths::default(),
            phone_columns: ColumnWidths::default(),
            mac_folder_sizes: false,
            phone_folder_sizes: false,
            mac_group: super::GroupBy::None,
            phone_group: super::GroupBy::None,
            mac_tiles: TileSize::default(),
            phone_tiles: TileSize::default(),
        }
    }
}

/// How a pane shows its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    List,
    /// Tiles with thumbnails.
    Grid,
}

/// How much room a list gives each row. Compact is Finder's own list;
/// Standard is a little roomier, and Large is for a far-off screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    Compact,
    #[default]
    Standard,
    Large,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Follow macOS's light or dark appearance.
    #[default]
    Auto,
    Light,
    Dark,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_read_and_order_as_numbers_not_text() {
        let v = |text| Version::parse(text).unwrap();

        assert!(v("0.10.0") > v("0.9.3"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v(" 2.0.1 ").to_string(), "2.0.1");
        for odd in ["", "1", "1.2", "1.2.3.4", "1.x.3", "-1.2.3"] {
            assert_eq!(Version::parse(odd), None, "{odd:?}");
        }
    }
}

/// Where each pane was left, to open there again: the Mac pane's folder, and
/// for each phone (its `Phone::key`) and storage, the folder its pane showed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastFolders {
    pub mac: Option<String>,
    /// Newest first, at most `MOST_PHONE_FOLDERS`.
    pub phones: Vec<PhoneFolder>,
    /// The Mac folders shown lately, newest first, at most `MOST_RECENT`.
    pub recent_mac: Vec<String>,
    /// The phone folders shown lately, newest first: at most `MOST_RECENT`
    /// for each phone, and the `MOST_RECENT_PHONES` phones seen last.
    pub recent_phone: Vec<PhoneFolder>,
}

/// The folder a phone's pane showed last on one of its storages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneFolder {
    pub phone: String,
    pub storage_id: u64,
    pub path: String,
}

/// How many phone storages' folders are kept.
pub const MOST_PHONE_FOLDERS: usize = 20;

/// How many recent folders are kept for the Mac, and for each phone.
pub const MOST_RECENT: usize = 10;

/// How many phones' recent folders are kept.
pub const MOST_RECENT_PHONES: usize = 10;

impl LastFolders {
    /// The folder last shown on `phone`'s storage `storage_id`.
    pub fn phone(&self, phone: &str, storage_id: u64) -> Option<&str> {
        self.phones
            .iter()
            .find(|kept| kept.phone == phone && kept.storage_id == storage_id)
            .map(|kept| kept.path.as_str())
    }

    /// Keeps `path` as the Mac pane's folder, and first of the recent ones;
    /// says whether anything changed.
    pub fn keep_mac(&mut self, path: &str) -> bool {
        if self.mac.as_deref() == Some(path) {
            return false;
        }
        self.mac = Some(path.to_owned());
        self.recent_mac.retain(|kept| kept != path);
        self.recent_mac.insert(0, path.to_owned());
        self.recent_mac.truncate(MOST_RECENT);
        true
    }

    /// The recent folders on `phone`, newest first.
    pub fn recent_on<'a>(&'a self, phone: &'a str) -> impl Iterator<Item = &'a PhoneFolder> {
        self.recent_phone
            .iter()
            .filter(move |kept| kept.phone == phone)
    }

    /// Forgets the recent folders (Go › Recent Folders › Clear Menu); the
    /// folders each pane opens at stay. Says whether there were any.
    pub fn clear_recent(&mut self) -> bool {
        let had = !self.recent_mac.is_empty() || !self.recent_phone.is_empty();
        self.recent_mac.clear();
        self.recent_phone.clear();
        had
    }

    /// Keeps `path` as the folder last shown on that phone's storage, first,
    /// and first of that phone's recent ones; says whether anything changed.
    pub fn keep_phone(&mut self, phone: &str, storage_id: u64, path: &str) -> bool {
        if self.phones.first().is_some_and(|kept| {
            kept.phone == phone && kept.storage_id == storage_id && kept.path == path
        }) {
            return false;
        }
        self.keep_recent_phone(phone, storage_id, path);
        self.phones
            .retain(|kept| !(kept.phone == phone && kept.storage_id == storage_id));
        self.phones.insert(
            0,
            PhoneFolder {
                phone: phone.to_owned(),
                storage_id,
                path: path.to_owned(),
            },
        );
        self.phones.truncate(MOST_PHONE_FOLDERS);
        true
    }

    fn keep_recent_phone(&mut self, phone: &str, storage_id: u64, path: &str) {
        let here = PhoneFolder {
            phone: phone.to_owned(),
            storage_id,
            path: path.to_owned(),
        };
        self.recent_phone.retain(|kept| *kept != here);
        self.recent_phone.insert(0, here);
        self.recent_phone = limit_recent(std::mem::take(&mut self.recent_phone));
    }
}

/// At most `MOST_RECENT` folders for each phone, from the `MOST_RECENT_PHONES`
/// phones met first (newest first), in the order given.
pub fn limit_recent(folders: Vec<PhoneFolder>) -> Vec<PhoneFolder> {
    let mut phones: Vec<(String, usize)> = Vec::new();
    folders
        .into_iter()
        .filter(|kept| {
            if let Some((_, count)) = phones.iter_mut().find(|(phone, _)| *phone == kept.phone) {
                *count += 1;
                return *count <= MOST_RECENT;
            }
            if phones.len() < MOST_RECENT_PHONES {
                phones.push((kept.phone.clone(), 1));
                return true;
            }
            false
        })
        .collect()
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    #[test]
    fn each_phone_storage_keeps_its_own_folder_newest_first() {
        let mut folders = LastFolders::default();
        assert!(folders.keep_phone("pixel", 1, "/DCIM"));
        assert!(folders.keep_phone("pixel", 2, "/Music"));
        assert!(folders.keep_phone("galaxy", 1, "/Download"));
        assert!(
            !folders.keep_phone("galaxy", 1, "/Download"),
            "the same again: no change"
        );
        assert!(folders.keep_phone("pixel", 1, "/DCIM/Camera"));

        assert_eq!(folders.phone("pixel", 1), Some("/DCIM/Camera"));
        assert_eq!(folders.phone("pixel", 2), Some("/Music"));
        assert_eq!(folders.phone("galaxy", 1), Some("/Download"));
        assert_eq!(folders.phone("galaxy", 2), None);
        assert_eq!(folders.phones[0].path, "/DCIM/Camera", "newest first");
        assert_eq!(folders.phones.len(), 3, "one entry per phone and storage");
    }

    #[test]
    fn the_folders_shown_lately_are_kept_newest_first_once_each() {
        let mut folders = LastFolders::default();
        assert!(folders.keep_mac("/Users/me"));
        assert!(folders.keep_mac("/Users/me/Desktop"));
        assert!(!folders.keep_mac("/Users/me/Desktop"), "the same again");
        assert!(folders.keep_mac("/Users/me"));
        assert_eq!(folders.mac.as_deref(), Some("/Users/me"));
        assert_eq!(folders.recent_mac, ["/Users/me", "/Users/me/Desktop"]);

        folders.keep_phone("pixel", 1, "/DCIM");
        folders.keep_phone("pixel", 2, "/Music");
        folders.keep_phone("galaxy", 1, "/Download");
        folders.keep_phone("pixel", 1, "/DCIM/Camera");
        folders.keep_phone("pixel", 1, "/DCIM");
        let pixel: Vec<(u64, &str)> = folders
            .recent_on("pixel")
            .map(|kept| (kept.storage_id, kept.path.as_str()))
            .collect();
        assert_eq!(pixel, [(1, "/DCIM"), (1, "/DCIM/Camera"), (2, "/Music")]);
        assert_eq!(folders.recent_on("galaxy").count(), 1);

        for n in 0..(MOST_RECENT + 3) {
            folders.keep_mac(&format!("/f{n}"));
            folders.keep_phone("pixel", 1, &format!("/p{n}"));
        }
        assert_eq!(folders.recent_mac.len(), MOST_RECENT);
        assert_eq!(folders.recent_mac[0], format!("/f{}", MOST_RECENT + 2));
        assert_eq!(folders.recent_on("pixel").count(), MOST_RECENT);
        assert_eq!(
            folders.recent_on("galaxy").count(),
            1,
            "another phone's are not pushed out"
        );

        assert!(folders.clear_recent());
        assert!(!folders.clear_recent(), "nothing left");
        assert!(folders.recent_mac.is_empty() && folders.recent_phone.is_empty());
        assert_eq!(
            folders.mac.as_deref(),
            Some(format!("/f{}", MOST_RECENT + 2).as_str())
        );
        assert!(
            folders.phone("pixel", 1).is_some(),
            "where panes open stays"
        );
    }

    #[test]
    fn only_the_phones_seen_last_keep_recent_folders() {
        let mut folders = LastFolders::default();
        for n in 0..(MOST_RECENT_PHONES + 2) {
            folders.keep_phone(&format!("phone{n}"), 1, "/DCIM");
        }
        assert_eq!(folders.recent_phone.len(), MOST_RECENT_PHONES);
        assert_eq!(folders.recent_on("phone0").count(), 0, "seen first, gone");
        assert_eq!(
            folders
                .recent_on(&format!("phone{}", MOST_RECENT_PHONES + 1))
                .count(),
            1
        );
    }

    #[test]
    fn only_the_most_recent_phone_folders_are_kept() {
        let mut folders = LastFolders::default();
        for storage in 0..(MOST_PHONE_FOLDERS as u64 + 5) {
            folders.keep_phone("pixel", storage, "/");
        }
        assert_eq!(folders.phones.len(), MOST_PHONE_FOLDERS);
        assert_eq!(folders.phone("pixel", 0), None, "the oldest fell off");
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;

    #[test]
    fn tiles_step_between_the_smallest_and_the_biggest() {
        let usual = TileSize::default();
        assert_eq!(usual.points(), 132);
        assert_eq!(usual.bigger().points(), 160);
        assert_eq!(usual.smaller().points(), 112);
        let biggest = usual.bigger().bigger().bigger();
        assert_eq!(biggest.points(), 192);
        assert!(biggest.is_biggest() && !biggest.is_smallest());
        let smallest = usual.smaller().smaller().smaller();
        assert_eq!(smallest.points(), 96);
        assert!(smallest.is_smallest() && !smallest.is_biggest());
        assert_eq!(TileSize::from_points(160), usual.bigger());
        assert_eq!(TileSize::from_points(150), usual, "not a step: the usual");
        assert_eq!(TileSize::from_points(0), usual);
    }
}
