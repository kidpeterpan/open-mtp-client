use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::known::{CAMERA_PROCESSES, CAMERA_SERVICES, KNOWN, Known, Quit};
use super::system::{SystemProcesses, SystemUsb};
use crate::dto::{Diagnosis, Error, Holder, MacInfo};

/// The programs running on the Mac and what can be done to them.
pub trait Processes: Send + Sync {
    /// Executable names of everything running.
    fn running(&self) -> Vec<String>;
    /// The macOS version, e.g. "15.1", if it can be read.
    fn os_version(&self) -> Option<String>;
    /// This process runs translated by Rosetta.
    fn translated(&self) -> bool {
        false
    }
    /// Asks the app to quit, like ⌘Q. Does nothing if it is not running.
    fn quit_app(&self, app: &str);
    /// Ends every process with exactly this name.
    fn kill(&self, process: &str);
    /// Ends every process of this user with exactly this name, at once and
    /// whether it wants to or not (SIGKILL).
    fn force_kill(&self, process: &str);
}

/// The USB bus.
pub trait Usb: Send + Sync {
    /// Names of devices that look like phones.
    fn phones(&self) -> Vec<String>;
    /// The programs macOS records as holding a USB device open for
    /// themselves alone (its registry's `UsbExclusiveOwner`), any device.
    fn owners(&self) -> Vec<UsbOwner> {
        Vec::new()
    }
}

/// A program macOS says has a USB device open for itself alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbOwner {
    pub pid: u32,
    /// Its process name, as macOS records it.
    pub process: String,
    /// The device's maker, when the registry gives it.
    pub vendor_id: Option<u16>,
}

/// How long to wait for quit apps to be gone.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Apps may ask to save documents first, but not forever.
    pub patience: Duration,
    pub poll: Duration,
    /// A program macOS restarts is not waited for; this gives the phone's
    /// USB interface a moment to be released.
    pub settle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            patience: Duration::from_secs(3),
            poll: Duration::from_millis(100),
            settle: Duration::from_millis(300),
        }
    }
}

/// Works out why a phone will not connect, and frees it from the apps that
/// are holding it.
pub struct Service {
    processes: Arc<dyn Processes>,
    usb: Arc<dyn Usb>,
    timing: Timing,
}

impl Service {
    pub fn new(processes: Arc<dyn Processes>, usb: Arc<dyn Usb>) -> Self {
        Self::with_timing(processes, usb, Timing::default())
    }

    pub fn with_timing(processes: Arc<dyn Processes>, usb: Arc<dyn Usb>, timing: Timing) -> Self {
        Self {
            processes,
            usb,
            timing,
        }
    }

    /// The real Mac.
    pub fn system() -> Self {
        Self::new(Arc::new(SystemProcesses), Arc::new(SystemUsb))
    }

    /// Looks into every cause at once: for someone asking "why won't my phone
    /// connect?" with no failure in hand. Both checks are made, so each of the
    /// two answers is `Some`.
    pub async fn checkup(&self) -> Diagnosis {
        let processes = Arc::clone(&self.processes);
        let usb = Arc::clone(&self.usb);
        tokio::task::spawn_blocking(move || Diagnosis {
            phones: Some(distinct(usb.phones())),
            ..who_holds(processes.as_ref(), usb.as_ref())
        })
        .await
        .unwrap_or_default()
    }

    /// The Mac this is running on, for a support report.
    pub async fn mac(&self) -> MacInfo {
        let processes = Arc::clone(&self.processes);
        let (os_version, translated) =
            tokio::task::spawn_blocking(move || (processes.os_version(), processes.translated()))
                .await
                .unwrap_or_default();
        MacInfo {
            os_version: os_version.unwrap_or_default(),
            arch: std::env::consts::ARCH.to_owned(),
            translated,
            // The crash reports are the app's own, not the Mac's: whoever
            // joins this with them fills it in.
            crashes: Vec::new(),
        }
    }

    /// Looks into what the failure could be about. Each check runs only for
    /// the failures it can explain, so the answer is never padded with
    /// results that don't matter.
    pub async fn diagnose(&self, failure: &Error) -> Diagnosis {
        let holders_matter = matches!(
            failure,
            Error::DeviceInUse
                | Error::AndroidFileTransferRunning
                | Error::PermissionDenied
                | Error::Timeout
                | Error::General(_)
        );
        let phones_matter = matches!(failure, Error::NoDevice);

        let processes = Arc::clone(&self.processes);
        let usb = Arc::clone(&self.usb);
        // `ps` and the USB listing block, so they stay off the async threads.
        tokio::task::spawn_blocking(move || {
            let held = if holders_matter {
                who_holds(processes.as_ref(), usb.as_ref())
            } else {
                Diagnosis::default()
            };
            Diagnosis {
                phones: phones_matter.then(|| distinct(usb.phones())),
                ..held
            }
        })
        .await
        .unwrap_or_default()
    }

    /// Quits `holders` and waits for them to be gone (up to the timing's
    /// patience), so connecting right after finds the phone free. Only
    /// programs from the known list are ever touched, whatever `holders`
    /// says.
    pub async fn quit(&self, holders: &[Holder]) {
        let targets: Vec<&'static Known> = holders
            .iter()
            .flat_map(|holder| KNOWN.iter().filter(move |known| known.name == holder.name))
            .collect();

        let processes = Arc::clone(&self.processes);
        let to_quit = targets.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for known in to_quit {
                match known.quit {
                    Quit::App => processes.quit_app(known.process),
                    // They ignore a polite quit: ended outright.
                    Quit::Kill if CAMERA_PROCESSES.contains(&known.process) => {
                        processes.force_kill(known.process);
                    }
                    Quit::Kill => processes.kill(known.process),
                }
            }
        })
        .await;

        let waits_for: HashSet<&str> = targets
            .iter()
            .filter(|known| !known.restarts)
            .map(|known| known.process)
            .collect();
        if targets.iter().any(|known| known.restarts) {
            tokio::time::sleep(self.timing.settle).await;
        }
        self.wait_until_gone(&waits_for).await;
    }

    /// After an open failed because something has the phone: when macOS's
    /// camera services are what has it, ends them at once (they ignore a
    /// polite quit, and relaunch: nothing is waited for, so the phone can be
    /// opened again before they take it back) and says so. macOS's own
    /// record of who has the phone decides, whatever else runs; without one,
    /// they must be the only known programs running. Otherwise nothing is
    /// touched and the user is asked, as before.
    pub async fn free_from_camera_services(&self) -> bool {
        let processes = Arc::clone(&self.processes);
        let usb = Arc::clone(&self.usb);
        tokio::task::spawn_blocking(move || {
            let owners = phone_owners(usb.owners(), std::process::id());
            let only_cameras = if owners.is_empty() {
                let found = holders(&processes.running());
                !found.is_empty() && found.iter().all(|holder| holder.name == CAMERA_SERVICES)
            } else {
                owners
                    .iter()
                    .all(|owner| CAMERA_PROCESSES.contains(&owner.process.as_str()))
            };
            if only_cameras {
                for process in CAMERA_PROCESSES {
                    processes.force_kill(process);
                }
            }
            only_cameras
        })
        .await
        .unwrap_or(false)
    }

    async fn wait_until_gone(&self, names: &HashSet<&str>) {
        if names.is_empty() {
            return;
        }
        let started = Instant::now();
        loop {
            let processes = Arc::clone(&self.processes);
            let running = tokio::task::spawn_blocking(move || processes.running())
                .await
                .unwrap_or_default();
            let any_left = running.iter().any(|name| names.contains(name.as_str()));
            if !any_left || started.elapsed() >= self.timing.patience {
                return;
            }
            tokio::time::sleep(self.timing.poll).await;
        }
    }
}

/// Who has the phone: what macOS records as owning it when it records
/// anything (known programs as holders, the rest by process name), else the
/// known programs that run and could.
fn who_holds(processes: &dyn Processes, usb: &dyn Usb) -> Diagnosis {
    let owners = phone_owners(usb.owners(), std::process::id());
    if owners.is_empty() {
        return Diagnosis {
            holders: Some(holders(&processes.running())),
            ..Diagnosis::default()
        };
    }
    let (holders, unknown_owners) = owned_by(&owners);
    Diagnosis {
        holders: Some(holders),
        owner_seen: true,
        unknown_owners,
        ..Diagnosis::default()
    }
}

/// The owners that can be about a phone: of a phone maker's device, or a
/// program known to take phones (macOS's camera service claims a Switch or
/// a Garmin as readily). Another device's own app (a mouse's, a drawing
/// tablet's) is none of the phone's business, and this app (`me`) is not
/// in its own way. A device printed twice (inside a hub) repeats its owner:
/// `owned_by` names each once.
fn phone_owners(owners: Vec<UsbOwner>, me: u32) -> Vec<UsbOwner> {
    owners
        .into_iter()
        .filter(|owner| owner.pid != me)
        .filter(|owner| {
            owner
                .vendor_id
                .is_some_and(|id| super::known::phone_vendor(id).is_some())
                || known_process(&owner.process).is_some()
        })
        .collect()
}

fn known_process(process: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|known| known.process == process)
}

/// Owners as holders the app can quit, one per name, and the process names
/// of the rest, once each.
fn owned_by(owners: &[UsbOwner]) -> (Vec<Holder>, Vec<String>) {
    let mut holders: Vec<Holder> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for owner in owners {
        match known_process(&owner.process) {
            Some(known) => {
                if holders.iter().all(|h| h.name != known.name) {
                    holders.push(Holder {
                        name: known.name.to_owned(),
                        restarts: known.restarts,
                    });
                }
            }
            None => {
                if !unknown.contains(&owner.process) {
                    unknown.push(owner.process.clone());
                }
            }
        }
    }
    (holders, unknown)
}

/// The known programs among `running`, most likely culprit first, one entry
/// per name (the Android File Transfer app and its agent are one holder).
fn holders(running: &[String]) -> Vec<Holder> {
    let running: HashSet<&str> = running.iter().map(String::as_str).collect();
    let mut found: Vec<Holder> = Vec::new();
    for known in KNOWN {
        if running.contains(known.process) && found.iter().all(|h| h.name != known.name) {
            found.push(Holder {
                name: known.name.to_owned(),
                restarts: known.restarts,
            });
        }
    }
    found
}

fn distinct(names: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A Mac with `running` programs. Quitting removes a program after
    /// `exits_after` listings, unless it is stubborn or restarts itself.
    struct FakeProcesses {
        running: Mutex<Vec<String>>,
        /// Programs that never go away.
        stubborn: Vec<&'static str>,
        /// Listings it takes a quit program to disappear.
        exits_after: usize,
        closing: Mutex<Vec<(String, usize)>>,
        log: Mutex<Vec<String>>,
        listings: Mutex<usize>,
    }

    impl FakeProcesses {
        fn new(running: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                running: Mutex::new(running.iter().map(|s| (*s).to_owned()).collect()),
                stubborn: Vec::new(),
                exits_after: 0,
                closing: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
                listings: Mutex::new(0),
            })
        }

        fn stubborn(running: &[&str], stubborn: &[&'static str]) -> Arc<Self> {
            let mut fake = Arc::try_unwrap(Self::new(running)).ok().unwrap();
            fake.stubborn = stubborn.to_vec();
            Arc::new(fake)
        }

        fn slow(running: &[&str], exits_after: usize) -> Arc<Self> {
            let mut fake = Arc::try_unwrap(Self::new(running)).ok().unwrap();
            fake.exits_after = exits_after;
            Arc::new(fake)
        }

        fn close(&self, process: &str) {
            if self.stubborn.contains(&process) {
                return;
            }
            self.closing
                .lock()
                .unwrap()
                .push((process.to_owned(), self.exits_after));
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Processes for FakeProcesses {
        fn os_version(&self) -> Option<String> {
            Some("15.1".to_owned())
        }

        fn running(&self) -> Vec<String> {
            *self.listings.lock().unwrap() += 1;
            let mut closing = self.closing.lock().unwrap();
            let mut running = self.running.lock().unwrap();
            for (process, left) in closing.iter_mut() {
                if *left == 0 {
                    running.retain(|name| name != process);
                } else {
                    *left -= 1;
                }
            }
            running.clone()
        }

        fn quit_app(&self, app: &str) {
            self.log.lock().unwrap().push(format!("quit {app}"));
            self.close(app);
        }

        fn kill(&self, process: &str) {
            self.log.lock().unwrap().push(format!("kill {process}"));
            self.close(process);
        }

        fn force_kill(&self, process: &str) {
            self.log.lock().unwrap().push(format!("force {process}"));
            self.running.lock().unwrap().retain(|name| name != process);
        }
    }

    #[tokio::test]
    async fn the_camera_services_alone_are_ended_at_once_and_nothing_is_waited_for() {
        let mac = FakeProcesses::stubborn(
            &["Finder", "ptpcamerad", "mscamerad-xpc"],
            &["ptpcamerad", "mscamerad-xpc"],
        );
        let doctor = Service::with_timing(
            mac.clone(),
            Arc::new(FakeUsb(vec![])),
            Timing {
                settle: Duration::from_secs(5),
                patience: Duration::from_secs(5),
                ..FAST
            },
        );

        let started = Instant::now();
        let freed = doctor.free_from_camera_services().await;

        assert!(freed);
        assert_eq!(mac.log(), ["force ptpcamerad", "force mscamerad-xpc"]);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "no settle, no waiting: the phone is opened again at once"
        );
        assert_eq!(
            holders(&["ptpcamerad".into(), "mscamerad-xpc".into()]).len(),
            1,
            "one holder to the user"
        );
    }

    #[tokio::test]
    async fn with_anything_else_that_could_hold_the_phone_nothing_is_ended() {
        let mac = FakeProcesses::new(&["ptpcamerad", "mscamerad-xpc", "Photos"]);
        assert!(!service(mac.clone(), &[]).free_from_camera_services().await);
        assert!(mac.log().is_empty(), "the user is asked instead");

        let quiet = FakeProcesses::new(&["Finder"]);
        assert!(
            !service(quiet.clone(), &[])
                .free_from_camera_services()
                .await
        );
        assert!(quiet.log().is_empty(), "nothing of ours to end");

        let helper_only = FakeProcesses::new(&["mscamerad-xpc"]);
        assert!(
            service(helper_only.clone(), &[])
                .free_from_camera_services()
                .await
        );
        assert_eq!(
            helper_only.log(),
            ["force ptpcamerad", "force mscamerad-xpc"]
        );
    }

    struct FakeUsb(Vec<&'static str>);

    impl Usb for FakeUsb {
        fn phones(&self) -> Vec<String> {
            self.0.iter().map(|s| (*s).to_owned()).collect()
        }
    }

    /// A USB bus where macOS records `owners` as holding devices.
    struct OwnedUsb(Vec<UsbOwner>);

    impl Usb for OwnedUsb {
        fn phones(&self) -> Vec<String> {
            vec!["Pixel 9".into()]
        }

        fn owners(&self) -> Vec<UsbOwner> {
            self.0.clone()
        }
    }

    const SAMSUNG: u16 = 0x04e8;

    fn owner(pid: u32, process: &str, vendor_id: u16) -> UsbOwner {
        UsbOwner {
            pid,
            process: process.to_owned(),
            vendor_id: Some(vendor_id),
        }
    }

    fn owned(processes: Arc<FakeProcesses>, owners: Vec<UsbOwner>) -> Service {
        Service::with_timing(processes, Arc::new(OwnedUsb(owners)), FAST)
    }

    #[tokio::test]
    async fn the_owner_macos_records_is_named_alone_whatever_else_runs() {
        let mac = FakeProcesses::new(&["Photos", "Dropbox", "Garmin Express Service"]);
        let doctor = owned(
            mac.clone(),
            vec![owner(4021, "Garmin Express Service", SAMSUNG)],
        );

        let diagnosis = doctor.diagnose(&Error::DeviceInUse).await;

        assert_eq!(names(&diagnosis), ["Garmin Express"]);
        assert!(diagnosis.owner_seen);
        assert!(diagnosis.unknown_owners.is_empty());

        doctor.quit(diagnosis.holders.as_deref().unwrap()).await;
        assert_eq!(
            mac.log(),
            ["quit Garmin Express", "quit Garmin Express Service"],
            "both of its programs, and nothing that merely runs"
        );
    }

    #[tokio::test]
    async fn an_owner_the_app_does_not_know_is_named_and_never_quit() {
        let mac = FakeProcesses::new(&["Photos", "SyncMate"]);
        let doctor = owned(
            mac.clone(),
            vec![
                owner(88, "SyncMate", 0x18d1),
                owner(88, "SyncMate", 0x18d1),
                owner(91, "SyncMate", 0x18d1),
            ],
        );

        let diagnosis = doctor.diagnose(&Error::DeviceInUse).await;

        assert_eq!(names(&diagnosis), Vec::<&str>::new());
        assert!(diagnosis.owner_seen);
        assert_eq!(
            diagnosis.unknown_owners,
            ["SyncMate"],
            "once, however recorded"
        );
        doctor.quit(diagnosis.holders.as_deref().unwrap()).await;
        doctor.quit(&[holder("SyncMate", false)]).await;
        assert!(mac.log().is_empty(), "only known programs are ever touched");
    }

    #[tokio::test]
    async fn another_devices_owner_or_this_app_is_not_the_phones() {
        let mac = FakeProcesses::new(&["Preview"]);
        let diagnosis = owned(
            mac.clone(),
            vec![
                owner(12, "Logi Options+", 0x046d),
                owner(std::process::id(), "open-mtp-client", SAMSUNG),
            ],
        )
        .diagnose(&Error::DeviceInUse)
        .await;

        assert!(!diagnosis.owner_seen, "nothing recorded about a phone");
        assert_eq!(names(&diagnosis), ["Preview"], "what runs, as before");

        // macOS's camera service takes a Switch as readily as a phone; its
        // two programs, each recorded on an interface, are one to the user.
        let switch = owned(
            mac,
            vec![
                owner(968, "ptpcamerad", 0x057e),
                owner(970, "mscamerad-xpc", 0x057e),
            ],
        )
        .diagnose(&Error::DeviceInUse)
        .await;
        assert_eq!(names(&switch), ["macOS's camera service"]);
        assert!(switch.owner_seen);
    }

    #[tokio::test]
    async fn the_owner_is_looked_for_only_where_a_holder_could_explain_it() {
        let mac = FakeProcesses::new(&["Preview"]);
        let doctor = owned(mac, vec![owner(4021, "MacDroid", SAMSUNG)]);

        let none = doctor.diagnose(&Error::NoDevice).await;
        assert_eq!((none.holders, none.owner_seen), (None, false));
        assert!(none.unknown_owners.is_empty());

        let checkup = doctor.checkup().await;
        assert_eq!(names(&checkup), ["MacDroid"]);
        assert!(checkup.owner_seen);
        assert_eq!(checkup.phones.as_deref(), Some(&["Pixel 9".to_owned()][..]));
    }

    #[tokio::test]
    async fn the_camera_service_owning_the_phone_is_ended_whatever_else_runs() {
        // Google Drive or Dropbox running used to keep it from being freed.
        let mac = FakeProcesses::new(&["ptpcamerad", "Google Drive", "Dropbox"]);
        let doctor = owned(mac.clone(), vec![owner(968, "ptpcamerad", SAMSUNG)]);

        assert!(doctor.free_from_camera_services().await);
        assert_eq!(mac.log(), ["force ptpcamerad", "force mscamerad-xpc"]);

        let garmin = FakeProcesses::new(&["ptpcamerad"]);
        let held = owned(
            garmin.clone(),
            vec![
                owner(968, "ptpcamerad", SAMSUNG),
                owner(4021, "Garmin Express", SAMSUNG),
            ],
        );
        assert!(
            !held.free_from_camera_services().await,
            "something else owns it too: ending the camera service wouldn't free it"
        );
        assert!(garmin.log().is_empty());
    }

    const FAST: Timing = Timing {
        patience: Duration::from_millis(400),
        poll: Duration::from_millis(5),
        settle: Duration::from_millis(5),
    };

    fn service(processes: Arc<FakeProcesses>, phones: &[&'static str]) -> Service {
        Service::with_timing(processes, Arc::new(FakeUsb(phones.to_vec())), FAST)
    }

    fn holder(name: &str, restarts: bool) -> Holder {
        Holder {
            name: name.to_owned(),
            restarts,
        }
    }

    fn names(diagnosis: &Diagnosis) -> Vec<&str> {
        diagnosis
            .holders
            .as_ref()
            .unwrap()
            .iter()
            .map(|h| h.name.as_str())
            .collect()
    }

    #[tokio::test]
    async fn an_app_holding_the_phone_is_named() {
        let mac = FakeProcesses::new(&["launchd", "Finder", "Preview"]);

        let diagnosis = service(mac, &[]).diagnose(&Error::DeviceInUse).await;

        assert_eq!(names(&diagnosis), ["Preview"]);
        assert_eq!(diagnosis.phones, None, "no USB scan for this failure");
    }

    #[tokio::test]
    async fn the_likeliest_culprit_comes_first_whatever_the_process_order() {
        let mac = FakeProcesses::new(&["Dropbox", "Preview", "Photos", "ptpcamerad"]);

        let diagnosis = service(mac, &[]).diagnose(&Error::DeviceInUse).await;

        assert_eq!(
            names(&diagnosis),
            ["macOS's camera service", "Photos", "Preview", "Dropbox"]
        );
        let holders = diagnosis.holders.unwrap();
        assert!(holders[0].restarts);
        assert!(!holders[1].restarts);
    }

    #[tokio::test]
    async fn android_file_transfer_and_its_agent_are_one_holder() {
        let mac = FakeProcesses::new(&["Android File Transfer", "Android File Transfer Agent"]);

        let diagnosis = service(mac, &[])
            .diagnose(&Error::AndroidFileTransferRunning)
            .await;

        assert_eq!(names(&diagnosis), ["Android File Transfer"]);
    }

    #[tokio::test]
    async fn the_agent_alone_counts() {
        // The agent holds the phone with no window open.
        let mac = FakeProcesses::new(&["Android File Transfer Agent"]);

        let diagnosis = service(mac, &[])
            .diagnose(&Error::AndroidFileTransferRunning)
            .await;

        assert_eq!(names(&diagnosis), ["Android File Transfer"]);
    }

    #[tokio::test]
    async fn nothing_known_running_is_reported_as_nothing_not_as_unchecked() {
        let mac = FakeProcesses::new(&["launchd", "Finder"]);

        let diagnosis = service(mac, &[]).diagnose(&Error::DeviceInUse).await;

        assert_eq!(diagnosis.holders, Some(vec![]));
    }

    #[tokio::test]
    async fn no_device_looks_at_usb_not_at_apps() {
        let mac = FakeProcesses::new(&["Photos"]);

        let diagnosis = service(mac, &["Pixel 7"]).diagnose(&Error::NoDevice).await;

        assert_eq!(diagnosis.phones, Some(vec!["Pixel 7".to_owned()]));
        assert_eq!(diagnosis.holders, None);
    }

    #[tokio::test]
    async fn no_device_and_no_phone_on_usb_is_an_empty_list() {
        let diagnosis = service(FakeProcesses::new(&[]), &[])
            .diagnose(&Error::NoDevice)
            .await;

        assert_eq!(diagnosis.phones, Some(vec![]));
    }

    #[tokio::test]
    async fn a_phone_seen_twice_is_listed_once() {
        let diagnosis = service(
            FakeProcesses::new(&[]),
            &["Pixel 7", "Pixel 7", "Galaxy S23"],
        )
        .diagnose(&Error::NoDevice)
        .await;

        assert_eq!(
            diagnosis.phones,
            Some(vec!["Pixel 7".to_owned(), "Galaxy S23".to_owned()])
        );
    }

    #[tokio::test]
    async fn app_scans_cover_the_failures_apps_can_cause() {
        for failure in [
            Error::DeviceInUse,
            Error::AndroidFileTransferRunning,
            Error::PermissionDenied,
            Error::Timeout,
            Error::General("boom".into()),
        ] {
            let diagnosis = service(FakeProcesses::new(&["Photos"]), &["Pixel 7"])
                .diagnose(&failure)
                .await;
            assert_eq!(names(&diagnosis), ["Photos"], "{failure:?}");
            assert_eq!(diagnosis.phones, None, "{failure:?}");
        }
    }

    #[tokio::test]
    async fn other_failures_have_nothing_to_diagnose() {
        for failure in [
            Error::AllowStorageAccess,
            Error::MultipleDevices,
            Error::Disconnected,
            Error::NotConnected,
            Error::Cancelled,
        ] {
            let diagnosis = service(FakeProcesses::new(&["Photos"]), &["Pixel 7"])
                .diagnose(&failure)
                .await;
            assert_eq!(diagnosis, Diagnosis::default(), "{failure:?}");
        }
    }

    #[tokio::test]
    async fn quitting_asks_apps_nicely_and_ends_background_processes() {
        let mac = FakeProcesses::new(&["Photos", "Android File Transfer Agent"]);
        let doctor = service(Arc::clone(&mac), &[]);

        doctor
            .quit(&[
                holder("Photos", false),
                holder("Android File Transfer", false),
            ])
            .await;

        let mut log = mac.log();
        log.sort();
        // Both of the Android File Transfer programs are asked, the one that
        // isn't running does nothing.
        assert_eq!(
            log,
            [
                "kill Android File Transfer Agent",
                "quit Android File Transfer",
                "quit Photos"
            ]
        );
    }

    #[tokio::test]
    async fn quitting_waits_until_the_apps_are_gone() {
        let mac = FakeProcesses::slow(&["Photos"], 5);
        let doctor = service(Arc::clone(&mac), &[]);

        doctor.quit(&[holder("Photos", false)]).await;

        assert!(
            !mac.running().contains(&"Photos".to_owned()),
            "Photos was still running when quit returned"
        );
    }

    #[tokio::test]
    async fn quitting_gives_up_waiting_on_an_app_that_will_not_go() {
        let mac = FakeProcesses::stubborn(&["Preview"], &["Preview"]);
        let doctor = service(Arc::clone(&mac), &[]);
        let started = Instant::now();

        doctor.quit(&[holder("Preview", false)]).await;

        let waited = started.elapsed();
        assert!(waited >= FAST.patience, "gave up after {waited:?}");
        assert!(waited < FAST.patience * 4, "waited {waited:?}");
        assert!(mac.running().contains(&"Preview".to_owned()));
    }

    #[tokio::test]
    async fn a_program_macos_restarts_is_not_waited_for() {
        // It comes straight back, so waiting for it to be gone would only
        // burn the whole patience.
        let mac = FakeProcesses::stubborn(&["ptpcamerad"], &["ptpcamerad"]);
        let doctor = service(Arc::clone(&mac), &[]);
        let started = Instant::now();

        doctor.quit(&[holder("macOS's camera service", true)]).await;

        assert!(started.elapsed() < FAST.patience, "waited for a restarter");
        assert_eq!(
            mac.log(),
            ["force ptpcamerad", "force mscamerad-xpc"],
            "both camera services, ended outright: a polite quit leaves them"
        );
    }

    #[tokio::test]
    async fn only_known_programs_are_ever_touched() {
        let mac = FakeProcesses::new(&["Finder", "loginwindow"]);
        let doctor = service(Arc::clone(&mac), &[]);

        doctor
            .quit(&[
                holder("Finder", false),
                holder("loginwindow", false),
                holder("", false),
            ])
            .await;

        assert!(mac.log().is_empty(), "{:?}", mac.log());
    }

    #[tokio::test]
    async fn quitting_nobody_returns_at_once() {
        let mac = FakeProcesses::new(&["Photos"]);
        let doctor = service(Arc::clone(&mac), &[]);
        let started = Instant::now();

        doctor.quit(&[]).await;

        assert!(started.elapsed() < FAST.patience);
        assert!(mac.log().is_empty());
    }

    #[test]
    fn the_real_service_builds() {
        let _ = Service::system();
        let timing = Timing::default();
        assert!(timing.patience > timing.poll);
    }

    #[tokio::test]
    async fn the_real_service_diagnoses_without_touching_anything() {
        // Whatever this Mac is running, a failure nothing can explain finds nothing.
        let diagnosis = Service::system().diagnose(&Error::Cancelled).await;
        assert_eq!(diagnosis, Diagnosis::default());

        let diagnosis = Service::system().diagnose(&Error::DeviceInUse).await;
        assert!(diagnosis.holders.is_some());
    }

    #[tokio::test]
    async fn a_checkup_looks_at_apps_and_usb_at_once() {
        let mac = FakeProcesses::new(&["Photos", "Finder"]);

        let diagnosis = service(mac, &["Pixel 7"]).checkup().await;

        assert_eq!(names(&diagnosis), ["Photos"]);
        assert_eq!(diagnosis.phones, Some(vec!["Pixel 7".to_owned()]));
    }

    #[tokio::test]
    async fn a_checkup_with_nothing_found_says_so_for_both() {
        let diagnosis = service(FakeProcesses::new(&["Finder"]), &[])
            .checkup()
            .await;

        assert_eq!(diagnosis.holders, Some(vec![]));
        assert_eq!(diagnosis.phones, Some(vec![]));
    }

    #[tokio::test]
    async fn a_checkup_lists_a_phone_seen_twice_once() {
        let diagnosis = service(FakeProcesses::new(&[]), &["Pixel 7", "Pixel 7"])
            .checkup()
            .await;

        assert_eq!(diagnosis.phones, Some(vec!["Pixel 7".to_owned()]));
    }

    #[tokio::test]
    async fn the_mac_is_described_by_version_and_architecture() {
        let info = service(FakeProcesses::new(&[]), &[]).mac().await;

        assert_eq!(info.os_version, "15.1");
        assert_eq!(info.arch, std::env::consts::ARCH);
        assert!(!info.arch.is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_version_is_left_empty() {
        struct NoVersion;
        impl Processes for NoVersion {
            fn running(&self) -> Vec<String> {
                Vec::new()
            }
            fn os_version(&self) -> Option<String> {
                None
            }
            fn quit_app(&self, _: &str) {}
            fn kill(&self, _: &str) {}
            fn force_kill(&self, _: &str) {}
        }
        let doctor = Service::with_timing(Arc::new(NoVersion), Arc::new(FakeUsb(vec![])), FAST);

        assert_eq!(doctor.mac().await.os_version, "");
        assert!(!doctor.mac().await.translated, "nothing says so");
    }

    #[tokio::test]
    async fn an_app_running_under_rosetta_says_so() {
        struct Translated;
        impl Processes for Translated {
            fn running(&self) -> Vec<String> {
                Vec::new()
            }
            fn os_version(&self) -> Option<String> {
                Some("15.1".into())
            }
            fn translated(&self) -> bool {
                true
            }
            fn quit_app(&self, _: &str) {}
            fn kill(&self, _: &str) {}
            fn force_kill(&self, _: &str) {}
        }
        let doctor = Service::with_timing(Arc::new(Translated), Arc::new(FakeUsb(vec![])), FAST);

        assert!(doctor.mac().await.translated);
    }

    #[tokio::test]
    async fn the_real_checkup_and_mac_work_here() {
        let doctor = Service::system();

        let diagnosis = doctor.checkup().await;
        let info = doctor.mac().await;

        assert!(diagnosis.holders.is_some() && diagnosis.phones.is_some());
        assert!(!info.os_version.is_empty());
        if std::env::consts::ARCH == "aarch64" {
            assert!(
                !info.translated,
                "an Apple Silicon build is never translated"
            );
        }
    }
}
