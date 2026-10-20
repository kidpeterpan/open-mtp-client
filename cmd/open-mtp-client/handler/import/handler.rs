//! The import dialog: what would come over from the phone's DCIM folder,
//! worked out before anything is copied, and the choice of a folder per day.
//! The import itself is queued as a transfer by `app.rs`.

use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::widget::{button, checkbox, column, container, row, rule, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::Request;
use crate::handler::error::{self, ErrorResponse};
use crate::i18n::{t, tf};

/// Told how many photos and videos the walk has found so far.
pub type Seen = Box<dyn Fn(usize) + Send + Sync>;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// What an import would do now, without copying anything.
    async fn preview(
        &self,
        request: dto::ImportRequest,
        cancel: dto::Cancel,
        seen: Seen,
    ) -> Result<dto::ImportPreview, dto::Error>;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    /// The walk of preview `preview` has found `count` so far.
    Seen {
        preview: u64,
        count: usize,
    },
    Previewed {
        preview: u64,
        result: Result<dto::ImportPreview, Problem>,
    },
}

/// Why there is no preview.
#[derive(Debug, Clone)]
pub enum Problem {
    /// The storage has no DCIM folder.
    NoDcim,
    Failed(ErrorResponse),
}

enum Preview {
    Looking { seen: usize },
    Ready(dto::ImportPreview),
    Failed(Problem),
}

struct Dialog {
    storage_id: u64,
    storage: String,
    destination: String,
    /// How the destination is shown ("~/Pictures").
    shown_destination: String,
    folders: dto::ImportFolders,
    /// Free the phone afterwards.
    remove: bool,
    preview: Preview,
    /// Pieces of an earlier preview (before the folder-per-day choice
    /// changed, say) are dropped.
    preview_id: u64,
    cancel: dto::Cancel,
}

pub struct Handler {
    service: Arc<dyn Service>,
    dialog: Option<Dialog>,
    previews: u64,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            dialog: None,
            previews: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Opens the dialog for the storage `storage_id`, importing into
    /// `destination` (shown from `home`), and starts looking.
    pub fn open(
        &mut self,
        storage_id: u64,
        storage: &str,
        destination: &str,
        home: &str,
        folders: dto::ImportFolders,
        remove: bool,
    ) -> Task<Message> {
        self.close();
        self.dialog = Some(Dialog {
            storage_id,
            storage: storage.to_owned(),
            destination: destination.to_owned(),
            shown_destination: mapper::destination_line(destination, home),
            folders,
            remove,
            preview: Preview::Looking { seen: 0 },
            preview_id: 0,
            cancel: dto::Cancel::new(),
        });
        self.look()
    }

    pub fn language_changed(&mut self) {
        if let Some(dialog) = self.dialog.as_mut()
            && let Preview::Failed(Problem::Failed(error)) = &mut dialog.preview
        {
            *error = error.reworded();
        }
    }

    /// Closes the dialog, stopping a preview under way.
    pub fn close(&mut self) {
        if let Some(dialog) = self.dialog.take() {
            dialog.cancel.cancel();
        }
    }

    /// The import as the dialog stands, if there is something to import.
    pub fn request(&self) -> Option<dto::ImportRequest> {
        let dialog = self.dialog.as_ref()?;
        match &dialog.preview {
            Preview::Ready(preview) if preview.new_files > 0 => Some(dto::ImportRequest {
                storage_id: dialog.storage_id,
                destination: dialog.destination.clone(),
                folders: dialog.folders,
                remove_from_phone: dialog.remove,
            }),
            _ => None,
        }
    }

    /// Starts a preview as the dialog stands, dropping one under way.
    fn look(&mut self) -> Task<Message> {
        self.previews += 1;
        let preview = self.previews;
        let Some(dialog) = self.dialog.as_mut() else {
            return Task::none();
        };
        dialog.cancel.cancel();
        dialog.cancel = dto::Cancel::new();
        dialog.preview_id = preview;
        dialog.preview = Preview::Looking { seen: 0 };
        let request = dto::ImportRequest {
            storage_id: dialog.storage_id,
            destination: dialog.destination.clone(),
            folders: dialog.folders,
            remove_from_phone: false,
        };
        let (service, cancel) = (Arc::clone(&self.service), dialog.cancel.clone());
        Task::run(
            iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
                let counted = Mutex::new(output.clone());
                // Best effort: a dropped count is replaced by the next one.
                let seen: Seen = Box::new(move |count| {
                    if let Ok(mut sender) = counted.lock() {
                        let _ = sender.try_send(Message::Seen { preview, count });
                    }
                });
                let result =
                    service
                        .preview(request, cancel, seen)
                        .await
                        .map_err(|err| match err {
                            dto::Error::PathNotFound(path) if path == "/DCIM" => Problem::NoDcim,
                            other => Problem::Failed(error::to_error_response(other)),
                        });
                let _ = output.send(Message::Previewed { preview, result }).await;
            }),
            |message| message,
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // Changes what happens after the copy, not what would be copied.
            Message::Request(Request::RemoveAfter(on)) => {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.remove = on;
                }
                Task::none()
            }
            Message::Request(Request::Folders(folders)) => {
                let Some(dialog) = self.dialog.as_mut() else {
                    return Task::none();
                };
                if dialog.folders == folders {
                    return Task::none();
                }
                dialog.folders = folders;
                // What is already in the destination depends on where each
                // file would go.
                self.look()
            }
            // `app.rs` queues the import; closing is all that is left here.
            Message::Request(Request::Import | Request::Close) => {
                self.close();
                Task::none()
            }
            Message::Seen { preview, count } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.preview_id == preview
                    && let Preview::Looking { seen } = &mut dialog.preview
                {
                    *seen = count;
                }
                Task::none()
            }
            Message::Previewed { preview, result } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.preview_id == preview
                {
                    dialog.preview = match result {
                        Ok(preview) => Preview::Ready(preview),
                        Err(problem) => Preview::Failed(problem),
                    };
                }
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(dialog) = &self.dialog else {
            return space().into();
        };
        let send = |request| Message::Request(request);
        let status: Element<'_, Message> = match &dialog.preview {
            Preview::Looking { seen } => text(mapper::looking(*seen)).into(),
            Preview::Ready(preview) => text(mapper::preview_line(preview)).into(),
            Preview::Failed(Problem::NoDcim) => text(mapper::no_dcim(&dialog.storage)).into(),
            Preview::Failed(Problem::Failed(error)) => column![
                text(&error.title).style(text::danger),
                text(&error.hint).size(12).style(text::secondary),
            ]
            .spacing(2)
            .into(),
        };
        let choice = |label: &'static str, folders: dto::ImportFolders| {
            iced::widget::radio(label, folders, Some(dialog.folders), move |folders| {
                send(Request::Folders(folders))
            })
            .text_size(14)
        };
        let mut by_date = column![
            choice(t("import.choice_none"), dto::ImportFolders::None),
            choice(t("import.choice_day"), dto::ImportFolders::Day),
            choice(t("import.choice_month"), dto::ImportFolders::Month),
            choice(t("import.choice_year_month"), dto::ImportFolders::YearMonth),
        ]
        .spacing(6);
        if dialog.folders != dto::ImportFolders::None {
            by_date = by_date.push(text(t("import.undated")).size(12).style(text::secondary));
        }
        let content = column![
            text(t("import.title")).size(20),
            text(tf("import.from", &[("storage", &dialog.storage)]))
                .size(13)
                .style(text::secondary),
            rule::horizontal(1),
            row![
                text(t("import.into")).size(13).style(text::secondary),
                text(&dialog.shown_destination).size(14),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(t("import.somewhere_else"))
                .size(12)
                .style(text::secondary),
            by_date,
            column![
                checkbox(dialog.remove)
                    .label(t("import.remove_after"))
                    .text_size(14)
                    .on_toggle(move |on| send(Request::RemoveAfter(on))),
                text(mapper::removing_note(dialog.remove))
                    .size(12)
                    .style(text::secondary),
            ]
            .spacing(4),
            rule::horizontal(1),
            status,
            text(t("import.left_out")).size(12).style(text::secondary),
            row![
                space::horizontal(),
                button(text(t("label.cancel")))
                    .style(button::secondary)
                    .on_press(send(Request::Close)),
                button(text(t("label.import")))
                    .style(button::primary)
                    .on_press_maybe(self.request().map(|_| send(Request::Import))),
            ]
            .spacing(8),
        ]
        .spacing(12);
        container(content)
            .padding(24)
            .width(Length::Fixed(480.0))
            .style(container::rounded_box)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeImport, drain};

    fn preview(new_files: usize, already: usize) -> dto::ImportPreview {
        dto::ImportPreview {
            new_files,
            new_bytes: new_files as u64 * 1_000_000,
            already,
        }
    }

    /// A dialog opened on "Internal" into ~/Pictures, its preview run to the end.
    async fn opened(service: FakeImport, by_date: bool) -> (Handler, Arc<FakeImport>) {
        let folders = if by_date {
            dto::ImportFolders::Day
        } else {
            dto::ImportFolders::None
        };
        let service = Arc::new(service);
        let mut handler = Handler::new(service.clone());
        let task = handler.open(
            5,
            "Internal",
            "/Users/me/Pictures",
            "/Users/me",
            folders,
            false,
        );
        settle(&mut handler, task).await;
        (handler, service)
    }

    async fn settle(handler: &mut Handler, task: Task<Message>) {
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    fn shows(handler: &Handler, words: &str) -> bool {
        iced_test::simulator(handler.view()).find(words).is_ok()
    }

    #[tokio::test]
    async fn a_change_of_language_words_the_open_dialog_and_a_failed_look_again() {
        use internal::dto::Language;

        let (mut handler, _) = opened(FakeImport::answering(Ok(preview(312, 4_898))), false).await;
        assert!(shows(&handler, "Import new photos and videos"));

        crate::i18n::set_language(Language::Thai);
        handler.language_changed();

        for key in [
            "import.title",
            "import.into",
            "import.remove_after",
            "label.cancel",
            "label.import",
        ] {
            assert!(shows(&handler, crate::i18n::t(key)), "{key}");
        }
        assert!(!shows(&handler, "Import new photos and videos"));
        assert_eq!(
            handler.request().map(|r| r.storage_id),
            Some(5),
            "the choice stays"
        );

        let (mut failed, _) = opened(FakeImport::answering(Err(dto::Error::Timeout)), false).await;
        let english = error::to_error_response(dto::Error::Timeout).title;
        crate::i18n::set_language(Language::English);
        assert!(shows(&failed, &english));
        crate::i18n::set_language(Language::Japanese);
        failed.language_changed();
        let said = error::to_error_response(dto::Error::Timeout).title;
        assert_ne!(said, english);
        assert!(shows(&failed, &said));
    }

    #[tokio::test]
    async fn freeing_the_phone_is_a_choice_that_changes_no_preview() {
        let (mut handler, service) = opened(FakeImport::answering(Ok(preview(3, 0))), false).await;
        assert_eq!(handler.request().map(|r| r.remove_from_phone), Some(false));
        assert!(shows(&handler, "Everything stays on the phone."));

        let task = handler.update(Message::Request(Request::RemoveAfter(true)));
        settle(&mut handler, task).await;

        assert_eq!(handler.request().map(|r| r.remove_from_phone), Some(true));
        assert_eq!(
            service.asked.lock().unwrap().len(),
            1,
            "nothing to look at again"
        );
        assert!(shows(
            &handler,
            "Only photos and videos whose copies are checked on the Mac are removed, one by one; folders stay."
        ));
    }

    #[tokio::test]
    async fn opening_looks_at_what_is_new_and_offers_to_import_it() {
        let (handler, service) =
            opened(FakeImport::answering(Ok(preview(312, 4_898))), false).await;

        assert!(handler.is_open());
        assert_eq!(
            *service.asked.lock().unwrap(),
            [dto::ImportRequest {
                storage_id: 5,
                destination: "/Users/me/Pictures".into(),
                folders: dto::ImportFolders::None,
                remove_from_phone: false,
            }]
        );
        assert!(shows(
            &handler,
            "312 photos and videos to import (312.0 MB) · 4,898 already on this Mac"
        ));
        assert!(shows(&handler, "~/Pictures"));
        assert_eq!(handler.request().map(|r| r.storage_id), Some(5));
    }

    #[test]
    fn while_looking_it_counts_and_import_waits() {
        let mut handler = Handler::new(Arc::new(FakeImport::default()));
        let _ = handler.open(
            5,
            "Internal",
            "/Users/me",
            "/Users/me",
            dto::ImportFolders::None,
            false,
        );
        let preview = handler.previews;

        let _ = handler.update(Message::Seen {
            preview,
            count: 1_200,
        });
        let _ = handler.update(Message::Seen {
            preview: preview + 7,
            count: 3,
        });

        assert!(shows(
            &handler,
            "Looking through DCIM… 1,200 photos and videos so far"
        ));
        assert!(shows(&handler, "~"));
        assert_eq!(
            handler.request(),
            None,
            "nothing to import until it is known"
        );
    }

    #[tokio::test]
    async fn choosing_a_folder_per_day_looks_again_and_drops_the_earlier_look() {
        let (mut handler, service) = opened(FakeImport::default(), false).await;
        let earlier = handler.previews;

        let task = handler.update(Message::Request(Request::Folders(dto::ImportFolders::Day)));
        assert!(shows(
            &handler,
            "Files without a date go straight into the folder."
        ));
        let _ = handler.update(Message::Previewed {
            preview: earlier,
            result: Ok(preview(99, 0)),
        });
        assert!(
            handler.request().is_none(),
            "the earlier answer is not this one"
        );
        settle(&mut handler, task).await;

        let asked: Vec<dto::ImportFolders> = service
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.folders)
            .collect();
        assert_eq!(asked, [dto::ImportFolders::None, dto::ImportFolders::Day]);
        assert_eq!(
            handler.request().map(|r| r.folders),
            Some(dto::ImportFolders::Day)
        );
        let _ = handler.update(Message::Request(Request::Folders(dto::ImportFolders::Day)));
        assert_eq!(service.asked.lock().unwrap().len(), 2, "no change, no look");
    }

    #[tokio::test]
    async fn a_folder_a_month_or_a_year_and_month_each_look_again() {
        let (mut handler, service) = opened(FakeImport::default(), false).await;
        // A radio draws its own label, which `find` can't see: each choice is
        // the message its radio sends.

        for folders in [dto::ImportFolders::Month, dto::ImportFolders::YearMonth] {
            let task = handler.update(Message::Request(Request::Folders(folders)));
            settle(&mut handler, task).await;
            assert_eq!(handler.request().map(|r| r.folders), Some(folders));
        }
        let asked: Vec<dto::ImportFolders> = service
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.folders)
            .collect();
        assert_eq!(
            asked,
            [
                dto::ImportFolders::None,
                dto::ImportFolders::Month,
                dto::ImportFolders::YearMonth
            ]
        );
    }

    #[tokio::test]
    async fn nothing_new_means_nothing_to_import() {
        let (handler, _) = opened(FakeImport::answering(Ok(preview(0, 4_898))), true).await;

        assert!(shows(
            &handler,
            "Nothing new: all 4,898 photos and videos are on this Mac already."
        ));
        assert_eq!(handler.request(), None);
    }

    #[tokio::test]
    async fn a_storage_without_dcim_says_there_is_nothing_from_the_camera() {
        let (handler, _) = opened(
            FakeImport::answering(Err(dto::Error::PathNotFound("/DCIM".into()))),
            false,
        )
        .await;

        assert!(shows(
            &handler,
            "Internal has no DCIM folder, so there is nothing from the camera to import."
        ));
        assert_eq!(handler.request(), None);
    }

    #[tokio::test]
    async fn a_look_that_fails_says_why() {
        let (handler, _) =
            opened(FakeImport::answering(Err(dto::Error::Disconnected)), false).await;

        let lost = error::to_error_response(dto::Error::Disconnected);
        assert!(shows(&handler, &lost.title));
        assert_eq!(handler.request(), None);
    }

    #[tokio::test]
    async fn closing_stops_the_look_and_the_dialog_goes() {
        let mut handler = Handler::new(Arc::new(FakeImport::default()));
        let _ = handler.open(
            5,
            "Internal",
            "/Users/me",
            "/Users/me",
            dto::ImportFolders::None,
            false,
        );
        let cancel = handler.dialog.as_ref().unwrap().cancel.clone();

        let _ = handler.update(Message::Request(Request::Close));

        assert!(cancel.is_cancelled());
        assert!(!handler.is_open());
        assert_eq!(handler.request(), None);
        let _ = handler.view();
        let _ = handler.update(Message::Request(Request::Folders(dto::ImportFolders::Day)));
        let _ = handler.update(Message::Seen {
            preview: 1,
            count: 1,
        });
        let _ = handler.update(Message::Previewed {
            preview: 1,
            result: Err(Problem::NoDcim),
        });
        assert!(!handler.is_open());
    }

    #[tokio::test]
    async fn import_closes_the_dialog_for_the_app_to_queue_it() {
        let (mut handler, _) = opened(FakeImport::default(), false).await;

        let _ = handler.update(Message::Request(Request::Import));

        assert!(!handler.is_open());
    }
}
