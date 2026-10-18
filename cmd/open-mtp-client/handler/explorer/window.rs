//! Which rows of a long, equal-height list are worth building. A folder can
//! hold tens of thousands of files, and a widget per file made every redraw
//! (every hover, scroll tick and arriving thumbnail) cost time proportional to
//! the folder. Only the rows near the screen are built; spacers stand in for
//! the rest so the scrollbar and scroll positions stay exact.

use std::ops::Range;

/// Before the list has reported its size.
const DEFAULT_HEIGHT: f32 = 700.0;
/// Rows built beyond each edge of the screen, so a fast scroll doesn't show
/// gaps while the next window is being worked out.
pub const OVERSCAN: usize = 6;

/// Indices of the rows to build when `count` rows of `row_height` pixels are
/// shown through a view `height` tall, scrolled `offset` down.
pub fn rows(offset: f32, height: f32, row_height: f32, count: usize) -> Range<usize> {
    let height = if height > 0.0 { height } else { DEFAULT_HEIGHT };
    let offset = offset.max(0.0);
    let first = (offset / row_height).floor() as usize;
    let last = ((offset + height) / row_height).ceil() as usize;
    let start = first.saturating_sub(OVERSCAN).min(count);
    let end = (last + OVERSCAN).min(count);
    start..end
}

/// The rows a box from `top` to `bottom` (list coordinates, either way
/// round) touches. A box of no height touches the row it is on; one below
/// the last row touches nothing.
pub fn rows_between(top: f32, bottom: f32, row_height: f32, count: usize) -> Range<usize> {
    let (top, bottom) = (top.min(bottom), top.max(bottom));
    if bottom < 0.0 || count == 0 {
        return 0..0;
    }
    let first = ((top.max(0.0) / row_height).floor() as usize).min(count);
    let last = ((bottom / row_height).floor() as usize + 1).min(count);
    first..last.max(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_touches_the_rows_it_spans_and_no_more() {
        // 20 px rows.
        assert_eq!(rows_between(5.0, 5.0, 20.0, 10), 0..1, "a point is its row");
        assert_eq!(rows_between(45.0, 15.0, 20.0, 10), 0..3, "drawn upwards");
        assert_eq!(
            rows_between(-30.0, 25.0, 20.0, 10),
            0..2,
            "from above the list"
        );
        assert_eq!(rows_between(150.0, 900.0, 20.0, 10), 7..10, "past the end");
        assert_eq!(
            rows_between(250.0, 300.0, 20.0, 10),
            10..10,
            "below every row"
        );
        assert_eq!(
            rows_between(-50.0, -10.0, 20.0, 10),
            0..0,
            "above every row"
        );
        assert_eq!(rows_between(0.0, 10.0, 20.0, 0), 0..0, "an empty folder");
    }

    #[test]
    fn the_top_of_a_long_list_builds_the_screen_plus_overscan() {
        // 32 px rows, 320 px visible: rows 0–9 on screen.
        assert_eq!(rows(0.0, 320.0, 32.0, 10_000), 0..10 + OVERSCAN);
    }

    #[test]
    fn scrolled_down_builds_a_window_around_the_screen() {
        // 3,200 px down is row 100; 320 px shows rows 100–109.
        assert_eq!(
            rows(3_200.0, 320.0, 32.0, 10_000),
            100 - OVERSCAN..110 + OVERSCAN
        );
    }

    #[test]
    fn a_partly_visible_row_is_built() {
        // Starting half way into row 3 and ending half way into row 13.
        assert_eq!(
            rows(3.0 * 32.0 + 16.0, 320.0, 32.0, 10_000),
            3 - 3.min(OVERSCAN)..14 + OVERSCAN
        );
    }

    #[test]
    fn the_window_never_passes_either_end() {
        assert_eq!(rows(0.0, 10_000.0, 32.0, 7), 0..7);
        assert_eq!(rows(1_000_000.0, 320.0, 32.0, 7), 7..7);
        assert_eq!(rows(0.0, 320.0, 32.0, 0), 0..0);
    }

    #[test]
    fn an_unknown_height_assumes_a_typical_pane() {
        let range = rows(0.0, 0.0, 32.0, 10_000);
        assert_eq!(range, 0..(700.0_f32 / 32.0).ceil() as usize + OVERSCAN);
    }

    #[test]
    fn a_bounce_above_the_top_counts_as_the_top() {
        assert_eq!(
            rows(-40.0, 320.0, 32.0, 10_000),
            rows(0.0, 320.0, 32.0, 10_000)
        );
    }

    #[test]
    fn however_long_the_folder_the_window_stays_small() {
        for count in [100, 10_000, 1_000_000] {
            let window = rows(32.0 * (count / 2) as f32, 700.0, 32.0, count);
            assert!(
                window.len() < 50,
                "{count} files built {} rows",
                window.len()
            );
        }
    }
}
