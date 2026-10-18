//! Filesystem watcher for detecting out-of-band changes to backing directories.
//!
//! When files are written directly to a virtual device's backing directory (bypassing
//! MTP protocol operations), this watcher detects the changes and queues the
//! corresponding MTP events (`ObjectAdded`, `ObjectRemoved`).
//!
//! ## Dedup strategy
//!
//! MTP protocol handlers (upload, delete, move, etc.) modify the filesystem while
//! holding the `state` mutex and insert object handles into `state.objects` before
//! releasing the lock. The watcher callback also acquires `state` before processing
//! events. This mutex serialization guarantees that by the time the watcher sees a
//! filesystem notification for an MTP-initiated change, the state already reflects it:
//!
//! - **Creates**: the handle already exists in `state.objects` → watcher skips the event.
//! - **Removes**: the handle has already been removed from `state.objects` → watcher
//!   finds no handle for the path → nothing to emit.
//!
//! No TTL, no extra tracking structure: the state itself is the dedup mechanism.
//!
//! ## Pause/resume
//!
//! The watcher can be paused via [`pause_watcher`](super::registry::pause_watcher),
//! which returns a [`WatcherGuard`](super::registry::WatcherGuard) that resumes
//! on drop. While paused, all filesystem events are silently dropped. This is
//! needed when external code deletes and recreates files in the backing directory:
//! without pausing, the watcher may process stale deletion events after a rescan
//! has already added the new objects, incorrectly removing them.

use super::builders::build_event;
use super::state::VirtualDeviceState;
use crate::ptp::{EventCode, ObjectHandle, StorageId};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Mutex;

/// Mapping from backing dir to its storage ID, used by the watcher callback.
type StorageMap = Vec<(PathBuf, StorageId)>;

/// Start a filesystem watcher for all backing directories.
///
/// Returns the watcher (must be kept alive) or `None` if starting fails.
/// The watcher pushes events into `state.event_queue` via the shared mutex.
pub(super) fn start_fs_watcher(
    state: &std::sync::Arc<Mutex<VirtualDeviceState>>,
) -> Option<RecommendedWatcher> {
    let state_clone = std::sync::Arc::clone(state);

    // Build a map of backing_dir → storage_id for resolving events.
    let storage_map: StorageMap = {
        let s = state.lock().unwrap();
        s.storages
            .iter()
            .map(|ss| {
                let dir = ss
                    .config
                    .backing_dir
                    .canonicalize()
                    .unwrap_or_else(|_| ss.config.backing_dir.clone());
                (dir, ss.storage_id)
            })
            .collect()
    };

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let event = match res {
                Ok(e) => e,
                Err(_) => return,
            };

            handle_notify_event(&state_clone, &storage_map, event);
        },
        Config::default(),
    )
    .ok()?;

    // Watch all backing directories recursively.
    let state_lock = state.lock().unwrap();
    for storage in &state_lock.storages {
        let _ = watcher.watch(&storage.config.backing_dir, RecursiveMode::Recursive);
    }
    drop(state_lock);

    Some(watcher)
}

/// Process a single notify event and queue MTP events if appropriate.
fn handle_notify_event(
    state: &std::sync::Arc<Mutex<VirtualDeviceState>>,
    storage_map: &StorageMap,
    event: notify::Event,
) {
    // Only handle create, remove, and rename events.
    let is_create = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To
            ))
    );
    let is_remove = matches!(
        event.kind,
        EventKind::Remove(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From
            ))
    );
    // macOS FSEvents can't distinguish rename source from target, so it reports
    // RenameMode::Any. We determine the direction by checking if the path exists.
    let is_rename_any = matches!(
        event.kind,
        EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Any
        ))
    );

    if !is_create && !is_remove && !is_rename_any {
        return;
    }

    for path in &event.paths {
        // Canonicalize for reliable comparison (ignore errors for removed paths).
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

        // For RenameMode::Any, determine direction from whether the path exists.
        let (is_create, _is_remove) = if is_rename_any {
            if path.exists() {
                (true, false)
            } else {
                (false, true)
            }
        } else {
            (is_create, is_remove)
        };

        // Find which storage this path belongs to.
        let (storage_id, backing_dir) = match find_storage_for_path(&canonical, storage_map) {
            Some(v) => v,
            None => continue,
        };

        // Compute relative path within the storage.
        let rel_path = match canonical.strip_prefix(backing_dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };

        // Skip events for the backing directory itself (empty rel_path).
        // macOS FSEvents reports the watched directory as "created" on startup.
        if rel_path.as_os_str().is_empty() {
            continue;
        }

        let mut state = state.lock().unwrap();

        if state.pause_count > 0 {
            // Record the dropped path so test harnesses can wait for a sentinel
            // file's event to confirm the queue has drained past their writes.
            // Bound the ring at DROPPED_PATHS_CAP so memory stays scoped.
            state.dropped_paths.push_back(canonical.clone());
            if state.dropped_paths.len() > super::state::DROPPED_PATHS_CAP {
                state.dropped_paths.pop_front();
            }
            continue;
        }

        if is_create {
            // Check if a handle already exists for this path. If so, the MTP handler
            // already created it and emitted events, so skip to avoid duplicates.
            let already_known = state
                .objects
                .iter()
                .any(|(_, obj)| obj.storage_id == storage_id && obj.rel_path == rel_path);

            if already_known {
                continue;
            }

            // Determine the parent handle.
            let parent =
                if let Some(parent_rel) = rel_path.parent() {
                    if parent_rel == std::path::Path::new("") {
                        ObjectHandle::ROOT
                    } else {
                        // Look up the parent object.
                        match state.objects.iter().find(|(_, obj)| {
                            obj.storage_id == storage_id && obj.rel_path == parent_rel
                        }) {
                            Some((&h, _)) => ObjectHandle(h),
                            None => ObjectHandle::ROOT,
                        }
                    }
                } else {
                    ObjectHandle::ROOT
                };

            let handle = state.alloc_handle();
            state.objects.insert(
                handle.0,
                super::state::VirtualObject {
                    rel_path,
                    storage_id,
                    parent,
                },
            );

            state
                .event_queue
                .push_back(build_event(EventCode::ObjectAdded, &[handle.0]));
            state
                .event_queue
                .push_back(build_event(EventCode::StorageInfoChanged, &[storage_id.0]));
        } else {
            // Remove: find the handle for this path and remove it.
            // If the handle is already gone, the MTP handler already processed
            // the removal, so skip to avoid duplicates.
            let handle = state
                .objects
                .iter()
                .find(|(_, obj)| obj.storage_id == storage_id && obj.rel_path == rel_path)
                .map(|(&h, _)| h);

            if let Some(h) = handle {
                state.objects.remove(&h);
                state
                    .event_queue
                    .push_back(build_event(EventCode::ObjectRemoved, &[h]));
                state
                    .event_queue
                    .push_back(build_event(EventCode::StorageInfoChanged, &[storage_id.0]));
            }
        }
    }
}

/// Find which storage a path belongs to by checking if it's under a backing dir.
fn find_storage_for_path<'a>(
    path: &std::path::Path,
    storage_map: &'a StorageMap,
) -> Option<(StorageId, &'a PathBuf)> {
    storage_map
        .iter()
        .find(|(dir, _)| path.starts_with(dir))
        .map(|(dir, sid)| (*sid, dir))
}
