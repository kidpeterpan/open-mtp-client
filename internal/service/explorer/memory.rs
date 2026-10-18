//! What the explorer remembers of folders it has read from the phone.
//!
//! A phone describes its files one request each, so reading a big folder
//! again just to find three files by name (to rename, delete or download
//! them), or to show it a second time, is the slowest thing the app does.
//! The last full reading of a folder is kept instead, and trusted only while
//! it is plausible: it came from this connection, and the phone still says
//! the folder holds as many items. Whoever acts on a remembered file checks it
//! with the phone first.

use std::collections::{HashMap, VecDeque};

use mtp_rs::mtp::{ObjectHandle, ObjectInfo};

/// What the memory needs to know about an item in a folder: which one it is,
/// and how to rename it. A phone's `ObjectInfo` in the app, a stand-in in tests
/// (the library gives no way to build one).
pub(super) trait Child {
    fn handle(&self) -> ObjectHandle;
    fn rename(&mut self, name: &str);
}

impl Child for ObjectInfo {
    fn handle(&self) -> ObjectHandle {
        self.handle
    }

    fn rename(&mut self, name: &str) {
        self.filename = name.to_owned();
    }
}

/// Device serial, storage id, absolute folder path.
pub(super) type FolderKey = (String, u64, String);

/// How many folders are kept; the oldest goes first.
const KEPT: usize = 12;

/// One folder as last read.
pub(super) struct Remembered<T = ObjectInfo> {
    /// The connection it was read on. A reconnect may re-key every file.
    epoch: u64,
    /// How many items the phone reported for the folder (which can be more
    /// than `children`, when it refused to describe some).
    pub(super) total: usize,
    pub(super) children: Vec<T>,
}

pub(super) struct Memory<T = ObjectInfo> {
    folders: HashMap<FolderKey, Remembered<T>>,
    /// Oldest first.
    order: VecDeque<FolderKey>,
    /// How many folders, and how many items in all, are kept at most; the
    /// oldest folders go first.
    most_folders: usize,
    most_items: usize,
    /// Items kept now, in all folders.
    items: usize,
}

impl<T> Default for Memory<T> {
    fn default() -> Self {
        Self::with_room(KEPT, usize::MAX)
    }
}

impl<T> Memory<T> {
    /// A memory that keeps at most `folders` folders and `items` items.
    pub(super) fn with_room(folders: usize, items: usize) -> Self {
        Self {
            folders: HashMap::new(),
            order: VecDeque::new(),
            most_folders: folders,
            most_items: items,
            items: 0,
        }
    }

    /// The children as last read, if that reading is still plausible: from
    /// connection `epoch`, and the phone still reports `total` items.
    pub(super) fn recall(&self, key: &FolderKey, epoch: u64, total: usize) -> Option<&Vec<T>> {
        let kept = self.folders.get(key)?;
        (kept.epoch == epoch && kept.total == total).then_some(&kept.children)
    }

    pub(super) fn holds(&self, key: &FolderKey) -> bool {
        self.folders.contains_key(key)
    }

    /// The children as last read on connection `epoch`, without asking the
    /// phone whether the folder still holds as many: a hint, never acted on.
    pub(super) fn peek(&self, key: &FolderKey, epoch: u64) -> Option<&Vec<T>> {
        let kept = self.folders.get(key)?;
        (kept.epoch == epoch).then_some(&kept.children)
    }

    pub(super) fn keep(&mut self, key: FolderKey, epoch: u64, total: usize, children: Vec<T>) {
        self.items += children.len();
        if let Some(earlier) = self.folders.insert(
            key.clone(),
            Remembered {
                epoch,
                total,
                children,
            },
        ) {
            self.items -= earlier.children.len();
            self.order.retain(|other| *other != key);
        }
        self.order.push_back(key);
        while self.order.len() > self.most_folders
            || (self.items > self.most_items && self.order.len() > 1)
        {
            if let Some(oldest) = self.order.pop_front()
                && let Some(gone) = self.folders.remove(&oldest)
            {
                self.items -= gone.children.len();
            }
        }
    }

    /// Changes a remembered folder to match something that was just done to
    /// it, so the next reading of it is still right. Nothing if it isn't kept.
    pub(super) fn amend(&mut self, key: &FolderKey, change: impl FnOnce(&mut Remembered<T>)) {
        if let Some(kept) = self.folders.get_mut(key) {
            let before = kept.children.len();
            change(kept);
            self.items = self.items + kept.children.len() - before;
        }
    }

    pub(super) fn forget(&mut self, key: &FolderKey) {
        if let Some(gone) = self.folders.remove(key) {
            self.items -= gone.children.len();
            self.order.retain(|other| other != key);
        }
    }

    /// Forgets a folder and every folder under it on `storage`.
    pub(super) fn forget_below(&mut self, serial: &str, storage: u64, path: &str) {
        let under = |key: &FolderKey| {
            key.0 == serial
                && key.1 == storage
                && (path == "/" || key.2 == path || key.2.starts_with(&format!("{path}/")))
        };
        let gone: usize = self
            .folders
            .iter()
            .filter(|(key, _)| under(key))
            .map(|(_, kept)| kept.children.len())
            .sum();
        self.items -= gone;
        self.folders.retain(|key, _| !under(key));
        self.order.retain(|key| !under(key));
    }

    /// Items kept, in all folders.
    #[cfg(test)]
    pub(super) fn items(&self) -> usize {
        self.items
    }
}

/// The search index can hold this many folders, and items in all: a big phone,
/// at about 200 bytes an item, in some 40 MB at most.
const INDEX_FOLDERS: usize = 20_000;
const INDEX_ITEMS: usize = 200_000;

/// The recent folders the panes use, and the index a search of the whole phone
/// builds, kept true together: every change the app makes to a phone folder
/// goes to both, and a folder is recalled from either (so downloading search
/// results finds their folders without reading them again).
pub(super) struct Remembering<T = ObjectInfo> {
    recent: Memory<T>,
    index: Memory<T>,
}

impl<T> Default for Remembering<T> {
    fn default() -> Self {
        Self {
            recent: Memory::default(),
            index: Memory::with_room(INDEX_FOLDERS, INDEX_ITEMS),
        }
    }
}

impl<T: Clone> Remembering<T> {
    pub(super) fn recall(&self, key: &FolderKey, epoch: u64, total: usize) -> Option<&Vec<T>> {
        self.recent
            .recall(key, epoch, total)
            .or_else(|| self.index.recall(key, epoch, total))
    }

    /// What is remembered of a folder, from either memory, unchecked.
    pub(super) fn peek(&self, key: &FolderKey, epoch: u64) -> Option<&Vec<T>> {
        self.recent
            .peek(key, epoch)
            .or_else(|| self.index.peek(key, epoch))
    }

    /// A folder read in full for a pane. A folder the index holds is brought
    /// up to date there too.
    pub(super) fn keep(&mut self, key: FolderKey, epoch: u64, total: usize, children: Vec<T>) {
        if self.index.holds(&key) {
            self.index.keep(key.clone(), epoch, total, children.clone());
        }
        self.recent.keep(key, epoch, total, children);
    }

    /// A folder read in full by a search.
    pub(super) fn index(&mut self, key: FolderKey, epoch: u64, total: usize, children: Vec<T>) {
        self.index.keep(key, epoch, total, children);
    }

    pub(super) fn amend(&mut self, key: &FolderKey, change: impl Fn(&mut Remembered<T>)) {
        self.recent.amend(key, &change);
        self.index.amend(key, &change);
    }

    pub(super) fn forget(&mut self, key: &FolderKey) {
        self.recent.forget(key);
        self.index.forget(key);
    }

    pub(super) fn forget_below(&mut self, serial: &str, storage: u64, path: &str) {
        self.recent.forget_below(serial, storage, path);
        self.index.forget_below(serial, storage, path);
    }
}

impl<T: Child> Remembered<T> {
    /// An item that was added to the folder.
    pub(super) fn added(&mut self, child: T) {
        self.children.push(child);
        self.total += 1;
    }

    /// Items that were deleted from the folder.
    pub(super) fn removed(&mut self, handles: &[ObjectHandle]) {
        let before = self.children.len();
        self.children
            .retain(|child| !handles.contains(&child.handle()));
        self.total = self.total.saturating_sub(before - self.children.len());
    }

    /// An item that was renamed.
    pub(super) fn renamed(&mut self, handle: ObjectHandle, name: &str) {
        if let Some(child) = self
            .children
            .iter_mut()
            .find(|child| child.handle() == handle)
        {
            child.rename(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Item {
        handle: ObjectHandle,
        name: String,
    }

    impl Child for Item {
        fn handle(&self) -> ObjectHandle {
            self.handle
        }

        fn rename(&mut self, name: &str) {
            self.name = name.to_owned();
        }
    }

    fn object(handle: u64, name: &str) -> Item {
        Item {
            handle: ObjectHandle(handle),
            name: name.to_owned(),
        }
    }

    fn key(path: &str) -> FolderKey {
        ("serial".to_owned(), 1, path.to_owned())
    }

    fn names(memory: &Memory<Item>, path: &str, total: usize) -> Option<Vec<String>> {
        memory
            .recall(&key(path), 1, total)
            .map(|children| children.iter().map(|c| c.name.clone()).collect())
    }

    #[test]
    fn a_memory_keeps_no_more_folders_or_items_than_it_has_room_for() {
        let mut memory: Memory<Item> = Memory::with_room(3, 5);
        memory.keep(key("/a"), 1, 2, vec![object(1, "a1"), object(2, "a2")]);
        memory.keep(key("/b"), 1, 2, vec![object(3, "b1"), object(4, "b2")]);
        assert_eq!(memory.items(), 4);

        // Seven items in all is two too many: the oldest folder goes.
        memory.keep(
            key("/c"),
            1,
            3,
            vec![object(5, "c1"), object(6, "c2"), object(7, "c3")],
        );

        assert_eq!(names(&memory, "/a", 2), None);
        assert!(names(&memory, "/b", 2).is_some() && names(&memory, "/c", 3).is_some());
        assert_eq!(memory.items(), 5);
        memory.amend(&key("/b"), |kept| kept.added(object(8, "b3")));
        assert_eq!(memory.items(), 6, "an amend is counted");
        memory.forget_below("serial", 1, "/");
        assert_eq!(memory.items(), 0);
    }

    #[test]
    fn the_index_answers_what_the_recent_folders_have_let_go_and_both_are_kept_true() {
        let mut both: Remembering<Item> = Remembering::default();
        both.index(
            key("/DCIM/Camera"),
            1,
            2,
            vec![object(1, "a"), object(2, "b")],
        );
        // Twelve folders read for a pane push nothing out of the index.
        for i in 0..20 {
            both.keep(key(&format!("/F{i}")), 1, 0, Vec::new());
        }

        assert!(both.recall(&key("/DCIM/Camera"), 1, 2).is_some());
        both.amend(&key("/DCIM/Camera"), |kept| {
            kept.renamed(ObjectHandle(1), "aa")
        });
        assert_eq!(
            both.recall(&key("/DCIM/Camera"), 1, 2).unwrap()[0].name,
            "aa",
            "a change the app makes reaches the index"
        );
        // A pane reading the folder again brings the index up to date.
        both.keep(key("/DCIM/Camera"), 1, 1, vec![object(9, "only")]);
        assert_eq!(
            both.index.recall(&key("/DCIM/Camera"), 1, 1).unwrap().len(),
            1
        );
        // A folder only a pane has read does not go into the index.
        assert!(!both.index.holds(&key("/F3")));
        both.forget_below("serial", 1, "/DCIM");
        assert!(both.recall(&key("/DCIM/Camera"), 1, 1).is_none());
        both.forget(&key("/F19"));
        assert!(both.recall(&key("/F19"), 1, 0).is_none());
    }

    #[test]
    fn a_folder_is_recalled_while_the_phone_still_reports_as_many_items() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 2, vec![object(1, "a"), object(2, "b")]);

        assert_eq!(
            names(&memory, "/DCIM", 2),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(names(&memory, "/DCIM", 3), None, "a file was added");
        assert_eq!(names(&memory, "/DCIM", 1), None, "a file was removed");
        assert_eq!(names(&memory, "/Music", 2), None, "another folder");
    }

    #[test]
    fn a_new_connection_does_not_trust_the_old_reading() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 1, vec![object(1, "a")]);

        assert!(memory.recall(&key("/DCIM"), 1, 1).is_some());
        assert!(memory.recall(&key("/DCIM"), 2, 1).is_none());
    }

    #[test]
    fn items_the_phone_would_not_describe_still_count_towards_the_total() {
        let mut memory: Memory<Item> = Memory::default();
        // 3 handles, one refused: 2 children, total 3.
        memory.keep(key("/DCIM"), 1, 3, vec![object(1, "a"), object(2, "b")]);

        assert!(memory.recall(&key("/DCIM"), 1, 3).is_some());
        assert!(memory.recall(&key("/DCIM"), 1, 2).is_none());
    }

    #[test]
    fn keeping_a_folder_again_replaces_it() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 1, vec![object(1, "old")]);
        memory.keep(key("/DCIM"), 1, 1, vec![object(1, "new")]);

        assert_eq!(names(&memory, "/DCIM", 1), Some(vec!["new".into()]));
        assert_eq!(memory.order.len(), 1);
    }

    #[test]
    fn only_the_most_recent_folders_are_kept() {
        let mut memory: Memory<Item> = Memory::default();
        for i in 0..(KEPT + 3) {
            memory.keep(key(&format!("/f{i}")), 1, 0, vec![]);
        }

        assert_eq!(memory.folders.len(), KEPT);
        assert!(names(&memory, "/f0", 0).is_none(), "the oldest went");
        assert!(names(&memory, &format!("/f{}", KEPT + 2), 0).is_some());
    }

    #[test]
    fn keeping_a_folder_again_makes_it_the_newest() {
        let mut memory: Memory<Item> = Memory::default();
        for i in 0..KEPT {
            memory.keep(key(&format!("/f{i}")), 1, 0, vec![]);
        }
        memory.keep(key("/f0"), 1, 0, vec![]);
        memory.keep(key("/new"), 1, 0, vec![]);

        assert!(
            names(&memory, "/f0", 0).is_some(),
            "f0 was touched, f1 went instead"
        );
        assert!(names(&memory, "/f1", 0).is_none());
    }

    #[test]
    fn what_was_done_to_a_folder_can_be_applied_to_its_reading() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(
            key("/DCIM"),
            1,
            3,
            vec![object(1, "a"), object(2, "b"), object(3, "c")],
        );

        memory.amend(&key("/DCIM"), |kept| kept.renamed(ObjectHandle(2), "bee"));
        assert_eq!(
            names(&memory, "/DCIM", 3),
            Some(vec!["a".into(), "bee".into(), "c".into()])
        );

        memory.amend(&key("/DCIM"), |kept| {
            kept.removed(&[ObjectHandle(1), ObjectHandle(3)])
        });
        assert_eq!(
            names(&memory, "/DCIM", 1),
            Some(vec!["bee".into()]),
            "total follows"
        );

        memory.amend(&key("/DCIM"), |kept| kept.added(object(9, "new")));
        assert_eq!(
            names(&memory, "/DCIM", 2),
            Some(vec!["bee".into(), "new".into()])
        );
    }

    #[test]
    fn removing_something_that_was_not_there_changes_nothing() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 1, vec![object(1, "a")]);

        memory.amend(&key("/DCIM"), |kept| kept.removed(&[ObjectHandle(99)]));

        assert_eq!(names(&memory, "/DCIM", 1), Some(vec!["a".into()]));
    }

    #[test]
    fn amending_a_folder_that_is_not_kept_does_nothing() {
        let mut memory: Memory<Item> = Memory::default();

        memory.amend(&key("/nowhere"), |kept| kept.added(object(1, "a")));

        assert!(memory.folders.is_empty());
    }

    #[test]
    fn a_folder_can_be_forgotten() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 0, vec![]);
        memory.keep(key("/Music"), 1, 0, vec![]);

        memory.forget(&key("/DCIM"));
        memory.forget(&key("/never-kept"));

        assert!(names(&memory, "/DCIM", 0).is_none());
        assert!(names(&memory, "/Music", 0).is_some());
        assert_eq!(memory.order.len(), 1);
    }

    #[test]
    fn forgetting_below_a_folder_takes_everything_under_it() {
        let mut memory: Memory<Item> = Memory::default();
        for path in [
            "/DCIM",
            "/DCIM/Camera",
            "/DCIM/Camera/2026",
            "/DCIM2",
            "/Music",
        ] {
            memory.keep(key(path), 1, 0, vec![]);
        }

        memory.forget_below("serial", 1, "/DCIM");

        assert!(names(&memory, "/DCIM", 0).is_none());
        assert!(names(&memory, "/DCIM/Camera", 0).is_none());
        assert!(names(&memory, "/DCIM/Camera/2026", 0).is_none());
        assert!(
            names(&memory, "/DCIM2", 0).is_some(),
            "a different folder with a similar name"
        );
        assert!(names(&memory, "/Music", 0).is_some());
        assert_eq!(memory.order.len(), 2);
    }

    #[test]
    fn forgetting_below_the_root_takes_the_whole_storage_only() {
        let mut memory: Memory<Item> = Memory::default();
        memory.keep(key("/DCIM"), 1, 0, vec![]);
        memory.keep(("serial".into(), 2, "/SD".into()), 1, 0, vec![]);
        memory.keep(("other".into(), 1, "/DCIM".into()), 1, 0, vec![]);

        memory.forget_below("serial", 1, "/");

        assert!(names(&memory, "/DCIM", 0).is_none());
        assert!(
            memory
                .recall(&("serial".into(), 2, "/SD".into()), 1, 0)
                .is_some(),
            "another storage"
        );
        assert!(
            memory
                .recall(&("other".into(), 1, "/DCIM".into()), 1, 0)
                .is_some(),
            "another phone"
        );
    }
}
