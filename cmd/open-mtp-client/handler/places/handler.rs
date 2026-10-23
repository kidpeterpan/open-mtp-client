//! The sidebar's places: the Mac's own folders and mounted disks, and the
//! folders pinned on either side. `app.rs` takes the user to them.

use std::sync::Arc;

use iced::widget::{button, column, progress_bar, row, rule, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use crate::handler::drop_spot::{SidebarDrop, drop_spot};
use crate::handler::error;
use crate::handler::guard;
use crate::i18n::{english_only, t, tf};

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// The Mac's own folders and mounted disks (reads the disk).
    async fn places(&self) -> Vec<dto::Place>;
    fn pins(&self) -> Vec<dto::Pin>;
    fn save_pins(&self, pins: &[dto::Pin]) -> Result<(), dto::Error>;
    fn mirrors(&self) -> Vec<dto::SavedMirror>;
    fn save_mirrors(&self, mirrors: &[dto::SavedMirror]) -> Result<(), dto::Error>;
}

/// Where a place or pin goes, and where a drag dropped on it lands.
pub use crate::handler::drop_spot::Spot as Target;

#[derive(Debug, Clone)]
pub enum Message {
    /// The Mac's places as they are now.
    Places(Vec<dto::Place>),
    /// Look at the mounted disks again.
    Refresh,
    Go(Target),
    Unpin(usize),
    /// Open the mirror dialog for the saved mirror at this place in the
    /// whole list, on this storage of the open phone (`app.rs` does it).
    OpenMirror {
        at: usize,
        storage_id: u64,
    },
    /// Forget the saved mirror at this place in the whole list.
    ForgetMirror(usize),
    Dismiss,
    /// A drag came onto (`true`) or left a place or pin (`app.rs` keeps it).
    DropHover(Target, bool),
}

/// The phone open now, as the sidebar needs it: its serial number and its
/// storages (id, name).
pub struct OpenPhone<'a> {
    pub serial: &'a str,
    pub storages: Vec<(u64, String)>,
}

pub struct Handler {
    service: Arc<dyn Service>,
    places: Vec<dto::Place>,
    pins: Vec<dto::Pin>,
    mirrors: Vec<dto::SavedMirror>,
    /// Why a pin could not be made or kept.
    notice: Option<String>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        let pins = service.pins();
        let mirrors = service.mirrors();
        Self {
            service,
            places: Vec::new(),
            pins,
            mirrors,
            notice: None,
        }
    }

    pub fn language_changed(&mut self) {
        self.notice = None;
    }

    /// The saved mirror at `at` in the whole list.
    pub fn mirror(&self, at: usize) -> Option<&dto::SavedMirror> {
        self.mirrors.get(at)
    }

    /// Whether this mirror is saved, removing just what it says.
    pub fn is_saved(&self, mirror: &dto::SavedMirror) -> bool {
        self.mirrors.iter().any(|kept| kept == mirror)
    }

    /// Keeps a mirror to run again; the same one saved again takes the new
    /// choice of what it removes.
    pub fn save_mirror(&mut self, mirror: dto::SavedMirror) {
        self.notice = None;
        if let Some(kept) = self
            .mirrors
            .iter_mut()
            .find(|kept| kept.same_mirror(&mirror))
        {
            *kept = mirror;
        } else if self.mirrors.len() >= dto::MOST_MIRRORS {
            self.notice = Some(tf(
                "places.mirrors_full",
                &[("count", &dto::MOST_MIRRORS.to_string())],
            ));
            return;
        } else {
            self.mirrors.push(mirror);
        }
        self.save_the_mirrors();
    }

    fn save_the_mirrors(&mut self) {
        if let Err(err) = self.service.save_mirrors(&self.mirrors) {
            self.notice = Some(error::to_error_response(err).title);
        }
    }

    /// Reads the Mac's places, off the UI thread.
    pub fn refresh(&self) -> Task<Message> {
        let service = Arc::clone(&self.service);
        // A panic leaves what is shown as it is.
        let shown = self.places.clone();
        guard::perform(
            async move { service.places().await },
            move || shown,
            Message::Places,
        )
    }

    /// Pins a folder; one already pinned is left as it is.
    pub fn pin(&mut self, pin: dto::Pin) {
        self.notice = None;
        if self.pins.iter().any(|kept| kept.same_place(&pin)) {
            return;
        }
        if self.pins.len() >= dto::MOST_PINS {
            self.notice = Some(tf(
                "places.pins_full",
                &[("count", &dto::MOST_PINS.to_string())],
            ));
            return;
        }
        self.pins.push(pin);
        self.save();
    }

    /// Says why a pin was not made, or not gone to.
    pub fn refuse(&mut self, why: String) {
        self.notice = Some(why);
    }

    fn save(&mut self) {
        // Kept here even if it can't be written down; said so.
        if let Err(err) = self.service.save_pins(&self.pins) {
            self.notice = Some(error::to_error_response(err).title);
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Places(places) => {
                if places != self.places {
                    self.places = places;
                }
                Task::none()
            }
            Message::Refresh => self.refresh(),
            // `app.rs` goes there.
            Message::Go(_) => Task::none(),
            Message::OpenMirror { .. } | Message::DropHover(..) => Task::none(),
            Message::ForgetMirror(at) => {
                if at < self.mirrors.len() {
                    self.mirrors.remove(at);
                    self.notice = None;
                    self.save_the_mirrors();
                }
                Task::none()
            }
            Message::Unpin(at) => {
                if at < self.pins.len() {
                    self.pins.remove(at);
                    self.notice = None;
                    self.save();
                }
                Task::none()
            }
            Message::Dismiss => {
                self.notice = None;
                Task::none()
            }
        }
    }

    /// Places and Pinned, for the sidebar. A phone pin shows while its phone
    /// is the one open and its storage is there; `mac_path` marks the place
    /// the Mac pane shows. While a drag is on (`drop`), each place and pin
    /// it can be dropped on says when the pointer is over it.
    pub fn view(
        &self,
        phone: Option<&OpenPhone<'_>>,
        mac_path: Option<&str>,
        drop: Option<&SidebarDrop>,
    ) -> Element<'_, Message> {
        let heading = |title| text(title).size(13).style(text::secondary);
        let entry = |label: String,
                     tag: Option<&'static str>,
                     space: Option<&dto::DiskSpace>,
                     current: bool,
                     target: Target| {
            let mut line = row![text(label).size(14).wrapping(text::Wrapping::None)]
                .spacing(8)
                .align_y(Alignment::Center);
            if let Some(tag) = tag {
                line = line.push(text(tag).size(11).style(text::secondary));
            }
            let mut content = column![line].spacing(4);
            // A disk shows how full it is, as the phone's storages do.
            if let Some(space) = space {
                let (used, words) = mapper::disk_usage(space);
                content = content
                    .push(progress_bar(0.0..=1.0, used).girth(3))
                    .push(text(words).size(11));
            }
            let go = button(content)
                .width(Length::Fill)
                .padding([4, 10])
                .style(if current {
                    button::secondary
                } else {
                    button::subtle
                })
                .on_press(Message::Go(target.clone()));
            drop_spot(go, target, drop, Message::DropHover)
        };

        let mut list = column![
            rule::horizontal(1),
            heading(t("places.title")),
            column(self.places.iter().map(|place| {
                entry(
                    mapper::place_label(place),
                    place.volume.then_some(t("places.tag.disk")),
                    place.space.as_ref(),
                    mac_path == Some(place.path.as_str()),
                    Target::Mac(place.path.clone()),
                )
            }))
            .spacing(2),
        ]
        .spacing(10);

        let shown: Vec<(usize, &dto::Pin, Target)> = self
            .pins
            .iter()
            .enumerate()
            .filter_map(|(at, pin)| {
                let target = match (&pin.phone, phone) {
                    (None, _) => Target::Mac(pin.path.clone()),
                    (Some(on), Some(open)) if on.serial == open.serial => Target::Phone {
                        storage_id: mapper::storage_of(on, &open.storages)?,
                        path: pin.path.clone(),
                    },
                    _ => return None,
                };
                Some((at, pin, target))
            })
            .collect();
        if !shown.is_empty() || self.notice.is_some() {
            let both = shown.iter().any(|(_, pin, _)| pin.phone.is_some())
                && shown.iter().any(|(_, pin, _)| pin.phone.is_none());
            let rows = shown.into_iter().map(|(at, pin, target)| {
                let tag = both.then_some(if pin.phone.is_some() {
                    t("places.tag.phone")
                } else {
                    english_only("Mac")
                });
                row![
                    entry(pin.label.clone(), tag, None, false, target),
                    button(text("×").size(13))
                        .style(button::text)
                        .padding([4, 6])
                        .on_press(Message::Unpin(at)),
                ]
                .align_y(Alignment::Center)
                .into()
            });
            list = list
                .push(rule::horizontal(1))
                .push(heading(t("places.pinned")))
                .push(column(rows).spacing(2));
        }
        // The open phone's saved mirrors, on storages it has now: a click
        // opens the mirror dialog, which compares first; nothing runs here.
        let mirrors: Vec<Element<'_, Message>> = phone
            .map(|open| {
                self.mirrors
                    .iter()
                    .enumerate()
                    .filter(|(_, mirror)| mirror.phone.serial == open.serial)
                    .filter_map(|(at, mirror)| {
                        let storage_id = mapper::storage_of(&mirror.phone, &open.storages)?;
                        let tag = match mirror.direction {
                            dto::MirrorDirection::ToMac => t("places.tag.to_mac"),
                            dto::MirrorDirection::ToPhone => t("places.tag.to_phone"),
                        };
                        let open_it = button(
                            row![
                                text(mapper::mirror_label(mirror))
                                    .size(14)
                                    .wrapping(text::Wrapping::None),
                                text(tag)
                                    .size(11)
                                    .style(text::secondary)
                                    .wrapping(text::Wrapping::None),
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        )
                        .width(Length::Fill)
                        .padding([4, 10])
                        .style(button::subtle)
                        .on_press(Message::OpenMirror { at, storage_id });
                        Some(
                            row![
                                open_it,
                                button(text("×").size(13))
                                    .style(button::text)
                                    .padding([4, 6])
                                    .on_press(Message::ForgetMirror(at)),
                            ]
                            .align_y(Alignment::Center)
                            .into(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !mirrors.is_empty() {
            list = list
                .push(rule::horizontal(1))
                .push(heading(t("places.mirrors")))
                .push(column(mirrors).spacing(2));
        }
        if let Some(notice) = &self.notice {
            list = list.push(
                row![
                    text(notice)
                        .size(12)
                        .style(text::secondary)
                        .width(Length::Fill),
                    button(text("×").size(12))
                        .style(button::text)
                        .on_press(Message::Dismiss),
                ]
                .align_y(Alignment::Center),
            );
        }
        list.push(space().height(4)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakePlaces, drain};

    fn mac_pin(path: &str, label: &str) -> dto::Pin {
        dto::Pin {
            side: dto::PinSide::Mac,
            path: path.into(),
            label: label.into(),
            phone: None,
        }
    }

    fn phone_pin(
        path: &str,
        label: &str,
        serial: &str,
        storage_id: u64,
        storage: &str,
    ) -> dto::Pin {
        dto::Pin {
            side: dto::PinSide::Phone,
            path: path.into(),
            label: label.into(),
            phone: Some(dto::PinPhone {
                serial: serial.into(),
                storage_id,
                storage_name: storage.into(),
            }),
        }
    }

    async fn loaded(service: FakePlaces) -> (Handler, Arc<FakePlaces>) {
        let service = Arc::new(service);
        let mut handler = Handler::new(service.clone());
        for message in drain(handler.refresh()).await.0 {
            let _ = handler.update(message);
        }
        (handler, service)
    }

    fn shows(element: Element<'_, Message>, words: &str) -> bool {
        iced_test::simulator(element).find(words).is_ok()
    }

    #[tokio::test]
    async fn the_macs_places_are_listed_with_disks_marked() {
        let (handler, _) = loaded(FakePlaces::default()).await;

        assert!(shows(handler.view(None, Some("/Users/me"), None), "Home"));
        assert!(shows(handler.view(None, None, None), "Downloads"));
        assert!(shows(handler.view(None, None, None), "disk"));
        assert!(
            !shows(handler.view(None, None, None), "Pinned"),
            "nothing pinned yet"
        );
    }

    #[tokio::test]
    async fn the_four_fixed_places_are_named_in_the_language_by_their_path_and_a_disk_keeps_its_name()
     {
        use internal::dto::Language;

        let mut service = FakePlaces::default();
        service.places.push(dto::Place {
            label: "Downloads".into(),
            path: "/Volumes/Downloads".into(),
            volume: true,
            space: None,
        });
        let (handler, _) = loaded(service).await;

        for language in Language::ALL {
            crate::i18n::with_language(language, || {
                let view = || handler.view(None, None, None);
                assert!(shows(view(), crate::i18n::t("places.home")), "{language:?}");
                assert!(
                    shows(view(), crate::i18n::t("places.downloads")),
                    "{language:?}"
                );
                assert!(
                    shows(view(), crate::i18n::t("places.title")),
                    "{language:?}"
                );
                assert!(
                    shows(view(), "Backup"),
                    "a disk is called what it is called"
                );
                assert!(
                    shows(view(), "Downloads"),
                    "a disk that is called Downloads still is"
                );
            });
        }
        crate::i18n::with_language(Language::Thai, || {
            assert!(!shows(handler.view(None, None, None), "Places"));
        });
    }

    #[tokio::test]
    async fn a_change_of_language_clears_the_notice_and_keeps_pins_and_mirrors() {
        let (mut handler, _) = loaded(FakePlaces::default()).await;
        handler.pin(mac_pin("/Users/me/Music", "Music"));
        handler.refuse("one thing and another".into());
        assert!(shows(
            handler.view(None, None, None),
            "one thing and another"
        ));

        crate::i18n::set_language(dto::Language::Japanese);
        handler.language_changed();

        assert!(!shows(
            handler.view(None, None, None),
            "one thing and another"
        ));
        assert!(shows(handler.view(None, None, None), "Music"));
        assert_eq!(handler.pins.len(), 1);
    }

    #[test]
    fn a_full_list_of_pins_says_so_in_the_language_in_use() {
        let mut handler = Handler::new(Arc::new(FakePlaces::default()));
        for at in 0..dto::MOST_PINS {
            handler.pin(mac_pin(&format!("/Users/me/{at}"), "x"));
        }
        crate::i18n::set_language(dto::Language::ChineseSimplified);

        handler.pin(mac_pin("/Users/me/one-more", "x"));

        assert_eq!(
            handler.notice.as_deref(),
            Some(
                crate::i18n::tf(
                    "places.pins_full",
                    &[("count", &dto::MOST_PINS.to_string())]
                )
                .as_str()
            )
        );
    }

    /// Moves the pointer onto the words `on` and then well below everything.
    fn pass_over(element: Element<'_, Message>, on: &str) -> Vec<Message> {
        use iced::{Event, Point, mouse};
        let mut ui = iced_test::simulator(element);
        let at = ui.find(on).unwrap().bounds().center();
        let away = Point::new(at.x, at.y + 5_000.0);
        for point in [at, away] {
            ui.point_at(point);
            ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: point })]);
        }
        ui.into_messages().collect()
    }

    #[tokio::test]
    async fn while_a_drag_is_on_a_place_says_when_the_pointer_comes_and_goes() {
        use crate::handler::drop_spot::{Origin, SidebarDrop};
        let (mut handler, _) = loaded(FakePlaces::default()).await;
        handler.pin(mac_pin("/Users/me/Work", "Work"));
        let drop = SidebarDrop {
            over: None,
            from: Origin::Phone {
                storage_id: 1,
                folder: "/DCIM".into(),
            },
        };

        let home = Target::Mac("/Users/me".into());
        assert!(matches!(
            pass_over(handler.view(None, None, Some(&drop)), "Home").as_slice(),
            [Message::DropHover(a, true), Message::DropHover(b, false)] if *a == home && *b == home
        ));
        let work = Target::Mac("/Users/me/Work".into());
        assert!(
            matches!(
                pass_over(handler.view(None, None, Some(&drop)), "Work").as_slice(),
                [Message::DropHover(a, true), Message::DropHover(b, false)] if *a == work && *b == work
            ),
            "a pin too"
        );
        assert!(
            pass_over(handler.view(None, None, None), "Home").is_empty(),
            "no drag, no word"
        );
        let over_home = SidebarDrop {
            over: Some(home),
            ..drop
        };
        let _ = handler.update(Message::DropHover(Target::Mac("/".into()), true));
        assert!(shows(handler.view(None, None, Some(&over_home)), "Home"));
    }

    #[tokio::test]
    async fn a_disk_shows_how_full_it_is_and_a_folder_does_not() {
        let (handler, _) = loaded(FakePlaces::default()).await;

        assert!(shows(
            handler.view(None, None, None),
            "312.0 GB free of 1.0 TB"
        ));
        let mut ui = iced_test::simulator(handler.view(None, None, None));
        let messages: Vec<Message> = {
            let _ = ui.click("Backup").unwrap();
            ui.into_messages().collect()
        };
        assert!(
            matches!(messages.as_slice(), [Message::Go(Target::Mac(path))] if path == "/Volumes/Backup"),
            "still a place to go to"
        );
        let no_room = Handler {
            places: vec![dto::Place {
                label: "Network".into(),
                path: "/Volumes/Network".into(),
                volume: true,
                space: None,
            }],
            ..handler
        };
        assert!(
            !shows(no_room.view(None, None, None), "free of"),
            "nothing guessed"
        );
    }

    #[tokio::test]
    async fn a_folder_is_pinned_once_and_unpinned() {
        let (mut handler, service) = loaded(FakePlaces::default()).await;

        handler.pin(mac_pin("/Users/me/Work", "Work"));
        handler.pin(mac_pin("/Users/me/Work", "Work again"));
        assert_eq!(handler.pins.len(), 1, "the same folder once");
        assert!(shows(handler.view(None, None, None), "Work"));
        let _ = handler.update(Message::Unpin(0));
        let _ = handler.update(Message::Unpin(7));

        assert!(handler.pins.is_empty());
        let saved = service.saved.lock().unwrap();
        assert_eq!(saved.len(), 2, "saved when pinned and when unpinned");
        assert!(saved[1].is_empty());
    }

    #[tokio::test]
    async fn fifty_pins_at_most_and_a_save_that_fails_says_so() {
        let (mut handler, _) = loaded(FakePlaces::default()).await;
        for n in 0..dto::MOST_PINS {
            handler.pin(mac_pin(&format!("/f{n}"), "f"));
        }
        handler.pin(mac_pin("/f0", "f"));
        assert!(handler.notice.is_none(), "one already pinned is no new pin");
        handler.pin(mac_pin("/one more", "x"));
        assert_eq!(handler.pins.len(), dto::MOST_PINS);
        assert!(shows(
            handler.view(None, None, None),
            "50 folders are pinned already: unpin one first."
        ));
        let _ = handler.update(Message::Dismiss);
        assert!(handler.notice.is_none());

        let (mut failing, _) = loaded(FakePlaces {
            save_fails: true,
            ..FakePlaces::default()
        })
        .await;
        failing.pin(mac_pin("/a", "a"));
        assert_eq!(failing.pins.len(), 1, "kept for now");
        assert!(failing.notice.is_some());
    }

    #[tokio::test]
    async fn a_phones_pins_show_only_while_it_is_open_and_its_storage_is_found() {
        let (handler, _) = loaded(FakePlaces {
            pins: std::sync::Mutex::new(vec![
                mac_pin("/Users/me/Work", "Work"),
                phone_pin("/DCIM/Camera", "Camera", "ABC123", 9, "Internal"),
                phone_pin("/Music", "Music", "OTHER", 1, "Internal"),
            ]),
            ..FakePlaces::default()
        })
        .await;
        let open = OpenPhone {
            serial: "ABC123",
            storages: vec![(65537, "Internal".to_owned())],
        };

        let without = handler.view(None, None, None);
        assert!(!shows(without, "Camera"));
        assert!(
            shows(handler.view(Some(&open), None, None), "Camera"),
            "found by name"
        );
        assert!(
            shows(handler.view(Some(&open), None, None), "phone"),
            "both sides: tagged"
        );
        assert!(
            !shows(handler.view(Some(&open), None, None), "Music"),
            "another phone's"
        );

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view(Some(&open), None, None));
            let at = ui.find("Camera").unwrap().bounds().center();
            ui.point_at(at);
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };
        assert!(
            matches!(
                clicked.as_slice(),
                [Message::Go(Target::Phone { storage_id: 65537, path })] if path == "/DCIM/Camera"
            ),
            "{clicked:?}"
        );
    }

    #[tokio::test]
    async fn the_cross_unpins_its_own_pin_when_another_phones_is_hidden_ahead() {
        let (mut handler, service) = loaded(FakePlaces {
            pins: std::sync::Mutex::new(vec![
                phone_pin("/Music", "Music", "OTHER", 1, "Internal"),
                mac_pin("/Users/me/Work", "Work"),
            ]),
            ..FakePlaces::default()
        })
        .await;
        let open = OpenPhone {
            serial: "ABC123",
            storages: vec![(65537, "Internal".to_owned())],
        };

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view(Some(&open), None, None));
            let at = ui.find("×").unwrap().bounds().center();
            ui.point_at(at);
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert_eq!(
            handler.pins,
            vec![phone_pin("/Music", "Music", "OTHER", 1, "Internal")],
            "Work went; the other phone's pin stayed"
        );
        assert_eq!(service.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_read_of_the_places_that_panics_keeps_the_ones_shown() {
        let (mut handler, _) = loaded(FakePlaces::default()).await;
        let before = handler.places.clone();
        assert!(!before.is_empty());
        handler.service = Arc::new(FakePlaces {
            panics: true,
            ..FakePlaces::default()
        });

        let refresh = handler.update(Message::Refresh);
        for message in drain(refresh).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(handler.places, before, "the sidebar is not blanked");
    }

    #[tokio::test]
    async fn the_same_places_are_not_stored_again_and_going_is_the_apps() {
        let (mut handler, _) = loaded(FakePlaces::default()).await;
        let before = handler.places.clone();

        let _ = handler.update(Message::Places(before.clone()));
        let refresh = handler.update(Message::Refresh);
        let go = handler.update(Message::Go(Target::Mac("/".into())));

        assert_eq!(handler.places, before);
        assert!(refresh.units() > 0);
        assert_eq!(go.units(), 0);
    }

    fn saved(phone_folder: &str, mac_folder: &str, serial: &str) -> dto::SavedMirror {
        dto::SavedMirror {
            phone: dto::PinPhone {
                serial: serial.into(),
                storage_id: 65537,
                storage_name: "Internal".into(),
            },
            phone_folder: phone_folder.into(),
            mac_folder: mac_folder.into(),
            direction: dto::MirrorDirection::ToMac,
            remove_extras: false,
        }
    }

    #[test]
    fn saved_mirrors_show_under_their_phone_and_open_on_a_click() {
        let fake = Arc::new(FakePlaces::default());
        *fake.mirrors.lock().unwrap() = vec![
            saved("/DCIM/Camera", "/Users/me/Phone photos", "ABC"),
            saved("/Music", "/Users/me/Music", "OTHER"),
            dto::SavedMirror {
                direction: dto::MirrorDirection::ToPhone,
                ..saved("/Podcasts", "/Users/me/Podcasts", "ABC")
            },
        ];
        let handler = Handler::new(fake);
        let open = OpenPhone {
            serial: "ABC",
            // The SD card came back with another id: found by its name.
            storages: vec![(7, "Internal".into())],
        };

        let mut ui = iced_test::simulator(handler.view(Some(&open), None, None));
        assert!(ui.find("Mirrors").is_ok());
        assert!(ui.find("Music → Music").is_err(), "another phone's");
        assert!(ui.find("Podcasts → Podcasts").is_ok());
        let _ = ui.click("Camera → Phone photos").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::OpenMirror {
                at: 0,
                storage_id: 7
            }]
        ));

        let ui = iced_test::simulator(handler.view(None, None, None));
        drop(ui);
        let mut ui = iced_test::simulator(handler.view(None, None, None));
        assert!(ui.find("Mirrors").is_err(), "no phone open: none shown");
    }

    #[test]
    fn a_mirror_saved_again_takes_its_new_choice_and_one_can_be_forgotten() {
        let fake = Arc::new(FakePlaces::default());
        let mut handler = Handler::new(fake.clone());
        let camera = saved("/DCIM/Camera", "/Users/me/Phone photos", "ABC");

        handler.save_mirror(camera.clone());
        assert!(handler.is_saved(&camera));
        let removing = dto::SavedMirror {
            remove_extras: true,
            ..camera.clone()
        };
        assert!(!handler.is_saved(&removing), "saved removing nothing");
        handler.save_mirror(removing.clone());
        assert_eq!(
            handler.mirrors,
            std::slice::from_ref(&removing),
            "replaced, not added"
        );
        assert_eq!(handler.mirror(0), Some(&removing));
        assert_eq!(fake.mirrors_saved.lock().unwrap().len(), 2);

        let _ = handler.update(Message::ForgetMirror(5));
        assert_eq!(handler.mirrors.len(), 1, "no such one");
        let _ = handler.update(Message::ForgetMirror(0));
        assert!(handler.mirrors.is_empty());
        assert_eq!(fake.mirrors_saved.lock().unwrap().last().unwrap().len(), 0);
        let _ = handler.update(Message::OpenMirror {
            at: 0,
            storage_id: 1,
        });
    }

    #[test]
    fn too_many_mirrors_or_a_file_that_cannot_be_written_says_so() {
        let fake = Arc::new(FakePlaces::default());
        let mut handler = Handler::new(fake);
        for n in 0..dto::MOST_MIRRORS {
            handler.save_mirror(saved(&format!("/f{n}"), "/m", "ABC"));
        }
        handler.save_mirror(saved("/one more", "/m", "ABC"));
        assert_eq!(handler.mirrors.len(), dto::MOST_MIRRORS);
        assert!(
            handler
                .notice
                .as_deref()
                .unwrap()
                .contains("forget one first")
        );

        let failing = Arc::new(FakePlaces {
            save_fails: true,
            ..FakePlaces::default()
        });
        let mut handler = Handler::new(failing);
        handler.save_mirror(saved("/DCIM", "/m", "ABC"));
        assert!(handler.notice.is_some());
        assert_eq!(handler.mirrors.len(), 1, "kept here even so");
    }

    #[test]
    fn a_saved_mirror_is_named_from_where_to_where() {
        assert_eq!(
            mapper::mirror_label(&saved("/DCIM/Camera", "/Users/me/Phone photos", "A")),
            "Camera → Phone photos"
        );
        let to_phone = dto::SavedMirror {
            direction: dto::MirrorDirection::ToPhone,
            ..saved("/", "/Users/me/Music", "A")
        };
        assert_eq!(mapper::mirror_label(&to_phone), "Music → Internal");
    }
}
