//! Keeping the Mac awake while transfers run. A Mac left alone goes to sleep
//! after a while, and sleep drops the phone's USB session: an overnight copy
//! would fail at the first idle timeout. While a transfer works, the app tells
//! macOS it is busy with something the user asked for (an `NSProcessInfo`
//! activity, `NSActivityUserInitiated`), which holds off idle sleep and keeps
//! App Nap from slowing the app while its window is behind others. The
//! display may still sleep, and closing a laptop's lid still sleeps it.
//!
//! A seam: the app holds the real activity, tests record. macOS ends the
//! activity by itself when the process exits, so a crash can't leave the Mac
//! awake.

use std::sync::Mutex;

/// What `pmset -g assertions` and Activity Monitor show as the reason the Mac
/// is awake.
pub const AWAKE_REASON: &str = "open-mtp-client is copying files with your phone";

pub trait Awake: Send + Sync {
    /// Keeps the Mac from idling to sleep while `awake`; `false` lets it
    /// sleep again. Says whether the Mac is now being kept awake.
    fn keep_awake(&self, awake: bool) -> bool;
}

/// What tests see: every change asked for. `refuse` stands for a Mac that
/// won't be held.
#[derive(Default)]
pub struct RecordingAwake {
    pub changes: Mutex<Vec<bool>>,
    pub refuse: bool,
}

impl Awake for RecordingAwake {
    fn keep_awake(&self, awake: bool) -> bool {
        self.changes.lock().unwrap().push(awake);
        awake && !self.refuse
    }
}

/// The real thing. Held and let go on one thread (the app's updates run on
/// the main thread): the activity is kept per thread, because the object
/// macOS hands back may not cross threads.
#[derive(Default)]
pub struct SystemAwake;

impl Awake for SystemAwake {
    fn keep_awake(&self, awake: bool) -> bool {
        if awake {
            system::hold(AWAKE_REASON);
            true
        } else {
            system::release();
            false
        }
    }
}

mod system {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    thread_local! {
        /// The activity under way on this thread, if any.
        static HELD: RefCell<Option<Retained<NSObject>>> = const { RefCell::new(None) };
    }

    /// Begins the activity, once: holding it again changes nothing.
    pub(super) fn hold(reason: &str) {
        HELD.with_borrow_mut(|held| {
            if held.is_none() {
                let reason = NSString::from_str(reason);
                // SAFETY: `NSProcessInfo` is thread-safe, the options are one
                // of Foundation's own constants and `reason` is a live string;
                // the activity returned is kept until `release` ends it.
                let activity = unsafe {
                    NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                        NSActivityOptions::NSActivityUserInitiated,
                        &reason,
                    )
                };
                *held = Some(activity);
            }
        });
    }

    /// Ends the activity, if one is held.
    pub(super) fn release() {
        HELD.with_borrow_mut(|held| {
            if let Some(activity) = held.take() {
                // SAFETY: `activity` is the object `beginActivity…` returned
                // on this thread, ended exactly once.
                unsafe { NSProcessInfo::processInfo().endActivity(&activity) };
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The assertions macOS holds right now, as `pmset` prints them.
    fn assertions() -> String {
        let out = Command::new("/usr/bin/pmset")
            .args(["-g", "assertions"])
            .output()
            .expect("pmset runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn held_by_us(listing: &str) -> bool {
        listing
            .lines()
            .any(|line| line.contains(AWAKE_REASON) && line.contains("PreventUserIdleSystemSleep"))
    }

    // The real activity, held for a moment: it changes nothing the user can
    // see (no window, no sound), and it is what proves the Mac is really held.
    #[test]
    fn the_mac_is_held_awake_under_our_name_and_let_go() {
        let awake = SystemAwake;
        assert!(!held_by_us(&assertions()), "nothing held before");

        assert!(awake.keep_awake(true));
        assert!(awake.keep_awake(true), "holding twice is still one hold");
        let listing = assertions();
        assert!(held_by_us(&listing), "pmset shows it: {listing}");
        assert_eq!(
            listing.matches(AWAKE_REASON).count(),
            1,
            "one activity, not two"
        );

        assert!(!awake.keep_awake(false));
        assert!(!held_by_us(&assertions()), "let go");
        assert!(!awake.keep_awake(false), "letting go twice is harmless");
    }

    #[test]
    fn the_recorder_keeps_every_change() {
        let awake = RecordingAwake::default();
        assert!(awake.keep_awake(true));
        assert!(!awake.keep_awake(false));
        assert_eq!(*awake.changes.lock().unwrap(), [true, false]);

        let refusing = RecordingAwake {
            refuse: true,
            ..RecordingAwake::default()
        };
        assert!(!refusing.keep_awake(true));
    }
}
