//! A banner below the panes when the app's own code failed: at start-up for
//! reports an earlier run left (`crash-<ms>.txt` beside the settings, written
//! by `internal::service::crash`'s panic hook), and while running when a task
//! or a worker thread panicked and the app went on without what it was doing.
//! Nothing leaves the Mac; "Show Report" opens the file in the text editor.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::Request;
use super::response::Banner;
use crate::i18n::t;

pub trait Service: Send + Sync {
    /// Reports the user has not been told about, newest first.
    fn unreported(&self) -> Vec<dto::CrashNote>;
    /// The user has been told: the reports are kept, marked as seen.
    fn mark_reported(&self);
    /// How many panics this process has had.
    fn panics(&self) -> u64;
    /// The latest panic of this process.
    fn last_panic(&self) -> Option<dto::CrashNote>;
    /// Opens a report in its app; false if that failed.
    fn open(&self, file: &Path) -> bool;
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
}

enum Cause {
    Earlier(Vec<dto::CrashNote>),
    Stopped(dto::CrashNote),
}

/// What the banner is saying.
struct Shown {
    cause: Cause,
    banner: Banner,
    file: Option<PathBuf>,
    open_failed: bool,
}

impl Shown {
    fn new(cause: Cause) -> Option<Self> {
        let (banner, file) = Self::worded(&cause, false)?;
        Some(Self {
            cause,
            banner,
            file,
            open_failed: false,
        })
    }

    fn worded(cause: &Cause, open_failed: bool) -> Option<(Banner, Option<PathBuf>)> {
        let (mut banner, file) = match cause {
            Cause::Earlier(notes) => (
                mapper::earlier(notes)?,
                notes.first().and_then(|note| note.file.clone()),
            ),
            Cause::Stopped(note) => (mapper::stopped(note), note.file.clone()),
        };
        if open_failed {
            banner.hint = t("crash.could_not_open").to_owned();
        }
        Some((banner, file))
    }

    fn reworded(&mut self) {
        if let Some((banner, _)) = Self::worded(&self.cause, self.open_failed) {
            self.banner = banner;
        }
    }
}

pub struct Handler {
    service: Arc<dyn Service>,
    shown: Option<Shown>,
    /// Panics of this process already looked at.
    seen: u64,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        let shown = Shown::new(Cause::Earlier(service.unreported()));
        let seen = service.panics();
        Self {
            service,
            shown,
            seen,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.shown.is_some()
    }

    pub fn language_changed(&mut self) {
        if let Some(shown) = &mut self.shown {
            shown.reworded();
        }
    }

    /// Looks for a panic since the last look: one atomic load, so `app.rs`
    /// does it after every update. A newer panic replaces what is shown.
    pub fn check(&mut self) {
        let panics = self.service.panics();
        if panics == self.seen {
            return;
        }
        self.seen = panics;
        if let Some(note) = self.service.last_panic() {
            self.shown = Shown::new(Cause::Stopped(note));
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let Message::Request(request) = message;
        match request {
            Request::Show => {
                if let Some(shown) = &mut self.shown
                    && let Some(file) = &shown.file
                    && !self.service.open(file)
                {
                    shown.open_failed = true;
                    shown.reworded();
                }
            }
            Request::Dismiss => {
                self.service.mark_reported();
                self.shown = None;
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(Shown { banner, .. }) = &self.shown else {
            return space().into();
        };
        let mut actions = row![].spacing(8).align_y(Alignment::Center);
        if banner.can_show {
            actions = actions.push(
                button(text(t("crash.show_report")).size(13))
                    .style(button::secondary)
                    .on_press(Message::Request(Request::Show)),
            );
        }
        actions = actions.push(
            button(text(t("label.dismiss")).size(13))
                .style(button::secondary)
                .on_press(Message::Request(Request::Dismiss)),
        );
        container(
            row![
                column![
                    text(banner.title.as_str()).size(13),
                    text(banner.hint.as_str()).size(12).style(text::secondary),
                ]
                .spacing(2)
                .width(Length::Fill),
                actions,
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .style(container::rounded_box)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::handler::testing::FakeCrash;

    fn note(at_ms: u64, file: Option<&str>) -> dto::CrashNote {
        dto::CrashNote {
            at_ms,
            location: "internal/x.rs:1:1".to_owned(),
            reported: false,
            file: file.map(PathBuf::from),
        }
    }

    fn handler_with(fake: FakeCrash) -> (Handler, Arc<FakeCrash>) {
        let fake = Arc::new(fake);
        (Handler::new(Arc::clone(&fake) as Arc<dyn Service>), fake)
    }

    fn shows(handler: &Handler, words: &str) -> bool {
        iced_test::simulator(handler.view()).find(words).is_ok()
    }

    #[test]
    fn a_change_of_language_words_the_banner_again_and_a_failed_open_stays_failed() {
        use internal::dto::Language;

        let fake = FakeCrash {
            unreported: vec![note(1_792_000_000_000, Some("/c/crash-1.txt"))],
            open_fails: true,
            ..FakeCrash::default()
        };
        let (mut handler, _) = handler_with(fake);
        assert!(shows(&handler, "Show Report"));

        for language in [
            Language::Thai,
            Language::Japanese,
            Language::ChineseSimplified,
        ] {
            crate::i18n::set_language(language);
            handler.language_changed();

            assert!(
                shows(&handler, crate::i18n::t("crash.show_report")),
                "{language:?}"
            );
            assert!(
                shows(&handler, crate::i18n::t("label.dismiss")),
                "{language:?}"
            );
            assert!(!shows(&handler, "Show Report"), "{language:?}");
        }

        let _ = handler.update(Message::Request(Request::Show));
        crate::i18n::set_language(Language::English);
        handler.language_changed();
        assert!(shows(
            &handler,
            "The report could not be opened. It is in the app's folder on this Mac, in Application Support, under crashes."
        ));
    }

    #[test]
    fn a_panic_banner_is_said_again_in_the_new_language_too() {
        let (mut handler, fake) = handler_with(FakeCrash::default());
        fake.panic_at(1_792_000_000_000, Some("/c/crash-2.txt"));
        handler.check();
        assert!(shows(&handler, "Dismiss"));

        crate::i18n::set_language(internal::dto::Language::Thai);
        handler.language_changed();

        assert!(handler.is_visible());
        assert!(shows(&handler, crate::i18n::t("label.dismiss")));
        assert!(!shows(&handler, "Dismiss"));
    }

    #[test]
    fn a_quiet_start_shows_nothing_at_all() {
        let (handler, _) = handler_with(FakeCrash::default());

        assert!(!handler.is_visible());
        assert!(!shows(&handler, "Dismiss"));
    }

    #[test]
    fn reports_an_earlier_run_left_are_said_at_start_up() {
        let fake = FakeCrash {
            unreported: vec![note(1_792_000_000_000, Some("/c/crash-1.txt"))],
            ..FakeCrash::default()
        };
        let (handler, _) = handler_with(fake);

        assert!(handler.is_visible());
        assert!(shows(&handler, "Show Report"));
        assert!(shows(&handler, "Dismiss"));
        assert!(shows(
            &handler,
            "Transfers that were not finished, if any, are offered below. A report was saved on this Mac; it goes nowhere unless you send it."
        ));
    }

    #[test]
    fn a_report_that_could_not_be_saved_offers_no_way_to_open_it() {
        let fake = FakeCrash {
            unreported: vec![note(1, None)],
            ..FakeCrash::default()
        };
        let (handler, _) = handler_with(fake);

        assert!(shows(&handler, "Dismiss"));
        assert!(!shows(&handler, "Show Report"));
    }

    #[test]
    fn show_opens_the_newest_report_and_leaves_the_banner() {
        let fake = FakeCrash {
            unreported: vec![
                note(2, Some("/c/crash-2.txt")),
                note(1, Some("/c/crash-1.txt")),
            ],
            ..FakeCrash::default()
        };
        let (mut handler, fake) = handler_with(fake);

        let _ = handler.update(Message::Request(Request::Show));

        assert_eq!(
            *fake.opened.lock().unwrap(),
            [PathBuf::from("/c/crash-2.txt")]
        );
        assert!(handler.is_visible(), "looking is not being done with it");
        assert!(fake.told.load(Ordering::SeqCst) == 0);
    }

    #[test]
    fn a_report_that_will_not_open_says_where_it_is() {
        let fake = FakeCrash {
            unreported: vec![note(1, Some("/c/crash-1.txt"))],
            open_fails: true,
            ..FakeCrash::default()
        };
        let (mut handler, _) = handler_with(fake);

        let _ = handler.update(Message::Request(Request::Show));

        assert!(shows(
            &handler,
            "The report could not be opened. It is in the app's folder on this Mac, in Application Support, under crashes."
        ));
    }

    #[test]
    fn dismissing_marks_the_reports_as_seen_and_clears_the_banner() {
        let fake = FakeCrash {
            unreported: vec![note(1, Some("/c/crash-1.txt"))],
            ..FakeCrash::default()
        };
        let (mut handler, fake) = handler_with(fake);

        let _ = handler.update(Message::Request(Request::Dismiss));

        assert_eq!(fake.told.load(Ordering::SeqCst), 1);
        assert!(!handler.is_visible());
        assert!(!shows(&handler, "Dismiss"));
    }

    #[test]
    fn a_panic_while_running_is_said_and_only_once() {
        let (mut handler, fake) = handler_with(FakeCrash::default());
        handler.check();
        assert!(!handler.is_visible(), "no panic, nothing to say");

        fake.panic_at(5_000, Some("/c/crash-5.txt"));
        handler.check();

        assert!(handler.is_visible());
        assert!(shows(
            &handler,
            "The app is still running, but what it was doing may not have finished. If a pane or a dialog seems stuck, close it, or quit and open the app again. A report was saved on this Mac; it goes nowhere unless you send it."
        ));

        // Dismissed, and no new panic: it stays gone, however often it looks.
        let _ = handler.update(Message::Request(Request::Dismiss));
        handler.check();
        handler.check();
        assert!(!handler.is_visible());

        // Another panic brings it back, with the newer one.
        fake.panic_at(9_000, None);
        handler.check();
        assert!(handler.is_visible());
        assert!(shows(
            &handler,
            "The app is still running, but what it was doing may not have finished. If a pane or a dialog seems stuck, close it, or quit and open the app again. A report could not be saved."
        ));
    }

    #[test]
    fn a_panic_before_the_handler_began_is_not_said_again() {
        let fake = FakeCrash::default();
        fake.panic_at(1, None);

        let (mut handler, _) = handler_with(fake);
        handler.check();

        assert!(!handler.is_visible());
    }

    #[test]
    fn a_panic_replaces_what_an_earlier_run_left_on_the_banner() {
        let fake = FakeCrash {
            unreported: vec![note(1, Some("/c/crash-1.txt"))],
            ..FakeCrash::default()
        };
        let (mut handler, fake) = handler_with(fake);
        assert!(shows(
            &handler,
            "Transfers that were not finished, if any, are offered below. A report was saved on this Mac; it goes nowhere unless you send it."
        ));

        fake.panic_at(7_000, Some("/c/crash-7.txt"));
        handler.check();
        let _ = handler.update(Message::Request(Request::Show));

        assert_eq!(
            *fake.opened.lock().unwrap(),
            [PathBuf::from("/c/crash-7.txt")]
        );
    }
}
