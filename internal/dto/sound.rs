/// What a sound says when the transfer queue empties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Everything went.
    Done,
    /// Something failed or was left behind.
    Problem,
}
