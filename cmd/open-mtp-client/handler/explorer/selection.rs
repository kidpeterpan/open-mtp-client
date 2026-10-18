//! Which rows are selected, Finder-style: a set of names, an anchor that
//! shift-ranges grow from, and a cursor that the arrow keys move.

/// How a click on a row changes the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Plain click: just this row.
    Only,
    /// ⌘-click: add or remove this row.
    Toggle,
    /// ⇧-click: everything from the anchor to this row.
    Range,
}

/// How a lasso combines with what was selected when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// Plain: just what the box touches.
    Replace,
    /// ⌘: what the box touches changes over, the rest stays.
    Toggle,
    /// ⇧: what the box touches is added.
    Add,
}

impl Sweep {
    pub fn from_keys(command: bool, shift: bool) -> Self {
        if command {
            Sweep::Toggle
        } else if shift {
            Sweep::Add
        } else {
            Sweep::Replace
        }
    }
}

/// What a lasso leaves selected, in row order: each row is selected if the
/// box touches it, combined with whether it was selected at the start
/// (`base`) as `sweep` says. One pass over the rows.
pub fn swept<'a>(
    rows: impl IntoIterator<Item = &'a str>,
    touched: impl Fn(usize) -> bool,
    base: &std::collections::HashSet<String>,
    sweep: Sweep,
) -> Vec<String> {
    rows.into_iter()
        .enumerate()
        .filter(|(index, name)| {
            let inside = touched(*index);
            let was = base.contains(*name);
            match sweep {
                Sweep::Replace => inside,
                Sweep::Toggle => inside != was,
                Sweep::Add => inside || was,
            }
        })
        .map(|(_, name)| name.to_owned())
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Selected names, in row order.
    names: Vec<String>,
    anchor: Option<String>,
    cursor: Option<String>,
}

impl Selection {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|selected| selected == name)
    }

    /// The single selected name, for actions that take exactly one.
    pub fn single(&self) -> Option<&str> {
        match self.names.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Selects only `name`.
    pub fn only(&mut self, name: &str) {
        self.names = vec![name.to_owned()];
        self.anchor = Some(name.to_owned());
        self.cursor = Some(name.to_owned());
    }

    /// Selects exactly `names`, e.g. what a paste just brought in. The
    /// anchor and cursor are the first and last, so arrow keys carry on from
    /// where the new items end.
    pub fn set(&mut self, names: &[String]) {
        self.names = names.to_vec();
        self.anchor = names.first().cloned();
        self.cursor = names.last().cloned();
    }

    pub fn click(&mut self, rows: &[String], name: &str, click: Click) {
        match click {
            Click::Only => self.only(name),
            Click::Toggle => {
                if self.contains(name) {
                    self.names.retain(|selected| selected != name);
                } else {
                    self.names.push(name.to_owned());
                    self.sort_like(rows);
                }
                self.anchor = Some(name.to_owned());
                self.cursor = Some(name.to_owned());
            }
            Click::Range => {
                let anchor = self.anchor.clone().unwrap_or_else(|| name.to_owned());
                self.range(rows, &anchor, name);
                self.anchor = Some(anchor);
                self.cursor = Some(name.to_owned());
            }
        }
    }

    pub fn all(&mut self, rows: &[String]) {
        self.names = rows.to_vec();
        if self.cursor.is_none() {
            self.cursor = rows.first().cloned();
            self.anchor = self.cursor.clone();
        }
    }

    /// Moves the cursor `delta` rows (clamped to the list); with `extend`
    /// the selection grows from the anchor, otherwise it follows the cursor.
    /// With no cursor yet, down starts at the first row and up at the last.
    pub fn step(&mut self, rows: &[String], delta: isize, extend: bool) {
        if rows.is_empty() {
            return;
        }
        let index = match self.cursor_index(rows) {
            Some(index) => index.saturating_add_signed(delta),
            None if delta < 0 => rows.len() - 1,
            None => 0,
        };
        self.go_to(rows, index, extend);
    }

    /// Moves the cursor to row `index` (clamped to the list); with `extend`
    /// the selection grows from the anchor, otherwise it is that row alone.
    pub fn go_to(&mut self, rows: &[String], index: usize, extend: bool) {
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let name = rows[index.min(last)].clone();
        if extend {
            let anchor = self.anchor.clone().unwrap_or_else(|| name.clone());
            self.range(rows, &anchor, &name);
            self.anchor = Some(anchor);
            self.cursor = Some(name);
        } else {
            self.only(&name);
        }
    }

    pub fn cursor_index(&self, rows: &[String]) -> Option<usize> {
        let cursor = self.cursor.as_deref()?;
        rows.iter().position(|row| row == cursor)
    }

    /// Drops names that are no longer listed, e.g. after a refresh.
    pub fn retain(&mut self, rows: &[String]) {
        self.names.retain(|name| rows.contains(name));
        self.sort_like(rows);
        if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| !rows.contains(anchor))
        {
            self.anchor = None;
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| !rows.contains(cursor))
        {
            self.cursor = None;
        }
    }

    /// Keeps the selected names in row order after the rows are re-sorted.
    pub fn sort_like(&mut self, rows: &[String]) {
        self.names
            .sort_by_key(|name| rows.iter().position(|row| row == name));
    }

    fn range(&mut self, rows: &[String], from: &str, to: &str) {
        let (Some(a), Some(b)) = (
            rows.iter().position(|row| row == from),
            rows.iter().position(|row| row == to),
        ) else {
            self.only(to);
            return;
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        self.names = rows[start..=end].to_vec();
    }
}

/// How far to scroll so row `index` is fully visible, or `None` if it
/// already is. Rows are `row_height` tall; the view shows `height` pixels
/// starting `offset` pixels down.
pub fn scroll_to_show(index: usize, row_height: f32, offset: f32, height: f32) -> Option<f32> {
    let top = index as f32 * row_height;
    let bottom = top + row_height;
    if top < offset {
        Some(top)
    } else if bottom > offset + height {
        Some(bottom - height)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn going_to_a_row_selects_it_or_grows_from_the_anchor_and_stays_in_the_list() {
        let rows = rows();
        let mut selection = Selection::default();
        selection.go_to(&[], 3, false);
        assert!(selection.is_empty(), "no rows, nothing");

        selection.go_to(&rows, usize::MAX, false);
        assert_eq!(selection.names(), ["e"], "past the end is the last");
        selection.only("b");
        selection.go_to(&rows, 3, true);
        assert_eq!(selection.names(), ["b", "c", "d"]);
        assert_eq!(selection.cursor(), Some("d"));
        selection.go_to(&rows, 0, true);
        assert_eq!(selection.names(), ["a", "b"], "the anchor stays");
        selection.go_to(&rows, 2, false);
        assert_eq!(selection.names(), ["c"]);
    }

    #[test]
    fn a_lasso_replaces_toggles_or_adds_and_keeps_row_order() {
        let rows = ["a", "b", "c", "d", "e"];
        let base: std::collections::HashSet<String> =
            ["a", "c"].iter().map(|name| (*name).to_owned()).collect();
        let touched = |index: usize| (1..4).contains(&index);

        assert_eq!(swept(rows, touched, &base, Sweep::Replace), ["b", "c", "d"]);
        assert_eq!(swept(rows, touched, &base, Sweep::Toggle), ["a", "b", "d"]);
        assert_eq!(
            swept(rows, touched, &base, Sweep::Add),
            ["a", "b", "c", "d"]
        );
        assert_eq!(Sweep::from_keys(true, true), Sweep::Toggle, "⌘ wins");
        assert_eq!(Sweep::from_keys(false, true), Sweep::Add);
        assert_eq!(Sweep::from_keys(false, false), Sweep::Replace);
    }

    fn rows() -> Vec<String> {
        ["a", "b", "c", "d", "e"].map(String::from).to_vec()
    }

    fn names(selection: &Selection) -> Vec<&str> {
        selection.names().iter().map(String::as_str).collect()
    }

    #[test]
    fn a_plain_click_selects_only_that_row() {
        let mut selection = Selection::default();
        selection.click(&rows(), "b", Click::Only);
        selection.click(&rows(), "d", Click::Only);

        assert_eq!(names(&selection), ["d"]);
        assert_eq!(selection.single(), Some("d"));
        assert_eq!(selection.cursor(), Some("d"));
    }

    #[test]
    fn command_click_adds_and_removes_in_row_order() {
        let mut selection = Selection::default();
        selection.click(&rows(), "d", Click::Only);
        selection.click(&rows(), "b", Click::Toggle);
        assert_eq!(names(&selection), ["b", "d"]);
        assert_eq!(selection.single(), None);

        selection.click(&rows(), "d", Click::Toggle);
        assert_eq!(names(&selection), ["b"]);
    }

    #[test]
    fn shift_click_selects_the_range_from_the_anchor_either_way() {
        let mut selection = Selection::default();
        selection.click(&rows(), "b", Click::Only);

        selection.click(&rows(), "d", Click::Range);
        assert_eq!(names(&selection), ["b", "c", "d"]);

        selection.click(&rows(), "a", Click::Range);
        assert_eq!(names(&selection), ["a", "b"]);
    }

    #[test]
    fn shift_click_without_an_anchor_selects_just_that_row() {
        let mut selection = Selection::default();

        selection.click(&rows(), "c", Click::Range);

        assert_eq!(names(&selection), ["c"]);
    }

    #[test]
    fn select_all_takes_every_row() {
        let mut selection = Selection::default();

        selection.all(&rows());

        assert_eq!(selection.len(), 5);
        assert_eq!(selection.cursor(), Some("a"));
    }

    #[test]
    fn arrows_start_at_the_ends_then_move_and_clamp() {
        let mut down = Selection::default();
        down.step(&rows(), 1, false);
        assert_eq!(names(&down), ["a"]);
        down.step(&rows(), 1, false);
        assert_eq!(names(&down), ["b"]);
        down.step(&rows(), 10, false);
        assert_eq!(names(&down), ["e"]);

        let mut up = Selection::default();
        up.step(&rows(), -1, false);
        assert_eq!(names(&up), ["e"]);
        up.step(&rows(), -10, false);
        assert_eq!(names(&up), ["a"]);
    }

    #[test]
    fn shift_arrows_grow_and_shrink_from_the_anchor() {
        let mut selection = Selection::default();
        selection.click(&rows(), "c", Click::Only);

        selection.step(&rows(), 1, true);
        selection.step(&rows(), 1, true);
        assert_eq!(names(&selection), ["c", "d", "e"]);

        selection.step(&rows(), -3, true);
        assert_eq!(names(&selection), ["b", "c"]);
    }

    #[test]
    fn stepping_through_nothing_does_nothing() {
        let mut selection = Selection::default();

        selection.step(&[], 1, false);

        assert!(selection.is_empty());
    }

    #[test]
    fn retain_drops_vanished_names_and_their_anchor() {
        let mut selection = Selection::default();
        selection.click(&rows(), "b", Click::Only);
        selection.click(&rows(), "d", Click::Toggle);

        selection.retain(&["a".into(), "b".into()]);

        assert_eq!(names(&selection), ["b"]);
        assert_eq!(selection.cursor(), None);
    }

    #[test]
    fn a_range_to_an_unlisted_row_falls_back_to_that_row() {
        let mut selection = Selection::default();
        selection.click(&rows(), "b", Click::Only);

        selection.click(&rows(), "z", Click::Range);

        assert_eq!(names(&selection), ["z"]);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut selection = Selection::default();
        selection.all(&rows());

        selection.clear();

        assert_eq!(selection, Selection::default());
    }

    #[test]
    fn scrolling_shows_rows_above_and_below_the_view() {
        // 30 px rows, view shows 100 px starting at 60 (rows 2..5 visible).
        assert_eq!(scroll_to_show(3, 30.0, 60.0, 100.0), None);
        assert_eq!(scroll_to_show(1, 30.0, 60.0, 100.0), Some(30.0));
        assert_eq!(scroll_to_show(6, 30.0, 60.0, 100.0), Some(110.0));
    }

    #[test]
    fn set_selects_exactly_those_names_and_carries_on_from_the_last() {
        let rows: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        let mut selection = Selection::default();
        selection.only("a");

        selection.set(&["c".to_owned(), "d".to_owned()]);

        assert_eq!(selection.names(), ["c", "d"]);
        assert_eq!(selection.cursor(), Some("d"));
        // ⇧↑ from there grows the selection from the first.
        selection.step(&rows, -1, true);
        assert_eq!(selection.names(), ["c"]);

        selection.set(&[]);
        assert!(selection.is_empty());
        assert_eq!(selection.cursor(), None);
    }
}
