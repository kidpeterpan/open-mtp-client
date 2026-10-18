//! Quick Look, as Finder's Space bar opens it: `qlmanage -p` shows the files
//! in a preview panel of its own; pressing Space again closes it.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub use crate::dto::Look as Shown;

/// Shows files in Quick Look, or closes the panel that is open. A seam: the
/// app runs `qlmanage`, tests record what they are asked.
pub trait QuickLook: Send + Sync {
    fn toggle(&self, paths: &[PathBuf]) -> std::io::Result<Shown>;
}

/// `quick_look.toggle(paths)`, with what went wrong said as the app says it.
pub fn show_in_quick_look(
    quick_look: &dyn QuickLook,
    paths: &[PathBuf],
) -> Result<Shown, crate::dto::Error> {
    quick_look
        .toggle(paths)
        .map_err(|err| super::mapper::to_error(&err, "Quick Look"))
}

/// The real one: one `qlmanage -p` at a time, closed with the app.
#[derive(Default)]
pub struct SystemQuickLook {
    open: Mutex<Option<Child>>,
}

impl QuickLook for SystemQuickLook {
    fn toggle(&self, paths: &[PathBuf]) -> std::io::Result<Shown> {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Still showing (the user may have closed it themselves): Space
        // closes it, as in Finder.
        if let Some(child) = open.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = child.kill();
            let _ = child.wait();
            *open = None;
            return Ok(Shown::Closed);
        }
        let child = Command::new("qlmanage")
            .arg("-p")
            .args(paths)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        *open = Some(child);
        Ok(Shown::Opened)
    }
}

impl Drop for SystemQuickLook {
    fn drop(&mut self) {
        if let Ok(mut open) = self.open.lock()
            && let Some(child) = open.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What tests see: every set of paths shown, opening and closing in turn as
/// the real panel would while it stays open.
#[derive(Default)]
pub struct RecordingQuickLook {
    pub shown: Mutex<Vec<Vec<PathBuf>>>,
    open: Mutex<bool>,
}

impl QuickLook for RecordingQuickLook {
    fn toggle(&self, paths: &[PathBuf]) -> std::io::Result<Shown> {
        let mut open = self.open.lock().unwrap();
        if *open {
            *open = false;
            return Ok(Shown::Closed);
        }
        self.shown.lock().unwrap().push(paths.to_vec());
        *open = true;
        Ok(Shown::Opened)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorder_opens_and_closes_in_turn_like_the_panel() {
        let look = RecordingQuickLook::default();
        let a = vec![PathBuf::from("/a.jpg")];

        assert_eq!(look.toggle(&a).unwrap(), Shown::Opened);
        assert_eq!(look.toggle(&a).unwrap(), Shown::Closed);
        assert_eq!(look.toggle(&a).unwrap(), Shown::Opened);
        assert_eq!(look.shown.lock().unwrap().len(), 2);
        assert_eq!(show_in_quick_look(&look, &a), Ok(Shown::Closed));
    }

    struct Missing;

    impl QuickLook for Missing {
        fn toggle(&self, _paths: &[PathBuf]) -> std::io::Result<Shown> {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }

    #[test]
    fn a_quick_look_that_cannot_start_says_so_in_the_apps_words() {
        assert!(show_in_quick_look(&Missing, &[]).is_err());
    }
}
