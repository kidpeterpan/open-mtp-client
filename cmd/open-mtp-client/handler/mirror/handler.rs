//! The mirror dialog: the phone pane's folder and the Mac pane's, which way
//! to mirror, whether to remove what is only in the target, and what that
//! would do, worked out before anything is touched. `app.rs` queues the
//! mirror itself as a transfer.

use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::widget::{button, checkbox, column, container, radio, row, rule, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto::{self, MirrorDirection};

use super::mapper;
use super::request::Request;
use crate::handler::error::{self, ErrorResponse};

/// Told how many items of the phone folder have been read so far.
pub type Seen = Box<dyn Fn(usize) + Send + Sync>;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// What a mirror would do now, without touching anything.
    async fn preview(
        &self,
        request: dto::MirrorRequest,
        cancel: dto::Cancel,
        seen: Seen,
    ) -> Result<dto::MirrorPreview, dto::Error>;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Seen {
        preview: u64,
        count: usize,
    },
    Previewed {
        preview: u64,
        result: Result<dto::MirrorPreview, ErrorResponse>,
    },
}

enum Preview {
    Comparing { seen: usize },
    Ready(dto::MirrorPreview),
    Failed(ErrorResponse),
}

struct Dialog {
    storage_id: u64,
    storage: String,
    phone_folder: String,
    mac_folder: String,
    shown_mac: String,
    direction: MirrorDirection,
    remove_extras: bool,
    /// Removing what is only on the Mac is not offered for the home folder
    /// or the disk itself: a mirror target is a folder made for it.
    may_remove_on_mac: bool,
    preview: Preview,
    preview_id: u64,
    cancel: dto::Cancel,
    /// The phone's serial, when it has one: only then can the mirror be
    /// saved, since a saved mirror is shown under its phone.
    serial: Option<String>,
    /// This mirror, as it stands, is saved.
    saved: bool,
}

impl Dialog {
    fn request(&self) -> dto::MirrorRequest {
        dto::MirrorRequest {
            storage_id: self.storage_id,
            phone_folder: self.phone_folder.clone(),
            mac_folder: self.mac_folder.clone(),
            direction: self.direction,
            remove_extras: self.remove_extras && self.may_remove(),
        }
    }

    fn may_remove(&self) -> bool {
        self.direction == MirrorDirection::ToPhone || self.may_remove_on_mac
    }
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

    /// Opens the dialog for the phone folder `phone_folder` on `storage` and
    /// the Mac folder `mac_folder` (shown from `home`), and starts comparing.
    pub fn open(
        &mut self,
        storage_id: u64,
        storage: &str,
        phone_folder: &str,
        mac_folder: &str,
        home: &str,
    ) -> Task<Message> {
        self.open_as(
            storage_id,
            storage,
            phone_folder,
            mac_folder,
            home,
            (MirrorDirection::ToMac, false),
        )
    }

    /// Opens the dialog for a saved mirror, as it was saved, on the storage
    /// `storage_id` of its phone now, and starts comparing.
    pub fn open_saved(
        &mut self,
        storage_id: u64,
        mirror: &dto::SavedMirror,
        home: &str,
    ) -> Task<Message> {
        let task = self.open_as(
            storage_id,
            &mirror.phone.storage_name,
            &mirror.phone_folder,
            &mirror.mac_folder,
            home,
            (mirror.direction, mirror.remove_extras),
        );
        self.offer_saving(Some(mirror.phone.serial.clone()));
        task
    }

    /// The phone's serial, for saving; none: the mirror can't be saved.
    pub fn offer_saving(&mut self, serial: Option<String>) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.serial = serial.filter(|serial| !serial.is_empty());
        }
    }

    /// Whether the mirror as it stands is saved (the app knows the list).
    pub fn set_saved(&mut self, saved: bool) {
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.saved = saved;
        }
    }

    /// The mirror as it stands, to be saved; `None` without a serial.
    pub fn to_save(&self) -> Option<dto::SavedMirror> {
        let dialog = self.dialog.as_ref()?;
        Some(dto::SavedMirror {
            phone: dto::PinPhone {
                serial: dialog.serial.clone()?,
                storage_id: dialog.storage_id,
                storage_name: dialog.storage.clone(),
            },
            phone_folder: dialog.phone_folder.clone(),
            mac_folder: dialog.mac_folder.clone(),
            direction: dialog.direction,
            remove_extras: dialog.request().remove_extras,
        })
    }

    fn open_as(
        &mut self,
        storage_id: u64,
        storage: &str,
        phone_folder: &str,
        mac_folder: &str,
        home: &str,
        (direction, remove_extras): (MirrorDirection, bool),
    ) -> Task<Message> {
        self.close();
        self.dialog = Some(Dialog {
            storage_id,
            storage: storage.to_owned(),
            phone_folder: phone_folder.to_owned(),
            mac_folder: mac_folder.to_owned(),
            shown_mac: mapper::mac_line(mac_folder, home),
            direction,
            remove_extras,
            may_remove_on_mac: mac_folder != home && mac_folder != "/",
            preview: Preview::Comparing { seen: 0 },
            preview_id: 0,
            cancel: dto::Cancel::new(),
            serial: None,
            saved: false,
        });
        self.compare()
    }

    pub fn close(&mut self) {
        if let Some(dialog) = self.dialog.take() {
            dialog.cancel.cancel();
        }
    }

    /// The mirror as the dialog stands, if it has something to do.
    pub fn request(&self) -> Option<dto::MirrorRequest> {
        let dialog = self.dialog.as_ref()?;
        let Preview::Ready(preview) = &dialog.preview else {
            return None;
        };
        let request = dialog.request();
        let removes = request.remove_extras && preview.extras > 0;
        (preview.new_files + preview.changed_files > 0 || removes).then_some(request)
    }

    fn compare(&mut self) -> Task<Message> {
        self.previews += 1;
        let preview = self.previews;
        let Some(dialog) = self.dialog.as_mut() else {
            return Task::none();
        };
        dialog.cancel.cancel();
        dialog.cancel = dto::Cancel::new();
        dialog.preview_id = preview;
        dialog.preview = Preview::Comparing { seen: 0 };
        let request = dialog.request();
        let (service, cancel) = (Arc::clone(&self.service), dialog.cancel.clone());
        Task::run(
            iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
                let counted = Mutex::new(output.clone());
                let seen: Seen = Box::new(move |count| {
                    if let Ok(mut sender) = counted.lock() {
                        let _ = sender.try_send(Message::Seen { preview, count });
                    }
                });
                let result = service
                    .preview(request, cancel, seen)
                    .await
                    .map_err(error::to_error_response);
                let _ = output.send(Message::Previewed { preview, result }).await;
            }),
            |message| message,
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(Request::Direction(direction)) => {
                let Some(dialog) = self.dialog.as_mut() else {
                    return Task::none();
                };
                if dialog.direction == direction {
                    return Task::none();
                }
                dialog.direction = direction;
                // What goes where, and what is extra, is the other way round.
                self.compare()
            }
            Message::Request(Request::RemoveExtras(on)) => {
                // What is copied doesn't change, only what is said about the
                // rest: no need to compare again.
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.remove_extras = on;
                }
                Task::none()
            }
            // `app.rs` keeps it, then says it is saved.
            Message::Request(Request::Save) => Task::none(),
            Message::Request(Request::Start | Request::Close) => {
                self.close();
                Task::none()
            }
            Message::Seen { preview, count } => {
                if let Some(dialog) = self.dialog.as_mut()
                    && dialog.preview_id == preview
                    && let Preview::Comparing { seen } = &mut dialog.preview
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
                        Err(error) => Preview::Failed(error),
                    };
                }
                Task::none()
            }
        }
    }

    /// "Save This Mirror", or "Saved" once it is; nothing for a phone
    /// without a serial (a saved mirror is shown under its phone).
    fn save_button<'a>(&self, dialog: &'a Dialog) -> Element<'a, Message> {
        if dialog.serial.is_none() {
            return space().into();
        }
        if dialog.saved {
            return text("Saved in the sidebar")
                .size(13)
                .style(text::secondary)
                .into();
        }
        button(text("Save This Mirror"))
            .style(button::text)
            .on_press(Message::Request(Request::Save))
            .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(dialog) = &self.dialog else {
            return space().into();
        };
        let send = |request| Message::Request(request);
        let direction = |label, value| {
            radio(label, value, Some(dialog.direction), |d| {
                send(Request::Direction(d))
            })
            .size(16)
            .text_size(14)
        };
        let target = mapper::target_name(dialog.direction);
        let side = |label, path: String| {
            row![
                text(label)
                    .size(13)
                    .style(text::secondary)
                    .width(Length::Fixed(56.0)),
                text(path).size(14),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        };

        let mut extras = column![].spacing(4);
        if dialog.may_remove() {
            extras = extras.push(
                checkbox(dialog.remove_extras)
                    .label(match dialog.direction {
                        MirrorDirection::ToMac => {
                            "Also remove what is only on the Mac (to the Trash)"
                        }
                        MirrorDirection::ToPhone => {
                            "Also remove what is only on the phone (deleted for good)"
                        }
                    })
                    .text_size(14)
                    .on_toggle(move |on| send(Request::RemoveExtras(on))),
            );
        } else {
            extras = extras.push(
                text("What is only on the Mac is kept: removing it is not offered for your home folder or the disk itself. Open a folder made for the mirror on the Mac side.")
                    .size(12)
                    .style(text::secondary),
            );
        }

        let status: Element<'_, Message> = match &dialog.preview {
            Preview::Comparing { seen } => text(mapper::comparing(*seen)).into(),
            Preview::Ready(preview) => {
                let mut lines = column(
                    mapper::preview_lines(
                        preview,
                        dialog.direction,
                        dialog.request().remove_extras,
                    )
                    .into_iter()
                    .map(|line| text(line).into()),
                )
                .spacing(4);
                let samples = &preview.samples;
                for (kind, sample, total) in [
                    ("New", &samples.new, preview.new_files),
                    ("Changed", &samples.changed, preview.changed_files),
                    ("Only there", &samples.extras, preview.extras),
                    ("Left alone", &samples.conflicts, preview.conflicts),
                ] {
                    if let Some(line) = mapper::sample_line(sample, total) {
                        lines = lines.push(
                            text(format!("{kind}: {line}"))
                                .size(12)
                                .style(text::secondary),
                        );
                    }
                }
                lines.into()
            }
            Preview::Failed(error) => column![
                text(&error.title).style(text::danger),
                text(&error.hint).size(12).style(text::secondary),
            ]
            .spacing(2)
            .into(),
        };

        let content = column![
            text("Mirror a folder").size(20),
            text(format!(
                "Makes {target}'s folder hold what the other holds: what is new is copied, what changed is replaced."
            ))
            .size(13)
            .style(text::secondary),
            rule::horizontal(1),
            side("Phone", format!("{} › {}", dialog.storage, dialog.phone_folder)),
            side("Mac", dialog.shown_mac.clone()),
            text("To mirror other folders, open them in the two panes first.")
                .size(12)
                .style(text::secondary),
            row![
                direction("Phone → Mac", MirrorDirection::ToMac),
                direction("Mac → Phone", MirrorDirection::ToPhone),
            ]
            .spacing(16),
            extras,
            rule::horizontal(1),
            status,
            text(match dialog.direction {
                MirrorDirection::ToMac => "Hidden files are left alone on both sides. A replaced file's old version goes to the Trash.",
                MirrorDirection::ToPhone => "Hidden files are left alone on both sides. A changed file on the phone is deleted and sent again.",
            })
            .size(12)
            .style(text::secondary),
            row![
                self.save_button(dialog),
                space::horizontal(),
                button(text("Cancel"))
                    .style(button::secondary)
                    .on_press(send(Request::Close)),
                button(text("Mirror"))
                    .style(button::primary)
                    .on_press_maybe(self.request().map(|_| send(Request::Start))),
            ]
            .spacing(8),
        ]
        .spacing(12);
        container(content)
            .padding(24)
            .width(Length::Fixed(560.0))
            .style(container::rounded_box)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakeMirror, drain};

    fn preview(new_files: usize, changed_files: usize, extras: usize) -> dto::MirrorPreview {
        dto::MirrorPreview {
            new_files,
            new_bytes: new_files as u64 * 1_000_000,
            changed_files,
            changed_bytes: changed_files as u64 * 1_000_000,
            extras,
            conflicts: 0,
            samples: dto::MirrorSamples::default(),
        }
    }

    async fn settle(handler: &mut Handler, task: Task<Message>) {
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    /// A dialog on Internal › /Backup and ~/Backup, its comparison done.
    async fn opened(service: FakeMirror) -> (Handler, Arc<FakeMirror>) {
        opened_at(service, "/Users/me/Backup").await
    }

    async fn opened_at(service: FakeMirror, mac: &str) -> (Handler, Arc<FakeMirror>) {
        let service = Arc::new(service);
        let mut handler = Handler::new(service.clone());
        let task = handler.open(5, "Internal", "/Backup", mac, "/Users/me");
        settle(&mut handler, task).await;
        (handler, service)
    }

    fn shows(handler: &Handler, words: &str) -> bool {
        iced_test::simulator(handler.view()).find(words).is_ok()
    }

    #[tokio::test]
    async fn opening_compares_the_two_panes_folders_phone_to_mac_keeping_extras() {
        let (handler, service) = opened(FakeMirror::default()).await;

        assert_eq!(
            *service.asked.lock().unwrap(),
            [dto::MirrorRequest {
                storage_id: 5,
                phone_folder: "/Backup".into(),
                mac_folder: "/Users/me/Backup".into(),
                direction: MirrorDirection::ToMac,
                remove_extras: false,
            }]
        );
        assert!(shows(&handler, "Internal › /Backup"));
        assert!(shows(&handler, "~/Backup"));
        assert!(shows(&handler, "2 files to copy (2.0 KB)"));
        assert!(shows(&handler, "3 items only on the Mac: kept"));
        assert!(shows(&handler, "New: a.jpg, b.jpg"));
        assert!(shows(&handler, "Only there: old and 2 more"));
        assert!(handler.request().is_some());
    }

    #[tokio::test]
    async fn the_other_way_round_compares_again_and_drops_the_earlier_answer() {
        let (mut handler, service) = opened(FakeMirror::default()).await;
        let earlier = handler.previews;

        let task = handler.update(Message::Request(Request::Direction(
            MirrorDirection::ToPhone,
        )));
        let _ = handler.update(Message::Previewed {
            preview: earlier,
            result: Ok(preview(9, 9, 9)),
        });
        assert!(handler.request().is_none(), "still comparing the other way");
        settle(&mut handler, task).await;

        let ways: Vec<MirrorDirection> = service
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.direction)
            .collect();
        assert_eq!(ways, [MirrorDirection::ToMac, MirrorDirection::ToPhone]);
        assert!(shows(
            &handler,
            "Hidden files are left alone on both sides. A changed file on the phone is deleted and sent again."
        ));
        let _ = handler.update(Message::Request(Request::Direction(
            MirrorDirection::ToPhone,
        )));
        assert_eq!(
            service.asked.lock().unwrap().len(),
            2,
            "no change, no comparison"
        );
    }

    #[tokio::test]
    async fn removing_extras_says_where_they_go_without_comparing_again() {
        let (mut handler, service) = opened(FakeMirror::default()).await;

        let _ = handler.update(Message::Request(Request::RemoveExtras(true)));

        assert_eq!(service.asked.lock().unwrap().len(), 1);
        assert!(shows(&handler, "3 items only on the Mac: to the Trash"));
        assert_eq!(handler.request().map(|r| r.remove_extras), Some(true));

        let task = handler.update(Message::Request(Request::Direction(
            MirrorDirection::ToPhone,
        )));
        settle(&mut handler, task).await;
        assert!(shows(
            &handler,
            "3 items only on the phone: deleted from the phone for good"
        ));
    }

    #[tokio::test]
    async fn removing_from_the_home_folder_itself_is_not_offered() {
        let (mut handler, _) = opened_at(FakeMirror::default(), "/Users/me").await;

        let _ = handler.update(Message::Request(Request::RemoveExtras(true)));

        assert_eq!(handler.request().map(|r| r.remove_extras), Some(false));
        assert!(shows(&handler, "3 items only on the Mac: kept"));
        assert!(!shows(
            &handler,
            "Also remove what is only on the Mac (to the Trash)"
        ));
        // Onto the phone, the phone's extras may still go.
        let task = handler.update(Message::Request(Request::Direction(
            MirrorDirection::ToPhone,
        )));
        settle(&mut handler, task).await;
        assert_eq!(handler.request().map(|r| r.remove_extras), Some(true));
    }

    #[tokio::test]
    async fn with_nothing_to_do_there_is_nothing_to_start() {
        let (mut handler, _) = opened(FakeMirror::answering(Ok(preview(0, 0, 2)))).await;

        assert!(shows(
            &handler,
            "Nothing to copy: the Mac has all of it already."
        ));
        assert!(handler.request().is_none());
        let _ = handler.update(Message::Request(Request::RemoveExtras(true)));
        assert!(
            handler.request().is_some(),
            "removing the extras is something to do"
        );
    }

    #[tokio::test]
    async fn a_comparison_that_fails_says_why_and_one_under_way_counts() {
        let (failed, _) = opened(FakeMirror::answering(Err(dto::Error::PathNotFound(
            "/Backup".into(),
        ))))
        .await;
        let gone = error::to_error_response(dto::Error::PathNotFound("/Backup".into()));
        assert!(shows(&failed, &gone.title));
        assert!(failed.request().is_none());

        let mut handler = Handler::new(Arc::new(FakeMirror::default()));
        let _ = handler.open(5, "Internal", "/Backup", "/Users/me/B", "/Users/me");
        let preview = handler.previews;
        let _ = handler.update(Message::Seen {
            preview,
            count: 1_200,
        });
        assert!(shows(
            &handler,
            "Comparing the folders… 1,200 items read on the phone"
        ));
    }

    #[tokio::test]
    async fn closing_or_starting_stops_the_comparison_and_the_dialog_goes() {
        let mut handler = Handler::new(Arc::new(FakeMirror::default()));
        let _ = handler.open(5, "Internal", "/Backup", "/Users/me/B", "/Users/me");
        let cancel = handler.dialog.as_ref().unwrap().cancel.clone();

        let _ = handler.update(Message::Request(Request::Close));

        assert!(cancel.is_cancelled());
        assert!(!handler.is_open());
        let _ = handler.view();
        let _ = handler.update(Message::Request(Request::Direction(
            MirrorDirection::ToPhone,
        )));
        let _ = handler.update(Message::Request(Request::RemoveExtras(true)));
        let _ = handler.update(Message::Seen {
            preview: 1,
            count: 1,
        });
        let _ = handler.update(Message::Previewed {
            preview: 1,
            result: Ok(preview(1, 0, 0)),
        });
        assert!(handler.request().is_none());

        let (mut started, _) = opened(FakeMirror::default()).await;
        let _ = started.update(Message::Request(Request::Start));
        assert!(!started.is_open());
    }

    #[tokio::test]
    async fn removing_extras_is_a_click_away() {
        let (mut handler, _) = opened(FakeMirror::default()).await;

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let at = ui
                .find("Also remove what is only on the Mac (to the Trash)")
                .unwrap()
                .bounds()
                .center();
            ui.point_at(at);
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert!(handler.dialog.as_ref().unwrap().remove_extras);
    }

    fn camera() -> dto::SavedMirror {
        dto::SavedMirror {
            phone: dto::PinPhone {
                serial: "ABC".into(),
                storage_id: 65537,
                storage_name: "Internal".into(),
            },
            phone_folder: "/DCIM/Camera".into(),
            mac_folder: "/Users/me/Phone photos".into(),
            direction: MirrorDirection::ToPhone,
            remove_extras: true,
        }
    }

    #[tokio::test]
    async fn a_saved_mirror_opens_as_it_was_saved_on_its_storage_now() {
        let service = Arc::new(FakeMirror::default());
        let mut handler = Handler::new(service.clone());

        let task = handler.open_saved(7, &camera(), "/Users/me");
        settle(&mut handler, task).await;

        assert_eq!(
            *service.asked.lock().unwrap(),
            [dto::MirrorRequest {
                storage_id: 7,
                phone_folder: "/DCIM/Camera".into(),
                mac_folder: "/Users/me/Phone photos".into(),
                direction: MirrorDirection::ToPhone,
                remove_extras: true,
            }],
            "compared the way it was saved, removing what it said"
        );
        assert_eq!(
            handler.to_save(),
            Some(dto::SavedMirror {
                phone: dto::PinPhone {
                    storage_id: 7,
                    ..camera().phone
                },
                ..camera()
            })
        );
        assert!(handler.request().is_some(), "Mirror runs it once compared");
    }

    #[test]
    fn saving_is_offered_only_for_a_phone_with_a_serial_and_says_once_it_is_saved() {
        let mut handler = Handler::new(Arc::new(FakeMirror::default()));
        let _ = handler.open(1, "Internal", "/DCIM", "/Users/me/Backup", "/Users/me");
        assert_eq!(handler.to_save(), None, "no serial told");
        assert!(
            iced_test::simulator(handler.view())
                .find("Save This Mirror")
                .is_err()
        );

        handler.offer_saving(Some(String::new()));
        assert_eq!(handler.to_save(), None, "an empty serial is none");
        handler.offer_saving(Some("ABC".into()));
        let mut ui = iced_test::simulator(handler.view());
        let _ = ui.click("Save This Mirror").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::Request(Request::Save)]
        ));
        for message in messages {
            let _ = handler.update(message);
        }
        assert!(handler.is_open(), "saving leaves the dialog open");

        handler.set_saved(true);
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Saved in the sidebar").is_ok());
        assert!(ui.find("Save This Mirror").is_err());
        drop(ui);
        handler.close();
        handler.set_saved(true);
        handler.offer_saving(Some("ABC".into()));
        assert_eq!(handler.to_save(), None, "no dialog");
    }
}
