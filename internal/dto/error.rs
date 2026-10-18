/// Failures a user can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("no MTP device found")]
    NoDevice,
    #[error("more than one MTP device is connected")]
    MultipleDevices,
    /// A switch to a phone that is no longer plugged in; the open phone stays.
    #[error("that phone is no longer plugged in")]
    PhoneUnplugged,
    #[error("another app is holding the device")]
    DeviceInUse,
    #[error("Google's Android File Transfer is holding the device")]
    AndroidFileTransferRunning,
    #[error("the device is locked or has not allowed access to its files")]
    AllowStorageAccess,
    #[error("permission denied while opening the device")]
    PermissionDenied,
    #[error("the device was disconnected")]
    Disconnected,
    #[error("the device did not respond in time")]
    Timeout,
    #[error("no device is connected")]
    NotConnected,
    #[error("folder not found: {0}")]
    PathNotFound(String),
    #[error("no permission to read {0}")]
    FolderAccessDenied(String),
    #[error("invalid name: {0:?}")]
    InvalidName(NameProblem),
    #[error("{0} already exists")]
    NameTaken(String),
    #[error("this storage is read-only")]
    ReadOnly,
    #[error("the device does not support this")]
    Unsupported,
    #[error("{0} cannot go inside itself")]
    IntoItself(String),
    #[error("cancelled")]
    Cancelled,
    #[error("the device reported an unsafe file name: {0:?}")]
    UnsafeName(String),
    #[error("too many items to copy at once")]
    TransferTooLarge,
    /// Editing on the Mac takes one file the phone gives a size for, up to
    /// `PREVIEW_LARGEST`.
    #[error("this can't be edited on the Mac")]
    NotEditable,
    /// The phone open now isn't the one this work is for (a file edited on
    /// the Mac goes back only to the phone it came from).
    #[error("another phone is open")]
    OtherPhone,
    /// `MOST_EDITS` working copies are open already.
    #[error("too many files are being edited")]
    TooManyEdits,
    #[error("the Mac's disk is full")]
    MacDiskFull,
    /// A transfer would not fit where it is going: refused before it starts.
    #[error("needs {needed} bytes; {free} are free {place}")]
    NotEnoughRoom {
        needed: u64,
        free: u64,
        place: String,
    },
    /// The Mac volume a transfer would write to can't be written (an NTFS
    /// drive, a locked SD card, a read-only disk image); where, as
    /// `NotEnoughRoom` names it.
    #[error("nothing can be written {0}")]
    VolumeReadOnly(String),
    /// A file too big for the file system it would go to (4 GB or more on
    /// FAT32); where, as `NotEnoughRoom` names it.
    #[error("too big for the disk {0}")]
    TooBigForVolume(String),
    /// An undo found the item no longer as the change left it (gone, moved
    /// again, or a new folder that has something in it now).
    #[error("{0} has changed since")]
    ChangedSince(String),
    /// An undo of several renames put some names back but not this one,
    /// which isn't as the rename left it.
    #[error("{0} kept its new name")]
    PartlyUndone(String),
    /// The app's own code panicked while doing this. What it was doing is
    /// stopped, the app goes on, and a crash report was written.
    #[error("something went wrong inside the app")]
    Crashed,
    #[error("{0}")]
    General(String),
}

/// Why a file or folder name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    IllegalCharacters,
    Reserved,
}

/// The part of System Settings › Privacy & Security that lets the app into a
/// folder macOS keeps private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyPane {
    /// Files and Folders: a switch each for Desktop, Documents, Downloads,
    /// removable and network volumes.
    FilesAndFolders,
    /// Full Disk Access: everything else macOS protects (other apps' data,
    /// another user's folders).
    FullDiskAccess,
}
