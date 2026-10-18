//! The Mac's share menu (AirDrop, Messages, Mail, Notes…) for files on the
//! Mac. A seam: the app shows AppKit's picker, tests record.
//!
//! AppKit may only be touched on the main thread (where the app's updates
//! run); anywhere else, tests included, nothing is shown.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::dto::Error;

pub trait Sharer: Send + Sync {
    /// Shows the share menu for `paths` beside `at` (points from the top
    /// left of the window's content; `None`: its middle). False when it
    /// can't be shown (no window, or not on the main thread).
    fn share(&self, paths: &[PathBuf], at: Option<(f64, f64)>) -> bool;
}

/// Shares `paths` through `sharer`, once each is there: a file gone since it
/// was listed says so rather than offering to send nothing.
pub fn share_files(
    sharer: &dyn Sharer,
    paths: &[PathBuf],
    at: Option<(f64, f64)>,
) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }
    if let Some(gone) = paths.iter().find(|path| !path.exists()) {
        return Err(Error::PathNotFound(gone.to_string_lossy().into_owned()));
    }
    if sharer.share(paths, at) {
        Ok(())
    } else {
        Err(Error::General("couldn't show the share menu".into()))
    }
}

/// Where the picker points, in the view's own coordinates: AppKit counts
/// from the bottom unless the view is flipped. `None` is the middle.
pub(crate) fn anchor(at: Option<(f64, f64)>, width: f64, height: f64, flipped: bool) -> (f64, f64) {
    let (x, y) = at.unwrap_or((width / 2.0, height / 2.0));
    let (x, y) = (x.clamp(0.0, width), y.clamp(0.0, height));
    if flipped { (x, y) } else { (x, height - y) }
}

/// The real one: AppKit's `NSSharingServicePicker`.
#[derive(Default)]
pub struct SystemSharer;

impl Sharer for SystemSharer {
    fn share(&self, paths: &[PathBuf], at: Option<(f64, f64)>) -> bool {
        system::share(paths, at)
    }
}

/// One share a recorder saw: the files, and where the menu showed.
pub type Shared = (Vec<PathBuf>, Option<(f64, f64)>);

/// What tests see: every set of files shared and where; `refuses` makes it
/// say it couldn't show the menu.
#[derive(Default)]
pub struct RecordingSharer {
    pub shared: Mutex<Vec<Shared>>,
    pub refuses: bool,
}

impl Sharer for RecordingSharer {
    fn share(&self, paths: &[PathBuf], at: Option<(f64, f64)>) -> bool {
        if self.refuses {
            return false;
        }
        self.shared.lock().unwrap().push((paths.to_vec(), at));
        true
    }
}

mod system {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSSharingServicePicker};
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL,
    };

    thread_local! {
        /// The picker on screen: kept until the next one, so AppKit has it
        /// for as long as it shows.
        static PICKER: RefCell<Option<Retained<NSSharingServicePicker>>> =
            const { RefCell::new(None) };
    }

    pub(super) fn share(paths: &[PathBuf], at: Option<(f64, f64)>) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: reading the app's windows on the main thread.
        let Some(window) = app.keyWindow().or_else(|| unsafe { app.mainWindow() }) else {
            return false;
        };
        let Some(view) = window.contentView() else {
            return false;
        };
        let urls: Vec<Retained<objc2::runtime::AnyObject>> = paths
            .iter()
            .map(|path| {
                // SAFETY: a file URL made from a path string.
                let url =
                    unsafe { NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())) };
                Retained::into_super(Retained::into_super(url))
            })
            .collect();
        let items = NSArray::from_vec(urls);
        let bounds = view.bounds();
        let (x, y) = super::anchor(at, bounds.size.width, bounds.size.height, view.isFlipped());
        let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(1.0, 1.0));
        // SAFETY: a picker made from file URLs, shown from the window's own
        // view, on the main thread; kept in `PICKER` while it shows.
        unsafe {
            let picker = NSSharingServicePicker::initWithItems(mtm.alloc(), &items);
            picker.showRelativeToRect_ofView_preferredEdge(rect, &view, NSRectEdge::NSMinYEdge);
            PICKER.with_borrow_mut(|kept| *kept = Some(picker));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_that_are_there_are_shared_where_asked() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        std::fs::write(&a, b"x").unwrap();
        let sharer = RecordingSharer::default();

        share_files(&sharer, std::slice::from_ref(&a), Some((40.0, 120.0))).unwrap();
        share_files(&sharer, &[], None).unwrap();

        assert_eq!(
            *sharer.shared.lock().unwrap(),
            [(vec![a.clone()], Some((40.0, 120.0)))],
            "nothing to share shows nothing"
        );
    }

    #[test]
    fn a_file_gone_or_a_menu_that_cannot_show_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let sharer = RecordingSharer::default();
        let gone = dir.path().join("gone.jpg");

        assert!(matches!(
            share_files(&sharer, &[gone], None),
            Err(Error::PathNotFound(path)) if path.ends_with("gone.jpg")
        ));
        assert!(sharer.shared.lock().unwrap().is_empty());

        let here = dir.path().join("here.jpg");
        std::fs::write(&here, b"x").unwrap();
        let refusing = RecordingSharer {
            refuses: true,
            ..RecordingSharer::default()
        };
        assert!(matches!(
            share_files(&refusing, std::slice::from_ref(&here), None),
            Err(Error::General(_))
        ));
        assert!(
            !SystemSharer.share(&[here], None),
            "off the main thread nothing is shown"
        );
    }

    #[test]
    fn the_picker_points_where_asked_counted_as_appkit_counts() {
        assert_eq!(
            anchor(Some((40.0, 100.0)), 800.0, 600.0, false),
            (40.0, 500.0)
        );
        assert_eq!(
            anchor(Some((40.0, 100.0)), 800.0, 600.0, true),
            (40.0, 100.0)
        );
        assert_eq!(anchor(None, 800.0, 600.0, false), (400.0, 300.0));
        assert_eq!(
            anchor(Some((900.0, -5.0)), 800.0, 600.0, true),
            (800.0, 0.0),
            "kept inside the view"
        );
    }
}
