use std::sync::Arc;

use iced::widget::{
    button, column, container, radio, responsive, row, rule, scrollable, space, text, toggler,
};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::request::Request;
use crate::handler::error::{self, ErrorResponse};
use crate::i18n::{english_only, t};

pub trait Service: Send + Sync {
    fn load(&self) -> dto::Settings;
    fn save(&self, settings: &dto::Settings) -> Result<(), dto::Error>;
    /// Where the panes were left, kept apart from the settings; by default
    /// nowhere, and nothing kept.
    fn load_folders(&self) -> dto::LastFolders {
        dto::LastFolders::default()
    }

    fn save_folders(&self, _folders: &dto::LastFolders) -> Result<(), dto::Error> {
        Ok(())
    }

    /// The transfers not finished when the app last ended; by default none.
    fn load_transfers(&self) -> Vec<dto::SavedTransfer> {
        Vec::new()
    }

    fn save_transfers(&self, _saved: &[dto::SavedTransfer]) -> Result<(), dto::Error> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
}

/// The app's preferences and the dialog that edits them. Every change is
/// applied at once and saved; there is no OK button to forget.
pub struct Handler {
    service: Arc<dyn Service>,
    settings: dto::Settings,
    /// Where the panes were left, to open there next time.
    folders: dto::LastFolders,
    open: bool,
    save_error: Option<ErrorResponse>,
    /// The unfinished transfers as last written: what the app found at its
    /// start, then whatever it last kept.
    transfers: Vec<dto::SavedTransfer>,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        let settings = service.load();
        let folders = service.load_folders();
        let transfers = service.load_transfers();
        Self {
            service,
            settings,
            folders,
            open: false,
            save_error: None,
            transfers,
        }
    }

    pub fn current(&self) -> dto::Settings {
        self.settings
    }

    /// Where the panes were left.
    pub fn folders(&self) -> &dto::LastFolders {
        &self.folders
    }

    /// The Mac pane shows `path` now; saved only when that is new.
    pub fn keep_mac_folder(&mut self, path: &str) {
        if self.folders.keep_mac(path) {
            self.save_folders();
        }
    }

    /// Forgets the recent folders (Go › Recent Folders › Clear Menu).
    pub fn clear_recent(&mut self) {
        if self.folders.clear_recent() {
            self.save_folders();
        }
    }

    /// `phone`'s pane shows `path` on its storage `storage_id` now.
    pub fn keep_phone_folder(&mut self, phone: &str, storage_id: u64, path: &str) {
        if self.folders.keep_phone(phone, storage_id, path) {
            self.save_folders();
        }
    }

    /// A failure to save where the panes were costs nothing but that.
    fn save_folders(&self) {
        let _ = self.service.save_folders(&self.folders);
    }

    /// The transfers not finished when the app last ended.
    pub fn saved_transfers(&self) -> &[dto::SavedTransfer] {
        &self.transfers
    }

    /// Keeps the transfers not yet finished, written only when they changed
    /// (a failure to write costs nothing but the next launch's offer).
    pub fn keep_transfers(&mut self, now: Vec<dto::SavedTransfer>) {
        if now != self.transfers {
            let _ = self.service.save_transfers(&now);
            self.transfers = now;
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Changes and saves the settings, e.g. from a toggle in a pane footer.
    pub fn change(&mut self, edit: impl FnOnce(&mut dto::Settings)) {
        let before = self.settings;
        edit(&mut self.settings);
        if self.settings == before {
            return;
        }
        self.save_error = self
            .service
            .save(&self.settings)
            .err()
            .map(error::to_error_response);
    }

    pub fn language_changed(&mut self) {
        self.save_error = self.save_error.as_ref().map(ErrorResponse::reworded);
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let Message::Request(request) = message;
        match request {
            Request::Open => self.open = true,
            Request::Close => self.open = false,
            Request::SetTheme(theme) => self.change(|s| s.theme = theme),
            Request::SetShowHiddenOnMac(on) => self.change(|s| s.show_hidden_on_mac = on),
            Request::SetShowHiddenOnPhone(on) => self.change(|s| s.show_hidden_on_phone = on),
            Request::SetFoldersFirst(on) => self.change(|s| s.folders_first = on),
            Request::SetStatusBar(on) => self.change(|s| s.status_bar = on),
            Request::SetShowMacPane(on) => self.change(|s| s.show_mac_pane = on),
            Request::SetMacPaneOnLeft(on) => self.change(|s| s.mac_pane_on_left = on),
            Request::SetAutoConnect(on) => self.change(|s| s.auto_connect = on),
            Request::SetFinishSound(on) => self.change(|s| s.finish_sound = on),
            Request::SetKeepDeleted(on) => self.change(|s| s.keep_deleted = on),
            Request::SetDensity(density) => self.change(|s| s.list_density = density),
            Request::SetLanguage(choice) => self.change(|s| s.language = choice),
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        responsive(move |size| {
            container(self.dialog(size.height))
                .center(Length::Fill)
                .into()
        })
        .into()
    }

    fn dialog(&self, window_height: f32) -> Element<'_, Message> {
        let settings = &self.settings;
        let send = |request| Message::Request(request);
        let theme_choice = |label, choice| {
            radio(label, choice, Some(settings.theme), |c| {
                send(Request::SetTheme(c))
            })
            .size(16)
            .text_size(14)
        };
        let switch = |label, on, request: fn(bool) -> Request| {
            toggler(on)
                .label(label)
                .text_size(14)
                .on_toggle(move |on| send(request(on)))
        };
        let density = |label, choice| {
            radio(label, choice, Some(settings.list_density), |d| {
                send(Request::SetDensity(d))
            })
            .size(16)
            .text_size(14)
        };
        let heading = |title| text(title).size(13).style(text::secondary);
        let language_list =
            language_choices()
                .into_iter()
                .fold(column![].spacing(6), |list, (choice, label)| {
                    list.push(
                        radio(label, choice, Some(settings.language), |c| {
                            send(Request::SetLanguage(c))
                        })
                        .size(16)
                        .text_size(14),
                    )
                });

        let mut layout = column![
            heading(t("settings.heading.layout")),
            switch(
                t("settings.show_mac"),
                settings.show_mac_pane,
                Request::SetShowMacPane
            ),
        ]
        .spacing(10);
        if settings.show_mac_pane {
            layout = layout.push(switch(
                t("settings.mac_left"),
                settings.mac_pane_on_left,
                Request::SetMacPaneOnLeft,
            ));
        }

        let mut content = column![
            row![
                text(t("settings.title")).size(20),
                space::horizontal(),
                button(text(t("label.done")))
                    .style(button::primary)
                    .on_press(send(Request::Close)),
            ]
            .align_y(Alignment::Center),
            rule::horizontal(1),
            heading(t("settings.language.title")),
            language_list,
            rule::horizontal(1),
            heading(t("settings.heading.appearance")),
            row![
                theme_choice(t("settings.theme.auto"), dto::ThemeChoice::Auto),
                theme_choice(t("settings.theme.light"), dto::ThemeChoice::Light),
                theme_choice(t("settings.theme.dark"), dto::ThemeChoice::Dark),
            ]
            .spacing(16),
            rule::horizontal(1),
            heading(t("settings.heading.files")),
            switch(
                t("settings.show_hidden_mac"),
                settings.show_hidden_on_mac,
                Request::SetShowHiddenOnMac,
            ),
            switch(
                t("settings.show_hidden_phone"),
                settings.show_hidden_on_phone,
                Request::SetShowHiddenOnPhone,
            ),
            switch(
                t("settings.folders_first"),
                settings.folders_first,
                Request::SetFoldersFirst
            ),
            switch(
                t("settings.status_bar"),
                settings.status_bar,
                Request::SetStatusBar,
            ),
            row![
                text(t("settings.rows")).size(14),
                density(t("settings.density.compact"), dto::Density::Compact),
                density(t("settings.density.standard"), dto::Density::Standard),
                density(t("settings.density.large"), dto::Density::Large),
            ]
            .spacing(16)
            .align_y(Alignment::Center),
            rule::horizontal(1),
            layout,
            rule::horizontal(1),
            heading(t("settings.heading.phone")),
            switch(
                t("settings.auto_connect"),
                settings.auto_connect,
                Request::SetAutoConnect,
            ),
            switch(
                t("settings.finish_sound"),
                settings.finish_sound,
                Request::SetFinishSound,
            ),
            switch(
                t("settings.keep_deleted"),
                settings.keep_deleted,
                Request::SetKeepDeleted,
            ),
            rule::horizontal(1),
            about(),
        ]
        .spacing(12);
        if let Some(error) = &self.save_error {
            content = content.push(
                column![
                    text(&error.title).size(13).style(text::danger),
                    text(&error.hint).size(12).style(text::secondary),
                ]
                .spacing(2),
            );
        }

        container(scrollable(content.padding([0, 12])))
            .padding(24)
            .width(Length::Fixed(440.0))
            .max_height((window_height - DIALOG_MARGIN).max(DIALOG_LEAST))
            .style(container::rounded_box)
            .into()
    }
}

const DIALOG_MARGIN: f32 = 32.0;
const DIALOG_LEAST: f32 = 200.0;

fn language_choices() -> Vec<(dto::LanguageChoice, String)> {
    let follow_the_mac = (
        dto::LanguageChoice::FollowMac,
        t("settings.language.follow_mac").to_owned(),
    );
    let languages = dto::Language::ALL.into_iter().map(|language| {
        (
            dto::LanguageChoice::Chosen(language),
            language.native_name().to_owned(),
        )
    });
    std::iter::once(follow_the_mac).chain(languages).collect()
}

/// "open-mtp-client 0.1.0", what it is and what it's built on; iced apps have no
/// app menu to hold an About window, so it lives at the foot of Settings.
fn about_lines() -> [String; 3] {
    [
        english_only(format!("open-mtp-client {}", env!("CARGO_PKG_VERSION"))),
        t("settings.about.what").to_owned(),
        t("settings.about.credits").to_owned(),
    ]
}

fn about<'a>() -> Element<'a, Message> {
    let [name, what, credits] = about_lines();
    column![
        text(name).size(14),
        text(what).size(12).style(text::secondary),
        text(credits).size(12).style(text::secondary),
    ]
    .spacing(2)
    .into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn where_the_panes_are_is_saved_only_when_it_changes() {
        let service = Arc::new(crate::handler::testing::FakeSettings::default());
        let mut handler = Handler::new(service.clone());
        let saves = || {
            service
                .folders_saved
                .load(std::sync::atomic::Ordering::SeqCst)
        };

        handler.keep_mac_folder("/Users/me");
        handler.keep_mac_folder("/Users/me");
        handler.keep_phone_folder("pixel", 1, "/DCIM");
        handler.keep_phone_folder("pixel", 1, "/DCIM");

        assert_eq!(saves(), 2);
        assert_eq!(handler.folders().mac.as_deref(), Some("/Users/me"));
        assert_eq!(
            service.folders.lock().unwrap().phone("pixel", 1),
            Some("/DCIM")
        );
        let again = Handler::new(service.clone());
        assert_eq!(
            again.folders().phone("pixel", 1),
            Some("/DCIM"),
            "read back at start"
        );
    }

    use std::sync::Mutex;

    use super::*;

    #[test]
    fn unfinished_transfers_are_read_at_start_and_written_only_when_they_change() {
        use std::sync::atomic::Ordering;
        let kept = vec![dto::SavedTransfer {
            phone: None,
            job: dto::SavedJob::OnMac(dto::PasteRequest {
                mode: dto::Paste::Copy,
                from_storage: 0,
                from: "/a".into(),
                names: vec!["x".into()],
                to_storage: 0,
                to: "/b".into(),
            }),
        }];
        let service = Arc::new(crate::handler::testing::FakeSettings {
            transfers: std::sync::Mutex::new(kept.clone()),
            ..Default::default()
        });
        let mut handler = Handler::new(service.clone());
        assert_eq!(handler.saved_transfers(), kept.as_slice());

        handler.keep_transfers(kept.clone());
        assert_eq!(
            service.transfers_saved.load(Ordering::SeqCst),
            0,
            "unchanged"
        );
        handler.keep_transfers(Vec::new());
        assert_eq!(service.transfers_saved.load(Ordering::SeqCst), 1);
        assert!(service.transfers.lock().unwrap().is_empty());
        assert!(handler.saved_transfers().is_empty());
    }

    /// Settings kept in memory; `fail` makes every save fail.
    #[derive(Default)]
    struct Memory {
        saved: Mutex<Vec<dto::Settings>>,
        fail: bool,
    }

    impl Service for Memory {
        fn load(&self) -> dto::Settings {
            dto::Settings {
                status_bar: false,
                ..dto::Settings::default()
            }
        }

        fn save(&self, settings: &dto::Settings) -> Result<(), dto::Error> {
            if self.fail {
                return Err(dto::Error::General("couldn't save settings: disk".into()));
            }
            self.saved.lock().unwrap().push(*settings);
            Ok(())
        }
    }

    fn handler() -> (Handler, Arc<Memory>) {
        let memory = Arc::new(Memory::default());
        (Handler::new(memory.clone()), memory)
    }

    fn request(handler: &mut Handler, request: Request) {
        let _ = handler.update(Message::Request(request));
    }

    #[test]
    fn starts_from_the_saved_settings() {
        let (handler, _) = handler();

        assert!(!handler.current().status_bar);
        assert!(!handler.is_open());
    }

    #[test]
    fn opens_and_closes() {
        let (mut handler, _) = handler();

        request(&mut handler, Request::Open);
        assert!(handler.is_open());
        request(&mut handler, Request::Close);
        assert!(!handler.is_open());
    }

    #[test]
    fn every_switch_changes_and_saves_its_setting() {
        let (mut handler, memory) = handler();

        request(&mut handler, Request::SetTheme(dto::ThemeChoice::Light));
        request(&mut handler, Request::SetShowHiddenOnMac(true));
        request(&mut handler, Request::SetShowHiddenOnPhone(true));
        request(&mut handler, Request::SetFoldersFirst(false));
        request(&mut handler, Request::SetStatusBar(true));
        request(&mut handler, Request::SetShowMacPane(false));
        request(&mut handler, Request::SetMacPaneOnLeft(false));
        request(&mut handler, Request::SetAutoConnect(false));
        request(&mut handler, Request::SetFinishSound(true));
        request(&mut handler, Request::SetKeepDeleted(true));

        let expected = dto::Settings {
            theme: dto::ThemeChoice::Light,
            show_hidden_on_mac: true,
            show_hidden_on_phone: true,
            folders_first: false,
            status_bar: true,
            show_mac_pane: false,
            mac_pane_on_left: false,
            auto_connect: false,
            finish_sound: true,
            keep_deleted: true,
            ..dto::Settings::default()
        };
        assert_eq!(handler.current(), expected);
        assert_eq!(memory.saved.lock().unwrap().len(), 10);
        assert_eq!(memory.saved.lock().unwrap().last(), Some(&expected));
    }

    #[test]
    fn the_row_size_is_picked_with_a_click_and_saved() {
        let (mut handler, memory) = handler();

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            // A radio draws its own label, which `find` can't see: the
            // first circle is one spacing to the right of "Rows".
            let rows = ui.find("Rows").unwrap().bounds();
            ui.point_at(iced::Point::new(
                rows.x + rows.width + 16.0 + 8.0,
                rows.center_y(),
            ));
            let _ = ui.simulate(iced_test::simulator::click());
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert_eq!(handler.current().list_density, dto::Density::Compact);
        assert_eq!(
            memory.saved.lock().unwrap().last().map(|s| s.list_density),
            Some(dto::Density::Compact)
        );
    }

    #[test]
    fn the_dialog_offers_follow_the_mac_then_each_language_by_its_own_name() {
        let offered = language_choices();

        let labels: Vec<&str> = offered.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(
            labels,
            ["Follow the Mac", "English", "ไทย", "日本語", "简体中文"]
        );
        let choices: Vec<dto::LanguageChoice> = offered.iter().map(|(choice, _)| *choice).collect();
        assert_eq!(
            choices,
            [
                dto::LanguageChoice::FollowMac,
                dto::LanguageChoice::Chosen(dto::Language::English),
                dto::LanguageChoice::Chosen(dto::Language::Thai),
                dto::LanguageChoice::Chosen(dto::Language::Japanese),
                dto::LanguageChoice::Chosen(dto::Language::ChineseSimplified),
            ]
        );
    }

    #[test]
    fn each_language_keeps_its_own_name_whatever_language_the_dialog_is_in() {
        for language in dto::Language::ALL {
            let labels: Vec<String> = crate::i18n::with_language(language, || {
                language_choices()
                    .into_iter()
                    .map(|(_, label)| label)
                    .collect()
            });

            assert_eq!(
                &labels[1..],
                ["English", "ไทย", "日本語", "简体中文"],
                "{language:?}"
            );
        }
    }

    #[test]
    fn choosing_a_language_changes_and_saves_it() {
        let (mut handler, memory) = handler();

        for choice in [
            dto::LanguageChoice::Chosen(dto::Language::Thai),
            dto::LanguageChoice::Chosen(dto::Language::ChineseSimplified),
            dto::LanguageChoice::FollowMac,
        ] {
            request(&mut handler, Request::SetLanguage(choice));

            assert_eq!(handler.current().language, choice);
            assert_eq!(
                memory
                    .saved
                    .lock()
                    .unwrap()
                    .last()
                    .map(|saved| saved.language),
                Some(choice)
            );
        }
        assert_eq!(memory.saved.lock().unwrap().len(), 3);
    }

    #[test]
    fn choosing_the_language_already_chosen_saves_nothing() {
        let (mut handler, memory) = handler();

        request(
            &mut handler,
            Request::SetLanguage(dto::LanguageChoice::FollowMac),
        );

        assert!(memory.saved.lock().unwrap().is_empty());
    }

    #[test]
    fn a_language_switch_says_a_failed_save_again_in_the_new_language() {
        let memory = Arc::new(Memory {
            fail: true,
            ..Memory::default()
        });
        let mut handler = Handler::new(memory);
        let before = crate::i18n::with_language(dto::Language::English, || {
            request(&mut handler, Request::SetStatusBar(true));
            handler.save_error.clone().expect("the save failed").title
        });

        let after = crate::i18n::with_language(dto::Language::Thai, || {
            handler.language_changed();
            handler.save_error.clone().expect("still said").title
        });

        assert_ne!(after, before);
    }

    #[test]
    fn a_language_switch_does_not_make_a_failed_save_more_than_one_recent_problem() {
        let memory = Arc::new(Memory {
            fail: true,
            ..Memory::default()
        });
        let mut handler = Handler::new(memory);
        crate::handler::error::clear_recent();
        request(&mut handler, Request::SetStatusBar(true));

        crate::i18n::with_language(dto::Language::Thai, || handler.language_changed());

        let problems = crate::handler::error::recent_problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].times, 1);
    }

    #[test]
    fn the_dialog_draws_in_every_language() {
        for language in dto::Language::ALL {
            crate::i18n::with_language(language, || {
                let (handler, _) = handler();
                let _ = handler.view();
            });
        }
    }

    #[test]
    fn an_unchanged_setting_is_not_saved_again() {
        let (mut handler, memory) = handler();

        request(&mut handler, Request::SetStatusBar(false));

        assert!(memory.saved.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_save_is_shown_but_the_change_still_applies() {
        let memory = Arc::new(Memory {
            fail: true,
            ..Memory::default()
        });
        let mut handler = Handler::new(memory);

        request(&mut handler, Request::SetStatusBar(true));

        assert!(handler.current().status_bar);
        assert!(handler.save_error.is_some());
        let _ = handler.view();
    }

    #[test]
    fn about_names_the_app_its_version_and_where_it_lives() {
        let [name, what, credits] = about_lines();

        assert_eq!(
            name,
            format!("open-mtp-client {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(what.contains("Android"));
        assert!(what.contains("no telemetry"));
        assert!(credits.contains("github.com/kidpeterpan/open-mtp-client"));
    }

    #[test]
    fn renders_with_and_without_the_mac_pane() {
        let (mut handler, _) = handler();
        let _ = handler.view();

        request(&mut handler, Request::SetShowMacPane(false));
        let _ = handler.view();
    }
}
