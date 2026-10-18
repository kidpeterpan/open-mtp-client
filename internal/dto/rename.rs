use super::NameProblem;

/// How several selected items get new names at once, as Finder's Rename
/// does. A file's extension is kept as it is; a folder's whole name is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenamePattern {
    /// Every `find` in the name becomes `with` (an empty `find` changes
    /// nothing).
    Replace { find: String, with: String },
    /// `text` goes before the name, or after it (before the extension).
    Add { text: String, at_end: bool },
    /// One name and a number for all, in the order shown: "Trip 001.jpg",
    /// "Trip 002.jpg"…
    Number { base: String, start: u32 },
}

/// One selected item and the name it would get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renaming {
    pub from: String,
    pub to: String,
    /// Why it can't be renamed so; `None` when it can (or keeps its name).
    pub problem: Option<RenameProblem>,
}

impl Renaming {
    /// It would get another name, and can.
    pub fn changes(&self) -> bool {
        self.problem.is_none() && self.from != self.to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameProblem {
    /// The new name isn't one a file can have.
    Invalid(NameProblem),
    /// Something else in the folder has that name (ignoring case).
    Taken,
    /// Another selected item would get the same name.
    Twice,
}
