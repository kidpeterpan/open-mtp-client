/// A page of the Help dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Shortcuts,
    Troubleshooting,
    About,
    WhatsNew,
}

/// A web page Help can open. Fixed here, never taken from anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Where to report a problem.
    NewIssue,
    /// The project's home.
    Project,
    /// Where to buy the author a coffee.
    Support,
}

/// What the user asked of the Help dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open,
    Close,
    Show(Tab),
    /// Look at the Mac and the USB bus again.
    CheckAgain,
    /// Read the phone pane's folder from the phone and time it.
    TimeReading,
    /// Quit the apps the check found in the way, then check again.
    QuitHolders,
    /// Put the diagnostics report on the clipboard.
    CopyReport,
    /// Put the line that links the bundled command-line tool into the
    /// user's path on the clipboard.
    CopyCliLine,
    OpenLink(Link),
}
