use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::mapper;
use crate::dto::{Entry, Error};
use crate::service::names;

/// Moves one item to the Trash; injected so tests don't fill the real one.
/// It blocks, so it is called off the async workers (`trash_off_thread`).
pub type Trash = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

/// The macOS Trash, through `NSFileManager`'s `trashItemAtURL` in this
/// process. Never through Finder: the crate's default runs `osascript` to
/// ask Finder, which has no time limit, needs the user's leave to control
/// Finder, and left the pane on "Working…" for good while Finder didn't
/// answer. The price is that Finder's Put Back may be missing on some macOS
/// versions (the item can still be dragged out of the Trash).
pub fn system_trash(path: &Path) -> std::io::Result<()> {
    trash_context().delete(path).map_err(std::io::Error::other)
}

fn trash_context() -> trash::TrashContext {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context
}

/// Browses and edits folders on this Mac, the other side of every transfer.
pub struct Service {
    home: PathBuf,
    pub(super) trash: Trash,
    /// A test's way of making every move cross a volume, which it can't
    /// otherwise: moves copy and then trash the originals.
    #[cfg(test)]
    pub(super) copy_moves: bool,
}

impl Service {
    /// Deleting moves items to the macOS Trash, as Finder does.
    pub fn new(home: PathBuf) -> Self {
        Self::with_trash(home, Arc::new(system_trash))
    }

    pub fn with_trash(home: PathBuf, trash: Trash) -> Self {
        Self {
            home,
            trash,
            #[cfg(test)]
            copy_moves: false,
        }
    }

    /// Where the Mac pane starts.
    pub fn home(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }

    /// Lists one folder in directory order. Symlinks are shown as what they
    /// point to; a broken one is shown as an empty file rather than hidden.
    pub async fn list(&self, path: &str, show_hidden: bool) -> Result<Vec<Entry>, Error> {
        let folder = Path::new(path);
        let mut reader = tokio::fs::read_dir(folder)
            .await
            .map_err(|err| mapper::to_error(&err, path))?;

        let mut entries = Vec::new();
        loop {
            let item = match reader.next_entry().await {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(err) => return Err(mapper::to_error(&err, path)),
            };
            let metadata = match tokio::fs::metadata(item.path()).await {
                Ok(metadata) => metadata,
                Err(_) => match tokio::fs::symlink_metadata(item.path()).await {
                    Ok(metadata) => metadata,
                    // Vanished between listing and reading: skip it.
                    Err(_) => continue,
                },
            };

            let entry =
                mapper::to_entry(item.file_name().to_string_lossy().into_owned(), &metadata);
            if show_hidden || !(entry.is_hidden() || hidden_in_finder(&metadata)) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Opens Finder with `path` selected.
    pub fn reveal(&self, path: &str) {
        let (program, args) = reveal_command(path);
        // Nothing to report if Finder can't be asked; the copy is done anyway.
        let _ = std::process::Command::new(program).args(args).spawn();
    }

    /// Creates `name` inside `parent` and returns the name it was given.
    pub async fn make_folder(&self, parent: &str, name: &str) -> Result<String, Error> {
        let name = names::validate(name)?;
        let target = Path::new(parent).join(name);
        tokio::fs::create_dir(&target)
            .await
            .map_err(|err| mapper::to_error(&err, &target.to_string_lossy()))?;
        Ok(name.to_owned())
    }

    /// Renames `old` inside `parent` and returns the new name. A change of
    /// case alone is allowed; any other existing name is refused rather than
    /// silently replaced, which a plain rename would do.
    pub async fn rename(&self, parent: &str, old: &str, new: &str) -> Result<String, Error> {
        let new = names::validate(new)?;
        if new == old {
            return Ok(new.to_owned());
        }
        let from = Path::new(parent).join(old);
        let to = Path::new(parent).join(new);
        tokio::fs::symlink_metadata(&from)
            .await
            .map_err(|err| mapper::to_error(&err, &from.to_string_lossy()))?;
        if !names::same(old, new) && tokio::fs::symlink_metadata(&to).await.is_ok() {
            return Err(Error::NameTaken(new.to_owned()));
        }

        tokio::fs::rename(&from, &to)
            .await
            .map_err(|err| mapper::to_error(&err, &from.to_string_lossy()))?;
        Ok(new.to_owned())
    }

    /// Moves `names` inside `parent` to the Trash. Every name is checked
    /// first, so a missing one trashes nothing.
    pub async fn delete(&self, parent: &str, names: &[String]) -> Result<(), Error> {
        let mut targets = Vec::with_capacity(names.len());
        for name in names {
            let target = Path::new(parent).join(name);
            tokio::fs::symlink_metadata(&target)
                .await
                .map_err(|err| mapper::to_error(&err, &target.to_string_lossy()))?;
            targets.push(target);
        }
        for target in targets {
            self.trash_off_thread(&target)
                .await
                .map_err(|err| mapper::to_error(&err, &target.to_string_lossy()))?;
        }
        Ok(())
    }

    /// The Trash for `path`, on a thread of its own: it blocks, and on an
    /// async worker it would hold up every other task the worker has.
    pub(super) async fn trash_off_thread(&self, path: &Path) -> std::io::Result<()> {
        let (trash, path) = (Arc::clone(&self.trash), path.to_path_buf());
        tokio::task::spawn_blocking(move || trash(&path))
            .await
            .map_err(std::io::Error::other)?
    }
}

/// `open -R` opens a Finder window with the item selected.
fn reveal_command(path: &str) -> (&'static str, [&str; 2]) {
    ("open", ["-R", path])
}

/// macOS hides some items without a dot (`~/Library`, `/usr`) via the
/// UF_HIDDEN flag; Finder respects it and so does the Mac pane.
#[cfg(target_os = "macos")]
fn hidden_in_finder(metadata: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const UF_HIDDEN: u32 = 0x8000;
    metadata.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(target_os = "macos"))]
fn hidden_in_finder(_metadata: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    /// A home with Documents/report.pdf, Music/, notes.txt, a dot-file and
    /// a symlink to Documents.
    fn home() -> (tempfile::TempDir, Service) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Documents")).unwrap();
        fs::create_dir_all(dir.path().join("Music")).unwrap();
        fs::write(dir.path().join("Documents/report.pdf"), b"pdf").unwrap();
        fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
        fs::write(dir.path().join(".zshrc"), b"").unwrap();
        symlink(dir.path().join("Documents"), dir.path().join("Docs link")).unwrap();
        let service = Service::with_trash(dir.path().to_path_buf(), Arc::new(remove));
        (dir, service)
    }

    /// Stands in for the Trash: removes the item for real.
    fn remove(path: &Path) -> std::io::Result<()> {
        if fs::symlink_metadata(path)?.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    fn path(dir: &tempfile::TempDir, rel: &str) -> String {
        dir.path().join(rel).to_string_lossy().into_owned()
    }

    fn sorted_names(entries: &[Entry]) -> Vec<&str> {
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn home_is_where_the_pane_starts() {
        let (dir, service) = home();

        assert_eq!(service.home(), dir.path().to_string_lossy());
    }

    #[tokio::test]
    async fn lists_files_and_folders_without_dot_files() {
        let (dir, service) = home();

        let entries = service.list(&service.home(), false).await.unwrap();

        assert_eq!(
            sorted_names(&entries),
            ["Docs link", "Documents", "Music", "notes.txt"]
        );
        let notes = entries.iter().find(|e| e.name == "notes.txt").unwrap();
        assert_eq!(notes.size_bytes, 5);
        assert!(!notes.is_folder);
        assert!(notes.modified.is_some());
        drop(dir);
    }

    #[tokio::test]
    async fn dot_files_are_listed_when_asked() {
        let (_dir, service) = home();

        let entries = service.list(&service.home(), true).await.unwrap();

        assert!(entries.iter().any(|e| e.name == ".zshrc"));
    }

    #[tokio::test]
    async fn a_symlink_to_a_folder_is_a_folder() {
        let (_dir, service) = home();

        let entries = service.list(&service.home(), false).await.unwrap();

        let link = entries.iter().find(|e| e.name == "Docs link").unwrap();
        assert!(link.is_folder);
    }

    #[tokio::test]
    async fn a_broken_symlink_is_still_listed() {
        let (dir, service) = home();
        symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();

        let entries = service.list(&service.home(), false).await.unwrap();

        let dangling = entries.iter().find(|e| e.name == "dangling").unwrap();
        assert!(!dangling.is_folder);
    }

    #[tokio::test]
    async fn nested_folders_list_their_children() {
        let (dir, service) = home();

        let entries = service.list(&path(&dir, "Documents"), false).await.unwrap();

        assert_eq!(sorted_names(&entries), ["report.pdf"]);
    }

    #[tokio::test]
    async fn a_missing_folder_is_path_not_found() {
        let (dir, service) = home();
        let missing = path(&dir, "Nope");

        let result = service.list(&missing, false).await;

        assert_eq!(result, Err(Error::PathNotFound(missing)));
    }

    #[tokio::test]
    async fn a_file_is_not_a_folder() {
        let (dir, service) = home();
        let file = path(&dir, "notes.txt");

        let result = service.list(&file, false).await;

        assert_eq!(result, Err(Error::PathNotFound(file)));
    }

    #[tokio::test]
    async fn an_unreadable_folder_is_access_denied() {
        let (dir, service) = home();
        let locked = dir.path().join("Locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let locked_path = locked.to_string_lossy().into_owned();

        let result = service.list(&locked_path, false).await;

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result, Err(Error::FolderAccessDenied(locked_path)));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn items_hidden_in_finder_are_hidden_unless_asked() {
        let (dir, service) = home();
        let status = std::process::Command::new("chflags")
            .arg("hidden")
            .arg(dir.path().join("Music"))
            .status()
            .unwrap();
        assert!(status.success());

        let hidden = service.list(&service.home(), false).await.unwrap();
        let shown = service.list(&service.home(), true).await.unwrap();

        assert!(!hidden.iter().any(|e| e.name == "Music"));
        assert!(shown.iter().any(|e| e.name == "Music"));
    }

    #[tokio::test]
    async fn make_folder_creates_it() {
        let (dir, service) = home();

        let name = service
            .make_folder(&service.home(), " Trip ")
            .await
            .unwrap();

        assert_eq!(name, "Trip");
        assert!(dir.path().join("Trip").is_dir());
    }

    #[tokio::test]
    async fn make_folder_refuses_a_taken_name() {
        let (_dir, service) = home();

        let result = service.make_folder(&service.home(), "Music").await;

        assert_eq!(result, Err(Error::NameTaken("Music".into())));
    }

    #[tokio::test]
    async fn make_folder_refuses_an_invalid_name() {
        let (_dir, service) = home();

        let result = service.make_folder(&service.home(), "").await;

        assert_eq!(
            result,
            Err(Error::InvalidName(crate::dto::NameProblem::Empty))
        );
    }

    #[tokio::test]
    async fn make_folder_in_a_missing_folder_is_path_not_found() {
        let (dir, service) = home();

        let result = service.make_folder(&path(&dir, "Nope"), "Trip").await;

        assert_eq!(result, Err(Error::PathNotFound(path(&dir, "Nope/Trip"))));
    }

    #[tokio::test]
    async fn rename_a_file() {
        let (dir, service) = home();

        let name = service
            .rename(&service.home(), "notes.txt", "todo.txt")
            .await;

        assert_eq!(name, Ok("todo.txt".into()));
        assert!(dir.path().join("todo.txt").exists());
        assert!(!dir.path().join("notes.txt").exists());
    }

    #[tokio::test]
    async fn rename_never_replaces_another_item() {
        let (dir, service) = home();
        fs::write(dir.path().join("todo.txt"), b"keep me").unwrap();

        let result = service
            .rename(&service.home(), "notes.txt", "todo.txt")
            .await;

        assert_eq!(result, Err(Error::NameTaken("todo.txt".into())));
        assert_eq!(fs::read(dir.path().join("todo.txt")).unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn rename_may_change_only_the_case() {
        let (dir, service) = home();

        let name = service
            .rename(&service.home(), "notes.txt", "Notes.txt")
            .await;

        assert_eq!(name, Ok("Notes.txt".into()));
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"Notes.txt".to_owned()));
    }

    #[tokio::test]
    async fn rename_to_the_same_name_does_nothing() {
        let (_dir, service) = home();

        let name = service.rename("/nowhere", "notes.txt", "notes.txt").await;

        assert_eq!(name, Ok("notes.txt".into()));
    }

    #[tokio::test]
    async fn rename_of_a_missing_item_is_path_not_found() {
        let (dir, service) = home();

        let result = service.rename(&service.home(), "ghost", "x").await;

        assert_eq!(result, Err(Error::PathNotFound(path(&dir, "ghost"))));
    }

    #[tokio::test]
    async fn delete_moves_every_item_to_the_trash() {
        let (dir, service) = home();

        service
            .delete(&service.home(), &["notes.txt".into(), "Documents".into()])
            .await
            .unwrap();

        assert!(!dir.path().join("notes.txt").exists());
        assert!(!dir.path().join("Documents").exists());
    }

    #[tokio::test]
    async fn delete_with_a_missing_name_trashes_nothing() {
        let (dir, service) = home();

        let result = service
            .delete(&service.home(), &["notes.txt".into(), "ghost".into()])
            .await;

        assert_eq!(result, Err(Error::PathNotFound(path(&dir, "ghost"))));
        assert!(dir.path().join("notes.txt").exists());
    }

    #[tokio::test]
    async fn a_trash_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let service = Service::with_trash(
            dir.path().to_path_buf(),
            Arc::new(|_: &Path| Err(std::io::Error::other("Trash is full"))),
        );

        let result = service.delete(&service.home(), &["a.txt".into()]).await;

        assert_eq!(result, Err(Error::General("Trash is full".into())));
    }

    #[test]
    fn the_mac_trash_is_macos_own_never_a_script_to_finder() {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        // Finder's way waits on `osascript` with no limit and needs leave to
        // control Finder: a delete once stayed on "Working…" for good.
        assert!(matches!(
            trash_context().delete_method(),
            DeleteMethod::NsFileManager
        ));
    }

    #[tokio::test]
    async fn a_slow_trash_holds_up_nothing_else_the_app_is_doing() {
        // One async worker, as in a test: a Trash that blocked it would keep
        // the other task from running, so the Trash would never hear from it.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        // Each Trash call says it has started, then waits for the other task
        // to answer, which that task can only do while the worker is free.
        let (started, waiting) = std::sync::mpsc::channel::<()>();
        let (go, heard) = std::sync::mpsc::channel::<()>();
        let (started, heard) = (std::sync::Mutex::new(started), std::sync::Mutex::new(heard));
        let service = Service::with_trash(
            dir.path().to_path_buf(),
            Arc::new(move |path: &Path| {
                started.lock().unwrap().send(()).unwrap();
                heard
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| std::io::Error::other("the app was held up"))?;
                remove(path)
            }),
        );
        let other = tokio::spawn(async move {
            for _ in 0..2 {
                while waiting.try_recv().is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                go.send(()).unwrap();
            }
        });

        let result = service
            .delete(&service.home(), &["a.txt".into(), "b.txt".into()])
            .await;

        assert_eq!(result, Ok(()));
        other.await.unwrap();
        assert!(!dir.path().join("a.txt").exists());
        assert!(!dir.path().join("b.txt").exists());
    }

    #[test]
    fn reveal_asks_finder_to_select_the_item() {
        assert_eq!(
            reveal_command("/Users/me/Downloads/a.jpg"),
            ("open", ["-R", "/Users/me/Downloads/a.jpg"])
        );
    }
}
