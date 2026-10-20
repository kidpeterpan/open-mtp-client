use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::widget::{button, column, container, row, rule, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::Request;
use crate::handler::error::{self, ErrorResponse};
use crate::i18n::t;

/// Where Get Info's items are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Phone {
        storage_id: u64,
        storage: String,
        /// What the phone says it can do, shown for a folder on it.
        abilities: Option<dto::Abilities>,
    },
    Mac,
}

/// What Get Info is about: the selected `items` in `folder`, or, with none,
/// the folder itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    pub place: Place,
    pub folder: String,
    pub items: Vec<dto::Entry>,
}

/// Counts what is under a folder on either side.
#[async_trait::async_trait]
pub trait Service: Send + Sync {
    async fn tally(
        &self,
        place: &Place,
        path: &str,
        cancel: dto::Cancel,
        report: Box<dyn Fn(dto::Tally) + Send + Sync>,
    ) -> Result<dto::Tally, dto::Error>;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    /// Everything counted so far by count `run`, the selection included.
    Counted {
        run: u64,
        tally: dto::Tally,
    },
    /// Count `run` ended.
    Finished {
        run: u64,
        result: Result<dto::Tally, ErrorResponse>,
    },
}

/// The Get Info dialog (⌘I): what the selection is, and for folders what is
/// in them, counted on demand and shown as it goes.
pub struct Handler {
    service: Arc<dyn Service>,
    shown: Option<Shown>,
    /// Which count is the current one: a stopped count's late word is dropped.
    run: u64,
    cancel: Option<dto::Cancel>,
}

struct Shown {
    subject: Subject,
    /// The selection itself plus everything counted so far.
    tally: dto::Tally,
    counting: bool,
    error: Option<ErrorResponse>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            shown: None,
            run: 0,
            cancel: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.shown.is_some()
    }

    /// Whether the dialog is about the phone (so it goes with the phone).
    pub fn is_about_the_phone(&self) -> bool {
        self.shown
            .as_ref()
            .is_some_and(|shown| matches!(shown.subject.place, Place::Phone { .. }))
    }

    /// Shows `subject` and starts counting what is in its folders.
    pub fn open(&mut self, subject: Subject) -> Task<Message> {
        self.close();
        self.run += 1;
        let folders = mapper::folders_to_count(&subject);
        self.shown = Some(Shown {
            tally: mapper::base(&subject.items),
            counting: !folders.is_empty(),
            error: None,
            subject: subject.clone(),
        });
        if folders.is_empty() {
            return Task::none();
        }
        let cancel = dto::Cancel::new();
        self.cancel = Some(cancel.clone());
        let base = mapper::base(&subject.items);
        Task::run(
            count(
                Arc::clone(&self.service),
                subject.place,
                folders,
                base,
                cancel,
                self.run,
            ),
            |message| message,
        )
    }

    pub fn language_changed(&mut self) {
        if let Some(shown) = &mut self.shown
            && let Some(error) = &shown.error
        {
            shown.error = Some(error.reworded());
        }
    }

    /// Closes the dialog and stops its count.
    pub fn close(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.shown = None;
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // `app.rs` finds the files; the dialog goes.
            Message::Request(Request::Close | Request::ShowKind(_)) => self.close(),
            Message::Counted { run, tally } if run == self.run => {
                if let Some(shown) = &mut self.shown {
                    shown.tally = tally;
                }
            }
            Message::Finished { run, result } if run == self.run => {
                self.cancel = None;
                if let Some(shown) = &mut self.shown {
                    shown.counting = false;
                    match result {
                        Ok(tally) => shown.tally = tally,
                        Err(error) => shown.error = Some(error),
                    }
                }
            }
            // A count that was stopped or replaced.
            Message::Counted { .. } | Message::Finished { .. } => {}
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(shown) = &self.shown else {
            return space().into();
        };
        let today = chrono::Local::now().date_naive();
        let info = mapper::to_info_response(
            &shown.subject,
            &shown.tally,
            shown.counting,
            shown.error.as_ref(),
            today,
        );

        let mut rows = column![].spacing(6);
        for (label, value) in info.rows {
            rows = rows.push(
                row![
                    text(label)
                        .size(13)
                        .style(text::secondary)
                        .width(Length::Fixed(80.0))
                        .align_x(Alignment::End),
                    text(value).size(13).width(Length::Fill),
                ]
                .spacing(12),
            );
        }
        let mut content = column![
            row![
                text(info.title).size(20).width(Length::Fill),
                button(text(t("label.done")))
                    .style(button::primary)
                    .on_press(Message::Request(Request::Close)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            rule::horizontal(1),
            rows,
        ]
        .spacing(12);
        if !info.kinds.is_empty() {
            content = content.push(kinds_view(&info.kinds));
        }
        for note in info.notes {
            content = content.push(text(note).size(12).style(text::secondary));
        }

        container(content)
            .padding(24)
            .width(Length::Fixed(440.0))
            .style(container::rounded_box)
            .into()
    }
}

/// Counts each folder in turn, telling the running total (the selection plus
/// what is counted so far) as it goes, and ending with `Finished`.
fn count(
    service: Arc<dyn Service>,
    place: Place,
    folders: Vec<String>,
    base: dto::Tally,
    cancel: dto::Cancel,
    run: u64,
) -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(16, async move |mut output: mpsc::Sender<Message>| {
        let mut done = base;
        for folder in folders {
            let so_far = done;
            let sender = Mutex::new(output.clone());
            // Best effort: when the UI is behind, the next count replaces it.
            let report = Box::new(move |tally: dto::Tally| {
                let mut total = so_far;
                total.add(&tally);
                if let Ok(mut sender) = sender.lock() {
                    let _ = sender.try_send(Message::Counted { run, tally: total });
                }
            });
            match service.tally(&place, &folder, cancel.clone(), report).await {
                Ok(tally) => done.add(&tally),
                Err(err) => {
                    let _ = output
                        .send(Message::Finished {
                            run,
                            result: Err(error::to_error_response(err)),
                        })
                        .await;
                    return;
                }
            }
        }
        let _ = output
            .send(Message::Finished {
                run,
                result: Ok(done),
            })
            .await;
    })
}

/// What fills it: a bar split by kind, and each kind's bytes under it, a
/// click on one finding those files.
fn kinds_view(kinds: &[super::response::KindShare]) -> Element<'static, Message> {
    let bar = row(kinds.iter().map(|share| {
        let colour = kind_colour(share.kind);
        container(space())
            .width(Length::FillPortion(share.portion))
            .height(10)
            .style(move |_| container::background(colour))
            .into()
    }))
    .spacing(1)
    .width(Length::Fill);
    let words = iced::widget::Row::with_children(kinds.iter().map(|share| {
        let label = row![
            container(space()).width(8).height(8).style({
                let colour = kind_colour(share.kind);
                move |_| container::background(colour)
            }),
            text(share.label.clone()).size(12),
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        button(label)
            .style(button::text)
            .padding([2, 4])
            .on_press_maybe(
                share
                    .findable
                    .then_some(Message::Request(Request::ShowKind(share.kind))),
            )
            .into()
    }))
    .spacing(4)
    .wrap();
    column![bar, words].spacing(6).into()
}

/// Each kind's colour in the bar, as the pane's badges colour them.
fn kind_colour(kind: dto::SpaceKind) -> iced::Color {
    match kind {
        dto::SpaceKind::Photos => iced::Color::from_rgb8(0x4c, 0xaf, 0x50),
        dto::SpaceKind::Videos => iced::Color::from_rgb8(0x8e, 0x44, 0xad),
        dto::SpaceKind::Music => iced::Color::from_rgb8(0xe9, 0x1e, 0x63),
        dto::SpaceKind::Documents => iced::Color::from_rgb8(0x29, 0x80, 0xb9),
        dto::SpaceKind::Archives => iced::Color::from_rgb8(0xf3, 0x9c, 0x12),
        dto::SpaceKind::Apps => iced::Color::from_rgb8(0x16, 0xa0, 0x85),
        dto::SpaceKind::Other => iced::Color::from_rgb8(0x95, 0xa5, 0xa6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeInfo, drain, file, folder};

    fn phone_subject(items: Vec<dto::Entry>) -> Subject {
        Subject {
            place: Place::Phone {
                storage_id: 5,
                storage: "Internal".into(),
                abilities: None,
            },
            folder: "/DCIM".into(),
            items,
        }
    }

    fn shown_tally(handler: &Handler) -> dto::Tally {
        handler.shown.as_ref().unwrap().tally
    }

    fn says(handler: &Handler, wanted: &str) -> bool {
        iced_test::simulator(handler.view())
            .find(wanted.to_owned())
            .is_ok()
    }

    #[tokio::test]
    async fn an_open_count_is_said_again_in_the_new_language_from_what_was_counted() {
        let mut counted = dto::Tally::default();
        counted.add_file("clip.mp4", Some(4_000));
        counted.add_file("IMG_1.jpg", Some(1_000));
        let mut handler = Handler::new(Arc::new(FakeInfo::counting(counted)));
        let task = handler.open(phone_subject(Vec::new()));
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
        assert!(says(&handler, "Videos 4.0 KB"));
        assert!(says(&handler, "Folder"));

        crate::i18n::set_language(dto::Language::Japanese);
        handler.language_changed();

        let videos = crate::i18n::tf(
            "info.kind_share",
            &[
                ("kind", crate::i18n::t("label.kind.videos")),
                ("size", "4.0 KB"),
            ],
        );
        assert!(says(&handler, &videos));
        assert!(!says(&handler, "Videos 4.0 KB"));
        assert!(says(&handler, crate::i18n::t("explorer.kind.folder")));
        assert!(says(&handler, crate::i18n::t("label.done")));
        assert_eq!(shown_tally(&handler).files, 2, "what was counted is kept");
    }

    #[test]
    fn a_count_that_failed_says_why_again_in_the_new_language() {
        let mut handler = Handler::new(Arc::new(FakeInfo::failing(dto::Error::Timeout)));
        let _ = handler.open(phone_subject(vec![folder("Camera")]));
        let run = handler.run;
        let _ = handler.update(Message::Finished {
            run,
            result: Err(error::to_error_response(dto::Error::Timeout)),
        });
        let english = handler.shown.as_ref().unwrap().error.clone().unwrap().title;

        crate::i18n::set_language(dto::Language::Thai);
        handler.language_changed();

        let said = handler.shown.as_ref().unwrap().error.clone().unwrap().title;
        assert_ne!(said, english);
        assert_eq!(said, error::to_error_response(dto::Error::Timeout).title);
    }

    #[tokio::test]
    async fn what_fills_a_folder_shows_by_kind_and_a_click_finds_those_files() {
        let mut counted = dto::Tally::default();
        counted.add_file("clip.mp4", Some(4_000));
        counted.add_file("IMG_1.jpg", Some(1_000));
        counted.add_file("blob.bin", Some(500));
        let mut handler = Handler::new(Arc::new(FakeInfo::counting(counted)));
        let task = handler.open(phone_subject(Vec::new()));
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }

        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.click("Videos 4.0 KB").is_ok());
        let sent: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(
                sent.as_slice(),
                [Message::Request(Request::ShowKind(dto::SpaceKind::Videos))]
            ),
            "{sent:?}"
        );
        let mut ui = iced_test::simulator(handler.view());
        let _ = ui.click("Other 500 B");
        assert!(
            ui.into_messages().next().is_none(),
            "the rest has no kind to find by"
        );

        let _ = handler.update(Message::Request(Request::ShowKind(dto::SpaceKind::Videos)));
        assert!(!handler.is_open(), "the dialog makes way for the files");
    }

    #[tokio::test]
    async fn the_selected_folders_are_counted_and_added_to_the_selection() {
        let service = Arc::new(FakeInfo::counting(dto::Tally {
            files: 10,
            folders: 2,
            bytes: 1_000,
            ..dto::Tally::default()
        }));
        let mut handler = Handler::new(service.clone());
        let mut small = file("a.jpg");
        small.size_bytes = 5;

        let task = handler.open(phone_subject(vec![
            small,
            folder("Camera"),
            folder("Shots"),
        ]));
        assert!(handler.is_open() && handler.is_about_the_phone());
        assert!(handler.shown.as_ref().unwrap().counting);
        let _ = handler.view();
        let messages = drain(task).await.0;
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Counted { .. })),
            "the count is told as it goes"
        );
        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(
            *service.asked.lock().unwrap(),
            ["/DCIM/Camera".to_owned(), "/DCIM/Shots".to_owned()]
        );
        assert_eq!(
            shown_tally(&handler),
            dto::Tally {
                files: 21,
                folders: 6,
                bytes: 2_005,
                by_kind: [5, 0, 0, 0, 0, 0, 0],
                ..dto::Tally::default()
            },
            "a.jpg, the two folders, and what is in each"
        );
        assert!(!handler.shown.as_ref().unwrap().counting);
        let _ = handler.view();
    }

    #[tokio::test]
    async fn files_alone_need_no_count() {
        let service = Arc::new(FakeInfo::counting(dto::Tally::default()));
        let mut handler = Handler::new(service.clone());

        let task = handler.open(phone_subject(vec![file("a.jpg")]));

        assert!(drain(task).await.0.is_empty());
        assert!(service.asked.lock().unwrap().is_empty());
        assert!(!handler.shown.as_ref().unwrap().counting);
    }

    #[tokio::test]
    async fn a_count_that_fails_says_why() {
        let service = Arc::new(FakeInfo::failing(dto::Error::Disconnected));
        let mut handler = Handler::new(service);

        let task = handler.open(phone_subject(vec![]));
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }

        let shown = handler.shown.as_ref().unwrap();
        assert!(!shown.counting);
        assert!(shown.error.is_some());
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Could not be counted").is_ok());
    }

    #[tokio::test]
    async fn closing_stops_the_count_and_a_stopped_counts_word_is_dropped() {
        let service = Arc::new(FakeInfo::counting(dto::Tally::default()));
        let mut handler = Handler::new(service.clone());
        let first = handler.open(phone_subject(vec![]));
        let first_messages = drain(first).await.0;
        let stale_cancel = handler.cancel.clone().unwrap();

        // A second Get Info before the first's word arrives.
        let _ = handler.open(phone_subject(vec![folder("Camera")]));
        assert!(stale_cancel.is_cancelled(), "the first count was stopped");
        let before = shown_tally(&handler);
        for message in first_messages {
            let _ = handler.update(message);
        }
        assert_eq!(
            shown_tally(&handler),
            before,
            "the first count's word is dropped"
        );
        assert!(handler.shown.as_ref().unwrap().counting);

        let current = handler.cancel.clone().unwrap();
        let _ = handler.update(Message::Request(Request::Close));
        assert!(current.is_cancelled());
        assert!(!handler.is_open());
        let _ = handler.view();
    }

    #[test]
    fn done_closes_the_dialog() {
        let mut handler = Handler::new(Arc::new(FakeInfo::counting(dto::Tally::default())));
        let _ = handler.open(phone_subject(vec![file("a.jpg")]));

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("Done").unwrap();
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert!(!handler.is_open());
    }
}
