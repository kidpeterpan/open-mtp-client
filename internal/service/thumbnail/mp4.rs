//! Which bytes of an MP4 or QuickTime file a decoder needs for its first frame.
//!
//! A phone's camera writes `moov`, the index of the video, after the picture
//! data (`mdat`); a decoder needs the index and the start of the data, nothing
//! between. This reads only the top-level box headers (a handful of 16-byte
//! requests) to learn where those are, and says which ranges to fetch. Pure: no
//! I/O, so every rule is a plain test. The bytes come from a phone and are not
//! trusted: sizes are checked against the file's length with checked
//! arithmetic, the walk is bounded, and nothing here allocates by what a header
//! claims.

use std::ops::Range;

/// Top-level boxes looked at before giving up on finding `moov` and `mdat`.
pub const MAX_TOP_LEVEL_BOXES: usize = 16;
/// Bytes to read at each `Walker::next_offset`: a 64-bit box header.
pub const HEADER_PEEK: u32 = 16;
/// The biggest `moov` fetched (about an hour of video is a few MB).
pub const MAX_MOOV: u64 = 24 << 20;
/// How much picture data to fetch after the start of `mdat`, tried in turn: a
/// first picture is usually a few hundred KB, a 4K one a few MB.
pub const HEAD_TIERS: [u64; 3] = [2 << 20, 8 << 20, 32 << 20];
/// A box before `mdat` this small is fetched (`ftyp`, a little padding); a bigger
/// one is a hole in the sparse file, not a read. With at most
/// `MAX_TOP_LEVEL_BOXES` of them, `MAX_PREFIX` bounds what they cost together.
pub const SMALL_BOX: u64 = 64 << 10;
pub const MAX_PREFIX: u64 = MAX_TOP_LEVEL_BOXES as u64 * SMALL_BOX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Error {
    /// Not the start of a box.
    NotMp4,
    /// A box that ends past the end of the file, or whose size overflows.
    BoxOutOfBounds,
    TooManyBoxes,
    NoMoov,
    NoMdat,
    MoovTooBig,
}

/// A box header: its kind, how long the header is (8, or 16 with a 64-bit
/// size), and the box's whole length; `None` is "to the end of the file".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: [u8; 4],
    pub header_len: u8,
    pub len: Option<u64>,
}

pub fn parse_header(bytes: &[u8]) -> Result<Header, Mp4Error> {
    let (Some(size), Some(kind)) = (bytes.get(0..4), bytes.get(4..8)) else {
        return Err(Mp4Error::NotMp4);
    };
    let size = u32::from_be_bytes(size.try_into().map_err(|_| Mp4Error::NotMp4)?);
    let kind: [u8; 4] = kind.try_into().map_err(|_| Mp4Error::NotMp4)?;
    match size {
        0 => Ok(Header {
            kind,
            header_len: 8,
            len: None,
        }),
        1 => {
            let wide = bytes.get(8..16).ok_or(Mp4Error::NotMp4)?;
            let len = u64::from_be_bytes(wide.try_into().map_err(|_| Mp4Error::NotMp4)?);
            if len < 16 {
                return Err(Mp4Error::NotMp4);
            }
            Ok(Header {
                kind,
                header_len: 16,
                len: Some(len),
            })
        }
        2..=7 => Err(Mp4Error::NotMp4),
        len => Ok(Header {
            kind,
            header_len: 8,
            len: Some(u64::from(len)),
        }),
    }
}

/// Where the parts of the file that matter are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The small boxes before `mdat` (`ftyp` and the like), each its own range.
    pub prefix: Vec<Range<u64>>,
    /// Where the `mdat` box starts, and where its data starts after its header.
    pub mdat_start: u64,
    pub mdat_data: u64,
    pub moov: Range<u64>,
}

/// Walks the top-level boxes: ask for `HEADER_PEEK` bytes at `next_offset`,
/// `feed` them, repeat until `feed` answers.
pub struct Walker {
    file_len: u64,
    offset: u64,
    seen: usize,
    prefix: Vec<Range<u64>>,
    mdat: Option<(u64, u64)>,
    moov: Option<Range<u64>>,
}

impl Walker {
    pub fn new(file_len: u64) -> Self {
        Self {
            file_len,
            offset: 0,
            seen: 0,
            prefix: Vec::new(),
            mdat: None,
            moov: None,
        }
    }

    /// Where the next header is; never past the end of the file.
    pub fn next_offset(&self) -> u64 {
        self.offset.min(self.file_len)
    }

    /// Takes the bytes read at `next_offset` (fewer at the end of the file).
    /// `Ok(Some(_))` once `moov` and `mdat` are both found; `Ok(None)` to ask
    /// for the next header.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Layout>, Mp4Error> {
        if self.seen >= MAX_TOP_LEVEL_BOXES {
            return Err(Mp4Error::TooManyBoxes);
        }
        let header = parse_header(bytes)?;
        let start = self.offset;
        let to_end = self
            .file_len
            .checked_sub(start)
            .ok_or(Mp4Error::BoxOutOfBounds)?;
        let len = header.len.unwrap_or(to_end);
        let end = start.checked_add(len).ok_or(Mp4Error::BoxOutOfBounds)?;
        if end > self.file_len || len < u64::from(header.header_len) {
            return Err(Mp4Error::BoxOutOfBounds);
        }
        match &header.kind {
            b"moov" => {
                if len > MAX_MOOV {
                    return Err(Mp4Error::MoovTooBig);
                }
                self.moov.get_or_insert(start..end);
            }
            b"mdat" => {
                self.mdat
                    .get_or_insert((start, start + u64::from(header.header_len)));
            }
            _ if self.mdat.is_none() && len <= SMALL_BOX => self.prefix.push(start..end),
            _ => {}
        }
        self.seen += 1;
        self.offset = end;

        if let (Some(moov), Some((mdat_start, mdat_data))) = (&self.moov, self.mdat) {
            return Ok(Some(Layout {
                prefix: std::mem::take(&mut self.prefix),
                mdat_start,
                mdat_data,
                moov: moov.clone(),
            }));
        }
        if self.seen >= MAX_TOP_LEVEL_BOXES {
            return Err(Mp4Error::TooManyBoxes);
        }
        if self.offset >= self.file_len {
            return Err(if self.moov.is_none() {
                Mp4Error::NoMoov
            } else {
                Mp4Error::NoMdat
            });
        }
        Ok(None)
    }
}

/// The ranges to fetch for the first frame at head size `HEAD_TIERS[tier]`:
/// sorted, merged where they touch, clamped to the file. `None` past the last
/// tier. A faststart file (`moov` first) is one range; a phone's usual file is
/// the start and the `moov`.
pub fn read_plan(layout: &Layout, file_len: u64, tier: usize) -> Option<Vec<Range<u64>>> {
    let head = *HEAD_TIERS.get(tier)?;
    let data_end = layout.mdat_data.saturating_add(head).min(file_len);
    let mut wanted: Vec<Range<u64>> = layout.prefix.clone();
    wanted.push(layout.mdat_start..data_end);
    wanted.push(layout.moov.clone());
    wanted.retain(|range| range.start < range.end && range.end <= file_len);
    wanted.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<u64>> = Vec::new();
    for range in wanted {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    Some(merged)
}

/// The parts of `want` that `have` does not cover: what a bigger tier still
/// has to read.
pub fn missing(have: &[Range<u64>], want: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut out = Vec::new();
    for range in want {
        let mut from = range.start;
        let mut covering: Vec<&Range<u64>> = have
            .iter()
            .filter(|h| h.start < range.end && h.end > range.start)
            .collect();
        covering.sort_by_key(|h| h.start);
        for h in covering {
            if h.start > from {
                out.push(from..h.start);
            }
            from = from.max(h.end);
        }
        if from < range.end {
            out.push(from..range.end);
        }
    }
    out
}

#[cfg(test)]
// A list holding one range is exactly what these tests mean.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use std::ops::Range;

    const MIB: u64 = 1 << 20;

    /// One top-level box for `build`: its kind, how many bytes follow its
    /// header, and whether its header carries a 64-bit size.
    struct B(&'static [u8; 4], u64, bool);

    /// The bytes of a file made of `boxes`, payloads zero.
    fn build(boxes: &[B]) -> Vec<u8> {
        let mut out = Vec::new();
        for B(kind, payload, wide) in boxes {
            let header: u64 = if *wide { 16 } else { 8 };
            if *wide {
                out.extend_from_slice(&1u32.to_be_bytes());
                out.extend_from_slice(*kind);
                out.extend_from_slice(&(header + payload).to_be_bytes());
            } else {
                out.extend_from_slice(&((header + payload) as u32).to_be_bytes());
                out.extend_from_slice(*kind);
            }
            out.resize(out.len() + *payload as usize, 0);
        }
        out
    }

    /// Walks `bytes` the way the service does: read `HEADER_PEEK` bytes where asked.
    fn walk(bytes: &[u8]) -> Result<Layout, Mp4Error> {
        walk_claiming(bytes, bytes.len() as u64)
    }

    fn walk_claiming(bytes: &[u8], file_len: u64) -> Result<Layout, Mp4Error> {
        let mut walker = Walker::new(file_len);
        for _ in 0..=MAX_TOP_LEVEL_BOXES + 1 {
            let at = walker.next_offset() as usize;
            let end = (at + HEADER_PEEK as usize).min(bytes.len());
            let window = bytes.get(at..end).unwrap_or(&[]);
            if let Some(layout) = walker.feed(window)? {
                return Ok(layout);
            }
        }
        panic!("the walk did not end");
    }

    #[test]
    fn parse_header_reads_a_32_bit_size_and_the_kind() {
        let header = parse_header(&build(&[B(b"ftyp", 20, false)])).unwrap();
        assert_eq!(
            header,
            Header {
                kind: *b"ftyp",
                header_len: 8,
                len: Some(28)
            }
        );
    }

    #[test]
    fn parse_header_reads_a_64_bit_size() {
        let header = parse_header(&build(&[B(b"mdat", 100, true)])).unwrap();
        assert_eq!(
            header,
            Header {
                kind: *b"mdat",
                header_len: 16,
                len: Some(116)
            }
        );
    }

    #[test]
    fn a_size_of_zero_means_to_the_end_of_the_file() {
        let mut bytes = vec![0, 0, 0, 0];
        bytes.extend_from_slice(b"mdat");
        assert_eq!(
            parse_header(&bytes),
            Ok(Header {
                kind: *b"mdat",
                header_len: 8,
                len: None
            })
        );
    }

    #[test]
    fn a_size_below_its_own_header_is_refused() {
        for size in [2u32, 7] {
            let mut bytes = size.to_be_bytes().to_vec();
            bytes.extend_from_slice(b"free");
            assert_eq!(parse_header(&bytes), Err(Mp4Error::NotMp4), "size {size}");
        }
        let mut wide = 1u32.to_be_bytes().to_vec();
        wide.extend_from_slice(b"mdat");
        wide.extend_from_slice(&15u64.to_be_bytes());
        assert_eq!(parse_header(&wide), Err(Mp4Error::NotMp4), "64-bit size 15");
    }

    #[test]
    fn fewer_than_eight_bytes_or_a_cut_off_64_bit_size_is_refused() {
        assert_eq!(
            parse_header(&[0, 0, 0, 8, b'f', b'r', b'e']),
            Err(Mp4Error::NotMp4)
        );
        assert_eq!(parse_header(&[]), Err(Mp4Error::NotMp4));
        let mut cut = 1u32.to_be_bytes().to_vec();
        cut.extend_from_slice(b"mdat");
        cut.extend_from_slice(&[0, 0, 0]);
        assert_eq!(parse_header(&cut), Err(Mp4Error::NotMp4));
    }

    #[test]
    fn moov_at_the_end_is_found() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"free", 100, false),
            B(b"mdat", 5000, false),
            B(b"moov", 300, false),
        ]);
        let layout = walk(&bytes).unwrap();
        // ftyp 28, free 108, mdat starts at 136 with an 8-byte header
        assert_eq!(layout.mdat_start, 136);
        assert_eq!(layout.mdat_data, 144);
        assert_eq!(layout.moov, 5144..5452);
        assert_eq!(layout.prefix, vec![0..28, 28..136]);
    }

    #[test]
    fn moov_before_mdat_is_found() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"moov", 300, false),
            B(b"mdat", 5000, false),
        ]);
        let layout = walk(&bytes).unwrap();
        assert_eq!(layout.moov, 28..336);
        assert_eq!(layout.mdat_start, 336);
        assert_eq!(layout.mdat_data, 344);
    }

    #[test]
    fn a_64_bit_mdat_is_walked_over() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"mdat", 5000, true),
            B(b"moov", 40, false),
        ]);
        let layout = walk(&bytes).unwrap();
        assert_eq!(layout.mdat_start, 28);
        assert_eq!(layout.mdat_data, 44, "after the 16-byte header");
        assert_eq!(layout.moov, 28 + 5016..28 + 5016 + 48);
    }

    #[test]
    fn offsets_past_4_gb_stay_u64() {
        // Only the headers are read, so the 5 GiB file is never built: a reader
        // answers each peek from a table.
        let file_len: u64 = 5 << 30;
        let mdat_len = file_len - 28 - 4096;
        let mut walker = Walker::new(file_len);
        let ftyp = build(&[B(b"ftyp", 20, false)]);
        assert_eq!(walker.next_offset(), 0);
        assert_eq!(walker.feed(&ftyp[..16]), Ok(None));
        assert_eq!(walker.next_offset(), 28);
        let mut mdat = 1u32.to_be_bytes().to_vec();
        mdat.extend_from_slice(b"mdat");
        mdat.extend_from_slice(&mdat_len.to_be_bytes());
        assert_eq!(walker.feed(&mdat), Ok(None));
        assert_eq!(walker.next_offset(), 28 + mdat_len);
        assert!(walker.next_offset() > u64::from(u32::MAX));
        let mut moov = 4096u32.to_be_bytes().to_vec();
        moov.extend_from_slice(b"moov");
        let layout = walker.feed(&moov).unwrap().unwrap();
        assert_eq!(layout.moov, file_len - 4096..file_len);
        assert_eq!(layout.mdat_data, 28 + 16);
        let pieces = read_plan(&layout, file_len, 0).unwrap();
        assert_eq!(pieces, vec![0..44 + 2 * MIB, file_len - 4096..file_len]);
    }

    #[test]
    fn a_to_the_end_mdat_means_no_moov_after_it() {
        let mut bytes = build(&[B(b"ftyp", 20, false)]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"mdat");
        bytes.extend_from_slice(&[0; 1000]);
        assert_eq!(walk(&bytes), Err(Mp4Error::NoMoov));
    }

    #[test]
    fn no_mdat_is_refused() {
        let bytes = build(&[B(b"ftyp", 20, false), B(b"moov", 100, false)]);
        assert_eq!(walk(&bytes), Err(Mp4Error::NoMdat));
    }

    #[test]
    fn a_box_that_runs_past_the_end_of_the_file_is_refused() {
        let bytes = build(&[B(b"ftyp", 20, false), B(b"mdat", 5000, false)]);
        let cut = &bytes[..bytes.len() - 1];
        assert_eq!(walk(cut), Err(Mp4Error::BoxOutOfBounds));
    }

    #[test]
    fn a_box_size_near_u64_max_does_not_overflow() {
        let mut bytes = build(&[B(b"ftyp", 20, false)]);
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(walk(&bytes), Err(Mp4Error::BoxOutOfBounds));
    }

    #[test]
    fn more_than_sixteen_boxes_without_both_is_refused() {
        let boxes: Vec<B> = (0..20).map(|_| B(b"free", 0, false)).collect();
        assert_eq!(walk(&build(&boxes)), Err(Mp4Error::TooManyBoxes));
    }

    #[test]
    fn a_zero_payload_box_still_advances() {
        let boxes: Vec<B> = (0..16).map(|_| B(b"skip", 0, false)).collect();
        assert_eq!(
            walk(&build(&boxes)),
            Err(Mp4Error::TooManyBoxes),
            "ends by the cap, never loops"
        );
    }

    #[test]
    fn a_moov_over_the_cap_is_refused() {
        let file_len = 28 + 8 + MAX_MOOV + 1;
        let mut walker = Walker::new(file_len);
        walker.feed(&build(&[B(b"ftyp", 20, false)])[..16]).unwrap();
        let mut mdat = 8u32.to_be_bytes().to_vec();
        mdat.extend_from_slice(b"mdat");
        walker.feed(&mdat).unwrap();
        let mut moov = ((MAX_MOOV + 1) as u32).to_be_bytes().to_vec();
        moov.extend_from_slice(b"moov");
        assert_eq!(walker.feed(&moov), Err(Mp4Error::MoovTooBig));
    }

    #[test]
    fn a_big_box_before_mdat_is_left_a_hole_not_read() {
        let file_len = 28 + (1 << 30) + 8 + 5000 + 300;
        let mut walker = Walker::new(file_len);
        walker.feed(&build(&[B(b"ftyp", 20, false)])[..16]).unwrap();
        let mut free = 1u32.to_be_bytes().to_vec();
        free.extend_from_slice(b"free");
        free.extend_from_slice(&(1u64 << 30).to_be_bytes());
        walker.feed(&free).unwrap();
        let mut mdat = 5008u32.to_be_bytes().to_vec();
        mdat.extend_from_slice(b"mdat");
        walker.feed(&mdat).unwrap();
        let mut moov = 300u32.to_be_bytes().to_vec();
        moov.extend_from_slice(b"moov");
        let layout = walker.feed(&moov).unwrap().unwrap();
        assert_eq!(layout.prefix, vec![0..28], "only the ftyp");
        let pieces = read_plan(&layout, file_len, 0).unwrap();
        assert_eq!(pieces[0], 0..28);
        assert!(
            pieces[1].start >= 28 + (1 << 30),
            "the gigabyte of free is not read"
        );
    }

    fn layout_moov_last() -> (Layout, u64) {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"mdat", 10 * MIB, false),
            B(b"moov", 300, false),
        ]);
        (walk(&bytes).unwrap(), bytes.len() as u64)
    }

    #[test]
    fn read_plan_for_a_moov_last_file_reads_the_start_and_the_moov_separately() {
        let (layout, len) = layout_moov_last();
        let pieces = read_plan(&layout, len, 0).unwrap();
        assert_eq!(
            pieces,
            vec![0..layout.mdat_data + 2 * MIB, layout.moov.clone()]
        );
    }

    #[test]
    fn read_plan_for_a_faststart_file_is_one_range() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"moov", 300, false),
            B(b"mdat", 10 * MIB, false),
        ]);
        let layout = walk(&bytes).unwrap();
        let pieces = read_plan(&layout, bytes.len() as u64, 0).unwrap();
        assert_eq!(pieces, vec![0..layout.mdat_data + 2 * MIB]);
    }

    #[test]
    fn read_plan_clamps_to_a_small_file_and_merges_what_touches() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"mdat", 1000, false),
            B(b"moov", 300, false),
        ]);
        let layout = walk(&bytes).unwrap();
        let len = bytes.len() as u64;
        assert_eq!(read_plan(&layout, len, 0).unwrap(), vec![0..len]);
    }

    #[test]
    fn read_plan_tiers_grow_and_the_last_is_32_mib() {
        let bytes = build(&[
            B(b"ftyp", 20, false),
            B(b"mdat", 100 * MIB, false),
            B(b"moov", 300, false),
        ]);
        let layout = walk(&bytes).unwrap();
        let len = bytes.len() as u64;
        let end = |tier| read_plan(&layout, len, tier).unwrap()[0].end - layout.mdat_data;
        assert_eq!([end(0), end(1), end(2)], [2 * MIB, 8 * MIB, 32 * MIB]);
    }

    #[test]
    fn read_plan_past_the_last_tier_is_none() {
        let (layout, len) = layout_moov_last();
        assert_eq!(read_plan(&layout, len, 3), None);
    }

    #[test]
    fn missing_is_what_was_not_read_yet() {
        let want = vec![0..100, 500..600];
        assert_eq!(missing(&[], &want), want);
        assert_eq!(
            missing(&[0..100, 500..600], &want),
            Vec::<Range<u64>>::new()
        );
        assert_eq!(missing(&[0..40], &want), vec![40..100, 500..600]);
        assert_eq!(
            missing(&[20..60, 50..80], &want),
            vec![0..20, 80..100, 500..600]
        );
        assert_eq!(missing(&[0..1000], &want), Vec::<Range<u64>>::new());
        assert_eq!(
            missing(&[0..100], &[0..150, 500..600]),
            vec![100..150, 500..600]
        );
    }

    /// A small deterministic generator, so a failure names a seed that replays.
    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 33
        }
    }

    #[test]
    fn random_bytes_never_panic_and_always_end() {
        for seed in 0..5_000u64 {
            let mut next = rng(seed);
            let file_len = next() << (next() % 12);
            let mut walker = Walker::new(file_len);
            let mut ended = false;
            for _ in 0..=MAX_TOP_LEVEL_BOXES {
                assert!(walker.next_offset() <= file_len, "seed {seed}");
                let mut window = [0u8; 16];
                for byte in &mut window {
                    *byte = if next().is_multiple_of(3) {
                        0
                    } else {
                        next() as u8
                    };
                }
                if next().is_multiple_of(4) {
                    window[4..8].copy_from_slice(if next().is_multiple_of(2) {
                        b"moov"
                    } else {
                        b"mdat"
                    });
                }
                let cut = (next() % 17) as usize;
                match walker.feed(&window[..cut]) {
                    Ok(Some(layout)) => {
                        assert!(layout.moov.start < layout.moov.end, "seed {seed}");
                        assert!(layout.moov.end <= file_len, "seed {seed}");
                        assert!(layout.mdat_data <= file_len, "seed {seed}");
                        ended = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        ended = true;
                        break;
                    }
                }
            }
            assert!(
                ended,
                "seed {seed}: the walk did not end within the box cap"
            );
        }
    }

    #[test]
    fn a_random_box_sequence_is_found_where_it_was_put() {
        const FILLER: [&[u8; 4]; 4] = [b"free", b"wide", b"skip", b"uuid"];
        for seed in 0..2_000u64 {
            let mut next = rng(seed);
            let before = (next() % 5) as usize;
            let mut boxes = vec![B(b"ftyp", 20, false)];
            let mut order = vec![b"mdat", b"moov"];
            if next().is_multiple_of(2) {
                order.reverse();
            }
            for kind in order {
                for _ in 0..(next() % (before as u64 + 1)) {
                    boxes.push(B(
                        FILLER[(next() % 4) as usize],
                        next() % 3000,
                        next().is_multiple_of(2),
                    ));
                }
                let payload = 8 + next() % 20_000;
                let wide = next().is_multiple_of(2);
                boxes.push(B(kind, payload, wide));
            }
            let bytes = build(&boxes);
            let layout = walk(&bytes).unwrap_or_else(|err| panic!("seed {seed}: {err:?}"));

            let mut at = 0u64;
            let (mut moov, mut mdat) = (None, None);
            for B(kind, payload, wide) in &boxes {
                let header = if *wide { 16 } else { 8 };
                let end = at + header + payload;
                match *kind {
                    b"moov" => moov = Some(at..end),
                    b"mdat" => mdat = Some((at, at + header)),
                    _ => {}
                }
                at = end;
            }
            assert_eq!(Some(layout.moov.clone()), moov, "seed {seed}");
            let (start, data) = mdat.unwrap();
            assert_eq!(
                (layout.mdat_start, layout.mdat_data),
                (start, data),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn the_small_boxes_before_mdat_cost_at_most_max_prefix_together() {
        // 14 boxes of exactly the biggest "small" size, then mdat and moov.
        let mut boxes: Vec<B> = vec![B(b"ftyp", SMALL_BOX - 8, false)];
        boxes.extend((0..13).map(|_| B(b"free", SMALL_BOX - 8, false)));
        boxes.push(B(b"mdat", 5000, false));
        boxes.push(B(b"moov", 300, false));
        let bytes = build(&boxes);
        let layout = walk(&bytes).unwrap();
        assert_eq!(layout.prefix.len(), 14);
        let len = bytes.len() as u64;
        let pieces = read_plan(&layout, len, 0).unwrap();
        let total: u64 = pieces.iter().map(|p| p.end - p.start).sum();
        assert_eq!(total, len, "the whole small file");
        const { assert!(14 * SMALL_BOX <= MAX_PREFIX) };
        // One byte bigger and it is a hole, not a read.
        let mut boxes: Vec<B> = vec![B(b"ftyp", SMALL_BOX - 7, false)];
        boxes.push(B(b"mdat", 5000, false));
        boxes.push(B(b"moov", 300, false));
        assert!(walk(&build(&boxes)).unwrap().prefix.is_empty());
    }

    #[test]
    fn what_a_plan_reads_is_always_bounded() {
        for seed in 0..2_000u64 {
            let mut next = rng(seed);
            let len = 100 + (next() << (next() % 14));
            let a = next() % len;
            let layout = Layout {
                prefix: vec![0..(next() % 28.min(len))],
                mdat_start: a,
                mdat_data: (a + 16).min(len),
                moov: {
                    let start = (next() % len).min(len - 1);
                    start..(start + 1 + next() % MAX_MOOV).min(len)
                },
            };
            for (tier, head) in HEAD_TIERS.iter().enumerate() {
                let pieces = read_plan(&layout, len, tier).unwrap();
                let total: u64 = pieces.iter().map(|p| p.end - p.start).sum();
                assert!(total <= MAX_MOOV + head + 16 + MAX_PREFIX, "seed {seed}");
                for pair in pieces.windows(2) {
                    assert!(pair[0].end < pair[1].start, "seed {seed}: sorted, merged");
                }
                assert!(
                    pieces.iter().all(|p| p.start < p.end && p.end <= len),
                    "seed {seed}"
                );
            }
        }
    }
}
