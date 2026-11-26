# Video thumbnails for phone files: design

Status: proposed, 2026-11-26. README's feature table lists "Video thumbnails" as 🚧.

## What exists

- Phone files: `service::thumbnail::Service::phone` asks the phone for its own thumbnail (`GetThumb`) for `mp4 mov 3gp mkv webm` among others (`mapper::PHONE_MEDIA`). A phone that has none, or does not support `GetThumb`, gets nothing for a video: the fallback ("fetch at most 4 MB and shrink") only handles still images. The tile then shows the badge ("MP4 / Video").
- Mac files: `mp4 mov m4v` (and heic, dng, pdf) go through `qlmanage -t` with a 5 s timeout. `qlmanage` is slow to start and can hang (it hung for the whole 120 s on the test file below).

## Goal

A video on the phone shows a frame from the video in its grid tile when the phone gives no thumbnail, without copying the video. A phone's own thumbnail still wins.

## Approach

Read only what a decoder needs, put it at its original offsets in a sparse temporary file, and ask AVFoundation for the first frame.

1. Walk the MP4's top-level boxes with 16-byte partial reads (`ftyp`, `free`/`wide`, `mdat`, `moov`; at most 16 boxes) to learn where `moov` (the index; Android camera files put it **last**) and the data of `mdat` are.
2. Read `moov` (at most 24 MiB) and the first 2 MiB of the data after the start of `mdat`, in 1 MiB requests so other phone traffic interleaves.
3. Write them into a sparse file the size of the original (`set_len`, then the pieces at their offsets; the middle stays a hole) in a private temporary folder.
4. `AVAssetImageGenerator.copyCGImage(at: 0)` with `appliesPreferredTrackTransform = true` (portrait videos are stored rotated) and `maximumSize = THUMBNAIL_SIZE`.
5. If AVFoundation cannot make a frame and the head was smaller than the file, read more (8 MiB, then 32 MiB: `HEAD_TIERS`) and try again; then give up with `None`.
6. Delete the temporary folder. The result goes through the existing disk cache, so a video costs the phone nothing the second time.

Measured on this Mac with a 30 MB 1080p H.264 file whose `moov` is at the end: 4 MiB head + 1 MiB tail gave a thumbnail **byte-identical** to the one from the whole file, in 78 ms. With no head, AVFoundation fails cleanly ("Cannot Open"). `qlmanage` on the same sparse file hung, so it is not used for this.

## Decisions

- AVFoundation (not Quick Look, not ffmpeg): in the system, handles rotation, no process to kill. ffmpeg is not shipped.
- Supported containers: `mp4 m4v mov 3gp` (what AVFoundation reads). `mkv` and `webm` stay as today: the phone's thumbnail or the badge.
- The first frame, time 0: the only frame whose data is certain to be at the start of the file. A video that opens on black gets a black tile.
- One video at a time (a semaphore of 1), because the phone has one session and a video costs several MB of USB; the pane's batch cap (24 tiles) is unchanged.
- Bindings: the `objc2-av-foundation`, `objc2-core-media` and `objc2-core-graphics` crates at 0.3, which need `objc2` 0.6. The tree already builds `objc2` 0.6 for iced's dependencies, so no second copy is compiled, but the app's own code uses 0.5; the two never share an object (a path goes in, RGBA bytes come out). If the spike (plan Task 4) shows that is not workable, fall back to `msg_send!` on 0.5 against the same three classes.
- In-process parsing of a file from the phone: the same trust level as the in-process image decoding that exists. It is bounded by the caps below and a 5 s deadline. A helper process would isolate it further and is the follow-up if this is a concern.

## Limits (the phone's bytes are untrusted)

- File size claimed by the phone above 1 TiB: no thumbnail (a sparse `set_len` of an absurd size is refused, not attempted).
- Box sizes are checked against the file size with checked arithmetic; a box that runs past the end, a size under its header, a size of 0 anywhere but the last box, or more than 16 boxes before `moov` and `mdat` are both found: no thumbnail.
- `moov` above 24 MiB: no thumbnail. Total read above 32 MiB + `moov`: no thumbnail.
- Phone without partial reads (`supports_partial_download == false`): no thumbnail, no extra request.
- 5 s deadline on AVFoundation; a phone that goes silent is `Err(Timeout)` through the existing `device.ask`, not `None`.

## Out of scope

Video duration or a play badge on the tile; thumbnails for mkv/webm; frames other than the first; the list view.

## Mac side (optional last step)

Use the same `first_frame` for `mp4 mov m4v` on the Mac, keeping Quick Look for heic, dng and pdf: no process, no hang, rotation handled the same. It changes behavior users have today, so it is its own task and can be dropped without affecting the rest.
