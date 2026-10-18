/// An app that can be holding the phone's USB connection, so that nothing
/// else can open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// What the user knows it as, e.g. "Photos".
    pub name: String,
    /// macOS starts it again by itself, so quitting it may need repeating.
    pub restarts: bool,
}

/// What is known about the Mac, for a support report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacInfo {
    /// "15.1", or empty when it could not be read.
    pub os_version: String,
    /// "aarch64" or "x86_64": what the app is running as.
    pub arch: String,
    /// Running as x86_64 on Apple Silicon, translated by Rosetta (someone
    /// ticked "Open using Rosetta"): slower than it needs to be.
    pub translated: bool,
    /// The crash reports kept, newest first: when and where in the code,
    /// never what was in them.
    pub crashes: Vec<super::CrashNote>,
}

/// What was found out about why the phone would not connect. `None` means the
/// check does not apply to this failure (an app holding the phone does not
/// explain "no device", and a USB scan does not explain a locked phone).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnosis {
    /// Known apps that are running and may be holding the phone, most likely
    /// culprit first.
    pub holders: Option<Vec<Holder>>,
    /// Names of USB devices that look like phones. A phone that shows up here
    /// but is not found as an MTP device is plugged in without offering file
    /// transfer.
    pub phones: Option<Vec<String>>,
    /// The holders are what macOS itself records as having the phone open
    /// for itself alone (its USB registry), not apps that merely run.
    pub owner_seen: bool,
    /// Programs macOS says have the phone that are not known to the app (so
    /// it can't offer to quit them), by their process name.
    pub unknown_owners: Vec<String>,
}
