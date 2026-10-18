//! How full each mounted disk is, as macOS last knew it. `getfsstat` with
//! `MNT_NOWAIT` hands back what the kernel already holds for every mount
//! without asking any of them, so a network disk that has stopped answering
//! can't hold the sidebar up the way a `statfs` of it would.

use crate::dto::DiskSpace;

/// A mounted file system: where it is mounted, and its space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub path: String,
    pub space: DiskSpace,
}

/// Every mount, from what the kernel already knows; empty if it can't say.
pub fn mounted() -> Vec<Mount> {
    // SAFETY: a null buffer asks only how many mounts there are.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Room for a few more, in case one is mounted between the two calls.
    let room = count + 4;
    // SAFETY: `statfs` is plain data; all zeros is a valid value of it.
    let mut stats: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; room];
    let size = i32::try_from(room * std::mem::size_of::<libc::statfs>()).unwrap_or(i32::MAX);
    // SAFETY: `stats` holds `room` entries, `size` bytes, which the call fills
    // in up to that size and says how many it filled.
    let filled = unsafe { libc::getfsstat(stats.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    let Ok(filled) = usize::try_from(filled) else {
        return Vec::new();
    };
    stats
        .iter()
        .take(filled.min(room))
        .map(|stat| {
            // SAFETY: `f_mntonname` is a NUL-terminated array the call filled in.
            let path = unsafe { std::ffi::CStr::from_ptr(stat.f_mntonname.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let block = u64::from(stat.f_bsize);
            Mount {
                path,
                space: DiskSpace {
                    free: stat.f_bavail.saturating_mul(block),
                    capacity: stat.f_blocks.saturating_mul(block),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_up_disk_is_mounted_with_some_room_known() {
        let mounts = mounted();
        let root = mounts
            .iter()
            .find(|mount| mount.path == "/")
            .expect("/ is mounted");

        assert!(root.space.capacity > 0);
        assert!(root.space.free <= root.space.capacity);
        // The same as asking the disk itself (to within what was written
        // between the two looks).
        let mut asked: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: a valid C string, and a zeroed `statfs` the call fills in.
        assert_eq!(unsafe { libc::statfs(c"/".as_ptr(), &mut asked) }, 0);
        let block = u64::from(asked.f_bsize);
        let (free, capacity) = (asked.f_bavail * block, asked.f_blocks * block);
        assert_eq!(root.space.capacity, capacity);
        assert!(
            root.space.free.abs_diff(free) < capacity / 100,
            "{} against {free}",
            root.space.free
        );
    }
}
