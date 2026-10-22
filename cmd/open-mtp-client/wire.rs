//! Composition root, written by hand (Rust has no google/wire). Builds every
//! service once and binds each concrete `internal::service` type to the
//! `Service` trait its handler declares.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use internal::dto;
use internal::service::{
    crash, device, doctor, explorer, help, local, places, settings, thumbnail, transfer,
};

use crate::handler;

/// Set to a folder to browse it as a fake phone instead of a USB device.
const VIRTUAL_DEVICE_ENV: &str = "OPEN_MTP_CLIENT_VIRTUAL_DIR";
const VIRTUAL_DEVICE_SERIAL: &str = "virtual-0001";

pub struct Handlers {
    pub device: handler::device::Handler,
    pub device_pane: handler::explorer::Handler,
    pub local_pane: handler::explorer::Handler,
    /// Where the Mac pane opens.
    pub local_home: String,
    pub transfer: handler::transfer::Handler,
    pub settings: handler::settings::Handler,
    pub help: handler::help::Handler,
    /// What to say when the app's own code failed.
    pub crash: handler::crash::Handler,
    pub search: handler::search::Handler,
    pub import: handler::import::Handler,
    pub space: handler::space::Handler,
    pub mirror: handler::mirror::Handler,
    pub preview: handler::preview::Handler,
    pub places: handler::places::Handler,
    pub info: handler::info::Handler,
    pub menubar: handler::menubar::Handler,
    pub languages: Arc<dyn local::Languages>,
}

pub fn initialize() -> Handlers {
    let doctor_service = Arc::new(doctor::Service::system());
    let device_service = Arc::new(device::Service::new(device_source(std::env::var_os(
        VIRTUAL_DEVICE_ENV,
    ))));
    let explorer_service = Arc::new(explorer::Service::new(Arc::clone(&device_service)));
    let home = home_dir(std::env::var_os("HOME"));
    let settings_service = Arc::new(settings_service(&home));
    let imports = transfer::ImportRecord::at(imports_dir(&home));
    let sidebar = PlacesStore {
        places: Arc::new(places::Service::new(
            places_path(&home),
            home.clone(),
            PathBuf::from("/Volumes"),
        )),
    };
    let crashes = Arc::new(crash::Crashes::new(crashes_dir(&home)));
    let local_service = Arc::new(local::Service::new(home.clone()));
    let local_home = local_service.home();
    // Asked at start-up: a notification posted before the answer is dropped.
    let dock = Arc::new(local::SystemDock);
    dock.ask_to_notify();
    let awake = Arc::new(local::SystemAwake);
    let counting = Counting {
        phone: Arc::clone(&explorer_service),
        mac: Arc::clone(&local_service),
    };
    let thumbnails = Arc::new(thumbnails(Arc::clone(&device_service), &home));
    let transfers = transfers(
        Arc::clone(&device_service),
        Arc::clone(&explorer_service),
        Arc::clone(&local_service),
        local::system_trash,
        imports,
        fresh_preview_cache(&std::env::temp_dir()),
        Presence { dock, awake },
    )
    .with_editing(editing_dir(&home), Arc::new(local::SystemOpener));

    let previews = ImportPreviews {
        transfer: Arc::clone(&transfers.transfer),
    };
    let spaces = Spaces {
        transfer: Arc::clone(&transfers.transfer),
    };
    let comparisons = MirrorPreviews {
        transfer: Arc::clone(&transfers.transfer),
    };
    let looks = Looks {
        transfer: Arc::clone(&transfers.transfer),
        quick_look: Arc::new(local::SystemQuickLook::default()),
        opener: Arc::new(local::SystemOpener),
        sharer: Arc::new(local::SystemSharer),
    };

    Handlers {
        device: handler::device::Handler::new(Arc::new(Devices {
            device: device_service,
            doctor: Arc::clone(&doctor_service),
            // The panes' own: the folders found are remembered for them too.
            explorer: Arc::clone(&explorer_service),
        })),
        device_pane: handler::explorer::Handler::new(Arc::new(PhoneFiles {
            files: Arc::clone(&explorer_service),
            thumbnails: Arc::clone(&thumbnails),
        })),
        local_pane: handler::explorer::Handler::new(Arc::new(MacFiles {
            files: local_service,
            thumbnails,
            opener: Arc::new(local::SystemOpener),
        })),
        local_home,
        transfer: handler::transfer::Handler::new(Arc::new(transfers)),
        settings: handler::settings::Handler::new(settings_service),
        help: handler::help::Handler::new(Arc::new(HelpDesk {
            help: help::Service::system(doctor_service),
            files: Arc::clone(&explorer_service),
            crashes: Arc::clone(&crashes),
        })),
        crash: handler::crash::Handler::new(Arc::new(CrashDesk {
            crashes,
            opener: Arc::new(local::SystemOpener),
        })),
        search: handler::search::Handler::new(Arc::new(PhoneSearch {
            files: Arc::clone(&explorer_service),
        })),
        import: handler::import::Handler::new(Arc::new(previews)),
        space: handler::space::Handler::new(Arc::new(spaces)),
        mirror: handler::mirror::Handler::new(Arc::new(comparisons)),
        preview: handler::preview::Handler::new(Arc::new(looks)),
        places: handler::places::Handler::new(Arc::new(sidebar)),
        info: handler::info::Handler::new(Arc::new(counting)),
        menubar: handler::menubar::Handler::new(Arc::new(MenuBar {
            bar: Arc::new(local::SystemMenuBar),
        })),
        languages: Arc::new(local::SystemLanguages),
    }
}

/// The Mac's menu bar: the real one in the app, a recorder in tests.
struct MenuBar {
    bar: Arc<dyn local::MenuBar>,
}

impl handler::menubar::Service for MenuBar {
    fn install(&self, menus: &[dto::Menu], on_click: Box<dyn Fn(u32) + Send + Sync>) {
        self.bar.install(menus, on_click);
    }

    fn set_state(&self, state: &dto::MenuState) {
        self.bar.set_state(state);
    }

    fn fill(&self, title: &str, entries: &[dto::MenuEntry]) {
        self.bar.fill(title, entries);
    }
}

/// The window as it opens: where it was left, if that is still on a screen
/// (`placement::opening`). Read before the app starts.
pub fn opening_window() -> iced::window::Settings {
    opening_window_in(&home_dir(std::env::var_os("HOME")), &local::screens())
}

/// `opening_window` for the settings under `home` and the given screens.
fn opening_window_in(home: &std::path::Path, screens: &[dto::Screen]) -> iced::window::Settings {
    crate::placement::opening(settings_service(home).load().window, screens)
}

/// Get Info's counts: a phone folder through the explorer's walk, a Mac
/// folder on the disk.
struct Counting {
    phone: Arc<explorer::Service>,
    mac: Arc<local::Service>,
}

#[async_trait::async_trait]
impl handler::info::Service for Counting {
    async fn tally(
        &self,
        place: &handler::info::Place,
        path: &str,
        cancel: dto::Cancel,
        report: Box<dyn Fn(dto::Tally) + Send + Sync>,
    ) -> Result<dto::Tally, dto::Error> {
        match place {
            handler::info::Place::Phone { storage_id, .. } => {
                self.phone.tally(*storage_id, path, cancel, report).await
            }
            handler::info::Place::Mac => self.mac.tally(path, cancel, report).await,
        }
    }
}

/// The folders pinned to the sidebar.
fn places_path(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/open-mtp-client/places.json")
}

/// The sidebar's places, read off the UI thread (a folder of the user's on a
/// network share that has stopped answering would hold up a look at it).
struct PlacesStore {
    places: Arc<places::Service>,
}

#[async_trait::async_trait]
impl handler::places::Service for PlacesStore {
    async fn places(&self) -> Vec<dto::Place> {
        let places = Arc::clone(&self.places);
        tokio::task::spawn_blocking(move || places.places())
            .await
            .unwrap_or_default()
    }

    fn pins(&self) -> Vec<dto::Pin> {
        self.places.pins()
    }

    fn save_pins(&self, pins: &[dto::Pin]) -> Result<(), dto::Error> {
        self.places.save_pins(pins)
    }

    fn mirrors(&self) -> Vec<dto::SavedMirror> {
        self.places.mirrors()
    }

    fn save_mirrors(&self, mirrors: &[dto::SavedMirror]) -> Result<(), dto::Error> {
        self.places.save_mirrors(mirrors)
    }
}

/// Phone thumbnails kept between launches: a cache, which macOS may empty.
fn thumbnails_dir(home: &std::path::Path) -> PathBuf {
    home.join("Library/Caches/open-mtp-client/thumbnails")
}

/// Thumbnails for both panes, the phone's kept in the app's cache folder.
fn thumbnails(device: Arc<device::Service>, home: &std::path::Path) -> thumbnail::Service {
    thumbnail::Service::new(device)
        .with_disk_cache(thumbnails_dir(home), thumbnail::DISK_CACHE_MOST)
}

/// What was imported from each phone, so it is not imported again.
/// Where working copies of phone files being edited are kept: beside the
/// settings, not in a cache macOS or a cleaner may empty.
fn editing_dir(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/open-mtp-client/editing")
}

/// Where a panic leaves its report: beside the settings, never in a place the
/// system or the app clears by itself.
fn crashes_dir(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/open-mtp-client/crashes")
}

/// Makes every panic from now on write a report (`crash::Crashes::install`).
/// First thing in `main`, before anything that could panic.
pub fn watch_for_crashes() {
    let home = home_dir(std::env::var_os("HOME"));
    crash::Crashes::new(crashes_dir(&home)).install(env!("CARGO_PKG_VERSION"));
}

/// The banner about the app's own failures: the reports on disk, the panics
/// of this run, and opening a report in the text editor.
struct CrashDesk {
    crashes: Arc<crash::Crashes>,
    opener: Arc<dyn local::Opener>,
}

impl handler::crash::Service for CrashDesk {
    fn unreported(&self) -> Vec<dto::CrashNote> {
        self.crashes.unreported()
    }

    fn mark_reported(&self) {
        self.crashes.mark_reported();
    }

    fn panics(&self) -> u64 {
        crash::panics_so_far()
    }

    fn last_panic(&self) -> Option<dto::CrashNote> {
        self.crashes.last_panic()
    }

    fn open(&self, file: &std::path::Path) -> bool {
        self.opener.open(file).is_ok()
    }
}

fn imports_dir(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/open-mtp-client/imported")
}

/// The settings file, in the app's own Application Support folder.
fn settings_path(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/open-mtp-client/settings.json")
}

/// Where settings were saved while the app was called mtprust.
fn earlier_settings_path(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support/mtprust/settings.json")
}

/// The settings store, with settings from the app's earlier name moved in.
fn settings_service(home: &std::path::Path) -> settings::Service {
    let service = settings::Service::new(settings_path(home));
    service.adopt(&earlier_settings_path(home));
    service
}

/// The Mac pane starts at `$HOME`, or the disk root if it is unset.
fn home_dir(home: Option<OsString>) -> PathBuf {
    home.map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// USB, or the fake phone in the folder the variable names. Several folders
/// joined with `:` are several fake phones plugged in at once.
fn device_source(virtual_dir: Option<OsString>) -> device::Source {
    let Some(dirs) = virtual_dir else {
        return device::Source::Usb;
    };
    let mut phones: Vec<device::VirtualPhone> = std::env::split_paths(&dirs)
        .enumerate()
        .map(|(at, dir)| {
            let serial = if at == 0 {
                VIRTUAL_DEVICE_SERIAL.to_owned()
            } else {
                format!("virtual-{:04}", at + 1)
            };
            // Report on-disk changes, so `make run-virtual` shows live refresh.
            device::VirtualPhone {
                watch_files: true,
                // So the sidebar's battery line shows.
                battery: Some(80),
                ..device::VirtualPhone::new(dir, serial)
            }
        })
        .collect();
    if phones.len() == 1 {
        device::Source::Virtual(phones.remove(0))
    } else {
        device::Source::Phones(phones)
    }
}

/// The import dialog's look at what is new, by the transfer service.
struct ImportPreviews {
    transfer: Arc<transfer::Service>,
}

#[async_trait::async_trait]
impl handler::import::Service for ImportPreviews {
    async fn preview(
        &self,
        request: dto::ImportRequest,
        cancel: dto::Cancel,
        seen: handler::import::Seen,
    ) -> Result<dto::ImportPreview, dto::Error> {
        self.transfer.import_preview(&request, &cancel, seen).await
    }
}

/// Free Up Space's look and removal, by the transfer service.
struct Spaces {
    transfer: Arc<transfer::Service>,
}

#[async_trait::async_trait]
impl handler::space::Service for Spaces {
    async fn look(
        &self,
        storage_id: u64,
        cancel: dto::Cancel,
        progress: handler::space::Told<dto::SpaceProgress>,
    ) -> Result<dto::SpaceReport, dto::Error> {
        self.transfer
            .space_report(storage_id, &cancel, progress)
            .await
    }

    async fn free_up(
        &self,
        request: dto::FreeUpRequest,
        cancel: dto::Cancel,
        removed: handler::space::Told<usize>,
    ) -> Result<dto::Removal, dto::Error> {
        self.transfer
            .free_up(&request, &cancel, move |progress| {
                removed(progress.files_done)
            })
            .await
    }
}

/// Where phone files fetched for Quick Look go: under the user's own
/// temporary folder, emptied when the app starts (the last run's copies go).
fn fresh_preview_cache(temp: &std::path::Path) -> PathBuf {
    let cache = temp.join("open-mtp-client/preview");
    let _ = std::fs::remove_dir_all(&cache);
    cache
}

/// Quick Look: phone files fetched by the transfer service, shown by macOS.
struct Looks {
    transfer: Arc<transfer::Service>,
    quick_look: Arc<dyn local::QuickLook>,
    opener: Arc<dyn local::Opener>,
    sharer: Arc<dyn local::Sharer>,
}

#[async_trait::async_trait]
impl handler::preview::Service for Looks {
    async fn fetch(
        &self,
        request: dto::PreviewRequest,
        cancel: dto::Cancel,
        progress: handler::preview::Progress,
    ) -> Result<dto::Fetched, dto::Error> {
        self.transfer
            .fetch_for_preview(&request, &cancel, progress)
            .await
    }

    fn look(&self, paths: &[String]) -> Result<dto::Look, dto::Error> {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        local::show_in_quick_look(self.quick_look.as_ref(), &paths)
    }

    fn open(&self, path: &str) -> Result<(), dto::Error> {
        local::open_in_app(self.opener.as_ref(), std::path::Path::new(path))
    }

    fn share(&self, paths: &[String], at: Option<(f32, f32)>) -> Result<(), dto::Error> {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let at = at.map(|(x, y)| (f64::from(x), f64::from(y)));
        local::share_files(self.sharer.as_ref(), &paths, at)
    }
}

/// The mirror dialog's comparison, by the transfer service.
struct MirrorPreviews {
    transfer: Arc<transfer::Service>,
}

#[async_trait::async_trait]
impl handler::mirror::Service for MirrorPreviews {
    async fn preview(
        &self,
        request: dto::MirrorRequest,
        cancel: dto::Cancel,
        seen: handler::mirror::Seen,
    ) -> Result<dto::MirrorPreview, dto::Error> {
        self.transfer.mirror_preview(&request, &cancel, seen).await
    }
}

/// The device panel: the phone, and the doctor that works out why it won't
/// connect.
struct Devices {
    device: Arc<device::Service>,
    doctor: Arc<doctor::Service>,
    /// For which usual places are on a storage.
    explorer: Arc<explorer::Service>,
}

#[async_trait::async_trait]
impl handler::device::Service for Devices {
    async fn connect(&self, prefer: Option<String>) -> Result<dto::Device, dto::Error> {
        self.device.connect_to(prefer.as_deref()).await
    }

    async fn phones(&self) -> Result<Vec<dto::Phone>, dto::Error> {
        self.device.phones().await
    }

    async fn battery(&self) -> Result<Option<u8>, dto::Error> {
        self.device.battery().await
    }

    async fn storages(&self) -> Result<Vec<dto::Storage>, dto::Error> {
        self.device.storages().await
    }

    async fn disconnect(&self) -> Result<(), dto::Error> {
        self.device.disconnect().await
    }

    fn watch(
        &self,
    ) -> Result<iced::futures::stream::BoxStream<'static, dto::DeviceChange>, dto::Error> {
        self.device.watch()
    }

    async fn next_change(&self) -> Result<dto::ContentChange, dto::Error> {
        self.device.next_change().await
    }

    async fn diagnose(&self, failure: &dto::Error) -> dto::Diagnosis {
        self.doctor.diagnose(failure).await
    }

    async fn folders_present(
        &self,
        storage_id: u64,
        paths: Vec<String>,
    ) -> Result<Vec<String>, dto::Error> {
        self.explorer.folders_present(storage_id, &paths).await
    }

    async fn release(&self, holders: &[dto::Holder]) {
        self.doctor.quit(holders).await
    }

    async fn free_from_camera(&self) -> bool {
        self.doctor.free_from_camera_services().await
    }
}

/// Help: the connection check, the report's Mac, links, and a timed reading
/// of a phone folder.
struct HelpDesk {
    help: help::Service,
    files: Arc<explorer::Service>,
    crashes: Arc<crash::Crashes>,
}

#[async_trait::async_trait]
impl handler::help::Service for HelpDesk {
    async fn checkup(&self) -> dto::Diagnosis {
        self.help.checkup().await
    }

    async fn release(&self, holders: &[dto::Holder]) {
        self.help.release(holders).await
    }

    async fn mac(&self) -> dto::MacInfo {
        // The reports are read off the UI thread, a handful of small files.
        let crashes = Arc::clone(&self.crashes);
        let crashes = tokio::task::spawn_blocking(move || crashes.notes())
            .await
            .unwrap_or_default();
        dto::MacInfo {
            crashes,
            ..self.help.mac().await
        }
    }

    fn open_link(&self, url: &str) -> bool {
        self.help.open_link(url)
    }

    async fn time_reading(
        &self,
        storage_id: u64,
        path: &str,
    ) -> Result<dto::ReadTiming, dto::Error> {
        self.files
            .time_reading(storage_id, path, dto::Cancel::new())
            .await
    }

    fn bundled_cli(&self) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        bundled_cli_beside(&exe).map(|path| path.to_string_lossy().into_owned())
    }
}

/// The command-line tool `make app` puts beside the app's own program in
/// `<name>.app/Contents/MacOS`; `None` for a program not in a bundle (run
/// from Cargo) or a bundle without it.
fn bundled_cli_beside(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let folder = exe.parent()?;
    let in_bundle = folder.ends_with("Contents/MacOS")
        && folder
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::extension)
            .is_some_and(|extension| extension == "app");
    let cli = folder.join("open-mtp-client-cli");
    (in_bundle && cli.is_file()).then_some(cli)
}

/// The phone pane: browsing, editing and thumbnails.
struct PhoneFiles {
    files: Arc<explorer::Service>,
    thumbnails: Arc<thumbnail::Service>,
}

#[async_trait::async_trait]
impl handler::explorer::Service for PhoneFiles {
    async fn list(
        &self,
        storage_id: u64,
        path: &str,
        show_hidden: bool,
    ) -> Result<Vec<dto::Entry>, dto::Error> {
        self.files.list(storage_id, path, show_hidden).await
    }

    fn sort(&self, entries: &mut [dto::Entry], sort: dto::Sort) {
        explorer::sort(entries, sort);
    }

    async fn make_folder(
        &self,
        volume_id: u64,
        parent: &str,
        name: &str,
    ) -> Result<String, dto::Error> {
        self.files.make_folder(volume_id, parent, name).await
    }

    async fn rename(
        &self,
        volume_id: u64,
        parent: &str,
        old: &str,
        new: &str,
    ) -> Result<String, dto::Error> {
        self.files.rename(volume_id, parent, old, new).await
    }

    async fn folder_names(&self, volume_id: u64, path: &str) -> Vec<String> {
        self.files.known_folders(volume_id, path).await
    }

    async fn folder_size(
        &self,
        volume_id: u64,
        path: &str,
        cancel: dto::Cancel,
    ) -> Result<dto::Tally, dto::Error> {
        self.files.tally(volume_id, path, cancel, |_| {}).await
    }

    fn plan_renames(
        &self,
        items: &[(String, bool)],
        folder: &[String],
        pattern: &dto::RenamePattern,
    ) -> Vec<dto::Renaming> {
        internal::service::renaming::plan(items, folder, pattern)
    }

    async fn delete(
        &self,
        volume_id: u64,
        parent: &str,
        names: &[String],
    ) -> Result<(), dto::Error> {
        self.files.delete(volume_id, parent, names).await
    }

    async fn paste(&self, request: &dto::PasteRequest) -> Result<dto::PasteSummary, dto::Error> {
        self.files.paste(request).await
    }

    async fn thumbnail(
        &self,
        volume_id: u64,
        folder: &str,
        entry: &dto::Entry,
    ) -> Result<Option<dto::Thumbnail>, dto::Error> {
        self.thumbnails.phone(volume_id, folder, entry).await
    }

    /// Phone files aren't in Finder; the phone pane never offers this.
    fn reveal(&self, _path: &str) {}

    /// A phone describes its files one round trip each, so a big folder is
    /// read piece by piece and shown as it arrives.
    async fn open_listing(
        &self,
        storage_id: u64,
        path: &str,
        show_hidden: bool,
        cancel: dto::Cancel,
        allow_remembered: bool,
    ) -> Result<Box<dyn handler::explorer::Listing>, dto::Error> {
        let listing = self
            .files
            .open_listing(storage_id, path, show_hidden, cancel, allow_remembered)
            .await?;
        Ok(Box::new(PhoneListing(listing)))
    }
}

/// A phone folder arriving in pieces.
struct PhoneListing(explorer::Listing);

#[async_trait::async_trait]
impl handler::explorer::Listing for PhoneListing {
    async fn next(&mut self) -> Option<Result<dto::ListingUpdate, dto::Error>> {
        self.0.next().await
    }
}

/// Searching the whole phone: a walk of a storage.
struct PhoneSearch {
    files: Arc<explorer::Service>,
}

#[async_trait::async_trait]
impl handler::search::Service for PhoneSearch {
    async fn walk(
        &self,
        storage_id: u64,
        include_hidden: bool,
        cancel: dto::Cancel,
    ) -> Result<Box<dyn handler::search::Walking>, dto::Error> {
        let walk = self.files.walk(storage_id, include_hidden, cancel).await?;
        Ok(Box::new(PhoneWalk(walk)))
    }
}

/// A walk of the phone arriving in pieces.
struct PhoneWalk(explorer::Walk);

#[async_trait::async_trait]
impl handler::search::Walking for PhoneWalk {
    async fn next(&mut self) -> Option<Result<dto::WalkUpdate, dto::Error>> {
        self.0.next().await
    }
}

/// The Mac pane: browsing, editing and thumbnails.
struct MacFiles {
    files: Arc<local::Service>,
    thumbnails: Arc<thumbnail::Service>,
    /// What opens System Settings for a refused folder: the real `open` in
    /// the app, a recorder in tests.
    opener: Arc<dyn local::Opener>,
}

#[async_trait::async_trait]
impl handler::explorer::Service for MacFiles {
    async fn list(
        &self,
        _volume_id: u64,
        path: &str,
        show_hidden: bool,
    ) -> Result<Vec<dto::Entry>, dto::Error> {
        self.files.list(path, show_hidden).await
    }

    fn sort(&self, entries: &mut [dto::Entry], sort: dto::Sort) {
        explorer::sort(entries, sort);
    }

    async fn make_folder(
        &self,
        _volume_id: u64,
        parent: &str,
        name: &str,
    ) -> Result<String, dto::Error> {
        self.files.make_folder(parent, name).await
    }

    async fn rename(
        &self,
        _volume_id: u64,
        parent: &str,
        old: &str,
        new: &str,
    ) -> Result<String, dto::Error> {
        self.files.rename(parent, old, new).await
    }

    async fn folder_size(
        &self,
        _volume_id: u64,
        path: &str,
        cancel: dto::Cancel,
    ) -> Result<dto::Tally, dto::Error> {
        self.files.tally(path, cancel, |_| {}).await
    }

    async fn folder_names(&self, _volume_id: u64, path: &str) -> Vec<String> {
        self.files
            .list(path, true)
            .await
            .map(|entries| {
                entries
                    .into_iter()
                    .filter(|entry| entry.is_folder)
                    .map(|entry| entry.name)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn plan_renames(
        &self,
        items: &[(String, bool)],
        folder: &[String],
        pattern: &dto::RenamePattern,
    ) -> Vec<dto::Renaming> {
        internal::service::renaming::plan(items, folder, pattern)
    }

    async fn delete(
        &self,
        _volume_id: u64,
        parent: &str,
        names: &[String],
    ) -> Result<(), dto::Error> {
        self.files.delete(parent, names).await
    }

    async fn paste(&self, request: &dto::PasteRequest) -> Result<dto::PasteSummary, dto::Error> {
        self.files.paste(request).await
    }

    async fn thumbnail(
        &self,
        _volume_id: u64,
        folder: &str,
        entry: &dto::Entry,
    ) -> Result<Option<dto::Thumbnail>, dto::Error> {
        let path = std::path::Path::new(folder).join(&entry.name);
        self.thumbnails.mac(&path.to_string_lossy()).await
    }

    fn reveal(&self, path: &str) {
        self.files.reveal(path);
    }

    fn open_privacy_settings(&self, pane: dto::PrivacyPane) {
        // Best effort: if `open` can't start, the hint still says where to go.
        let _ = self.opener.open_privacy_settings(pane);
    }
}

impl handler::settings::Service for settings::Service {
    fn load(&self) -> dto::Settings {
        settings::Service::load(self)
    }

    fn save(&self, settings: &dto::Settings) -> Result<(), dto::Error> {
        settings::Service::save(self, settings)
    }

    fn load_folders(&self) -> dto::LastFolders {
        settings::Service::load_folders(self)
    }

    fn save_folders(&self, folders: &dto::LastFolders) -> Result<(), dto::Error> {
        settings::Service::save_folders(self, folders)
    }

    fn load_transfers(&self) -> Vec<dto::SavedTransfer> {
        settings::Service::load_transfers(self)
    }

    fn save_transfers(&self, saved: &[dto::SavedTransfer]) -> Result<(), dto::Error> {
        settings::Service::save_transfers(self, saved)
    }
}

/// What the transfer panel needs: copying, and Finder for "Show in Finder".
struct Transfers {
    transfer: Arc<transfer::Service>,
    local: Arc<local::Service>,
    /// The Dock icon's bar and notifications: the real Dock in the app, a
    /// recorder in tests.
    dock: Arc<dyn local::Dock>,
    /// What keeps the Mac awake while transfers run: the real power
    /// activity in the app, a recorder in tests.
    awake: Arc<dyn local::Awake>,
    /// Opens a working copy for editing in its Mac app (`with_editing`):
    /// the real `open` in the app, a recorder in tests.
    opener: Option<Arc<dyn local::Opener>>,
}

impl Transfers {
    /// Lets phone files be edited on the Mac: working copies under `dir`,
    /// opened with `opener`. Called before the service is shared.
    fn with_editing(mut self, dir: PathBuf, opener: Arc<dyn local::Opener>) -> Self {
        self.transfer = match Arc::try_unwrap(self.transfer) {
            Ok(service) => Arc::new(service.with_editing(dir)),
            Err(shared) => shared,
        };
        self.opener = Some(opener);
        self
    }
}

/// How transfers show outside the window: the Dock icon and notifications,
/// and the Mac kept awake. The real ones in the app, recorders in tests.
struct Presence {
    dock: Arc<dyn local::Dock>,
    awake: Arc<dyn local::Awake>,
}

/// The transfer service as the app uses it. A move to the phone puts its
/// originals in the Trash by way of `trash`: the real one in the app, a
/// recorder in tests, never the user's Trash.
fn transfers(
    device: Arc<device::Service>,
    explorer: Arc<explorer::Service>,
    local: Arc<local::Service>,
    trash: impl Fn(&std::path::Path) -> std::io::Result<()> + Send + Sync + 'static,
    imports: transfer::ImportRecord,
    preview_cache: PathBuf,
    presence: Presence,
) -> Transfers {
    let Presence { dock, awake } = presence;
    Transfers {
        dock,
        awake,
        opener: None,
        transfer: Arc::new(
            transfer::Service::new(device, explorer)
                .with_trash(trash)
                .with_imports(imports)
                .with_preview_cache(preview_cache),
        ),
        local,
    }
}

#[async_trait::async_trait]
impl handler::transfer::Service for Transfers {
    async fn download(
        &self,
        mode: dto::Paste,
        request: dto::DownloadRequest,
        conflict: dto::Conflict,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer
            .download_with(mode, &request, conflict, &cancel, progress)
            .await
    }

    async fn taken_on_mac(&self, request: dto::DownloadRequest) -> Result<Vec<String>, dto::Error> {
        self.transfer.taken_on_mac(&request).await
    }

    async fn paste_on_mac(
        &self,
        request: dto::PasteRequest,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.local.paste_queued(&request, &cancel, progress).await
    }

    async fn import(
        &self,
        request: dto::ImportRequest,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer.import(&request, &cancel, progress).await
    }

    async fn mirror(
        &self,
        request: dto::MirrorRequest,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer.mirror(&request, &cancel, progress).await
    }

    async fn check_contents(
        &self,
        request: dto::CheckRequest,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer
            .check_contents(&request, &cancel, progress)
            .await
    }

    async fn edit_on_mac(&self, request: dto::EditRequest) -> Result<dto::WorkingCopy, dto::Error> {
        let copy = self.transfer.edit_on_mac(&request).await?;
        if let Some(opener) = &self.opener {
            local::open_in_app(opener.as_ref(), std::path::Path::new(&copy.path))?;
        }
        Ok(copy)
    }

    async fn working_copies(&self) -> Vec<dto::WorkingCopy> {
        self.transfer.working_copies().await
    }

    async fn copy_states(&self, paths: Vec<String>) -> Vec<Option<dto::CopyState>> {
        tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|path| transfer::copy_state(std::path::Path::new(path)))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    async fn send_edit(
        &self,
        copy: dto::WorkingCopy,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer.send_edit(&copy, &cancel, progress).await
    }

    async fn discard_edit(&self, copy: dto::WorkingCopy) -> Result<(), dto::Error> {
        self.transfer.discard_edit(&copy).await
    }

    async fn delete_keeping_copy(
        &self,
        request: dto::DeleteRequest,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer
            .delete_keeping_copy(&request, &cancel, progress)
            .await
    }

    async fn paste_on_phone(
        &self,
        request: dto::PasteRequest,
        resuming: bool,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer
            .paste_through_mac(&request, resuming, &cancel, progress)
            .await
    }

    async fn existing(&self, request: dto::UploadRequest) -> Result<Vec<String>, dto::Error> {
        self.transfer.existing(&request).await
    }

    async fn upload(
        &self,
        mode: dto::Paste,
        request: dto::UploadRequest,
        conflict: dto::Conflict,
        cancel: dto::Cancel,
        progress: handler::transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.transfer
            .upload_as(mode, &request, conflict, &cancel, progress)
            .await
    }

    async fn save_report(&self, folder: String, text: String) -> Result<String, dto::Error> {
        self.transfer.save_report(&folder, &text).await
    }

    fn reveal(&self, path: &str) {
        self.local.reveal(path);
    }

    fn show_progress(&self, percent: Option<u8>) {
        self.dock.show_progress(percent);
    }

    fn notify(&self, title: &str, body: &str) {
        self.dock.notify(title, body);
    }

    fn keep_awake(&self, awake: bool) -> bool {
        self.awake.keep_awake(awake)
    }

    fn chime(&self, sound: dto::Sound) {
        self.dock.chime(sound);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn virtual_device(dir: &tempfile::TempDir) -> Arc<device::Service> {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Arc::new(device::Service::new(device::Source::Virtual(
            device::VirtualPhone::new(
                dir.path().to_path_buf(),
                format!("wire-test-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            ),
        )))
    }

    #[test]
    fn without_the_env_var_the_device_is_usb() {
        assert!(matches!(device_source(None), device::Source::Usb));
    }

    #[test]
    fn the_env_var_selects_a_virtual_device() {
        let source = device_source(Some("/tmp/phone".into()));

        assert!(matches!(
            source,
            device::Source::Virtual(phone)
                if phone.backing_dir.as_os_str() == "/tmp/phone"
                    && phone.serial == VIRTUAL_DEVICE_SERIAL
                    && !phone.read_only
                    && phone.watch_files
        ));
    }

    #[test]
    fn the_env_var_can_name_several_fake_phones() {
        let source = device_source(Some("/tmp/a:/tmp/b".into()));

        let device::Source::Phones(phones) = source else {
            panic!("several phones");
        };
        let found: Vec<(&str, &str)> = phones
            .iter()
            .map(|p| (p.backing_dir.to_str().unwrap(), p.serial.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("/tmp/a", VIRTUAL_DEVICE_SERIAL),
                ("/tmp/b", "virtual-0002")
            ]
        );
        assert!(phones.iter().all(|p| p.watch_files));
    }

    #[test]
    fn initialize_builds_every_handler() {
        let handlers = initialize();

        assert!(!handlers.device_pane.is_open());
        assert_eq!(handlers.device.selected_storage(), None);
        assert!(!handlers.local_home.is_empty());
        assert!(!handlers.transfer.is_visible());
    }

    #[tokio::test]
    async fn device_service_is_reachable_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        let service: Arc<dyn handler::device::Service> = Arc::new(devices(virtual_device(&dir)));

        let device = service.connect(None).await.unwrap();
        let storages = service.storages().await.unwrap();
        service.disconnect().await.unwrap();

        assert_eq!(device.model, "Virtual Phone");
        assert_eq!(storages.len(), 1);
    }

    #[tokio::test]
    async fn several_phones_are_listed_and_switched_through_the_handler_trait() {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let source = device_source(Some(
            std::env::join_paths([one.path(), two.path()]).unwrap(),
        ));
        let service: Arc<dyn handler::device::Service> =
            Arc::new(devices(Arc::new(device::Service::new(source))));

        let phones = service.phones().await.unwrap();
        let keys: Vec<&str> = phones.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, [VIRTUAL_DEVICE_SERIAL, "virtual-0002"]);
        assert_eq!(
            service.connect(None).await,
            Err(dto::Error::MultipleDevices)
        );

        let first = service.connect(Some("virtual-0002".into())).await.unwrap();
        let second = service
            .connect(Some(VIRTUAL_DEVICE_SERIAL.into()))
            .await
            .unwrap();

        assert_eq!(first.key, "virtual-0002");
        assert_eq!(second.key, VIRTUAL_DEVICE_SERIAL);
        assert_eq!(
            service.connect(Some("unplugged".into())).await,
            Err(dto::Error::PhoneUnplugged)
        );
        service.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn the_battery_and_android_version_are_reachable_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        let source = device_source(Some(dir.path().as_os_str().to_owned()));
        let service: Arc<dyn handler::device::Service> =
            Arc::new(devices(Arc::new(device::Service::new(source))));

        let device = service.connect(None).await.unwrap();
        let level = service.battery().await;
        service.disconnect().await.unwrap();

        assert!(device.android);
        assert_eq!(level, Ok(Some(80)), "the level `make run-virtual` shows");
    }

    #[tokio::test]
    async fn an_import_is_previewed_and_run_through_the_handler_traits() {
        let (phone, mac, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir_all(phone.path().join("DCIM/Camera")).unwrap();
        std::fs::write(phone.path().join("DCIM/Camera/a.jpg"), b"aaa").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let transfers = transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            |_| Ok(()),
            transfer::ImportRecord::at(imports_dir(support.path())),
            support.path().join("preview"),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        );
        let previews: Arc<dyn handler::import::Service> = Arc::new(ImportPreviews {
            transfer: Arc::clone(&transfers.transfer),
        });
        let queue: Arc<dyn handler::transfer::Service> = Arc::new(transfers);
        let request = dto::ImportRequest {
            storage_id,
            destination: mac.path().to_string_lossy().into_owned(),
            folders: dto::ImportFolders::None,
            remove_from_phone: false,
        };

        let before = previews
            .preview(request.clone(), dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();
        let summary = queue
            .import(request.clone(), dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();
        let after = previews
            .preview(request, dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();

        assert_eq!(
            (before.new_files, summary.files, after.new_files),
            (1, 1, 0)
        );
        assert_eq!(std::fs::read(mac.path().join("a.jpg")).unwrap(), b"aaa");
        assert!(
            support
                .path()
                .join("Library/Application Support/open-mtp-client/imported")
                .is_dir(),
            "the record is kept beside the settings"
        );
    }

    #[tokio::test]
    async fn a_phone_file_is_edited_opened_watched_and_sent_back_through_the_handler_trait() {
        let (phone, mac, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::write(phone.path().join("notes.txt"), b"first").unwrap();
        let device = virtual_device(&phone);
        let key = device.connect().await.unwrap().key;
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let opener = Arc::new(local::RecordingOpener::default());
        let queue: Arc<dyn handler::transfer::Service> = Arc::new(
            transfers(
                device,
                explorer,
                Arc::new(local::Service::new(mac.path().to_path_buf())),
                |_| Ok(()),
                transfer::ImportRecord::at(imports_dir(support.path())),
                support.path().join("preview"),
                Presence {
                    dock: Arc::new(local::RecordingDock::default()),
                    awake: Arc::new(local::RecordingAwake::default()),
                },
            )
            .with_editing(editing_dir(support.path()), opener.clone()),
        );

        let copy = queue
            .edit_on_mac(dto::EditRequest {
                phone: key,
                storage_id,
                folder: "/".into(),
                name: "notes.txt".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            *opener.opened.lock().unwrap(),
            [PathBuf::from(&copy.path)],
            "opened in its Mac app"
        );
        assert!(
            copy.path
                .contains("Application Support/open-mtp-client/editing/")
        );
        assert_eq!(queue.working_copies().await, std::slice::from_ref(&copy));
        std::fs::write(&copy.path, b"second, edited").unwrap();
        let states = queue
            .copy_states(vec![copy.path.clone(), "/nowhere".into()])
            .await;
        assert_eq!(states[0].map(|state| state.size), Some(14));
        assert_eq!(states[1], None);

        queue
            .send_edit(copy.clone(), dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(phone.path().join("notes.txt")).unwrap(),
            b"second, edited"
        );
        assert!(queue.working_copies().await.is_empty());
        assert!(
            queue.discard_edit(copy).await.is_err(),
            "sent, so gone already"
        );
    }

    #[tokio::test]
    async fn a_delete_keeping_a_copy_reaches_the_trash_through_the_handler_trait() {
        let (phone, mac, support, bin) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::write(phone.path().join("a.jpg"), b"photo").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let into = bin.path().to_path_buf();
        // Never the user's Trash: the item goes into the test's own folder.
        let queue: Arc<dyn handler::transfer::Service> = Arc::new(transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            move |path: &std::path::Path| {
                std::fs::rename(path, into.join(path.file_name().unwrap()))
            },
            transfer::ImportRecord::at(imports_dir(support.path())),
            support.path().join("preview"),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        ));

        let summary = queue
            .delete_keeping_copy(
                dto::DeleteRequest {
                    storage_id,
                    folder: "/".into(),
                    names: vec!["a.jpg".into()],
                },
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(summary.removal.unwrap().removed, 1);
        assert_eq!(std::fs::read(bin.path().join("a.jpg")).unwrap(), b"photo");
        assert!(!phone.path().join("a.jpg").exists());
    }

    #[tokio::test]
    async fn free_up_space_looks_and_removes_a_copy_through_its_handler_trait() {
        let (phone, mac, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir_all(phone.path().join("Download")).unwrap();
        std::fs::write(phone.path().join("a.jpg"), b"same bytes").unwrap();
        std::fs::write(phone.path().join("Download/a.jpg"), b"same bytes").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let transfers = transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            |_| Ok(()),
            transfer::ImportRecord::at(imports_dir(support.path())),
            support.path().join("preview"),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        );
        let spaces: Arc<dyn handler::space::Service> = Arc::new(Spaces {
            transfer: Arc::clone(&transfers.transfer),
        });

        let report = spaces
            .look(storage_id, dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();
        assert_eq!(report.duplicates.len(), 1);
        let copy = report.duplicates[0].files[1].path.clone();
        let told = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&told);
        let removal = spaces
            .free_up(
                dto::FreeUpRequest {
                    storage_id,
                    paths: vec![copy],
                },
                dto::Cancel::new(),
                Box::new(move |done| seen.lock().unwrap().push(done)),
            )
            .await
            .unwrap();

        assert_eq!((removal.removed, removal.freed), (1, 10));
        assert_eq!(told.lock().unwrap().last(), Some(&1), "how many have gone");
        let left = [
            phone.path().join("a.jpg").exists(),
            phone.path().join("Download/a.jpg").exists(),
        ];
        assert_eq!(left.iter().filter(|there| **there).count(), 1);
    }

    #[tokio::test]
    async fn a_mirror_is_compared_and_run_both_ways_through_the_handler_traits() {
        let (phone, mac, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir_all(phone.path().join("Backup")).unwrap();
        std::fs::write(phone.path().join("Backup/a.jpg"), b"aaa").unwrap();
        std::fs::write(mac.path().join("m.txt"), b"m").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let transfers = transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            |_| Ok(()),
            transfer::ImportRecord::at(imports_dir(support.path())),
            support.path().join("preview"),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        );
        let comparisons: Arc<dyn handler::mirror::Service> = Arc::new(MirrorPreviews {
            transfer: Arc::clone(&transfers.transfer),
        });
        let queue: Arc<dyn handler::transfer::Service> = Arc::new(transfers);
        let request = |direction| dto::MirrorRequest {
            storage_id,
            phone_folder: "/Backup".into(),
            mac_folder: mac.path().to_string_lossy().into_owned(),
            direction,
            remove_extras: false,
        };

        let to_mac = comparisons
            .preview(
                request(dto::MirrorDirection::ToMac),
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();
        queue
            .mirror(
                request(dto::MirrorDirection::ToMac),
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();
        queue
            .mirror(
                request(dto::MirrorDirection::ToPhone),
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!((to_mac.new_files, to_mac.extras), (1, 1));
        assert_eq!(std::fs::read(mac.path().join("a.jpg")).unwrap(), b"aaa");
        assert_eq!(
            std::fs::read(phone.path().join("Backup/m.txt")).unwrap(),
            b"m"
        );
    }

    #[tokio::test]
    async fn quick_look_fetches_and_shows_through_the_handler_trait_never_opening_a_window() {
        let (phone, mac, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::write(phone.path().join("a.jpg"), b"aaa").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let transfers = transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            |_| Ok(()),
            transfer::ImportRecord::at(imports_dir(support.path())),
            fresh_preview_cache(support.path()),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        );
        let recorder = Arc::new(local::RecordingQuickLook::default());
        let opener = Arc::new(local::RecordingOpener::default());
        let sharer = Arc::new(local::RecordingSharer::default());
        let looks: Arc<dyn handler::preview::Service> = Arc::new(Looks {
            transfer: Arc::clone(&transfers.transfer),
            quick_look: recorder.clone(),
            opener: opener.clone(),
            sharer: sharer.clone(),
        });

        let fetched = looks
            .fetch(
                dto::PreviewRequest {
                    storage_id,
                    folder: "/".into(),
                    names: vec!["a.jpg".into()],
                },
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();
        let shown = looks.look(&fetched.paths);

        assert_eq!(shown, Ok(dto::Look::Opened));
        assert_eq!(std::fs::read(&fetched.paths[0]).unwrap(), b"aaa");
        assert!(
            fetched.paths[0].starts_with(
                &*support
                    .path()
                    .join("open-mtp-client/preview")
                    .to_string_lossy()
            )
        );
        assert_eq!(recorder.shown.lock().unwrap().len(), 1);
        looks.open(&fetched.paths[0]).unwrap();
        assert_eq!(
            *opener.opened.lock().unwrap(),
            [std::path::PathBuf::from(&fetched.paths[0])]
        );
        looks.share(&fetched.paths, Some((12.0, 34.0))).unwrap();
        assert_eq!(
            *sharer.shared.lock().unwrap(),
            [(
                vec![std::path::PathBuf::from(&fetched.paths[0])],
                Some((12.0, 34.0))
            )]
        );
    }

    #[test]
    fn the_preview_cache_starts_empty_each_run() {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("open-mtp-client/preview/aa-1/x/a.jpg");
        std::fs::create_dir_all(left.parent().unwrap()).unwrap();
        std::fs::write(&left, b"old").unwrap();

        let cache = fresh_preview_cache(temp.path());

        assert_eq!(cache, temp.path().join("open-mtp-client/preview"));
        assert!(!cache.exists(), "the last run's copies are gone");
    }

    #[test]
    fn the_window_opens_where_the_settings_file_says() {
        let home = tempfile::tempdir().unwrap();
        let screens = [dto::Screen {
            x: 0,
            y: 37,
            width: 1512,
            height: 945,
        }];
        assert!(matches!(
            opening_window_in(home.path(), &screens).position,
            iced::window::Position::Centered
        ));

        let settings = dto::Settings {
            window: Some(dto::WindowPlace {
                x: 60,
                y: 80,
                width: 1250,
                height: 760,
                fullscreen: false,
            }),
            ..dto::Settings::default()
        };
        settings_service(home.path()).save(&settings).unwrap();
        let opening = opening_window_in(home.path(), &screens);

        assert!(matches!(
            opening.position,
            iced::window::Position::Specific(point) if point == iced::Point::new(60.0, 80.0)
        ));
        assert_eq!(opening.size, iced::Size::new(1250.0, 760.0));
    }

    #[test]
    fn the_transfer_panel_reaches_the_dock_and_the_mac_it_was_given() {
        let dock = Arc::new(local::RecordingDock::default());
        let awake = Arc::new(local::RecordingAwake::default());
        let phone = tempfile::tempdir().unwrap();
        let device = virtual_device(&phone);
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(phone.path().to_path_buf())),
            dock: Arc::clone(&dock) as Arc<dyn local::Dock>,
            awake: Arc::clone(&awake) as Arc<dyn local::Awake>,
        });

        service.show_progress(Some(5));
        service.notify("Downloaded 1 file", "all arrived");
        assert!(service.keep_awake(true));
        assert!(!service.keep_awake(false));

        assert_eq!(*awake.changes.lock().unwrap(), [true, false]);

        assert_eq!(*dock.progress.lock().unwrap(), [Some(5)]);
        assert_eq!(
            *dock.notes.lock().unwrap(),
            [("Downloaded 1 file".to_owned(), "all arrived".to_owned())]
        );
    }

    #[tokio::test]
    async fn get_info_counts_either_side_through_the_handler_trait() {
        let (phone_dir, mac_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(phone_dir.path().join("DCIM/Camera")).unwrap();
        std::fs::write(phone_dir.path().join("DCIM/Camera/a.jpg"), b"aaa").unwrap();
        std::fs::write(mac_dir.path().join("b.txt"), b"bb").unwrap();
        let device = virtual_device(&phone_dir);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let counting: Arc<dyn handler::info::Service> = Arc::new(Counting {
            phone: Arc::new(explorer::Service::new(device)),
            mac: Arc::new(local::Service::new(mac_dir.path().to_path_buf())),
        });
        let phone = handler::info::Place::Phone {
            storage_id,
            storage: "Internal".into(),
            abilities: None,
        };

        let on_phone = counting
            .tally(&phone, "/DCIM", dto::Cancel::new(), Box::new(|_| {}))
            .await
            .unwrap();
        let on_mac = counting
            .tally(
                &handler::info::Place::Mac,
                mac_dir.path().to_str().unwrap(),
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(
            (on_phone.files, on_phone.folders, on_phone.bytes),
            (1, 1, 3)
        );
        assert_eq!((on_mac.files, on_mac.folders, on_mac.bytes), (1, 0, 2));
    }

    #[tokio::test]
    async fn the_sidebar_places_and_pins_are_reachable_through_the_handler_trait() {
        let (home, volumes, support) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir(home.path().join("Downloads")).unwrap();
        std::fs::create_dir(volumes.path().join("Backup")).unwrap();
        let store: Arc<dyn handler::places::Service> = Arc::new(PlacesStore {
            places: Arc::new(places::Service::new(
                places_path(support.path()),
                home.path().to_path_buf(),
                volumes.path().to_path_buf(),
            )),
        });
        let pin = dto::Pin {
            side: dto::PinSide::Mac,
            path: "/Users/me/Work".into(),
            label: "Work".into(),
            phone: None,
        };

        let found: Vec<String> = store.places().await.into_iter().map(|p| p.label).collect();
        store.save_pins(std::slice::from_ref(&pin)).unwrap();

        assert_eq!(found, ["Home", "Downloads", "Backup"]);
        assert_eq!(store.pins(), [pin]);
        assert!(
            support
                .path()
                .join("Library/Application Support/open-mtp-client/places.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn the_doctor_is_reachable_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        let service: Arc<dyn handler::device::Service> = Arc::new(devices(virtual_device(&dir)));

        // Nothing to look into for a failure no app or cable can cause.
        let diagnosis = service.diagnose(&dto::Error::Cancelled).await;
        service.release(&[]).await;

        assert_eq!(diagnosis, dto::Diagnosis::default());
    }

    #[tokio::test]
    async fn explorer_service_is_reachable_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("DCIM")).unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));

        let mut entries = service.list(storage, "/", false).await.unwrap();
        service.sort(&mut entries, dto::Sort::default());

        assert_eq!(entries[0].name, "DCIM");
    }

    #[tokio::test]
    async fn the_phone_pane_pastes_inside_the_phone_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM")).unwrap();
        std::fs::create_dir_all(dir.path().join("Music")).unwrap();
        std::fs::write(dir.path().join("DCIM/a.jpg"), b"A").unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));
        let request = |mode, to: &str| dto::PasteRequest {
            mode,
            from_storage: storage,
            from: "/DCIM".into(),
            names: vec!["a.jpg".into()],
            to_storage: storage,
            to: to.into(),
        };

        let copied = service
            .paste(&request(dto::Paste::Copy, "/Music"))
            .await
            .unwrap();
        let again = service
            .paste(&request(dto::Paste::Copy, "/DCIM"))
            .await
            .unwrap();

        assert_eq!((copied.done, copied.names), (1, vec!["a.jpg".to_owned()]));
        assert_eq!(again.names, ["a (1).jpg"], "never over the original");
        assert_eq!(std::fs::read(dir.path().join("Music/a.jpg")).unwrap(), b"A");
        assert_eq!(std::fs::read(dir.path().join("DCIM/a.jpg")).unwrap(), b"A");
        assert_eq!(
            std::fs::read(dir.path().join("DCIM/a (1).jpg")).unwrap(),
            b"A"
        );
    }

    #[tokio::test]
    async fn the_mac_pane_pastes_inside_itself_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a/x.txt"), b"hello").unwrap();
        let local = local::Service::new(dir.path().to_path_buf());
        let service: Arc<dyn handler::explorer::Service> = Arc::new(mac_files(local));
        let request = |mode| dto::PasteRequest {
            mode,
            from_storage: 0,
            from: dir.path().join("a").to_string_lossy().into_owned(),
            names: vec!["x.txt".into()],
            to_storage: 0,
            to: dir.path().join("b").to_string_lossy().into_owned(),
        };

        let copied = service.paste(&request(dto::Paste::Copy)).await.unwrap();

        assert_eq!(
            (copied.done, copied.names.clone()),
            (1, vec!["x.txt".to_owned()])
        );
        assert_eq!(std::fs::read(dir.path().join("b/x.txt")).unwrap(), b"hello");
        assert!(
            dir.path().join("a/x.txt").exists(),
            "a copy leaves the original"
        );

        // Moving it again: the name is taken, so it is numbered, never replaced.
        let moved = service.paste(&request(dto::Paste::Move)).await.unwrap();

        assert_eq!(moved.names, ["x (1).txt"]);
        assert_eq!(std::fs::read(dir.path().join("b/x.txt")).unwrap(), b"hello");
        assert!(!dir.path().join("a/x.txt").exists());
    }

    #[tokio::test]
    async fn a_big_copy_inside_the_mac_goes_through_the_transfer_trait_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a/x.txt"), b"hello").unwrap();
        let phone = tempfile::tempdir().unwrap();
        let device = virtual_device(&phone);
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(dir.path().to_path_buf())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);

        let summary = service
            .paste_on_mac(
                dto::PasteRequest {
                    mode: dto::Paste::Copy,
                    from_storage: 0,
                    from: dir.path().join("a").to_string_lossy().into_owned(),
                    names: vec!["x.txt".into()],
                    to_storage: 0,
                    to: dir.path().join("b").to_string_lossy().into_owned(),
                },
                dto::Cancel::new(),
                Box::new(move |progress| log.lock().unwrap().push(progress)),
            )
            .await
            .unwrap();

        assert_eq!(summary.direction, dto::Direction::Local);
        assert_eq!((summary.files, summary.bytes), (1, 5));
        assert_eq!(std::fs::read(dir.path().join("b/x.txt")).unwrap(), b"hello");
        assert!(!seen.lock().unwrap().is_empty(), "progress arrived");
    }

    #[tokio::test]
    async fn the_phone_pane_reads_a_big_folder_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        for i in 0..700 {
            std::fs::write(dir.path().join(format!("DCIM/Camera/IMG_{i:04}.jpg")), b"x").unwrap();
        }
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));

        let mut listing = service
            .open_listing(storage, "/DCIM/Camera", false, dto::Cancel::new(), true)
            .await
            .unwrap();
        let mut pieces = Vec::new();
        while let Some(update) = listing.next().await {
            pieces.push(update.unwrap());
        }

        assert!(pieces.len() >= 2, "{} pieces", pieces.len());
        assert!(pieces.last().unwrap().done);
        assert_eq!(pieces.iter().map(|p| p.entries.len()).sum::<usize>(), 700);
        assert_eq!(pieces[0].total, 700);
    }

    #[tokio::test]
    async fn the_search_walks_the_whole_phone_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        std::fs::create_dir_all(dir.path().join(".thumbnails")).unwrap();
        std::fs::write(dir.path().join("DCIM/Camera/beach.mp4"), b"x").unwrap();
        std::fs::write(dir.path().join(".thumbnails/1.jpg"), b"x").unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let files = Arc::new(explorer::Service::new(device));
        let service: Arc<dyn handler::search::Service> = Arc::new(PhoneSearch { files });

        let mut walk = service
            .walk(storage, false, dto::Cancel::new())
            .await
            .unwrap();
        let mut found = Vec::new();
        let mut done = false;
        while let Some(update) = walk.next().await {
            let update = update.unwrap();
            done = update.done;
            found.extend(
                update
                    .found
                    .into_iter()
                    .map(|f| format!("{}|{}", f.folder, f.entry.name)),
            );
        }

        assert!(done);
        found.sort();
        assert_eq!(found, ["/DCIM/Camera|beach.mp4", "/DCIM|Camera", "/|DCIM"]);
    }

    #[tokio::test]
    async fn the_mac_pane_hands_over_a_folder_whole() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let service: Arc<dyn handler::explorer::Service> = Arc::new(MacFiles {
            files: Arc::new(local::Service::new(dir.path().to_path_buf())),
            thumbnails: Arc::new(thumbnail::Service::new(Arc::new(device::Service::new(
                device::Source::Usb,
            )))),
            opener: Arc::new(local::RecordingOpener::default()),
        });

        let mut listing = service
            .open_listing(
                0,
                &dir.path().to_string_lossy(),
                false,
                dto::Cancel::new(),
                true,
            )
            .await
            .unwrap();
        let update = listing.next().await.unwrap().unwrap();

        assert!(update.done);
        assert_eq!(update.entries.len(), 1);
        assert!(listing.next().await.is_none());
    }

    #[tokio::test]
    async fn help_is_reachable_through_the_handler_trait() {
        let phone = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(phone.path().join("DCIM")).unwrap();
        for name in ["a.jpg", "b.jpg", ".hidden"] {
            std::fs::write(phone.path().join("DCIM").join(name), b"x").unwrap();
        }
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::help::Service> = Arc::new(HelpDesk {
            help: help::Service::system(Arc::new(doctor::Service::system())),
            files: Arc::new(explorer::Service::new(device)),
            crashes: Arc::new(crash::Crashes::new(PathBuf::from("/nonexistent/crashes"))),
        });

        let reading = service.time_reading(storage_id, "/DCIM").await.unwrap();
        assert_eq!(reading.items, 3, "hidden ones too");

        let diagnosis = service.checkup().await;
        let mac = service.mac().await;
        service.release(&[]).await;

        assert!(diagnosis.holders.is_some() && diagnosis.phones.is_some());
        assert!(!mac.os_version.is_empty());
        // Only a refused address is tried here: a valid one would start a browser.
        assert!(!service.open_link("file:///etc/passwd"));
    }

    fn crash_report(dir: &std::path::Path, name: &str, location: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(name),
            format!("open-mtp-client crash report\nVersion: 0.1.0\nWhen: 1\nThread: t\nLocation: {location}\n\nMessage:\nx\n"),
        )
        .unwrap();
    }

    #[test]
    fn crash_reports_are_kept_beside_the_settings() {
        assert_eq!(
            crashes_dir(std::path::Path::new("/Users/me")),
            PathBuf::from("/Users/me/Library/Application Support/open-mtp-client/crashes")
        );
    }

    #[test]
    fn the_crash_banner_reads_the_reports_marks_them_and_opens_one() {
        let home = tempfile::tempdir().unwrap();
        let dir = crashes_dir(home.path());
        crash_report(&dir, "crash-1792000000000.txt", "internal/service/x.rs:7:3");
        let opener = Arc::new(local::RecordingOpener::default());
        let desk = CrashDesk {
            crashes: Arc::new(crash::Crashes::new(dir.clone())),
            opener: Arc::clone(&opener) as Arc<dyn local::Opener>,
        };
        let service: &dyn handler::crash::Service = &desk;

        let unreported = service.unreported();
        assert_eq!(unreported.len(), 1);
        assert_eq!(unreported[0].location, "internal/service/x.rs:7:3");
        let file = unreported[0].file.clone().unwrap();
        assert!(service.open(&file));
        assert_eq!(*opener.opened.lock().unwrap(), [file]);
        assert_eq!(service.panics(), crash::panics_so_far());
        assert!(service.last_panic().is_none(), "no panic in this process");

        service.mark_reported();

        assert!(service.unreported().is_empty());
        assert!(dir.join("reported-1792000000000.txt").exists());
    }

    #[test]
    fn a_report_that_the_mac_will_not_open_is_said_to_have_failed() {
        struct Refusing;
        impl local::Opener for Refusing {
            fn open(&self, _path: &std::path::Path) -> std::io::Result<()> {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            }

            fn open_privacy_settings(&self, _pane: dto::PrivacyPane) -> std::io::Result<()> {
                Ok(())
            }
        }
        let desk = CrashDesk {
            crashes: Arc::new(crash::Crashes::new(PathBuf::from("/nonexistent"))),
            opener: Arc::new(Refusing),
        };

        assert!(!handler::crash::Service::open(
            &desk,
            std::path::Path::new("/nonexistent/crash-1.txt")
        ));
    }

    #[tokio::test]
    async fn help_s_report_of_the_mac_names_the_crashes_kept() {
        let home = tempfile::tempdir().unwrap();
        let dir = crashes_dir(home.path());
        crash_report(&dir, "crash-100.txt", "a.rs:1:1");
        crash_report(&dir, "reported-200.txt", "b.rs:2:2");
        let service: Arc<dyn handler::help::Service> = Arc::new(HelpDesk {
            help: help::Service::system(Arc::new(doctor::Service::system())),
            files: Arc::new(explorer::Service::new(Arc::new(device::Service::new(
                device::Source::Usb,
            )))),
            crashes: Arc::new(crash::Crashes::new(dir)),
        });

        let mac = service.mac().await;

        let seen: Vec<(u64, &str, bool)> = mac
            .crashes
            .iter()
            .map(|note| (note.at_ms, note.location.as_str(), note.reported))
            .collect();
        assert_eq!(seen, [(200, "b.rs:2:2", true), (100, "a.rs:1:1", false)]);
        assert!(
            !mac.os_version.is_empty(),
            "the Mac's own part is still there"
        );
    }

    #[test]
    fn initialize_builds_the_help_dialog_closed() {
        let handlers = initialize();

        assert!(!handlers.help.is_open());
    }

    #[test]
    fn home_falls_back_to_the_disk_root() {
        assert_eq!(
            home_dir(Some("/Users/me".into())),
            PathBuf::from("/Users/me")
        );
        assert_eq!(home_dir(None), PathBuf::from("/"));
    }

    #[tokio::test]
    async fn local_service_is_reachable_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let local = local::Service::new(dir.path().to_path_buf());
        let home = local.home();
        let service: Arc<dyn handler::explorer::Service> = Arc::new(mac_files(local));

        let mut entries = service.list(0, &home, false).await.unwrap();
        service.sort(&mut entries, dto::Sort::default());

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b.txt"]);
    }

    #[tokio::test]
    async fn device_edits_are_reachable_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));

        let made = service.make_folder(storage, "/", "Trip").await.unwrap();
        let renamed = service
            .rename(storage, "/", "a.txt", "b.txt")
            .await
            .unwrap();
        service
            .delete(storage, "/", &["Trip".into()])
            .await
            .unwrap();

        assert_eq!((made.as_str(), renamed.as_str()), ("Trip", "b.txt"));
        assert!(dir.path().join("b.txt").exists());
        assert!(!dir.path().join("Trip").exists());
    }

    #[tokio::test]
    async fn mac_edits_are_reachable_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let local = local::Service::with_trash(
            dir.path().to_path_buf(),
            std::sync::Arc::new(|path: &std::path::Path| std::fs::remove_file(path)),
        );
        let home = local.home();
        let service: Arc<dyn handler::explorer::Service> = Arc::new(mac_files(local));

        let made = service.make_folder(0, &home, "Trip").await.unwrap();
        let renamed = service.rename(0, &home, "a.txt", "b.txt").await.unwrap();
        service.delete(0, &home, &["b.txt".into()]).await.unwrap();

        assert_eq!((made.as_str(), renamed.as_str()), ("Trip", "b.txt"));
        assert!(dir.path().join("Trip").is_dir());
        assert!(!dir.path().join("b.txt").exists());
    }

    #[tokio::test]
    async fn downloads_are_reachable_through_the_transfer_trait() {
        let phone = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        std::fs::write(phone.path().join("a.txt"), b"abc").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(mac.path().to_path_buf())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });

        let summary = service
            .download(
                dto::Paste::Copy,
                dto::DownloadRequest {
                    storage_id,
                    source: "/".into(),
                    names: vec!["a.txt".into()],
                    destination: mac.path().to_string_lossy().into_owned(),
                },
                dto::Conflict::KeepBoth,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(summary.files, 1);
        assert_eq!(summary.removal, None, "a copy removes nothing");
        assert_eq!(std::fs::read(mac.path().join("a.txt")).unwrap(), b"abc");
        assert!(phone.path().join("a.txt").exists());

        let request = dto::DownloadRequest {
            storage_id,
            source: "/".into(),
            names: vec!["a.txt".into()],
            destination: mac.path().to_string_lossy().into_owned(),
        };
        assert!(
            service
                .taken_on_mac(request.clone())
                .await
                .unwrap()
                .is_empty(),
            "the very file is there: no question"
        );
        std::fs::write(mac.path().join("a.txt"), b"changed on the Mac").unwrap();
        assert_eq!(service.taken_on_mac(request).await.unwrap(), ["a.txt"]);

        // The report came with it, and is saved where the file went.
        assert_eq!(summary.report.len(), 1);
        assert_eq!(summary.report[0].path, "a.txt");
        let saved = service
            .save_report(
                mac.path().to_string_lossy().into_owned(),
                "File\ta.txt\n".into(),
            )
            .await
            .unwrap();
        assert!(saved.ends_with(".tsv"));
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "File\ta.txt\n");
    }

    #[tokio::test]
    async fn a_move_is_reachable_through_the_transfer_trait_and_removes_the_original() {
        let phone = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        std::fs::write(phone.path().join("a.txt"), b"abc").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(mac.path().to_path_buf())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });

        let summary = service
            .download(
                dto::Paste::Move,
                dto::DownloadRequest {
                    storage_id,
                    source: "/".into(),
                    names: vec!["a.txt".into()],
                    destination: mac.path().to_string_lossy().into_owned(),
                },
                dto::Conflict::KeepBoth,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(summary.removal.map(|removal| removal.removed), Some(1));
        assert_eq!(std::fs::read(mac.path().join("a.txt")).unwrap(), b"abc");
        assert!(!phone.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn a_copy_inside_the_phone_is_reachable_through_the_transfer_trait() {
        let phone = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        std::fs::create_dir(phone.path().join("Music")).unwrap();
        std::fs::write(phone.path().join("a.txt"), b"abc").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(mac.path().to_path_buf())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });

        let summary = service
            .paste_on_phone(
                dto::PasteRequest {
                    mode: dto::Paste::Copy,
                    from_storage: storage_id,
                    from: "/".into(),
                    names: vec!["a.txt".into()],
                    to_storage: storage_id,
                    to: "/Music".into(),
                },
                false,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert!(summary.through_mac);
        assert_eq!(summary.files, 1);
        assert_eq!(
            std::fs::read(phone.path().join("Music/a.txt")).unwrap(),
            b"abc"
        );
        assert!(
            phone.path().join("a.txt").exists(),
            "a copy leaves the original"
        );
    }

    #[tokio::test]
    async fn a_retried_move_inside_the_phone_merges_through_the_trait() {
        let phone = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(phone.path().join("Music/Album")).unwrap();
        std::fs::create_dir_all(phone.path().join("Download")).unwrap();
        std::fs::write(phone.path().join("Music/Album/a.txt"), b"aa").unwrap();
        std::fs::write(phone.path().join("Music/Album/b.txt"), b"bb").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(Arc::clone(&device), explorer)),
            local: Arc::new(local::Service::new(tempfile::tempdir().unwrap().keep())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });
        let request = |mode| dto::PasteRequest {
            mode,
            from_storage: storage_id,
            from: "/Music".into(),
            names: vec!["Album".into()],
            to_storage: storage_id,
            to: "/Download".into(),
        };
        // A first try that stopped after one file: Album is there, b is not.
        service
            .paste_on_phone(
                request(dto::Paste::Copy),
                false,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();
        std::fs::remove_file(phone.path().join("Download/Album/b.txt")).unwrap();
        // (The phone is read again, as it is when it is plugged back in.)
        device.connect().await.unwrap();

        let summary = service
            .paste_on_phone(
                request(dto::Paste::Move),
                true,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(summary.merged, 1);
        assert_eq!(summary.already_there, 1);
        assert!(phone.path().join("Download/Album/b.txt").exists());
        assert!(!phone.path().join("Download/Album (1)").exists());
        assert!(!phone.path().join("Music/Album").exists(), "moved");
    }

    #[tokio::test]
    async fn a_move_to_the_phone_goes_through_the_trait_and_the_trash_it_was_given() {
        let phone = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        std::fs::write(mac.path().join("a.txt"), b"abc").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let trashed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (log, bin_path) = (Arc::clone(&trashed), bin.path().to_path_buf());
        // Never the user's Trash: the item goes into a folder of the test's.
        let service: Arc<dyn handler::transfer::Service> = Arc::new(transfers(
            device,
            explorer,
            Arc::new(local::Service::new(mac.path().to_path_buf())),
            move |path| {
                log.lock().unwrap().push(path.to_path_buf());
                std::fs::rename(path, bin_path.join("a.txt"))
            },
            transfer::ImportRecord::at(bin.path().join("imported")),
            bin.path().join("preview"),
            Presence {
                dock: Arc::new(local::RecordingDock::default()),
                awake: Arc::new(local::RecordingAwake::default()),
            },
        ));
        let request = dto::UploadRequest {
            sources: vec![mac.path().join("a.txt").to_string_lossy().into_owned()],
            storage_id,
            destination: "/".into(),
        };

        let summary = service
            .upload(
                dto::Paste::Move,
                request,
                dto::Conflict::KeepBoth,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read(phone.path().join("a.txt")).unwrap(), b"abc");
        assert!(!mac.path().join("a.txt").exists());
        assert_eq!(*trashed.lock().unwrap(), [mac.path().join("a.txt")]);
        let removal = summary.removal.unwrap();
        assert!(removal.from_mac);
        assert_eq!(removal.removed, 1);
    }

    #[tokio::test]
    async fn uploads_are_reachable_through_the_transfer_trait() {
        let phone = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        std::fs::write(phone.path().join("a.txt"), b"old").unwrap();
        std::fs::write(mac.path().join("a.txt"), b"new").unwrap();
        // Dated long before the phone's file, so it is not the same file.
        std::fs::File::options()
            .write(true)
            .open(mac.path().join("a.txt"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800))
            .unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage_id = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let service: Arc<dyn handler::transfer::Service> = Arc::new(Transfers {
            opener: None,
            transfer: Arc::new(transfer::Service::new(device, explorer)),
            local: Arc::new(local::Service::new(mac.path().to_path_buf())),
            dock: Arc::new(local::RecordingDock::default()),
            awake: Arc::new(local::RecordingAwake::default()),
        });
        let request = dto::UploadRequest {
            sources: vec![mac.path().join("a.txt").to_string_lossy().into_owned()],
            storage_id,
            destination: "/".into(),
        };

        let taken = service.existing(request.clone()).await.unwrap();
        let summary = service
            .upload(
                dto::Paste::Copy,
                request,
                dto::Conflict::Replace,
                dto::Cancel::new(),
                Box::new(|_| {}),
            )
            .await
            .unwrap();

        assert_eq!(taken, ["a.txt"]);
        assert_eq!(summary.replaced, 1);
        assert_eq!(std::fs::read(phone.path().join("a.txt")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn the_device_panel_finds_the_usual_places_through_the_explorer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        std::fs::create_dir_all(dir.path().join("Music")).unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::device::Service> = Arc::new(devices(device));

        let present = service
            .folders_present(
                storage,
                vec!["/DCIM/Camera".into(), "/Download".into(), "/Music".into()],
            )
            .await
            .unwrap();

        assert_eq!(present, ["/DCIM/Camera", "/Music"]);
    }

    #[tokio::test]
    async fn both_panes_count_what_is_in_a_folder_for_its_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        std::fs::write(dir.path().join("DCIM/a.jpg"), vec![0u8; 1_000]).unwrap();
        std::fs::write(dir.path().join("DCIM/Camera/b.jpg"), vec![0u8; 500]).unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let phone: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));
        let mac: Arc<dyn handler::explorer::Service> = Arc::new(MacFiles {
            files: Arc::new(local::Service::new(dir.path().to_path_buf())),
            thumbnails: Arc::new(thumbnail::Service::new(virtual_device(&dir))),
            opener: Arc::new(local::RecordingOpener::default()),
        });

        let on_phone = phone
            .folder_size(storage, "/DCIM", dto::Cancel::new())
            .await
            .unwrap();
        let mac_dcim = dir.path().join("DCIM").to_string_lossy().into_owned();
        let on_mac = mac
            .folder_size(0, &mac_dcim, dto::Cancel::new())
            .await
            .unwrap();

        for tally in [on_phone, on_mac] {
            assert_eq!((tally.files, tally.bytes), (2, 1_500), "{tally:?}");
        }
    }

    #[tokio::test]
    async fn both_panes_offer_the_folders_they_know_for_go_to_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let phone: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));
        let mac: Arc<dyn handler::explorer::Service> = Arc::new(MacFiles {
            files: Arc::new(local::Service::new(dir.path().to_path_buf())),
            thumbnails: Arc::new(thumbnail::Service::new(virtual_device(&dir))),
            opener: Arc::new(local::RecordingOpener::default()),
        });

        let mut on_mac = mac.folder_names(0, &dir.path().to_string_lossy()).await;
        on_mac.sort();
        assert_eq!(on_mac, [".hidden", "DCIM"], "folders, hidden ones too");
        assert!(mac.folder_names(0, "/no/such/folder").await.is_empty());

        assert!(
            phone.folder_names(storage, "/").await.is_empty(),
            "not read yet"
        );
        let _ = phone.list(storage, "/", true).await.unwrap();
        let mut on_phone = phone.folder_names(storage, "/").await;
        on_phone.sort();
        assert_eq!(on_phone, [".hidden", "DCIM"]);
    }

    #[tokio::test]
    async fn both_panes_plan_renames_by_the_name_rules() {
        let dir = tempfile::tempdir().unwrap();
        let phone: Arc<dyn handler::explorer::Service> =
            Arc::new(phone_files(virtual_device(&dir)));
        let mac: Arc<dyn handler::explorer::Service> = Arc::new(MacFiles {
            files: Arc::new(local::Service::new(dir.path().to_path_buf())),
            thumbnails: Arc::new(thumbnail::Service::new(virtual_device(&dir))),
            opener: Arc::new(local::RecordingOpener::default()),
        });
        let items = vec![("a.jpg".to_owned(), false), ("b.jpg".to_owned(), false)];
        let folder = vec!["a.jpg".to_owned(), "b.jpg".to_owned(), "x b.jpg".to_owned()];
        let pattern = dto::RenamePattern::Add {
            text: "x ".into(),
            at_end: false,
        };

        for pane in [phone, mac] {
            let plan = pane.plan_renames(&items, &folder, &pattern);
            assert_eq!(plan[0].to, "x a.jpg");
            assert_eq!(plan[1].problem, Some(dto::RenameProblem::Taken));
        }
    }

    #[test]
    fn phone_thumbnails_are_kept_in_the_apps_cache_folder() {
        assert_eq!(
            thumbnails_dir(std::path::Path::new("/Users/me")),
            PathBuf::from("/Users/me/Library/Caches/open-mtp-client/thumbnails")
        );
    }

    #[tokio::test]
    async fn the_phone_pane_keeps_its_thumbnails_by_the_folder_they_are_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM")).unwrap();
        std::fs::write(dir.path().join("DCIM/dot.png"), PNG).unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let home = tempfile::tempdir().unwrap();
        let service: Arc<dyn handler::explorer::Service> = Arc::new(PhoneFiles {
            files: Arc::new(explorer::Service::new(Arc::clone(&device))),
            thumbnails: Arc::new(thumbnails(device, home.path())),
        });
        let entries = service.list(storage, "/DCIM", false).await.unwrap();

        let thumbnail = service.thumbnail(storage, "/DCIM", &entries[0]).await;

        assert!(matches!(thumbnail, Ok(Some(_))));
        let kept: Vec<_> = std::fs::read_dir(thumbnails_dir(home.path()))
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .collect();
        assert_eq!(kept.len(), 1, "kept, not dropped at once");
        assert!(
            kept[0].windows(13).any(|part| part == b"/DCIM/dot.png"),
            "under its path"
        );
    }

    #[test]
    fn settings_live_in_application_support() {
        assert_eq!(
            settings_path(std::path::Path::new("/Users/me")),
            PathBuf::from("/Users/me/Library/Application Support/open-mtp-client/settings.json")
        );
    }

    #[test]
    fn settings_saved_as_mtprust_carry_over() {
        let home = tempfile::tempdir().unwrap();
        let changed = dto::Settings {
            folders_first: false,
            ..dto::Settings::default()
        };
        settings::Service::new(earlier_settings_path(home.path()))
            .save(&changed)
            .unwrap();

        let service = settings_service(home.path());

        assert_eq!(service.load(), changed);
        assert!(settings_path(home.path()).exists());
    }

    #[test]
    fn settings_are_reachable_through_the_handler_trait() {
        let dir = tempfile::tempdir().unwrap();
        let service: Arc<dyn handler::settings::Service> =
            Arc::new(settings::Service::new(settings_path(dir.path())));
        let changed = dto::Settings {
            status_bar: false,
            ..dto::Settings::default()
        };

        service.save(&changed).unwrap();

        assert_eq!(service.load(), changed);
    }

    /// A valid 1×1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn devices(device: Arc<device::Service>) -> Devices {
        Devices {
            explorer: Arc::new(explorer::Service::new(Arc::clone(&device))),
            device,
            doctor: Arc::new(doctor::Service::system()),
        }
    }

    /// A Mac where only macOS's camera services run, noting what is ended.
    #[derive(Default)]
    struct CamerasOnly {
        ended: std::sync::Mutex<Vec<String>>,
    }

    impl doctor::Processes for CamerasOnly {
        fn running(&self) -> Vec<String> {
            vec!["ptpcamerad".into(), "mscamerad-xpc".into()]
        }
        fn os_version(&self) -> Option<String> {
            None
        }
        fn quit_app(&self, _: &str) {}
        fn kill(&self, _: &str) {}
        fn force_kill(&self, process: &str) {
            self.ended.lock().unwrap().push(process.to_owned());
        }
    }

    struct NoUsb;

    impl doctor::Usb for NoUsb {
        fn phones(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn the_device_handler_frees_the_phone_through_the_doctor() {
        let dir = tempfile::tempdir().unwrap();
        let device = virtual_device(&dir);
        let processes = Arc::new(CamerasOnly::default());
        let devices = Devices {
            doctor: Arc::new(doctor::Service::new(processes.clone(), Arc::new(NoUsb))),
            ..devices(device)
        };

        assert!(handler::device::Service::free_from_camera(&devices).await);
        assert_eq!(
            *processes.ended.lock().unwrap(),
            ["ptpcamerad", "mscamerad-xpc"]
        );
    }

    fn phone_files(device: Arc<device::Service>) -> PhoneFiles {
        PhoneFiles {
            files: Arc::new(explorer::Service::new(Arc::clone(&device))),
            thumbnails: Arc::new(thumbnail::Service::new(device)),
        }
    }

    #[test]
    fn the_mac_pane_opens_the_privacy_settings_it_is_asked_for() {
        let opener = Arc::new(local::RecordingOpener::default());
        let files: Arc<dyn handler::explorer::Service> = Arc::new(MacFiles {
            opener: Arc::clone(&opener) as Arc<dyn local::Opener>,
            ..mac_files(local::Service::new(PathBuf::from("/Users/me")))
        });

        files.open_privacy_settings(dto::PrivacyPane::FullDiskAccess);

        assert_eq!(
            *opener.settings.lock().unwrap(),
            [dto::PrivacyPane::FullDiskAccess]
        );
    }

    // Undo (⌘Z) on the real services: the Mac's, in a test folder with a
    // Trash that only records, and the virtual phone's.

    /// A Mac pane over `dir` whose Trash moves items into `bin`.
    fn mac_pane_over(
        dir: &std::path::Path,
        bin: &std::path::Path,
    ) -> Arc<dyn handler::explorer::Service> {
        let bin = bin.to_path_buf();
        let local = local::Service::with_trash(
            dir.to_path_buf(),
            std::sync::Arc::new(move |path: &std::path::Path| {
                std::fs::rename(path, bin.join(path.file_name().unwrap()))
            }),
        );
        Arc::new(mac_files(local))
    }

    #[tokio::test]
    async fn a_rename_and_a_new_folder_on_the_mac_are_undone() {
        use crate::handler::explorer::undo::{Undo, undo};
        let (dir, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let home = dir.path().to_string_lossy().into_owned();
        std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
        std::fs::create_dir(dir.path().join("Trip")).unwrap();
        let mac = mac_pane_over(dir.path(), bin.path());

        let renamed = Undo::Renamed {
            folder: home.clone(),
            from: "a.txt".into(),
            to: "b.txt".into(),
        };
        let (folder, select) = undo(Arc::clone(&mac), 0, renamed.clone()).await.unwrap();
        assert_eq!(
            (folder.as_str(), select.as_slice()),
            (home.as_str(), ["a.txt".to_owned()].as_slice())
        );
        assert!(dir.path().join("a.txt").exists() && !dir.path().join("b.txt").exists());
        assert!(
            undo(Arc::clone(&mac), 0, renamed).await.is_err(),
            "b.txt is gone now: nothing to rename back"
        );

        let made = Undo::MadeFolder {
            folder: home.clone(),
            name: "Trip".into(),
        };
        std::fs::write(dir.path().join("Trip/.DS_Store"), b"").unwrap();
        undo(Arc::clone(&mac), 0, made).await.unwrap();
        assert!(
            !dir.path().join("Trip").exists(),
            "still empty (Finder's clutter aside): gone"
        );
        assert!(
            bin.path().join("Trip").exists(),
            "into the Trash, as Finder's undo does"
        );

        std::fs::create_dir(dir.path().join("Used")).unwrap();
        std::fs::write(dir.path().join("Used/kept.txt"), b"k").unwrap();
        let used = Undo::MadeFolder {
            folder: home,
            name: "Used".into(),
        };
        assert_eq!(
            undo(mac, 0, used).await,
            Err(dto::Error::ChangedSince("Used".into()))
        );
        assert!(
            dir.path().join("Used/kept.txt").exists(),
            "something in it now: left alone"
        );
    }

    #[tokio::test]
    async fn a_move_on_the_mac_is_undone_under_the_names_it_had() {
        use crate::handler::explorer::undo::{MovedStep, Undo, undo};
        let (dir, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let home = dir.path().to_string_lossy().into_owned();
        let trip = dir.path().join("Trip");
        std::fs::create_dir(&trip).unwrap();
        // a.jpg arrived as "a (1).jpg" because Trip had an a.jpg of its own.
        std::fs::write(trip.join("a.jpg"), b"theirs").unwrap();
        std::fs::write(trip.join("a (1).jpg"), b"mine").unwrap();
        std::fs::write(trip.join("b.jpg"), b"b").unwrap();
        let mac = mac_pane_over(dir.path(), bin.path());

        let moved = Undo::Moved(vec![MovedStep {
            from: home.clone(),
            into: trip.to_string_lossy().into_owned(),
            items: vec![
                ("a.jpg".into(), "a (1).jpg".into()),
                ("b.jpg".into(), "b.jpg".into()),
            ],
        }]);
        let (folder, select) = undo(mac, 0, moved).await.unwrap();

        assert_eq!(folder, home);
        assert_eq!(select, ["a.jpg", "b.jpg"]);
        assert_eq!(std::fs::read(dir.path().join("a.jpg")).unwrap(), b"mine");
        assert!(dir.path().join("b.jpg").exists());
        assert_eq!(
            std::fs::read(trip.join("a.jpg")).unwrap(),
            b"theirs",
            "never touched"
        );
        assert!(!trip.join("a (1).jpg").exists());
    }

    #[tokio::test]
    async fn a_rename_and_a_move_on_the_phone_are_undone() {
        use crate::handler::explorer::undo::{MovedStep, Undo, undo};
        let phone = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(phone.path().join("DCIM/Trip")).unwrap();
        std::fs::write(phone.path().join("DCIM/new.jpg"), b"n").unwrap();
        std::fs::write(phone.path().join("DCIM/Trip/b.jpg"), b"b").unwrap();
        let device = virtual_device(&phone);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let files: Arc<dyn handler::explorer::Service> = Arc::new(PhoneFiles {
            files: Arc::new(explorer::Service::new(Arc::clone(&device))),
            thumbnails: Arc::new(thumbnail::Service::new(device)),
        });

        let renamed = Undo::Renamed {
            folder: "/DCIM".into(),
            from: "old.jpg".into(),
            to: "new.jpg".into(),
        };
        undo(Arc::clone(&files), storage, renamed).await.unwrap();
        assert!(phone.path().join("DCIM/old.jpg").exists());

        let moved = Undo::Moved(vec![MovedStep {
            from: "/DCIM".into(),
            into: "/DCIM/Trip".into(),
            items: vec![("b.jpg".into(), "b.jpg".into())],
        }]);
        let (folder, select) = undo(files, storage, moved).await.unwrap();
        assert_eq!(
            (folder.as_str(), select.as_slice()),
            ("/DCIM", ["b.jpg".to_owned()].as_slice())
        );
        assert!(phone.path().join("DCIM/b.jpg").exists());
        assert!(!phone.path().join("DCIM/Trip/b.jpg").exists());
    }

    fn mac_files(local: local::Service) -> MacFiles {
        let no_phone = Arc::new(device::Service::new(device::Source::Usb));
        MacFiles {
            files: Arc::new(local),
            thumbnails: Arc::new(thumbnail::Service::new(no_phone)),
            opener: Arc::new(local::RecordingOpener::default()),
        }
    }

    #[tokio::test]
    async fn phone_thumbnails_are_reachable_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dot.png"), PNG).unwrap();
        let device = virtual_device(&dir);
        device.connect().await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let service: Arc<dyn handler::explorer::Service> = Arc::new(phone_files(device));
        let entries = service.list(storage, "/", false).await.unwrap();

        let thumbnail = service.thumbnail(storage, "/", &entries[0]).await.unwrap();

        assert_eq!(thumbnail.map(|t| (t.width, t.height)), Some((1, 1)));
    }

    #[tokio::test]
    async fn mac_thumbnails_are_reachable_through_the_pane_trait() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dot.png"), PNG).unwrap();
        let local = local::Service::new(dir.path().to_path_buf());
        let home = local.home();
        let service: Arc<dyn handler::explorer::Service> = Arc::new(mac_files(local));
        let entries = service.list(0, &home, false).await.unwrap();

        let thumbnail = service.thumbnail(0, &home, &entries[0]).await.unwrap();

        assert_eq!(thumbnail.map(|t| (t.width, t.height)), Some((1, 1)));
    }

    #[test]
    fn the_bundled_command_line_tool_is_found_only_inside_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("open-mtp-client.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("open-mtp-client");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(bundled_cli_beside(&exe), None, "a bundle without it");

        std::fs::write(macos.join("open-mtp-client-cli"), b"").unwrap();
        assert_eq!(
            bundled_cli_beside(&exe),
            Some(macos.join("open-mtp-client-cli"))
        );

        let loose = dir.path().join("target/release");
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(loose.join("open-mtp-client-cli"), b"").unwrap();
        assert_eq!(
            bundled_cli_beside(&loose.join("open-mtp-client")),
            None,
            "a build run from Cargo isn't the app"
        );
        let not_an_app = dir.path().join("Thing/Contents/MacOS");
        std::fs::create_dir_all(&not_an_app).unwrap();
        std::fs::write(not_an_app.join("open-mtp-client-cli"), b"").unwrap();
        assert_eq!(bundled_cli_beside(&not_an_app.join("x")), None);
        // Inside an app, but not where its program is.
        let resources = dir.path().join("open-mtp-client.app/Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(resources.join("open-mtp-client-cli"), b"").unwrap();
        assert_eq!(bundled_cli_beside(&resources.join("x")), None);
        // Where an app's program goes, but not in an app.
        let zipped = dir.path().join("Thing.zip/Contents/MacOS");
        std::fs::create_dir_all(&zipped).unwrap();
        std::fs::write(zipped.join("open-mtp-client-cli"), b"").unwrap();
        assert_eq!(bundled_cli_beside(&zipped.join("x")), None);
        assert_eq!(bundled_cli_beside(std::path::Path::new("/")), None);
    }

    #[test]
    fn the_app_is_built_once_for_both_kinds_of_mac() {
        let makefile = include_str!("../../Makefile");
        let line = |start: &str| {
            makefile
                .lines()
                .find(|line| line.starts_with(start))
                .unwrap_or_else(|| panic!("the Makefile has a {start} line"))
        };

        assert_eq!(
            line("TARGETS := "),
            "TARGETS := aarch64-apple-darwin x86_64-apple-darwin",
            "Apple Silicon and Intel"
        );
        assert_eq!(
            line("app: "),
            "app: universal",
            "the app holds the joined binary"
        );
        assert!(
            line("MACOS_MIN := ").contains("LSMinimumSystemVersion"),
            "built for the oldest macOS Info.plist promises, read from it, so the two can't differ"
        );
        for step in [
            "MACOSX_DEPLOYMENT_TARGET=$(MACOS_MIN) cargo build --release",
            "lipo -create -output $(UNIVERSAL)",
            "for arch in arm64 x86_64; do lipo $(UNIVERSAL) -verify_arch $$arch",
            "cp $(UNIVERSAL) $(APP)",
            "codesign --verify --strict $(APP)",
            // The command-line tool, joined, checked, inside, signed first,
            // and run.
            "cargo build --release -p open-mtp-client -p open-mtp-client-cli",
            "lipo -create -output $(UNIVERSAL_CLI)",
            "lipo $(UNIVERSAL_CLI) -verify_arch $$arch",
            "cp $(UNIVERSAL_CLI) $(APP)/Contents/MacOS/open-mtp-client-cli",
            "codesign --force --sign - $(APP)/Contents/MacOS/open-mtp-client-cli",
            "$(APP)/Contents/MacOS/open-mtp-client-cli --help",
        ] {
            assert!(makefile.contains(step), "make app: {step}");
        }
        assert!(
            include_str!("../../packaging/Info.plist")
                .contains("<key>LSMinimumSystemVersion</key>")
        );
        let signs_nested = makefile
            .find("codesign --force --sign - $(APP)/Contents/MacOS/open-mtp-client-cli")
            .unwrap();
        let signs_bundle = makefile.find("codesign --force --sign - $(APP)\n").unwrap();
        assert!(
            signs_nested < signs_bundle,
            "nested code is signed before the bundle"
        );
    }

    /// Runs `make prune` in a folder holding the real Makefile and a fake
    /// `cargo` that only writes down what it was asked, so nothing is built.
    fn prune(folder: &std::path::Path, max_gb: u32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = folder.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let cargo = bin.join("cargo");
        std::fs::write(
            &cargo,
            format!("#!/bin/sh\necho \"$*\" >> {}/calls\n", folder.display()),
        )
        .unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(folder.join("Makefile"), include_str!("../../Makefile")).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let made = std::process::Command::new("make")
            .args(["-C"])
            .arg(folder)
            .args(["prune", &format!("TARGET_MAX_GB={max_gb}")])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(made.status.success(), "make prune: {made:?}");
        std::fs::read_to_string(folder.join("calls")).unwrap_or_default()
    }

    #[test]
    fn a_target_folder_past_its_size_loses_the_debug_and_coverage_builds_only() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        for (folder, file) in [("llvm-cov-target", "a"), ("release", "b")] {
            std::fs::create_dir_all(target.join(folder)).unwrap();
            std::fs::write(target.join(folder).join(file), vec![0u8; 4096]).unwrap();
        }

        assert_eq!(
            prune(dir.path(), 1),
            "",
            "under the limit nothing is touched"
        );
        assert!(target.join("llvm-cov-target").exists());

        assert_eq!(
            prune(dir.path(), 0),
            "clean --profile dev\n",
            "over it only the debug profile is cleaned"
        );
        assert!(!target.join("llvm-cov-target").exists(), "coverage build");
        assert!(target.join("release/b").exists(), "release builds stay");
    }

    #[test]
    fn pruning_a_checkout_that_was_never_built_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(prune(dir.path(), 0), "");
    }

    #[test]
    fn every_target_that_compiles_debug_code_prunes_first() {
        let makefile = include_str!("../../Makefile");
        for target in [
            "run",
            "run-virtual",
            "run-virtual-two",
            "build",
            "test",
            "lint",
            "coverage",
        ] {
            assert!(
                makefile
                    .lines()
                    .any(|line| line == format!("{target}: prune")),
                "make {target} prunes target/ first"
            );
        }
    }
}
