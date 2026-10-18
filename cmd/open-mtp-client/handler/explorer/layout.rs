//! What fits in a pane of a given width: low-priority columns step aside
//! before anything gets clipped. The columns are as wide as the user made
//! them (`dto::ColumnWidths`); Name takes what is left, never less than
//! `NAME_LEAST`.

use internal::dto::ColumnWidths;

/// Which optional parts of the list view are shown, and how wide each
/// column is, in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub kind: bool,
    pub size: bool,
    /// "Folders first" and "Hidden" in the footer; they are also in Settings.
    pub footer_toggles: bool,
    pub kind_width: u16,
    pub size_width: u16,
    pub modified_width: u16,
    /// The pane's width this was worked out for.
    pub width: u16,
}

/// The narrowest pane whose path row also holds the filter and its date and
/// kind menus; a narrower one gives them a row of their own, so the path, the
/// field and the menus stay readable.
pub const FILTERS_INLINE: u16 = 720;

/// Whether the filter and its menus share the path row at `width`.
pub fn filters_inline(width: u16) -> bool {
    width >= FILTERS_INLINE
}

/// The narrowest Name is let get by the other columns.
pub const NAME_LEAST: u16 = 140;
/// The narrowest Modified is squeezed to in a very narrow pane.
const MODIFIED_LEAST: u16 = 60;
/// Space between columns, and the row's padding either side.
pub const GAP: u16 = 8;
const EDGES: u16 = 20;

/// Name and Modified always show; Kind goes first, then Size, whenever
/// Name would otherwise get narrower than `NAME_LEAST`.
pub fn for_width(width: f32, columns: ColumnWidths) -> Fit {
    let width = width.max(0.0) as u32;
    let needs = |kind: bool, size: bool| {
        let mut needed = u32::from(NAME_LEAST + columns.modified + GAP + EDGES);
        if kind {
            needed += u32::from(columns.kind + GAP);
        }
        if size {
            needed += u32::from(columns.size + GAP);
        }
        needed
    };
    // Kind needs more room than Size alone, so a pane with room for Kind
    // has room for Size.
    let size = width >= needs(false, true);
    let kind = width >= needs(true, true);
    let room = width.saturating_sub(u32::from(NAME_LEAST + GAP + EDGES));
    Fit {
        kind,
        size,
        footer_toggles: width >= 520,
        kind_width: columns.kind,
        size_width: columns.size,
        modified_width: (u32::from(columns.modified).min(room) as u16).max(MODIFIED_LEAST),
        width: width.min(u32::from(u16::MAX)) as u16,
    }
}

/// A column edge that can be dragged: the left edge of that column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Kind,
    Size,
    Modified,
}

/// The widths after dragging `edge` by `dx` points (left is negative), from
/// `start`, in a list `width` wide showing what `fit` shows. The column right
/// of the edge grows as the edge moves left; the one left of it gives that
/// room, or, when that is Name, Name does (never below `NAME_LEAST`). Every
/// column stays within `ColumnWidths::LEAST`..`MOST`.
pub fn resized(start: ColumnWidths, edge: Edge, dx: f32, width: f32, fit: Fit) -> ColumnWidths {
    let dx = dx.round() as i32;
    let bound = |value: i32| {
        value.clamp(
            i32::from(ColumnWidths::LEAST),
            i32::from(ColumnWidths::MOST),
        )
    };
    let mut widths = start;
    // The column left of the edge, when it isn't Name.
    let left = match edge {
        Edge::Kind => None,
        Edge::Size => fit.kind.then_some(Edge::Kind),
        Edge::Modified => {
            if fit.size {
                Some(Edge::Size)
            } else if fit.kind {
                Some(Edge::Kind)
            } else {
                None
            }
        }
    };
    let get = |widths: &ColumnWidths, edge: Edge| match edge {
        Edge::Kind => widths.kind,
        Edge::Size => widths.size,
        Edge::Modified => widths.modified,
    };
    let set = |widths: &mut ColumnWidths, edge: Edge, value: i32| {
        let value = bound(value) as u16;
        match edge {
            Edge::Kind => widths.kind = value,
            Edge::Size => widths.size = value,
            Edge::Modified => widths.modified = value,
        }
    };
    let right_start = i32::from(get(&start, edge));
    match left {
        Some(left) => {
            // Two columns trade room; their sum stays as it was.
            let left_start = i32::from(get(&start, left));
            let least = i32::from(ColumnWidths::LEAST);
            let most = i32::from(ColumnWidths::MOST);
            let grow = (-dx)
                .min(left_start - least)
                .min(most - right_start)
                .max(least - right_start)
                .max(left_start - most);
            set(&mut widths, edge, right_start + grow);
            set(&mut widths, left, left_start - grow);
        }
        None => {
            // Name gives the room, down to its least.
            let shown = |on: bool, value: u16| if on { i32::from(value + GAP) } else { 0 };
            let others = shown(fit.kind && edge != Edge::Kind, start.kind)
                + shown(fit.size && edge != Edge::Size, start.size)
                + if edge == Edge::Modified {
                    0
                } else {
                    i32::from(start.modified + GAP)
                };
            let room = width as i32 - i32::from(NAME_LEAST + GAP + EDGES) - others;
            set(
                &mut widths,
                edge,
                (right_start - dx).min(room.max(i32::from(ColumnWidths::LEAST))),
            );
        }
    }
    widths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usual() -> ColumnWidths {
        ColumnWidths::default()
    }

    #[test]
    fn a_wide_pane_shows_everything_at_the_widths_given() {
        let fit = for_width(800.0, usual());
        assert!(fit.kind && fit.size && fit.footer_toggles);
        assert_eq!(
            (fit.kind_width, fit.size_width, fit.modified_width),
            (120, 90, 170)
        );
    }

    #[test]
    fn kind_goes_first_then_size_as_name_runs_out_of_room() {
        // Name 140 + Modified 170 + Kind 128 + Size 98 + 28: 564.
        assert!(for_width(564.0, usual()).kind);
        let narrower = for_width(563.0, usual());
        assert!(!narrower.kind && narrower.size && narrower.footer_toggles);
        // Without Kind: 436.
        assert!(for_width(436.0, usual()).size);
        let narrow = for_width(435.0, usual());
        assert!(!narrow.kind && !narrow.size && !narrow.footer_toggles);
        let tiny = for_width(200.0, usual());
        assert_eq!(tiny.modified_width, 60, "squeezed, but there");
    }

    #[test]
    fn wider_columns_step_aside_sooner() {
        let wide = ColumnWidths {
            kind: 300,
            ..usual()
        };
        assert!(!for_width(700.0, wide).kind);
        assert!(for_width(700.0, usual()).kind);
    }

    #[test]
    fn the_first_edge_takes_its_room_from_name_down_to_its_least() {
        let fit = for_width(800.0, usual());

        let wider = resized(usual(), Edge::Kind, -30.0, 800.0, fit);
        assert_eq!(wider.kind, 150, "dragged left: Kind grows");
        let narrower = resized(usual(), Edge::Kind, 100.0, 800.0, fit);
        assert_eq!(narrower.kind, ColumnWidths::LEAST);
        // 800 - Name 140 - 28 - Size 98 - Modified 178: Kind gets at most 356.
        let most = resized(usual(), Edge::Kind, -1_000.0, 800.0, fit);
        assert_eq!(most.kind, 356);
        assert_eq!(
            (most.size, most.modified),
            (90, 170),
            "the others as they were"
        );
    }

    #[test]
    fn an_edge_between_two_columns_trades_room_between_them() {
        let fit = for_width(800.0, usual());

        let traded = resized(usual(), Edge::Size, -20.0, 800.0, fit);
        assert_eq!((traded.kind, traded.size), (100, 110));
        let at_least = resized(usual(), Edge::Size, -500.0, 800.0, fit);
        assert_eq!(
            (at_least.kind, at_least.size),
            (ColumnWidths::LEAST, 160),
            "Kind stops at its least, the sum kept"
        );
        let modified = resized(usual(), Edge::Modified, 30.0, 800.0, fit);
        assert_eq!((modified.size, modified.modified), (120, 140));
    }

    #[test]
    fn with_kind_and_size_hidden_modified_trades_with_name() {
        let fit = for_width(420.0, usual());
        assert!(!fit.kind && !fit.size);

        let wider = resized(usual(), Edge::Modified, -10.0, 420.0, fit);
        assert_eq!(wider.modified, 180);
        assert_eq!((wider.kind, wider.size), (120, 90));
        // 420 - Name 140 - 28: Modified gets at most 252.
        assert_eq!(
            resized(usual(), Edge::Modified, -500.0, 420.0, fit).modified,
            252
        );
    }
}
