//! Fake services for handler and app tests.

use std::sync::Mutex;

use internal::dto;

use super::{
    crash, device, explorer, help, import, info, mirror, places, preview, search, settings,
    transfer,
};

pub fn phone() -> dto::Device {
    dto::Device {
        abilities: dto::Abilities::default(),
        link: None,
        key: "ABC123".into(),
        manufacturer: "Google".into(),
        model: "Pixel 9".into(),
        serial_number: "ABC123".into(),
        firmware_version: "15".into(),
        android: true,
    }
}

pub fn storage(id: u64, name: &str) -> dto::Storage {
    dto::Storage {
        id,
        name: name.into(),
        capacity_bytes: 64_000_000_000,
        free_bytes: 16_000_000_000,
        is_writable: true,
    }
}

pub fn folder(name: &str) -> dto::Entry {
    dto::Entry {
        id: 0,
        name: name.into(),
        is_folder: true,
        size_bytes: 0,
        modified: None,
    }
}

pub fn file(name: &str) -> dto::Entry {
    dto::Entry {
        id: 0,
        name: name.into(),
        is_folder: false,
        size_bytes: 1_000,
        modified: None,
    }
}

/// A device service that answers from fixed results.
pub struct FakeDevice {
    pub connect: Result<dto::Device, dto::Error>,
    pub storages: Result<Vec<dto::Storage>, dto::Error>,
    /// What the hotplug watch reports, in order.
    pub plugs: Vec<dto::DeviceChange>,
    pub watch_fails: bool,
    /// Changes the phone reports, in order; then it is "gone".
    pub changes: Mutex<std::collections::VecDeque<dto::ContentChange>>,
    /// What the doctor finds for any failure.
    pub diagnosis: dto::Diagnosis,
    /// Names of the apps asked to quit, one entry per call.
    pub released: Mutex<Vec<Vec<String>>>,
    /// The phones plugged in; asking for one of them by key opens it.
    pub phones: Vec<dto::Phone>,
    /// The phone each connect asked for.
    pub asked: Mutex<Vec<Option<String>>>,
    /// The battery level each question about it is answered with, in turn;
    /// the last one stays.
    pub battery: Mutex<Vec<Result<Option<u8>, dto::Error>>>,
    /// How many times the battery was asked about.
    pub battery_asks: std::sync::atomic::AtomicUsize,
    /// The folders on every storage, and each storage asked about.
    pub present: Vec<String>,
    pub places_asked: Mutex<Vec<u64>>,
    /// What the first connects answer, in turn, before `connect` does.
    pub first_connects: Mutex<std::collections::VecDeque<Result<dto::Device, dto::Error>>>,
    /// Whether macOS's camera services were all that held the phone, and
    /// each time they were asked to be ended.
    pub cameras_only: bool,
    pub cameras_ended: std::sync::atomic::AtomicUsize,
    /// The call that panics (`"connect"`, `"phones"`, `"battery"`,
    /// `"storages"`, `"disconnect"`, `"folders_present"` or `"release"`).
    pub panics_in: Option<&'static str>,
}

/// A phone plugged in, as the fake lists it.
pub fn plugged(key: &str, model: &str) -> dto::Phone {
    dto::Phone {
        key: key.into(),
        manufacturer: "Google".into(),
        model: model.into(),
        serial: key.into(),
    }
}

impl FakeDevice {
    pub fn with_storages(storages: Vec<dto::Storage>) -> Self {
        Self {
            connect: Ok(phone()),
            storages: Ok(storages),
            plugs: Vec::new(),
            watch_fails: false,
            changes: Mutex::new(std::collections::VecDeque::new()),
            diagnosis: dto::Diagnosis::default(),
            released: Mutex::new(Vec::new()),
            phones: Vec::new(),
            asked: Mutex::new(Vec::new()),
            present: Vec::new(),
            places_asked: Mutex::new(Vec::new()),
            battery: Mutex::new(vec![Ok(None)]),
            battery_asks: std::sync::atomic::AtomicUsize::new(0),
            first_connects: Mutex::new(std::collections::VecDeque::new()),
            cameras_only: false,
            cameras_ended: std::sync::atomic::AtomicUsize::new(0),
            panics_in: None,
        }
    }

    /// Panics if this is the call a test asked to break.
    fn breaks_in(&self, call: &'static str) {
        assert!(self.panics_in != Some(call), "the fake {call} broke");
    }
}

#[async_trait::async_trait]
impl device::Service for FakeDevice {
    async fn connect(&self, prefer: Option<String>) -> Result<dto::Device, dto::Error> {
        self.breaks_in("connect");
        self.asked.lock().unwrap().push(prefer.clone());
        if let Some(first) = self.first_connects.lock().unwrap().pop_front() {
            return first;
        }
        let Some(key) = prefer else {
            return self.connect.clone();
        };
        match self.phones.iter().find(|phone| phone.key == key) {
            Some(phone) => Ok(dto::Device {
                abilities: dto::Abilities::default(),
                link: None,
                key: phone.key.clone(),
                manufacturer: phone.manufacturer.clone(),
                model: phone.model.clone(),
                serial_number: phone.serial.clone(),
                firmware_version: String::new(),
                android: true,
            }),
            None if self.phones.is_empty() => self.connect.clone(),
            None => Err(dto::Error::PhoneUnplugged),
        }
    }

    async fn phones(&self) -> Result<Vec<dto::Phone>, dto::Error> {
        self.breaks_in("phones");
        Ok(self.phones.clone())
    }

    async fn battery(&self) -> Result<Option<u8>, dto::Error> {
        self.breaks_in("battery");
        self.battery_asks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut answers = self.battery.lock().unwrap();
        if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers[0].clone()
        }
    }

    async fn storages(&self) -> Result<Vec<dto::Storage>, dto::Error> {
        self.breaks_in("storages");
        self.storages.clone()
    }

    async fn disconnect(&self) -> Result<(), dto::Error> {
        self.breaks_in("disconnect");
        Ok(())
    }

    fn watch(
        &self,
    ) -> Result<iced::futures::stream::BoxStream<'static, dto::DeviceChange>, dto::Error> {
        use iced::futures::StreamExt;
        if self.watch_fails {
            return Err(dto::Error::General("no USB".into()));
        }
        Ok(iced::futures::stream::iter(self.plugs.clone()).boxed())
    }

    async fn next_change(&self) -> Result<dto::ContentChange, dto::Error> {
        self.changes
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(dto::Error::Disconnected)
    }

    async fn diagnose(&self, _failure: &dto::Error) -> dto::Diagnosis {
        self.diagnosis.clone()
    }

    async fn folders_present(
        &self,
        storage_id: u64,
        paths: Vec<String>,
    ) -> Result<Vec<String>, dto::Error> {
        self.breaks_in("folders_present");
        self.places_asked.lock().unwrap().push(storage_id);
        Ok(paths
            .into_iter()
            .filter(|path| self.present.contains(path))
            .collect())
    }

    async fn release(&self, holders: &[dto::Holder]) {
        self.breaks_in("release");
        self.released
            .lock()
            .unwrap()
            .push(holders.iter().map(|h| h.name.clone()).collect());
    }

    async fn free_from_camera(&self) -> bool {
        self.cameras_ended
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.cameras_only
    }
}

/// An explorer service that answers from fixed results and records each call.
pub struct FakeExplorer {
    pub result: Result<Vec<dto::Entry>, dto::Error>,
    pub calls: Mutex<Vec<(u64, String, bool)>>,
    /// Answer for every edit; on success the edit echoes the requested name.
    pub edit: Result<(), dto::Error>,
    /// A rename of this name is refused, whatever `edit` says.
    pub refuse_rename: Option<&'static str>,
    /// What each folder's contents hold, by path, and each folder counted.
    pub folder_sizes: Vec<(String, u64)>,
    /// Folders whose count meets files the phone gives no size for.
    pub partly_sized: Vec<String>,
    pub sized: Mutex<Vec<String>>,
    /// What `folder_names` answers for any folder, and each folder asked.
    pub folders: Vec<String>,
    pub folders_asked: Mutex<Vec<String>>,
    pub edits: Mutex<Vec<String>>,
    /// Pieces `open_listing` hands out, in order, instead of the whole folder.
    pub pieces: Mutex<Option<std::collections::VecDeque<Result<dto::ListingUpdate, dto::Error>>>>,
    /// Why `open_listing` can't even start, if it can't.
    pub open_fails: Option<dto::Error>,
    /// The cancel flag of every listing started.
    pub opened: Mutex<Vec<dto::Cancel>>,
    /// Whether each listing started was allowed to use what was remembered.
    pub allowed: Mutex<Vec<bool>>,
    /// Every paste asked for.
    pub pastes: Mutex<Vec<dto::PasteRequest>>,
    /// Answers to pastes, in order; once spent, a paste does all it was asked.
    pub paste_results: Mutex<std::collections::VecDeque<Result<dto::PasteSummary, dto::Error>>>,
    /// Why every thumbnail fails, if it does.
    pub thumbnail_error: Option<dto::Error>,
    /// The call that panics (`"open_listing"`, `"next"`, `"paste"`,
    /// `"make_folder"`, `"rename"`, `"delete"`, `"folder_size"`,
    /// `"folder_names"` or `"thumbnail"`): the service code the pane waits on
    /// breaking, which no scripted answer can show.
    pub panics_in: Option<&'static str>,
}

/// A listing that hands out what it was given.
pub struct ScriptedListing(pub std::collections::VecDeque<Result<dto::ListingUpdate, dto::Error>>);

#[async_trait::async_trait]
impl explorer::Listing for ScriptedListing {
    async fn next(&mut self) -> Option<Result<dto::ListingUpdate, dto::Error>> {
        self.0.pop_front()
    }
}

/// A listing whose first piece never comes: reading it panics.
struct PanicsOnNext;

#[async_trait::async_trait]
impl explorer::Listing for PanicsOnNext {
    async fn next(&mut self) -> Option<Result<dto::ListingUpdate, dto::Error>> {
        panic!("the fake listing broke");
    }
}

/// A piece of a folder, `done` or not.
pub fn piece(names: &[&str], loaded: usize, total: usize, done: bool) -> dto::ListingUpdate {
    dto::ListingUpdate {
        entries: names.iter().map(|name| file(name)).collect(),
        restart: false,
        loaded,
        total,
        done,
    }
}

impl FakeExplorer {
    pub fn new(result: Result<Vec<dto::Entry>, dto::Error>) -> Self {
        Self {
            result,
            calls: Mutex::new(Vec::new()),
            edit: Ok(()),
            refuse_rename: None,
            folders: Vec::new(),
            folders_asked: Mutex::new(Vec::new()),
            folder_sizes: Vec::new(),
            partly_sized: Vec::new(),
            sized: Mutex::new(Vec::new()),
            edits: Mutex::new(Vec::new()),
            pieces: Mutex::new(None),
            open_fails: None,
            opened: Mutex::new(Vec::new()),
            allowed: Mutex::new(Vec::new()),
            pastes: Mutex::new(Vec::new()),
            paste_results: Mutex::new(std::collections::VecDeque::new()),
            thumbnail_error: None,
            panics_in: None,
        }
    }

    /// A service whose `call` panics.
    pub fn panicking_in(call: &'static str) -> Self {
        Self {
            panics_in: Some(call),
            ..Self::new(Ok(vec![]))
        }
    }

    /// Panics if this is the call a test asked to break.
    fn breaks_in(&self, call: &'static str) {
        assert!(self.panics_in != Some(call), "the fake {call} broke");
    }

    /// Answers the next pastes with `results`, in order.
    pub fn answering_pastes(results: Vec<Result<dto::PasteSummary, dto::Error>>) -> Self {
        let fake = Self::new(Ok(vec![]));
        *fake.paste_results.lock().unwrap() = results.into();
        fake
    }

    /// A folder that arrives as `pieces`.
    pub fn arriving(pieces: Vec<Result<dto::ListingUpdate, dto::Error>>) -> Self {
        let fake = Self::new(Ok(vec![]));
        *fake.pieces.lock().unwrap() = Some(pieces.into());
        fake
    }

    pub fn failing_edits(error: dto::Error) -> Self {
        Self {
            edit: Err(error),
            ..Self::new(Ok(vec![]))
        }
    }

    fn record(&self, edit: String) {
        self.edits.lock().unwrap().push(edit);
    }
}

#[async_trait::async_trait]
impl explorer::Service for FakeExplorer {
    async fn list(
        &self,
        volume_id: u64,
        path: &str,
        show_hidden: bool,
    ) -> Result<Vec<dto::Entry>, dto::Error> {
        self.calls
            .lock()
            .unwrap()
            .push((volume_id, path.to_owned(), show_hidden));
        self.result.clone()
    }

    fn sort(&self, entries: &mut [dto::Entry], sort: dto::Sort) {
        internal::service::explorer::sort(entries, sort);
    }

    async fn make_folder(
        &self,
        volume_id: u64,
        parent: &str,
        name: &str,
    ) -> Result<String, dto::Error> {
        self.breaks_in("make_folder");
        self.record(format!("mkdir {volume_id} {parent} {name}"));
        self.edit.clone().map(|()| name.to_owned())
    }

    async fn rename(
        &self,
        volume_id: u64,
        parent: &str,
        old: &str,
        new: &str,
    ) -> Result<String, dto::Error> {
        self.breaks_in("rename");
        self.record(format!("rename {volume_id} {parent} {old} -> {new}"));
        if self.refuse_rename == Some(old) {
            return Err(dto::Error::NameTaken(new.to_owned()));
        }
        self.edit.clone().map(|()| new.to_owned())
    }

    fn plan_renames(
        &self,
        items: &[(String, bool)],
        folder: &[String],
        pattern: &dto::RenamePattern,
    ) -> Vec<dto::Renaming> {
        internal::service::renaming::plan(items, folder, pattern)
    }

    async fn folder_size(
        &self,
        _volume_id: u64,
        path: &str,
        _cancel: dto::Cancel,
    ) -> Result<dto::Tally, dto::Error> {
        self.breaks_in("folder_size");
        self.sized.lock().unwrap().push(path.to_owned());
        self.folder_sizes
            .iter()
            .find(|(folder, _)| folder == path)
            .map(|(_, bytes)| dto::Tally {
                bytes: *bytes,
                unknown_sizes: usize::from(self.partly_sized.iter().any(|folder| folder == path)),
                ..dto::Tally::default()
            })
            .ok_or(dto::Error::PathNotFound(path.to_owned()))
    }

    async fn folder_names(&self, _volume_id: u64, path: &str) -> Vec<String> {
        self.breaks_in("folder_names");
        self.folders_asked.lock().unwrap().push(path.to_owned());
        self.folders.clone()
    }

    async fn delete(
        &self,
        volume_id: u64,
        parent: &str,
        names: &[String],
    ) -> Result<(), dto::Error> {
        self.breaks_in("delete");
        self.record(format!("delete {volume_id} {parent} {}", names.join(",")));
        self.edit.clone()
    }

    async fn paste(&self, request: &dto::PasteRequest) -> Result<dto::PasteSummary, dto::Error> {
        self.breaks_in("paste");
        self.pastes.lock().unwrap().push(request.clone());
        if let Some(answer) = self.paste_results.lock().unwrap().pop_front() {
            return answer;
        }
        Ok(dto::PasteSummary {
            mode: request.mode,
            done: request.names.len(),
            renamed: 0,
            into: request.to.clone(),
            names: request.names.clone(),
            failures: Vec::new(),
        })
    }

    /// `.jpg` files get a 2×2 thumbnail; everything else has none.
    async fn thumbnail(
        &self,
        _volume_id: u64,
        _folder: &str,
        entry: &dto::Entry,
    ) -> Result<Option<dto::Thumbnail>, dto::Error> {
        self.breaks_in("thumbnail");
        self.record(format!("thumbnail {}", entry.name));
        if let Some(error) = &self.thumbnail_error {
            return Err(error.clone());
        }
        Ok(entry.name.ends_with(".jpg").then(|| dto::Thumbnail {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        }))
    }

    fn reveal(&self, path: &str) {
        self.record(format!("reveal {path}"));
    }

    fn open_privacy_settings(&self, pane: dto::PrivacyPane) {
        self.record(format!("privacy settings {pane:?}"));
    }

    async fn open_listing(
        &self,
        volume_id: u64,
        path: &str,
        show_hidden: bool,
        cancel: dto::Cancel,
        allow_remembered: bool,
    ) -> Result<Box<dyn explorer::Listing>, dto::Error> {
        self.breaks_in("open_listing");
        self.opened.lock().unwrap().push(cancel);
        self.allowed.lock().unwrap().push(allow_remembered);
        if let Some(err) = &self.open_fails {
            return Err(err.clone());
        }
        if self.panics_in == Some("next") {
            return Ok(Box::new(PanicsOnNext));
        }
        if let Some(pieces) = self.pieces.lock().unwrap().take() {
            return Ok(Box::new(ScriptedListing(pieces)));
        }
        let entries = self.list(volume_id, path, show_hidden).await?;
        let whole = dto::ListingUpdate {
            loaded: entries.len(),
            total: entries.len(),
            entries,
            restart: false,
            done: true,
        };
        Ok(Box::new(ScriptedListing([Ok(whole)].into())))
    }
}

/// A transfer service that reports fixed progress, then a fixed result, and
/// records what it was asked to do.
pub struct FakeTransfer {
    pub result: Result<dto::TransferSummary, dto::Error>,
    pub updates: Vec<dto::TransferProgress>,
    pub requests: Mutex<Vec<dto::DownloadRequest>>,
    /// Whether each download was a copy or a move.
    pub modes: Mutex<Vec<dto::Paste>>,
    /// Every copy or move inside the phone that was asked for.
    pub pastes: Mutex<Vec<dto::PasteRequest>>,
    /// Whether each of those was the retry of a move.
    pub resumings: Mutex<Vec<bool>>,
    /// Every copy or move inside the Mac that was queued.
    pub mac_pastes: Mutex<Vec<dto::PasteRequest>>,
    pub uploads: Mutex<Vec<(dto::UploadRequest, dto::Conflict)>>,
    /// Whether each upload was a copy or a move.
    pub upload_modes: Mutex<Vec<dto::Paste>>,
    /// Every report saved: the folder and the text.
    pub reports: Mutex<Vec<(String, String)>>,
    pub revealed: Mutex<Vec<String>>,
    /// Every import that was run.
    pub imports: Mutex<Vec<dto::ImportRequest>>,
    /// Every mirror that was run.
    pub mirrors: Mutex<Vec<dto::MirrorRequest>>,
    pub checks: Mutex<Vec<dto::CheckRequest>>,
    /// What editing a phone file answers.
    pub edit: Result<dto::WorkingCopy, dto::Error>,
    pub edit_requests: Mutex<Vec<dto::EditRequest>>,
    /// The working copies found at start-up.
    pub copies: Vec<dto::WorkingCopy>,
    /// What each copy is now, by path (`None`: gone).
    pub copy_states: Mutex<std::collections::HashMap<String, Option<dto::CopyState>>>,
    pub sent_edits: Mutex<Vec<dto::WorkingCopy>>,
    pub discarded: Mutex<Vec<String>>,
    /// Every delete that keeps copies asked for.
    pub deletes: Mutex<Vec<dto::DeleteRequest>>,
    /// Every change of the Dock icon's bar.
    pub dock: Mutex<Vec<Option<u8>>>,
    /// Every notification.
    pub notes: Mutex<Vec<(String, String)>>,
    /// Every change of whether the Mac is kept awake.
    pub awake: Mutex<Vec<bool>>,
    /// Every sound played.
    pub sounds: Mutex<Vec<dto::Sound>>,
    /// A Mac that won't be kept awake.
    pub refuse_awake: bool,
    /// What the Mac already has of a download's names, and each download's
    /// answer about them.
    pub taken_on_mac: Vec<String>,
    pub download_conflicts: Mutex<Vec<dto::Conflict>>,
    /// The Mac folder can't be looked at.
    pub mac_unreadable: bool,
    /// A service with a bug in it: a download panics.
    pub panics_on_download: bool,
    /// A side call that panics: `"existing"`, `"edit_on_mac"`,
    /// `"working_copies"`, `"copy_states"`, `"save_report"` or
    /// `"discard_edit"`.
    pub panics_in: Option<&'static str>,
}

impl FakeTransfer {
    pub fn new(result: Result<dto::TransferSummary, dto::Error>) -> Self {
        Self {
            result,
            updates: Vec::new(),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(Vec::new()),
            pastes: Mutex::new(Vec::new()),
            resumings: Mutex::new(Vec::new()),
            mac_pastes: Mutex::new(Vec::new()),
            imports: Mutex::new(Vec::new()),
            mirrors: Mutex::new(Vec::new()),
            checks: Mutex::new(Vec::new()),
            edit: Err(dto::Error::Unsupported),
            edit_requests: Mutex::new(Vec::new()),
            copies: Vec::new(),
            copy_states: Mutex::new(std::collections::HashMap::new()),
            sent_edits: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            uploads: Mutex::new(Vec::new()),
            upload_modes: Mutex::new(Vec::new()),
            reports: Mutex::new(Vec::new()),
            revealed: Mutex::new(Vec::new()),
            dock: Mutex::new(Vec::new()),
            notes: Mutex::new(Vec::new()),
            sounds: Mutex::new(Vec::new()),
            awake: Mutex::new(Vec::new()),
            refuse_awake: false,
            taken_on_mac: Vec::new(),
            download_conflicts: Mutex::new(Vec::new()),
            mac_unreadable: false,
            panics_on_download: false,
            panics_in: None,
        }
    }

    /// Panics if this is the side call a test asked to break.
    fn breaks_in(&self, call: &'static str) {
        assert!(self.panics_in != Some(call), "the fake {call} broke");
    }

    pub fn reporting(self, updates: Vec<dto::TransferProgress>) -> Self {
        Self { updates, ..self }
    }
}

#[async_trait::async_trait]
impl transfer::Service for FakeTransfer {
    async fn download(
        &self,
        mode: dto::Paste,
        request: dto::DownloadRequest,
        conflict: dto::Conflict,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        assert!(!self.panics_on_download, "the transfer service broke");
        self.download_conflicts.lock().unwrap().push(conflict);
        self.modes.lock().unwrap().push(mode);
        self.requests.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn taken_on_mac(&self, request: dto::DownloadRequest) -> Result<Vec<String>, dto::Error> {
        if self.mac_unreadable {
            return Err(dto::Error::FolderAccessDenied(request.destination));
        }
        Ok(self.taken_on_mac.clone())
    }

    async fn mirror(
        &self,
        request: dto::MirrorRequest,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.mirrors.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn check_contents(
        &self,
        request: dto::CheckRequest,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.checks.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn edit_on_mac(&self, request: dto::EditRequest) -> Result<dto::WorkingCopy, dto::Error> {
        self.breaks_in("edit_on_mac");
        self.edit_requests.lock().unwrap().push(request.clone());
        self.edit.clone().map(|copy| dto::WorkingCopy {
            name: request.name,
            ..copy
        })
    }

    async fn working_copies(&self) -> Vec<dto::WorkingCopy> {
        self.breaks_in("working_copies");
        self.copies.clone()
    }

    async fn copy_states(&self, paths: Vec<String>) -> Vec<Option<dto::CopyState>> {
        self.breaks_in("copy_states");
        let states = self.copy_states.lock().unwrap();
        paths
            .iter()
            .map(|path| states.get(path).copied().flatten())
            .collect()
    }

    async fn send_edit(
        &self,
        copy: dto::WorkingCopy,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.sent_edits.lock().unwrap().push(copy);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn discard_edit(&self, copy: dto::WorkingCopy) -> Result<(), dto::Error> {
        self.breaks_in("discard_edit");
        self.discarded.lock().unwrap().push(copy.id);
        Ok(())
    }

    async fn delete_keeping_copy(
        &self,
        request: dto::DeleteRequest,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.deletes.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn import(
        &self,
        request: dto::ImportRequest,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.imports.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn paste_on_mac(
        &self,
        request: dto::PasteRequest,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.mac_pastes.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn paste_on_phone(
        &self,
        request: dto::PasteRequest,
        resuming: bool,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.resumings.lock().unwrap().push(resuming);
        self.pastes.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    /// Reports every source as taken.
    async fn existing(&self, request: dto::UploadRequest) -> Result<Vec<String>, dto::Error> {
        self.breaks_in("existing");
        Ok(request
            .sources
            .iter()
            .filter_map(|source| std::path::Path::new(source).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect())
    }

    async fn upload(
        &self,
        mode: dto::Paste,
        request: dto::UploadRequest,
        conflict: dto::Conflict,
        _cancel: dto::Cancel,
        progress: transfer::Progress,
    ) -> Result<dto::TransferSummary, dto::Error> {
        self.upload_modes.lock().unwrap().push(mode);
        self.uploads.lock().unwrap().push((request, conflict));
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    async fn save_report(&self, folder: String, text: String) -> Result<String, dto::Error> {
        self.breaks_in("save_report");
        let path = format!("{folder}/Transfer report.tsv");
        self.reports.lock().unwrap().push((folder, text));
        Ok(path)
    }

    fn reveal(&self, path: &str) {
        self.revealed.lock().unwrap().push(path.to_owned());
    }

    fn show_progress(&self, percent: Option<u8>) {
        self.dock.lock().unwrap().push(percent);
    }

    fn notify(&self, title: &str, body: &str) {
        self.notes
            .lock()
            .unwrap()
            .push((title.to_owned(), body.to_owned()));
    }

    fn keep_awake(&self, awake: bool) -> bool {
        self.awake.lock().unwrap().push(awake);
        awake && !self.refuse_awake
    }

    fn chime(&self, sound: dto::Sound) {
        self.sounds.lock().unwrap().push(sound);
    }
}

/// Settings kept in memory, starting from `loaded` (or the defaults).
#[derive(Default)]
pub struct FakeSettings {
    pub saved: Mutex<Vec<dto::Settings>>,
    /// What the file holds at start-up; the defaults when `None`.
    pub loaded: Option<dto::Settings>,
    /// Where the panes were left, at start-up and as saved since.
    pub folders: Mutex<dto::LastFolders>,
    pub folders_saved: std::sync::atomic::AtomicUsize,
    /// The unfinished transfers, at start-up and as saved since.
    pub transfers: Mutex<Vec<dto::SavedTransfer>>,
    pub transfers_saved: std::sync::atomic::AtomicUsize,
}

impl settings::Service for FakeSettings {
    fn load(&self) -> dto::Settings {
        self.loaded.unwrap_or_default()
    }

    fn load_folders(&self) -> dto::LastFolders {
        self.folders.lock().unwrap().clone()
    }

    fn save_folders(&self, folders: &dto::LastFolders) -> Result<(), dto::Error> {
        *self.folders.lock().unwrap() = folders.clone();
        self.folders_saved
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn save(&self, settings: &dto::Settings) -> Result<(), dto::Error> {
        self.saved.lock().unwrap().push(*settings);
        Ok(())
    }

    fn load_transfers(&self) -> Vec<dto::SavedTransfer> {
        self.transfers.lock().unwrap().clone()
    }

    fn save_transfers(&self, saved: &[dto::SavedTransfer]) -> Result<(), dto::Error> {
        *self.transfers.lock().unwrap() = saved.to_vec();
        self.transfers_saved
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// A help service that answers from fixed results and records what is asked.
/// The crash reports as a fake has them: what an earlier run left, and the
/// panics of this one (`panic_at`).
#[derive(Default)]
pub struct FakeCrash {
    pub unreported: Vec<dto::CrashNote>,
    pub open_fails: bool,
    /// Every report opened.
    pub opened: Mutex<Vec<std::path::PathBuf>>,
    /// How many times the user was told (the reports marked as seen).
    pub told: std::sync::atomic::AtomicUsize,
    pub panics: std::sync::atomic::AtomicU64,
    pub last: Mutex<Option<dto::CrashNote>>,
}

impl FakeCrash {
    /// A panic in this run, as the hook would have noted it.
    pub fn panic_at(&self, at_ms: u64, file: Option<&str>) {
        *self.last.lock().unwrap() = Some(dto::CrashNote {
            at_ms,
            location: "internal/x.rs:1:1".to_owned(),
            reported: false,
            file: file.map(std::path::PathBuf::from),
        });
        self.panics
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl crash::Service for FakeCrash {
    fn unreported(&self) -> Vec<dto::CrashNote> {
        self.unreported.clone()
    }

    fn mark_reported(&self) {
        self.told.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn panics(&self) -> u64 {
        self.panics.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn last_panic(&self) -> Option<dto::CrashNote> {
        self.last.lock().unwrap().clone()
    }

    fn open(&self, file: &std::path::Path) -> bool {
        self.opened.lock().unwrap().push(file.to_path_buf());
        !self.open_fails
    }
}

pub struct FakeHelp {
    pub diagnosis: dto::Diagnosis,
    pub mac: dto::MacInfo,
    /// Names of the apps asked to quit, one entry per call.
    pub released: Mutex<Vec<Vec<String>>>,
    pub links: Mutex<Vec<String>>,
    /// How many connection checks have been made.
    pub checks: std::sync::atomic::AtomicUsize,
    /// Every folder read and timed, and what a reading answers.
    pub readings: Mutex<Vec<(u64, String)>>,
    pub reading: Result<dto::ReadTiming, dto::Error>,
    /// Where the bundled command-line tool is, as an app run from its
    /// bundle would say.
    pub cli: Option<String>,
    /// The call that panics: `"checkup"`, `"release"`, `"mac"` or
    /// `"time_reading"`.
    pub panics_in: Option<&'static str>,
}

impl FakeHelp {
    fn breaks_in(&self, call: &'static str) {
        assert!(self.panics_in != Some(call), "the fake {call} broke");
    }
}

impl Default for FakeHelp {
    fn default() -> Self {
        Self {
            diagnosis: dto::Diagnosis {
                holders: Some(vec![]),
                phones: Some(vec![]),
                ..Default::default()
            },
            mac: dto::MacInfo {
                os_version: "15.1".into(),
                arch: "aarch64".into(),
                translated: false,
                crashes: Vec::new(),
            },
            released: Mutex::new(Vec::new()),
            links: Mutex::new(Vec::new()),
            checks: std::sync::atomic::AtomicUsize::new(0),
            readings: Mutex::new(Vec::new()),
            reading: Ok(dto::ReadTiming {
                items: 312,
                took: std::time::Duration::from_millis(1_430),
            }),
            cli: None,
            panics_in: None,
        }
    }
}

#[async_trait::async_trait]
impl help::Service for FakeHelp {
    async fn checkup(&self) -> dto::Diagnosis {
        self.breaks_in("checkup");
        self.checks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.diagnosis.clone()
    }

    async fn release(&self, holders: &[dto::Holder]) {
        self.breaks_in("release");
        self.released
            .lock()
            .unwrap()
            .push(holders.iter().map(|h| h.name.clone()).collect());
    }

    async fn mac(&self) -> dto::MacInfo {
        self.breaks_in("mac");
        self.mac.clone()
    }

    fn open_link(&self, url: &str) -> bool {
        self.links.lock().unwrap().push(url.to_owned());
        true
    }

    async fn time_reading(
        &self,
        storage_id: u64,
        path: &str,
    ) -> Result<dto::ReadTiming, dto::Error> {
        self.breaks_in("time_reading");
        self.readings
            .lock()
            .unwrap()
            .push((storage_id, path.to_owned()));
        self.reading.clone()
    }

    fn bundled_cli(&self) -> Option<String> {
        self.cli.clone()
    }
}

/// Runs a task to the end and returns what it produced: the messages it sent,
/// and the text it asked to put on the clipboard. This is how a test sees what
/// an update asked the world to do.
pub async fn drain<M: 'static>(task: iced::Task<M>) -> (Vec<M>, Vec<String>) {
    use iced::futures::StreamExt;

    let Some(stream) = iced_test::runtime::task::into_stream(task) else {
        return (Vec::new(), Vec::new());
    };
    let mut messages = Vec::new();
    let mut copied = Vec::new();
    let mut stream = stream;
    while let Some(action) = stream.next().await {
        match action {
            iced_test::runtime::Action::Output(message) => messages.push(message),
            iced_test::runtime::Action::Clipboard(
                iced_test::runtime::clipboard::Action::Write { contents, .. },
            ) => copied.push(contents),
            _ => {}
        }
    }
    (messages, copied)
}

/// Whether a task ends the app (`iced::exit`).
pub async fn exits<M: 'static>(task: iced::Task<M>) -> bool {
    use iced::futures::StreamExt;

    let Some(mut stream) = iced_test::runtime::task::into_stream(task) else {
        return false;
    };
    while let Some(action) = stream.next().await {
        if matches!(action, iced_test::runtime::Action::Exit) {
            return true;
        }
    }
    false
}

/// A search that walks a scripted phone: each walk hands over `pieces` in turn.
#[derive(Default)]
pub struct FakeSearch {
    pub pieces: Vec<Result<dto::WalkUpdate, dto::Error>>,
    /// Each walk asked for: the storage, and whether hidden files were wanted.
    pub walks: Mutex<Vec<(u64, bool)>>,
    /// Where it panics: `"walk"` (starting) or `"next"` (the first piece).
    pub panics_in: Option<&'static str>,
}

impl FakeSearch {
    /// One walk finding `found` (as folder, name, is_folder) in one piece.
    pub fn finding(found: &[(&str, &str, bool)]) -> Self {
        let found = found
            .iter()
            .map(|(folder, name, is_folder)| dto::Found {
                folder: (*folder).to_owned(),
                entry: dto::Entry {
                    id: 1,
                    name: (*name).to_owned(),
                    is_folder: *is_folder,
                    size_bytes: 1_000,
                    modified: None,
                },
            })
            .collect::<Vec<_>>();
        let items = found.len();
        Self {
            pieces: vec![Ok(dto::WalkUpdate {
                found,
                folders_read: 1,
                items,
                done: true,
                ..dto::WalkUpdate::default()
            })],
            walks: Mutex::new(Vec::new()),
            panics_in: None,
        }
    }
}

/// A walk whose first piece never comes: reading it panics.
struct WalkPanicsOnNext;

#[async_trait::async_trait]
impl search::Walking for WalkPanicsOnNext {
    async fn next(&mut self) -> Option<Result<dto::WalkUpdate, dto::Error>> {
        panic!("the fake walk broke");
    }
}

struct ScriptedWalk(std::collections::VecDeque<Result<dto::WalkUpdate, dto::Error>>);

#[async_trait::async_trait]
impl search::Walking for ScriptedWalk {
    async fn next(&mut self) -> Option<Result<dto::WalkUpdate, dto::Error>> {
        self.0.pop_front()
    }
}

#[async_trait::async_trait]
impl search::Service for FakeSearch {
    async fn walk(
        &self,
        storage_id: u64,
        include_hidden: bool,
        _cancel: dto::Cancel,
    ) -> Result<Box<dyn search::Walking>, dto::Error> {
        assert!(self.panics_in != Some("walk"), "the fake walk broke");
        self.walks
            .lock()
            .unwrap()
            .push((storage_id, include_hidden));
        if self.panics_in == Some("next") {
            return Ok(Box::new(WalkPanicsOnNext));
        }
        Ok(Box::new(ScriptedWalk(
            self.pieces.iter().cloned().collect(),
        )))
    }
}

/// Free Up Space's look and removal, answered from fixed results.
pub struct FakeSpace {
    pub report: Result<dto::SpaceReport, dto::Error>,
    /// What the walk reports before the look answers.
    pub progress: Vec<dto::SpaceProgress>,
    pub removal: Result<dto::Removal, dto::Error>,
    pub looks: Mutex<Vec<u64>>,
    pub removals: Mutex<Vec<dto::FreeUpRequest>>,
    /// The call that panics: `"look"` or `"free_up"`.
    pub panics_in: Option<&'static str>,
}

impl Default for FakeSpace {
    fn default() -> Self {
        Self {
            report: Ok(dto::SpaceReport::default()),
            progress: Vec::new(),
            removal: Ok(dto::Removal::default()),
            looks: Mutex::new(Vec::new()),
            removals: Mutex::new(Vec::new()),
            panics_in: None,
        }
    }
}

#[async_trait::async_trait]
impl crate::handler::space::Service for FakeSpace {
    async fn look(
        &self,
        storage_id: u64,
        _cancel: dto::Cancel,
        progress: crate::handler::space::Told<dto::SpaceProgress>,
    ) -> Result<dto::SpaceReport, dto::Error> {
        assert!(self.panics_in != Some("look"), "the fake look broke");
        self.looks.lock().unwrap().push(storage_id);
        for update in &self.progress {
            progress(*update);
        }
        self.report.clone()
    }

    async fn free_up(
        &self,
        request: dto::FreeUpRequest,
        _cancel: dto::Cancel,
        removed: crate::handler::space::Told<usize>,
    ) -> Result<dto::Removal, dto::Error> {
        assert!(self.panics_in != Some("free_up"), "the fake free_up broke");
        removed(request.paths.len());
        self.removals.lock().unwrap().push(request);
        self.removal.clone()
    }
}

/// The import dialog's look at what is new, answered from a fixed result.
pub struct FakeImport {
    pub result: Result<dto::ImportPreview, dto::Error>,
    /// The counts the walk reports before it answers.
    pub counts: Vec<usize>,
    /// Every preview asked for.
    pub asked: Mutex<Vec<dto::ImportRequest>>,
    /// The preview panics.
    pub panics: bool,
}

impl FakeImport {
    pub fn answering(result: Result<dto::ImportPreview, dto::Error>) -> Self {
        Self {
            result,
            counts: Vec::new(),
            asked: Mutex::new(Vec::new()),
            panics: false,
        }
    }
}

impl Default for FakeImport {
    fn default() -> Self {
        Self::answering(Ok(dto::ImportPreview {
            new_files: 3,
            new_bytes: 3_000,
            already: 2,
        }))
    }
}

#[async_trait::async_trait]
impl import::Service for FakeImport {
    async fn preview(
        &self,
        request: dto::ImportRequest,
        _cancel: dto::Cancel,
        seen: import::Seen,
    ) -> Result<dto::ImportPreview, dto::Error> {
        assert!(!self.panics, "the fake import preview broke");
        self.asked.lock().unwrap().push(request);
        for count in &self.counts {
            seen(*count);
        }
        self.result.clone()
    }
}

/// The mirror dialog's comparison, answered from a fixed result.
pub struct FakeMirror {
    pub result: Result<dto::MirrorPreview, dto::Error>,
    /// The counts the walk reports before it answers.
    pub counts: Vec<usize>,
    /// Every comparison asked for.
    pub asked: Mutex<Vec<dto::MirrorRequest>>,
    /// The comparison panics.
    pub panics: bool,
}

impl FakeMirror {
    pub fn answering(result: Result<dto::MirrorPreview, dto::Error>) -> Self {
        Self {
            result,
            counts: Vec::new(),
            asked: Mutex::new(Vec::new()),
            panics: false,
        }
    }
}

impl Default for FakeMirror {
    fn default() -> Self {
        Self::answering(Ok(dto::MirrorPreview {
            new_files: 2,
            new_bytes: 2_000,
            changed_files: 1,
            changed_bytes: 1_000,
            extras: 3,
            conflicts: 0,
            samples: dto::MirrorSamples {
                new: vec!["a.jpg".into(), "b.jpg".into()],
                changed: vec!["c.jpg".into()],
                extras: vec!["old".into()],
                conflicts: Vec::new(),
            },
        }))
    }
}

#[async_trait::async_trait]
impl mirror::Service for FakeMirror {
    async fn preview(
        &self,
        request: dto::MirrorRequest,
        _cancel: dto::Cancel,
        seen: mirror::Seen,
    ) -> Result<dto::MirrorPreview, dto::Error> {
        assert!(!self.panics, "the fake mirror comparison broke");
        self.asked.lock().unwrap().push(request);
        for count in &self.counts {
            seen(*count);
        }
        self.result.clone()
    }
}

/// Quick Look's fetches and looks, recorded; fetches answered from a fixed
/// result.
/// Files shared through the fake, and where the menu showed.
pub type SharedFiles = (Vec<String>, Option<(f32, f32)>);

pub struct FakePreview {
    pub result: Result<dto::Fetched, dto::Error>,
    pub updates: Vec<dto::TransferProgress>,
    /// Every fetch asked for.
    pub fetches: Mutex<Vec<dto::PreviewRequest>>,
    /// Every set of paths shown.
    pub looks: Mutex<Vec<Vec<String>>>,
    pub look_fails: bool,
    /// Every file opened in its app.
    pub opened: Mutex<Vec<String>>,
    /// Every set of files shared, and where the menu showed.
    pub shared: Mutex<Vec<SharedFiles>>,
    /// The fetch panics.
    pub panics: bool,
}

impl FakePreview {
    pub fn fetching(result: Result<dto::Fetched, dto::Error>) -> Self {
        Self {
            result,
            updates: Vec::new(),
            fetches: Mutex::new(Vec::new()),
            looks: Mutex::new(Vec::new()),
            look_fails: false,
            opened: Mutex::new(Vec::new()),
            shared: Mutex::new(Vec::new()),
            panics: false,
        }
    }
}

impl Default for FakePreview {
    fn default() -> Self {
        Self::fetching(Ok(dto::Fetched {
            paths: vec!["/tmp/preview/a.jpg".into()],
            ..dto::Fetched::default()
        }))
    }
}

#[async_trait::async_trait]
impl preview::Service for FakePreview {
    async fn fetch(
        &self,
        request: dto::PreviewRequest,
        _cancel: dto::Cancel,
        progress: preview::Progress,
    ) -> Result<dto::Fetched, dto::Error> {
        assert!(!self.panics, "the fake fetch broke");
        self.fetches.lock().unwrap().push(request);
        for update in &self.updates {
            progress(update.clone());
        }
        self.result.clone()
    }

    fn look(&self, paths: &[String]) -> Result<dto::Look, dto::Error> {
        if self.look_fails {
            return Err(dto::Error::General("qlmanage is missing".into()));
        }
        self.looks.lock().unwrap().push(paths.to_vec());
        Ok(dto::Look::Opened)
    }

    fn open(&self, path: &str) -> Result<(), dto::Error> {
        if self.look_fails {
            return Err(dto::Error::General("no app opens it".into()));
        }
        self.opened.lock().unwrap().push(path.to_owned());
        Ok(())
    }

    fn share(&self, paths: &[String], at: Option<(f32, f32)>) -> Result<(), dto::Error> {
        if self.look_fails {
            return Err(dto::Error::General("couldn't show the share menu".into()));
        }
        self.shared.lock().unwrap().push((paths.to_vec(), at));
        Ok(())
    }
}

/// The sidebar's places and pins, kept in memory.
pub struct FakePlaces {
    pub places: Vec<dto::Place>,
    pub pins: Mutex<Vec<dto::Pin>>,
    /// Every list of pins saved.
    pub saved: Mutex<Vec<Vec<dto::Pin>>>,
    pub save_fails: bool,
    pub mirrors: Mutex<Vec<dto::SavedMirror>>,
    /// Every list of saved mirrors written.
    pub mirrors_saved: Mutex<Vec<Vec<dto::SavedMirror>>>,
    /// Reading the places panics.
    pub panics: bool,
}

impl Default for FakePlaces {
    fn default() -> Self {
        Self {
            places: vec![
                dto::Place {
                    label: "Home".into(),
                    path: "/Users/me".into(),
                    volume: false,
                    space: None,
                },
                dto::Place {
                    label: "Downloads".into(),
                    path: "/Users/me/Downloads".into(),
                    volume: false,
                    space: None,
                },
                dto::Place {
                    label: "Backup".into(),
                    path: "/Volumes/Backup".into(),
                    volume: true,
                    space: Some(dto::DiskSpace {
                        free: 312_000_000_000,
                        capacity: 1_000_000_000_000,
                    }),
                },
            ],
            pins: Mutex::new(Vec::new()),
            saved: Mutex::new(Vec::new()),
            save_fails: false,
            mirrors: Mutex::new(Vec::new()),
            mirrors_saved: Mutex::new(Vec::new()),
            panics: false,
        }
    }
}

#[async_trait::async_trait]
impl places::Service for FakePlaces {
    async fn places(&self) -> Vec<dto::Place> {
        assert!(!self.panics, "the fake places broke");
        self.places.clone()
    }

    fn pins(&self) -> Vec<dto::Pin> {
        self.pins.lock().unwrap().clone()
    }

    fn save_pins(&self, pins: &[dto::Pin]) -> Result<(), dto::Error> {
        if self.save_fails {
            return Err(dto::Error::General("disk full".into()));
        }
        self.saved.lock().unwrap().push(pins.to_vec());
        Ok(())
    }

    fn mirrors(&self) -> Vec<dto::SavedMirror> {
        self.mirrors.lock().unwrap().clone()
    }

    fn save_mirrors(&self, mirrors: &[dto::SavedMirror]) -> Result<(), dto::Error> {
        if self.save_fails {
            return Err(dto::Error::General("disk full".into()));
        }
        self.mirrors_saved.lock().unwrap().push(mirrors.to_vec());
        Ok(())
    }
}

/// Counts every folder as `tally`, telling half of it first; or fails.
pub struct FakeInfo {
    pub result: Result<dto::Tally, dto::Error>,
    /// Every folder asked about, in order.
    pub asked: Mutex<Vec<String>>,
    /// The count panics.
    pub panics: bool,
}

impl FakeInfo {
    pub fn counting(tally: dto::Tally) -> Self {
        Self {
            result: Ok(tally),
            asked: Mutex::new(Vec::new()),
            panics: false,
        }
    }

    pub fn failing(error: dto::Error) -> Self {
        Self {
            result: Err(error),
            asked: Mutex::new(Vec::new()),
            panics: false,
        }
    }

    /// A count that panics.
    pub fn panicking() -> Self {
        Self {
            panics: true,
            ..Self::counting(dto::Tally::default())
        }
    }
}

#[async_trait::async_trait]
impl info::Service for FakeInfo {
    async fn tally(
        &self,
        _place: &info::Place,
        path: &str,
        _cancel: dto::Cancel,
        report: Box<dyn Fn(dto::Tally) + Send + Sync>,
    ) -> Result<dto::Tally, dto::Error> {
        assert!(!self.panics, "the fake count broke");
        self.asked.lock().unwrap().push(path.to_owned());
        if let Ok(tally) = &self.result {
            report(dto::Tally {
                files: tally.files / 2,
                ..dto::Tally::default()
            });
        }
        self.result.clone()
    }
}

/// The menu bar as tests see it: the menus put up, every state, and clicks.
#[derive(Default)]
pub struct FakeMenuBar {
    pub installed: Mutex<Vec<Vec<dto::Menu>>>,
    pub states: Mutex<Vec<dto::MenuState>>,
    /// Every submenu filled again: its title and entries.
    pub filled: Mutex<Vec<(String, Vec<dto::MenuEntry>)>>,
    on_click: Mutex<Option<internal::service::local::OnClick>>,
}

impl FakeMenuBar {
    /// Clicks the command `tag`; false before the menus are up.
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

impl crate::handler::menubar::Service for FakeMenuBar {
    fn install(&self, menus: &[dto::Menu], on_click: Box<dyn Fn(u32) + Send + Sync>) {
        self.installed.lock().unwrap().push(menus.to_vec());
        *self.on_click.lock().unwrap() = Some(on_click);
    }

    fn set_state(&self, state: &dto::MenuState) {
        self.states.lock().unwrap().push(state.clone());
    }

    fn fill(&self, title: &str, entries: &[dto::MenuEntry]) {
        self.filled
            .lock()
            .unwrap()
            .push((title.to_owned(), entries.to_vec()));
    }
}
