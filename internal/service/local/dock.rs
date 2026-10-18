//! The app's Dock icon, notifications and sounds: a progress bar on the icon
//! while a transfer runs, a notification when one finishes while the user is
//! in another app, and, if asked for, one of the Mac's own sounds when the
//! queue empties. A seam: the app draws on the real Dock and plays the real
//! sound, tests record.
//!
//! AppKit may only be touched from the main thread (where the app's updates
//! run); anywhere else the Dock is left alone. Notifications go through the
//! notification centre when the app runs as a bundle (it then has a name and
//! an icon to post them under, and has asked once for permission); a run
//! straight from a terminal has neither, and the centre would abort the
//! process, so it posts through `osascript` instead.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::dto::Sound;

pub trait Dock: Send + Sync {
    /// Shows `percent` of a transfer on the Dock icon; `None` takes it away.
    fn show_progress(&self, percent: Option<u8>);
    /// Tells the user something finished (or waits for them) while they are
    /// elsewhere.
    fn notify(&self, title: &str, body: &str);
    /// Plays the Mac's own sound for `sound`, and returns at once.
    fn chime(&self, sound: Sound);
}

/// What tests see: every change of the bar, every notification and every
/// sound.
#[derive(Default)]
pub struct RecordingDock {
    pub progress: Mutex<Vec<Option<u8>>>,
    pub notes: Mutex<Vec<(String, String)>>,
    pub sounds: Mutex<Vec<Sound>>,
}

impl Dock for RecordingDock {
    fn show_progress(&self, percent: Option<u8>) {
        self.progress.lock().unwrap().push(percent);
    }

    fn notify(&self, title: &str, body: &str) {
        self.notes
            .lock()
            .unwrap()
            .push((title.to_owned(), body.to_owned()));
    }

    fn chime(&self, sound: Sound) {
        self.sounds.lock().unwrap().push(sound);
    }
}

/// The real Dock and notification centre.
#[derive(Default)]
pub struct SystemDock;

impl SystemDock {
    /// Asks, once at start-up, whether the app may post notifications. Asked
    /// early because a notification posted before the answer is dropped.
    pub fn ask_to_notify(&self) {
        if system::is_bundled() {
            system::ask_to_notify();
        }
    }
}

impl Dock for SystemDock {
    fn show_progress(&self, percent: Option<u8>) {
        system::show_progress(percent);
    }

    fn notify(&self, title: &str, body: &str) {
        if system::is_bundled() {
            system::post(title, body);
        } else {
            run_briefly("/usr/bin/osascript", &osascript_args(title, body));
        }
    }

    fn chime(&self, sound: Sound) {
        run_briefly(AFPLAY, &[sound_file(sound).to_owned()]);
    }
}

/// Plays a sound file and returns when it ends.
const AFPLAY: &str = "/usr/bin/afplay";

/// The Mac's own sound for `sound`, as Mail and Xcode use them: Glass when
/// all is done, Basso when something went wrong.
pub fn sound_file(sound: Sound) -> &'static str {
    match sound {
        Sound::Done => "/System/Library/Sounds/Glass.aiff",
        Sound::Problem => "/System/Library/Sounds/Basso.aiff",
    }
}

/// The Dock tile as it looks at `percent`, over the icon at `icon`, drawn
/// offscreen to PNG bytes: how the bar is checked by eye without touching the
/// Dock (`cargo run --example dock_tile`). Only on the main thread; the
/// process gets no Dock icon of its own.
pub fn dock_tile_png(icon: &str, percent: u8) -> Option<Vec<u8>> {
    system::tile_png(icon, percent)
}

/// `osascript`'s arguments for a notification. The title and body are the
/// script's own arguments, never part of its text, so a file name with quotes
/// in it can't become a script; `--` keeps one starting with `-` from being
/// read as an option.
pub(crate) fn osascript_args(title: &str, body: &str) -> Vec<String> {
    [
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
        "--",
        title,
        body,
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

/// How long `osascript` or `afplay` may take before it is given up on.
const HELPER_LIMIT: Duration = Duration::from_secs(10);

/// Starts `program` with `args` and lets it go: reaped off the UI thread,
/// and stopped if it is still going after `HELPER_LIMIT`.
fn run_briefly(program: &str, args: &[String]) {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // Reaped off the UI thread, and stopped if it hangs.
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        while started.elapsed() < HELPER_LIMIT {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    });
}

/// The AppKit and notification-centre calls. Every one is `unsafe` only
/// because the bindings are; each is made on the main thread, with objects
/// the framework hands out, as Apple documents them.
mod system {
    use std::cell::RefCell;

    use block2::RcBlock;
    use objc2::ClassType;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBitmapImageFileType, NSBox, NSBoxType,
        NSColor, NSControlSize, NSImage, NSImageScaling, NSImageView, NSProgressIndicator,
        NSProgressIndicatorStyle, NSTitlePosition, NSView,
    };
    use objc2_foundation::{
        MainThreadMarker, NSBundle, NSDictionary, NSError, NSPoint, NSRect, NSSize, NSString,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };

    /// The views drawn on the Dock icon while a transfer runs.
    struct Tile {
        content: Retained<NSView>,
        bar: Retained<NSProgressIndicator>,
    }

    /// The icon filling a tile of `size`, with a bar along its bottom, as
    /// Finder draws its own. `mtm` is the proof it is on the main thread.
    fn build_tile(mtm: MainThreadMarker, icon: Option<&NSImage>, size: NSSize) -> Tile {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
        // SAFETY: on the main thread (`mtm`), with views made here.
        unsafe {
            let content = NSView::initWithFrame(mtm.alloc(), frame);
            if let Some(icon) = icon {
                let image = NSImageView::imageViewWithImage(icon, mtm);
                image.setFrame(frame);
                image.setImageScaling(NSImageScaling::NSImageScaleProportionallyUpOrDown);
                content.addSubview(&image);
            }
            // A light rounded panel under the bar, so its empty part shows on
            // any icon.
            let (left, bottom) = (size.width * 0.16, size.height * 0.15);
            let (width, height) = (size.width * 0.68, size.height * 0.11);
            let panel = NSBox::initWithFrame(
                mtm.alloc(),
                NSRect::new(NSPoint::new(left, bottom), NSSize::new(width, height)),
            );
            panel.setBoxType(NSBoxType::NSBoxCustom);
            panel.setTitlePosition(NSTitlePosition::NSNoTitle);
            panel.setBorderWidth(0.0);
            panel.setCornerRadius(height / 2.0);
            panel.setFillColor(&NSColor::colorWithWhite_alpha(1.0, 0.9));
            content.addSubview(&panel);
            let inset = height * 0.25;
            let bar = NSProgressIndicator::initWithFrame(
                mtm.alloc(),
                NSRect::new(
                    NSPoint::new(left + inset, bottom + inset),
                    NSSize::new(width - 2.0 * inset, height - 2.0 * inset),
                ),
            );
            bar.setControlSize(NSControlSize::Large);
            bar.setStyle(NSProgressIndicatorStyle::Bar);
            bar.setIndeterminate(false);
            bar.setMinValue(0.0);
            bar.setMaxValue(100.0);
            content.addSubview(&bar);
            Tile { content, bar }
        }
    }

    pub(super) fn tile_png(icon: &str, percent: u8) -> Option<Vec<u8>> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        // No icon in the user's Dock for a process that only draws.
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        // SAFETY: on the main thread (checked above), with objects built here.
        unsafe {
            let image =
                NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(icon))?;
            let size = NSSize::new(256.0, 256.0);
            let tile = build_tile(mtm, Some(&image), size);
            tile.bar.setDoubleValue(f64::from(percent));
            let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
            let bitmap = tile.content.bitmapImageRepForCachingDisplayInRect(rect)?;
            tile.content
                .cacheDisplayInRect_toBitmapImageRep(rect, &bitmap);
            let png = bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )?;
            Some(png.bytes().to_vec())
        }
    }

    thread_local! {
        static TILE: RefCell<Option<Tile>> = const { RefCell::new(None) };
    }

    pub(super) fn is_bundled() -> bool {
        // SAFETY: reading the main bundle's identifier has no requirements.
        unsafe { NSBundle::mainBundle().bundleIdentifier().is_some() }
    }

    pub(super) fn show_progress(percent: Option<u8>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: on the main thread (checked above), with AppKit's own objects.
        unsafe {
            let tile = app.dockTile();
            match percent {
                None => {
                    TILE.with(|views| views.borrow_mut().take());
                    tile.setContentView(None);
                }
                Some(percent) => TILE.with(|views| {
                    let mut views = views.borrow_mut();
                    if views.is_none() {
                        let icon = app.applicationIconImage();
                        let built = build_tile(mtm, icon.as_deref(), tile.size());
                        tile.setContentView(Some(&built.content));
                        *views = Some(built);
                    }
                    if let Some(views) = views.as_ref() {
                        views.bar.setDoubleValue(f64::from(percent));
                    }
                }),
            }
            // Nothing on the tile changes until it is told to draw.
            tile.display();
        }
    }

    pub(super) fn ask_to_notify() {
        let answered = RcBlock::new(|_granted: Bool, _error: *mut NSError| {});
        // SAFETY: only called when the app is a bundle (the centre aborts
        // otherwise); the block lives as long as the centre needs it.
        unsafe {
            UNUserNotificationCenter::currentNotificationCenter()
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::UNAuthorizationOptionAlert
                        | UNAuthorizationOptions::UNAuthorizationOptionSound,
                    &answered,
                );
        }
    }

    pub(super) fn post(title: &str, body: &str) {
        let id = format!(
            "open-mtp-client-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        );
        // SAFETY: only called when the app is a bundle; plain objects built
        // and handed to the centre.
        unsafe {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(body));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&id),
                &content,
                None,
            );
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notifications_words_are_the_scripts_arguments_never_its_text() {
        let args = osascript_args("Downloaded \"x\"; do shell script \"rm\"", "-r all arrived");

        assert_eq!(
            &args[..7],
            [
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                "--",
            ]
        );
        assert_eq!(
            &args[7..],
            ["Downloaded \"x\"; do shell script \"rm\"", "-r all arrived"]
        );
    }

    #[test]
    fn the_recorder_keeps_every_change_and_note() {
        let dock = RecordingDock::default();
        dock.show_progress(Some(40));
        dock.show_progress(None);
        dock.notify("Downloaded 1 file", "all arrived");

        assert_eq!(*dock.progress.lock().unwrap(), [Some(40), None]);
        assert_eq!(dock.notes.lock().unwrap().len(), 1);

        dock.chime(Sound::Problem);
        assert_eq!(*dock.sounds.lock().unwrap(), [Sound::Problem]);
    }

    #[test]
    fn each_sound_is_one_of_the_macs_own_and_they_differ() {
        for sound in [Sound::Done, Sound::Problem] {
            let file = sound_file(sound);
            assert!(file.starts_with("/System/Library/Sounds/"), "{file}");
            assert!(
                std::path::Path::new(file).is_file(),
                "{file} is on every Mac"
            );
        }
        assert_ne!(sound_file(Sound::Done), sound_file(Sound::Problem));
        assert!(std::path::Path::new(AFPLAY).is_file());
    }

    #[test]
    fn away_from_the_main_thread_the_dock_is_left_alone() {
        // Tests run off the main thread: this must do nothing, not crash. On
        // the main thread it would put an icon in the user's Dock: not here.
        if objc2_foundation::MainThreadMarker::new().is_some() {
            return;
        }
        SystemDock.show_progress(Some(10));
        SystemDock.show_progress(None);
        assert!(!system::is_bundled(), "a test binary is no app bundle");
    }
}
