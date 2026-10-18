//! Display-ready view models: every string here is shown as-is.

/// One line of the Shortcuts tab: what it does and the keys that do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutLine {
    pub label: String,
    /// Each way of doing it, as keys: ["⌘↑", "⌘B"].
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutGroup {
    pub title: String,
    pub lines: Vec<ShortcutLine>,
}
