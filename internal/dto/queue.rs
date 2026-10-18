//! Transfers kept across a quit, a crash or a restart: what the queue had
//! still to do, as it would run it next, and the phone it was for.

use super::{
    CheckRequest, Conflict, DownloadRequest, ImportRequest, MirrorRequest, Paste, PasteRequest,
    UploadRequest,
};

/// A transfer not finished when the app last ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTransfer {
    /// The phone it was for (`Phone::key`); `None` for one on the Mac alone.
    pub phone: Option<String>,
    pub job: SavedJob,
}

/// A queued job, as the transfer queue would run it next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedJob {
    Download {
        request: DownloadRequest,
        mode: Paste,
        answer: SavedAnswer,
    },
    Upload {
        request: UploadRequest,
        mode: Paste,
        answer: SavedAnswer,
    },
    /// A copy or move inside the phone, by way of the Mac; `resuming` for a
    /// move that carries on into what it started.
    OnPhone {
        request: PasteRequest,
        resuming: bool,
    },
    /// A copy or move inside the Mac.
    OnMac(PasteRequest),
    Import(ImportRequest),
    Mirror(MirrorRequest),
    Check(CheckRequest),
}

/// What became of the question about taken names, as the queue keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedAnswer {
    Unasked,
    NothingTaken,
    Chose(Conflict),
    Retried(Conflict),
}
