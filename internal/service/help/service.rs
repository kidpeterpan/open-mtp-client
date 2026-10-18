//! What the Help dialog needs from the Mac: the connection check-up, the
//! Mac's details for a support report, and a way to open a web page.

use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::dto::{Diagnosis, Holder, MacInfo};
use crate::service::doctor;

/// Opens a link in the user's browser.
pub trait Opener: Send + Sync {
    fn open(&self, url: &str);
}

/// `open <url>`, as Finder does.
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, url: &str) {
        // Nothing useful to say if no browser can be started.
        let _ = Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

pub struct Service {
    doctor: Arc<doctor::Service>,
    opener: Arc<dyn Opener>,
}

impl Service {
    pub fn new(doctor: Arc<doctor::Service>, opener: Arc<dyn Opener>) -> Self {
        Self { doctor, opener }
    }

    /// The real Mac, around `doctor`.
    pub fn system(doctor: Arc<doctor::Service>) -> Self {
        Self::new(doctor, Arc::new(SystemOpener))
    }

    /// Looks into every reason a phone might not connect, right now.
    pub async fn checkup(&self) -> Diagnosis {
        self.doctor.checkup().await
    }

    /// Quits the apps in the way and waits for them to be gone.
    pub async fn release(&self, holders: &[Holder]) {
        self.doctor.quit(holders).await;
    }

    /// The Mac, for a support report.
    pub async fn mac(&self) -> MacInfo {
        self.doctor.mac().await
    }

    /// Opens a web page. Only plain `https://` addresses are opened: whatever
    /// is passed, it can never start an app, open a file or run anything. The
    /// Help dialog only ever passes fixed addresses; this is the second lock.
    pub fn open_link(&self, url: &str) -> bool {
        if !is_plain_https(url) {
            return false;
        }
        self.opener.open(url);
        true
    }
}

fn is_plain_https(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && rest
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '\\' && c != '"' && c != '\'' && c != '`')
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::service::doctor::{Processes, Usb};

    struct Opened(Mutex<Vec<String>>);

    impl Opener for Opened {
        fn open(&self, url: &str) {
            self.0.lock().unwrap().push(url.to_owned());
        }
    }

    struct Quiet;

    impl Processes for Quiet {
        fn running(&self) -> Vec<String> {
            vec!["Photos".into()]
        }
        fn os_version(&self) -> Option<String> {
            Some("15.1".into())
        }
        fn quit_app(&self, _: &str) {}
        fn kill(&self, _: &str) {}
        fn force_kill(&self, _: &str) {}
    }

    struct Nothing;

    impl Usb for Nothing {
        fn phones(&self) -> Vec<String> {
            vec!["Pixel 7".into()]
        }
    }

    fn service() -> (Service, Arc<Opened>) {
        let opened = Arc::new(Opened(Mutex::new(Vec::new())));
        let fast = doctor::Timing {
            patience: std::time::Duration::from_millis(50),
            poll: std::time::Duration::from_millis(5),
            settle: std::time::Duration::from_millis(5),
        };
        let doctor = Arc::new(doctor::Service::with_timing(
            Arc::new(Quiet),
            Arc::new(Nothing),
            fast,
        ));
        (
            Service::new(doctor, Arc::clone(&opened) as Arc<dyn Opener>),
            opened,
        )
    }

    #[tokio::test]
    async fn the_checkup_and_the_mac_come_from_the_doctor() {
        let (help, _) = service();

        let diagnosis = help.checkup().await;
        let mac = help.mac().await;

        assert_eq!(diagnosis.phones, Some(vec!["Pixel 7".to_owned()]));
        assert_eq!(diagnosis.holders.unwrap()[0].name, "Photos");
        assert_eq!(mac.os_version, "15.1");
    }

    #[tokio::test]
    async fn releasing_goes_through_the_doctor_and_only_known_apps() {
        let (help, _) = service();

        // Nothing to assert beyond not panicking and returning: the doctor's own
        // tests prove what is and isn't touched.
        help.release(&[Holder {
            name: "Photos".into(),
            restarts: false,
        }])
        .await;
        help.release(&[]).await;
    }

    #[test]
    fn a_plain_https_address_is_opened() {
        let (help, opened) = service();

        assert!(help.open_link("https://github.com/kidpeterpan/open-mtp-client/issues/new"));

        assert_eq!(
            *opened.0.lock().unwrap(),
            ["https://github.com/kidpeterpan/open-mtp-client/issues/new"]
        );
    }

    #[test]
    fn anything_else_is_refused_and_never_reaches_the_opener() {
        let (help, opened) = service();

        for url in [
            "",
            "http://example.com",
            "ftp://example.com",
            "file:///etc/passwd",
            "/Applications/Calculator.app",
            "Calculator",
            "-a Calculator",
            "https://",
            "https:///etc",
            "https://example.com/a b",
            "https://example.com/\n",
            "https://example.com/\"x",
            "https://example.com/`x`",
            "https://example.com/\\x",
            "javascript:alert(1)",
            "HTTPS://example.com",
            " https://example.com",
        ] {
            assert!(!help.open_link(url), "{url:?} should be refused");
        }
        assert!(opened.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_real_service_builds() {
        let _ = Service::system(Arc::new(doctor::Service::system()));
    }
}
