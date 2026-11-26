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
        for name in ["moov_at_end.mp4", "faststart.mp4", "portrait.mp4"] {
            assert!(bytes(name).len() <= 60 * 1024, "{name} stays small");
        }
    }
}
