# Video Thumbnails Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A phone video with no phone-made thumbnail shows its first frame in the grid tile, fetched from a few MB of the file, never the whole video.

**Architecture:** A pure MP4 box walker plans which byte ranges are needed; the service reads them with partial reads, writes them at their original offsets into a sparse temp file, and AVFoundation (through `objc2`) returns the first frame as RGBA. The existing disk cache keeps the result.

**Tech Stack:** Rust 2024, `mtp-rs` (vendored), `objc2` 0.6 + `objc2-av-foundation`/`objc2-core-media`/`objc2-core-graphics` 0.3 (macOS only), tokio, `tempfile`, a Swift script for fixtures (dev only).

**Spec:** `docs/superpowers/specs/2026-11-26-video-thumbnails-design.md`

## Global Constraints

- Handlers never import `mtp_rs`; services never import `iced`. All new code is in `internal/service/thumbnail/`; only `dto::Thumbnail` crosses out (no UI change).
- `make lint` (fmt + clippy `-D warnings`, `clippy.toml` bans bare `Task::perform`), `make test`, and coverage >= 96% lines must pass; CI runs on macOS.
- Every behaviour ships with its test in the same commit. Test the phone's bytes as untrusted: no panic, no unbounded allocation, no unbounded loop.
- Count round trips, don't time them: use `device.activity().times_heard()` and bound it. A stopwatch is only for the AVFoundation deadline test (give it 10x headroom).
- Never block the UI thread; the phone has one session: at most one video at a time, reads in 1 MiB requests through `device.ask`.
- No `Co-Authored-By` trailers. Version is NOT bumped in this plan (release is a separate step the user asks for).
- New dependencies: macOS-only, licenses `Zlib OR Apache-2.0 OR MIT`; CI's dependency review and `cargo audit` must stay green.
- Constants (exact): `MAX_TOP_LEVEL_BOXES = 16`, `HEADER_PEEK = 16`, `HEAD_TIERS = [2 MiB, 8 MiB, 32 MiB]`, `MAX_MOOV = 24 MiB`, `READ_CHUNK = 1 MiB`, `MAX_VIDEO_BYTES = 1 TiB`, AVFoundation deadline 5 s, `VIDEOS_AT_ONCE = 1`.

## Review Focus

Failure modes the spec implies that no single task's happy path covers; each has its test in the named task.

1. A portrait phone video (stored rotated, rotation in the track matrix) comes out upright (Task 4).
2. The phone lies: claimed size bigger or smaller than the real file, box sizes past the end, `moov` of 2 GB, 4 GB+ file (offsets past `u32`) (Tasks 2, 5).
3. The phone is unplugged or goes silent half way: `Err`, not `None`, the session dropped once, no temp folder left (Task 5).
4. A folder of many videos: one at a time, none re-read once cached, no leaked temp folders or threads, a hanging AVFoundation call ends at the deadline (Tasks 4, 5).
5. Nothing changes where it already works: a phone with its own thumbnail is not asked for more; a phone without partial reads gets no extra request; `mkv`/`webm`/non-video files behave as before (Task 5).

## File Structure

- Create `internal/service/thumbnail/mp4.rs`: pure box walker and read plan.
- Create `internal/service/thumbnail/sparse.rs`: sparse file writer.
- Create `internal/service/thumbnail/video.rs`: `first_frame` (AVFoundation, macOS) and `with_deadline`.
- Modify `internal/service/thumbnail/{mod.rs,mapper.rs,service.rs}`, `Cargo.toml`, `internal/Cargo.toml`.
- Create `internal/tests/fixtures/video/*.mp4` and `scripts/make_video_fixtures.swift`.
- Modify `CLAUDE.md` (Thumbnails section, remove the Quick Look line if Task 6 lands), `README.md` (feature table).

---

### Task 1: Video fixtures

**Files:**
- Create: `scripts/make_video_fixtures.swift`
- Create: `internal/tests/fixtures/video/moov_at_end.mp4`, `faststart.mp4`, `portrait.mp4`
- Create: `internal/service/thumbnail/fixtures.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `fixtures::bytes(name: &str) -> Vec<u8>` (reads `internal/tests/fixtures/video/<name>`), and the facts tests rely on: each clip is 2 s, 30 fps, H.264, <= 60 KB; frame 0 has a red top-left quadrant on a blue background as it should be SEEN; `moov_at_end` and `faststart` are 160 wide by 96 high; `portrait` is stored as 160x96 pixels with a 90 degree track matrix, so it is SEEN as 96 wide by 160 high with the red quadrant still top-left.

- [ ] **Step 1:** Write the Swift script (AVAssetWriter, `shouldOptimizeForNetworkUse` false for two files and true for `faststart`; `AVAssetWriterInput.transform` set for `portrait`). It prints each file's top-level boxes.
- [ ] **Step 2:** Run `swift scripts/make_video_fixtures.swift internal/tests/fixtures/video`. Expected: three files; box lists end `moov` for `moov_at_end`/`portrait` and have `moov` before `mdat` for `faststart`. Verify sizes <= 60 KB each.
- [ ] **Step 3:** Add `fixtures.rs` with `bytes()` and a test `fixtures_have_the_layouts_the_tests_assume` that scans top-level boxes with a 10-line local helper (not `mp4.rs`, which does not exist yet) and asserts the box order above.
- [ ] **Step 4:** `cargo test -p open-mtp-client-internal fixtures` passes.
- [ ] **Step 5:** Commit `Add small video fixtures and the script that makes them`.

### Task 2: MP4 box walker and read plan (pure)

**Files:**
- Create: `internal/service/thumbnail/mp4.rs`; Modify: `internal/service/thumbnail/mod.rs` (`mod mp4;`)
- Test: in `mp4.rs`

**Interfaces:**
- Produces (exact):
  ```rust
  pub const MAX_TOP_LEVEL_BOXES: usize = 16;
  pub const HEADER_PEEK: u32 = 16;
  pub const MAX_MOOV: u64 = 24 << 20;
  pub const HEAD_TIERS: [u64; 3] = [2 << 20, 8 << 20, 32 << 20];
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Mp4Error { NotMp4, BoxOutOfBounds, TooManyBoxes, NoMoov, NoMdat, MoovTooBig }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Header { pub kind: [u8; 4], pub header_len: u8, pub len: Option<u64> } // None: to the end of the file
  pub fn parse_header(bytes: &[u8]) -> Result<Header, Mp4Error>;
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Layout { pub moov: std::ops::Range<u64>, pub mdat_data: u64 }
  pub struct Walker { /* private */ }
  impl Walker {
      pub fn new(file_len: u64) -> Self;
      /// Where to read the next `HEADER_PEEK` bytes (fewer at the end of the file).
      pub fn next_offset(&self) -> u64;
      /// Feeds what was read at `next_offset`; `Ok(Some(layout))` once `moov` and `mdat` are both found.
      pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Layout>, Mp4Error>;
  }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ReadPlan { pub head: std::ops::Range<u64>, pub moov: Option<std::ops::Range<u64>> } // moov None: inside head
  pub fn read_plan(layout: &Layout, file_len: u64, tier: usize) -> Option<ReadPlan>;
  ```
- `read_plan`: `head` starts at 0 and ends at `mdat_data + HEAD_TIERS[tier]` clamped to `file_len`; when `moov` ends at or before the head's end (moov-first files) `moov` is `None` and `head` is extended to cover it; `None` when `tier` is out of range.

- [ ] **Step 1: Write the failing tests** (names and assertions):
  - `parse_header_reads_a_32_bit_size_and_the_kind` (size 28 `ftyp` -> `len: Some(28)`, `header_len: 8`)
  - `parse_header_reads_a_64_bit_size` (size field 1 + 8-byte largesize -> `header_len: 16`)
  - `a_size_of_zero_means_to_the_end_of_the_file` (-> `len: None`)
  - `a_size_below_its_own_header_is_refused` (sizes 2, 7 and `1` with a 64-bit size below 16 -> `Err(NotMp4)`); `fewer_than_eight_bytes_is_refused`
  - `moov_at_the_end_is_found` (ftyp, free, mdat, moov built by a test helper `boxes(&[(kind, payload_len)])`): layout `moov` range and `mdat_data` exact
  - `moov_before_mdat_is_found`; `a_64_bit_mdat_is_walked_over`; `offsets_past_4_gb_stay_u64` (file_len 5 GiB, a 64-bit `mdat` of 5 GiB - 4 KiB then `moov`: `Layout.moov.start > u32::MAX`, `read_plan` ranges equal the exact `u64` values); `a_to_the_end_mdat_means_no_moov_after_it` -> `Err(NoMoov)`
  - `a_box_that_runs_past_the_end_of_the_file_is_refused` (`BoxOutOfBounds`); `a_box_size_near_u64_max_does_not_overflow`
  - `more_than_sixteen_boxes_without_both_is_refused` (`TooManyBoxes`); `no_mdat_is_refused`; `a_moov_over_the_cap_is_refused` (`MoovTooBig`)
  - `a_zero_length_box_after_eight_bytes_still_advances` (size 8 empty boxes terminate by the box cap, never loop)
  - `read_plan_for_a_moov_last_file_reads_the_head_and_the_moov_separately` (tier 0: `head == 0..mdat_data+2MiB`, `moov == Some(..)`)
  - `read_plan_for_a_faststart_file_is_one_range` (`moov: None`, head covers moov and 2 MiB of data)
  - `read_plan_clamps_to_a_small_file`, `read_plan_tiers_grow_and_the_last_is_32_mib`, `read_plan_past_the_last_tier_is_none`
  - property, seeded PRNG, 5,000 rounds: `random_bytes_never_panic_and_always_end` (feeding random 16-byte windows: result is `Ok`/`Err` within `MAX_TOP_LEVEL_BOXES` feeds; every `next_offset` <= `file_len`), and `a_random_box_sequence_is_found_where_it_was_put` (build 1-12 random boxes with a random `moov`/`mdat` position, walk it with a reader over the built bytes, expect the exact ranges)
- [ ] **Step 2:** `cargo test -p open-mtp-client-internal mp4` -> FAIL (module missing).
- [ ] **Step 3:** Implement the signatures above. Checked arithmetic only (`checked_add`); `Walker::feed` records `moov`/`mdat` and advances `next_offset` by the box length; a `len: None` box ends the walk (everything after it belongs to it).
- [ ] **Step 4:** Same command -> PASS. Also `cargo clippy -p open-mtp-client-internal --all-targets -- -D warnings`.
- [ ] **Step 5:** Commit `Add a pure MP4 box walker and the read plan for a video's first frame`.

### Task 3: Sparse file writer

**Files:**
- Create: `internal/service/thumbnail/sparse.rs`; Modify: `mod.rs`
- Test: in `sparse.rs`

**Interfaces:**
- Produces: `pub struct Piece<'a> { pub offset: u64, pub bytes: &'a [u8] }` and `pub fn write_sparse(path: &std::path::Path, len: u64, pieces: &[Piece<'_>]) -> std::io::Result<()>` (creates the file, `set_len(len)`, writes each piece; a piece past `len` or overflowing is `InvalidInput`; an existing file is `AlreadyExists`).

- [ ] **Step 1: Failing tests:** `pieces_land_at_their_offsets` (read back with seek); `the_middle_stays_zero`; `a_piece_past_the_end_is_invalid_input`; `a_piece_whose_end_overflows_u64_is_invalid_input`; `an_existing_file_is_not_overwritten`; `a_big_file_with_small_pieces_takes_little_disk` (unix: 64 MiB file, `metadata().blocks() * 512 < 4 MiB`); `a_zero_length_file_with_no_pieces_is_fine`.
- [ ] **Step 2:** Run -> FAIL. **Step 3:** Implement with `OpenOptions::new().write(true).create_new(true)`. **Step 4:** PASS. **Step 5:** Commit `Add a sparse file writer for the pieces of a video`.

### Task 4: AVFoundation first frame, and the dependency spike

**Files:**
- Modify: `Cargo.toml` (workspace deps), `internal/Cargo.toml` (macOS target deps)
- Create: `internal/service/thumbnail/video.rs`; Modify: `mod.rs`
- Test: in `video.rs`

**Interfaces:**
- Consumes: `fixtures::bytes`, `sparse::write_sparse`, `mp4`.
- Produces:
  ```rust
  /// The first frame of the video at `path`, at most THUMBNAIL_SIZE a side, upright. `None` when it can't be made or `timeout` passes.
  pub fn first_frame(path: &std::path::Path, timeout: std::time::Duration) -> Option<crate::dto::Thumbnail>;
  /// Runs `work` on its own thread; `None` when it hasn't answered within `timeout` (the thread is left to finish and its result dropped).
  pub fn with_deadline<T: Send + 'static>(timeout: std::time::Duration, work: impl FnOnce() -> T + Send + 'static) -> Option<T>;
  ```
  On non-macOS `first_frame` is `None` (the crate builds there).

- [ ] **Step 1: Spike first, and push it.** Add the dependencies (`objc2 = "0.6"` under another name such as `objc2_06`, `objc2-av-foundation`, `objc2-core-media`, `objc2-core-graphics`, `objc2-core-foundation`, `block2 = "0.6"`, minimal features), write the smallest `first_frame` that decodes `moov_at_end.mp4`, push the branch and watch CI. Expected: builds on macOS CI and a trivial test passes there (AVFoundation works without a window session). **If it does not**, stop and use the fallback in the spec (`msg_send!` on 0.5); report to the user before continuing.
- [ ] **Step 2: Failing tests** (macOS only, `#[cfg(target_os = "macos")]`):
  - `the_first_frame_of_a_moov_last_video_is_made` (dims <= 192, aspect 160:96 within 1 px, red in the top-left quadrant and blue elsewhere, sampled at quadrant centres)
  - `a_faststart_video_gives_the_same_picture` (same assertions)
  - `a_portrait_video_comes_out_upright` (dims h > w, 96:160 within 1 px, red top-left)
  - `a_video_with_only_its_start_and_its_index_is_enough` (sparse file from `moov_at_end` with head = `read_plan` tier 0 and the moov range: picture equal to the full file's, `==` on the RGBA)
  - `a_video_without_its_start_has_no_frame` (sparse with only the moov: `None`, within 2 s)
  - `garbage_and_empty_files_have_no_frame` (0 bytes, 1 KB random, a PNG renamed `.mp4`): all `None`, none panics
  - `a_missing_file_has_no_frame`
  - `with_deadline_returns_the_answer_in_time` / `with_deadline_gives_up_on_a_slow_worker` (worker sleeps 2 s, timeout 100 ms: `None` in < 1 s)
  - `two_hundred_frames_in_a_row_all_succeed` (200 calls on the fixture, each `Some`; guards a leak that would fail a later call, and the whole test must finish in under 60 s, 10x its measured time)
- [ ] **Step 3: Implement** `first_frame` (`AVURLAsset` from a file URL; `AVAssetImageGenerator` with `appliesPreferredTrackTransform = true`, `maximumSize = 192x192`, tolerances zero; `copyCGImageAtTime` at zero inside `with_deadline`; draw the `CGImage` into an RGBA8 premultiplied-last context of its own size; return `Thumbnail`) and `with_deadline` (`std::thread::spawn` + `mpsc::channel` + `recv_timeout`).
- [ ] **Step 4:** PASS locally and on CI. **Step 5:** Commit `Make a video's first frame with AVFoundation`.

### Task 5: Phone videos in `Service::phone`

**Files:**
- Modify: `internal/service/thumbnail/mapper.rs` (`pub fn is_phone_video(name: &str) -> bool`: `mp4 m4v mov 3gp`), `service.rs`
- Test: `service.rs` tests (virtual phone; the virtual device has no `GetThumb`, which is the case being built)

**Interfaces:**
- Consumes: `mp4::{Walker, read_plan, HEAD_TIERS, MAX_MOOV, HEADER_PEEK}`, `sparse::write_sparse`, `video::first_frame`.
- Produces (private): `pub(super) enum Fallback { Video, Still, Nothing }` and `pub(super) fn fallback_for(name: &str, size_bytes: u64, partial_reads: bool) -> Fallback` (the existing 4 MB still-image rule moves into it unchanged); `async fn video_from_phone(&self, session: &mtp_rs::MtpDevice, epoch: u64, storage: &Storage, handle: ObjectHandle, entry: &Entry) -> Result<Option<Thumbnail>, Error>` called from `ask_phone` after the `GetThumb` attempt and before the 4 MB still-image fallback when `mapper::is_phone_video(name)`; `static VIDEOS: Semaphore` with `VIDEOS_AT_ONCE`; `async fn read_in_chunks(...)` reading a range as 1 MiB `read_range` requests through `self.device.ask`.
- Rules: `None` without any request when `!session.capabilities().supports_partial_download`, when `size_bytes == 0` or `> MAX_VIDEO_BYTES`. Box walk by `HEADER_PEEK` reads. A short read (phone has fewer bytes than it claimed) is `None`. Temp folder `std::env::temp_dir()/open-mtp-client-vt-<pid>-<n>` created `0700`, removed on every path (use a guard struct with `Drop`). Tier loop: try tier 0, on `None` frame try tier 1 then 2, re-reading only the head, never the moov twice.

- [ ] **Step 1: Failing tests** (helper `video_phone(files: &[(&str, Vec<u8>)])` builds a virtual phone dir from fixture bytes):
  - `a_moov_last_video_gets_its_first_frame_from_a_few_megabytes` (assert `times_heard()` delta <= 12 and bytes read < 2.5 MiB via the fake's read counter or `full_reads() == 0`)
  - `a_faststart_video_gets_its_first_frame`; `a_portrait_video_is_upright`
  - `the_phones_own_thumbnail_comes_first`: the virtual phone never has one (`handle_get_thumb` answers `NO_THUMBNAIL`), so this is structural: `ask_phone` returns as soon as the phone's own thumbnail decodes, and the new path is a separate `fallback_for` step reached only after it. Test the decision as a table over the pure `fn fallback_for(name: &str, size_bytes: u64, partial_reads: bool) -> Fallback` with `enum Fallback { Video, Still, Nothing }`: `("a.mp4", 5 MB, true) -> Video`, `("a.mp4", 5 MB, false) -> Nothing`, `("a.mp4", 0, true) -> Nothing`, `("a.mp4", 2 TiB, true) -> Nothing`, `("a.mkv", 5 MB, true) -> Nothing`, `("a.jpg", 1 MB, true) -> Still`, `("a.jpg", 9 MB, true) -> Nothing`, `("notes.txt", 10, true) -> Nothing`. A real phone's own thumbnail is on the real-phone checklist (Task 8).
  - `a_second_look_at_a_cached_video_asks_the_phone_nothing` (disk cache, `times_heard` delta 0)
  - `a_phone_that_cannot_read_part_of_a_file_gets_no_video_frame_and_no_request`
  - `mkv_webm_and_non_video_files_are_as_before` (`Ok(None)`, no extra requests)
  - `a_video_claiming_more_than_a_terabyte_is_refused_untried`; `a_video_whose_boxes_lie_gets_none` (fixture bytes with the `moov` size patched to 2 GB, to past the end, and `mdat` size 0 first); `a_file_shorter_than_the_phone_claimed_gets_none`
  - `a_head_that_is_too_small_is_read_again_larger` (fixture with its first frame placed after 3 MiB of padding in `mdat`: tier 0 fails, tier 1 succeeds; assert two head reads, one moov read)
  - `a_phone_unplugged_halfway_is_an_error_and_leaves_no_temp_folder` (existing silent/lost-phone pattern; count `open-mtp-client-vt-*` before/after)
  - `thumbnails_from_a_silent_phone_end_as_a_timeout_for_videos_too`
  - `videos_are_made_one_at_a_time` (spawn 8 `phone()` calls; an `AtomicUsize` high-water mark inside `video_from_phone` under `#[cfg(test)]` is 1)
  - `no_temp_folders_are_left_after_many_videos` (50 calls, count before/after)
- [ ] **Step 2:** Run -> FAIL. **Step 3:** Implement. **Step 4:** Run the module, then `cargo test --workspace`. **Step 5:** Commit `Show a phone video's first frame in its tile when the phone gives none`.

### Task 6 (optional): Mac videos through the same frame

**Files:** Modify `mapper.rs` (`is_quick_look` loses `mov mp4 m4v`; `is_av_video`), `service.rs::mac`.
- [ ] **Step 1: Failing tests:** `a_mac_video_gets_an_avfoundation_frame_not_quick_look` (fixture on disk -> `Some`, no `qlmanage` process started: use the existing `quick_look` seam's timeout test pattern), `a_mac_portrait_video_is_upright`, `a_damaged_mac_video_has_no_thumbnail_and_nothing_hangs` (< 2 s), `heic_dng_pdf_still_use_quick_look` (existing tests stay), `a_missing_mac_video_is_path_not_found`.
- [ ] **Step 2-4:** implement, run the module and the workspace. **Step 5:** Commit `Make Mac video thumbnails with AVFoundation instead of Quick Look`.

### Task 7: Docs

**Files:** Modify `CLAUDE.md` (the Thumbnails bullet: the video path, the caps, the tiers, `with_deadline`, the fixtures and the script; the "Mac thumbnails ... Quick Look" bullet if Task 6 landed), `README.md` (feature table: "Video thumbnails" 🚧 -> ✅).
- [ ] **Step 1:** Edit. **Step 2:** `grep -n "Video thumbnails" README.md` shows ✅ and CLAUDE.md names `video.rs`, `mp4.rs`, `sparse.rs`. **Step 3:** Commit `Document video thumbnails`.

### Task 8: Gate and handoff

- [ ] **Step 1:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Expected: all pass (record the test count).
- [ ] **Step 2:** Performance (the repo's standing rule): on the virtual phone with 300 videos in one folder in Grid view, scroll through it; record `times_heard()` per video and the time for the first screenful on a release build; put the numbers in the PR. Expected: <= 12 requests a video, 24 per batch, none for a video already cached.
- [ ] **Step 3:** Push the branch, open the PR with the repo template, wait for `gh pr checks` (including coverage >= 96%).
- [ ] **Step 4:** Tell the user what only a real phone can show, and ask for it before release: a recent Pixel/Samsung 4K HEVC clip (is the frame right, HDR colours), a portrait clip, a clip that opens on black, a 3 GB clip (speed), and the numbers in Step 2 on real USB. Do not claim it works on a phone until they have said so.
