/// What the user asked the device panel to do.
#[derive(Debug, Clone)]
pub enum Request {
    Connect,
    Disconnect,
    SelectStorage(u64),
    /// Open this phone (a `PhoneResponse::key`), closing the one open.
    Open(String),
    /// Quit the apps found holding the phone, then connect again.
    QuitHolders,
    /// Import the open storage's new photos (`app.rs` opens the dialog).
    Import,
    /// Look for what could go from the open storage (`app.rs` opens the
    /// Free Up Space dialog).
    FreeUpSpace,
    /// Show this folder of the open storage in the phone pane (`app.rs`).
    GoToPlace(String),
}
