//! The settings file format. Kept apart from `dto::Settings` so the entity
//! stays free of serde, and so the file can change shape without the app.

use serde::{Deserialize, Serialize};

use crate::dto::{
    ColumnWidths, Density, Language, LanguageChoice, LastFolders, MOST_PHONE_FOLDERS, MOST_RECENT,
    PhoneFolder, Settings, SortKey, SortOrder, ThemeChoice, TileSize, Version, ViewMode,
    WindowPlace, limit_recent,
};

/// Where the panes were left, as stored in `folders.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FoldersFile {
    mac: Option<String>,
    phones: Vec<PhoneFolderFile>,
    recent_mac: Vec<String>,
    recent_phone: Vec<PhoneFolderFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneFolderFile {
    phone: String,
    storage_id: u64,
    path: String,
}

fn to_phone_folder_files(folders: &[PhoneFolder]) -> Vec<PhoneFolderFile> {
    folders
        .iter()
        .map(|kept| PhoneFolderFile {
            phone: kept.phone.clone(),
            storage_id: kept.storage_id,
            path: kept.path.clone(),
        })
        .collect()
}

pub fn to_folders_file(folders: &LastFolders) -> FoldersFile {
    FoldersFile {
        mac: folders.mac.clone(),
        phones: to_phone_folder_files(&folders.phones),
        recent_mac: folders.recent_mac.clone(),
        recent_phone: to_phone_folder_files(&folders.recent_phone),
    }
}

/// The phone folders in `files` that name an absolute path on a phone.
fn to_phone_folders(files: &[PhoneFolderFile]) -> impl Iterator<Item = PhoneFolder> + '_ {
    files
        .iter()
        .filter(|kept| kept.path.starts_with('/') && !kept.phone.is_empty())
        .map(|kept| PhoneFolder {
            phone: kept.phone.clone(),
            storage_id: kept.storage_id,
            path: kept.path.clone(),
        })
}

/// Only absolute paths are kept; a hand-edited file can't hold more than the
/// most there may be.
pub fn to_folders(file: &FoldersFile) -> LastFolders {
    let absolute = |path: &str| path.starts_with('/');
    LastFolders {
        mac: file.mac.clone().filter(|path| absolute(path)),
        phones: to_phone_folders(&file.phones)
            .take(MOST_PHONE_FOLDERS)
            .collect(),
        recent_mac: file
            .recent_mac
            .iter()
            .filter(|path| absolute(path))
            .take(MOST_RECENT)
            .cloned()
            .collect(),
        recent_phone: limit_recent(to_phone_folders(&file.recent_phone).collect()),
    }
}

/// Bumped when a field changes meaning.
const VERSION: u32 = 1;

/// Every field has a default, so a file from an older or newer version
/// still loads: missing fields fall back, unknown ones are ignored.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingsFile {
    version: u32,
    theme: String,
    show_hidden_on_mac: bool,
    show_hidden_on_phone: bool,
    folders_first: bool,
    status_bar: bool,
    show_mac_pane: bool,
    mac_pane_on_left: bool,
    auto_connect: bool,
    mac_view: String,
    phone_view: String,
    /// Read from files written before `import_folders` (true: a folder a day).
    import_by_date: bool,
    /// Missing (a file from before it) is empty, so `import_by_date` decides.
    #[serde(default)]
    import_folders: String,
    import_remove: bool,
    finish_sound: bool,
    keep_deleted: bool,
    mac_folder_sizes: bool,
    phone_folder_sizes: bool,
    mac_group: String,
    phone_group: String,
    list_density: String,
    language: String,
    /// "1.2.3", or empty before the first run.
    last_seen_version: String,
    /// Missing until the window has been placed once.
    window: Option<WindowFile>,
    mac_sort: String,
    mac_sort_descending: bool,
    phone_sort: String,
    phone_sort_descending: bool,
    /// Missing until a column has been made wider or narrower.
    mac_columns: Option<ColumnsFile>,
    phone_columns: Option<ColumnsFile>,
    /// A grid tile's width in points; 0 (missing) is the usual size.
    mac_tiles: u16,
    phone_tiles: u16,
}

/// A list's column widths, as stored.
#[derive(Debug, Serialize, Deserialize)]
pub struct ColumnsFile {
    kind: u16,
    size: u16,
    modified: u16,
}

fn to_columns_file(widths: ColumnWidths) -> Option<ColumnsFile> {
    (widths != ColumnWidths::default()).then_some(ColumnsFile {
        kind: widths.kind,
        size: widths.size,
        modified: widths.modified,
    })
}

/// A stored width outside what a column may be is brought inside it.
fn to_columns(file: Option<&ColumnsFile>) -> ColumnWidths {
    let Some(file) = file else {
        return ColumnWidths::default();
    };
    let fit = |width: u16| width.clamp(ColumnWidths::LEAST, ColumnWidths::MOST);
    ColumnWidths {
        kind: fit(file.kind),
        size: fit(file.size),
        modified: fit(file.modified),
    }
}

/// The window's place, as stored.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowFile {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    #[serde(default)]
    fullscreen: bool,
}

/// Smaller than this, a stored window is a damaged or hand-edited file, not a
/// window anyone used: it is forgotten and the window opens as on a first run.
const SMALLEST_WINDOW: u32 = 200;
/// Larger than any screen there is.
const LARGEST_WINDOW: u32 = 20_000;

impl Default for SettingsFile {
    fn default() -> Self {
        to_file(&Settings::default())
    }
}

/// An import's folders as the files write them.
pub(super) fn folders_name(folders: crate::dto::ImportFolders) -> &'static str {
    match folders {
        crate::dto::ImportFolders::None => "none",
        crate::dto::ImportFolders::Day => "day",
        crate::dto::ImportFolders::Month => "month",
        crate::dto::ImportFolders::YearMonth => "yearMonth",
    }
}

/// An import's folders as a file says them; a file from before they had a
/// name says only whether there was a folder a day (`by_date`).
pub(super) fn folders_of(name: &str, by_date: bool) -> crate::dto::ImportFolders {
    match name {
        "day" => crate::dto::ImportFolders::Day,
        "month" => crate::dto::ImportFolders::Month,
        "yearMonth" => crate::dto::ImportFolders::YearMonth,
        "none" => crate::dto::ImportFolders::None,
        _ if by_date => crate::dto::ImportFolders::Day,
        _ => crate::dto::ImportFolders::None,
    }
}

/// A pane's grouping as the file writes it.
fn group_name(by: crate::dto::GroupBy) -> &'static str {
    match by {
        crate::dto::GroupBy::None => "none",
        crate::dto::GroupBy::Date => "date",
        crate::dto::GroupBy::Kind => "kind",
        crate::dto::GroupBy::Size => "size",
    }
}

/// A pane's grouping as the file says it; anything else is none.
fn group_of(name: &str) -> crate::dto::GroupBy {
    match name {
        "date" => crate::dto::GroupBy::Date,
        "kind" => crate::dto::GroupBy::Kind,
        "size" => crate::dto::GroupBy::Size,
        _ => crate::dto::GroupBy::None,
    }
}

pub fn to_file(settings: &Settings) -> SettingsFile {
    SettingsFile {
        version: VERSION,
        theme: match settings.theme {
            ThemeChoice::Auto => "auto",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
        .to_owned(),
        show_hidden_on_mac: settings.show_hidden_on_mac,
        show_hidden_on_phone: settings.show_hidden_on_phone,
        folders_first: settings.folders_first,
        status_bar: settings.status_bar,
        show_mac_pane: settings.show_mac_pane,
        mac_pane_on_left: settings.mac_pane_on_left,
        auto_connect: settings.auto_connect,
        mac_view: view_name(settings.mac_view).to_owned(),
        phone_view: view_name(settings.phone_view).to_owned(),
        import_by_date: settings.import_folders != crate::dto::ImportFolders::None,
        import_folders: folders_name(settings.import_folders).to_owned(),
        import_remove: settings.import_remove,
        mac_folder_sizes: settings.mac_folder_sizes,
        mac_group: group_name(settings.mac_group).to_owned(),
        phone_group: group_name(settings.phone_group).to_owned(),
        phone_folder_sizes: settings.phone_folder_sizes,
        finish_sound: settings.finish_sound,
        keep_deleted: settings.keep_deleted,
        list_density: match settings.list_density {
            Density::Compact => "compact",
            Density::Standard => "standard",
            Density::Large => "large",
        }
        .to_owned(),
        language: match settings.language {
            LanguageChoice::FollowMac => "system",
            LanguageChoice::Chosen(language) => language.code(),
        }
        .to_owned(),
        last_seen_version: settings
            .last_seen_version
            .map(|version| version.to_string())
            .unwrap_or_default(),
        window: settings.window.map(|place| WindowFile {
            x: place.x,
            y: place.y,
            width: place.width,
            height: place.height,
            fullscreen: place.fullscreen,
        }),
        mac_sort: sort_name(settings.mac_order.key).to_owned(),
        mac_sort_descending: settings.mac_order.descending,
        phone_sort: sort_name(settings.phone_order.key).to_owned(),
        phone_sort_descending: settings.phone_order.descending,
        mac_columns: to_columns_file(settings.mac_columns),
        phone_columns: to_columns_file(settings.phone_columns),
        mac_tiles: settings.mac_tiles.points(),
        phone_tiles: settings.phone_tiles.points(),
    }
}

fn sort_name(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Size => "size",
        SortKey::Modified => "modified",
        SortKey::Kind => "kind",
    }
}

/// An unknown or missing column sorts by name.
fn to_order(name: &str, descending: bool) -> SortOrder {
    let key = match name {
        "size" => SortKey::Size,
        "modified" => SortKey::Modified,
        "kind" => SortKey::Kind,
        _ => SortKey::Name,
    };
    SortOrder { key, descending }
}

/// A stored window too small or too large to be real is forgotten.
fn to_place(file: &WindowFile) -> Option<WindowPlace> {
    let fits = |side: u32| (SMALLEST_WINDOW..=LARGEST_WINDOW).contains(&side);
    (fits(file.width) && fits(file.height)).then_some(WindowPlace {
        x: file.x,
        y: file.y,
        width: file.width,
        height: file.height,
        fullscreen: file.fullscreen,
    })
}

pub fn to_settings(file: &SettingsFile) -> Settings {
    Settings {
        theme: match file.theme.as_str() {
            "light" => ThemeChoice::Light,
            "dark" => ThemeChoice::Dark,
            _ => ThemeChoice::Auto,
        },
        show_hidden_on_mac: file.show_hidden_on_mac,
        show_hidden_on_phone: file.show_hidden_on_phone,
        folders_first: file.folders_first,
        status_bar: file.status_bar,
        show_mac_pane: file.show_mac_pane,
        mac_pane_on_left: file.mac_pane_on_left,
        auto_connect: file.auto_connect,
        mac_view: to_view(&file.mac_view, ViewMode::List),
        phone_view: to_view(&file.phone_view, ViewMode::Grid),
        import_folders: folders_of(&file.import_folders, file.import_by_date),
        import_remove: file.import_remove,
        mac_folder_sizes: file.mac_folder_sizes,
        mac_group: group_of(&file.mac_group),
        phone_group: group_of(&file.phone_group),
        phone_folder_sizes: file.phone_folder_sizes,
        finish_sound: file.finish_sound,
        keep_deleted: file.keep_deleted,
        list_density: match file.list_density.as_str() {
            "compact" => Density::Compact,
            "large" => Density::Large,
            _ => Density::Standard,
        },
        language: Language::from_code(&file.language)
            .map_or(LanguageChoice::FollowMac, LanguageChoice::Chosen),
        last_seen_version: Version::parse(&file.last_seen_version),
        window: file.window.as_ref().and_then(to_place),
        mac_order: to_order(&file.mac_sort, file.mac_sort_descending),
        phone_order: to_order(&file.phone_sort, file.phone_sort_descending),
        mac_columns: to_columns(file.mac_columns.as_ref()),
        phone_columns: to_columns(file.phone_columns.as_ref()),
        mac_tiles: TileSize::from_points(file.mac_tiles),
        phone_tiles: TileSize::from_points(file.phone_tiles),
    }
}

fn view_name(view: ViewMode) -> &'static str {
    match view {
        ViewMode::List => "list",
        ViewMode::Grid => "grid",
    }
}

/// An unknown or missing view keeps that pane's default.
fn to_view(name: &str, default: ViewMode) -> ViewMode {
    match name {
        "list" => ViewMode::List,
        "grid" => ViewMode::Grid,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_panes_grouping_is_kept_by_name_and_anything_else_is_none() {
        let settings = Settings {
            mac_group: crate::dto::GroupBy::Kind,
            phone_group: crate::dto::GroupBy::Date,
            ..Settings::default()
        };
        let json = serde_json::to_string(&to_file(&settings)).unwrap();
        assert!(json.contains("\"macGroup\":\"kind\""), "{json}");
        assert!(json.contains("\"phoneGroup\":\"date\""), "{json}");
        let back = to_settings(&serde_json::from_str(&json).unwrap());
        assert_eq!(
            (back.mac_group, back.phone_group),
            (crate::dto::GroupBy::Kind, crate::dto::GroupBy::Date)
        );
        let size =
            to_settings(&serde_json::from_str(&json.replace("\"kind\"", "\"size\"")).unwrap());
        assert_eq!(size.mac_group, crate::dto::GroupBy::Size);
        let odd =
            to_settings(&serde_json::from_str(&json.replace("\"kind\"", "\"colour\"")).unwrap());
        assert_eq!(odd.mac_group, crate::dto::GroupBy::None);
        assert_eq!(Settings::default().phone_group, crate::dto::GroupBy::None);
    }

    #[test]
    fn every_setting_round_trips() {
        let settings = Settings {
            theme: ThemeChoice::Dark,
            show_hidden_on_mac: true,
            show_hidden_on_phone: true,
            folders_first: false,
            status_bar: false,
            show_mac_pane: false,
            mac_pane_on_left: false,
            auto_connect: false,
            mac_view: ViewMode::Grid,
            phone_view: ViewMode::List,
            import_folders: crate::dto::ImportFolders::YearMonth,
            import_remove: true,
            finish_sound: true,
            keep_deleted: true,
            mac_group: crate::dto::GroupBy::Size,
            phone_group: crate::dto::GroupBy::Date,
            mac_folder_sizes: true,
            phone_folder_sizes: false,
            list_density: Density::Compact,
            language: LanguageChoice::Chosen(Language::Japanese),
            last_seen_version: Some(Version {
                major: 1,
                minor: 12,
                patch: 3,
            }),
            window: Some(WindowPlace {
                x: -1440,
                y: 38,
                width: 1280,
                height: 800,
                fullscreen: true,
            }),
            mac_order: SortOrder {
                key: SortKey::Modified,
                descending: true,
            },
            phone_order: SortOrder {
                key: SortKey::Size,
                descending: false,
            },
            mac_columns: ColumnWidths {
                kind: 80,
                size: 70,
                modified: 220,
            },
            phone_columns: ColumnWidths::default(),
            mac_tiles: TileSize::default(),
            phone_tiles: TileSize::default().bigger(),
        };

        assert_eq!(to_settings(&to_file(&settings)), settings);
        let large = Settings {
            list_density: Density::Large,
            ..settings
        };
        assert_eq!(to_settings(&to_file(&large)), large);
        let light = Settings {
            theme: ThemeChoice::Light,
            ..settings
        };
        assert_eq!(to_settings(&to_file(&light)), light);
    }

    #[test]
    fn an_old_folder_a_day_setting_is_read_as_day_and_each_layout_kept_by_name() {
        let old: SettingsFile =
            serde_json::from_str(r#"{"version": 1, "importByDate": true}"#).unwrap();
        assert_eq!(
            to_settings(&old).import_folders,
            crate::dto::ImportFolders::Day
        );
        for folders in [
            crate::dto::ImportFolders::None,
            crate::dto::ImportFolders::Day,
            crate::dto::ImportFolders::Month,
            crate::dto::ImportFolders::YearMonth,
        ] {
            let json = serde_json::to_string(&to_file(&Settings {
                import_folders: folders,
                ..Settings::default()
            }))
            .unwrap();
            assert_eq!(
                to_settings(&serde_json::from_str(&json).unwrap()).import_folders,
                folders,
                "{json}"
            );
        }
    }

    #[test]
    fn a_file_from_before_imports_imports_flat() {
        let file: SettingsFile =
            serde_json::from_str(r#"{"version": 1, "theme": "dark"}"#).unwrap();

        assert_eq!(
            to_settings(&file).import_folders,
            crate::dto::ImportFolders::None
        );
        assert!(
            !to_settings(&file).finish_sound,
            "no sound unless asked for"
        );
        assert!(
            !to_settings(&file).import_remove,
            "nothing removed unless asked for"
        );
    }

    #[test]
    fn keeping_deleted_files_is_off_unless_asked_for_and_kept_as_keep_deleted() {
        assert!(!Settings::default().keep_deleted);
        let file = to_file(&Settings {
            keep_deleted: true,
            ..Settings::default()
        });
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"keepDeleted\":true"), "{json}");
        assert!(to_settings(&serde_json::from_str(&json).unwrap()).keep_deleted);
    }

    #[test]
    fn the_finish_sound_is_kept_as_finish_sound() {
        let on = Settings {
            finish_sound: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&to_file(&on)).unwrap();
        assert!(json.contains(r#""finishSound":true"#), "{json}");
    }

    #[test]
    fn an_unknown_theme_means_auto() {
        let file = SettingsFile {
            theme: "sepia".into(),
            ..SettingsFile::default()
        };

        assert_eq!(to_settings(&file).theme, ThemeChoice::Auto);
    }

    #[test]
    fn an_unknown_view_keeps_each_panes_default() {
        let file = SettingsFile {
            mac_view: "columns".into(),
            phone_view: String::new(),
            ..SettingsFile::default()
        };

        let settings = to_settings(&file);

        assert_eq!(settings.mac_view, ViewMode::List);
        assert_eq!(settings.phone_view, ViewMode::Grid);
    }

    #[test]
    fn a_file_without_a_density_or_with_an_unknown_one_is_standard() {
        let old: SettingsFile = serde_json::from_str(r#"{"version": 1, "theme": "dark"}"#).unwrap();
        let odd = SettingsFile {
            list_density: "tiny".into(),
            ..SettingsFile::default()
        };

        assert_eq!(to_settings(&old).list_density, Density::Standard);
        assert_eq!(to_settings(&odd).list_density, Density::Standard);
    }

    #[test]
    fn a_file_without_a_language_or_with_an_unknown_one_follows_the_mac() {
        let old: SettingsFile = serde_json::from_str(r#"{"version": 1, "theme": "dark"}"#).unwrap();
        let odd = SettingsFile {
            language: "klingon".into(),
            ..SettingsFile::default()
        };

        assert_eq!(to_settings(&old).language, LanguageChoice::FollowMac);
        assert_eq!(to_settings(&odd).language, LanguageChoice::FollowMac);
        assert_eq!(Settings::default().language, LanguageChoice::FollowMac);
    }

    #[test]
    fn each_language_is_read_back_by_the_code_the_file_names_it_with() {
        for (code, expected) in [
            ("system", LanguageChoice::FollowMac),
            ("en", LanguageChoice::Chosen(Language::English)),
            ("th", LanguageChoice::Chosen(Language::Thai)),
            ("ja", LanguageChoice::Chosen(Language::Japanese)),
            (
                "zh-Hans",
                LanguageChoice::Chosen(Language::ChineseSimplified),
            ),
        ] {
            let file: SettingsFile =
                serde_json::from_str(&format!(r#"{{"version": 1, "language": "{code}"}}"#))
                    .unwrap();

            assert_eq!(to_settings(&file).language, expected, "{code}");
        }
    }

    #[test]
    fn each_language_choice_is_written_under_the_language_key_and_survives_a_read() {
        let choices = [LanguageChoice::FollowMac]
            .into_iter()
            .chain(Language::ALL.into_iter().map(LanguageChoice::Chosen));

        for choice in choices {
            let settings = Settings {
                language: choice,
                ..Settings::default()
            };
            let written = serde_json::to_value(to_file(&settings)).unwrap();
            let expected_code = match choice {
                LanguageChoice::FollowMac => "system",
                LanguageChoice::Chosen(language) => language.code(),
            };

            assert_eq!(written["language"], expected_code, "{choice:?}");
            assert_eq!(to_settings(&to_file(&settings)), settings, "{choice:?}");
        }
    }

    #[test]
    fn a_file_with_a_key_this_build_does_not_know_still_loads() {
        let file: SettingsFile = serde_json::from_str(
            r#"{"version": 1, "language": "th", "aKeyFromALaterBuild": {"x": 1}}"#,
        )
        .unwrap();

        assert_eq!(
            to_settings(&file).language,
            LanguageChoice::Chosen(Language::Thai)
        );
    }

    #[test]
    fn a_file_from_before_versions_were_kept_has_seen_none() {
        let old: SettingsFile = serde_json::from_str(r#"{"version": 1}"#).unwrap();
        let odd = SettingsFile {
            last_seen_version: "1.2".into(),
            ..SettingsFile::default()
        };

        assert_eq!(to_settings(&old).last_seen_version, None);
        assert_eq!(to_settings(&odd).last_seen_version, None);
    }

    #[test]
    fn a_file_from_before_windows_were_kept_opens_as_on_a_first_run() {
        let old: SettingsFile = serde_json::from_str(r#"{"version": 1}"#).unwrap();

        let settings = to_settings(&old);

        assert_eq!(settings.window, None);
        assert_eq!(settings.mac_order, SortOrder::default());
        assert_eq!(settings.phone_order, SortOrder::default());
    }

    #[test]
    fn the_window_is_stored_as_plain_numbers() {
        let file: SettingsFile = serde_json::from_str(
            r#"{"window": {"x": 120, "y": -40, "width": 1300, "height": 820}, "phoneSort": "modified", "phoneSortDescending": true}"#,
        )
        .unwrap();

        let settings = to_settings(&file);

        assert_eq!(
            settings.window,
            Some(WindowPlace {
                x: 120,
                y: -40,
                width: 1300,
                height: 820,
                fullscreen: false,
            })
        );
        assert_eq!(
            settings.phone_order,
            SortOrder {
                key: SortKey::Modified,
                descending: true,
            }
        );
    }

    #[test]
    fn a_window_too_small_or_too_large_to_be_real_is_forgotten() {
        let window = |width, height| SettingsFile {
            window: Some(WindowFile {
                x: 0,
                y: 0,
                width,
                height,
                fullscreen: false,
            }),
            ..SettingsFile::default()
        };

        assert_eq!(to_settings(&window(0, 800)).window, None);
        assert_eq!(to_settings(&window(1200, 199)).window, None);
        assert_eq!(to_settings(&window(20_001, 800)).window, None);
        assert!(to_settings(&window(200, 200)).window.is_some());
    }

    #[test]
    fn a_sort_by_kind_is_kept_as_kind() {
        let settings = Settings {
            phone_order: SortOrder {
                key: SortKey::Kind,
                descending: false,
            },
            ..Settings::default()
        };
        let file = to_file(&settings);

        assert_eq!(file.phone_sort, "kind");
        assert_eq!(to_settings(&file).phone_order.key, SortKey::Kind);
    }

    #[test]
    fn an_unknown_sort_column_sorts_by_name() {
        let file = SettingsFile {
            mac_sort: "colour".into(),
            mac_sort_descending: true,
            ..SettingsFile::default()
        };

        assert_eq!(
            to_settings(&file).mac_order,
            SortOrder {
                key: SortKey::Name,
                descending: true,
            }
        );
    }

    #[test]
    fn column_widths_are_kept_only_once_changed_and_brought_within_bounds() {
        assert!(
            SettingsFile::default().mac_columns.is_none(),
            "defaults are not written"
        );
        let file: SettingsFile =
            serde_json::from_str(r#"{"phoneColumns": {"kind": 10, "size": 95, "modified": 9000}}"#)
                .unwrap();

        let settings = to_settings(&file);

        assert_eq!(
            settings.phone_columns,
            ColumnWidths {
                kind: ColumnWidths::LEAST,
                size: 95,
                modified: ColumnWidths::MOST,
            }
        );
        assert_eq!(settings.mac_columns, ColumnWidths::default());
    }

    #[test]
    fn each_panes_tile_size_is_kept_and_an_odd_one_read_back_as_usual() {
        let settings = Settings {
            mac_tiles: TileSize::default().smaller(),
            phone_tiles: TileSize::default().bigger().bigger(),
            ..Settings::default()
        };
        let json = serde_json::to_string(&to_file(&settings)).unwrap();
        assert!(
            json.contains("\"macTiles\":112") && json.contains("\"phoneTiles\":192"),
            "{json}"
        );
        let back: SettingsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(to_settings(&back).mac_tiles, settings.mac_tiles);
        assert_eq!(to_settings(&back).phone_tiles, settings.phone_tiles);

        let odd: SettingsFile = serde_json::from_str(r#"{"macTiles": 999}"#).unwrap();
        assert_eq!(to_settings(&odd).mac_tiles, TileSize::default());
        assert_eq!(
            to_settings(&odd).phone_tiles,
            TileSize::default(),
            "missing"
        );
    }

    #[test]
    fn the_folders_file_keeps_absolute_paths_only() {
        let file: FoldersFile = serde_json::from_str(
            r#"{"mac": "Desktop", "phones": [{"phone": "pixel", "storageId": 1, "path": "/DCIM"}, {"phone": "", "storageId": 2, "path": "/x"}, {"phone": "pixel", "storageId": 3, "path": "Music"}]}"#,
        )
        .unwrap();

        let folders = to_folders(&file);

        assert_eq!(folders.mac, None, "not absolute: dropped");
        assert_eq!(folders.phones.len(), 1);
        assert_eq!(folders.phone("pixel", 1), Some("/DCIM"));
        let again = to_folders(&to_folders_file(&folders));
        assert_eq!(again, folders, "round trip");
    }

    #[test]
    fn recent_folders_read_back_are_absolute_and_no_more_than_kept() {
        let mac: Vec<String> = (0..15).map(|n| format!("\"/f{n}\"")).collect();
        let phone: Vec<String> = (0..15)
            .map(|n| format!(r#"{{"phone": "pixel", "storageId": 1, "path": "/p{n}"}}"#))
            .collect();
        let json = format!(
            r#"{{"recentMac": ["Desktop", {}], "recentPhone": [{{"phone": "pixel", "storageId": 1, "path": "DCIM"}}, {}]}}"#,
            mac.join(","),
            phone.join(",")
        );
        let file: FoldersFile = serde_json::from_str(&json).unwrap();

        let folders = to_folders(&file);

        assert_eq!(folders.recent_mac.len(), MOST_RECENT);
        assert_eq!(folders.recent_mac[0], "/f0", "the relative one dropped");
        assert_eq!(folders.recent_on("pixel").count(), MOST_RECENT);
        assert_eq!(folders.recent_phone[0].path, "/p0");
        assert_eq!(to_folders(&to_folders_file(&folders)), folders);
        let old: FoldersFile = serde_json::from_str(r#"{"mac": "/Users/me"}"#).unwrap();
        assert!(
            to_folders(&old).recent_mac.is_empty(),
            "a file from before has none"
        );
    }
}
