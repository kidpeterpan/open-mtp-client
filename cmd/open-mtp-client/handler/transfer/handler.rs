use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::{button, column, container, progress_bar, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::Request;
use super::response::{ProgressResponse, SummaryResponse};
use crate::handler::error::{self, ErrorResponse};
use crate::i18n::{count, t, tf};

/// Called by the service after every chunk.
pub type Progress = Box<dyn Fn(dto::TransferProgress) + Send + Sync>;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Copies phone items to the Mac; with `Paste::Move`, removes each
    /// original once its copy is checked.
    async fn download(
        &self,
        mode: dto::Paste,
        request: dto::DownloadRequest,
        conflict: dto::Conflict,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Which of the requested names already hold something different on the
    /// Mac (a folder, or another file).
    async fn taken_on_mac(&self, request: dto::DownloadRequest) -> Result<Vec<String>, dto::Error>;

    /// Copies or moves items from one phone folder to another by way of the
    /// Mac, for a phone that can't do it by itself.
    ///
    /// `resuming`: this is the retry of a move that stopped part way, so a
    /// folder is merged into the one the first try made, not numbered.
    async fn paste_on_phone(
        &self,
        request: dto::PasteRequest,
        resuming: bool,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Copies or moves items from one Mac folder to another, of any size, with
    /// progress and a way to stop: what the pane hands over when a copy is too
    /// big to do on the spot. A name that is taken is never replaced.
    async fn paste_on_mac(
        &self,
        request: dto::PasteRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Which of the requested names are already taken on the phone.
    async fn existing(&self, request: dto::UploadRequest) -> Result<Vec<String>, dto::Error>;

    /// Copies Mac items to the phone; with `Paste::Move`, puts each original
    /// in the Trash once its copy has been read back from the phone.
    async fn upload(
        &self,
        mode: dto::Paste,
        request: dto::UploadRequest,
        conflict: dto::Conflict,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Copies the phone's new photos and videos into a Mac folder.
    async fn import(
        &self,
        request: dto::ImportRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Makes one folder's contents match the other's.
    async fn mirror(
        &self,
        request: dto::MirrorRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Compares phone files with the Mac's of the same name, byte for byte;
    /// changes nothing.
    async fn check_contents(
        &self,
        request: dto::CheckRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Fetches a phone file into a working copy and opens it in its Mac app.
    async fn edit_on_mac(&self, request: dto::EditRequest) -> Result<dto::WorkingCopy, dto::Error>;

    /// The working copies there are, after a relaunch too.
    async fn working_copies(&self) -> Vec<dto::WorkingCopy>;

    /// What each copy on the Mac is now; `None` for one that has gone.
    async fn copy_states(&self, paths: Vec<String>) -> Vec<Option<dto::CopyState>>;

    /// Sends an edited copy back to the phone (replacing the phone's file if
    /// it is still the one fetched, else beside it), and forgets the copy.
    async fn send_edit(
        &self,
        copy: dto::WorkingCopy,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Forgets a working copy and its edits.
    async fn discard_edit(&self, copy: dto::WorkingCopy) -> Result<(), dto::Error>;

    /// Deletes phone items, each only after a whole copy of it is in the
    /// Mac's Trash.
    async fn delete_keeping_copy(
        &self,
        request: dto::DeleteRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::TransferSummary, dto::Error>;

    /// Opens Finder with `path` selected.
    fn reveal(&self, path: &str);

    /// Shows how far the transfer under way is on the app's Dock icon;
    /// `None` takes the bar away.
    fn show_progress(&self, percent: Option<u8>);

    /// Tells the user, while they are in another app, that transfers ended
    /// or that one waits for them.
    fn notify(&self, title: &str, body: &str);

    /// Keeps the Mac from idling to sleep while `awake` (sleep would drop the
    /// phone); says whether it is now kept awake.
    fn keep_awake(&self, awake: bool) -> bool;

    /// Plays the Mac's own sound for how the queue ended.
    fn chime(&self, sound: dto::Sound);

    /// Writes a transfer's report into `folder` as a new file; says where.
    async fn save_report(&self, folder: String, text: String) -> Result<String, dto::Error>;
}

#[derive(Debug, Clone)]
// A finished transfer's summary makes `Finished` the big variant; it comes
// once per transfer, so boxing it would buy nothing but noise at every use.
#[allow(clippy::large_enum_variant)]
pub enum Message {
    Request(Request),
    /// Which upload names are already taken on the phone.
    Checked {
        request: dto::UploadRequest,
        result: Result<Vec<String>, ErrorResponse>,
    },
    /// Which download names are already taken on the Mac.
    CheckedOnMac {
        request: dto::DownloadRequest,
        result: Result<Vec<String>, ErrorResponse>,
    },
    Progress(dto::TransferProgress),
    Finished(Result<dto::TransferSummary, ErrorResponse>),
    /// The report shown was scrolled this far down.
    ReportScrolled(f32),
    /// Where the report was saved, or why it wasn't.
    ReportSaved(Result<String, ErrorResponse>),
    /// The working copies found at start-up.
    EditsLoaded(Vec<dto::WorkingCopy>),
    /// A phone file fetched to edit (and opened), or why not.
    Edited(Result<dto::WorkingCopy, ErrorResponse>),
    /// Time to look whether the working copies were saved (every
    /// `EDIT_CHECK` while there are any).
    CheckEdits,
    /// What each working copy is now, by its id.
    EditsChecked(Vec<(String, Option<dto::CopyState>)>),
    /// A working copy was discarded, or couldn't be.
    EditDiscarded(String, Result<(), ErrorResponse>),
}

/// How often the working copies are looked at while there are any.
pub const EDIT_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// A phone file being edited on the Mac.
#[derive(Debug, Clone)]
struct Edit {
    copy: dto::WorkingCopy,
    /// What the copy was at the last look.
    last: Option<dto::CopyState>,
    /// Changed from what was fetched, and the same at two looks in a row:
    /// saved, not being written.
    saved: bool,
}

impl Edit {
    fn new(copy: dto::WorkingCopy) -> Self {
        Self {
            copy,
            last: None,
            saved: false,
        }
    }

    /// Takes in what the copy is now; `false` when it has gone.
    fn look(&mut self, now: Option<dto::CopyState>) -> bool {
        let Some(now) = now else {
            return false;
        };
        self.saved = now != self.copy.fetched && self.last == Some(now);
        self.last = Some(now);
        true
    }
}

impl Message {
    /// The error this message carries if it means the device is gone.
    pub fn device_lost(&self) -> Option<&ErrorResponse> {
        match self {
            Message::Finished(Err(error))
            | Message::Checked {
                result: Err(error), ..
            }
            | Message::CheckedOnMac {
                result: Err(error), ..
            } if error.device_lost => Some(error),
            _ => None,
        }
    }

    /// What a successful transfer copied, so the destination pane can refresh.
    pub fn finished(&self) -> Option<&dto::TransferSummary> {
        match self {
            Message::Finished(Ok(summary)) => Some(summary),
            _ => None,
        }
    }
}

/// One queued transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    /// Copied to the Mac; a `Paste::Move` also removes the originals. The
    /// question about names taken on the Mac is asked when its turn comes,
    /// and its answer kept, as for an upload.
    Download(dto::DownloadRequest, dto::Paste, Answer),
    /// The conflict question is asked when the upload's turn comes, as the
    /// phone may have changed while it waited. A `Paste::Move` also puts the
    /// originals in the Trash. What became of the question is kept (`Answer`)
    /// so that a retry can act on it.
    Upload(dto::UploadRequest, dto::Paste, Answer),
    /// A copy or move inside the phone, through the Mac: the phone can't do it
    /// by itself. A taken name is never replaced, so there is nothing to ask.
    /// The flag marks the retry of a move that stopped part way, which merges
    /// into the folder the first try made.
    OnPhone(dto::PasteRequest, bool),
    /// A copy or move from one Mac folder to another that was too big for the
    /// pane to do on the spot (a move to another volume is a copy).
    OnMac(dto::PasteRequest),
    /// The phone's new photos and videos into a Mac folder. A retry imports
    /// what is still new, so it is the same request.
    Import(dto::ImportRequest),
    /// One folder's contents onto the other. A retry compares again, so it
    /// is the same request.
    Mirror(dto::MirrorRequest),
    /// Phone files read and compared with the Mac's, byte for byte; nothing
    /// is written, so it can always be run again from the start.
    Check(dto::CheckRequest),
    /// A file edited on the Mac, sent back to the phone. It asks the phone
    /// again each time it runs, so it can carry on after a stop; it is not
    /// kept across a quit (its working copy is, with its own Send).
    SendEdit(dto::WorkingCopy),
    /// Phone items deleted after a copy of each is in the Mac's Trash. Not
    /// carried on after a stop (nothing it hadn't copied was deleted), nor
    /// kept across a quit: a delete is asked for again.
    DeleteKeeping(dto::DeleteRequest),
}

impl Job {
    fn label(&self) -> String {
        match self {
            Job::Download(request, mode, _) => mapper::job_label(
                dto::Direction::Download,
                *mode == dto::Paste::Move,
                &request.names,
                &request.destination,
            ),
            Job::Upload(request, mode, _) => mapper::job_label(
                dto::Direction::Upload,
                *mode == dto::Paste::Move,
                &request.sources,
                &request.destination,
            ),
            Job::OnPhone(request, _) => {
                mapper::on_phone_label(request.mode, &request.names, &request.to)
            }
            Job::OnMac(request) => mapper::job_label(
                dto::Direction::Local,
                request.mode == dto::Paste::Move,
                &request.names,
                &request.to,
            ),
            Job::Import(request) => mapper::import_label(&request.destination),
            Job::Mirror(request) => mapper::mirror_label(request),
            Job::Check(request) => mapper::check_label(request),
            Job::SendEdit(copy) => mapper::send_edit_label(&copy.name),
            Job::DeleteKeeping(request) => mapper::delete_keeping_label(&request.names),
        }
    }
}

/// What became of an upload's question about names already taken on the phone,
/// which a retry of a move acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Not asked: the first try had not got that far.
    Unasked,
    /// Asked, and nothing was taken, so there was nothing to answer: whatever
    /// of the same name is on the phone now is what the first try made.
    NothingTaken,
    /// The user's answer.
    Chose(dto::Conflict),
    /// A retry, going on without asking, with this.
    Retried(dto::Conflict),
}

impl Answer {
    /// What the upload is run with, once it is running.
    fn conflict(self) -> dto::Conflict {
        match self {
            Answer::Unasked | Answer::NothingTaken => dto::Conflict::KeepBoth,
            Answer::Chose(conflict) | Answer::Retried(conflict) => conflict,
        }
    }

    /// What a retry of this move does about taken names. Nothing taken: it
    /// merges into the folders the first try made, since any of that name is
    /// theirs. Skip and Replace were the user's choice about names the first
    /// try met, and still stand (a name skipped is still left alone). Keep both
    /// numbered what the first try made, so which folder to go on in is not
    /// known: the question is asked again, as for a first try.
    fn for_a_retry(self) -> Answer {
        match self {
            Answer::NothingTaken => Answer::Retried(dto::Conflict::Merge),
            Answer::Chose(
                conflict @ (dto::Conflict::Skip
                | dto::Conflict::Replace
                | dto::Conflict::ReplaceOlder),
            )
            | Answer::Retried(conflict) => Answer::Retried(conflict),
            Answer::Chose(_) | Answer::Unasked => Answer::Unasked,
        }
    }
}

/// What the running transfer is doing.
enum Stage {
    /// Asking the phone which upload names are taken.
    Checking,
    /// Waiting for the user to say what to do with taken names.
    Asking { existing: Vec<String> },
    Running {
        progress: ProgressResponse,
        last: Option<dto::TransferProgress>,
        cancel: dto::Cancel,
        cancelling: bool,
        /// Stopping to be resumed: the job is kept, not finished.
        pausing: bool,
    },
}

/// A transfer stopped by Pause, or by losing the phone: the job that
/// carries on from where it stopped, and how far it had got, for the panel.
struct Paused {
    job: Job,
    progress: ProgressResponse,
    last: Option<dto::TransferProgress>,
    /// Stopped because the phone went away or went silent: it carries on by
    /// itself when the same phone opens again (`phone_back`).
    for_phone: bool,
}

struct Current {
    job: Job,
    stage: Stage,
}

struct Finished {
    job: Job,
    outcome: Result<SummaryResponse, ErrorResponse>,
    source: Option<(dto::TransferSummary, Option<dto::UsbSpeed>)>,
    /// Another phone has been opened since: the job's storage ids and paths
    /// would name that phone's files, so it is not run again.
    other_phone: bool,
}

impl Job {
    /// Whether the job works on the phone (every kind but a Mac-only paste).
    fn uses_phone(&self) -> bool {
        !matches!(self, Job::OnMac(_))
    }

    /// The job that carries on from where this one was stopped, without
    /// asking anything again; `None` where none can. A download keeps its
    /// part files and skips what arrived, an import and a mirror compare
    /// again, an upload skips the files the phone has whole (a file stopped
    /// part way is sent again: the phone keeps no part), a move through the
    /// Mac merges into what it started (as its Retry does). A copy inside one
    /// side would be made again beside what it made, a paste inside the Mac
    /// leaves no account of what it moved, and an upload whose taken names
    /// were numbered can't tell its folder from the one it numbered past.
    fn resumed(&self) -> Option<Job> {
        match self {
            // Carries on with the answer it had (its parts, what it skipped
            // or replaced as they were), without asking again.
            Job::Download(request, mode, answer) => Some(Job::Download(
                request.clone(),
                *mode,
                Answer::Retried(answer.conflict()),
            )),
            Job::Import(_) | Job::Mirror(_) | Job::Check(_) | Job::SendEdit(_) => {
                Some(self.clone())
            }
            Job::OnPhone(request, _) if request.mode == dto::Paste::Move => {
                Some(Job::OnPhone(request.clone(), true))
            }
            Job::OnPhone(..) | Job::OnMac(_) | Job::DeleteKeeping(_) => None,
            Job::Upload(request, mode, answer) => {
                let answer = match (mode, answer) {
                    // Nothing was taken when it started: what is there now is
                    // its own, to merge into. Any other copy carries on only
                    // as a retry already did.
                    (dto::Paste::Copy, Answer::NothingTaken) => {
                        Answer::Retried(dto::Conflict::Merge)
                    }
                    // Run again it replaces nothing it replaced already: those
                    // are as new as the Mac's now.
                    (dto::Paste::Copy, Answer::Chose(dto::Conflict::ReplaceOlder)) => {
                        Answer::Retried(dto::Conflict::ReplaceOlder)
                    }
                    (dto::Paste::Copy, answer) => *answer,
                    (dto::Paste::Move, answer) => answer.for_a_retry(),
                };
                matches!(answer, Answer::Retried(_))
                    .then(|| Job::Upload(request.clone(), *mode, answer))
            }
        }
    }
}

impl Finished {
    fn reword(&mut self) {
        self.outcome = match (&self.source, &self.outcome) {
            (Some((summary, link)), Ok(_)) => Ok(summarize(Some(&self.job), summary, *link)),
            (_, Ok(summary)) => Ok(summary.clone()),
            (_, Err(error)) => Err(error.reworded()),
        };
    }

    /// Worth trying again: it failed, it finished with files missing, or a
    /// move left originals behind. A retry copies only what is not there yet,
    /// so nothing is done twice. Never on a phone other than its own.
    fn is_retryable(&self) -> bool {
        if self.other_phone && self.job.uses_phone() {
            return false;
        }
        match (&self.job, &self.outcome) {
            // A copy inside the phone that partly worked would be copied again
            // in full under new names, beside the part that did: not offered.
            // (A move picks up where it stopped, and a copy that failed
            // altogether has nothing beside it.)
            (Job::OnPhone(request, _) | Job::OnMac(request), Ok(_))
                if request.mode == dto::Paste::Copy =>
            {
                false
            }
            (_, Ok(summary)) => !summary.failures.is_empty() || !summary.left.is_empty(),
            (_, Err(_)) => true,
        }
    }

    /// The job to run again. A move that got part way has already taken some
    /// items off the phone, so it goes on with the ones still there.
    fn retry_job(self) -> Job {
        match (self.job, self.outcome) {
            (Job::Download(mut request, dto::Paste::Move, answer), Ok(summary))
                if !summary.left.is_empty() =>
            {
                request.names = summary.left;
                Job::Download(
                    request,
                    dto::Paste::Move,
                    Answer::Retried(answer.conflict()),
                )
            }
            // A move that got part way left what it started on the phone: the
            // retry merges into it instead of making `name (1)` beside it, and
            // does not send again what is there.
            (Job::OnPhone(mut request, _), Ok(summary))
                if request.mode == dto::Paste::Move && !summary.left.is_empty() =>
            {
                request.names = summary.left;
                // A folder that was numbered because its name was taken is not
                // the folder of that name now: only when nothing was numbered
                // is the one there the first try's.
                Job::OnPhone(request, summary.renamed == 0)
            }
            (Job::OnPhone(request, _), Err(_)) if request.mode == dto::Paste::Move => {
                Job::OnPhone(request, true)
            }
            // The originals that were trashed are gone; the retry goes on with
            // the ones still where they were.
            (Job::OnMac(mut request), Ok(summary))
                if request.mode == dto::Paste::Move && !summary.left.is_empty() =>
            {
                request.names = summary.left;
                Job::OnMac(request)
            }
            // What went to the Trash is gone from the Mac; the retry sends the
            // items still there (found already on the phone, a file is not
            // sent again).
            (Job::Upload(mut request, dto::Paste::Move, answer), Ok(summary))
                if !summary.left.is_empty() =>
            {
                request.sources = summary.left;
                Job::Upload(request, dto::Paste::Move, answer.for_a_retry())
            }
            (Job::Upload(request, dto::Paste::Move, answer), Err(_)) => {
                Job::Upload(request, dto::Paste::Move, answer.for_a_retry())
            }
            (job, _) => job,
        }
    }
}

/// Transfers run one at a time (the phone has one session); the rest wait
/// in a queue. All of it, plus what already finished, shows in a panel
/// under both panes.
pub struct Handler {
    service: Arc<dyn Service>,
    current: Option<Current>,
    waiting: VecDeque<Job>,
    history: Vec<Finished>,
    /// The app's window is the one in front: no notifications then.
    focused: bool,
    /// What the Dock icon shows, so it is redrawn only when that changes.
    dock_shown: Option<u8>,
    /// Whether the Mac was last asked to stay awake, and whether it is.
    awake_asked: bool,
    awake: bool,
    /// Where in `history` the jobs since the queue was last idle begin: what
    /// the next notification sums up.
    batch_start: usize,
    /// A sound when the queue empties (Settings).
    sound: bool,
    /// What that sound says about the jobs since the queue was last idle;
    /// `None` while only the user's own cancels have ended.
    batch_sound: Option<dto::Sound>,
    /// The transfer stopped by Pause; nothing else starts until it is
    /// resumed or cancelled.
    paused: Option<Paused>,
    /// The open phone's USB link, so a long copy's note can say a slow one
    /// is why.
    link: Option<dto::UsbSpeed>,
    /// The finished transfer (its place in `history`) whose report is shown
    /// in the finished list's place.
    details: Option<usize>,
    /// How far down the report shown is scrolled.
    report_offset: f32,
    /// What became of a save or a copy of the report.
    report_note: Option<String>,
    /// The app is quitting: what runs is being stopped, and nothing more
    /// starts.
    quitting: bool,
    /// Transfers not finished when the app last ended, offered again; they
    /// run only once the user says Continue.
    offered: Vec<Job>,
    /// Moves on whenever what `saved` returns may have changed, so the app
    /// can skip comparing the whole queue (a selection of thousands of
    /// names) after every message that couldn't have touched it.
    changes: u64,
    /// Phone files being edited on the Mac.
    edits: Vec<Edit>,
    /// Why the last file couldn't be fetched to edit.
    edit_note: Option<String>,
    /// The phone open now (`Phone::key`): only its edits can be sent.
    open_phone: Option<String>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            current: None,
            waiting: VecDeque::new(),
            history: Vec::new(),
            focused: true,
            dock_shown: None,
            awake_asked: false,
            awake: false,
            batch_start: 0,
            sound: false,
            batch_sound: None,
            paused: None,
            link: None,
            details: None,
            report_offset: 0.0,
            report_note: None,
            quitting: false,
            offered: Vec::new(),
            changes: 0,
            edits: Vec::new(),
            edit_note: None,
            open_phone: None,
        }
    }

    /// Whether to play a sound when the queue empties.
    pub fn set_sound(&mut self, on: bool) {
        self.sound = on;
    }

    #[cfg(test)]
    pub fn plays_sound(&self) -> bool {
        self.sound
    }

    /// The open phone's USB link, as the app learns it.
    pub fn set_link(&mut self, link: Option<dto::UsbSpeed>) {
        self.link = link;
    }

    #[cfg(test)]
    pub fn link(&self) -> Option<dto::UsbSpeed> {
        self.link
    }

    /// The app's window came to the front (`true`) or went behind another.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// How far the transfer under way is, in whole percent; 0 while it is
    /// being checked or waits for an answer; `None` with nothing under way.
    fn dock_percent(&self) -> Option<u8> {
        let current = self.current.as_ref()?;
        Some(match &current.stage {
            Stage::Running { progress, .. } => {
                (progress.total_fraction.clamp(0.0, 1.0) * 100.0).floor() as u8
            }
            Stage::Checking | Stage::Asking { .. } => 0,
        })
    }

    /// Tells the Dock, but only when what it shows changes: a big import
    /// sends thousands of progress reports and the icon is redrawn at most
    /// a hundred times.
    fn sync_dock(&mut self) {
        let now = self.dock_percent();
        if now != self.dock_shown {
            self.dock_shown = now;
            self.service.show_progress(now);
        }
    }

    /// Whether the Mac should be kept awake: while a transfer is checked or
    /// runs. Not while a question waits for the user, who may have gone: a
    /// question left overnight must not keep the Mac up all night.
    fn wants_awake(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| matches!(current.stage, Stage::Checking | Stage::Running { .. }))
    }

    /// Asks the Mac to stay awake or lets it sleep, only when that changes.
    fn sync_awake(&mut self) {
        let wanted = self.wants_awake();
        if wanted != self.awake_asked {
            self.awake_asked = wanted;
            self.awake = self.service.keep_awake(wanted);
        }
    }

    /// Tells everything outside the window (the Dock, the Mac's sleep) what
    /// the queue is doing now.
    fn sync_outside(&mut self) {
        self.sync_dock();
        self.sync_awake();
    }

    /// A transfer is under way or waiting on the user.
    pub fn is_running(&self) -> bool {
        self.current.is_some()
    }

    /// Whether a job running or waiting works on the phone: the phone open
    /// now must stay open until it is done.
    pub fn has_phone_work(&self) -> bool {
        self.current.as_ref().is_some_and(|c| c.job.uses_phone())
            || self.paused.as_ref().is_some_and(|p| p.job.uses_phone())
            || self.waiting.iter().any(Job::uses_phone)
    }

    /// What the queue still has to do, to be kept across a quit or a
    /// crash, each with whether it works on the phone: the transfers offered
    /// from last time, the one running as it would carry on (none when it
    /// can't: a copy inside one side would be made twice), the paused one,
    /// and what waits.
    pub fn saved(&self) -> Vec<(bool, dto::SavedJob)> {
        let running = self
            .current
            .as_ref()
            .and_then(|current| match current.stage {
                Stage::Running { .. } => current.job.resumed(),
                Stage::Checking | Stage::Asking { .. } => Some(current.job.clone()),
            });
        self.offered
            .iter()
            .cloned()
            .chain(running)
            .chain(self.paused.as_ref().map(|paused| paused.job.clone()))
            .chain(self.waiting.iter().cloned())
            .filter_map(|job| Some((job.uses_phone(), to_saved(&job)?)))
            .collect()
    }

    /// Fetches a phone file to edit on the Mac, and opens it.
    pub fn edit(&mut self, request: dto::EditRequest) -> Task<Message> {
        self.edit_note = None;
        let service = Arc::clone(&self.service);
        Task::perform(
            async move {
                service
                    .edit_on_mac(request)
                    .await
                    .map_err(error::to_error_response)
            },
            Message::Edited,
        )
    }

    /// Reads the working copies kept from before (at start-up).
    pub fn load_edits(&self) -> Task<Message> {
        let service = Arc::clone(&self.service);
        Task::perform(
            async move { service.working_copies().await },
            Message::EditsLoaded,
        )
    }

    /// There are working copies to watch.
    pub fn has_edits(&self) -> bool {
        !self.edits.is_empty()
    }

    /// The phone open now, as last told.
    pub fn open_phone(&self) -> Option<&str> {
        self.open_phone.as_deref()
    }

    /// The phone open now, whose edits can be sent.
    pub fn set_open_phone(&mut self, phone: Option<String>) {
        self.open_phone = phone;
    }

    /// A count that moves on whenever what `saved` returns may have changed.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Offers transfers kept from last time, to Continue or Discard.
    pub fn offer(&mut self, jobs: Vec<dto::SavedJob>) {
        self.changes += 1;
        self.offered.extend(jobs.into_iter().map(from_saved));
    }

    /// Takes back the offered transfers that work on the phone (another
    /// phone has opened: they are for the last one); the Mac's own stay.
    pub fn withdraw_offered(&mut self) -> Vec<dto::SavedJob> {
        self.changes += 1;
        let (phone, mac): (Vec<Job>, Vec<Job>) = self.offered.drain(..).partition(Job::uses_phone);
        self.offered = mac;
        phone.iter().filter_map(to_saved).collect()
    }

    /// Anything running, paused or waiting: what a quit would stop.
    pub fn is_busy(&self) -> bool {
        self.current.is_some() || self.paused.is_some() || !self.waiting.is_empty()
    }

    /// The app is quitting: what runs stops the way Pause stops it where it
    /// can carry on (at the next window, its part kept for next time), and
    /// is cancelled where it can't (an upload's half-sent file is deleted);
    /// a question is dropped; nothing more starts. Says whether something
    /// is still stopping, to be waited for.
    pub fn stop_for_quit(&mut self) -> bool {
        self.changes += 1;
        self.quitting = true;
        match &mut self.current {
            Some(Current {
                job,
                stage:
                    Stage::Running {
                        cancel,
                        cancelling,
                        pausing,
                        ..
                    },
            }) => {
                cancel.cancel();
                if job.resumed().is_some() {
                    *pausing = true;
                } else {
                    *cancelling = true;
                }
                true
            }
            Some(_) => {
                self.current = None;
                false
            }
            None => false,
        }
    }

    /// A transfer is paused, holding the queue.
    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    /// A transfer stopped by losing the phone waits for it to come back.
    pub fn waits_for_phone(&self) -> bool {
        self.paused.as_ref().is_some_and(|paused| paused.for_phone)
    }

    /// The phone a transfer was waiting for has opened again: it carries
    /// on from where it stopped, ahead of what queued behind it.
    pub fn phone_back(&mut self) -> Task<Message> {
        self.changes += 1;
        if !self.waits_for_phone() {
            return Task::none();
        }
        let Some(paused) = self.paused.take() else {
            return Task::none();
        };
        self.waiting.push_front(paused.job);
        let task = self.start_next();
        self.sync_outside();
        task
    }

    /// Another phone is open now: what was done on the last one stays in the
    /// list, but is not offered again.
    pub fn language_changed(&mut self) {
        for finished in &mut self.history {
            finished.reword();
        }
        self.report_note = None;
        self.edit_note = None;
        if let Some(Current {
            job,
            stage: Stage::Running { progress, last, .. },
        }) = &mut self.current
        {
            *progress = match last {
                Some(last) => worded_progress(job, last),
                None => preparing_for(job),
            };
        }
        if let Some(Paused {
            job,
            progress,
            last,
            ..
        }) = &mut self.paused
        {
            *progress = match last {
                Some(last) => worded_progress(job, last),
                None => preparing_for(job),
            };
        }
    }

    pub fn forget_phone(&mut self) {
        self.changes += 1;
        // An edit waiting to go back belongs to the last phone; its working
        // copy stays, to be sent when that phone is back.
        self.waiting.retain(|job| !matches!(job, Job::SendEdit(_)));
        // A paused phone job can't carry on on another phone: it ends as
        // cancelled, listed without Retry like the rest.
        if self.paused.as_ref().is_some_and(|p| p.job.uses_phone())
            && let Some(paused) = self.paused.take()
        {
            self.history.push(Finished {
                job: paused.job,
                outcome: Err(error::to_error_response(dto::Error::Cancelled)),
                source: None,
                other_phone: false,
            });
        }
        for finished in &mut self.history {
            finished.other_phone = true;
        }
    }

    pub fn is_visible(&self) -> bool {
        !self.edits.is_empty()
            || self.edit_note.is_some()
            || !self.offered.is_empty()
            || self.current.is_some()
            || self.paused.is_some()
            || !self.waiting.is_empty()
            || !self.history.is_empty()
    }

    /// A plain copy; the app asks for the mode it wants with `download_as`.
    #[cfg(test)]
    pub fn download(&mut self, request: dto::DownloadRequest) -> Task<Message> {
        self.download_as(dto::Paste::Copy, request)
    }

    /// Copies, or moves (copies and then removes the originals), phone items
    /// to the Mac.
    pub fn download_as(
        &mut self,
        mode: dto::Paste,
        request: dto::DownloadRequest,
    ) -> Task<Message> {
        self.enqueue(Job::Download(request, mode, Answer::Unasked))
    }

    #[cfg(test)]
    pub fn upload(&mut self, request: dto::UploadRequest) -> Task<Message> {
        self.upload_as(dto::Paste::Copy, request)
    }

    /// Copies, or moves (copies, reads back, and puts the originals in the
    /// Trash), Mac items to the phone.
    pub fn upload_as(&mut self, mode: dto::Paste, request: dto::UploadRequest) -> Task<Message> {
        self.enqueue(Job::Upload(request, mode, Answer::Unasked))
    }

    /// Copies or moves phone items to another phone folder by way of the Mac.
    pub fn paste_on_phone(&mut self, request: dto::PasteRequest) -> Task<Message> {
        self.enqueue(Job::OnPhone(request, false))
    }

    /// Queues a mirror of one folder onto the other.
    pub fn mirror(&mut self, request: dto::MirrorRequest) -> Task<Message> {
        self.enqueue(Job::Mirror(request))
    }

    /// Queues an import of the phone's new photos and videos.
    pub fn import(&mut self, request: dto::ImportRequest) -> Task<Message> {
        self.enqueue(Job::Import(request))
    }

    /// Copies or moves Mac items to another Mac folder, as a queued transfer.
    /// Queues a phone delete that keeps a copy of each item in the Trash.
    pub fn delete_keeping(&mut self, request: dto::DeleteRequest) -> Task<Message> {
        self.enqueue(Job::DeleteKeeping(request))
    }

    /// Queues a check of phone files against the Mac's, byte for byte.
    pub fn check(&mut self, request: dto::CheckRequest) -> Task<Message> {
        self.enqueue(Job::Check(request))
    }

    pub fn paste_on_mac(&mut self, request: dto::PasteRequest) -> Task<Message> {
        self.enqueue(Job::OnMac(request))
    }

    fn enqueue(&mut self, job: Job) -> Task<Message> {
        self.changes += 1;
        self.waiting.push_back(job);
        let task = self.start_next();
        self.sync_outside();
        task
    }

    /// Starts the first waiting job if nothing is running.
    fn start_next(&mut self) -> Task<Message> {
        if self.current.is_some() || self.paused.is_some() || self.quitting {
            return Task::none();
        }
        let Some(job) = self.waiting.pop_front() else {
            return Task::none();
        };
        match &job {
            // A retry or a resumed download knows what to do already.
            Job::Download(request, mode, answer @ Answer::Retried(_)) => {
                let (request, mode, answer) = (request.clone(), *mode, *answer);
                self.start_download(mode, request, answer)
            }
            // Looks at the Mac first, in the same task: with nothing taken
            // it goes straight on, as downloads always did; with names taken
            // it stops there and the question is asked.
            Job::Download(request, mode, _) => {
                let (request, mode) = (request.clone(), *mode);
                let cancel = self.begin(job);
                Task::run(
                    download_events(Arc::clone(&self.service), cancel, mode, request),
                    |message| message,
                )
            }
            Job::OnPhone(request, resuming) => {
                let (request, resuming) = (request.clone(), *resuming);
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move {
                        service
                            .paste_on_phone(request, resuming, cancel, progress)
                            .await
                    })
                })
            }
            Job::OnMac(request) => {
                let request = request.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move { service.paste_on_mac(request, cancel, progress).await })
                })
            }
            Job::Import(request) => {
                let request = request.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move { service.import(request, cancel, progress).await })
                })
            }
            Job::Mirror(request) => {
                let request = request.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move { service.mirror(request, cancel, progress).await })
                })
            }
            Job::Check(request) => {
                let request = request.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move { service.check_contents(request, cancel, progress).await })
                })
            }
            Job::SendEdit(copy) => {
                let copy = copy.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(async move { service.send_edit(copy, cancel, progress).await })
                })
            }
            Job::DeleteKeeping(request) => {
                let request = request.clone();
                self.run(job, move |service, cancel, progress| {
                    Box::pin(
                        async move { service.delete_keeping_copy(request, cancel, progress).await },
                    )
                })
            }
            // A retry knows what happened: no question about taken names.
            Job::Upload(request, mode, answer @ Answer::Retried(_)) => {
                let (request, mode, answer) = (request.clone(), *mode, *answer);
                self.start_upload(mode, request, answer)
            }
            Job::Upload(request, _, _) => {
                let request = request.clone();
                self.current = Some(Current {
                    job,
                    stage: Stage::Checking,
                });
                let service = Arc::clone(&self.service);
                Task::perform(
                    async move {
                        let result = service
                            .existing(request.clone())
                            .await
                            .map_err(error::to_error_response);
                        (request, result)
                    },
                    |(request, result)| Message::Checked { request, result },
                )
            }
        }
    }

    fn run(
        &mut self,
        job: Job,
        transfer: impl FnOnce(Arc<dyn Service>, dto::Cancel, Progress) -> Transfer + Send + 'static,
    ) -> Task<Message> {
        let preparing = preparing_for(&job);
        let cancel = dto::Cancel::new();
        self.current = Some(Current {
            job,
            stage: Stage::Running {
                progress: preparing,
                last: None,
                cancel: cancel.clone(),
                cancelling: false,
                pausing: false,
            },
        });
        Task::run(
            transfer_events(Arc::clone(&self.service), cancel, transfer),
            |message| message,
        )
    }

    /// Makes `job` the one running, preparing, and gives its cancel switch.
    fn begin(&mut self, job: Job) -> dto::Cancel {
        let (direction, moving) = match &job {
            Job::Download(_, mode, _) => (dto::Direction::Download, *mode == dto::Paste::Move),
            _ => (dto::Direction::Download, false),
        };
        let cancel = dto::Cancel::new();
        self.current = Some(Current {
            job,
            stage: Stage::Running {
                progress: mapper::preparing_response(direction, moving),
                last: None,
                cancel: cancel.clone(),
                cancelling: false,
                pausing: false,
            },
        });
        cancel
    }

    fn start_download(
        &mut self,
        mode: dto::Paste,
        request: dto::DownloadRequest,
        answer: Answer,
    ) -> Task<Message> {
        let conflict = answer.conflict();
        let job = Job::Download(request.clone(), mode, answer);
        self.run(job, move |service, cancel, progress| {
            Box::pin(async move {
                service
                    .download(mode, request, conflict, cancel, progress)
                    .await
            })
        })
    }

    fn start_upload(
        &mut self,
        mode: dto::Paste,
        request: dto::UploadRequest,
        answer: Answer,
    ) -> Task<Message> {
        let conflict = answer.conflict();
        let job = Job::Upload(request.clone(), mode, answer);
        self.run(job, move |service, cancel, progress| {
            Box::pin(async move {
                service
                    .upload(mode, request, conflict, cancel, progress)
                    .await
            })
        })
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.changes += 1;
        let task = self.route(message);
        self.sync_outside();
        task
    }

    fn route(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(request) => self.handle(request),
            Message::Checked { request, result } => {
                let checking = match &self.current {
                    Some(Current {
                        job: Job::Upload(job, mode, _),
                        stage: Stage::Checking,
                    }) if *job == request => Some(*mode),
                    _ => None,
                };
                let Some(mode) = checking else {
                    return Task::none();
                };
                match result {
                    // Nothing taken: there's no question to ask.
                    Ok(existing) if existing.is_empty() => {
                        self.current = None;
                        self.start_upload(mode, request, Answer::NothingTaken)
                    }
                    Ok(existing) => {
                        // Nothing goes on until the question is answered: say
                        // so to someone who is in another app.
                        if !self.focused {
                            let (title, body) = mapper::question_note(existing.len());
                            self.service.notify(&title, &body);
                        }
                        if let Some(current) = &mut self.current {
                            current.stage = Stage::Asking { existing };
                        }
                        Task::none()
                    }
                    Err(error) => self.finish(Err(error), None),
                }
            }
            // The download found names taken on the Mac before copying.
            Message::CheckedOnMac { request, result } => {
                let preparing = matches!(
                    &self.current,
                    Some(Current {
                        job: Job::Download(job, _, _),
                        stage: Stage::Running { .. },
                    }) if *job == request
                );
                match result {
                    Ok(taken) if preparing && !taken.is_empty() => {
                        if !self.focused {
                            let (title, body) = mapper::question_note(taken.len());
                            self.service.notify(&title, &body);
                        }
                        if let Some(current) = &mut self.current {
                            current.stage = Stage::Asking { existing: taken };
                        }
                        Task::none()
                    }
                    _ => Task::none(),
                }
            }
            Message::Progress(progress) => {
                if let Some(Current {
                    job,
                    stage:
                        Stage::Running {
                            progress: shown,
                            last,
                            ..
                        },
                }) = &mut self.current
                {
                    *shown = worded_progress(job, &progress);
                    *last = Some(progress);
                }
                Task::none()
            }
            Message::EditsLoaded(copies) => {
                for copy in copies {
                    if self.edits.iter().all(|edit| edit.copy.id != copy.id) {
                        self.edits.push(Edit::new(copy));
                    }
                }
                Task::none()
            }
            Message::Edited(Ok(copy)) => {
                if self.edits.iter().all(|edit| edit.copy.id != copy.id) {
                    self.edits.push(Edit::new(copy));
                }
                Task::none()
            }
            Message::Edited(Err(error)) => {
                self.edit_note = Some(tf(
                    "transfer.edit.couldnt_open",
                    &[("reason", &error.title)],
                ));
                Task::none()
            }
            Message::CheckEdits => {
                let (ids, paths): (Vec<String>, Vec<String>) = self
                    .edits
                    .iter()
                    .map(|edit| (edit.copy.id.clone(), edit.copy.path.clone()))
                    .unzip();
                if ids.is_empty() {
                    return Task::none();
                }
                let service = Arc::clone(&self.service);
                Task::perform(
                    async move {
                        ids.into_iter()
                            .zip(service.copy_states(paths).await)
                            .collect()
                    },
                    Message::EditsChecked,
                )
            }
            Message::EditsChecked(states) => {
                let states: HashMap<String, Option<dto::CopyState>> = states.into_iter().collect();
                self.edits
                    .retain_mut(|edit| match states.get(&edit.copy.id) {
                        Some(now) => edit.look(*now),
                        // Not asked about (it came after the look): kept.
                        None => true,
                    });
                Task::none()
            }
            Message::EditDiscarded(id, result) => {
                match result {
                    Ok(()) => self.edits.retain(|edit| edit.copy.id != id),
                    Err(error) => {
                        self.edit_note = Some(tf(
                            "transfer.edit.couldnt_discard",
                            &[("reason", &error.title)],
                        ));
                    }
                }
                Task::none()
            }
            Message::ReportScrolled(offset) => {
                self.report_offset = offset;
                Task::none()
            }
            Message::ReportSaved(result) => {
                self.report_note = Some(match result {
                    Ok(path) => mapper::saved_note(&path),
                    Err(error) => tf("transfer.report.not_saved", &[("reason", &error.title)]),
                });
                Task::none()
            }
            Message::Finished(result) => {
                // A working copy the phone now has is gone (the service
                // removed it): its row goes too.
                if let (Some(Job::SendEdit(copy)), Ok(summary)) =
                    (self.current.as_ref().map(|current| &current.job), &result)
                    && summary.failures.is_empty()
                {
                    let id = copy.id.clone();
                    self.edits.retain(|edit| edit.copy.id != id);
                }
                if let Ok(summary) = &result {
                    note_problems_shown(summary);
                }
                let job = self.current.as_ref().map(|current| &current.job);
                let link = self.link;
                let source = result.as_ref().ok().map(|summary| (summary.clone(), link));
                let outcome = result.map(|summary| summarize(job, &summary, link));
                self.finish(outcome, source)
            }
        }
    }

    fn handle(&mut self, request: Request) -> Task<Message> {
        match request {
            Request::Cancel => match &mut self.current {
                Some(Current {
                    stage:
                        Stage::Running {
                            cancel, cancelling, ..
                        },
                    ..
                }) => {
                    cancel.cancel();
                    *cancelling = true;
                    Task::none()
                }
                // A question not yet answered: drop this upload, move on.
                Some(_) => {
                    self.current = None;
                    self.start_next()
                }
                // The paused transfer ends as cancelled (retryable, as one
                // cancelled while running is) and the queue moves on.
                None => match self.paused.take() {
                    Some(paused) => {
                        self.history.push(Finished {
                            job: paused.job,
                            outcome: Err(error::to_error_response(dto::Error::Cancelled)),
                            source: None,
                            other_phone: false,
                        });
                        self.carry_on()
                    }
                    None => Task::none(),
                },
            },
            Request::Pause => {
                if let Some(Current {
                    job,
                    stage:
                        Stage::Running {
                            cancel,
                            cancelling: false,
                            pausing,
                            ..
                        },
                }) = &mut self.current
                    && job.resumed().is_some()
                {
                    cancel.cancel();
                    *pausing = true;
                }
                Task::none()
            }
            // One waiting for the phone resumes when the phone comes back.
            Request::Resume if self.waits_for_phone() => Task::none(),
            Request::Resume => match self.paused.take() {
                Some(paused) => {
                    self.waiting.push_front(paused.job);
                    self.start_next()
                }
                None => Task::none(),
            },
            Request::Resolve(conflict) => match self.current.take() {
                Some(Current {
                    job: Job::Upload(request, mode, _),
                    stage: Stage::Asking { .. },
                }) => self.start_upload(mode, request, Answer::Chose(conflict)),
                Some(Current {
                    job: Job::Download(request, mode, _),
                    stage: Stage::Asking { .. },
                }) => self.start_download(mode, request, Answer::Chose(conflict)),
                other => {
                    self.current = other;
                    Task::none()
                }
            },
            Request::Remove(index) => {
                self.waiting.remove(index);
                Task::none()
            }
            Request::Retry(index) => match self.history.get(index) {
                Some(finished) if finished.is_retryable() => {
                    // The places in the list move.
                    self.details = None;
                    let job = self.history.remove(index).retry_job();
                    self.enqueue(job)
                }
                _ => Task::none(),
            },
            Request::ShowInFinder(index) => {
                if let Some(Finished {
                    outcome:
                        Ok(SummaryResponse {
                            reveal: Some(path), ..
                        }),
                    ..
                }) = self.history.get(index)
                {
                    self.service.reveal(path);
                }
                Task::none()
            }
            Request::Details(index) => {
                if self.report_of(index).is_some() {
                    self.details = Some(index);
                    self.report_offset = 0.0;
                    self.report_note = None;
                }
                Task::none()
            }
            Request::CloseDetails => {
                self.details = None;
                Task::none()
            }
            Request::SaveReport => {
                let Some(summary) = self.details.and_then(|index| self.report_of(index)) else {
                    return Task::none();
                };
                let Some(folder) = summary.report_folder.clone() else {
                    return Task::none();
                };
                let text = mapper::report_tsv(&summary.report, summary.report_left_out);
                let service = Arc::clone(&self.service);
                Task::perform(
                    async move { service.save_report(folder, text).await },
                    |result| Message::ReportSaved(result.map_err(error::to_error_response)),
                )
            }
            Request::CopyReport => {
                let Some(summary) = self.details.and_then(|index| self.report_of(index)) else {
                    return Task::none();
                };
                let text = mapper::report_tsv(&summary.report, summary.report_left_out);
                self.report_note = Some(mapper::copied_note(summary.report.len()));
                iced::clipboard::write(text)
            }
            Request::ContinueSaved => {
                let offered = std::mem::take(&mut self.offered);
                self.waiting.extend(offered);
                let task = self.start_next();
                self.sync_outside();
                task
            }
            Request::DiscardSaved => {
                self.offered.clear();
                Task::none()
            }
            Request::SendEdit(id) => {
                let Some(edit) = self.edits.iter().find(|edit| edit.copy.id == id) else {
                    return Task::none();
                };
                // Only to the phone it came from, and once.
                let queued = |job: &Job| matches!(job, Job::SendEdit(copy) if copy.id == id);
                let busy = self
                    .current
                    .as_ref()
                    .is_some_and(|current| queued(&current.job))
                    || self.waiting.iter().any(queued);
                if busy || self.open_phone.as_deref() != Some(edit.copy.phone.as_str()) {
                    return Task::none();
                }
                self.enqueue(Job::SendEdit(edit.copy.clone()))
            }
            Request::DiscardEdit(id) => {
                let Some(edit) = self.edits.iter().find(|edit| edit.copy.id == id) else {
                    return Task::none();
                };
                let (service, copy) = (Arc::clone(&self.service), edit.copy.clone());
                Task::perform(
                    async move {
                        let result = service
                            .discard_edit(copy)
                            .await
                            .map_err(error::to_error_response);
                        (id, result)
                    },
                    |(id, result)| Message::EditDiscarded(id, result),
                )
            }
            Request::Clear => {
                self.edit_note = None;
                self.details = None;
                self.history.clear();
                self.batch_start = 0;
                self.batch_sound = None;
                Task::none()
            }
        }
    }

    /// The summary of the finished transfer at `index`, if it has a report.
    fn report_of(&self, index: usize) -> Option<&SummaryResponse> {
        match self.history.get(index) {
            Some(Finished {
                outcome: Ok(summary),
                ..
            }) if !summary.report.is_empty() => Some(summary),
            _ => None,
        }
    }

    /// Records the running job's outcome and starts the next one; a job the
    /// user paused that stopped as asked is kept instead, and the queue
    /// waits for it.
    fn finish(
        &mut self,
        outcome: Result<SummaryResponse, ErrorResponse>,
        source: Option<(dto::TransferSummary, Option<dto::UsbSpeed>)>,
    ) -> Task<Message> {
        if let Some(Current {
            job,
            stage:
                Stage::Running {
                    progress,
                    last,
                    pausing: true,
                    cancelling: false,
                    ..
                },
        }) = &self.current
            && matches!(&outcome, Err(error) if error.cancelled)
            && let Some(resumed) = job.resumed()
        {
            self.paused = Some(Paused {
                job: resumed,
                progress: progress.clone(),
                last: last.clone(),
                for_phone: false,
            });
            self.current = None;
            return Task::none();
        }
        // The phone went away or went silent mid-way (a cable, a locked
        // screen): a job that can carry on waits for the same phone, its
        // parts kept, rather than ending; the user's own cancel still ends.
        if let Some(Current {
            job,
            stage:
                Stage::Running {
                    progress,
                    last,
                    cancelling: false,
                    ..
                },
        }) = &self.current
            && matches!(&outcome, Err(error) if error.device_lost)
            && let Some(resumed) = job.resumed()
        {
            let label = job.label();
            self.paused = Some(Paused {
                job: resumed,
                progress: progress.clone(),
                last: last.clone(),
                for_phone: true,
            });
            self.current = None;
            if !self.focused {
                let (title, body) = mapper::phone_lost_note(&label);
                self.service.notify(&title, &body);
            }
            return Task::none();
        }
        if let Some(current) = self.current.take() {
            // The user's own cancel needs no sound: they are looking.
            let cancelled = matches!(
                current.stage,
                Stage::Running {
                    cancelling: true,
                    ..
                }
            ) && outcome.is_err();
            if !cancelled {
                self.batch_sound = Some(if mapper::is_troubled(&outcome) {
                    dto::Sound::Problem
                } else {
                    self.batch_sound.unwrap_or(dto::Sound::Done)
                });
            }
            self.history.push(Finished {
                job: current.job,
                outcome,
                source,
                other_phone: false,
            });
        }
        self.carry_on()
    }

    /// Starts the next job; with none left, tells the user how the queue
    /// went.
    fn carry_on(&mut self) -> Task<Message> {
        let next = self.start_next();
        // The queue is done: one note for everything since it was last idle,
        // if the user is in another app.
        if self.current.is_none() && self.waiting.is_empty() {
            let start = self.batch_start.min(self.history.len());
            if !self.focused {
                let batch: Vec<(String, &Result<SummaryResponse, ErrorResponse>)> = self.history
                    [start..]
                    .iter()
                    .map(|finished| (finished.job.label(), &finished.outcome))
                    .collect();
                if let Some((title, body)) = mapper::batch_note(&batch) {
                    self.service.notify(&title, &body);
                }
            }
            self.batch_start = self.history.len();
            if let Some(sound) = self.batch_sound.take()
                && self.sound
            {
                self.service.chime(sound);
            }
        }
        next
    }

    pub fn view(&self) -> Element<'_, Message> {
        if !self.is_visible() {
            return space().into();
        }
        let mut panel = column![].spacing(10);
        if !self.offered.is_empty() {
            panel = panel.push(offered_view(&self.offered));
        }
        for edit in &self.edits {
            panel = panel.push(edit_row(edit, self.open_phone.as_deref()));
        }
        if let Some(note) = &self.edit_note {
            panel = panel.push(text(note).size(12).style(text::danger));
        }

        if let Some(current) = &self.current {
            panel = panel.push(match &current.stage {
                Stage::Checking => text(t("transfer.checking_phone"))
                    .size(13)
                    .style(text::secondary)
                    .into(),
                Stage::Asking { existing } => asking(
                    existing,
                    match current.job {
                        Job::Download(..) => Asked::OnMac,
                        Job::Upload(_, dto::Paste::Copy, _) => Asked::PhoneCopy,
                        _ => Asked::PhoneMove,
                    },
                ),
                Stage::Running {
                    progress,
                    cancelling,
                    pausing,
                    ..
                } => running(
                    progress,
                    Stopping {
                        cancelling: *cancelling,
                        pausing: *pausing,
                        can_pause: current.job.resumed().is_some(),
                    },
                    self.awake,
                    self.link,
                ),
            });
        }
        if let Some(paused) = &self.paused {
            panel = panel.push(paused_view(&paused.progress, paused.for_phone));
        }

        if !self.waiting.is_empty() {
            let rows = self.waiting.iter().enumerate().map(|(index, job)| {
                row![
                    text(job.label()).size(12),
                    space::horizontal(),
                    button(text("×").size(14))
                        .style(button::text)
                        .padding([0, 6])
                        .on_press(Message::Request(Request::Remove(index))),
                ]
                .align_y(Alignment::Center)
                .into()
            });
            panel = panel.push(
                column![
                    text(tf(
                        "transfer.waiting_count",
                        &[("count", &self.waiting.len().to_string())],
                    ))
                    .size(12)
                    .style(text::secondary),
                    column(rows).spacing(2),
                ]
                .spacing(4),
            );
        }

        if let Some(summary) = self.details.and_then(|index| self.report_of(index)) {
            if self.current.is_some() || self.paused.is_some() || !self.waiting.is_empty() {
                panel = panel.push(rule::horizontal(1));
            }
            panel = panel.push(report_view(
                summary,
                self.report_offset,
                self.report_note.as_deref(),
            ));
        } else if !self.history.is_empty() {
            if self.current.is_some() || self.paused.is_some() || !self.waiting.is_empty() {
                panel = panel.push(rule::horizontal(1));
            }
            let rows = self
                .history
                .iter()
                .enumerate()
                .rev()
                .map(|(index, finished)| history_row(index, finished));
            panel = panel.push(
                column![
                    row![
                        text(t("label.finished")).size(12).style(text::secondary),
                        space::horizontal(),
                        button(text(t("label.clear")).size(12))
                            .style(button::text)
                            .padding([0, 6])
                            .on_press(Message::Request(Request::Clear)),
                    ]
                    .align_y(Alignment::Center),
                    scrollable(column(rows).spacing(6)).height(Length::Shrink),
                ]
                .spacing(4),
            );
        }

        container(panel)
            .padding(12)
            .width(Length::Fill)
            .max_height(320)
            .style(container::rounded_box)
            .into()
    }
}

/// A transfer the queue runs: the service call for one job.
type Transfer = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<dto::TransferSummary, dto::Error>> + Send>,
>;

/// `work`, with a panic in it turned into the job's own failure
/// (`dto::Error::Crashed`). The queue waits for every job's `Finished`: a
/// panic that ended the task instead would leave the job "running" for good,
/// holding the queue, the Dock bar, the Mac awake and the phone's session.
/// The panic hook has already written its report.
fn note_problems_shown(summary: &dto::TransferSummary) {
    for failure in &summary.failures {
        error::note_shown(&failure.error);
    }
    let Some(removal) = &summary.removal else {
        return;
    };
    for kept in &removal.kept {
        if let dto::Why::Refused(refusal) = &kept.why {
            error::note_shown(refusal);
        }
    }
    if let Some(stopped) = &removal.stopped {
        error::note_shown(stopped);
    }
}

fn preparing_for(job: &Job) -> ProgressResponse {
    let (direction, moving) = match job {
        Job::Download(_, mode, _) => (dto::Direction::Download, *mode == dto::Paste::Move),
        Job::Upload(_, mode, _) => (dto::Direction::Upload, *mode == dto::Paste::Move),
        Job::SendEdit(_) => (dto::Direction::Upload, false),
        Job::DeleteKeeping(_) => (dto::Direction::Download, true),
        Job::OnPhone(request, _) => (dto::Direction::Upload, request.mode == dto::Paste::Move),
        Job::OnMac(request) => (dto::Direction::Local, request.mode == dto::Paste::Move),
        Job::Import(_) | Job::Check(_) => (dto::Direction::Download, false),
        Job::Mirror(request) => (
            match request.direction {
                dto::MirrorDirection::ToMac => dto::Direction::Download,
                dto::MirrorDirection::ToPhone => dto::Direction::Upload,
            },
            false,
        ),
    };
    match job {
        Job::OnPhone(..) => mapper::preparing_via_mac(moving),
        Job::Import(_) => mapper::preparing_import(),
        Job::Mirror(_) => mapper::preparing_mirror(),
        Job::Check(_) => mapper::preparing_check(),
        _ => mapper::preparing_response(direction, moving),
    }
}

fn worded_progress(job: &Job, progress: &dto::TransferProgress) -> ProgressResponse {
    match job {
        Job::Import(_) => mapper::to_import_progress(progress),
        Job::Mirror(_) => mapper::to_mirror_progress(progress),
        Job::Check(_) => mapper::to_check_progress(progress),
        _ => mapper::to_progress_response(progress),
    }
}

fn summarize(
    job: Option<&Job>,
    summary: &dto::TransferSummary,
    link: Option<dto::UsbSpeed>,
) -> SummaryResponse {
    let response = match job {
        Some(Job::Import(_)) => mapper::to_import_summary(summary),
        Some(Job::Mirror(_)) => mapper::to_mirror_summary(summary),
        Some(Job::Check(_)) => mapper::to_check_summary(summary),
        Some(Job::SendEdit(copy)) => mapper::to_edit_summary(summary, &copy.name),
        Some(Job::DeleteKeeping(_)) => mapper::to_delete_summary(summary),
        _ => mapper::to_summary_response(summary),
    };
    SummaryResponse {
        timing: mapper::timing_line(summary, link),
        ..response
    }
}

async fn guarded<T>(
    work: impl std::future::Future<Output = Result<T, dto::Error>>,
) -> Result<T, dto::Error> {
    use iced::futures::FutureExt;
    std::panic::AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or(Err(dto::Error::Crashed))
}

/// Runs one transfer, turning its progress callbacks into messages and
/// ending with `Finished`.
fn transfer_events(
    service: Arc<dyn Service>,
    cancel: dto::Cancel,
    transfer: impl FnOnce(Arc<dyn Service>, dto::Cancel, Progress) -> Transfer + Send + 'static,
) -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
        let progress_output = Mutex::new(output.clone());
        // Progress is best effort: when the UI is behind, a dropped update is
        // replaced by the next one.
        let progress: Progress = Box::new(move |progress| {
            if let Ok(mut sender) = progress_output.lock() {
                let _ = sender.try_send(Message::Progress(progress));
            }
        });
        // Timed here, around every kind of job, for the finished line's
        // time and speed.
        let started = std::time::Instant::now();
        // Inside the guard even the call that makes the future: a closure
        // that panics before its first await is a panic in the job too.
        let result = guarded(async move { transfer(service, cancel, progress).await })
            .await
            .map(|summary| dto::TransferSummary {
                elapsed: started.elapsed(),
                ..summary
            });
        let _ = output
            .send(Message::Finished(result.map_err(error::to_error_response)))
            .await;
    })
}

/// A download that first looks at the Mac: names already there that hold
/// something different stop it before a byte is copied (`CheckedOnMac`, and
/// the question is asked); otherwise it goes on as `transfer_events` would,
/// keeping what is there as downloads always did. A Mac that can't be looked
/// at is the download's failure.
fn download_events(
    service: Arc<dyn Service>,
    cancel: dto::Cancel,
    mode: dto::Paste,
    request: dto::DownloadRequest,
) -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        64,
        async move |mut output: mpsc::Sender<Message>| match guarded(
            service.taken_on_mac(request.clone()),
        )
        .await
        {
            Ok(taken) if !taken.is_empty() => {
                let _ = output
                    .send(Message::CheckedOnMac {
                        request,
                        result: Ok(taken),
                    })
                    .await;
            }
            Err(err) => {
                let _ = output
                    .send(Message::Finished(Err(error::to_error_response(err))))
                    .await;
            }
            Ok(_) => {
                let mut events = std::pin::pin!(transfer_events(
                    service,
                    cancel,
                    move |service, cancel, progress| {
                        Box::pin(async move {
                            service
                                .download(mode, request, dto::Conflict::KeepBoth, cancel, progress)
                                .await
                        })
                    }
                ));
                while let Some(message) = events.next().await {
                    if output.send(message).await.is_err() {
                        break;
                    }
                }
            }
        },
    )
}

/// The transfers not finished last time: what they are, and Continue or
/// Discard. Nothing runs until Continue.
fn offered_view(offered: &[Job]) -> Element<'_, Message> {
    let mut list = column![
        row![
            text(mapper::offered_title(offered.len())).size(15),
            space::horizontal(),
            button(text(t("label.continue")).size(13))
                .style(button::primary)
                .on_press(Message::Request(Request::ContinueSaved)),
            button(text(t("label.discard")).size(13))
                .style(button::secondary)
                .on_press(Message::Request(Request::DiscardSaved)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(4);
    for job in offered.iter().take(OFFERED_SHOWN) {
        list = list.push(text(job.label()).size(12).style(text::secondary));
    }
    if offered.len() > OFFERED_SHOWN {
        list = list.push(
            text(count(
                "explorer.paste.more",
                offered.len() - OFFERED_SHOWN,
                &[],
            ))
            .size(12)
            .style(text::secondary),
        );
    }
    list.push(text(mapper::offered_note()).size(12).style(text::secondary))
        .into()
}

/// The most offered transfers named; the rest are counted.
const OFFERED_SHOWN: usize = 5;

/// A job as it is kept across a quit.
/// The job as it is kept across a quit; `None` for one that isn't (an
/// edit's working copy is kept instead).
fn to_saved(job: &Job) -> Option<dto::SavedJob> {
    Some(match job {
        Job::Download(request, mode, answer) => dto::SavedJob::Download {
            request: request.clone(),
            mode: *mode,
            answer: to_saved_answer(*answer),
        },
        Job::Upload(request, mode, answer) => dto::SavedJob::Upload {
            request: request.clone(),
            mode: *mode,
            answer: to_saved_answer(*answer),
        },
        Job::OnPhone(request, resuming) => dto::SavedJob::OnPhone {
            request: request.clone(),
            resuming: *resuming,
        },
        Job::OnMac(request) => dto::SavedJob::OnMac(request.clone()),
        Job::Import(request) => dto::SavedJob::Import(request.clone()),
        Job::Mirror(request) => dto::SavedJob::Mirror(request.clone()),
        Job::Check(request) => dto::SavedJob::Check(request.clone()),
        Job::SendEdit(_) | Job::DeleteKeeping(_) => return None,
    })
}

fn from_saved(saved: dto::SavedJob) -> Job {
    match saved {
        dto::SavedJob::Download {
            request,
            mode,
            answer,
        } => Job::Download(request, mode, from_saved_answer(answer)),
        dto::SavedJob::Upload {
            request,
            mode,
            answer,
        } => Job::Upload(request, mode, from_saved_answer(answer)),
        dto::SavedJob::OnPhone { request, resuming } => Job::OnPhone(request, resuming),
        dto::SavedJob::OnMac(request) => Job::OnMac(request),
        dto::SavedJob::Import(request) => Job::Import(request),
        dto::SavedJob::Mirror(request) => Job::Mirror(request),
        dto::SavedJob::Check(request) => Job::Check(request),
    }
}

fn to_saved_answer(answer: Answer) -> dto::SavedAnswer {
    match answer {
        Answer::Unasked => dto::SavedAnswer::Unasked,
        Answer::NothingTaken => dto::SavedAnswer::NothingTaken,
        Answer::Chose(conflict) => dto::SavedAnswer::Chose(conflict),
        Answer::Retried(conflict) => dto::SavedAnswer::Retried(conflict),
    }
}

fn from_saved_answer(answer: dto::SavedAnswer) -> Answer {
    match answer {
        dto::SavedAnswer::Unasked => Answer::Unasked,
        dto::SavedAnswer::NothingTaken => Answer::NothingTaken,
        dto::SavedAnswer::Chose(conflict) => Answer::Chose(conflict),
        dto::SavedAnswer::Retried(conflict) => Answer::Retried(conflict),
    }
}

/// The paused transfer: how far it got, and Resume or Cancel. One waiting
/// for the phone has no Resume: it carries on when the phone is back.
fn paused_view(progress: &ProgressResponse, for_phone: bool) -> Element<'_, Message> {
    let (title, note) = if for_phone {
        (
            mapper::waiting_title(&progress.title),
            mapper::waiting_note(),
        )
    } else {
        (mapper::paused_title(&progress.title), mapper::paused_note())
    };
    let mut buttons = row![text(title).size(15), space::horizontal()]
        .spacing(8)
        .align_y(Alignment::Center);
    if !for_phone {
        buttons = buttons.push(
            button(text(t("label.resume")).size(13))
                .style(button::primary)
                .on_press(Message::Request(Request::Resume)),
        );
    }
    column![
        buttons.push(
            button(text(t("label.cancel")).size(13))
                .style(button::secondary)
                .on_press(Message::Request(Request::Cancel)),
        ),
        progress_bar(0.0..=1.0, progress.total_fraction).girth(8),
        text(note).size(12).style(text::secondary),
    ]
    .spacing(6)
    .into()
}

/// A phone file being edited: changed and saved, it offers to go back to
/// the phone; not yet, it can be let go of.
fn edit_row<'a>(edit: &'a Edit, open_phone: Option<&str>) -> Element<'a, Message> {
    let id = edit.copy.id.clone();
    let can_send = open_phone == Some(edit.copy.phone.as_str());
    let (title, note) = if edit.saved {
        (
            mapper::edit_saved_title(&edit.copy.name),
            if can_send {
                mapper::edit_send_note()
            } else {
                mapper::edit_phone_note()
            },
        )
    } else {
        (
            mapper::edit_open_title(&edit.copy.name),
            mapper::edit_open_note(),
        )
    };
    let mut buttons = row![
        button(
            text(if edit.saved {
                t("label.discard")
            } else {
                t("label.stop_editing")
            })
            .size(13)
        )
        .style(button::secondary)
        .on_press(Message::Request(Request::DiscardEdit(id.clone()))),
    ]
    .spacing(8);
    if edit.saved {
        buttons = buttons.push(
            button(text(t("transfer.edit.send_button")).size(13))
                .style(button::primary)
                .on_press_maybe(can_send.then_some(Message::Request(Request::SendEdit(id)))),
        );
    }
    row![
        text("✎").size(16),
        column![
            text(title).size(14),
            text(note).size(12).style(text::secondary),
        ]
        .spacing(2)
        .width(Length::Fill),
        buttons,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn history_row(index: usize, finished: &Finished) -> Element<'_, Message> {
    let (mark, title, detail): (&str, String, String) = match &finished.outcome {
        // Done, but not everything: say what is missing right there. A move
        // that left originals behind is not a clean one either.
        // A check that found copies not the same did its job, with bad news.
        Ok(summary)
            if !summary.failures.is_empty() || !summary.left.is_empty() || summary.mismatched =>
        {
            ("⚠", summary.title.clone(), summary.detail.clone())
        }
        Ok(summary) => ("✓", summary.title.clone(), summary.detail.clone()),
        Err(error) => (
            "✕",
            format!("{} — {}", finished.job.label(), error.title),
            error.hint.clone(),
        ),
    };
    // What failed, one line each, under the result, and what a move left behind.
    let (failures, notes): (&[String], &[String]) = match &finished.outcome {
        Ok(summary) => (&summary.failures, &summary.notes),
        Err(_) => (&[], &[]),
    };
    let mut lines = column![
        text(title).size(13),
        text(detail).size(11).style(text::secondary),
    ]
    .spacing(2);
    // The count at the end: what a person checks before deleting anything.
    if let Ok(SummaryResponse {
        arrival: Some(arrival),
        ..
    }) = &finished.outcome
    {
        lines = lines.push(text(arrival).size(11));
    }
    if let Ok(SummaryResponse {
        timing: Some(timing),
        ..
    }) = &finished.outcome
    {
        lines = lines.push(text(timing).size(11).style(text::secondary));
    }
    for failure in failures {
        lines = lines.push(text(failure).size(11).style(text::danger));
    }
    for note in notes {
        lines = lines.push(text(note).size(11));
    }
    let mut line = row![text(mark).size(14), lines, space::horizontal(),]
        .spacing(8)
        .align_y(Alignment::Center);
    match &finished.outcome {
        Ok(summary) if summary.reveal.is_some() => {
            line = line.push(
                button(text(t("menu.item.show_in_finder")).size(12))
                    .style(button::secondary)
                    .on_press(Message::Request(Request::ShowInFinder(index))),
            );
            // Retrying copies only what is missing: what arrived is skipped.
            if finished.is_retryable() {
                line = line.push(
                    button(text(retry_label(summary)).size(12))
                        .style(button::secondary)
                        .on_press(Message::Request(Request::Retry(index))),
                );
            }
        }
        Ok(summary) if finished.is_retryable() => {
            line = line.push(
                button(text(retry_label(summary)).size(12))
                    .style(button::secondary)
                    .on_press(Message::Request(Request::Retry(index))),
            );
        }
        Err(_) if finished.is_retryable() => {
            line = line.push(
                button(text(t("label.retry")).size(12))
                    .style(button::secondary)
                    .on_press(Message::Request(Request::Retry(index))),
            );
        }
        Ok(_) | Err(_) => {}
    }
    if matches!(&finished.outcome, Ok(summary) if !summary.report.is_empty()) {
        line = line.push(
            button(text(t("label.details")).size(12))
                .style(button::secondary)
                .on_press(Message::Request(Request::Details(index))),
        );
    }
    line.into()
}

/// A report row's height: rows are equal, so only those in view are built.
const REPORT_ROW: f32 = 22.0;
/// How tall the report's list is drawn.
const REPORT_HEIGHT: f32 = 200.0;

/// What became of each file of a finished transfer, in the finished list's
/// place: only the rows in view are built (a report can list 100,000).
fn report_view<'a>(
    summary: &'a SummaryResponse,
    offset: f32,
    note: Option<&'a str>,
) -> Element<'a, Message> {
    let count = summary.report.len();
    let shown = crate::handler::explorer::window::rows(offset, REPORT_HEIGHT, REPORT_ROW, count);
    let spacer = |rows: usize| space().height(rows as f32 * REPORT_ROW);
    let mut list = column![spacer(shown.start)];
    for row in &summary.report[shown.clone()] {
        list = list.push(
            row![
                text(row.mark).size(12).width(16),
                text(&row.path)
                    .size(12)
                    .width(Length::FillPortion(3))
                    .wrapping(text::Wrapping::None),
                text(&row.what)
                    .size(12)
                    .style(text::secondary)
                    .width(Length::FillPortion(3))
                    .wrapping(text::Wrapping::None),
                text(&row.original)
                    .size(12)
                    .style(text::secondary)
                    .width(Length::FillPortion(2))
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(8)
            .height(REPORT_ROW)
            .align_y(Alignment::Center)
            .clip(true),
        );
    }
    list = list.push(spacer(count - shown.end));

    let mut header = row![
        button(text(format!("‹ {}", t("label.finished"))).size(12))
            .style(button::text)
            .padding([0, 6])
            .on_press(Message::Request(Request::CloseDetails)),
        text(mapper::report_title(
            &summary.title,
            count,
            summary.report_left_out
        ))
        .size(12)
        .style(text::secondary),
        space::horizontal(),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if summary.report_folder.is_some() {
        header = header.push(
            button(text(t("label.save_report")).size(12))
                .style(button::secondary)
                .on_press(Message::Request(Request::SaveReport)),
        );
    }
    header = header.push(
        button(text(t("menu.item.copy")).size(12))
            .style(button::secondary)
            .on_press(Message::Request(Request::CopyReport)),
    );
    let mut view = column![
        header,
        scrollable(list)
            .height(REPORT_HEIGHT)
            .on_scroll(|viewport| Message::ReportScrolled(viewport.absolute_offset().y)),
    ]
    .spacing(4);
    if let Some(note) = note {
        view = view.push(text(note).size(11));
    }
    view.into()
}

/// "Retry failed" when files failed; just "Retry" when a move only has
/// originals left to take off the phone.
fn retry_label(summary: &SummaryResponse) -> &'static str {
    if summary.failures.is_empty() {
        t("label.retry")
    } else {
        t("label.retry_failed")
    }
}

/// The question goes on its own line and the choices on a row below, so
/// every button stays visible in a narrow window.
/// "Replace older" only for a copy: a move would put an original whose copy
/// was kept back in the Trash.
/// Where the taken names are, and so what the question offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// A download: the names are on the Mac. Replace older is offered for a
    /// move too: what it keeps back stays on the phone.
    OnMac,
    PhoneCopy,
    /// A move to the phone: no Replace older, since a kept-back item's
    /// original would go to the Trash.
    PhoneMove,
}

fn asking(existing: &[String], asked: Asked) -> Element<'_, Message> {
    let choice = |label, conflict| {
        button(text(label).size(13))
            .style(button::secondary)
            .on_press(Message::Request(Request::Resolve(conflict)))
    };
    let mut choices = row![
        space::horizontal(),
        button(text(t("label.cancel")).size(13))
            .style(button::text)
            .on_press(Message::Request(Request::Cancel)),
        choice(t("label.skip"), dto::Conflict::Skip),
        choice(t("label.keep_both"), dto::Conflict::KeepBoth),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if asked != Asked::PhoneMove {
        choices = choices.push(choice(
            t("label.replace_older"),
            dto::Conflict::ReplaceOlder,
        ));
    }
    column![
        text(mapper::conflict_title(existing, asked == Asked::OnMac)).size(15),
        text(match asked {
            Asked::OnMac => t("transfer.ask.on_mac"),
            Asked::PhoneCopy => t("transfer.ask.phone_copy"),
            Asked::PhoneMove => t("transfer.ask.phone_move"),
        })
        .size(12)
        .style(text::secondary),
        choices.push(
            button(text(t("label.replace")).size(13))
                .style(button::danger)
                .on_press(Message::Request(Request::Resolve(dto::Conflict::Replace))),
        ),
    ]
    .spacing(6)
    .into()
}

/// Whether the running transfer is being stopped, and whether it can be
/// paused.
struct Stopping {
    cancelling: bool,
    pausing: bool,
    can_pause: bool,
}

fn running(
    progress: &ProgressResponse,
    stopping: Stopping,
    awake: bool,
    link: Option<dto::UsbSpeed>,
) -> Element<'_, Message> {
    let Stopping {
        cancelling,
        pausing,
        can_pause,
    } = stopping;
    let cancel = button(
        text(if cancelling {
            t("label.cancelling")
        } else {
            t("label.cancel")
        })
        .size(13),
    )
    .style(button::secondary)
    .on_press_maybe((!cancelling).then_some(Message::Request(Request::Cancel)));
    let mut buttons = row![].spacing(8).align_y(Alignment::Center);
    if can_pause {
        buttons = buttons.push(
            button(
                text(if pausing {
                    t("label.pausing")
                } else {
                    t("label.pause")
                })
                .size(13),
            )
            .style(button::secondary)
            .on_press_maybe((!pausing && !cancelling).then_some(Message::Request(Request::Pause))),
        );
    }
    buttons = buttons.push(cancel);

    let mut panel = column![
        row![text(&progress.title).size(15), space::horizontal(), buttons]
            .align_y(Alignment::Center),
        text(&progress.current_file).size(12).style(text::secondary),
        progress_bar(0.0..=1.0, progress.file_fraction).girth(4),
        progress_bar(0.0..=1.0, progress.total_fraction).girth(8),
        text(&progress.detail).size(12).style(text::secondary),
    ]
    .spacing(6);
    if let Some(note) = &progress.note {
        panel = panel.push(
            text(mapper::with_link(mapper::with_awake(note, awake), link))
                .size(12)
                .style(text::secondary),
        );
    }
    panel.into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::handler::testing::FakeTransfer;

    fn download_request() -> dto::DownloadRequest {
        dto::DownloadRequest {
            storage_id: 1,
            source: "/DCIM".into(),
            names: vec!["a.jpg".into()],
            destination: "/Users/me/Downloads".into(),
        }
    }

    fn upload_request() -> dto::UploadRequest {
        dto::UploadRequest {
            sources: vec!["/Users/me/notes.txt".into()],
            storage_id: 1,
            destination: "/".into(),
        }
    }

    fn summary() -> dto::TransferSummary {
        dto::TransferSummary {
            files: 1,
            bytes: 4,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("a.jpg".into()),
            ..Default::default()
        }
    }

    fn progress(bytes_done: u64) -> dto::TransferProgress {
        dto::TransferProgress {
            current_file: "a.jpg".into(),
            files_total: 1,
            file_bytes_total: 4,
            file_bytes_done: bytes_done,
            bytes_total: 4,
            bytes_done,
            elapsed: Duration::from_secs(2),
            ..dto::TransferProgress::default()
        }
    }

    fn handler_with(service: FakeTransfer) -> (Handler, Arc<FakeTransfer>) {
        let service = Arc::new(service);
        (Handler::new(service.clone()), service)
    }

    fn handler() -> Handler {
        handler_with(FakeTransfer::new(Ok(summary()))).0
    }

    fn checked(existing: Result<Vec<String>, dto::Error>) -> Message {
        Message::Checked {
            request: upload_request(),
            result: existing.map_err(error::to_error_response),
        }
    }

    fn request(handler: &mut Handler, request: Request) {
        let _ = handler.update(Message::Request(request));
    }

    fn finish_ok(handler: &mut Handler) {
        let _ = handler.update(Message::Finished(Ok(summary())));
    }

    fn finish_err(handler: &mut Handler, err: dto::Error) {
        let _ = handler.update(Message::Finished(Err(error::to_error_response(err))));
    }

    fn stage(handler: &Handler) -> Option<&Stage> {
        handler.current.as_ref().map(|current| &current.stage)
    }

    fn in_thai<Output>(work: impl FnOnce() -> Output) -> Output {
        crate::i18n::with_language(dto::Language::Thai, work)
    }

    #[test]
    fn a_language_switch_rewords_what_a_job_says_before_its_first_report() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let Some(Current {
            stage:
                Stage::Running {
                    progress,
                    last: None,
                    ..
                },
            ..
        }) = &handler.current
        else {
            panic!("it is preparing");
        };
        let english = progress.title.clone();
        let in_thai_now = in_thai(|| mapper::preparing_response(dto::Direction::Download, false));
        assert_ne!(in_thai_now.title, english, "the catalog has Thai wording");

        in_thai(|| handler.language_changed());

        let Some(Current {
            stage: Stage::Running { progress, .. },
            ..
        }) = &handler.current
        else {
            panic!("still preparing");
        };
        assert_eq!(progress.title, in_thai_now.title);
        assert_eq!(progress.detail, in_thai_now.detail);
    }

    #[test]
    fn a_language_switch_rewords_a_finished_row_from_what_it_came_from() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        let english = handler.history[0].outcome.clone().expect("it finished");
        let in_thai_now = in_thai(|| mapper::to_summary_response(&summary()));
        assert_ne!(
            in_thai_now.title, english.title,
            "the catalog has Thai wording"
        );

        in_thai(|| handler.language_changed());

        let reworded = handler.history[0].outcome.as_ref().expect("still finished");
        assert_eq!(reworded.title, in_thai_now.title);
        assert_eq!(reworded.detail, in_thai_now.detail);
        assert_eq!(reworded.reveal, english.reveal);
        assert_eq!(reworded.left, english.left);
        assert_eq!(reworded.renamed, english.renamed);
    }

    #[test]
    fn a_files_failure_is_noted_once_and_not_again_when_the_words_change() {
        error::clear_recent();
        let mut handler = handler();
        let _ = handler.download(download_request());
        let failed = dto::TransferSummary {
            failures: vec![dto::FileFailure {
                label: "a.jpg".into(),
                error: dto::Error::MacDiskFull,
            }],
            ..summary()
        };
        let _ = handler.update(Message::Finished(Ok(failed)));

        in_thai(|| handler.language_changed());

        let problems = error::recent_problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].title, "Your Mac's disk is full");
        assert_eq!(problems[0].times, 1);
    }

    #[test]
    fn a_failed_row_is_said_again_from_its_error() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        in_thai(|| handler.language_changed());

        let Err(error) = &handler.history[0].outcome else {
            panic!("it failed");
        };
        let said = in_thai(|| error::describe_again(&dto::Error::MacDiskFull).title);
        assert_ne!(said, "Your Mac's disk is full");
        assert_eq!(error.title, said);
        assert!(handler.history[0].is_retryable());
    }

    #[test]
    fn a_language_switch_clears_what_was_said_about_a_report_or_an_edit() {
        let mut handler = handler();
        handler.report_note = Some("Saved “a.tsv” in “Trip”".into());
        handler.edit_note = Some("Couldn't open it for editing: x".into());

        in_thai(|| handler.language_changed());

        assert!(handler.report_note.is_none());
        assert!(handler.edit_note.is_none());
    }

    #[test]
    fn a_transfer_running_through_the_switch_finishes_the_same_in_the_new_language() {
        let run = |switch: bool| {
            let mut handler = handler();
            let _ = handler.download(download_request());
            let _ = handler.update(Message::Progress(progress(2)));
            if switch {
                in_thai(|| handler.language_changed());
            }
            in_thai(|| {
                let _ = handler.update(Message::Progress(progress(3)));
            });
            let title = running_title(&handler);
            in_thai(|| finish_ok(&mut handler));
            (handler, title)
        };

        let (switched, title) = run(true);

        assert_eq!(
            title,
            in_thai(|| mapper::to_progress_response(&progress(3)).title)
        );
        assert_ne!(
            title, "Downloading 1 of 1",
            "the next progress text is in Thai"
        );
        let (plain, _) = run(false);
        let (a, b) = (
            switched.history[0].outcome.as_ref().unwrap(),
            plain.history[0].outcome.as_ref().unwrap(),
        );
        assert_eq!(
            (&a.reveal, &a.left, a.renamed),
            (&b.reveal, &b.left, b.renamed)
        );
        assert_eq!(switched.history.len(), 1);
    }

    #[test]
    fn a_paused_transfer_is_said_again_in_the_new_language() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Progress(progress(2)));
        request(&mut handler, Request::Pause);
        finish_err(&mut handler, dto::Error::Cancelled);
        let paused = |handler: &Handler| {
            handler
                .paused
                .as_ref()
                .expect("paused")
                .progress
                .title
                .clone()
        };
        assert_eq!(paused(&handler), "Downloading 1 of 1");

        in_thai(|| handler.language_changed());

        assert_eq!(
            paused(&handler),
            in_thai(|| mapper::to_progress_response(&progress(2)).title)
        );
        assert_ne!(paused(&handler), "Downloading 1 of 1");
    }

    #[test]
    fn starts_idle_and_hidden() {
        let handler = handler();

        assert!(!handler.is_visible());
        assert!(!handler.is_running());
        let _ = handler.view();
    }

    #[test]
    fn a_download_shows_preparing_until_progress_arrives() {
        let mut handler = handler();

        let _ = handler.download(download_request());

        assert!(handler.is_running());
        assert!(
            matches!(stage(&handler), Some(Stage::Running { progress, .. })
            if progress.title == "Preparing download…")
        );
    }

    #[test]
    fn progress_updates_the_panel() {
        let mut handler = handler();
        let _ = handler.download(download_request());

        let _ = handler.update(Message::Progress(progress(2)));

        assert!(
            matches!(stage(&handler), Some(Stage::Running { progress, .. })
            if progress.title == "Downloading 1 of 1" && progress.total_fraction == 0.5)
        );
    }

    #[test]
    fn a_second_transfer_waits_its_turn() {
        let mut handler = handler();
        let _ = handler.download(download_request());

        let _ = handler.upload(upload_request());
        let _ = handler.download(download_request());

        assert_eq!(handler.waiting.len(), 2);
        let _ = handler.view();
    }

    #[test]
    fn when_one_finishes_the_next_starts() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.upload(upload_request());

        finish_ok(&mut handler);

        assert!(matches!(stage(&handler), Some(Stage::Checking)));
        assert!(handler.waiting.is_empty());
        assert_eq!(handler.history.len(), 1);
    }

    #[test]
    fn a_waiting_transfer_can_be_removed() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.upload(upload_request());

        request(&mut handler, Request::Remove(0));
        request(&mut handler, Request::Remove(5));

        assert!(handler.waiting.is_empty());
        assert!(handler.is_running());
    }

    #[test]
    fn cancel_flips_the_shared_switch_once() {
        let mut handler = handler();
        let _ = handler.download(download_request());

        request(&mut handler, Request::Cancel);

        let Some(Stage::Running {
            cancel, cancelling, ..
        }) = stage(&handler)
        else {
            panic!("still running until the service stops");
        };
        assert!(cancel.is_cancelled());
        assert!(*cancelling);
        let _ = handler.view();
    }

    #[test]
    fn an_upload_first_checks_the_phone() {
        let mut handler = handler();

        let _ = handler.upload(upload_request());

        assert!(matches!(stage(&handler), Some(Stage::Checking)));
        let _ = handler.view();
    }

    #[test]
    fn an_upload_with_nothing_taken_starts_without_asking() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());

        let _ = handler.update(checked(Ok(vec![])));

        assert!(
            matches!(stage(&handler), Some(Stage::Running { progress, .. })
            if progress.title == "Preparing upload…")
        );
    }

    #[test]
    fn taken_names_are_asked_about_before_anything_is_sent() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());

        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));

        assert!(matches!(stage(&handler), Some(Stage::Asking { existing })
            if existing == &["notes.txt".to_owned()]));
        let _ = handler.view();
    }

    #[test]
    fn answering_starts_the_upload() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));

        request(&mut handler, Request::Resolve(dto::Conflict::Replace));

        assert!(matches!(stage(&handler), Some(Stage::Running { .. })));
    }

    #[tokio::test]
    async fn a_copy_may_replace_only_older_files_and_a_move_is_not_offered_it() {
        let service = Arc::new(FakeTransfer::new(Ok(summary())));
        let mut copying = Handler::new(service.clone());
        let _ = copying.upload(upload_request());
        let _ = copying.update(checked(Ok(vec!["notes.txt".into()])));
        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(copying.view());
            assert!(ui.find("Replace a file only with a newer one").is_err());
            let _ = ui.click("Replace older").unwrap();
            ui.into_messages().collect()
        };
        let mut task = Task::none();
        for message in clicked {
            task = copying.update(message);
        }
        let _ = crate::handler::testing::drain(task).await;
        assert_eq!(
            *service.uploads.lock().unwrap(),
            [(upload_request(), dto::Conflict::ReplaceOlder)]
        );

        let mut moving = handler();
        let _ = moving.upload_as(dto::Paste::Move, upload_request());
        let _ = moving.update(checked(Ok(vec!["notes.txt".into()])));
        let mut ui = iced_test::simulator(moving.view());
        assert!(
            ui.find("Replace older").is_err(),
            "a move would trash what it kept back"
        );
        assert!(ui.find("Replace").is_ok());
    }

    #[test]
    fn replace_older_carries_on_after_a_pause_and_stands_for_a_retry() {
        let copy = Job::Upload(
            upload_request(),
            dto::Paste::Copy,
            Answer::Chose(dto::Conflict::ReplaceOlder),
        );
        assert!(matches!(
            copy.resumed(),
            Some(Job::Upload(
                _,
                dto::Paste::Copy,
                Answer::Retried(dto::Conflict::ReplaceOlder)
            ))
        ));
        assert_eq!(
            Answer::Chose(dto::Conflict::ReplaceOlder).for_a_retry(),
            Answer::Retried(dto::Conflict::ReplaceOlder)
        );
        assert_eq!(
            Answer::Retried(dto::Conflict::ReplaceOlder).conflict(),
            dto::Conflict::ReplaceOlder
        );
    }

    #[tokio::test]
    async fn a_download_onto_names_the_mac_has_asks_first_and_copies_nothing_until_answered() {
        let service = Arc::new(FakeTransfer {
            taken_on_mac: vec!["notes.txt".into()],
            ..FakeTransfer::new(Ok(summary()))
        });
        let mut handler = Handler::new(service.clone());

        let task = handler.download(download_request());
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert!(
            matches!(stage(&handler), Some(Stage::Asking { existing }) if existing == &["notes.txt".to_owned()])
        );
        assert!(
            service.requests.lock().unwrap().is_empty(),
            "nothing copied yet"
        );
        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("“notes.txt” already exists on your Mac").is_ok());
            let _ = ui.click("Replace older").unwrap();
            ui.into_messages().collect()
        };
        let mut task = Task::none();
        for message in clicked {
            task = handler.update(message);
        }
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            *service.download_conflicts.lock().unwrap(),
            [dto::Conflict::ReplaceOlder]
        );
        assert_eq!(*service.requests.lock().unwrap(), [download_request()]);
        assert!(matches!(
            &handler.history[0].job,
            Job::Download(_, _, Answer::Chose(dto::Conflict::ReplaceOlder))
        ));
    }

    #[tokio::test]
    async fn a_download_with_nothing_taken_goes_straight_on_as_before() {
        let service = Arc::new(FakeTransfer::new(Ok(summary())));
        let mut handler = Handler::new(service.clone());

        let task = handler.download_as(dto::Paste::Move, download_request());
        assert!(
            matches!(stage(&handler), Some(Stage::Running { .. })),
            "preparing at once"
        );
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            *service.download_conflicts.lock().unwrap(),
            [dto::Conflict::KeepBoth]
        );
        assert!(!handler.is_running());
    }

    #[tokio::test]
    async fn a_move_onto_taken_names_is_offered_replace_older_too() {
        let service = Arc::new(FakeTransfer {
            taken_on_mac: vec!["a.jpg".into(), "b.jpg".into()],
            ..FakeTransfer::new(Ok(summary()))
        });
        let mut handler = Handler::new(service);
        let task = handler.download_as(dto::Paste::Move, download_request());
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("2 items already exist on your Mac").is_ok());
        assert!(
            ui.find("Replace older").is_ok(),
            "what it keeps back stays on the phone"
        );
    }

    /// Every message the handler's tasks send, each fed back, until nothing
    /// is pending.
    async fn run_to_the_end(handler: &mut Handler, task: Task<Message>) {
        let mut pending = vec![task];
        while let Some(task) = pending.pop() {
            for message in crate::handler::testing::drain(task).await.0 {
                pending.push(handler.update(message));
            }
        }
    }

    fn crashed(handler: &Handler, at: usize) -> bool {
        matches!(
            &handler.history[at].outcome,
            Err(err) if err.title == "Something went wrong inside the app"
        )
    }

    #[tokio::test]
    async fn a_service_that_panics_ends_its_job_as_a_failure_and_the_queue_goes_on() {
        let service = Arc::new(FakeTransfer {
            panics_on_download: true,
            ..FakeTransfer::new(Ok(summary()))
        });
        let mut handler = Handler::new(service);
        // Two waiting one behind the other: if the first stayed "running", the
        // second would never start.
        let first = handler.download(download_request());
        let second = handler.download(download_request());

        run_to_the_end(&mut handler, Task::batch([first, second])).await;

        assert!(!handler.is_running(), "nothing is left running");
        assert_eq!(handler.history.len(), 2, "both jobs finished");
        assert!(crashed(&handler, 0) && crashed(&handler, 1));
        let _ = handler.view();
    }

    #[tokio::test]
    async fn a_panic_before_the_job_awaits_anything_is_the_jobs_failure_too() {
        let service: Arc<dyn Service> = Arc::new(FakeTransfer::new(Ok(summary())));

        let messages: Vec<Message> = transfer_events(service, dto::Cancel::new(), |_, _, _| {
            panic!("the closure broke while making the work")
        })
        .collect()
        .await;

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(matches!(
            &messages[0],
            Message::Finished(Err(err)) if err.title == "Something went wrong inside the app"
        ));
    }

    #[tokio::test]
    async fn a_job_that_does_not_panic_is_untouched_by_the_guard() {
        let service: Arc<dyn Service> = Arc::new(FakeTransfer::new(Ok(summary())));

        let messages: Vec<Message> = transfer_events(service, dto::Cancel::new(), |_, _, _| {
            Box::pin(async { Ok(summary()) })
        })
        .collect()
        .await;

        assert!(matches!(messages.last(), Some(Message::Finished(Ok(done))) if done.files == 1));
    }

    #[tokio::test]
    async fn the_guard_hands_back_what_the_work_answered() {
        assert_eq!(guarded(async { Ok::<_, dto::Error>(7) }).await, Ok(7));
        assert_eq!(
            guarded(async { Err::<u8, _>(dto::Error::Cancelled) }).await,
            Err(dto::Error::Cancelled)
        );
        assert_eq!(
            guarded(async { panic!("broke") as Result<u8, dto::Error> }).await,
            Err(dto::Error::Crashed)
        );
    }

    #[tokio::test]
    async fn a_mac_folder_that_cannot_be_looked_at_fails_the_download() {
        let service = Arc::new(FakeTransfer {
            mac_unreadable: true,
            ..FakeTransfer::new(Ok(summary()))
        });
        let mut handler = Handler::new(service.clone());
        let task = handler.download(download_request());
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert!(!handler.is_running());
        assert!(handler.history[0].outcome.is_err());
        assert!(service.requests.lock().unwrap().is_empty());
        // A late answer about another download changes nothing.
        let _ = handler.update(Message::CheckedOnMac {
            request: download_request(),
            result: Ok(vec!["x".into()]),
        });
        assert!(handler.current.is_none());
    }

    #[test]
    fn an_answer_without_a_question_does_nothing() {
        let mut handler = handler();
        request(&mut handler, Request::Resolve(dto::Conflict::Skip));
        assert!(!handler.is_running());

        let _ = handler.download(download_request());
        request(&mut handler, Request::Resolve(dto::Conflict::Skip));
        assert!(matches!(stage(&handler), Some(Stage::Running { .. })));
    }

    #[test]
    fn cancelling_a_question_drops_that_upload_and_moves_on() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());
        let _ = handler.download(download_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));

        request(&mut handler, Request::Cancel);

        assert!(matches!(stage(&handler), Some(Stage::Running { .. })));
        assert!(handler.history.is_empty());
    }

    #[test]
    fn cancel_while_checking_or_idle() {
        let mut handler = handler();
        request(&mut handler, Request::Cancel);
        assert!(!handler.is_visible());

        let _ = handler.upload(upload_request());
        request(&mut handler, Request::Cancel);
        assert!(!handler.is_visible());
    }

    #[test]
    fn a_check_answer_for_something_else_is_ignored() {
        let mut handler = handler();
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));
        assert!(!handler.is_running());

        let _ = handler.download(download_request());
        let _ = handler.update(checked(Ok(vec![])));
        assert!(
            matches!(stage(&handler), Some(Stage::Running { progress, .. })
            if progress.title == "Preparing download…")
        );
    }

    #[test]
    fn a_failed_check_goes_to_the_history() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());

        let _ = handler.update(checked(Err(dto::Error::ReadOnly)));

        assert!(!handler.is_running());
        assert!(
            matches!(&handler.history[0].outcome, Err(e) if e.title == "This storage is read-only")
        );
    }

    #[test]
    fn finished_transfers_stay_until_cleared() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        assert!(handler.is_visible());
        assert!(
            matches!(&handler.history[0].outcome, Ok(s) if s.title == "Downloaded 1 file (4 B)")
        );
        let _ = handler.view();

        request(&mut handler, Request::Clear);
        assert!(!handler.is_visible());
    }

    #[test]
    fn a_failed_transfer_can_be_retried() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::Cancelled);
        let _ = handler.view();

        request(&mut handler, Request::Retry(0));

        assert!(handler.history.is_empty());
        assert!(
            matches!(&handler.current, Some(Current { job: Job::Download(r, _, _), .. }) if *r == download_request())
        );
    }

    #[test]
    fn a_failed_phone_transfer_is_not_retried_on_another_phone() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);
        let _ = handler.paste_on_mac(paste_request(dto::Paste::Copy));
        finish_err(&mut handler, dto::Error::Cancelled);

        handler.forget_phone();
        assert!(!handler.history[0].is_retryable(), "the phone's job");
        assert!(handler.history[1].is_retryable(), "the Mac's own job");
        let _ = handler.view();
        request(&mut handler, Request::Retry(0));
        assert!(handler.current.is_none(), "the phone job is not run again");

        request(&mut handler, Request::Retry(1));
        assert_eq!(handler.history.len(), 1, "the phone job stays listed");
        assert!(
            matches!(
                &handler.current,
                Some(Current {
                    job: Job::OnMac(_),
                    ..
                })
            ),
            "the Mac-only job can still be retried"
        );
    }

    fn import_request() -> dto::ImportRequest {
        dto::ImportRequest {
            storage_id: 5,
            destination: "/Users/me/Pictures".into(),
            folders: dto::ImportFolders::Day,
            remove_from_phone: false,
        }
    }

    fn running_title(handler: &Handler) -> String {
        match stage(handler) {
            Some(Stage::Running { progress, .. }) => progress.title.clone(),
            _ => panic!("not running"),
        }
    }

    #[tokio::test]
    async fn an_import_runs_in_the_queue_as_phone_work_and_says_importing() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(dto::TransferSummary {
            files: 3,
            bytes: 3_000_000,
            already_there: 4_898,
            destination: "/Users/me/Pictures".into(),
            first_item: Some("2026-10-17/a.jpg".into()),
            arrival: Some(dto::Arrival {
                files: 3,
                bytes: 3_000_000,
                there: 3,
                checked: Some(3),
                complete: true,
            }),
            ..Default::default()
        })));

        let task = handler.import(import_request());
        assert!(handler.has_phone_work());
        assert_eq!(running_title(&handler), "Looking for new photos…");
        let _ = handler.update(Message::Progress(progress(500)));
        assert!(
            running_title(&handler).starts_with("Importing "),
            "{}",
            running_title(&handler)
        );
        let _ = handler.view();
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(*service.imports.lock().unwrap(), [import_request()]);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Imported 3 photos and videos (3.0 MB)").is_ok());
        assert!(ui.find("to Pictures · 4,898 imported before").is_ok());
        assert!(ui.find("3 files, 3.0 MB — all arrived").is_ok());
    }

    #[test]
    fn a_failed_import_is_retried_as_it_was_asked_and_not_on_another_phone() {
        let mut handler = handler();
        let _ = handler.import(import_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);
        assert_eq!(
            handler.history[0].job.label(),
            "Import new photos to Pictures"
        );

        request(&mut handler, Request::Retry(0));
        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Import(r), .. }) if *r == import_request()
        ));

        finish_err(&mut handler, dto::Error::MacDiskFull);
        handler.forget_phone();
        assert!(!handler.history[0].is_retryable());
    }

    fn mirror_request(direction: dto::MirrorDirection) -> dto::MirrorRequest {
        dto::MirrorRequest {
            storage_id: 5,
            phone_folder: "/Backup".into(),
            mac_folder: "/Users/me/Backup".into(),
            direction,
            remove_extras: true,
        }
    }

    #[tokio::test]
    async fn a_mirror_runs_in_the_queue_says_mirroring_and_reports_what_it_did() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(dto::TransferSummary {
            direction: dto::Direction::Download,
            files: 5,
            bytes: 5_000_000,
            destination: "/Users/me/Backup".into(),
            mirrored: Some(dto::Mirrored {
                copied: 3,
                replaced: 2,
                removed: 1,
                kept: 0,
                conflicts: 1,
            }),
            ..Default::default()
        })));

        let task = handler.mirror(mirror_request(dto::MirrorDirection::ToMac));
        assert!(handler.has_phone_work());
        assert_eq!(running_title(&handler), "Comparing the folders…");
        let _ = handler.update(Message::Progress(progress(500)));
        assert!(
            running_title(&handler).starts_with("Mirroring "),
            "{}",
            running_title(&handler)
        );
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            *service.mirrors.lock().unwrap(),
            [mirror_request(dto::MirrorDirection::ToMac)]
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find("Mirrored: 3 files copied, 2 files replaced, 1 item removed")
                .is_ok()
        );
        assert!(
            ui.find("into Backup · removed and replaced ones are in the Trash · 1 left alone (names clash)")
                .is_ok()
        );
    }

    fn check_request() -> dto::CheckRequest {
        dto::CheckRequest {
            storage_id: 5,
            phone_folder: "/DCIM".into(),
            mac_folder: "/Users/me/Backup".into(),
            names: vec!["Camera".into(), "notes.txt".into()],
        }
    }

    fn check_line(path: &str, outcome: dto::FileOutcome) -> dto::FileReport {
        dto::FileReport {
            path: path.into(),
            from: None,
            outcome,
            original: None,
        }
    }

    #[tokio::test]
    async fn a_check_runs_in_the_queue_says_checking_and_reports_file_by_file() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(dto::TransferSummary {
            direction: dto::Direction::Download,
            files: 4,
            bytes: 4_000_000,
            destination: "/Users/me/Backup".into(),
            first_item: Some("Camera".into()),
            compared: Some(dto::Compared {
                identical: 2,
                differ: 1,
                missing: 1,
                unreadable: 0,
            }),
            report: vec![
                check_line("Camera/a.jpg", dto::FileOutcome::Identical),
                check_line("Camera/b.jpg", dto::FileOutcome::Differs { at: 1_048_575 }),
                check_line("Camera/c.jpg", dto::FileOutcome::NotOnMac),
                check_line("notes.txt", dto::FileOutcome::Identical),
            ],
            ..Default::default()
        })));

        let task = handler.check(check_request());
        assert!(handler.has_phone_work(), "it reads the phone");
        assert_eq!(running_title(&handler), "Checking contents…");
        let _ = handler.update(Message::Progress(progress(500)));
        assert!(
            running_title(&handler).starts_with("Checking "),
            "{}",
            running_title(&handler)
        );
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(*service.checks.lock().unwrap(), [check_request()]);
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find("Checked 4 files: 1 differs, 1 on one side only, 2 identical")
                .is_ok()
        );
        assert!(
            ui.find("against Backup on your Mac · nothing was changed")
                .is_ok()
        );
        assert!(
            ui.find("Show in Finder").is_err(),
            "nothing arrived anywhere"
        );
        assert!(ui.find("⚠").is_ok(), "copies not the same are bad news");
        drop(ui);
        request(&mut handler, Request::Details(0));
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Differs at byte 1,048,576").is_ok());
        assert!(ui.find("Not on the Mac").is_ok());
        assert!(
            ui.find("Save Report").is_err(),
            "no file written into a folder only looked at"
        );
    }

    fn working_copy() -> dto::WorkingCopy {
        dto::WorkingCopy {
            id: "c0ffee".into(),
            phone: "ABC123".into(),
            storage_id: 5,
            folder: "/Documents".into(),
            name: "notes.txt".into(),
            path: "/support/editing/c0ffee/notes.txt".into(),
            fetched: dto::CopyState {
                size: 11,
                modified_ms: 1_000,
            },
            phone_size: 11,
            phone_date: "2026-10-18 09:00:00".into(),
        }
    }

    fn state(size: u64, modified_ms: u64) -> Option<dto::CopyState> {
        Some(dto::CopyState { size, modified_ms })
    }

    /// A handler editing `working_copy`, its phone open.
    async fn editing(fake: FakeTransfer) -> (Handler, Arc<FakeTransfer>) {
        let (mut handler, service) = handler_with(FakeTransfer {
            edit: Ok(working_copy()),
            ..fake
        });
        handler.set_open_phone(Some("ABC123".into()));
        let task = handler.edit(dto::EditRequest {
            phone: "ABC123".into(),
            storage_id: 5,
            folder: "/Documents".into(),
            name: "notes.txt".into(),
        });
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
        (handler, service)
    }

    async fn look(handler: &mut Handler, service: &FakeTransfer, now: Option<dto::CopyState>) {
        service
            .copy_states
            .lock()
            .unwrap()
            .insert(working_copy().path, now);
        let task = handler.update(Message::CheckEdits);
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    #[tokio::test]
    async fn an_edit_is_offered_back_once_its_copy_was_saved_and_stayed_still() {
        let (mut handler, service) = editing(FakeTransfer::new(Ok(summary()))).await;
        assert!(handler.has_edits());
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("notes.txt is open for editing on the Mac").is_ok());
        assert!(ui.find("Send to Phone").is_err(), "nothing to send yet");
        drop(ui);

        // Unchanged, then being written, then still: only then offered.
        look(&mut handler, &service, state(11, 1_000)).await;
        assert!(!handler.edits[0].saved);
        look(&mut handler, &service, state(30, 5_000)).await;
        assert!(!handler.edits[0].saved, "it may still be being written");
        look(&mut handler, &service, state(30, 5_000)).await;
        assert!(handler.edits[0].saved);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("notes.txt was changed on the Mac").is_ok());
        assert!(ui.find(mapper::edit_send_note()).is_ok());
    }

    #[tokio::test]
    async fn sending_queues_one_upload_and_the_row_goes_once_the_phone_has_it() {
        let (mut handler, service) = editing(FakeTransfer::new(Ok(dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: 1,
            replaced: 1,
            destination: "/Documents".into(),
            report: vec![check_line("notes.txt", dto::FileOutcome::ReplacedOnPhone)],
            ..Default::default()
        })))
        .await;
        look(&mut handler, &service, state(30, 5_000)).await;
        look(&mut handler, &service, state(30, 5_000)).await;

        let task = handler.update(Message::Request(Request::SendEdit("c0ffee".into())));
        assert!(
            crate::handler::testing::drain(
                handler.update(Message::Request(Request::SendEdit("c0ffee".into())))
            )
            .await
            .0
            .is_empty(),
            "once"
        );
        assert!(handler.has_phone_work());
        assert!(
            running_title(&handler).starts_with("Preparing upload"),
            "{}",
            running_title(&handler)
        );
        assert!(
            handler.saved().is_empty(),
            "the working copy is kept instead"
        );
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(*service.sent_edits.lock().unwrap(), [working_copy()]);
        assert!(!handler.has_edits());
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Sent “notes.txt” back to the phone").is_ok());
        assert!(ui.find("it replaced the phone's copy").is_ok());
    }

    #[tokio::test]
    async fn an_edit_sent_beside_a_changed_phone_file_says_so() {
        let response = mapper::to_edit_summary(
            &dto::TransferSummary {
                renamed: 1,
                report: vec![check_line("notes (1).txt", dto::FileOutcome::Sent)],
                ..Default::default()
            },
            "notes.txt",
        );
        assert_eq!(response.title, "Sent your edit as “notes (1).txt”");
        assert_eq!(
            response.detail,
            "notes.txt had changed on the phone too, so both are kept"
        );
        assert_eq!(response.reveal, None);
    }

    #[tokio::test]
    async fn an_edit_waits_for_its_own_phone_and_a_lost_phone_holds_the_send() {
        let (mut handler, service) = editing(FakeTransfer::new(Ok(summary()))).await;
        look(&mut handler, &service, state(30, 5_000)).await;
        look(&mut handler, &service, state(30, 5_000)).await;

        handler.set_open_phone(Some("OTHER".into()));
        request(&mut handler, Request::SendEdit("c0ffee".into()));
        assert!(handler.current.is_none(), "another phone is open");
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find(mapper::edit_phone_note()).is_ok());
        drop(ui);

        handler.set_open_phone(Some("ABC123".into()));
        request(&mut handler, Request::SendEdit("c0ffee".into()));
        let _ = handler.update(Message::Progress(progress(1)));
        let _ = handler.update(Message::Finished(Err(error::to_error_response(
            dto::Error::Disconnected,
        ))));
        assert!(handler.waits_for_phone(), "as any transfer does (93)");
        assert!(handler.has_edits(), "the copy is still there");
    }

    #[tokio::test]
    async fn a_delete_keeping_copies_runs_in_the_queue_and_says_what_stayed() {
        let request = dto::DeleteRequest {
            storage_id: 5,
            folder: "/DCIM".into(),
            names: vec!["Camera".into(), "a.jpg".into()],
        };
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(dto::TransferSummary {
            direction: dto::Direction::Download,
            destination: "/DCIM".into(),
            removal: Some(dto::Removal {
                removed: 7,
                kept: vec![dto::Kept {
                    label: "Camera".into(),
                    why: dto::Why::NotCopied,
                }],
                ..Default::default()
            }),
            ..Default::default()
        })));

        let task = handler.delete_keeping(request.clone());
        let job = handler.current.as_ref().unwrap().job.clone();
        assert_eq!(job.label(), "Delete 2 items, keeping copies in the Trash");
        assert_eq!(job.resumed(), None, "no Pause: a stop deletes nothing more");
        assert!(handler.saved().is_empty(), "never carried over a quit");
        assert!(handler.has_phone_work());
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(*service.deletes.lock().unwrap(), [request]);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Deleted 7 files from the phone").is_ok());
        assert!(ui.find("copies are in the Mac's Trash").is_ok());
        assert!(
            ui.find("Camera stayed on the phone: it can't be copied")
                .is_ok()
        );
        assert_eq!(
            mapper::delete_keeping_label(&["a.jpg".into()]),
            "Delete “a.jpg”, keeping a copy in the Trash"
        );
    }

    #[tokio::test]
    async fn an_edit_waiting_to_go_back_is_dropped_when_another_phone_opens() {
        let (mut handler, service) = editing(FakeTransfer::new(Ok(summary()))).await;
        look(&mut handler, &service, state(30, 5_000)).await;
        look(&mut handler, &service, state(30, 5_000)).await;
        // Something runs, so the send waits behind it.
        let _ = handler.download(download_request());
        request(&mut handler, Request::SendEdit("c0ffee".into()));
        assert_eq!(handler.waiting.len(), 1);

        handler.forget_phone();

        assert!(handler.waiting.is_empty(), "never run on the next phone");
        assert!(handler.has_edits(), "its working copy stays");
    }

    #[tokio::test]
    async fn discarding_forgets_the_copy_and_a_copy_gone_from_the_mac_goes_too() {
        let (mut handler, service) = editing(FakeTransfer::new(Ok(summary()))).await;

        let task = handler.update(Message::Request(Request::DiscardEdit("c0ffee".into())));
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
        assert_eq!(*service.discarded.lock().unwrap(), ["c0ffee"]);
        assert!(!handler.has_edits());

        let (mut gone, service) = editing(FakeTransfer::new(Ok(summary()))).await;
        look(&mut gone, &service, None).await;
        assert!(!gone.has_edits());
        assert!(!gone.is_visible());
    }

    #[tokio::test]
    async fn copies_from_last_time_come_back_and_a_failed_fetch_says_why() {
        let (mut handler, _) = handler_with(FakeTransfer {
            copies: vec![working_copy(), working_copy()],
            edit: Err(dto::Error::NotEditable),
            ..FakeTransfer::new(Ok(summary()))
        });
        for message in crate::handler::testing::drain(handler.load_edits()).await.0 {
            let _ = handler.update(message);
        }
        assert_eq!(handler.edits.len(), 1, "once each");

        let task = handler.edit(dto::EditRequest {
            phone: "ABC123".into(),
            storage_id: 5,
            folder: "/".into(),
            name: "DCIM".into(),
        });
        for message in crate::handler::testing::drain(task).await.0 {
            let _ = handler.update(message);
        }
        let note = format!(
            "Couldn't open it for editing: {}",
            error::to_error_response(dto::Error::NotEditable).title
        );
        assert!(
            iced_test::simulator(handler.view())
                .find(note.as_str())
                .is_ok()
        );
        request(&mut handler, Request::Clear);
        assert!(handler.edit_note.is_none());
    }

    #[test]
    fn a_check_pauses_carries_on_and_is_kept_for_next_time_as_it_was_asked() {
        let mut handler = handler();
        let _ = handler.check(check_request());
        assert_eq!(
            handler.saved(),
            [(true, dto::SavedJob::Check(check_request()))]
        );
        let job = handler.current.as_ref().unwrap().job.clone();
        assert_eq!(job.label(), "Check 2 items against Backup on your Mac");
        assert_eq!(
            job.resumed(),
            Some(job.clone()),
            "it changes nothing: run it again"
        );

        finish_err(&mut handler, dto::Error::MacDiskFull);
        request(&mut handler, Request::Retry(0));
        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Check(r), .. }) if *r == check_request()
        ));
    }

    #[test]
    fn a_failed_mirror_is_retried_as_it_was_asked() {
        let mut handler = handler();
        let _ = handler.mirror(mirror_request(dto::MirrorDirection::ToPhone));
        finish_err(&mut handler, dto::Error::MacDiskFull);
        assert_eq!(
            handler.history[0].job.label(),
            "Mirror Backup onto the phone"
        );

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Mirror(r), .. })
                if *r == mirror_request(dto::MirrorDirection::ToPhone)
        ));
    }

    #[test]
    fn phone_work_is_what_runs_or_waits_on_the_phone() {
        let mut handler = handler();
        assert!(!handler.has_phone_work());

        let _ = handler.paste_on_mac(paste_request(dto::Paste::Copy));
        assert!(!handler.has_phone_work(), "a paste inside the Mac");
        let _ = handler.upload(upload_request());
        assert!(handler.has_phone_work(), "an upload waiting its turn");
        finish_ok(&mut handler);
        assert!(handler.has_phone_work(), "the upload running");
        finish_ok(&mut handler);
        assert!(!handler.has_phone_work());
    }

    #[test]
    fn a_successful_transfer_is_not_retried() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        request(&mut handler, Request::Retry(0));
        request(&mut handler, Request::Retry(9));

        assert_eq!(handler.history.len(), 1);
        assert!(!handler.is_running());
    }

    #[test]
    fn show_in_finder_reveals_a_finished_downloads_first_item() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        request(&mut handler, Request::ShowInFinder(0));
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        request(&mut handler, Request::ShowInFinder(0));

        assert_eq!(
            *service.revealed.lock().unwrap(),
            ["/Users/me/Downloads/a.jpg"]
        );
    }

    #[test]
    fn a_finish_with_nothing_running_is_ignored() {
        let mut handler = handler();

        finish_ok(&mut handler);

        assert!(!handler.is_visible());
    }

    #[test]
    fn messages_report_device_loss_and_success() {
        let lost = Message::Finished(Err(error::to_error_response(dto::Error::Disconnected)));
        let lost_check = checked(Err(dto::Error::Disconnected));
        let other = Message::Finished(Err(error::to_error_response(dto::Error::ReadOnly)));
        let ok = Message::Finished(Ok(summary()));

        assert!(lost.device_lost().is_some());
        assert!(lost_check.device_lost().is_some());
        assert!(other.device_lost().is_none());
        assert!(ok.device_lost().is_none());
        assert_eq!(ok.finished(), Some(&summary()));
        assert!(lost.finished().is_none());
        assert!(Message::Progress(progress(1)).finished().is_none());
    }

    #[tokio::test]
    async fn the_event_stream_relays_progress_then_the_result() {
        let service =
            Arc::new(FakeTransfer::new(Ok(summary())).reporting(vec![progress(1), progress(4)]));
        let request = download_request();

        let events: Vec<Message> = transfer_events(
            service.clone(),
            dto::Cancel::new(),
            move |service, cancel, progress| {
                Box::pin(async move {
                    service
                        .download(
                            dto::Paste::Copy,
                            request,
                            dto::Conflict::KeepBoth,
                            cancel,
                            progress,
                        )
                        .await
                })
            },
        )
        .collect()
        .await;

        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], Message::Progress(p) if p.bytes_done == 1));
        assert!(matches!(&events[2], Message::Finished(Ok(_))));
        assert_eq!(*service.requests.lock().unwrap(), [download_request()]);
    }

    #[tokio::test]
    async fn the_event_stream_ends_with_the_error() {
        let service = Arc::new(FakeTransfer::new(Err(dto::Error::MacDiskFull)));
        let request = upload_request();

        let events: Vec<Message> = transfer_events(
            service.clone(),
            dto::Cancel::new(),
            move |service, cancel, progress| {
                Box::pin(async move {
                    service
                        .upload(
                            dto::Paste::Copy,
                            request,
                            dto::Conflict::Skip,
                            cancel,
                            progress,
                        )
                        .await
                })
            },
        )
        .collect()
        .await;

        assert!(matches!(&events[..], [Message::Finished(Err(e))]
            if e.title == "Your Mac's disk is full"));
        assert_eq!(
            *service.uploads.lock().unwrap(),
            [(upload_request(), dto::Conflict::Skip)]
        );
    }

    fn partly_done() -> dto::TransferSummary {
        dto::TransferSummary {
            files: 9,
            bytes: 90,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("DCIM".into()),
            failures: vec![dto::FileFailure {
                label: "DCIM/IMG_9.jpg".into(),
                error: dto::Error::Unsupported,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_batch_that_finished_with_failures_can_be_retried() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Finished(Ok(partly_done())));
        assert!(handler.history[0].is_retryable());
        let _ = handler.view();

        request(&mut handler, Request::Retry(0));

        assert!(
            handler.history.is_empty(),
            "the old row gives way to the retry"
        );
        assert!(
            matches!(&handler.current, Some(Current { job: Job::Download(r, _, _), .. }) if *r == download_request())
        );
    }

    #[test]
    fn a_clean_finish_is_not_retryable_and_a_failure_is() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert!(!handler.history[0].is_retryable());

        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);
        assert!(handler.history[1].is_retryable());
    }

    #[test]
    fn the_history_marks_a_partial_result_and_lists_what_failed() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Finished(Ok(partly_done())));

        let mut ui = iced_test::simulator(handler.view());

        assert!(ui.find("⚠").is_ok());
        assert!(
            ui.find("DCIM/IMG_9.jpg — Your phone can't do this over USB")
                .is_ok()
        );
        assert!(ui.find("Retry failed").is_ok());
        assert!(ui.find("Show in Finder").is_ok());
        let _ = ui.click("Retry failed").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Request(Request::Retry(0))]),
            "{messages:?}"
        );
    }

    #[test]
    fn a_partial_result_without_a_folder_to_show_still_offers_the_retry() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let summary = dto::TransferSummary {
            first_item: None,
            ..partly_done()
        };
        let _ = handler.update(Message::Finished(Ok(summary)));

        let mut ui = iced_test::simulator(handler.view());

        assert!(ui.find("Retry failed").is_ok());
        assert!(ui.find("Show in Finder").is_err());
    }

    #[test]
    fn a_clean_result_shows_neither_a_warning_nor_a_retry() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        let mut ui = iced_test::simulator(handler.view());

        assert!(ui.find("⚠").is_err());
        assert!(ui.find("Retry failed").is_err());
        assert!(ui.find("Show in Finder").is_ok());
    }

    #[test]
    fn a_long_copy_shows_the_keep_unlocked_note_under_the_bars() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let long = dto::TransferProgress {
            bytes_total: 500_000_000,
            finishing: true,
            ..progress(1)
        };
        let _ = handler.update(Message::Progress(long));

        let mut ui = iced_test::simulator(handler.view());

        assert!(
            ui.find(
                "Keep your phone unlocked and plugged in until this finishes. \
                 Your Mac will stay awake until then."
            )
            .is_ok()
        );
        assert!(ui.find("Finishing a.jpg…").is_ok());
    }

    #[test]
    fn a_long_copy_on_a_usb_2_link_says_that_is_why_it_is_slow() {
        let mut handler = handler();
        handler.set_link(Some(dto::UsbSpeed::High));
        let _ = handler.download(download_request());
        let long = dto::TransferProgress {
            bytes_total: 500_000_000,
            ..progress(1)
        };
        let _ = handler.update(Message::Progress(long));

        let mut ui = iced_test::simulator(handler.view());

        assert!(
            ui.find(
                "Keep your phone unlocked and plugged in until this finishes. \
                 Your Mac will stay awake until then. \
                 Connected at USB 2 speed (480 Mb/s): about 35 MB/s at most. \
                 A USB 3 cable and port would be faster."
            )
            .is_ok()
        );
    }

    #[test]
    fn a_short_copy_shows_no_note() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Progress(progress(1)));

        let mut ui = iced_test::simulator(handler.view());

        assert!(
            ui.find("Keep your phone unlocked and plugged in until this finishes.")
                .is_err()
        );
    }

    // ---- moves ----

    fn left_behind() -> dto::TransferSummary {
        dto::TransferSummary {
            files: 3,
            bytes: 30,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(dto::Removal {
                removed: 1,
                kept: vec![dto::Kept {
                    label: "b.jpg".into(),
                    why: dto::Why::Changed,
                }],
                left: vec!["b.jpg".into(), "c.jpg".into()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn move_request() -> dto::DownloadRequest {
        dto::DownloadRequest {
            names: vec!["a.jpg".into(), "b.jpg".into(), "c.jpg".into()],
            ..download_request()
        }
    }

    #[tokio::test]
    async fn a_move_is_asked_of_the_service_as_a_move_and_a_download_as_a_copy() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let moving = handler.download_as(dto::Paste::Move, move_request());
        let _ = crate::handler::testing::drain(moving).await;
        let _ = handler.update(Message::Finished(Ok(summary())));
        let copying = handler.download(download_request());
        let _ = crate::handler::testing::drain(copying).await;

        assert_eq!(
            *service.modes.lock().unwrap(),
            [dto::Paste::Move, dto::Paste::Copy]
        );
    }

    #[test]
    fn a_move_says_so_while_it_waits_and_while_it_runs() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let _ = handler.download_as(dto::Paste::Move, download_request());

        assert_eq!(
            handler.waiting[0].label(),
            "Move “a.jpg” to Downloads",
            "the second one waits behind the first"
        );
        let Some(Current {
            stage: Stage::Running { progress, .. },
            ..
        }) = &handler.current
        else {
            panic!("the first move is running");
        };
        assert_eq!(progress.title, "Preparing move…");
        let _ = handler.view();

        let _ = handler.update(Message::Progress(dto::TransferProgress {
            moving: true,
            files_total: 3,
            ..Default::default()
        }));
        let Some(Current {
            stage: Stage::Running { progress, .. },
            ..
        }) = &handler.current
        else {
            panic!("still running");
        };
        assert_eq!(progress.title, "Moving 1 of 3");
    }

    #[test]
    fn a_move_that_left_originals_can_be_retried_even_with_nothing_failed() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let _ = handler.update(Message::Finished(Ok(left_behind())));

        assert!(handler.history[0].is_retryable());
        let _ = handler.view();
    }

    #[test]
    fn retrying_a_move_goes_on_with_what_is_still_on_the_phone() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let _ = handler.update(Message::Finished(Ok(left_behind())));

        request(&mut handler, Request::Retry(0));

        let Some(Current {
            job: Job::Download(retry, mode, Answer::Retried(dto::Conflict::KeepBoth)),
            ..
        }) = &handler.current
        else {
            panic!("the retry is running, without asking again");
        };
        assert_eq!(*mode, dto::Paste::Move);
        assert_eq!(
            retry.names,
            ["b.jpg", "c.jpg"],
            "a.jpg is already off the phone"
        );
        assert_eq!(retry.source, "/DCIM");
    }

    #[test]
    fn retrying_a_copy_keeps_every_name() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Copy, move_request());
        let _ = handler.update(Message::Finished(Ok(partly_done())));

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Download(r, dto::Paste::Copy, _), .. }) if r.names.len() == 3
        ));
    }

    #[test]
    fn a_failed_move_is_retried_whole() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Download(r, dto::Paste::Move, _), .. }) if r.names.len() == 3
        ));
    }

    // ---- moves to the phone ----

    fn left_on_the_mac() -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: 3,
            bytes: 30,
            destination: "/".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(dto::Removal {
                from_mac: true,
                removed: 1,
                kept: vec![dto::Kept {
                    label: "b.jpg".into(),
                    why: dto::Why::Changed,
                }],
                left: vec!["/Users/me/b.jpg".into(), "/Users/me/c.jpg".into()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn upload_move_request() -> dto::UploadRequest {
        dto::UploadRequest {
            sources: vec![
                "/Users/me/a.jpg".into(),
                "/Users/me/b.jpg".into(),
                "/Users/me/c.jpg".into(),
            ],
            ..upload_request()
        }
    }

    /// The upload's question answered (nothing is taken), as the app sees it.
    fn checked_free(handler: &mut Handler, request: dto::UploadRequest) -> Task<Message> {
        handler.update(Message::Checked {
            request,
            result: Ok(vec![]),
        })
    }

    #[test]
    fn a_move_to_the_phone_says_so_while_it_waits_and_while_it_runs() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.upload_as(dto::Paste::Move, upload_request());

        assert!(
            handler.waiting[0]
                .label()
                .starts_with("Move “notes.txt” to "),
            "{}",
            handler.waiting[0].label()
        );

        finish_ok(&mut handler);
        let _ = handler.update(checked(Ok(vec![])));
        let Some(Current {
            stage: Stage::Running { progress, .. },
            ..
        }) = &handler.current
        else {
            panic!("the move is running");
        };
        assert_eq!(progress.title, "Preparing move…");
        let _ = handler.view();
    }

    #[tokio::test]
    async fn a_move_to_the_phone_is_asked_of_the_service_as_a_move_and_an_upload_as_a_copy() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.upload_as(dto::Paste::Move, upload_request());
        let moving = checked_free(&mut handler, upload_request());
        let _ = crate::handler::testing::drain(moving).await;
        let _ = handler.update(Message::Finished(Ok(summary())));
        let _ = handler.upload(upload_request());
        let copying = checked_free(&mut handler, upload_request());
        let _ = crate::handler::testing::drain(copying).await;

        assert_eq!(
            *service.upload_modes.lock().unwrap(),
            [dto::Paste::Move, dto::Paste::Copy]
        );
    }

    #[tokio::test]
    async fn a_move_to_the_phone_stays_a_move_through_the_question_about_taken_names() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.upload_as(dto::Paste::Move, upload_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));
        assert!(matches!(stage(&handler), Some(Stage::Asking { .. })));

        let answer = handler.update(Message::Request(Request::Resolve(dto::Conflict::Replace)));
        let _ = crate::handler::testing::drain(answer).await;

        assert!(matches!(
            &handler.current,
            Some(Current {
                job: Job::Upload(_, dto::Paste::Move, _),
                ..
            })
        ));
        assert_eq!(*service.upload_modes.lock().unwrap(), [dto::Paste::Move]);
        assert_eq!(
            service.uploads.lock().unwrap()[0].1,
            dto::Conflict::Replace,
            "what the user answered is what is sent"
        );
    }

    #[test]
    fn a_move_to_the_phone_that_left_originals_can_be_retried_with_nothing_failed() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let _ = handler.update(Message::Finished(Ok(left_on_the_mac())));

        assert!(handler.history[0].is_retryable());
        let Ok(summary) = &handler.history[0].outcome else {
            panic!("it finished");
        };
        assert_eq!(summary.notes.len(), 1);
        assert!(summary.notes[0].contains("stayed on your Mac"));
        let _ = handler.view();
    }

    #[test]
    fn retrying_a_move_to_the_phone_sends_what_is_still_on_the_mac() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        // Nothing of those names was on the phone, so nothing was asked.
        let _ = checked_free(&mut handler, upload_move_request());
        let _ = handler.update(Message::Finished(Ok(left_on_the_mac())));

        request(&mut handler, Request::Retry(0));

        let Some(Current {
            job: Job::Upload(retry, mode, answer),
            stage,
        }) = &handler.current
        else {
            panic!("the retry is running");
        };
        assert_eq!(*mode, dto::Paste::Move);
        assert_eq!(
            *answer,
            Answer::Retried(dto::Conflict::Merge),
            "it is the retry of a move that got part way"
        );
        assert!(
            matches!(stage, Stage::Running { .. }),
            "no question about names: it knows what happened"
        );
        assert_eq!(
            retry.sources,
            ["/Users/me/b.jpg", "/Users/me/c.jpg"],
            "a.jpg is already in the Trash"
        );
        assert_eq!(retry.destination, "/");
    }

    #[tokio::test]
    async fn the_retry_of_a_move_to_the_phone_asks_the_service_to_merge() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let first = checked_free(&mut handler, upload_move_request());
        let _ = crate::handler::testing::drain(first).await;
        let _ = handler.update(Message::Finished(Ok(left_on_the_mac())));

        let retry = handler.update(Message::Request(Request::Retry(0)));
        let _ = crate::handler::testing::drain(retry).await;

        let uploads = service.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[1].0.sources, ["/Users/me/b.jpg", "/Users/me/c.jpg"]);
        assert_eq!(
            uploads[1].1,
            dto::Conflict::Merge,
            "nothing on the phone is replaced and no folder is numbered"
        );
        assert_eq!(
            *service.upload_modes.lock().unwrap(),
            [dto::Paste::Move, dto::Paste::Move]
        );
    }

    #[test]
    fn a_first_try_still_asks_about_taken_names_and_a_copy_retry_does_too() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        assert!(
            matches!(stage(&handler), Some(Stage::Checking)),
            "a first try"
        );
        let _ = handler.update(Message::Finished(Ok(left_on_the_mac())));
        request(&mut handler, Request::Clear);

        let _ = handler.upload_as(dto::Paste::Copy, upload_move_request());
        let mut partly = left_on_the_mac();
        partly.removal = None;
        partly.failures = vec![dto::FileFailure {
            label: "b.jpg".into(),
            error: dto::Error::Timeout,
        }];
        let _ = handler.update(Message::Finished(Ok(partly)));
        request(&mut handler, Request::Retry(0));

        assert!(
            matches!(
                &handler.current,
                Some(Current {
                    job: Job::Upload(_, dto::Paste::Copy, Answer::Unasked),
                    stage: Stage::Checking
                })
            ),
            "a copy that is retried is not a move that is resumed"
        );
    }

    #[test]
    fn retrying_a_copy_to_the_phone_keeps_every_source() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Copy, upload_move_request());
        let mut partly = left_on_the_mac();
        partly.removal = None;
        partly.failures = vec![dto::FileFailure {
            label: "b.jpg".into(),
            error: dto::Error::Timeout,
        }];
        let _ = handler.update(Message::Finished(Ok(partly)));

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Upload(r, dto::Paste::Copy, _), .. }) if r.sources.len() == 3
        ));
    }

    #[test]
    fn a_failed_move_to_the_phone_is_retried_whole() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let _ = checked_free(&mut handler, upload_move_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Upload(r, dto::Paste::Move, Answer::Retried(dto::Conflict::Merge)), .. }) if r.sources.len() == 3
        ));
    }

    /// A move to the phone whose question was answered with `conflict` and
    /// that then failed outright, retried: what the service is asked the
    /// second time, and what the handler is left doing.
    async fn retried_after_answering(
        conflict: dto::Conflict,
    ) -> (
        Handler,
        Arc<FakeTransfer>,
        Vec<(dto::UploadRequest, dto::Conflict)>,
    ) {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let _ = handler.update(Message::Checked {
            request: upload_move_request(),
            result: Ok(vec!["b.jpg".into()]),
        });
        let answered = handler.update(Message::Request(Request::Resolve(conflict)));
        let _ = crate::handler::testing::drain(answered).await;
        finish_err(&mut handler, dto::Error::MacDiskFull);

        let retry = handler.update(Message::Request(Request::Retry(0)));
        let _ = crate::handler::testing::drain(retry).await;

        let asked = service.uploads.lock().unwrap().clone();
        (handler, service, asked)
    }

    #[tokio::test]
    async fn a_retry_after_skip_skips_the_same_names_again() {
        // The user said to leave b.jpg alone; a first try that failed part way
        // must not, on retry, put a numbered copy of it on the phone and its
        // original in the Trash.
        let (handler, _service, asked) = retried_after_answering(dto::Conflict::Skip).await;

        assert_eq!(asked.len(), 2);
        assert_eq!(asked[1].0.sources.len(), 3, "the same sources");
        assert_eq!(asked[1].1, dto::Conflict::Skip, "and the same answer");
        assert!(matches!(
            &handler.current,
            Some(Current {
                job: Job::Upload(_, _, Answer::Retried(dto::Conflict::Skip)),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_retry_after_replace_replaces_again() {
        let (_, _, asked) = retried_after_answering(dto::Conflict::Replace).await;

        assert_eq!(asked[1].1, dto::Conflict::Replace);
    }

    #[tokio::test]
    async fn a_retry_after_keep_both_asks_again_because_the_folder_it_made_is_not_known() {
        let (handler, _service, asked) = retried_after_answering(dto::Conflict::KeepBoth).await;

        assert_eq!(asked.len(), 1, "nothing is sent until the user answers");
        assert!(matches!(
            &handler.current,
            Some(Current {
                job: Job::Upload(_, _, Answer::Unasked),
                stage: Stage::Checking
            })
        ));
    }

    #[test]
    fn a_retry_of_a_try_that_never_got_as_far_as_asking_asks() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let _ = handler.update(Message::Checked {
            request: upload_move_request(),
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current {
                job: Job::Upload(_, _, Answer::Unasked),
                stage: Stage::Checking
            })
        ));
    }

    #[test]
    fn a_move_that_got_part_way_after_skip_goes_on_skipping_with_what_is_left() {
        let mut handler = handler();
        let _ = handler.upload_as(dto::Paste::Move, upload_move_request());
        let _ = handler.update(Message::Checked {
            request: upload_move_request(),
            result: Ok(vec!["b.jpg".into()]),
        });
        request(&mut handler, Request::Resolve(dto::Conflict::Skip));
        let _ = handler.update(Message::Finished(Ok(left_on_the_mac())));

        request(&mut handler, Request::Retry(0));

        let Some(Current {
            job: Job::Upload(retry, _, answer),
            ..
        }) = &handler.current
        else {
            panic!("the retry is running");
        };
        assert_eq!(*answer, Answer::Retried(dto::Conflict::Skip));
        assert_eq!(retry.sources, ["/Users/me/b.jpg", "/Users/me/c.jpg"]);
    }

    #[test]
    fn what_a_move_left_is_shown_under_the_result() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let _ = handler.update(Message::Finished(Ok(left_behind())));

        let Ok(summary) = &handler.history[0].outcome else {
            panic!("it finished");
        };
        assert_eq!(summary.notes.len(), 1);
        assert_eq!(retry_label(summary), "Retry");
        let _ = handler.view();
        let mut with_failures = summary.clone();
        with_failures.failures = vec!["x — y".into()];
        assert_eq!(retry_label(&with_failures), "Retry failed");
    }

    #[test]
    fn a_move_that_left_originals_is_marked_with_a_warning_and_says_why() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let _ = handler.update(Message::Finished(Ok(left_behind())));

        let mut ui = iced_test::simulator(handler.view());

        assert!(
            ui.find("⚠").is_ok(),
            "a move that left files behind is not a clean ✓"
        );
        assert!(ui.find("✓").is_err());
        assert!(
            ui.find("b.jpg — stayed on the phone: it changed after it was copied")
                .is_ok()
        );
        assert!(ui.find("Retry").is_ok());
        assert!(ui.find("Retry failed").is_err(), "nothing failed to copy");
    }

    #[test]
    fn a_move_that_took_everything_is_a_plain_success() {
        let mut handler = handler();
        let _ = handler.download_as(dto::Paste::Move, move_request());
        let clean = dto::TransferSummary {
            files: 3,
            destination: "/Users/me/Downloads".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(dto::Removal {
                removed: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        let _ = handler.update(Message::Finished(Ok(clean)));

        let mut ui = iced_test::simulator(handler.view());

        assert!(ui.find("✓").is_ok());
        assert!(ui.find("⚠").is_err());
        assert!(ui.find("Retry").is_err());
    }

    // ---- inside the phone, through the Mac ----

    fn paste_request(mode: dto::Paste) -> dto::PasteRequest {
        dto::PasteRequest {
            mode,
            from_storage: 1,
            from: "/DCIM".into(),
            names: vec!["a.jpg".into(), "b.jpg".into()],
            to_storage: 1,
            to: "/Music".into(),
        }
    }

    fn on_phone_left() -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: 2,
            destination: "/Music".into(),
            through_mac: true,
            removal: Some(dto::Removal {
                removed: 1,
                kept: vec![dto::Kept {
                    label: "b.jpg".into(),
                    why: dto::Why::Unchecked,
                }],
                left: vec!["b.jpg".into()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_copy_through_the_mac_is_asked_of_the_service_as_it_was_asked() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let task = handler.paste_on_phone(paste_request(dto::Paste::Move));
        let _ = crate::handler::testing::drain(task).await;

        assert_eq!(
            *service.pastes.lock().unwrap(),
            [paste_request(dto::Paste::Move)]
        );
    }

    #[test]
    fn a_copy_through_the_mac_waits_and_runs_under_its_own_name() {
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Copy));
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Move));

        assert_eq!(
            handler.waiting[0].label(),
            "Move 2 items to Music via your Mac"
        );
        let Some(Current {
            stage: Stage::Running { progress, .. },
            ..
        }) = &handler.current
        else {
            panic!("the first is running");
        };
        assert_eq!(progress.title, "Preparing copy via your Mac…");
        let _ = handler.view();
    }

    // ---- inside the Mac ----

    fn mac_request(mode: dto::Paste) -> dto::PasteRequest {
        dto::PasteRequest {
            mode,
            from_storage: 0,
            from: "/Users/me/Documents".into(),
            names: vec!["a.jpg".into(), "b.jpg".into(), "c.jpg".into()],
            to_storage: 0,
            to: "/Users/me/Backup".into(),
        }
    }

    fn mac_done(left: &[&str]) -> dto::TransferSummary {
        dto::TransferSummary {
            direction: dto::Direction::Local,
            files: 3,
            bytes: 30,
            destination: "/Users/me/Backup".into(),
            first_item: Some("a.jpg".into()),
            removal: Some(dto::Removal {
                from_mac: true,
                removed: 3 - left.len(),
                left: left.iter().map(|name| (*name).to_owned()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_queued_mac_paste_is_asked_of_the_service_and_says_what_it_is() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(mac_done(&[]))));

        let copying = handler.paste_on_mac(mac_request(dto::Paste::Copy));
        assert!(matches!(
            stage(&handler),
            Some(Stage::Running { progress, .. }) if progress.title == "Preparing copy…"
        ));
        let _ = crate::handler::testing::drain(copying).await;
        let _ = handler.update(Message::Finished(Ok(mac_done(&[]))));
        let moving = handler.paste_on_mac(mac_request(dto::Paste::Move));
        assert!(matches!(
            stage(&handler),
            Some(Stage::Running { progress, .. }) if progress.title == "Preparing move…"
        ));
        let _ = crate::handler::testing::drain(moving).await;

        let asked = service.mac_pastes.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            (asked[0].mode, asked[1].mode),
            (dto::Paste::Copy, dto::Paste::Move)
        );
        assert_eq!(asked[0].to, "/Users/me/Backup");
        let _ = handler.view();
    }

    #[test]
    fn a_mac_paste_says_so_while_it_waits() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.paste_on_mac(mac_request(dto::Paste::Copy));
        let _ = handler.paste_on_mac(mac_request(dto::Paste::Move));

        assert_eq!(handler.waiting[0].label(), "Copy 3 items to Backup");
        assert_eq!(handler.waiting[1].label(), "Move 3 items to Backup");
    }

    #[test]
    fn a_mac_move_that_left_originals_is_retried_with_what_is_left() {
        let mut handler = handler();
        let _ = handler.paste_on_mac(mac_request(dto::Paste::Move));
        let _ = handler.update(Message::Finished(Ok(mac_done(&["b.jpg", "c.jpg"]))));
        assert!(handler.history[0].is_retryable());

        request(&mut handler, Request::Retry(0));

        let Some(Current {
            job: Job::OnMac(retry),
            ..
        }) = &handler.current
        else {
            panic!("the retry is running");
        };
        assert_eq!(retry.mode, dto::Paste::Move);
        assert_eq!(
            retry.names,
            ["b.jpg", "c.jpg"],
            "a.jpg is already in the Trash"
        );
        assert_eq!(retry.from, "/Users/me/Documents");
    }

    #[test]
    fn a_mac_copy_that_partly_worked_is_not_offered_again_but_one_that_failed_is() {
        let mut handler = handler();
        let _ = handler.paste_on_mac(mac_request(dto::Paste::Copy));
        let mut partly = mac_done(&[]);
        partly.removal = None;
        partly.failures = vec![dto::FileFailure {
            label: "b.jpg".into(),
            error: dto::Error::MacDiskFull,
        }];
        let _ = handler.update(Message::Finished(Ok(partly)));
        assert!(
            !handler.history[0].is_retryable(),
            "a retry would copy it all again under new names"
        );

        let _ = handler.paste_on_mac(mac_request(dto::Paste::Copy));
        finish_err(&mut handler, dto::Error::Cancelled);
        assert!(handler.history[1].is_retryable());
        request(&mut handler, Request::Retry(1));
        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::OnMac(r), .. }) if r.names.len() == 3
        ));
    }

    #[tokio::test]
    async fn only_the_retry_of_a_move_through_the_mac_is_resumed() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(on_phone_left())));

        let first = handler.paste_on_phone(paste_request(dto::Paste::Move));
        let _ = crate::handler::testing::drain(first).await;
        let _ = handler.update(Message::Finished(Ok(on_phone_left())));
        let retry = handler.update(Message::Request(Request::Retry(0)));
        let _ = crate::handler::testing::drain(retry).await;
        // A move that failed outright is retried whole, and resumed too: it
        // may have got somewhere before it failed. (With nothing known of
        // what it did, the folder of that name is taken to be its own.)
        let _ = handler.update(Message::Finished(Err(error::to_error_response(
            dto::Error::MacDiskFull,
        ))));
        let again = handler.update(Message::Request(Request::Retry(0)));
        let _ = crate::handler::testing::drain(again).await;
        // A copy that failed outright is not resumed: nothing is merged into a
        // folder that was not made by this copy.
        let _ = handler.update(Message::Finished(Err(error::to_error_response(
            dto::Error::MacDiskFull,
        ))));
        request(&mut handler, Request::Clear);
        let copy = handler.paste_on_phone(paste_request(dto::Paste::Copy));
        let _ = crate::handler::testing::drain(copy).await;
        let _ = handler.update(Message::Finished(Err(error::to_error_response(
            dto::Error::MacDiskFull,
        ))));
        let copy_again = handler.update(Message::Request(Request::Retry(0)));
        let _ = crate::handler::testing::drain(copy_again).await;

        assert_eq!(
            *service.resumings.lock().unwrap(),
            [false, true, true, false, false]
        );
    }

    #[test]
    fn a_retry_through_the_mac_does_not_merge_into_a_folder_the_first_try_had_to_number() {
        // The first try put the folder beside one of the same name that was
        // already there (`Trip (1)`), so a folder called `Trip` is not the one
        // it started and a merge into it would mix the two.
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Move));
        let mut numbered = on_phone_left();
        numbered.renamed = 1;
        let _ = handler.update(Message::Finished(Ok(numbered)));

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current {
                job: Job::OnPhone(_, false),
                ..
            })
        ));
    }

    #[test]
    fn retrying_a_move_through_the_mac_goes_on_with_what_is_still_where_it_was() {
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Move));
        let _ = handler.update(Message::Finished(Ok(on_phone_left())));
        assert!(handler.history[0].is_retryable());
        let _ = handler.view();

        request(&mut handler, Request::Retry(0));

        let Some(Current {
            job: Job::OnPhone(retry, resuming),
            ..
        }) = &handler.current
        else {
            panic!("the retry is running");
        };
        assert_eq!(retry.mode, dto::Paste::Move);
        assert!(*resuming, "it merges into what the first try made");
        assert_eq!(retry.names, ["b.jpg"], "a.jpg already moved");
        assert_eq!(
            (retry.from.as_str(), retry.to.as_str()),
            ("/DCIM", "/Music")
        );
    }

    #[test]
    fn retrying_a_copy_through_the_mac_keeps_every_name() {
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Copy));
        finish_err(&mut handler, dto::Error::MacDiskFull);

        request(&mut handler, Request::Retry(0));

        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::OnPhone(r, _), .. }) if r.names.len() == 2
        ));
    }

    #[test]
    fn a_copy_through_the_mac_that_partly_worked_is_not_offered_a_retry() {
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Copy));
        let mut partly = on_phone_left();
        partly.failures = vec![dto::FileFailure {
            label: "b.jpg".into(),
            error: dto::Error::ReadOnly,
        }];
        partly.removal = None;
        let _ = handler.update(Message::Finished(Ok(partly)));

        assert!(!handler.history[0].is_retryable());
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Retry failed").is_err());
        assert!(ui.find("Retry").is_err());
        assert!(ui.find("⚠").is_ok(), "but it still says something failed");
    }

    #[test]
    fn a_move_through_the_mac_that_partly_worked_is_offered_one() {
        let mut handler = handler();
        let _ = handler.paste_on_phone(paste_request(dto::Paste::Move));
        let _ = handler.update(Message::Finished(Ok(on_phone_left())));

        assert!(handler.history[0].is_retryable());
    }

    // The Dock icon and notifications.

    fn progress_of(done: u64, total: u64) -> dto::TransferProgress {
        dto::TransferProgress {
            current_file: "a.jpg".into(),
            files_total: 1,
            file_bytes_total: total,
            file_bytes_done: done,
            bytes_total: total,
            bytes_done: done,
            elapsed: Duration::from_secs(2),
            ..dto::TransferProgress::default()
        }
    }

    #[test]
    fn the_dock_bar_follows_the_transfer_and_is_redrawn_only_when_the_percent_changes() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.download(download_request());
        let _ = handler.update(Message::Progress(progress_of(4_000, 10_000)));
        let _ = handler.update(Message::Progress(progress_of(4_040, 10_000)));
        for done in 4_100..4_200 {
            let _ = handler.update(Message::Progress(progress_of(done, 10_000)));
        }
        finish_ok(&mut handler);

        assert_eq!(
            *service.dock.lock().unwrap(),
            [Some(0), Some(40), Some(41), None],
            "a hundred reports inside one percent draw nothing"
        );
    }

    #[test]
    fn a_question_holds_the_bar_at_nothing_done_and_a_cancel_takes_it_away() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));
        request(&mut handler, Request::Cancel);

        assert_eq!(*service.dock.lock().unwrap(), [Some(0), None]);
    }

    // Keeping the Mac awake.

    #[test]
    fn the_mac_is_kept_awake_while_a_transfer_works_and_let_go_when_the_queue_is_idle() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        assert_eq!(*service.awake.lock().unwrap(), [true]);
        for done in 1..50 {
            let _ = handler.update(Message::Progress(progress_of(done, 100)));
        }
        finish_ok(&mut handler);
        assert_eq!(
            *service.awake.lock().unwrap(),
            [true],
            "held through progress and from one job to the next"
        );
        finish_ok(&mut handler);

        assert_eq!(*service.awake.lock().unwrap(), [true, false]);
    }

    #[test]
    fn a_question_left_waiting_lets_the_mac_sleep_until_it_is_answered() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));
        assert_eq!(
            *service.awake.lock().unwrap(),
            [true, false],
            "checking holds it; the question does not"
        );
        request(&mut handler, Request::Resolve(dto::Conflict::Skip));
        assert_eq!(*service.awake.lock().unwrap(), [true, false, true]);
        request(&mut handler, Request::Cancel);
        assert_eq!(
            *service.awake.lock().unwrap(),
            [true, false, true],
            "a cancel still holds it until the transfer has stopped"
        );
        let _ = handler.update(Message::Finished(Err(error::to_error_response(
            dto::Error::Cancelled,
        ))));

        assert_eq!(*service.awake.lock().unwrap(), [true, false, true, false]);
    }

    #[test]
    fn a_long_copy_says_the_mac_stays_awake_only_when_it_does() {
        let note = "Keep your phone unlocked and plugged in until this finishes. Your Mac will stay awake until then.";
        let long = progress_of(1, 200_000_000);

        let (mut held, _) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = held.download(download_request());
        let _ = held.update(Message::Progress(long.clone()));
        assert!(iced_test::simulator(held.view()).find(note).is_ok());

        let (mut refused, _) = handler_with(FakeTransfer {
            refuse_awake: true,
            ..FakeTransfer::new(Ok(summary()))
        });
        let _ = refused.download(download_request());
        let _ = refused.update(Message::Progress(long));
        let mut ui = iced_test::simulator(refused.view());
        assert!(ui.find(note).is_err(), "never claimed when macOS said no");
        assert!(
            ui.find("Keep your phone unlocked and plugged in until this finishes.")
                .is_ok()
        );
    }

    #[test]
    fn nothing_is_notified_while_the_app_is_in_front() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        assert!(service.notes.lock().unwrap().is_empty());
    }

    #[test]
    fn one_transfer_ending_in_the_background_says_how_it_went() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.set_focused(false);

        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        assert_eq!(
            *service.notes.lock().unwrap(),
            [(
                "Downloaded 1 file (4 B)".to_owned(),
                "to Downloads".to_owned()
            )]
        );
    }

    #[test]
    fn a_queue_ending_in_the_background_is_one_note_with_its_problems_counted() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.set_focused(false);

        for _ in 0..3 {
            let _ = handler.download(download_request());
        }
        finish_ok(&mut handler);
        finish_err(&mut handler, dto::Error::MacDiskFull);
        assert!(
            service.notes.lock().unwrap().is_empty(),
            "the queue is not done yet"
        );
        finish_ok(&mut handler);

        let notes = service.notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "3 transfers finished, 1 with a problem");
        assert!(
            notes[0].1.starts_with("Downloaded 1 file (4 B) · "),
            "{}",
            notes[0].1
        );
    }

    #[test]
    fn a_note_is_about_what_ended_since_the_queue_was_last_idle() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        handler.set_focused(false);
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        let notes = service.notes.lock().unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].0.ends_with(": stopped"),
            "the second one alone, not two transfers: {notes:?}"
        );
    }

    #[test]
    fn clearing_the_finished_list_mid_batch_leaves_the_note_about_what_is_left() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.set_focused(false);
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        finish_ok(&mut handler);

        request(&mut handler, Request::Clear);
        finish_err(&mut handler, dto::Error::MacDiskFull);

        let notes = service.notes.lock().unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].0.ends_with(": stopped"), "{notes:?}");
    }

    // Pause and resume.

    fn shows(handler: &Handler, label: &str) -> bool {
        iced_test::simulator(handler.view()).find(label).is_ok()
    }

    fn clicked(handler: &Handler, label: &str) -> Vec<Message> {
        let mut ui = iced_test::simulator(handler.view());
        let _ = ui.click(label).unwrap();
        ui.into_messages().collect()
    }

    fn paused_download() -> (Handler, Arc<FakeTransfer>) {
        let (mut handler, service) = sounding();
        handler.set_focused(false);
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Progress(progress(2)));
        for message in clicked(&handler, "Pause") {
            let _ = handler.update(message);
        }
        assert!(matches!(
            stage(&handler),
            Some(Stage::Running { cancel, pausing: true, .. }) if cancel.is_cancelled()
        ));
        assert!(shows(&handler, "Pausing…"));
        assert!(clicked(&handler, "Pausing…").is_empty(), "once is enough");
        finish_err(&mut handler, dto::Error::Cancelled);
        (handler, service)
    }

    #[test]
    fn a_paused_transfer_is_kept_and_holds_the_queue() {
        let (mut handler, service) = paused_download();
        let _ = handler.upload(upload_request());

        assert!(handler.is_paused() && !handler.is_running());
        assert!(handler.history.is_empty(), "not finished");
        assert_eq!(handler.waiting.len(), 1, "what was queued waits");
        assert!(handler.has_phone_work(), "the phone stays open for it");
        assert!(handler.is_visible());
        assert!(service.notes.lock().unwrap().is_empty());
        assert!(service.sounds.lock().unwrap().is_empty());
        assert_eq!(service.dock.lock().unwrap().last(), Some(&None));
        assert_eq!(
            service.awake.lock().unwrap().last(),
            Some(&false),
            "the Mac may sleep while it waits"
        );
        assert!(shows(&handler, "Paused: Downloading 1 of 1"));
        assert!(shows(&handler, mapper::paused_note()));

        for message in clicked(&handler, "Resume") {
            let _ = handler.update(message);
        }
        assert!(!handler.is_paused());
        assert!(matches!(
            &handler.current,
            Some(Current { job: Job::Download(r, dto::Paste::Copy, _), stage: Stage::Running { .. } })
                if *r == download_request()
        ));
        assert_eq!(handler.waiting.len(), 1, "it goes first, before the upload");
    }

    #[test]
    fn cancelling_a_paused_transfer_lists_it_and_moves_on() {
        let (mut handler, service) = paused_download();
        let _ = handler.upload(upload_request());

        for message in clicked(&handler, "Cancel") {
            let _ = handler.update(message);
        }

        assert!(!handler.is_paused());
        assert!(matches!(&handler.history[0].outcome, Err(e) if e.cancelled));
        assert!(handler.history[0].is_retryable());
        assert!(
            matches!(stage(&handler), Some(Stage::Checking)),
            "the upload's turn"
        );
        assert!(
            service.sounds.lock().unwrap().is_empty(),
            "the user's own cancel"
        );
    }

    #[test]
    fn a_pause_that_came_too_late_or_met_a_failure_finishes_as_usual() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        request(&mut handler, Request::Pause);
        finish_ok(&mut handler);
        assert!(!handler.is_paused());
        assert_eq!(handler.history.len(), 1);
        assert!(handler.is_running(), "the next one starts");

        request(&mut handler, Request::Pause);
        finish_err(&mut handler, dto::Error::MacDiskFull);
        assert!(!handler.is_paused());
        assert!(matches!(&handler.history[1].outcome, Err(e) if !e.cancelled));
    }

    #[test]
    fn cancel_while_pausing_cancels() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        request(&mut handler, Request::Pause);
        assert!(shows(&handler, "Cancel"), "still offered while pausing");
        request(&mut handler, Request::Cancel);
        assert!(
            clicked(&handler, "Pausing…").is_empty(),
            "no pause once cancelling"
        );
        finish_err(&mut handler, dto::Error::Cancelled);

        assert!(!handler.is_paused());
        assert_eq!(handler.history.len(), 1);

        let _ = handler.download(download_request());
        request(&mut handler, Request::Cancel);
        assert!(
            clicked(&handler, "Pause").is_empty(),
            "nor while cancelling"
        );
    }

    #[test]
    fn a_question_or_a_job_that_cannot_carry_on_is_not_paused() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["notes.txt".into()])));
        request(&mut handler, Request::Pause);
        assert!(matches!(stage(&handler), Some(Stage::Asking { .. })));
        request(&mut handler, Request::Cancel);

        let _ = handler.paste_on_mac(mac_request(dto::Paste::Copy));
        assert!(!shows(&handler, "Pause"), "not offered");
        request(&mut handler, Request::Pause);
        assert!(matches!(
            stage(&handler),
            Some(Stage::Running { cancel, pausing: false, .. }) if !cancel.is_cancelled()
        ));
        request(&mut handler, Request::Resume);
        assert!(handler.is_running(), "nothing to resume");
    }

    #[test]
    fn what_carries_on_after_a_pause() {
        let upload = |mode, answer| Job::Upload(upload_request(), mode, answer);
        let merge = Answer::Retried(dto::Conflict::Merge);

        let download = Job::Download(download_request(), dto::Paste::Move, Answer::Unasked);
        assert_eq!(
            download.resumed(),
            Some(Job::Download(
                download_request(),
                dto::Paste::Move,
                Answer::Retried(dto::Conflict::KeepBoth)
            )),
            "carries on as it was, without asking again"
        );
        let replacing = Job::Download(
            download_request(),
            dto::Paste::Copy,
            Answer::Chose(dto::Conflict::Replace),
        );
        assert!(matches!(
            replacing.resumed(),
            Some(Job::Download(_, _, Answer::Retried(dto::Conflict::Replace)))
        ));
        let import = Job::Import(import_request());
        assert_eq!(import.resumed(), Some(import.clone()));
        let mirror = Job::Mirror(mirror_request(dto::MirrorDirection::ToPhone));
        assert_eq!(mirror.resumed(), Some(mirror.clone()));
        assert_eq!(
            Job::OnPhone(paste_request(dto::Paste::Move), false).resumed(),
            Some(Job::OnPhone(paste_request(dto::Paste::Move), true)),
            "a move through the Mac merges into what it started"
        );
        assert_eq!(
            Job::OnPhone(paste_request(dto::Paste::Copy), false).resumed(),
            None
        );
        assert_eq!(Job::OnMac(mac_request(dto::Paste::Move)).resumed(), None);

        for mode in [dto::Paste::Copy, dto::Paste::Move] {
            assert_eq!(
                upload(mode, Answer::NothingTaken).resumed(),
                Some(upload(mode, merge)),
                "what is on the phone now is its own"
            );
            assert_eq!(upload(mode, merge).resumed(), Some(upload(mode, merge)));
            assert_eq!(
                upload(mode, Answer::Chose(dto::Conflict::KeepBoth)).resumed(),
                None,
                "its numbered folder can't be told from the one it went past"
            );
            assert_eq!(upload(mode, Answer::Unasked).resumed(), None);
        }
        assert_eq!(
            upload(dto::Paste::Copy, Answer::Chose(dto::Conflict::Skip)).resumed(),
            None,
            "what it made would be skipped too"
        );
        let skip = Answer::Chose(dto::Conflict::Skip);
        assert_eq!(
            upload(dto::Paste::Move, skip).resumed(),
            Some(upload(
                dto::Paste::Move,
                Answer::Retried(dto::Conflict::Skip)
            )),
            "as a move's Retry: an original goes only once it is checked on the phone"
        );
    }

    #[tokio::test]
    async fn a_paused_upload_carries_on_without_asking_again() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec![])));
        request(&mut handler, Request::Pause);
        finish_err(&mut handler, dto::Error::Cancelled);
        assert!(handler.is_paused());

        let resume = handler.update(Message::Request(Request::Resume));
        assert!(
            matches!(stage(&handler), Some(Stage::Running { .. })),
            "running, not checking or asking"
        );
        for message in crate::handler::testing::drain(resume).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            service.uploads.lock().unwrap().last(),
            Some(&(upload_request(), dto::Conflict::Merge)),
            "into what it made, sending what isn't there whole"
        );
        assert_eq!(handler.history.len(), 1);
    }

    #[test]
    fn a_paused_phone_job_ends_when_another_phone_opens() {
        let (mut handler, _) = paused_download();
        handler.forget_phone();

        assert!(!handler.is_paused() && !handler.has_phone_work());
        assert!(!handler.history[0].is_retryable());
        assert!(matches!(&handler.history[0].outcome, Err(e) if e.cancelled));
    }

    // Kept across a quit or a crash.

    fn saved_download() -> dto::SavedJob {
        dto::SavedJob::Download {
            request: download_request(),
            mode: dto::Paste::Copy,
            answer: dto::SavedAnswer::Retried(dto::Conflict::KeepBoth),
        }
    }

    #[test]
    fn what_the_queue_still_has_to_do_is_kept_as_it_would_run_next() {
        let mut queue = handler();
        assert!(queue.saved().is_empty());

        let _ = queue.download(download_request());
        let _ = queue.paste_on_mac(paste_request(dto::Paste::Copy));
        let saved = queue.saved();

        assert_eq!(saved.len(), 2);
        assert!(
            matches!(
                &saved[0],
                (
                    true,
                    dto::SavedJob::Download {
                        answer: dto::SavedAnswer::Retried(_),
                        ..
                    }
                )
            ),
            "the running download, as it carries on from its parts: {saved:?}"
        );
        assert!(
            matches!(&saved[1], (false, dto::SavedJob::OnMac(_))),
            "what waits, as asked; a Mac paste is the Mac's own"
        );

        // A copy inside one side that is running would be made twice: not kept.
        let mut copying = handler();
        let _ = copying.paste_on_phone(paste_request(dto::Paste::Copy));
        assert!(copying.saved().is_empty());

        // A question waiting, and a paused one, are kept as they are.
        let mut asking = handler();
        let _ = asking.upload(upload_request());
        let _ = asking.update(checked(Ok(vec!["a.jpg".into()])));
        assert!(matches!(
            &asking.saved()[..],
            [(true, dto::SavedJob::Upload { .. })]
        ));
        let (paused, _) = paused_download();
        assert!(matches!(
            &paused.saved()[..],
            [(true, dto::SavedJob::Download { .. })]
        ));
    }

    #[tokio::test]
    async fn transfers_from_last_time_wait_for_continue_or_discard() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.offer(vec![saved_download(), saved_download()]);

        assert!(
            handler.is_visible() && !handler.is_running(),
            "nothing runs unasked"
        );
        assert!(shows(&handler, "2 transfers weren't finished last time"));
        assert!(shows(&handler, mapper::offered_note()));
        assert_eq!(handler.saved().len(), 2, "kept while offered");
        assert!(
            !handler.has_phone_work(),
            "not started: the phone may switch"
        );

        let continued = {
            let mut task = Task::none();
            for message in clicked(&handler, "Continue") {
                task = handler.update(message);
            }
            task
        };
        let _ = crate::handler::testing::drain(continued).await;
        assert!(handler.is_running());
        assert_eq!(handler.waiting.len(), 1);
        assert!(handler.offered.is_empty());
        assert_eq!(
            service.requests.lock().unwrap().len(),
            1,
            "carried on, without asking"
        );

        let mut discarding = handler_with(FakeTransfer::new(Ok(summary()))).0;
        discarding.offer(vec![saved_download()]);
        for message in clicked(&discarding, "Discard") {
            let _ = discarding.update(message);
        }
        assert!(!discarding.is_visible() && discarding.saved().is_empty());
    }

    #[test]
    fn many_offered_are_counted_and_another_phone_takes_its_own_back() {
        let mut handler = handler();
        handler.offer(vec![saved_download(); 7]);
        assert!(shows(&handler, "and 2 more"));
        handler.offer(vec![dto::SavedJob::OnMac(paste_request(dto::Paste::Copy))]);

        let withdrawn = handler.withdraw_offered();

        assert_eq!(withdrawn.len(), 7, "the phone's");
        assert_eq!(handler.offered.len(), 1, "the Mac's own stays");
        assert!(shows(&handler, "1 transfer wasn't finished last time"));
    }

    #[test]
    fn the_queue_counts_each_change_and_nothing_else() {
        let mut handler = handler();
        let mut last = handler.changes();
        let mut moved = |handler: &Handler, what: &str| {
            assert!(handler.changes() > last, "{what} is a change");
            last = handler.changes();
        };

        handler.offer(vec![saved_download()]);
        moved(&handler, "offer");
        let _ = handler.withdraw_offered();
        moved(&handler, "withdraw");
        let _ = handler.download(download_request());
        moved(&handler, "enqueue");
        let _ = handler.update(Message::Progress(dto::TransferProgress::default()));
        moved(&handler, "a message");
        let _ = handler.phone_back();
        moved(&handler, "the phone back");
        handler.forget_phone();
        moved(&handler, "another phone");
        let _ = handler.stop_for_quit();
        moved(&handler, "a quit");

        let before = handler.changes();
        handler.set_link(Some(dto::UsbSpeed::High));
        handler.set_focused(false);
        handler.set_sound(true);
        let _ = handler.saved();
        let _ = handler.view();
        assert_eq!(handler.changes(), before, "told things, not changed");
    }

    #[test]
    fn every_kind_of_job_is_kept_and_comes_back_the_same() {
        let jobs = [
            Job::Download(download_request(), dto::Paste::Move, Answer::NothingTaken),
            Job::Upload(
                upload_request(),
                dto::Paste::Copy,
                Answer::Chose(dto::Conflict::Skip),
            ),
            Job::Upload(upload_request(), dto::Paste::Move, Answer::Unasked),
            Job::OnPhone(paste_request(dto::Paste::Move), true),
            Job::OnMac(paste_request(dto::Paste::Copy)),
            Job::Import(dto::ImportRequest {
                storage_id: 1,
                destination: "/Users/me".into(),
                folders: dto::ImportFolders::Day,
                remove_from_phone: false,
            }),
            Job::Mirror(dto::MirrorRequest {
                storage_id: 1,
                phone_folder: "/DCIM".into(),
                mac_folder: "/Users/me/DCIM".into(),
                direction: dto::MirrorDirection::ToMac,
                remove_extras: true,
            }),
            Job::Download(
                download_request(),
                dto::Paste::Copy,
                Answer::Retried(dto::Conflict::Replace),
            ),
        ];
        for job in jobs {
            let back = from_saved(to_saved(&job).unwrap());
            assert_eq!(to_saved(&back), to_saved(&job), "{:?}", job.label());
            assert_eq!(back.label(), job.label());
        }
    }

    // Stopping for a quit.

    #[test]
    fn a_quit_stops_what_runs_as_pause_would_and_starts_nothing_more() {
        let mut idle = handler();
        assert!(!idle.is_busy());
        assert!(!idle.stop_for_quit(), "nothing to wait for");

        let mut handler = handler();
        let _ = handler.download(download_request());
        let _ = handler.upload(upload_request());
        assert!(handler.is_busy());

        assert!(
            handler.stop_for_quit(),
            "the download stops at its next window"
        );
        assert!(
            matches!(
                stage(&handler),
                Some(Stage::Running { cancel, pausing: true, cancelling: false, .. }) if cancel.is_cancelled()
            ),
            "stopped as Pause stops it: its part is kept"
        );
        finish_err(&mut handler, dto::Error::Cancelled);
        assert!(!handler.is_running(), "and the upload doesn't start");
        assert!(handler.is_paused());
    }

    #[tokio::test]
    async fn a_quit_cancels_what_cannot_carry_on_and_drops_a_question() {
        // A copy inside the phone can't carry on: cancelled.
        let mut copying = handler();
        let _ = copying.paste_on_phone(paste_request(dto::Paste::Copy));
        assert!(copying.stop_for_quit());
        assert!(matches!(
            stage(&copying),
            Some(Stage::Running {
                cancelling: true,
                pausing: false,
                ..
            })
        ));

        // Stopped for good, it is listed; what waited behind it still
        // doesn't start.
        let _ = copying.upload(upload_request());
        finish_err(&mut copying, dto::Error::Cancelled);
        assert!(!copying.is_running() && !copying.is_paused());
        assert!(
            copying.is_busy(),
            "the upload is still in the queue, unstarted"
        );

        // A paused transfer is something a quit would stop too.
        let (paused, _) = paused_download();
        assert!(!paused.is_running() && paused.is_busy());

        // A question waiting for the user: nothing to stop.
        let mut asking = handler();
        let _ = asking.upload(upload_request());
        let _ = asking.update(checked(Ok(vec!["a.jpg".into()])));
        assert!(matches!(stage(&asking), Some(Stage::Asking { .. })));
        assert!(!asking.stop_for_quit());
        assert!(!asking.is_running());
    }

    // How long it took and how fast.

    #[tokio::test]
    async fn the_queue_times_each_run_from_start_to_end() {
        use iced::futures::StreamExt;
        let service: Arc<dyn Service> = Arc::new(FakeTransfer::new(Ok(summary())));
        let events = transfer_events(service, dto::Cancel::new(), |_, _, _| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(summary())
            })
        });

        let messages: Vec<Message> = events.collect().await;

        let elapsed = messages.iter().find_map(|message| match message {
            Message::Finished(Ok(summary)) => Some(summary.elapsed),
            _ => None,
        });
        assert!(elapsed.unwrap() >= Duration::from_millis(30), "{elapsed:?}");
    }

    #[test]
    fn a_finished_transfer_says_how_long_it_took_and_why_a_slow_link_was_slow() {
        let mut handler = handler();
        handler.set_link(Some(dto::UsbSpeed::High));
        let _ = handler.download(download_request());
        let _ = handler.update(Message::Finished(Ok(dto::TransferSummary {
            direction: dto::Direction::Download,
            bytes: 71 * 30_000_000,
            elapsed: Duration::from_secs(71),
            ..summary()
        })));

        assert!(shows(
            &handler,
            &mapper::timing_line(
                &dto::TransferSummary {
                    direction: dto::Direction::Download,
                    bytes: 71 * 30_000_000,
                    elapsed: Duration::from_secs(71),
                    ..summary()
                },
                Some(dto::UsbSpeed::High)
            )
            .unwrap()
        ));
        assert!(matches!(
            &handler.history[0].outcome,
            Ok(SummaryResponse { timing: Some(line), .. }) if line.starts_with("Took 1 min 11 s · ")
                && line.contains("USB 2")
        ));

        // A quick one says nothing about time.
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert!(matches!(
            &handler.history[1].outcome,
            Ok(SummaryResponse { timing: None, .. })
        ));
    }

    // The report of what became of each file.

    /// A finished download of `files` photos, each with its report line.
    fn reported_download(files: usize) -> (Handler, Arc<FakeTransfer>) {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.download(download_request());
        let report = (0..files)
            .map(|i| dto::FileReport {
                path: format!("IMG_{i:04}.jpg"),
                from: None,
                outcome: dto::FileOutcome::Copied { checked: true },
                original: None,
            })
            .collect();
        let _ = handler.update(Message::Finished(Ok(dto::TransferSummary {
            direction: dto::Direction::Download,
            files,
            report,
            ..summary()
        })));
        (handler, service)
    }

    #[test]
    fn details_show_what_became_of_each_file_in_the_finished_lists_place() {
        let (mut handler, _) = reported_download(3);
        assert!(shows(&handler, "Details"));
        assert!(!shows(&handler, "IMG_0001.jpg"));

        for message in clicked(&handler, "Details") {
            let _ = handler.update(message);
        }
        assert!(shows(&handler, "IMG_0001.jpg"));
        assert!(shows(&handler, "Copied and checked"));
        assert!(shows(&handler, "Save Report") && shows(&handler, "Copy"));
        assert!(!shows(&handler, "Clear"), "the finished list is behind it");

        for message in clicked(&handler, "‹ Finished") {
            let _ = handler.update(message);
        }
        assert!(shows(&handler, "Clear") && !shows(&handler, "IMG_0001.jpg"));
    }

    #[test]
    fn a_long_report_builds_only_the_rows_in_view() {
        let (mut handler, _) = reported_download(5_000);
        request(&mut handler, Request::Details(0));
        assert!(shows(&handler, "IMG_0000.jpg"));
        assert!(!shows(&handler, "IMG_4999.jpg"), "far below: not built");

        let _ = handler.update(Message::ReportScrolled(4_990.0 * REPORT_ROW));
        assert!(shows(&handler, "IMG_4999.jpg"));
        assert!(!shows(&handler, "IMG_0000.jpg"));

        let started = std::time::Instant::now();
        let _ = iced_test::simulator(handler.view());
        let took = started.elapsed();
        assert!(took < Duration::from_millis(500), "{took:?}");
    }

    #[tokio::test]
    async fn the_report_is_saved_where_the_files_went_or_copied_as_text() {
        let (mut handler, service) = reported_download(2);
        request(&mut handler, Request::Details(0));

        let saved = handler.update(Message::Request(Request::SaveReport));
        for message in crate::handler::testing::drain(saved).await.0 {
            let _ = handler.update(message);
        }
        let reports = service.reports.lock().unwrap().clone();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "/Users/me/Downloads");
        assert!(reports[0].1.starts_with("File\tWhat happened\tOriginal\n"));
        assert_eq!(reports[0].1.lines().count(), 3);
        assert!(shows(
            &handler,
            "Saved “Transfer report.tsv” in “Downloads”"
        ));

        let copied = handler.update(Message::Request(Request::CopyReport));
        let (_, clipboard) = crate::handler::testing::drain(copied).await;
        assert_eq!(clipboard, [reports[0].1.clone()]);
        assert!(shows(&handler, &mapper::copied_note(2)));

        let _ = handler.update(Message::ReportSaved(Err(error::to_error_response(
            dto::Error::ReadOnly,
        ))));
        assert!(shows(
            &handler,
            &format!(
                "Not saved: {}",
                error::to_error_response(dto::Error::ReadOnly).title
            )
        ));
    }

    #[test]
    fn an_uploads_details_can_be_copied_but_not_saved_on_the_phone() {
        let mut handler = handler();
        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec![])));
        let _ = handler.update(Message::Finished(Ok(dto::TransferSummary {
            direction: dto::Direction::Upload,
            files: 1,
            destination: "/DCIM".into(),
            report: vec![dto::FileReport {
                path: "a.jpg".into(),
                from: None,
                outcome: dto::FileOutcome::Sent,
                original: None,
            }],
            ..Default::default()
        })));
        for message in clicked(&handler, "Details") {
            let _ = handler.update(message);
        }

        assert!(shows(&handler, "Sent; the phone took it"));
        assert!(shows(&handler, "Copy"));
        assert!(
            !shows(&handler, "Save Report"),
            "nowhere on the Mac to put it"
        );
    }

    #[test]
    fn details_are_offered_only_where_there_is_a_report_and_close_when_the_list_changes() {
        let mut handler = handler();
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert!(!shows(&handler, "Details"), "no report");
        request(&mut handler, Request::Details(0));
        request(&mut handler, Request::Details(7));
        assert!(handler.details.is_none());
        request(&mut handler, Request::SaveReport);
        request(&mut handler, Request::CopyReport);

        let (mut handler, _) = reported_download(1);
        request(&mut handler, Request::Details(0));
        request(&mut handler, Request::Clear);
        assert!(handler.details.is_none(), "cleared");

        // A retry takes a row out, so the places of the rest move.
        let (mut handler, _) = reported_download(1);
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);
        request(&mut handler, Request::Details(0));
        assert_eq!(handler.details, Some(0));
        request(&mut handler, Request::Retry(1));
        assert!(handler.details.is_none(), "retried: closed");
    }

    // A transfer carried over a lost phone.

    /// A download running in the background, with an upload queued behind
    /// it, that loses the phone (`err`) part way.
    fn cut_off(err: dto::Error) -> (Handler, Arc<FakeTransfer>) {
        let (mut handler, service) = sounding();
        handler.set_focused(false);
        let _ = handler.download(download_request());
        let _ = handler.upload(upload_request());
        let _ = handler.update(Message::Progress(progress(2)));
        finish_err(&mut handler, err);
        (handler, service)
    }

    #[tokio::test]
    async fn a_download_cut_off_by_a_lost_phone_waits_for_it_and_carries_on_when_it_is_back() {
        let (mut handler, service) = cut_off(dto::Error::Disconnected);

        assert!(handler.waits_for_phone() && handler.is_paused());
        assert!(!handler.is_running(), "the upload waits behind it");
        assert!(handler.history.is_empty(), "not finished, not failed");
        assert!(handler.has_phone_work());
        assert_eq!(service.dock.lock().unwrap().last(), Some(&None));
        assert_eq!(service.awake.lock().unwrap().last(), Some(&false));
        assert!(service.sounds.lock().unwrap().is_empty(), "nothing ended");
        assert_eq!(
            service.notes.lock().unwrap().as_slice(),
            [mapper::phone_lost_note(
                &Job::Download(download_request(), dto::Paste::Copy, Answer::Unasked).label()
            )],
            "told, being away"
        );
        assert!(shows(&handler, "Waiting for the phone: Downloading 1 of 1"));
        assert!(shows(&handler, mapper::waiting_note()));
        assert!(
            !shows(&handler, "Resume"),
            "it resumes when the phone is back"
        );
        request(&mut handler, Request::Resume);
        assert!(handler.waits_for_phone(), "a Resume can't bring the phone");

        let task = handler.phone_back();
        assert!(!handler.is_paused());
        assert!(
            matches!(
                &handler.current,
                Some(Current { job: Job::Download(r, dto::Paste::Copy, Answer::Retried(_)), .. })
                    if *r == download_request()
            ),
            "carries on without asking, from its parts"
        );
        let _ = crate::handler::testing::drain(task).await;
        assert_eq!(service.requests.lock().unwrap().len(), 1);
        assert_eq!(handler.waiting.len(), 1, "the upload still after it");
        assert_eq!(service.awake.lock().unwrap().last(), Some(&true));
        assert!(handler.phone_back().units() == 0, "nothing more waits");
    }

    #[test]
    fn the_phone_coming_back_leaves_a_transfer_the_user_paused_paused() {
        let (mut handler, _) = paused_download();
        assert_eq!(handler.phone_back().units(), 0);
        assert!(handler.is_paused() && !handler.waits_for_phone());
        assert!(!handler.is_running());
    }

    #[test]
    fn a_silent_phone_holds_the_transfer_as_a_lost_one_does() {
        let (handler, _) = cut_off(dto::Error::Timeout);
        assert!(handler.waits_for_phone());
    }

    #[test]
    fn another_phone_or_cancel_ends_a_transfer_waiting_for_the_phone() {
        let (mut handler, _) = cut_off(dto::Error::Disconnected);
        handler.forget_phone();
        assert!(!handler.is_paused());
        assert!(matches!(&handler.history[0].outcome, Err(e) if e.cancelled));
        assert!(
            !handler.history[0].is_retryable(),
            "its paths are the other phone's"
        );
        assert_eq!(handler.phone_back().units(), 0);

        let (mut handler, _) = cut_off(dto::Error::Disconnected);
        for message in clicked(&handler, "Cancel") {
            let _ = handler.update(message);
        }
        assert!(!handler.waits_for_phone());
        assert!(handler.history[0].is_retryable());
        assert!(
            matches!(stage(&handler), Some(Stage::Checking)),
            "the upload's turn"
        );
    }

    #[test]
    fn what_cannot_carry_on_or_was_being_cancelled_ends_as_before_when_the_phone_is_lost() {
        // The user's own cancel, met by the phone going.
        let mut queue = handler();
        let _ = queue.download(download_request());
        request(&mut queue, Request::Cancel);
        finish_err(&mut queue, dto::Error::Disconnected);
        assert!(!queue.is_paused());
        assert!(matches!(&queue.history[0].outcome, Err(e) if e.device_lost));

        // A copy inside the phone can't carry on: listed, with Retry.
        let mut queue = handler();
        let _ = queue.paste_on_phone(paste_request(dto::Paste::Copy));
        finish_err(&mut queue, dto::Error::Disconnected);
        assert!(!queue.is_paused());
        assert!(queue.history[0].is_retryable());

        // Lost while asking the phone which names are taken: nothing started.
        let mut queue = handler();
        let _ = queue.upload(upload_request());
        let _ = queue.update(checked(Err(dto::Error::Disconnected)));
        assert!(!queue.is_paused());
    }

    #[test]
    fn a_pause_met_by_the_phone_going_waits_for_the_phone() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.download(download_request());
        request(&mut handler, Request::Pause);
        finish_err(&mut handler, dto::Error::Disconnected);
        assert!(handler.waits_for_phone());
        assert!(
            service.notes.lock().unwrap().is_empty(),
            "no notification while the app is in front"
        );
    }

    // The sound when the queue empties.

    fn sounding() -> (Handler, Arc<FakeTransfer>) {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.set_sound(true);
        (handler, service)
    }

    #[test]
    fn no_sound_unless_asked_for() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        assert!(service.sounds.lock().unwrap().is_empty());
    }

    #[test]
    fn one_sound_when_the_queue_empties_in_front_or_behind() {
        let (mut handler, service) = sounding();
        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert!(
            service.sounds.lock().unwrap().is_empty(),
            "the queue is not done yet"
        );
        finish_ok(&mut handler);
        assert_eq!(*service.sounds.lock().unwrap(), [dto::Sound::Done]);

        handler.set_focused(false);
        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert_eq!(
            *service.sounds.lock().unwrap(),
            [dto::Sound::Done, dto::Sound::Done],
            "the notification makes no sound of its own"
        );
    }

    #[test]
    fn a_problem_anywhere_in_the_batch_sounds_as_a_problem() {
        let (mut handler, service) = sounding();
        for _ in 0..3 {
            let _ = handler.download(download_request());
        }
        let _ = handler.update(Message::Finished(Ok(partly_done())));
        finish_ok(&mut handler);
        finish_ok(&mut handler);
        assert_eq!(*service.sounds.lock().unwrap(), [dto::Sound::Problem]);

        let _ = handler.download(download_request());
        finish_ok(&mut handler);
        assert_eq!(
            service.sounds.lock().unwrap().last(),
            Some(&dto::Sound::Done),
            "the next batch starts afresh"
        );
    }

    #[test]
    fn the_users_own_cancel_makes_no_sound_but_a_stop_does() {
        let (mut handler, service) = sounding();
        let _ = handler.download(download_request());
        request(&mut handler, Request::Cancel);
        finish_err(&mut handler, dto::Error::Cancelled);
        assert!(
            service.sounds.lock().unwrap().is_empty(),
            "they are looking"
        );

        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        request(&mut handler, Request::Cancel);
        finish_err(&mut handler, dto::Error::Cancelled);
        finish_ok(&mut handler);
        assert_eq!(
            *service.sounds.lock().unwrap(),
            [dto::Sound::Done],
            "what ran after the cancel is still told"
        );

        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::Cancelled);
        assert_eq!(
            service.sounds.lock().unwrap().last(),
            Some(&dto::Sound::Problem),
            "stopped without the user asking"
        );
        let _ = handler.download(download_request());
        request(&mut handler, Request::Cancel);
        finish_ok(&mut handler);
        assert_eq!(
            service.sounds.lock().unwrap().last(),
            Some(&dto::Sound::Done),
            "a cancel too late to stop it: the transfer finished, and says so"
        );
    }

    #[test]
    fn clearing_the_finished_list_forgets_what_the_sound_would_say() {
        let (mut handler, service) = sounding();
        let _ = handler.download(download_request());
        let _ = handler.download(download_request());
        finish_err(&mut handler, dto::Error::MacDiskFull);

        request(&mut handler, Request::Clear);
        finish_ok(&mut handler);

        assert_eq!(*service.sounds.lock().unwrap(), [dto::Sound::Done]);
    }

    #[test]
    fn a_question_while_the_app_is_in_front_is_not_notified() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));

        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["a.txt".into()])));

        assert!(service.notes.lock().unwrap().is_empty());
    }

    #[test]
    fn a_question_waiting_in_the_background_is_notified() {
        let (mut handler, service) = handler_with(FakeTransfer::new(Ok(summary())));
        handler.set_focused(false);

        let _ = handler.upload(upload_request());
        let _ = handler.update(checked(Ok(vec!["a.txt".into(), "b.txt".into()])));

        assert_eq!(
            *service.notes.lock().unwrap(),
            [(
                "Waiting for you".to_owned(),
                "2 names are already taken on the phone: choose what to do with them.".to_owned()
            )]
        );
    }
}
