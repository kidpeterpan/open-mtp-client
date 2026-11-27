//! The small test videos in `internal/tests/fixtures/video`, made by
//! `scripts/make_video_fixtures.swift`. Frame 0 of each, as it is SEEN, has a
//! red top-left quadrant on blue.

/// The bytes of `internal/tests/fixtures/video/<name>`.
pub fn bytes(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/video")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Where the boxes of `moov_at_end.mp4` are: `(mdat start, mdat data, moov start)`.
fn layout_of(whole: &[u8]) -> (usize, usize, usize) {
    use super::mp4::{HEADER_PEEK, Walker};
    let mut walker = Walker::new(whole.len() as u64);
    let layout = loop {
        let at = walker.next_offset() as usize;
        let end = (at + HEADER_PEEK as usize).min(whole.len());
        if let Some(layout) = walker.feed(&whole[at..end]).unwrap() {
            break layout;
        }
    };
    assert_eq!(
        layout.mdat_data - layout.mdat_start,
        16,
        "the fixture's mdat has a 64-bit size"
    );
    (
        layout.mdat_start as usize,
        layout.mdat_data as usize,
        layout.moov.start as usize,
    )
}

/// `moov_at_end.mp4` with `extra` bytes of 0xAB filler at the end of its `mdat`:
/// the first picture is where it was and the index (which points at absolute
/// offsets before the filler) still works, but a long hole lies before `moov`.
pub fn padded_at_end(extra: usize) -> Vec<u8> {
    let whole = bytes("moov_at_end.mp4");
    let (mdat_start, _, moov_at) = layout_of(&whole);
    let size_at = mdat_start + 8;
    let old = u64::from_be_bytes(whole[size_at..size_at + 8].try_into().unwrap());
    let mut out = whole[..moov_at].to_vec();
    out[size_at..size_at + 8].copy_from_slice(&(old + extra as u64).to_be_bytes());
    out.extend(std::iter::repeat_n(0xAB, extra));
    out.extend_from_slice(&whole[moov_at..]);
    out
}

/// `moov_at_end.mp4` with `extra` bytes of 0xAB filler at the start of its
/// `mdat`: the first picture now lies `extra` bytes into the data, and the
/// index is made to say so.
pub fn padded_at_start(extra: usize) -> Vec<u8> {
    padded_before_chunk("moov_at_end.mp4", 0, extra)
}

/// `name` (a clip laid out like `moov_at_end.mp4`: `ftyp`, `mdat`, `moov`) with
/// `extra` bytes of 0xAB filler put in front of its chunk number `chunk`: that
/// chunk and every later one lie `extra` bytes further on, and the index is
/// made to say so. `padded_before_chunk(name, 0, extra)` is `padded_at_start`.
pub fn padded_before_chunk(name: &str, chunk: usize, extra: usize) -> Vec<u8> {
    let whole = bytes(name);
    let (mdat_start, _, moov_at) = layout_of(&whole);
    let size_at = mdat_start + 8;
    let old = u64::from_be_bytes(whole[size_at..size_at + 8].try_into().unwrap());
    let mut moov = whole[moov_at..].to_vec();
    // `stco`: size, kind, version and flags, a count, then 32-bit chunk offsets.
    let stco = moov
        .windows(4)
        .position(|w| w == b"stco")
        .expect("an stco box");
    let count = u32::from_be_bytes(moov[stco + 8..stco + 12].try_into().unwrap()) as usize;
    assert!(chunk < count, "{name} has {count} chunks");
    let entry = |at: usize| stco + 12 + at * 4;
    let from = u32::from_be_bytes(moov[entry(chunk)..entry(chunk) + 4].try_into().unwrap());
    for at in 0..count {
        let offset = u32::from_be_bytes(moov[entry(at)..entry(at) + 4].try_into().unwrap());
        if offset >= from {
            moov[entry(at)..entry(at) + 4].copy_from_slice(&(offset + extra as u32).to_be_bytes());
        }
    }
    let mut out = whole[..from as usize].to_vec();
    out[size_at..size_at + 8].copy_from_slice(&(old + extra as u64).to_be_bytes());
    out.extend(std::iter::repeat_n(0xAB, extra));
    out.extend_from_slice(&whole[from as usize..moov_at]);
    out.extend_from_slice(&moov);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (kind, start, size) of each top-level box: a size of 1 means the real
    /// size follows the kind as 8 bytes (AVFoundation writes `mdat` that way).
    fn boxes(bytes: &[u8]) -> Vec<(String, usize, usize)> {
        let mut found = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned();
            let size = if size == 1 {
                u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap()) as usize
            } else {
                size
            };
            found.push((kind, at, size));
            at += size;
        }
        assert_eq!(at, bytes.len(), "the boxes fill the file exactly");
        found
    }

    fn kinds(name: &str) -> Vec<String> {
        boxes(&bytes(name))
            .into_iter()
            .map(|(kind, ..)| kind)
            .collect()
    }

    #[test]
    fn fixtures_have_the_layouts_the_tests_assume() {
        assert_eq!(kinds("moov_at_end.mp4"), ["ftyp", "mdat", "moov"]);
        assert_eq!(kinds("portrait.mp4"), ["ftyp", "mdat", "moov"]);
        assert_eq!(kinds("faststart.mp4"), ["ftyp", "moov", "mdat"]);
        assert_eq!(kinds("dark_start.mp4"), ["ftyp", "mdat", "moov"]);
        assert_eq!(kinds("black.mp4"), ["ftyp", "mdat", "moov"]);
        for name in [
            "moov_at_end.mp4",
            "faststart.mp4",
            "portrait.mp4",
            "dark_start.mp4",
            "black.mp4",
        ] {
            assert!(bytes(name).len() <= 60 * 1024, "{name} stays small");
        }
    }

    #[test]
    fn padding_before_a_chunk_moves_that_chunk_and_the_ones_after_it() {
        let whole = bytes("dark_start.mp4");
        let padded = padded_before_chunk("dark_start.mp4", 1, 5000);
        assert_eq!(padded.len(), whole.len() + 5000);
        // `dark_start.mp4` has two chunks of 30 frames: 0-29 at 44, 30-59 at 1023.
        // (the `mdat`'s own size, at 36..44, is longer by the filler)
        assert_eq!(&padded[..36], &whole[..36]);
        assert_eq!(&padded[44..1023], &whole[44..1023], "the first chunk stays");
        assert!(padded[1023..6023].iter().all(|&b| b == 0xAB));
        assert_eq!(
            &padded[6023..6039],
            &whole[1023..1039],
            "the second chunk follows"
        );
        assert_eq!(
            padded_before_chunk("moov_at_end.mp4", 0, 777),
            padded_at_start(777)
        );
    }
}
