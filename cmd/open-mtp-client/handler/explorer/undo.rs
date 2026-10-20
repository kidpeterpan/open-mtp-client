//! Undo (⌘Z) of a pane's last change, as Finder's: a rename, a move inside
//! the pane's volume, a new folder. Each is undone with the change's own
//! opposite through the pane's service, which refuses what is no longer as
//! the change left it (the renamed item gone, the original name taken, a
//! moved item moved again); a new folder goes only while it is still empty.
//! A delete can't be undone: on the phone it is for good, and the Mac's
//! Trash can't be read back without Full Disk Access.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use internal::dto;

use super::handler::Service;
use super::mapper;
use crate::i18n::key;

/// A change that can be undone, in the folder (or folders) it happened in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    /// `from` was renamed `to` in `folder`.
    Renamed {
        folder: String,
        from: String,
        to: String,
    },
    /// The folder `name` was made in `folder`.
    MadeFolder { folder: String, name: String },
    /// Several items in `folder` were renamed: each old name and the new.
    RenamedMany {
        folder: String,
        renames: Vec<(String, String)>,
    },
    /// Items were moved, one source folder after another.
    Moved(Vec<MovedStep>),
    /// The folder `name` was made in `folder` and items moved into it (New
    /// Folder with Selection): they go back, then it goes.
    Grouped {
        folder: String,
        name: String,
        moved: Vec<MovedStep>,
    },
}

/// Items moved from one folder into another: each item's name before, and
/// the name it arrived under (" (1)" when its own was taken).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedStep {
    pub from: String,
    pub into: String,
    pub items: Vec<(String, String)>,
}

impl Undo {
    pub fn banner(&self) -> &'static str {
        match self {
            Undo::Renamed { .. } | Undo::RenamedMany { .. } => key("explorer.undo.renamed"),
            Undo::MadeFolder { .. } | Undo::Grouped { .. } => key("explorer.undo.new_folder"),
            Undo::Moved(_) => key("explorer.undo.moved"),
        }
    }
}

/// Orders changes across both panes: ⌘Z undoes the latest, whichever pane.
static CHANGES: AtomicU64 = AtomicU64::new(0);

/// A fresh mark for a change just made.
pub fn next_mark() -> u64 {
    CHANGES.fetch_add(1, Ordering::Relaxed) + 1
}

/// The undo of a complete move: every request a move, every item of it
/// moved and named in the summary. Anything less (a copy, a failure part
/// way) offers no undo, since what to put back would be a guess.
pub fn of_paste(requests: &[(dto::PasteRequest, dto::PasteSummary)]) -> Option<Undo> {
    let mut steps = Vec::new();
    for (request, summary) in requests {
        let whole = request.mode == dto::Paste::Move
            && summary.mode == dto::Paste::Move
            && summary.failures.is_empty()
            && summary.names.len() == request.names.len();
        if !whole {
            return None;
        }
        steps.push(MovedStep {
            from: request.from.clone(),
            into: request.to.clone(),
            items: request
                .names
                .iter()
                .cloned()
                .zip(summary.names.iter().cloned())
                .collect(),
        });
    }
    (!steps.is_empty()).then_some(Undo::Moved(steps))
}

/// Mac clutter that doesn't make a folder "used".
fn is_clutter(name: &str) -> bool {
    name == ".DS_Store"
}

/// Undoes `undo` on `volume_id`; `Ok` holds the folder to show and what to
/// select there.
pub async fn undo(
    service: Arc<dyn Service>,
    volume_id: u64,
    change: Undo,
) -> Result<(String, Vec<String>), dto::Error> {
    match change {
        Undo::Renamed { folder, from, to } => {
            let name = service.rename(volume_id, &folder, &to, &from).await?;
            Ok((folder, vec![name]))
        }
        // Each back to its old name, the last renamed first; one that can't
        // go back (renamed again, its old name taken) leaves the rest done
        // and says which.
        Undo::RenamedMany { folder, renames } => {
            let mut back = Vec::new();
            let mut refused = None;
            for (old, new) in renames.into_iter().rev() {
                match service.rename(volume_id, &folder, &new, &old).await {
                    Ok(name) => back.push(name),
                    Err(
                        err @ (dto::Error::Disconnected
                        | dto::Error::NotConnected
                        | dto::Error::Timeout),
                    ) => return Err(err),
                    Err(_) => {
                        refused.get_or_insert(new);
                    }
                }
            }
            match refused {
                Some(name) if back.is_empty() => Err(dto::Error::ChangedSince(name)),
                Some(name) => Err(dto::Error::PartlyUndone(name)),
                None => Ok((folder, back)),
            }
        }
        Undo::Grouped {
            folder,
            name,
            moved,
        } => {
            let (_, back) =
                Box::pin(undo(Arc::clone(&service), volume_id, Undo::Moved(moved))).await?;
            Box::pin(undo(
                service,
                volume_id,
                Undo::MadeFolder {
                    folder: folder.clone(),
                    name,
                },
            ))
            .await?;
            Ok((folder, back))
        }
        Undo::MadeFolder { folder, name } => {
            let inside = service
                .list(volume_id, &mapper::child_path(&folder, &name), true)
                .await?;
            if inside.iter().any(|entry| !is_clutter(&entry.name)) {
                return Err(dto::Error::ChangedSince(name));
            }
            service
                .delete(volume_id, &folder, std::slice::from_ref(&name))
                .await?;
            Ok((folder, Vec::new()))
        }
        Undo::Moved(steps) => {
            let mut shown = (String::new(), Vec::new());
            for step in steps {
                let request = dto::PasteRequest {
                    mode: dto::Paste::Move,
                    from_storage: volume_id,
                    from: step.into.clone(),
                    names: step
                        .items
                        .iter()
                        .map(|(_, placed)| placed.clone())
                        .collect(),
                    to_storage: volume_id,
                    to: step.from.clone(),
                };
                let summary = service.paste(&request).await?;
                if let Some(failure) = summary.failures.first() {
                    return Err(dto::Error::ChangedSince(failure.label.clone()));
                }
                // Back in its folder, each item gets its own name again where
                // the move had to number it and the name is free.
                let mut back = Vec::new();
                for ((original, placed), now) in step.items.iter().zip(summary.names) {
                    let name = if original != placed && now != *original {
                        service
                            .rename(volume_id, &step.from, &now, original)
                            .await
                            .unwrap_or(now)
                    } else {
                        now
                    };
                    back.push(name);
                }
                shown = (step.from, back);
            }
            Ok(shown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mode: dto::Paste, names: &[&str]) -> dto::PasteRequest {
        dto::PasteRequest {
            mode,
            from_storage: 1,
            from: "/DCIM".into(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            to_storage: 1,
            to: "/Trip".into(),
        }
    }

    fn summary(mode: dto::Paste, names: &[&str], failed: &[&str]) -> dto::PasteSummary {
        dto::PasteSummary {
            mode,
            done: names.len(),
            renamed: 0,
            into: "/Trip".into(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            failures: failed
                .iter()
                .map(|name| dto::FileFailure {
                    label: (*name).to_owned(),
                    error: dto::Error::NameTaken((*name).to_owned()),
                })
                .collect(),
        }
    }

    #[test]
    fn a_whole_move_is_undone_name_for_name() {
        let moved = of_paste(&[(
            request(dto::Paste::Move, &["a.jpg", "b.jpg"]),
            summary(dto::Paste::Move, &["a.jpg", "b (1).jpg"], &[]),
        )]);

        assert_eq!(
            moved,
            Some(Undo::Moved(vec![MovedStep {
                from: "/DCIM".into(),
                into: "/Trip".into(),
                items: vec![
                    ("a.jpg".into(), "a.jpg".into()),
                    ("b.jpg".into(), "b (1).jpg".into())
                ],
            }]))
        );
        assert_eq!(moved.unwrap().banner(), "explorer.undo.moved");
    }

    #[test]
    fn a_copy_or_a_move_that_stopped_part_way_offers_no_undo() {
        let copy = (
            request(dto::Paste::Copy, &["a.jpg"]),
            summary(dto::Paste::Copy, &["a.jpg"], &[]),
        );
        let partly = (
            request(dto::Paste::Move, &["a.jpg", "b.jpg"]),
            summary(dto::Paste::Move, &["a.jpg"], &["b.jpg"]),
        );
        let whole = (
            request(dto::Paste::Move, &["c.jpg"]),
            summary(dto::Paste::Move, &["c.jpg"], &[]),
        );

        let short = (
            request(dto::Paste::Move, &["a.jpg", "b.jpg"]),
            summary(dto::Paste::Move, &["a.jpg"], &[]),
        );
        assert_eq!(
            of_paste(&[short]),
            None,
            "one item not named (already there): which went where is a guess"
        );
        assert_eq!(of_paste(std::slice::from_ref(&copy)), None);
        assert_eq!(of_paste(&[whole.clone(), partly]), None, "all or nothing");
        assert!(of_paste(&[whole]).is_some());
        assert_eq!(of_paste(&[]), None);
    }

    #[test]
    fn marks_only_go_up() {
        let first = next_mark();
        assert!(next_mark() > first);
    }
}
