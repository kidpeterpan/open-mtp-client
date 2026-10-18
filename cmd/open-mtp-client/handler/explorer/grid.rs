//! Geometry of the grid view: tiles of one size (the pane's `TileSize`) in
//! as many columns as fit, so which tiles are on screen (and need
//! thumbnails) can be worked out.

use std::ops::Range;

use internal::dto::TileSize;

/// The usual tile's size, 132 points across (what tests measure by).
#[cfg(test)]
pub const TILE_WIDTH: f32 = 132.0;
#[cfg(test)]
pub const TILE_HEIGHT: f32 = 150.0;
pub const SPACING: f32 = 8.0;
/// Room under a tile's picture for its name.
const NAME_ROOM: f32 = 18.0;
/// Before the list has reported its size.
const DEFAULT_WIDTH: f32 = 560.0;
const DEFAULT_HEIGHT: f32 = 700.0;

/// A tile's size: its width, and what follows from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub width: f32,
}

impl Tile {
    pub fn of(size: TileSize) -> Tile {
        Tile {
            width: f32::from(size.points()),
        }
    }

    pub fn height(self) -> f32 {
        self.width + NAME_ROOM
    }

    /// From one row's top to the next's.
    pub fn row_height(self) -> f32 {
        self.height() + SPACING
    }

    /// From one column's left to the next's.
    pub fn pitch(self) -> f32 {
        self.width + SPACING
    }
}

impl Default for Tile {
    fn default() -> Self {
        Tile::of(TileSize::default())
    }
}

pub fn columns(width: f32, tile: Tile) -> usize {
    let width = if width > 0.0 { width } else { DEFAULT_WIDTH };
    (((width + SPACING) / tile.pitch()).floor() as usize).max(1)
}

/// Indices of the tiles on screen when the view shows `height` pixels
/// starting `offset` down, plus one row either side so scrolling finds
/// thumbnails already there.
pub fn visible(offset: f32, height: f32, columns: usize, count: usize, tile: Tile) -> Range<usize> {
    let height = if height > 0.0 { height } else { DEFAULT_HEIGHT };
    let row = tile.row_height();
    let first_row = ((offset / row).floor() as usize).saturating_sub(1);
    let last_row = ((offset + height) / row).ceil() as usize + 1;
    let start = (first_row * columns).min(count);
    let end = (last_row * columns).min(count);
    start..end
}

/// The tiles a box between corners `a` and `b` (grid coordinates) touches,
/// in order. The gaps between tiles belong to none, so a box in a gap
/// touches nothing.
pub fn tiles_touched(
    a: (f32, f32),
    b: (f32, f32),
    columns: usize,
    count: usize,
    tile: Tile,
) -> Vec<usize> {
    let (left, right) = (a.0.min(b.0), a.0.max(b.0));
    let (top, bottom) = (a.1.min(b.1), a.1.max(b.1));
    if right < 0.0 || bottom < 0.0 || columns == 0 {
        return Vec::new();
    }
    let pitch = tile.pitch();
    let row = tile.row_height();
    let first_column = (left.max(0.0) / pitch).floor() as usize;
    let last_column = ((right / pitch).floor() as usize).min(columns - 1);
    let first_row = (top.max(0.0) / row).floor() as usize;
    let last_row = (bottom / row).floor() as usize;
    let mut touched = Vec::new();
    for at_row in first_row..=last_row {
        let tile_top = at_row as f32 * row;
        if bottom < tile_top || top > tile_top + tile.height() {
            continue;
        }
        for column in first_column..=last_column {
            let tile_left = column as f32 * pitch;
            let index = at_row * columns + column;
            if index >= count {
                return touched;
            }
            if right >= tile_left && left <= tile_left + tile.width {
                touched.push(index);
            }
        }
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_touches_the_tiles_it_overlaps_and_not_the_gaps() {
        let pitch = TILE_WIDTH + SPACING;
        // A point on the second tile of the first row.
        assert_eq!(
            tiles_touched(
                (pitch + 5.0, 5.0),
                (pitch + 5.0, 5.0),
                4,
                100,
                Tile::default()
            ),
            [1]
        );
        // A point in the gap between the first two tiles.
        let gap = TILE_WIDTH + SPACING / 2.0;
        assert!(tiles_touched((gap, 5.0), (gap, 5.0), 4, 100, Tile::default()).is_empty());
        // From the middle of tile 1 down and right into the second row.
        assert_eq!(
            tiles_touched(
                (pitch + 5.0, 5.0),
                (2.0 * pitch + 5.0, Tile::default().row_height() + 5.0),
                4,
                100,
                Tile::default()
            ),
            [1, 2, 5, 6]
        );
        // Drawn from below right, beyond the last column, to above the grid.
        assert_eq!(
            tiles_touched(
                (10_000.0, Tile::default().row_height() - 1.0),
                (3.0 * pitch, -50.0),
                4,
                100,
                Tile::default()
            ),
            [3]
        );
        // Only tiles that exist.
        assert_eq!(
            tiles_touched((0.0, 0.0), (10_000.0, 10_000.0), 4, 6, Tile::default()),
            [0, 1, 2, 3, 4, 5]
        );
        assert!(tiles_touched((-20.0, -20.0), (-5.0, -5.0), 4, 6, Tile::default()).is_empty());
    }

    #[test]
    fn as_many_columns_as_fit_and_at_least_one() {
        assert_eq!(columns(140.0 * 4.0 - 8.0, Tile::default()), 4);
        assert_eq!(columns(140.0 * 4.0 - 9.0, Tile::default()), 3);
        assert_eq!(columns(50.0, Tile::default()), 1);
        assert_eq!(columns(0.0, Tile::default()), 4);
    }

    #[test]
    fn the_top_of_the_list_shows_the_first_rows() {
        // 4 columns, 158 px rows, 316 px visible: rows 0–1, plus one below.
        assert_eq!(visible(0.0, 316.0, 4, 100, Tile::default()), 0..12);
    }

    #[test]
    fn scrolled_down_shows_the_rows_in_view_and_one_either_side() {
        // 790–1106 px is exactly rows 5 and 6; with a row either side, 4–7.
        assert_eq!(visible(790.0, 316.0, 4, 100, Tile::default()), 16..32);
    }

    #[test]
    fn the_range_never_passes_the_end() {
        assert_eq!(visible(0.0, 10_000.0, 4, 7, Tile::default()), 0..7);
        assert_eq!(visible(50_000.0, 316.0, 4, 7, Tile::default()), 7..7);
    }

    #[test]
    fn a_bigger_tile_means_fewer_columns_and_taller_rows() {
        let big = Tile::of(TileSize::default().bigger().bigger());
        assert_eq!(big.width, 192.0);
        assert_eq!(big.height(), 210.0);
        assert_eq!(columns(560.0, big), 2);
        assert_eq!(columns(560.0, Tile::default()), 4);
        let small = Tile::of(TileSize::default().smaller().smaller());
        assert_eq!(columns(560.0, small), 5);
        // 316 px of 218 px rows: rows 0–1, plus one below.
        assert_eq!(visible(0.0, 316.0, 2, 100, big), 0..6);
        // 436–752 px is rows 2–3 of 218 px; with a row either side, 1–4.
        assert_eq!(visible(436.0, 316.0, 2, 100, big), 2..10);
        assert_eq!(
            tiles_touched(
                (big.pitch() + 5.0, 5.0),
                (big.pitch() + 5.0, 5.0),
                2,
                10,
                big
            ),
            [1]
        );
        assert_eq!(Tile::default().width, TILE_WIDTH);
        assert_eq!(Tile::default().height(), TILE_HEIGHT);
    }

    #[test]
    fn an_unknown_size_assumes_a_typical_pane() {
        assert_eq!(
            visible(0.0, 0.0, 4, 1_000, Tile::default()),
            0..((700.0_f32 / 158.0).ceil() as usize + 1) * 4
        );
    }
}
