//! A pane's back and forward, as a browser's or Finder's: the folders it
//! came through, and those it stepped back from. Pure: the pane says where
//! it went, this says where back and forward go.

/// How many folders each way are kept; older ones fall off.
pub const HISTORY_MOST: usize = 50;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct History {
    back: Vec<String>,
    forward: Vec<String>,
}

impl History {
    /// The pane went from `from` to `to` by itself (a folder opened, Up, a
    /// crumb, a pin): `from` is where Back returns, and what was ahead is
    /// gone, as in a browser. Staying in the same folder changes nothing.
    pub fn went(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        push_capped(&mut self.back, from);
        self.forward.clear();
    }

    /// Where Back goes from `current`, which Forward then returns to.
    pub fn back(&mut self, current: &str) -> Option<String> {
        let to = self.back.pop()?;
        push_capped(&mut self.forward, current);
        Some(to)
    }

    /// Where Forward goes from `current`, which Back then returns to.
    pub fn forward(&mut self, current: &str) -> Option<String> {
        let to = self.forward.pop()?;
        push_capped(&mut self.back, current);
        Some(to)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Forgets everything: the pane shows another volume now, whose paths
    /// mean other folders.
    pub fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
    }
}

fn push_capped(stack: &mut Vec<String>, path: &str) {
    if stack.len() == HISTORY_MOST {
        stack.remove(0);
    }
    stack.push(path.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward_walk_the_folders_gone_through() {
        let mut history = History::default();
        assert!(!history.can_go_back() && !history.can_go_forward());

        history.went("/", "/DCIM");
        history.went("/DCIM", "/DCIM/Camera");

        assert_eq!(history.back("/DCIM/Camera").as_deref(), Some("/DCIM"));
        assert_eq!(history.back("/DCIM").as_deref(), Some("/"));
        assert_eq!(history.back("/"), None, "nothing before the first");
        assert!(history.can_go_forward());
        assert_eq!(history.forward("/").as_deref(), Some("/DCIM"));
        assert_eq!(history.forward("/DCIM").as_deref(), Some("/DCIM/Camera"));
        assert_eq!(history.forward("/DCIM/Camera"), None);
        assert_eq!(history.back("/DCIM/Camera").as_deref(), Some("/DCIM"));
    }

    #[test]
    fn going_somewhere_new_drops_what_was_ahead() {
        let mut history = History::default();
        history.went("/", "/DCIM");
        let _ = history.back("/DCIM");
        assert!(history.can_go_forward());

        history.went("/", "/Music");

        assert!(!history.can_go_forward());
        assert_eq!(history.back("/Music").as_deref(), Some("/"));
    }

    #[test]
    fn staying_in_the_folder_is_not_a_step() {
        let mut history = History::default();
        history.went("/DCIM", "/DCIM");
        assert!(!history.can_go_back());
    }

    #[test]
    fn only_the_most_recent_folders_are_kept() {
        let mut history = History::default();
        for step in 0..HISTORY_MOST + 10 {
            history.went(&format!("/{step}"), &format!("/{}", step + 1));
        }
        let mut current = format!("/{}", HISTORY_MOST + 10);
        let mut steps = 0;
        while let Some(previous) = history.back(&current) {
            current = previous;
            steps += 1;
        }
        assert_eq!(steps, HISTORY_MOST);
        assert_eq!(current, "/10", "the oldest fell off");
    }

    #[test]
    fn clearing_forgets_both_ways() {
        let mut history = History::default();
        history.went("/", "/DCIM");
        history.went("/DCIM", "/DCIM/Camera");
        let _ = history.back("/DCIM/Camera");

        history.clear();

        assert!(!history.can_go_back() && !history.can_go_forward());
    }
}
