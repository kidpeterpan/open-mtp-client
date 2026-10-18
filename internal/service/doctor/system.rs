//! The real Mac behind the doctor's two seams: `ps`, AppleScript and `pkill`
//! for programs, `nusb` and `ioreg` for USB devices.

use std::process::{Command, Stdio};

use nusb::MaybeFuture;

use super::mapper;
use super::service::{Processes, Usb, UsbOwner};

pub struct SystemProcesses;

/// `ps` prints one executable name per line with `-c` (no path, no arguments).
fn running_command() -> (&'static str, [&'static str; 2]) {
    ("ps", ["-axco", "comm="])
}

/// The macOS version, as `sw_vers` prints it.
fn version_command() -> (&'static str, [&'static str; 1]) {
    ("sw_vers", ["-productVersion"])
}

/// Whether this process is translated: `sysctl` prints 1 under Rosetta and 0
/// natively, and an Intel Mac has no such key at all.
fn translated_command() -> (&'static str, [&'static str; 2]) {
    ("sysctl", ["-n", "sysctl.proc_translated"])
}

/// What `sysctl` said, read as "translated" only for exactly 1.
fn is_translated(succeeded: bool, printed: &str) -> bool {
    succeeded && printed.trim() == "1"
}

/// Quits `app` like ⌘Q, but only if it is running: a bare `tell application
/// "X" to quit` would launch X first.
fn quit_app_command(app: &str) -> (&'static str, [String; 2]) {
    let script =
        format!("if application \"{app}\" is running then tell application \"{app}\" to quit");
    ("osascript", ["-e".to_owned(), script])
}

/// Ends the process whose whole name is `process`.
fn kill_command(process: &str) -> (&'static str, [&str; 2]) {
    ("pkill", ["-x", process])
}

/// `pkill` sending SIGKILL, to this user's processes of exactly that name.
fn force_kill_command(process: &str, user: u32) -> (&'static str, Vec<String>) {
    (
        "pkill",
        vec![
            "-KILL".to_owned(),
            "-U".to_owned(),
            user.to_string(),
            "-x".to_owned(),
            process.to_owned(),
        ],
    )
}

/// Every USB device with its properties and its interfaces' below it, one
/// line each (`-w0`), where macOS writes who holds it (`UsbExclusiveOwner`).
fn registry_command() -> (&'static str, [&'static str; 5]) {
    ("ioreg", ["-r", "-c", "IOUSBHostDevice", "-l", "-w0"])
}

/// The user this app runs as.
fn this_user() -> u32 {
    // SAFETY: getuid only reads the process's user id; it cannot fail.
    unsafe { libc::getuid() }
}

fn run(program: &str, args: &[impl AsRef<std::ffi::OsStr>]) {
    // Nothing useful to say if it can't be run; the doctor checks again afterwards.
    let _ = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Processes for SystemProcesses {
    fn running(&self) -> Vec<String> {
        let (program, args) = running_command();
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|line| line.trim().to_owned())
                    .filter(|line| !line.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn os_version(&self) -> Option<String> {
        let (program, args) = version_command();
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (out.status.success() && !version.is_empty()).then_some(version)
    }

    fn translated(&self) -> bool {
        let (program, args) = translated_command();
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| {
                is_translated(out.status.success(), &String::from_utf8_lossy(&out.stdout))
            })
    }

    fn quit_app(&self, app: &str) {
        let (program, args) = quit_app_command(app);
        run(program, &args);
    }

    fn kill(&self, process: &str) {
        let (program, args) = kill_command(process);
        run(program, &args);
    }

    fn force_kill(&self, process: &str) {
        let (program, args) = force_kill_command(process, this_user());
        run(program, &args);
    }
}

pub struct SystemUsb;

impl Usb for SystemUsb {
    fn phones(&self) -> Vec<String> {
        // Waiting is fine: this runs on a blocking thread, and listing is quick.
        let Ok(devices) = nusb::list_devices().wait() else {
            return Vec::new();
        };
        devices
            .filter_map(|device| mapper::phone_name(device.vendor_id(), device.product_string()))
            .collect()
    }

    fn owners(&self) -> Vec<UsbOwner> {
        let (program, args) = registry_command();
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|out| mapper::owners_in_registry(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_read_for_every_usb_device_in_full() {
        assert_eq!(
            registry_command(),
            ("ioreg", ["-r", "-c", "IOUSBHostDevice", "-l", "-w0"])
        );
    }

    #[test]
    fn reading_this_macs_registry_finds_no_phone_owner_and_does_not_fail() {
        // No phone is plugged into this Mac, so nothing can hold one; what
        // the real read must do is come back.
        let owners = SystemUsb.owners();
        assert!(
            owners.iter().all(|owner| super::super::known::phone_vendor(
                owner.vendor_id.unwrap_or(0)
            )
            .is_none()),
            "{owners:?}"
        );
    }

    #[test]
    fn ps_lists_executable_names_only() {
        assert_eq!(running_command(), ("ps", ["-axco", "comm="]));
    }

    #[test]
    fn quitting_an_app_never_launches_it() {
        let (program, args) = quit_app_command("Photos");

        assert_eq!(program, "osascript");
        assert_eq!(args[0], "-e");
        assert_eq!(
            args[1],
            "if application \"Photos\" is running then tell application \"Photos\" to quit"
        );
    }

    #[test]
    fn rosetta_is_read_from_sysctl_and_only_a_1_counts() {
        assert_eq!(
            translated_command(),
            ("sysctl", ["-n", "sysctl.proc_translated"])
        );
        assert!(is_translated(true, "1\n"));
        assert!(!is_translated(true, "0\n"), "native");
        assert!(!is_translated(false, ""), "an Intel Mac has no such key");
        assert!(!is_translated(false, "1"), "a failed run says nothing");
    }

    #[test]
    fn the_version_comes_from_sw_vers() {
        assert_eq!(version_command(), ("sw_vers", ["-productVersion"]));
    }

    #[test]
    fn this_macs_version_looks_like_a_version() {
        let version = SystemProcesses
            .os_version()
            .expect("sw_vers works on a Mac");

        assert!(
            version.split('.').all(|part| part.parse::<u32>().is_ok()),
            "{version:?}"
        );
    }

    #[test]
    fn killing_matches_the_whole_process_name() {
        assert_eq!(kill_command("ptpcamerad"), ("pkill", ["-x", "ptpcamerad"]));
    }

    #[test]
    fn the_running_list_holds_real_processes() {
        let running = SystemProcesses.running();

        assert!(running.iter().any(|name| name == "launchd"), "{running:?}");
        assert!(
            running
                .iter()
                .all(|name| !name.is_empty() && name.trim() == name)
        );
    }

    /// Started by `stand_in`: waits a minute, so a test has a process of its
    /// own naming to end. Nothing when run as an ordinary test.
    #[test]
    #[ignore]
    fn sleeper() {
        if std::env::var_os("OMC_SLEEPER").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    /// A process named `name`, ignoring a polite quit if `stubborn`. It is
    /// this test program under another name: macOS ends a copy of one of its
    /// own programs (`/bin/sleep`) the moment it starts, which once made a
    /// test of ending one pass without ending anything.
    fn stand_in(dir: &std::path::Path, name: &str, stubborn: bool) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let copy = dir.join(name);
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        let mut command = Command::new(copy);
        command
            .args([
                "--ignored",
                "--exact",
                "service::doctor::system::tests::sleeper",
            ])
            .env("OMC_SLEEPER", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if stubborn {
            // As macOS's camera services do. An ignored signal stays ignored
            // across exec.
            // SAFETY: only `signal`, which is async-signal-safe, runs between
            // fork and exec.
            unsafe {
                command.pre_exec(|| {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                    Ok(())
                });
            }
        }
        let mut child = command.spawn().unwrap();
        // Running, under its own name, before anything is asked of it.
        let started = std::time::Instant::now();
        while !SystemProcesses
            .running()
            .iter()
            .any(|running| running == name)
        {
            assert!(child.try_wait().unwrap().is_none(), "{name} did not start");
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child
    }

    #[test]
    fn killing_stops_a_process_with_that_name_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("omcsleep{}", std::process::id());
        let other = format!("omckeep{}", std::process::id());
        let mut target = stand_in(dir.path(), &name, false);
        let mut bystander = stand_in(dir.path(), &other, false);

        SystemProcesses.kill(&name);

        let status = target.wait().unwrap();
        assert!(!status.success(), "ended by the signal");
        assert!(bystander.try_wait().unwrap().is_none(), "still running");
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    #[test]
    fn forcing_ends_this_users_process_of_that_name_even_one_that_ignores_a_quit() {
        assert_eq!(
            force_kill_command("ptpcamerad", 501),
            (
                "pkill",
                vec![
                    "-KILL".into(),
                    "-U".into(),
                    "501".into(),
                    "-x".into(),
                    "ptpcamerad".into()
                ]
            )
        );
        let dir = tempfile::tempdir().unwrap();
        let name = format!("omcstub{}", std::process::id());
        let other = format!("omcstay{}", std::process::id());
        let mut target = stand_in(dir.path(), &name, true);
        let mut bystander = stand_in(dir.path(), &other, true);

        SystemProcesses.kill(&name);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let early = target.try_wait().unwrap();
        assert!(early.is_none(), "a polite quit leaves it: {early:?}");
        SystemProcesses.force_kill(&name);

        let status = target.wait().unwrap();
        assert!(!status.success(), "ended");
        assert!(bystander.try_wait().unwrap().is_none(), "still running");
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    #[test]
    fn listing_usb_devices_does_not_fail() {
        // Whatever is plugged in, every name is a phone's.
        for name in SystemUsb.phones() {
            assert!(!name.trim().is_empty());
        }
    }
}
