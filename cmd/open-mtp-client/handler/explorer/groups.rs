//! Rows under headers, as Finder's View › Use Groups. Pure:
//! which group a row is in, the rows put in group order (stable, so the
//! pane's own order holds inside each group), and the mapping between rows
//! and the lines they are drawn on, a header being one line. Everything here
//! costs O(groups) a call, never O(rows), so the pane still builds only what
//! is on screen.

use chrono::{Datelike, NaiveDate};
use internal::dto::{Entry, FileKind, GroupBy};

use crate::handler::format::format_bytes;

/// Rows `start..start + len` under one header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    pub start: usize,
    pub len: usize,
    /// The files' bytes, where known.
    pub bytes: u64,
}

impl Group {
    /// What the header says after the label: "12 items, 40.0 MB".
    pub fn summary(&self) -> String {
        let items = match self.len {
            1 => "1 item".to_owned(),
            n => format!("{} items", crate::handler::format::format_count(n)),
        };
        if self.bytes == 0 {
            items
        } else {
            format!("{items}, {}", format_bytes(self.bytes))
        }
    }
}

/// Where a row goes: its group's rank (lower first) and label.
pub fn bucket(by: GroupBy, entry: &Entry, today: NaiveDate) -> (u32, String) {
    match by {
        GroupBy::None => (0, String::new()),
        GroupBy::Date => date_bucket(
            entry.modified.as_ref().and_then(|time| {
                let day = time.day();
                NaiveDate::from_ymd_opt(
                    i32::from(day.year),
                    u32::from(day.month),
                    u32::from(day.day),
                )
            }),
            today,
        ),
        GroupBy::Kind => kind_bucket(entry),
        GroupBy::Size => size_bucket(entry),
    }
}

/// Finder's date groups, newest first; a file without a date comes last and
/// one dated after today first.
pub fn date_bucket(day: Option<NaiveDate>, today: NaiveDate) -> (u32, String) {
    let Some(day) = day else {
        return (u32::MAX, "No Date".to_owned());
    };
    let ago = (today - day).num_days();
    match ago {
        ..0 => (0, "Later".to_owned()),
        0 => (1, "Today".to_owned()),
        1 => (2, "Yesterday".to_owned()),
        2..=7 => (3, "Previous 7 Days".to_owned()),
        8..=30 => (4, "Previous 30 Days".to_owned()),
        _ if day.year() == today.year() => (
            // This year's months, newest first.
            10 + (12 - day.month()),
            day.format("%B %Y").to_string(),
        ),
        _ => (
            100 + u32::try_from(today.year() - day.year()).unwrap_or(u32::MAX - 101),
            day.year().to_string(),
        ),
    }
}

fn kind_bucket(entry: &Entry) -> (u32, String) {
    if entry.is_folder {
        return (0, "Folders".to_owned());
    }
    match FileKind::of(&entry.name) {
        Some(FileKind::Photo) => (1, "Photos".to_owned()),
        Some(FileKind::Video) => (2, "Videos".to_owned()),
        Some(FileKind::Music) => (3, "Music".to_owned()),
        Some(FileKind::Document) => (4, "Documents".to_owned()),
        Some(FileKind::Archive) => (5, "Archives".to_owned()),
        None => (9, "Other".to_owned()),
    }
}

fn size_bucket(entry: &Entry) -> (u32, String) {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    if entry.is_folder {
        return (0, "Folders".to_owned());
    }
    // MTP's "4 GB or more".
    let size = entry.size_bytes;
    let (rank, label) = match size {
        _ if size == u64::from(u32::MAX) || size >= GB => (1, "1 GB or More"),
        _ if size >= 100 * MB => (2, "100 MB to 1 GB"),
        _ if size >= 10 * MB => (3, "10 to 100 MB"),
        _ if size >= MB => (4, "1 to 10 MB"),
        _ if size >= 100 * KB => (5, "100 KB to 1 MB"),
        0 => (7, "Empty"),
        _ => (6, "Under 100 KB"),
    };
    (rank, label.to_owned())
}

/// Puts the rows in group order (stable: within a group, as they were) and
/// says where each group starts. `keys` and `sizes` are the rows', in their
/// order; the answer is the new order of row positions and the groups.
pub fn arrange(keys: &[(u32, String)], sizes: &[u64]) -> (Vec<usize>, Vec<Group>) {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by_key(|&at| keys[at].0);
    let mut groups: Vec<Group> = Vec::new();
    for (row, &at) in order.iter().enumerate() {
        let (rank, label) = &keys[at];
        let bytes = sizes[at];
        match groups.last_mut() {
            Some(group) if keys[order[group.start]].0 == *rank => {
                group.len += 1;
                group.bytes = group.bytes.saturating_add(bytes);
            }
            _ => groups.push(Group {
                label: label.clone(),
                start: row,
                len: 1,
                bytes,
            }),
        }
    }
    (order, groups)
}

/// What a line of the list is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Header(usize),
    Row(usize),
}

/// Lines in the list: every row, and a header a group.
pub fn list_lines(groups: &[Group], rows: usize) -> usize {
    rows + groups.len()
}

/// The group `row` is in.
pub fn group_of(groups: &[Group], row: usize) -> usize {
    groups
        .partition_point(|group| group.start <= row)
        .saturating_sub(1)
}

/// What is drawn on the list's line `line`.
pub fn list_line(groups: &[Group], line: usize) -> Line {
    // Group g's header is on line start + g: the last group whose header is
    // at or above `line`, by binary search.
    let (mut low, mut high) = (0, groups.len());
    while low < high {
        let middle = (low + high) / 2;
        if groups[middle].start + middle <= line {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let g = low.saturating_sub(1);
    let header = groups.get(g).map_or(0, |group| group.start + g);
    if line == header {
        Line::Header(g)
    } else {
        Line::Row(line - g - 1)
    }
}

/// The list line `row` is drawn on.
pub fn list_line_of(groups: &[Group], row: usize) -> usize {
    row + group_of(groups, row) + 1
}

/// What a line of the grid is: a header, or the tiles of rows
/// `first..end` (at most `columns`, all of one group).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridLine {
    Header(usize),
    Tiles { first: usize, end: usize },
}

/// The line each group's header is on in a grid `columns` wide, and the
/// lines in all.
pub fn grid_starts(groups: &[Group], columns: usize) -> (Vec<usize>, usize) {
    let columns = columns.max(1);
    let mut starts = Vec::with_capacity(groups.len());
    let mut line = 0;
    for group in groups {
        starts.push(line);
        line += 1 + group.len.div_ceil(columns);
    }
    (starts, line)
}

/// What is drawn on the grid's line `line`.
pub fn grid_line(groups: &[Group], columns: usize, starts: &[usize], line: usize) -> GridLine {
    let columns = columns.max(1);
    let g = starts
        .partition_point(|&start| start <= line)
        .saturating_sub(1);
    let Some(group) = groups.get(g) else {
        return GridLine::Tiles { first: 0, end: 0 };
    };
    let into = line - starts[g];
    if into == 0 {
        return GridLine::Header(g);
    }
    let first = group.start + (into - 1) * columns;
    let end = (first + columns).min(group.start + group.len);
    GridLine::Tiles {
        first: first.min(end),
        end,
    }
}

/// The grid line `row`'s tile is on, and its column there.
pub fn grid_place(
    groups: &[Group],
    columns: usize,
    starts: &[usize],
    row: usize,
) -> (usize, usize) {
    let columns = columns.max(1);
    let g = group_of(groups, row);
    let into = row - groups.get(g).map_or(0, |group| group.start);
    (
        starts.get(g).copied().unwrap_or(0) + 1 + into / columns,
        into % columns,
    )
}

/// A grid grouped: tile lines `row` high, header lines `header` high,
/// headers starting at `starts` (from `grid_starts`), `lines` in all.
#[derive(Debug, Clone)]
pub struct GridFrame {
    pub starts: Vec<usize>,
    pub lines: usize,
    pub row: f32,
    pub header: f32,
}

impl GridFrame {
    pub fn new(groups: &[Group], columns: usize, row: f32, header: f32) -> Self {
        let (starts, lines) = grid_starts(groups, columns);
        Self {
            starts,
            lines,
            row,
            header,
        }
    }

    /// How far down line `line` starts.
    pub fn top(&self, line: usize) -> f32 {
        let headers = self.starts.partition_point(|&start| start < line);
        (line - headers) as f32 * self.row + headers as f32 * self.header
    }

    /// How tall line `line` is.
    pub fn height(&self, line: usize) -> f32 {
        if self.starts.binary_search(&line).is_ok() {
            self.header
        } else {
            self.row
        }
    }

    /// Everything's height.
    pub fn total(&self) -> f32 {
        self.top(self.lines)
    }

    /// The line at `y` (the last line, past the end).
    pub fn line_at(&self, y: f32) -> usize {
        let (mut low, mut high) = (0, self.lines);
        while low < high {
            let middle = (low + high) / 2;
            if self.top(middle + 1) <= y {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low.min(self.lines.saturating_sub(1))
    }

    /// The lines worth building for a view `height` high scrolled to
    /// `offset`, with a couple past each edge.
    pub fn window(&self, offset: f32, height: f32) -> std::ops::Range<usize> {
        if self.lines == 0 {
            return 0..0;
        }
        let first = self.line_at(offset.max(0.0)).saturating_sub(2);
        let last = (self.line_at(offset.max(0.0) + height.max(self.row)) + 3).min(self.lines);
        first..last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn dates_fall_into_finders_groups_at_their_edges() {
        let today = day(2026, 10, 18);
        let label = |d: Option<NaiveDate>| date_bucket(d, today).1;
        assert_eq!(label(Some(day(2026, 10, 19))), "Later");
        assert_eq!(label(Some(today)), "Today");
        assert_eq!(label(Some(day(2026, 10, 17))), "Yesterday");
        assert_eq!(label(Some(day(2026, 10, 16))), "Previous 7 Days");
        assert_eq!(label(Some(day(2026, 10, 11))), "Previous 7 Days");
        assert_eq!(label(Some(day(2026, 10, 10))), "Previous 30 Days");
        assert_eq!(label(Some(day(2026, 9, 18))), "Previous 30 Days");
        assert_eq!(label(Some(day(2026, 9, 17))), "September 2026");
        assert_eq!(label(Some(day(2026, 1, 1))), "January 2026");
        assert_eq!(label(Some(day(2025, 12, 31))), "2025");
        assert_eq!(label(Some(day(2019, 6, 1))), "2019");
        assert_eq!(label(None), "No Date");

        // Newest first, undated last.
        let rank = |d: Option<NaiveDate>| date_bucket(d, today).0;
        let order = [
            rank(Some(day(2026, 10, 19))),
            rank(Some(today)),
            rank(Some(day(2026, 10, 17))),
            rank(Some(day(2026, 10, 12))),
            rank(Some(day(2026, 10, 1))),
            rank(Some(day(2026, 8, 1))),
            rank(Some(day(2026, 2, 1))),
            rank(Some(day(2025, 2, 1))),
            rank(Some(day(2018, 2, 1))),
            rank(None),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    }

    #[test]
    fn on_new_years_day_last_week_is_still_previous_7_days() {
        let today = day(2027, 1, 1);
        assert_eq!(date_bucket(Some(day(2026, 12, 31)), today).1, "Yesterday");
        assert_eq!(
            date_bucket(Some(day(2026, 12, 26)), today).1,
            "Previous 7 Days"
        );
        assert_eq!(date_bucket(Some(day(2026, 11, 1)), today).1, "2026");
    }

    fn entry(name: &str, size: u64, folder: bool) -> Entry {
        Entry {
            id: 0,
            name: name.into(),
            size_bytes: size,
            is_folder: folder,
            modified: None,
        }
    }

    #[test]
    fn kinds_and_sizes_have_their_own_groups_with_folders_first() {
        let today = day(2026, 10, 18);
        let kind = |e: &Entry| bucket(GroupBy::Kind, e, today).1;
        assert_eq!(kind(&entry("DCIM", 0, true)), "Folders");
        assert_eq!(kind(&entry("a.JPG", 1, false)), "Photos");
        assert_eq!(kind(&entry("b.mp4", 1, false)), "Videos");
        assert_eq!(kind(&entry("c.bin", 1, false)), "Other");
        let size = |s: u64| bucket(GroupBy::Size, &entry("f", s, false), today).1;
        assert_eq!(size(u64::from(u32::MAX)), "1 GB or More");
        assert_eq!(size(2_000_000_000), "1 GB or More");
        assert_eq!(size(100_000_000), "100 MB to 1 GB");
        assert_eq!(size(99_999_999), "10 to 100 MB");
        assert_eq!(size(1_000_000), "1 to 10 MB");
        assert_eq!(size(100_000), "100 KB to 1 MB");
        assert_eq!(size(99_999), "Under 100 KB");
        assert_eq!(size(0), "Empty");
        assert_eq!(
            bucket(GroupBy::Size, &entry("D", 0, true), today).1,
            "Folders"
        );
    }

    /// Three groups: rows 0..2, 2..7, 7..8.
    fn three() -> Vec<Group> {
        let keys: Vec<(u32, String)> = [2, 1, 2, 1, 1, 3, 1, 1]
            .iter()
            .map(|&rank| (rank, format!("g{rank}")))
            .collect();
        let (order, groups) = arrange(&keys, &[10; 8]);
        // Rank 1 first (rows 1, 3, 4, 6, 7 in their order), then 2, then 3.
        assert_eq!(order, [1, 3, 4, 6, 7, 0, 2, 5]);
        groups
    }

    #[test]
    fn rows_are_put_in_group_order_keeping_their_own_order_inside() {
        let groups = three();
        let starts: Vec<(usize, usize, &str)> = groups
            .iter()
            .map(|g| (g.start, g.len, g.label.as_str()))
            .collect();
        assert_eq!(starts, [(0, 5, "g1"), (5, 2, "g2"), (7, 1, "g3")]);
        assert_eq!(groups[0].bytes, 50);
        assert_eq!(groups[0].summary(), "5 items, 50 B");
        assert_eq!(
            Group {
                label: String::new(),
                start: 0,
                len: 1,
                bytes: 0
            }
            .summary(),
            "1 item"
        );
    }

    #[test]
    fn list_lines_and_rows_map_both_ways_round_headers() {
        let groups = three();
        assert_eq!(list_lines(&groups, 8), 11);
        let drawn: Vec<Line> = (0..11).map(|line| list_line(&groups, line)).collect();
        assert_eq!(
            drawn,
            [
                Line::Header(0),
                Line::Row(0),
                Line::Row(1),
                Line::Row(2),
                Line::Row(3),
                Line::Row(4),
                Line::Header(1),
                Line::Row(5),
                Line::Row(6),
                Line::Header(2),
                Line::Row(7),
            ]
        );
        for row in 0..8 {
            assert_eq!(
                list_line(&groups, list_line_of(&groups, row)),
                Line::Row(row)
            );
        }
        assert_eq!(group_of(&groups, 6), 1);
    }

    #[test]
    fn a_grid_starts_each_group_on_a_new_line_under_its_header() {
        let groups = three();
        let (starts, lines) = grid_starts(&groups, 2);
        // 1 + 3 lines, 1 + 1, 1 + 1.
        assert_eq!((starts.as_slice(), lines), (&[0, 4, 6][..], 8));
        let drawn: Vec<GridLine> = (0..8)
            .map(|line| grid_line(&groups, 2, &starts, line))
            .collect();
        assert_eq!(
            drawn,
            [
                GridLine::Header(0),
                GridLine::Tiles { first: 0, end: 2 },
                GridLine::Tiles { first: 2, end: 4 },
                GridLine::Tiles { first: 4, end: 5 },
                GridLine::Header(1),
                GridLine::Tiles { first: 5, end: 7 },
                GridLine::Header(2),
                GridLine::Tiles { first: 7, end: 8 },
            ]
        );
        assert_eq!(grid_place(&groups, 2, &starts, 4), (3, 0));
        assert_eq!(grid_place(&groups, 2, &starts, 6), (5, 1));
        assert_eq!(grid_place(&groups, 2, &starts, 7), (7, 0));
    }

    #[test]
    fn a_grouped_grid_places_short_headers_and_tall_tile_lines() {
        let groups = three();
        let frame = GridFrame::new(&groups, 2, 150.0, 30.0);
        // Lines: H, T, T, T, H, T, H, T.
        assert_eq!(frame.top(0), 0.0);
        assert_eq!(frame.top(1), 30.0);
        assert_eq!(frame.top(4), 480.0);
        assert_eq!(frame.top(5), 510.0);
        assert_eq!(frame.total(), 3.0 * 30.0 + 5.0 * 150.0);
        assert_eq!((frame.height(4), frame.height(5)), (30.0, 150.0));
        assert_eq!(frame.line_at(0.0), 0);
        assert_eq!(frame.line_at(29.9), 0);
        assert_eq!(frame.line_at(30.0), 1);
        assert_eq!(frame.line_at(495.0), 4);
        assert_eq!(frame.line_at(10_000.0), 7);
        assert_eq!(frame.window(500.0, 100.0), 2..8);
        assert_eq!(GridFrame::new(&[], 2, 150.0, 30.0).window(0.0, 100.0), 0..0);
    }

    #[test]
    fn mapping_costs_nothing_per_row_on_a_big_folder() {
        let keys: Vec<(u32, String)> = (0..5_000)
            .map(|i| (i % 40, format!("g{}", i % 40)))
            .collect();
        let (_, groups) = arrange(&keys, &[1; 5_000]);
        let started = std::time::Instant::now();
        for line in 0..list_lines(&groups, 5_000) {
            let _ = list_line(&groups, line);
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }
}
