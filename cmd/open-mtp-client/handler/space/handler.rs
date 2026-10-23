//! Free Up Space: a dialog over the phone's storage that
//! looks first (copies of the same file, photos already imported to this
//! Mac), lets the user pick, asks, and removes what was picked. Its list is
//! built once per look and drawn only where it is on screen.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::widget::{button, checkbox, column, container, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper::{self, Line};
use super::request::Request;
use crate::handler::error::{self, ErrorResponse};
use crate::handler::guard;
use crate::i18n::{t, tf};

/// Told how a look, or a removal, is going.
pub type Told<T> = Box<dyn Fn(T) + Send + Sync>;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Walks the storage and says what could go.
    async fn look(
        &self,
        storage_id: u64,
        cancel: dto::Cancel,
        progress: Told<dto::SpaceProgress>,
    ) -> Result<dto::SpaceReport, dto::Error>;

    /// Removes the picked files, each only if still as the look found it;
    /// `removed` is told how many have gone so far.
    async fn free_up(
        &self,
        request: dto::FreeUpRequest,
        cancel: dto::Cancel,
        removed: Told<usize>,
    ) -> Result<dto::Removal, dto::Error>;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Progress {
        look: u64,
        progress: dto::SpaceProgress,
    },
    Looked {
        look: u64,
        result: Result<dto::SpaceReport, ErrorResponse>,
    },
    Scrolled(f32),
    RemovedSome {
        look: u64,
        done: usize,
    },
    Removed {
        look: u64,
        result: Result<dto::Removal, ErrorResponse>,
    },
}

/// One row of the list.
pub const ROW: f32 = 26.0;
/// How tall the list is drawn.
pub const LIST_HEIGHT: f32 = 320.0;

enum Stage {
    Looking(Option<dto::SpaceProgress>),
    Ready,
    Confirming,
    Removing {
        done: usize,
        total: usize,
    },
    Done {
        removal: dto::Removal,
        title: String,
        notes: Vec<String>,
    },
    Failed(ErrorResponse),
}

struct Dialog {
    storage_id: u64,
    storage: String,
    stage: Stage,
    report: dto::SpaceReport,
    lines: Vec<Line>,
    /// Each file's size, for the bytes picked.
    sizes: HashMap<String, u64>,
    picked: HashSet<String>,
    picked_bytes: u64,
    offset: f32,
    look: u64,
    cancel: dto::Cancel,
}

pub struct Handler {
    service: Arc<dyn Service>,
    dialog: Option<Dialog>,
    looks: u64,
    /// What "today" is for the dates shown (fixed in tests).
    today: fn() -> chrono::NaiveDate,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            dialog: None,
            looks: 0,
            today: crate::handler::format::local_today,
        }
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Looking at the phone or removing from it: work on its one session.
    pub fn is_busy(&self) -> bool {
        self.dialog.as_ref().is_some_and(|dialog| {
            matches!(dialog.stage, Stage::Looking(_) | Stage::Removing { .. })
        })
    }

    /// Opens the dialog on the storage `storage_id` and starts looking.
    pub fn open(&mut self, storage_id: u64, storage: &str) -> Task<Message> {
        self.close();
        self.dialog = Some(Dialog {
            storage_id,
            storage: storage.to_owned(),
            stage: Stage::Looking(None),
            report: dto::SpaceReport::default(),
            lines: Vec::new(),
            sizes: HashMap::new(),
            picked: HashSet::new(),
            picked_bytes: 0,
            offset: 0.0,
            look: 0,
            cancel: dto::Cancel::new(),
        });
        self.look()
    }

    pub fn language_changed(&mut self) {
        let today = (self.today)();
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if !dialog.lines.is_empty() {
            dialog.lines = mapper::lines(&dialog.report, today);
        }
        match &mut dialog.stage {
            Stage::Failed(error) => *error = error.reworded(),
            Stage::Done {
                removal,
                title,
                notes,
            } => (*title, *notes) = mapper::removed(removal),
            _ => {}
        }
    }

    /// Closes the dialog, stopping what it was doing (between windows or
    /// between files).
    pub fn close(&mut self) {
        if let Some(dialog) = self.dialog.take() {
            dialog.cancel.cancel();
        }
    }

    fn look(&mut self) -> Task<Message> {
        self.looks += 1;
        let look = self.looks;
        let Some(dialog) = self.dialog.as_mut() else {
            return Task::none();
        };
        dialog.cancel.cancel();
        dialog.cancel = dto::Cancel::new();
        dialog.look = look;
        dialog.stage = Stage::Looking(None);
        dialog.picked.clear();
        dialog.picked_bytes = 0;
        dialog.offset = 0.0;
        let (service, cancel, storage_id) = (
            Arc::clone(&self.service),
            dialog.cancel.clone(),
            dialog.storage_id,
        );
        guard::run(
            iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
                let told = Mutex::new(output.clone());
                // Best effort: a dropped report is replaced by the next.
                let progress: Told<dto::SpaceProgress> = Box::new(move |progress| {
                    if let Ok(mut sender) = told.lock() {
                        let _ = sender.try_send(Message::Progress { look, progress });
                    }
                });
                let result = service
                    .look(storage_id, cancel, progress)
                    .await
                    .map_err(error::to_error_response);
                let _ = output.send(Message::Looked { look, result }).await;
            }),
            move || Message::Looked {
                look,
                result: Err(guard::crashed()),
            },
            |message| message,
        )
    }

    fn remove(&mut self) -> Task<Message> {
        let Some(dialog) = self.dialog.as_mut() else {
            return Task::none();
        };
        // In the order the list shows them.
        let paths: Vec<String> = dialog
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::File { path, .. } if dialog.picked.contains(path) => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        dialog.stage = Stage::Removing {
            done: 0,
            total: paths.len(),
        };
        dialog.cancel = dto::Cancel::new();
        let look = dialog.look;
        let request = dto::FreeUpRequest {
            storage_id: dialog.storage_id,
            paths,
        };
        let (service, cancel) = (Arc::clone(&self.service), dialog.cancel.clone());
        guard::run(
            iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
                let told = Mutex::new(output.clone());
                let removed: Told<usize> = Box::new(move |done| {
                    if let Ok(mut sender) = told.lock() {
                        let _ = sender.try_send(Message::RemovedSome { look, done });
                    }
                });
                let result = service
                    .free_up(request, cancel, removed)
                    .await
                    .map_err(error::to_error_response);
                let _ = output.send(Message::Removed { look, result }).await;
            }),
            // Some files may be gone by then; the dialog says so and `app.rs`
            // reads the phone again.
            move || Message::Removed {
                look,
                result: Err(guard::crashed()),
            },
            |message| message,
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(Request::Close | Request::BiggestFiles) => {
                self.close();
                Task::none()
            }
            Message::Request(Request::LookAgain) => self.look(),
            Message::Request(Request::ConfirmRemove) => match &self.dialog {
                Some(dialog) if matches!(dialog.stage, Stage::Confirming) => self.remove(),
                _ => Task::none(),
            },
            Message::Request(request) => {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.pick(request);
                }
                Task::none()
            }
            Message::Progress { look, progress } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.look == look
                    && let Stage::Looking(shown) = &mut dialog.stage
                {
                    *shown = Some(progress);
                }
                Task::none()
            }
            Message::Looked { look, result } => {
                let today = (self.today)();
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.look == look
                {
                    match result {
                        Ok(report) => {
                            dialog.lines = mapper::lines(&report, today);
                            dialog.sizes = report
                                .duplicates
                                .iter()
                                .flat_map(|group| &group.files)
                                .chain(&report.imported)
                                .map(|file| (file.path.clone(), file.size))
                                .collect();
                            dialog.report = report;
                            dialog.stage = Stage::Ready;
                        }
                        Err(error) if error.cancelled => {}
                        Err(error) => dialog.stage = Stage::Failed(error),
                    }
                }
                Task::none()
            }
            Message::Scrolled(offset) => {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.offset = offset;
                }
                Task::none()
            }
            Message::RemovedSome { look, done } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.look == look
                    && let Stage::Removing { done: shown, .. } = &mut dialog.stage
                {
                    *shown = done;
                }
                Task::none()
            }
            Message::Removed { look, result } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.look == look
                {
                    dialog.stage = match result {
                        Ok(removal) => {
                            note_problems(&removal);
                            let (title, notes) = mapper::removed(&removal);
                            Stage::Done {
                                removal,
                                title,
                                notes,
                            }
                        }
                        Err(error) => Stage::Failed(error),
                    };
                    dialog.picked.clear();
                    dialog.picked_bytes = 0;
                }
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(dialog) = &self.dialog else {
            return space().into();
        };
        let send = Message::Request;
        let close = button(text(t("label.close")))
            .style(button::secondary)
            .on_press(send(Request::Close));
        let biggest = button(text(t("space.biggest_files")))
            .style(button::secondary)
            .on_press(send(Request::BiggestFiles));
        let body: Element<'_, Message> = match &dialog.stage {
            Stage::Looking(progress) => column![
                text(mapper::looking(*progress)),
                text(t("space.explain")).size(12).style(text::secondary),
                row![
                    space::horizontal(),
                    button(text(t("space.stop")))
                        .style(button::secondary)
                        .on_press(send(Request::Close))
                ],
            ]
            .spacing(12)
            .into(),
            Stage::Failed(error) => column![
                text(&error.title).style(text::danger),
                text(&error.hint).size(12).style(text::secondary),
                row![
                    space::horizontal(),
                    close,
                    button(text(t("space.look_again")))
                        .style(button::primary)
                        .on_press(send(Request::LookAgain)),
                ]
                .spacing(8),
            ]
            .spacing(8)
            .into(),
            Stage::Done { title, notes, .. } => column![text(title).size(16)]
                .extend(
                    notes
                        .iter()
                        .map(|note| text(note).size(12).style(text::secondary).into()),
                )
                .push(
                    row![
                        space::horizontal(),
                        close,
                        button(text(t("space.look_again")))
                            .style(button::primary)
                            .on_press(send(Request::LookAgain)),
                    ]
                    .spacing(8),
                )
                .spacing(8)
                .into(),
            Stage::Removing { done, total } => column![
                text(mapper::removing(*done, *total)),
                row![
                    space::horizontal(),
                    button(text(t("space.stop")))
                        .style(button::secondary)
                        .on_press(send(Request::Close))
                ],
            ]
            .spacing(12)
            .into(),
            Stage::Ready | Stage::Confirming => {
                let picked = dialog.picked.len();
                let footer: Element<'_, Message> = if matches!(dialog.stage, Stage::Confirming) {
                    column![
                        text(mapper::confirm(picked, dialog.picked_bytes)).size(13),
                        row![
                            space::horizontal(),
                            button(text(t("space.keep_them")))
                                .style(button::secondary)
                                .on_press(send(Request::KeepThem)),
                            button(text(t("label.delete")))
                                .style(button::danger)
                                .on_press(send(Request::ConfirmRemove)),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8)
                    .into()
                } else {
                    row![
                        biggest,
                        space::horizontal(),
                        close,
                        button(text(mapper::remove_label(picked, dialog.picked_bytes)))
                            .style(button::danger)
                            .on_press_maybe((picked > 0).then(|| send(Request::Remove))),
                    ]
                    .spacing(8)
                    .into()
                };
                column![
                    text(mapper::found(&dialog.report)).size(14),
                    row![
                        button(text(t("space.select_copies")).size(13))
                            .style(button::secondary)
                            .on_press_maybe(
                                (!dialog.report.duplicates.is_empty())
                                    .then(|| send(Request::SelectCopies))
                            ),
                        button(text(t("space.select_imported")).size(13))
                            .style(button::secondary)
                            .on_press_maybe(
                                (!dialog.report.imported.is_empty())
                                    .then(|| send(Request::SelectImported))
                            ),
                        button(text(t("space.select_none")).size(13))
                            .style(button::text)
                            .on_press_maybe((picked > 0).then(|| send(Request::SelectNone))),
                    ]
                    .spacing(8),
                    dialog.list(),
                    footer,
                ]
                .spacing(10)
                .into()
            }
        };
        container(
            column![
                text(tf("space.title", &[("storage", &dialog.storage)])).size(20),
                rule::horizontal(1),
                body,
            ]
            .spacing(12),
        )
        .padding(24)
        .width(Length::Fixed(640.0))
        .style(container::rounded_box)
        .into()
    }
}

fn note_problems(removal: &dto::Removal) {
    let refused = removal.kept.iter().filter_map(|kept| match &kept.why {
        dto::Why::Refused(error) => Some(error),
        _ => None,
    });
    for error in refused.chain(removal.stopped.iter()) {
        error::note_shown(error);
    }
}

impl Dialog {
    fn pick(&mut self, request: Request) {
        match request {
            Request::Toggle(path) => {
                if !matches!(self.stage, Stage::Ready) || !self.sizes.contains_key(&path) {
                    return;
                }
                if !self.picked.remove(&path) {
                    self.picked.insert(path);
                }
            }
            Request::SelectCopies => {
                for group in &self.report.duplicates {
                    self.picked
                        .extend(group.files.iter().skip(1).map(|file| file.path.clone()));
                }
            }
            Request::SelectImported => {
                self.picked
                    .extend(self.report.imported.iter().map(|file| file.path.clone()));
            }
            Request::SelectNone => self.picked.clear(),
            Request::Remove if !self.picked.is_empty() && matches!(self.stage, Stage::Ready) => {
                self.stage = Stage::Confirming;
            }
            Request::KeepThem if matches!(self.stage, Stage::Confirming) => {
                self.stage = Stage::Ready;
            }
            _ => {}
        }
        // A file can be in a group and imported both: counted once.
        self.picked_bytes = self
            .picked
            .iter()
            .filter_map(|path| self.sizes.get(path))
            .sum();
    }

    /// The rows in view, with spacers for the rest.
    fn list(&self) -> Element<'_, Message> {
        let count = self.lines.len();
        let shown = crate::handler::explorer::window::rows(self.offset, LIST_HEIGHT, ROW, count);
        let spacer = |rows: usize| space().height(rows as f32 * ROW);
        let mut list = column![spacer(shown.start)];
        for line in &self.lines[shown.clone()] {
            let content: Element<'_, Message> = match line {
                Line::Heading(title) => text(title).size(13).into(),
                Line::Group(title) => text(title).size(12).style(text::secondary).into(),
                Line::Empty(said) => text(said).size(12).style(text::secondary).into(),
                Line::File {
                    path,
                    detail,
                    original,
                } => {
                    let mut line = row![
                        checkbox(self.picked.contains(path))
                            .label(path.as_str())
                            .text_size(12)
                            .on_toggle(|_| Message::Request(Request::Toggle(path.clone()))),
                        space::horizontal(),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center);
                    if *original {
                        line = line.push(text(t("space.original")).size(11).style(text::success));
                    }
                    line.push(text(detail).size(11).style(text::secondary))
                        .into()
                }
            };
            list = list.push(
                container(content)
                    .height(ROW)
                    .align_y(Alignment::Center)
                    .clip(true),
            );
        }
        list = list.push(spacer(count - shown.end));
        scrollable(list)
            .height(Length::Fixed(LIST_HEIGHT))
            .on_scroll(|viewport| Message::Scrolled(viewport.absolute_offset().y))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeSpace, drain};

    fn file(path: &str, size: u64) -> dto::SpaceFile {
        dto::SpaceFile {
            path: path.into(),
            size,
            modified: None,
        }
    }

    /// One group of three 300 KB copies and one imported photo, which is
    /// also the second copy.
    fn report() -> dto::SpaceReport {
        dto::SpaceReport {
            storage_id: 5,
            files: 40,
            duplicates: vec![dto::DuplicateGroup {
                size: 300_000,
                files: vec![
                    file("/Download/IMG_1.jpg", 300_000),
                    file("/DCIM/Camera/IMG_1.jpg", 300_000),
                    file("/WhatsApp/IMG-1.jpg", 300_000),
                ],
            }],
            imported: vec![
                file("/DCIM/Camera/IMG_1.jpg", 300_000),
                file("/DCIM/Camera/IMG_2.jpg", 2_000_000),
            ],
        }
    }

    async fn looked(fake: FakeSpace) -> (Handler, Arc<FakeSpace>) {
        let fake = Arc::new(fake);
        let mut handler = Handler::new(fake.clone());
        let task = handler.open(5, "Internal");
        assert!(handler.is_busy(), "it reads the phone");
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
        (handler, fake)
    }

    fn picked(handler: &Handler) -> (usize, u64) {
        let dialog = handler.dialog.as_ref().unwrap();
        (dialog.picked.len(), dialog.picked_bytes)
    }

    fn ask(handler: &mut Handler, request: Request) -> Task<Message> {
        handler.update(Message::Request(request))
    }

    // ---- service code that panics: the dialog is never left waiting ----

    fn says_crashed(handler: &Handler) -> bool {
        matches!(
            &handler.dialog.as_ref().unwrap().stage,
            Stage::Failed(error) if error.source == dto::Error::Crashed
        )
    }

    #[tokio::test]
    async fn a_look_that_panics_ends_as_a_failure_not_an_endless_look() {
        let (handler, _) = looked(FakeSpace {
            panics_in: Some("look"),
            ..FakeSpace::default()
        })
        .await;

        assert!(!handler.is_busy(), "not left on Looking…");
        assert!(says_crashed(&handler));
    }

    #[tokio::test]
    async fn a_removal_that_panics_ends_as_a_failure_not_an_endless_removing() {
        let (mut handler, _) = looked(FakeSpace {
            report: Ok(report()),
            panics_in: Some("free_up"),
            ..FakeSpace::default()
        })
        .await;
        let _ = ask(&mut handler, Request::SelectCopies);
        let _ = ask(&mut handler, Request::Remove);

        let task = ask(&mut handler, Request::ConfirmRemove);
        assert!(handler.is_busy());
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert!(!handler.is_busy(), "not left on Removing…");
        assert!(says_crashed(&handler));
    }

    #[tokio::test]
    async fn the_look_lists_copies_with_the_one_to_keep_and_what_is_already_on_this_mac() {
        let (handler, fake) = looked(FakeSpace {
            report: Ok(report()),
            progress: vec![dto::SpaceProgress::Walking { files: 40 }],
            ..FakeSpace::default()
        })
        .await;

        assert_eq!(*fake.looks.lock().unwrap(), [5]);
        assert!(!handler.is_busy());
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Free Up Space on Internal").is_ok());
        assert!(
            ui.find("2 copies could go (600.0 KB) · 2 files already on this Mac (2.3 MB)")
                .is_ok()
        );
        assert!(ui.find("IMG_1.jpg · 3 copies of 300.0 KB").is_ok());
        assert!(ui.find("Original").is_ok());
        assert!(ui.find("/WhatsApp/IMG-1.jpg").is_ok());
        assert!(ui.find("Remove…").is_ok(), "nothing picked yet");
    }

    #[tokio::test]
    async fn a_change_of_language_words_the_list_the_result_and_a_failure_again_keeping_the_picks()
    {
        use internal::dto::Language;

        let (mut handler, _) = looked(FakeSpace {
            report: Ok(report()),
            removal: Ok(dto::Removal {
                removed: 1,
                freed: 300_000,
                kept: vec![dto::Kept {
                    label: "/WhatsApp/IMG-1.jpg".into(),
                    why: dto::Why::Changed,
                }],
                ..Default::default()
            }),
            ..FakeSpace::default()
        })
        .await;
        let _ = ask(&mut handler, Request::SelectCopies);
        assert_eq!(picked(&handler), (2, 600_000));
        let says = |handler: &Handler, wanted: &str| {
            iced_test::simulator(handler.view())
                .find(wanted.to_owned())
                .is_ok()
        };
        assert!(says(&handler, "Original"));

        crate::i18n::set_language(Language::Japanese);
        handler.language_changed();

        for key in ["space.original", "space.select_copies", "space.no_two_same"] {
            let wanted = crate::i18n::t(key);
            if key != "space.no_two_same" {
                assert!(says(&handler, wanted), "{key}");
            }
        }
        assert!(!says(&handler, "Original"));
        let heading = crate::i18n::tf("space.heading_copies", &[("size", "600.0 KB")]);
        assert!(says(&handler, &heading));
        assert_eq!(picked(&handler), (2, 600_000), "the picks stay");

        let _ = ask(&mut handler, Request::Remove);
        let task = ask(&mut handler, Request::ConfirmRemove);
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
        let removed = crate::i18n::tf(
            "space.removed",
            &[
                ("files", &crate::i18n::count("text.files", 1, &[])),
                ("size", "300.0 KB"),
            ],
        );
        assert!(says(&handler, &removed));
        crate::i18n::set_language(Language::Thai);
        handler.language_changed();
        let removed = crate::i18n::tf(
            "space.removed",
            &[
                ("files", &crate::i18n::count("text.files", 1, &[])),
                ("size", "300.0 KB"),
            ],
        );
        assert!(says(&handler, &removed), "what was done is said again");

        let (mut failed, _) = looked(FakeSpace {
            report: Err(dto::Error::Timeout),
            ..FakeSpace::default()
        })
        .await;
        crate::i18n::set_language(Language::ChineseSimplified);
        failed.language_changed();
        let title = error::to_error_response(dto::Error::Timeout).title;
        assert!(says(&failed, &title));
    }

    #[tokio::test]
    async fn select_copies_leaves_the_original_and_a_file_picked_twice_counts_once() {
        let (mut handler, _) = looked(FakeSpace {
            report: Ok(report()),
            ..FakeSpace::default()
        })
        .await;

        let _ = ask(&mut handler, Request::SelectCopies);
        assert_eq!(picked(&handler), (2, 600_000));
        assert!(
            !handler
                .dialog
                .as_ref()
                .unwrap()
                .picked
                .contains("/Download/IMG_1.jpg")
        );
        let _ = ask(&mut handler, Request::SelectImported);
        assert_eq!(
            picked(&handler),
            (3, 2_600_000),
            "the camera's copy counted once"
        );
        let _ = ask(&mut handler, Request::Toggle("/WhatsApp/IMG-1.jpg".into()));
        assert_eq!(picked(&handler), (2, 2_300_000));
        let _ = ask(&mut handler, Request::Toggle("/not/listed.jpg".into()));
        assert_eq!(picked(&handler).0, 2, "only what the look listed");
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Remove 2 Files (2.3 MB)…").is_ok());
        drop(ui);
        let _ = ask(&mut handler, Request::SelectNone);
        assert_eq!(picked(&handler), (0, 0));
    }

    #[tokio::test]
    async fn removing_asks_first_then_removes_what_was_picked_in_the_order_shown() {
        let (mut handler, fake) = looked(FakeSpace {
            report: Ok(report()),
            removal: Ok(dto::Removal {
                removed: 1,
                freed: 300_000,
                kept: vec![dto::Kept {
                    label: "/WhatsApp/IMG-1.jpg".into(),
                    why: dto::Why::Changed,
                }],
                ..Default::default()
            }),
            ..FakeSpace::default()
        })
        .await;
        let _ = ask(&mut handler, Request::SelectCopies);

        // Asking first; keeping them changes nothing.
        let _ = ask(&mut handler, Request::Remove);
        let mut ui = iced_test::simulator(handler.view());
        assert!(
            ui.find("2 files (600.0 KB) will be deleted from the phone for good. Each is looked up again first: one that changed since the look stays.")
                .is_ok()
        );
        drop(ui);
        let _ = ask(&mut handler, Request::KeepThem);
        assert!(
            drain(ask(&mut handler, Request::ConfirmRemove))
                .await
                .0
                .is_empty(),
            "not asked"
        );
        assert!(fake.removals.lock().unwrap().is_empty());

        let _ = ask(&mut handler, Request::Remove);
        let task = ask(&mut handler, Request::ConfirmRemove);
        assert!(handler.is_busy());
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(
            *fake.removals.lock().unwrap(),
            [dto::FreeUpRequest {
                storage_id: 5,
                paths: vec![
                    "/DCIM/Camera/IMG_1.jpg".into(),
                    "/WhatsApp/IMG-1.jpg".into()
                ],
            }]
        );
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Removed 1 file, 300.0 KB freed").is_ok());
        assert!(
            ui.find("/WhatsApp/IMG-1.jpg stayed: it changed since the look, or wasn't in it")
                .is_ok()
        );
        assert!(ui.find("Look Again").is_ok());
    }

    #[tokio::test]
    async fn a_look_that_fails_says_why_and_can_be_tried_again() {
        let (mut handler, fake) = looked(FakeSpace {
            report: Err(dto::Error::Disconnected),
            ..FakeSpace::default()
        })
        .await;

        let error = error::to_error_response(dto::Error::Disconnected);
        assert!(
            iced_test::simulator(handler.view())
                .find(error.title.as_str())
                .is_ok()
        );
        let again = ask(&mut handler, Request::LookAgain);
        let _ = drain(again).await;
        assert_eq!(fake.looks.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn answers_from_an_earlier_look_and_a_cancelled_one_are_dropped() {
        let fake = Arc::new(FakeSpace::default());
        let mut handler = Handler::new(fake);
        let _ = handler.open(5, "Internal");
        let _ = handler.update(Message::Progress {
            look: 1,
            progress: dto::SpaceProgress::Comparing {
                done: 1_000_000,
                total: 4_000_000,
            },
        });
        assert!(
            iced_test::simulator(handler.view())
                .find("Reading files of the same size to be sure they are copies: 1.0 MB of 4.0 MB")
                .is_ok()
        );
        let _ = handler.update(Message::Looked {
            look: 99,
            result: Ok(report()),
        });
        assert!(handler.is_busy(), "another look's answer");
        let _ = handler.update(Message::Looked {
            look: 1,
            result: Err(error::to_error_response(dto::Error::Cancelled)),
        });
        assert!(handler.is_busy(), "a stop says nothing");

        handler.close();
        assert!(!handler.is_open() && !handler.is_busy());
    }

    #[tokio::test]
    async fn only_the_rows_on_screen_are_built_however_many_there_are() {
        let many = dto::SpaceReport {
            imported: (0..5_000)
                .map(|i| file(&format!("/DCIM/Camera/IMG_{i:05}.jpg"), 1_000))
                .collect(),
            ..dto::SpaceReport::default()
        };
        let (mut handler, _) = looked(FakeSpace {
            report: Ok(many),
            ..FakeSpace::default()
        })
        .await;

        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("/DCIM/Camera/IMG_00000.jpg").is_ok());
        assert!(ui.find("/DCIM/Camera/IMG_04999.jpg").is_err(), "not built");
        drop(ui);
        let _ = handler.update(Message::Scrolled(4_999.0 * ROW));
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("/DCIM/Camera/IMG_04999.jpg").is_ok());
        drop(ui);
        let started = std::time::Instant::now();
        let _ = ask(&mut handler, Request::SelectImported);
        assert_eq!(picked(&handler), (5_000, 5_000_000));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
