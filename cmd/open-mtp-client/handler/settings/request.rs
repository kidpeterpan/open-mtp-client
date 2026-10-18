use internal::dto::{Density, ThemeChoice};

/// What the user changed in the Settings dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Open,
    Close,
    SetTheme(ThemeChoice),
    SetShowHiddenOnMac(bool),
    SetShowHiddenOnPhone(bool),
    SetFoldersFirst(bool),
    SetStatusBar(bool),
    SetShowMacPane(bool),
    SetMacPaneOnLeft(bool),
    SetAutoConnect(bool),
    SetFinishSound(bool),
    /// A phone delete keeps a copy in the Mac's Trash first.
    SetKeepDeleted(bool),
    SetDensity(Density),
}
