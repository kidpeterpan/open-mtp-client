use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::stream::{self, BoxStream, StreamExt};
use mtp_rs::mtp::MtpDevice;
use mtp_rs::{VirtualDeviceConfig, VirtualStorageConfig};
use tokio::sync::Mutex;

use super::mapper;
use crate::dto::{ContentChange, Device, DeviceChange, Error, Phone, Storage};
use crate::service::clock;
use crate::service::mapper as service_mapper;

const VIRTUAL_STORAGE_CAPACITY: u64 = 64 * 1024 * 1024 * 1024;
/// As long as the explorer gives a request answered at once (`bounded::ASK`).
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(60);

/// Where the device comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// The MTP devices plugged in over USB.
    Usb,
    /// A fake phone backed by a local folder, for development without hardware.
    Virtual(VirtualPhone),
    /// Several fake phones plugged in at once.
    Phones(Vec<VirtualPhone>),
}

#[derive(Debug, Clone)]
pub struct VirtualPhone {
    pub backing_dir: PathBuf,
    /// Must be unique among open virtual devices: mtp-rs's test hooks
    /// (`rekey_virtual_object`) find a virtual device by its serial.
    pub serial: String,
    pub read_only: bool,
    pub supports_rename: bool,
    /// Ranged reads (`GetPartialObject`); some cameras only send whole files.
    pub partial_reads: bool,
    /// Report changes made to the folder on disk, like a phone reports
    /// changes made on it.
    pub watch_files: bool,
    /// The battery level it reports; `None` for one that reports none.
    pub battery: Option<u8>,
    /// How big its storage is: what it reports as free is this less what the
    /// folder holds.
    pub capacity: u64,
}

impl VirtualPhone {
    /// A writable phone that can rename, like a current Android device.
    pub fn new(backing_dir: PathBuf, serial: String) -> Self {
        Self {
            backing_dir,
            serial,
            read_only: false,
            supports_rename: true,
            partial_reads: true,
            watch_files: false,
            battery: None,
            capacity: VIRTUAL_STORAGE_CAPACITY,
        }
    }
}

/// Owns the one open MTP session. Several phones can be plugged in
/// (`phones`), and one of them is open at a time: `connect_to` opens the one
/// asked for, closing the other first.
pub struct Service {
    source: Source,
    device: Mutex<Option<Open>>,
    /// Counts the sessions that have been opened or lost. What was learned
    /// about the phone on an earlier session (which handle is which file)
    /// can't be trusted on the next: Android re-keys files when it rescans.
    epoch: AtomicU64,
    /// When the phone last made progress, for telling a busy phone from a
    /// silent one: see `activity.rs`.
    activity: super::Activity,
    /// How long a question to the phone may go unanswered (with nothing else
    /// on the session making progress either).
    answer_within: std::time::Duration,
    /// The link a virtual phone says it has: none in the app, set in tests.
    virtual_link: Option<crate::dto::UsbSpeed>,
}

/// The open session, and which phone it is with.
struct Open {
    device: MtpDevice,
    key: String,
    /// Its USB link's speed, from the listing it was opened from.
    link: Option<crate::dto::UsbSpeed>,
}

/// A phone that can be opened: as USB lists it, or a fake one.
enum Candidate {
    Usb(mtp_rs::mtp::MtpDeviceInfo),
    Virtual(VirtualPhone),
}

impl Candidate {
    fn phone(&self) -> Phone {
        match self {
            Candidate::Usb(info) => mapper::to_phone(info),
            Candidate::Virtual(phone) => Phone {
                key: phone.serial.clone(),
                manufacturer: "open-mtp-client".into(),
                model: "Virtual Phone".into(),
                serial: phone.serial.clone(),
            },
        }
    }
}

impl Service {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            device: Mutex::new(None),
            epoch: AtomicU64::new(0),
            activity: super::Activity::default(),
            answer_within: ANSWER_WITHIN,
            virtual_link: None,
        }
    }

    /// The same service, its virtual phones saying their link is `link`
    /// (tests: a virtual phone has no USB link).
    #[cfg(test)]
    pub(crate) fn with_virtual_link(mut self, link: crate::dto::UsbSpeed) -> Self {
        self.virtual_link = Some(link);
        self
    }

    /// The phone open as `key`, as the app describes it: no request, all of
    /// it came when the phone was opened.
    fn describe(open: &Open) -> Device {
        let device = &open.device;
        let abilities = mapper::to_abilities(device.capabilities(), |code| {
            device.supports_operation(code)
        });
        mapper::to_device(
            &open.key,
            device.device_info(),
            device.is_android(),
            abilities,
            open.link,
        )
    }

    /// The same service, giving up on a silent phone sooner (tests).
    #[cfg(test)]
    pub(crate) fn answering_within(mut self, limit: std::time::Duration) -> Self {
        self.answer_within = limit;
        self
    }

    /// The record of the phone making progress; whoever hears from the phone
    /// (every answer, every chunk of a transfer) says so on it.
    pub(crate) fn activity(&self) -> super::Activity {
        self.activity.clone()
    }

    /// The open phone's `Phone::key`, if one is open.
    pub(crate) async fn open_key(&self) -> Option<String> {
        self.device
            .lock()
            .await
            .as_ref()
            .map(|open| open.key.clone())
    }

    /// Which session the phone is on; it changes whenever one is opened or
    /// lost.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Opens the phone plugged in, or returns the already-open one. With
    /// several plugged in and none open, it is `MultipleDevices`: which one
    /// is the user's choice (`connect_to`).
    pub async fn connect(&self) -> Result<Device, Error> {
        self.connect_to(None).await
    }

    /// Opens the phone `prefer` (a `Phone::key`) if it is plugged in, else
    /// the only phone plugged in. The phone already open is kept unless
    /// another one is asked for; then it is closed first, and what was learned
    /// on its session is not trusted on the next (the epoch moves).
    pub async fn connect_to(&self, prefer: Option<&str>) -> Result<Device, Error> {
        let mut slot = self.device.lock().await;
        if let Some(open) = slot.as_ref()
            && prefer.is_none_or(|key| key == open.key)
        {
            return Ok(Self::describe(open));
        }
        let candidates = self.candidates().await?;
        let chosen = match (slot.is_some(), prefer) {
            // Switching: only to the phone asked for, or the open one stays.
            (true, Some(key)) => candidates
                .iter()
                .find(|c| c.phone().key == key)
                .ok_or(Error::PhoneUnplugged)?,
            _ => choose(&candidates, prefer)?,
        };
        let key = chosen.phone().key;
        let link = match chosen {
            Candidate::Usb(info) => mapper::to_link(info.speed),
            Candidate::Virtual(_) => self.virtual_link,
        };
        if let Some(open) = slot.take() {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            // A close that fails, or never answers, still leaves the other
            // phone free to open.
            let _ = self.within(open.device.close()).await;
        }
        // Everything that needs the phone waits on this lock while a phone is
        // opened, so a phone that never answers must not hold it for good.
        let device = self.opening(self.open(chosen)).await?;
        let open = Open { device, key, link };
        let info = Self::describe(&open);
        *slot = Some(open);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        Ok(info)
    }

    /// `work` with the phone, waited for as long as a question may go
    /// unanswered with nothing else on the session making progress (`None`
    /// after that). A phone busy with a long transfer is not silent.
    pub(crate) async fn within<T>(&self, work: impl std::future::Future<Output = T>) -> Option<T> {
        self.activity.within(self.answer_within, false, work).await
    }

    /// One request to the open phone on session `epoch`, bounded as `within`
    /// says; a phone that stays silent is `Timeout`, and its session is
    /// dropped like a lost phone's (unless a newer one has opened since).
    pub(crate) async fn ask<T>(
        &self,
        epoch: u64,
        request: impl std::future::Future<Output = Result<T, mtp_rs::Error>>,
    ) -> Result<T, Error> {
        match self.within(request).await {
            None => Err(self.handle_error_at(epoch, mtp_rs::Error::Timeout).await),
            Some(Ok(answer)) => Ok(answer),
            Some(Err(err)) => Err(self.handle_error_at(epoch, err).await),
        }
    }

    /// Opening a phone, given as long as a question: one that never answers
    /// its first request is `Timeout`, so "Connecting…" can't stay up for ever.
    pub(super) async fn opening<T>(
        &self,
        open: impl std::future::Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        self.within(open).await.unwrap_or(Err(Error::Timeout))
    }

    /// How charged the open phone's battery is, in percent; `None` when it
    /// does not say. One request, given as long as any (the phone may be busy
    /// with a transfer). Only losing the phone is an error: a phone that
    /// refuses the question just has no level to show.
    pub async fn battery(&self) -> Result<Option<u8>, Error> {
        let (device, epoch) = self.session_at().await?;
        let answer = self.within(device.battery_level()).await;
        match answer {
            None => Err(self.handle_error_at(epoch, mtp_rs::Error::Timeout).await),
            Some(Ok(level)) => Ok(level),
            Some(Err(err)) => match self.handle_error_at(epoch, err).await {
                lost @ (Error::Disconnected | Error::Timeout) => Err(lost),
                _ => Ok(None),
            },
        }
    }

    /// The phones plugged in now, read from USB without opening any.
    pub async fn phones(&self) -> Result<Vec<Phone>, Error> {
        Ok(self
            .candidates()
            .await?
            .iter()
            .map(Candidate::phone)
            .collect())
    }

    async fn candidates(&self) -> Result<Vec<Candidate>, Error> {
        match &self.source {
            Source::Virtual(phone) => Ok(vec![Candidate::Virtual(phone.clone())]),
            Source::Phones(phones) => Ok(phones.iter().cloned().map(Candidate::Virtual).collect()),
            Source::Usb => {
                // Reading USB descriptors blocks; it asks no phone anything.
                let listed = tokio::task::spawn_blocking(MtpDevice::list_devices)
                    .await
                    .map_err(|_| Error::NoDevice)?;
                let listed = listed.map_err(|err| service_mapper::to_error(err, false))?;
                Ok(listed.into_iter().map(Candidate::Usb).collect())
            }
        }
    }

    /// Android lists no storages until the phone is unlocked and the user
    /// allows file access, so an empty list is reported as that, not as success.
    pub async fn storages(&self) -> Result<Vec<Storage>, Error> {
        let (device, epoch) = self.session_at().await?;
        let storages = self.ask(epoch, device.storages()).await?;
        if storages.is_empty() {
            return Err(Error::AllowStorageAccess);
        }
        Ok(storages
            .iter()
            .map(|s| mapper::to_storage(s.info()))
            .collect())
    }

    pub async fn disconnect(&self) -> Result<(), Error> {
        let Some(open) = self.device.lock().await.take() else {
            return Ok(());
        };
        self.epoch.fetch_add(1, Ordering::AcqRel);
        open.device
            .close()
            .await
            .map_err(|err| service_mapper::to_error(err, false))
    }

    /// Plug and unplug events for USB phones; phones already plugged in
    /// when watching starts come first, as `Plugged`. A virtual phone is
    /// never plugged in, so its watch stays silent.
    pub fn watch(&self) -> Result<BoxStream<'static, DeviceChange>, Error> {
        match &self.source {
            Source::Virtual(_) | Source::Phones(_) => Ok(stream::pending().boxed()),
            Source::Usb => {
                let watch = mtp_rs::mtp::watch_devices()
                    .map_err(|err| service_mapper::to_error(err, false))?;
                Ok(watch.map(mapper::to_device_change).boxed())
            }
        }
    }

    /// Waits for the next change on the connected phone that affects what is
    /// shown. Ends with an error once the phone is gone.
    pub async fn next_change(&self) -> Result<ContentChange, Error> {
        // Always waiting while a phone is open, so still waiting when the
        // user switches to another: its failure then is about the old one.
        let (device, epoch) = self.session_at().await?;
        loop {
            match device.next_event().await {
                Ok(event) => {
                    if let Some(change) = mapper::to_content_change(&event) {
                        return Ok(change);
                    }
                }
                // The device polls for events; a timeout only means none yet.
                Err(mtp_rs::Error::Timeout) if self.epoch() == epoch => {}
                Err(err) => return Err(self.handle_error_at(epoch, err).await),
            }
        }
    }

    async fn open(&self, chosen: &Candidate) -> Result<MtpDevice, Error> {
        match chosen {
            Candidate::Virtual(phone) => MtpDevice::builder()
                .open_virtual(virtual_config(phone.clone()))
                .await
                .map_err(|err| service_mapper::to_error(err, false)),
            // By where it is plugged in: unambiguous among the phones just
            // listed, serial number or not.
            Candidate::Usb(info) => MtpDevice::open_by_location(info.location_id)
                .await
                .map_err(|err| service_mapper::to_error(err, android_file_transfer_running())),
        }
    }

    pub(crate) async fn session(&self) -> Result<MtpDevice, Error> {
        Ok(self.session_at().await?.0)
    }

    /// The open session and its epoch, read together: an error on it is then
    /// handled with `handle_error_at`, which leaves a newer session alone.
    pub(crate) async fn session_at(&self) -> Result<(MtpDevice, u64), Error> {
        let slot = self.device.lock().await;
        let open = slot.as_ref().ok_or(Error::NotConnected)?;
        Ok((open.device.clone(), self.epoch()))
    }

    /// Maps a client error, dropping the session when the device has gone or
    /// stopped answering, so the next `connect` reopens it instead of reusing
    /// a dead or wedged handle.
    pub(crate) async fn handle_error(&self, err: mtp_rs::Error) -> Error {
        let err = service_mapper::to_error(err, false);
        if matches!(err, Error::Disconnected | Error::Timeout) {
            self.device.lock().await.take();
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
        err
    }

    /// Like `handle_error`, for an error on the session of `epoch`: the
    /// session is dropped only if it is still that one. After a switch to
    /// another phone, a late failure of a request to the first must not close
    /// the second.
    pub(crate) async fn handle_error_at(&self, epoch: u64, err: mtp_rs::Error) -> Error {
        let err = service_mapper::to_error(err, false);
        if matches!(err, Error::Disconnected | Error::Timeout) {
            let mut slot = self.device.lock().await;
            if self.epoch() == epoch && slot.take().is_some() {
                self.epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
        err
    }
}

/// Which phone to open: the one asked for if it is plugged in, else the only
/// one. With several and none asked for, picking one silently would risk
/// copying to the wrong phone, so the user is asked.
fn choose<'a>(candidates: &'a [Candidate], prefer: Option<&str>) -> Result<&'a Candidate, Error> {
    if let Some(key) = prefer
        && let Some(found) = candidates.iter().find(|c| c.phone().key == key)
    {
        return Ok(found);
    }
    match candidates {
        [] => Err(Error::NoDevice),
        [only] => Ok(only),
        _ => Err(Error::MultipleDevices),
    }
}

fn virtual_config(phone: VirtualPhone) -> VirtualDeviceConfig {
    VirtualDeviceConfig {
        manufacturer: "open-mtp-client".into(),
        model: "Virtual Phone".into(),
        serial: phone.serial,
        storages: vec![VirtualStorageConfig {
            description: "Internal shared storage".into(),
            capacity: phone.capacity,
            backing_dir: phone.backing_dir,
            read_only: phone.read_only,
        }],
        supports_rename: phone.supports_rename,
        supports_partial_object_64: phone.partial_reads,
        supports_partial_object: phone.partial_reads,
        watch_backing_dirs: phone.watch_files,
        battery_level: phone.battery,
        // It stands in for an Android phone.
        android: true,
        // A real phone writes zoneless dates in its own (usually our) zone.
        utc_offset: local_clock_offset(),
        ..Default::default()
    }
}

fn local_clock_offset() -> mtp_rs::mtp::UtcOffset {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    mtp_rs::mtp::UtcOffset::from_minutes(clock::local_offset_minutes(now))
        .unwrap_or(mtp_rs::mtp::UtcOffset::UTC)
}

/// Google's Android File Transfer agent grabs every MTP device on macOS, which
/// makes our own open fail with an exclusive-access error.
fn android_file_transfer_running() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    std::process::Command::new("pgrep")
        .args(["-x", "Android File Transfer Agent"])
        .output()
        .is_ok_and(|out| out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::virtual_source;

    fn virtual_service(dir: &tempfile::TempDir) -> Service {
        Service::new(virtual_source(dir.path()))
    }

    #[tokio::test]
    async fn connect_returns_device_identity() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);

        let device = service.connect().await.unwrap();

        assert_eq!(device.model, "Virtual Phone");
        assert_eq!(device.manufacturer, "open-mtp-client");
    }

    #[tokio::test]
    async fn the_phone_says_what_it_can_do_when_it_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let able = virtual_service(&dir).connect().await.unwrap().abilities;
        assert_eq!(
            able,
            crate::dto::Abilities {
                copy: true,
                moves: true,
                rename: true,
                partial_reads: true,
                partial_reads_64: true,
                thumbnails: true,
                prop_list: false,
                events: true,
            },
            "what the virtual phone lists"
        );

        let other = tempfile::tempdir().unwrap();
        let mut phone = VirtualPhone::new(other.path().to_path_buf(), "plain-0001".into());
        phone.partial_reads = false;
        phone.supports_rename = false;
        let plain = Service::new(Source::Virtual(phone))
            .connect()
            .await
            .unwrap()
            .abilities;
        assert!(!plain.partial_reads && !plain.partial_reads_64 && !plain.rename);
        assert!(plain.copy, "the rest as before");
    }

    #[tokio::test]
    async fn the_usb_link_comes_from_the_listing_and_stays_with_the_session() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            virtual_service(&dir).connect().await.unwrap().link,
            None,
            "a virtual phone has no USB link"
        );

        let other = tempfile::tempdir().unwrap();
        let service = Service::new(virtual_source(other.path()))
            .with_virtual_link(crate::dto::UsbSpeed::High);
        let first = service.connect().await.unwrap();
        let again = service.connect().await.unwrap();

        assert_eq!(first.link, Some(crate::dto::UsbSpeed::High));
        assert_eq!(again.link, first.link, "the open session keeps it");
    }

    #[tokio::test]
    async fn connect_twice_reuses_the_open_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);

        let first = service.connect().await.unwrap();
        let second = service.connect().await.unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn storages_lists_the_backing_storage() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();

        let storages = service.storages().await.unwrap();

        assert_eq!(storages.len(), 1);
        assert_eq!(storages[0].name, "Internal shared storage");
        assert_eq!(storages[0].capacity_bytes, VIRTUAL_STORAGE_CAPACITY);
        assert!(storages[0].is_writable);
    }

    #[tokio::test]
    async fn storages_before_connect_is_not_connected() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);

        assert_eq!(service.storages().await, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn disconnect_closes_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();

        service.disconnect().await.unwrap();

        assert_eq!(service.storages().await, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn disconnect_without_a_session_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);

        assert_eq!(service.disconnect().await, Ok(()));
    }

    #[tokio::test]
    async fn reconnect_after_disconnect_opens_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();
        service.disconnect().await.unwrap();

        service.connect().await.unwrap();

        assert_eq!(service.storages().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disconnected_error_drops_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();

        let err = service.handle_error(mtp_rs::Error::Disconnected).await;

        assert_eq!(err, Error::Disconnected);
        assert_eq!(service.storages().await, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn other_errors_keep_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();

        // A phone refusing one thing is still a working session.
        let err = service.handle_error(mtp_rs::Error::Unsupported).await;

        assert!(matches!(err, Error::General(_)));
        assert!(service.storages().await.is_ok());
    }

    #[tokio::test]
    async fn a_phone_that_stops_answering_loses_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        service.connect().await.unwrap();

        let err = service.handle_error(mtp_rs::Error::Timeout).await;

        // A wedged session can't be trusted again; connecting reopens it.
        assert_eq!(err, Error::Timeout);
        assert_eq!(service.storages().await, Err(Error::NotConnected));
        service.connect().await.unwrap();
        assert_eq!(service.storages().await.unwrap().len(), 1);
    }

    #[test]
    fn virtual_config_uses_the_given_folder_and_serial() {
        let config = virtual_config(VirtualPhone::new(PathBuf::from("/tmp/phone"), "abc".into()));

        assert_eq!(config.serial, "abc");
        assert_eq!(config.storages.len(), 1);
        assert_eq!(config.storages[0].backing_dir, PathBuf::from("/tmp/phone"));
        assert!(!config.storages[0].read_only);
        assert!(config.supports_rename);
    }

    #[test]
    fn virtual_config_can_model_a_limited_device() {
        let phone = VirtualPhone {
            read_only: true,
            supports_rename: false,
            partial_reads: false,
            watch_files: true,
            ..VirtualPhone::new(PathBuf::from("/tmp/phone"), "abc".into())
        };

        let config = virtual_config(phone);

        assert!(config.storages[0].read_only);
        assert!(!config.supports_rename);
        assert!(!config.supports_partial_object_64);
        assert!(!config.supports_partial_object);
        assert!(config.watch_backing_dirs);
    }

    fn fake(serial: &str) -> Candidate {
        Candidate::Virtual(VirtualPhone::new(PathBuf::from("/tmp/x"), serial.into()))
    }

    fn chosen(candidates: &[Candidate], prefer: Option<&str>) -> Result<String, Error> {
        choose(candidates, prefer).map(|c| c.phone().key)
    }

    #[test]
    fn the_phone_asked_for_is_opened_else_the_only_one_else_the_user_picks() {
        let two = [fake("a"), fake("b")];
        let one = [fake("a")];

        assert_eq!(chosen(&[], None), Err(Error::NoDevice));
        assert_eq!(chosen(&one, None), Ok("a".into()));
        assert_eq!(chosen(&two, None), Err(Error::MultipleDevices));
        assert_eq!(chosen(&two, Some("b")), Ok("b".into()));
        // The one asked for has gone: the only other phone, else ask.
        assert_eq!(chosen(&one, Some("gone")), Ok("a".into()));
        assert_eq!(chosen(&two, Some("gone")), Err(Error::MultipleDevices));
    }

    /// Two fake phones plugged in at once: "a" holds a.txt, "b" holds b.txt.
    struct Two {
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        service: Service,
        a: String,
        b: String,
    }

    fn two_phones() -> Two {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(one.path().join("a.txt"), b"a").unwrap();
        std::fs::write(two.path().join("b.txt"), b"b").unwrap();
        let (a, b) = (
            crate::service::testing::virtual_phone(one.path()),
            crate::service::testing::virtual_phone(two.path()),
        );
        let (a_key, b_key) = (a.serial.clone(), b.serial.clone());
        Two {
            service: Service::new(Source::Phones(vec![a, b])),
            _dirs: (one, two),
            a: a_key,
            b: b_key,
        }
    }

    /// The serial number of the phone the open session is with.
    async fn open_serial(service: &Service) -> String {
        service
            .session()
            .await
            .unwrap()
            .device_info()
            .serial_number
            .clone()
    }

    #[tokio::test]
    async fn every_phone_plugged_in_is_listed_and_none_is_opened_without_asking() {
        let two = two_phones();

        let phones = two.service.phones().await.unwrap();

        let keys: Vec<&str> = phones.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, [two.a.as_str(), two.b.as_str()]);
        assert_eq!(phones[0].model, "Virtual Phone");
        assert_eq!(phones[0].serial, two.a);
        assert_eq!(two.service.connect().await, Err(Error::MultipleDevices));
        assert_eq!(two.service.session().await.err(), Some(Error::NotConnected));
    }

    #[tokio::test]
    async fn switching_closes_one_phone_and_opens_the_other() {
        let two = two_phones();
        let a = two.service.connect_to(Some(&two.a)).await.unwrap();
        assert_eq!(a.key, two.a);
        assert_eq!(open_serial(&two.service).await, two.a);
        let on_a = two.service.epoch();

        let b = two.service.connect_to(Some(&two.b)).await.unwrap();

        assert_eq!(b.key, two.b);
        assert_eq!(open_serial(&two.service).await, two.b);
        assert!(
            two.service.epoch() > on_a,
            "nothing learned on a is trusted on b"
        );
        // Without asking for another, the open phone stays.
        assert_eq!(two.service.connect().await.unwrap().key, two.b);
        assert_eq!(
            two.service.connect_to(Some(&two.b)).await.unwrap().key,
            two.b
        );
    }

    #[tokio::test]
    async fn switching_to_a_phone_that_is_gone_keeps_the_open_one() {
        let two = two_phones();
        two.service.connect_to(Some(&two.a)).await.unwrap();
        let epoch = two.service.epoch();

        let gone = two.service.connect_to(Some("unplugged")).await;

        assert_eq!(gone, Err(Error::PhoneUnplugged));
        assert_eq!(open_serial(&two.service).await, two.a);
        assert_eq!(two.service.epoch(), epoch);
    }

    #[tokio::test]
    async fn a_late_failure_on_the_first_phone_leaves_the_second_open() {
        let two = two_phones();
        two.service.connect_to(Some(&two.a)).await.unwrap();
        let (_, on_a) = two.service.session_at().await.unwrap();
        two.service.connect_to(Some(&two.b)).await.unwrap();
        let on_b = two.service.epoch();

        let err = two
            .service
            .handle_error_at(on_a, mtp_rs::Error::Disconnected)
            .await;

        assert_eq!(err, Error::Disconnected, "still reported to its caller");
        assert_eq!(open_serial(&two.service).await, two.b);
        assert_eq!(two.service.epoch(), on_b);

        // The open session's own failure still drops it.
        two.service
            .handle_error_at(on_b, mtp_rs::Error::Timeout)
            .await;
        assert_eq!(two.service.session().await.err(), Some(Error::NotConnected));
        assert!(two.service.epoch() > on_b);
    }

    #[tokio::test]
    async fn the_phone_asked_for_is_opened_when_none_is_open() {
        let two = two_phones();

        let b = two.service.connect_to(Some(&two.b)).await.unwrap();

        assert_eq!(b.key, two.b);
        assert_eq!(b.serial_number, two.b);
        assert_eq!(two.service.storages().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_virtual_phone_is_never_plugged_in() {
        use futures::FutureExt;

        let dir = tempfile::tempdir().unwrap();
        let mut watch = virtual_service(&dir).watch().unwrap();

        assert!(watch.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn changes_need_a_connected_phone() {
        let dir = tempfile::tempdir().unwrap();

        let result = virtual_service(&dir).next_change().await;

        assert_eq!(result, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn a_file_appearing_on_the_phone_is_a_file_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut phone = crate::service::testing::virtual_phone(dir.path());
        phone.watch_files = true;
        let service = Service::new(Source::Virtual(phone));
        service.connect().await.unwrap();

        std::fs::write(dir.path().join("new.jpg"), b"x").unwrap();
        let change =
            tokio::time::timeout(std::time::Duration::from_secs(10), service.next_change())
                .await
                .expect("the virtual phone reports the new file");

        assert_eq!(change, Ok(ContentChange::Files));
    }

    #[tokio::test]
    async fn the_epoch_changes_whenever_a_session_is_opened_or_lost() {
        let dir = tempfile::tempdir().unwrap();
        let service = virtual_service(&dir);
        let never_connected = service.epoch();

        service.connect().await.unwrap();
        let first = service.epoch();
        service.connect().await.unwrap();
        assert_eq!(
            service.epoch(),
            first,
            "reusing the open session changes nothing"
        );

        service.disconnect().await.unwrap();
        let closed = service.epoch();
        service.connect().await.unwrap();
        let reopened = service.epoch();
        service.handle_error(mtp_rs::Error::Disconnected).await;
        let lost = service.epoch();
        service.connect().await.unwrap();
        service.handle_error(mtp_rs::Error::Timeout).await;
        let silent = service.epoch();
        service.connect().await.unwrap();
        // An error about one file leaves the session, and the epoch, alone.
        let before = service.epoch();
        service.handle_error(mtp_rs::Error::Unsupported).await;

        let all = [never_connected, first, closed, reopened, lost, silent];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "{all:?}");
            }
        }
        assert_eq!(service.epoch(), before);
        // Disconnecting with nothing open changes nothing either.
        service.disconnect().await.unwrap();
        let after = service.epoch();
        service.disconnect().await.unwrap();
        assert_eq!(service.epoch(), after);
    }

    fn charged(dir: &tempfile::TempDir, level: Option<u8>) -> (Service, String) {
        let phone = VirtualPhone {
            battery: level,
            ..crate::service::testing::virtual_phone(dir.path())
        };
        let serial = phone.serial.clone();
        (Service::new(Source::Virtual(phone)), serial)
    }

    #[tokio::test]
    async fn the_battery_level_is_read_from_the_phone_and_follows_it() {
        let dir = tempfile::tempdir().unwrap();
        let (service, serial) = charged(&dir, Some(76));
        let device = service.connect().await.unwrap();
        assert!(
            device.android,
            "the fake phone stands in for an Android one"
        );

        assert_eq!(service.battery().await, Ok(Some(76)));
        assert!(mtp_rs::set_virtual_battery_level(&serial, 81));
        assert_eq!(service.battery().await, Ok(Some(81)), "charging");
    }

    #[tokio::test]
    async fn a_phone_says_when_its_battery_level_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (service, serial) = charged(&dir, Some(76));
        service.connect().await.unwrap();

        assert!(mtp_rs::set_virtual_battery_level(&serial, 77));
        let change = tokio::time::timeout(std::time::Duration::from_secs(5), service.next_change())
            .await
            .expect("the change is announced");

        assert_eq!(change, Ok(ContentChange::Battery));
    }

    #[tokio::test]
    async fn a_phone_lost_while_asked_its_battery_is_lost_not_levelless() {
        let dir = tempfile::tempdir().unwrap();
        let (service, serial) = charged(&dir, Some(76));
        service.connect().await.unwrap();

        // The next request comes back as a reset, as a wedged phone's does.
        assert!(mtp_rs::force_operation_wedge(&serial));
        let level = service.battery().await;

        assert_eq!(level, Err(Error::Disconnected));
        assert_eq!(service.session().await.err(), Some(Error::NotConnected));
    }

    #[tokio::test]
    async fn a_phone_that_reports_no_battery_has_no_level_and_is_still_fine() {
        let dir = tempfile::tempdir().unwrap();
        let (service, _) = charged(&dir, None);
        service.connect().await.unwrap();

        assert_eq!(service.battery().await, Ok(None));
        assert!(service.session().await.is_ok());
    }

    #[tokio::test]
    async fn no_phone_no_battery() {
        let dir = tempfile::tempdir().unwrap();
        let (service, _) = charged(&dir, Some(50));

        assert_eq!(service.battery().await, Err(Error::NotConnected));
    }

    #[tokio::test]
    async fn the_battery_of_a_silent_phone_is_not_waited_for_for_ever() {
        let dir = tempfile::tempdir().unwrap();
        let (service, _) = charged(&dir, Some(50));
        let service = service.answering_within(std::time::Duration::from_millis(150));
        service.connect().await.unwrap();
        let storage = service.storages().await.unwrap()[0].id;
        let epoch = service.epoch();
        let _silence = crate::service::testing::silence(&service, storage).await;

        let level = tokio::time::timeout(std::time::Duration::from_secs(5), service.battery())
            .await
            .expect("it gives up");

        assert_eq!(level, Err(Error::Timeout));
        assert_eq!(service.session().await.err(), Some(Error::NotConnected));
        assert!(service.epoch() > epoch, "Reconnect is offered");
    }

    #[tokio::test]
    async fn after_a_switch_the_level_is_the_other_phones() {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = VirtualPhone {
            battery: Some(20),
            ..crate::service::testing::virtual_phone(one.path())
        };
        let b = VirtualPhone {
            battery: Some(90),
            ..crate::service::testing::virtual_phone(two.path())
        };
        let (a_key, b_key) = (a.serial.clone(), b.serial.clone());
        let service = Service::new(Source::Phones(vec![a, b]));

        service.connect_to(Some(&a_key)).await.unwrap();
        assert_eq!(service.battery().await, Ok(Some(20)));
        service.connect_to(Some(&b_key)).await.unwrap();
        assert_eq!(service.battery().await, Ok(Some(90)));
    }
}
