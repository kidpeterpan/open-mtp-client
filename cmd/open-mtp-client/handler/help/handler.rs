use std::sync::Arc;

use iced::widget::{button, column, container, responsive, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use super::request::{Link, Request, Tab};
use super::response::ShortcutGroup;
use super::whats_new;
use crate::handler::diagnosis::{self, DiagnosisResponse};
use crate::handler::error::{self, ErrorResponse};
use crate::handler::format;
use crate::handler::guard;
use crate::i18n::{english_only, key, t, tf};
use crate::keymap;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Looks at the Mac and the USB bus for anything that stops a phone connecting.
    async fn checkup(&self) -> dto::Diagnosis;
    /// Quits the apps in the way and waits for them to be gone.
    async fn release(&self, holders: &[dto::Holder]);
    /// The Mac, for a support report.
    async fn mac(&self) -> dto::MacInfo;
    /// Opens a web page; false if the address was refused.
    fn open_link(&self, url: &str) -> bool;
    /// Reads a phone folder from the phone and times it.
    async fn time_reading(
        &self,
        storage_id: u64,
        path: &str,
    ) -> Result<dto::ReadTiming, dto::Error>;
    /// Where the command-line tool that comes inside the app is, when the
    /// app runs from its bundle; `None` otherwise (a build run from Cargo).
    fn bundled_cli(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    /// A connection check finished. `generation` says which opening of the
    /// dialog asked, so a slow answer can't land in a later one.
    Checked {
        generation: u64,
        diagnosis: dto::Diagnosis,
        holders: Vec<dto::Holder>,
    },
    /// The diagnostics report is ready.
    Report {
        generation: u64,
        text: String,
    },
    /// A timed reading of the phone's folder finished.
    Timed {
        generation: u64,
        result: Result<dto::ReadTiming, ErrorResponse>,
    },
}

enum Check {
    Idle,
    Running,
    Done {
        diagnosis: dto::Diagnosis,
        response: DiagnosisResponse,
        /// What "Quit … and check again" quits.
        holders: Vec<dto::Holder>,
    },
}

enum Report {
    Idle,
    Gathering,
    Ready(String),
}

/// Timing one reading of the phone's folder.
enum Timing {
    Idle,
    Running,
    Done(Result<dto::ReadTiming, ErrorResponse>),
}

/// How the phone looks to the app when Help is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    /// A line for the report: "Pixel 9, serial …1234, 2 storages".
    pub summary: String,
    /// The app is connected to it, so the connection check has no fault to find.
    pub connected: bool,
    /// What the open phone says it can do.
    pub abilities: Option<dto::Abilities>,
    /// The storage and folder the phone pane shows: what a timed reading reads.
    pub folder: Option<(u64, String)>,
    /// The open phone's USB link, when known.
    pub link: Option<dto::UsbSpeed>,
}

impl Phone {
    pub fn new(summary: impl Into<String>, connected: bool) -> Self {
        Self {
            summary: summary.into(),
            connected,
            abilities: None,
            folder: None,
            link: None,
        }
    }

    /// The open phone's abilities and the folder its pane shows.
    pub fn with(
        mut self,
        abilities: Option<dto::Abilities>,
        folder: Option<(u64, String)>,
    ) -> Self {
        self.abilities = abilities;
        self.folder = folder;
        self
    }

    /// The open phone's USB link.
    pub fn with_link(mut self, link: Option<dto::UsbSpeed>) -> Self {
        self.link = link;
        self
    }
}

/// Help: the keyboard shortcuts, a live check of why a phone won't connect,
/// and a diagnostics report that can be read before it is copied.
pub struct Handler {
    service: Arc<dyn Service>,
    open: bool,
    tab: Tab,
    /// After an update: the version that ran before, so the releases since
    /// are marked new.
    new_since: Option<dto::Version>,
    generation: u64,
    check: Check,
    report: Report,
    /// How the phone looks to the app right now.
    phone: Phone,
    timing: Timing,
    copied: bool,
    shortcuts: Vec<ShortcutGroup>,
    /// The command-line tool inside the app, asked once (no disk per redraw).
    cli: Option<String>,
    cli_copied: bool,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        let cli = service.bundled_cli();
        Self {
            service,
            open: false,
            tab: Tab::Shortcuts,
            new_since: None,
            generation: 0,
            check: Check::Idle,
            report: Report::Idle,
            phone: Phone::new(english_only("not connected"), false),
            timing: Timing::Idle,
            copied: false,
            shortcuts: mapper::to_shortcut_groups(&keymap::catalog()),
            cli,
            cli_copied: false,
        }
    }

    /// The tab shown.
    #[cfg(test)]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the dialog on the Shortcuts tab; `phone` says how the phone looks
    /// to the app at this moment (it shapes the connection check and goes in
    /// the diagnostics report).
    pub fn open(&mut self, phone: Phone) {
        self.open = true;
        self.tab = Tab::Shortcuts;
        // Whatever was still being worked out for an earlier opening is stale.
        self.generation += 1;
        self.check = Check::Idle;
        self.report = Report::Idle;
        self.timing = Timing::Idle;
        self.copied = false;
        self.phone = phone;
        self.new_since = None;
    }

    /// Opens the dialog on "What's new", after an update from `since`: the
    /// releases after it are marked new.
    pub fn open_whats_new(&mut self, phone: Phone, since: dto::Version) {
        self.open(phone);
        self.tab = Tab::WhatsNew;
        self.new_since = Some(since);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.generation += 1;
    }

    pub fn language_changed(&mut self) {
        self.shortcuts = mapper::to_shortcut_groups(&keymap::catalog());
        if let Check::Done {
            diagnosis,
            response,
            ..
        } = &mut self.check
        {
            *response = respond(diagnosis, &self.phone);
        }
        if let Timing::Done(Err(error)) = &mut self.timing {
            *error = error.reworded();
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Request(request) => self.handle(request),
            Message::Checked {
                generation,
                diagnosis,
                holders,
            } => {
                if self.is_current(generation) {
                    self.check = Check::Done {
                        response: respond(&diagnosis, &self.phone),
                        diagnosis,
                        holders,
                    };
                }
                Task::none()
            }
            Message::Report { generation, text } => {
                if self.is_current(generation) {
                    self.report = Report::Ready(text);
                }
                Task::none()
            }
            Message::Timed { generation, result } => {
                if self.is_current(generation) {
                    self.timing = Timing::Done(result);
                }
                Task::none()
            }
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.open && generation == self.generation
    }

    fn handle(&mut self, request: Request) -> Task<Message> {
        match request {
            Request::Open => {
                let phone = self.phone.clone();
                self.open(phone);
                Task::none()
            }
            Request::Close => {
                self.close();
                Task::none()
            }
            Request::Show(tab) => {
                self.tab = tab;
                self.copied = false;
                match (tab, &self.check, &self.report) {
                    (Tab::Troubleshooting, Check::Idle, _) => self.start_check(),
                    (Tab::About, _, Report::Idle) => self.start_report(),
                    _ => Task::none(),
                }
            }
            Request::CheckAgain => self.start_check(),
            Request::TimeReading => {
                let Some((storage_id, path)) = self.phone.folder.clone() else {
                    return Task::none();
                };
                if matches!(self.timing, Timing::Running) {
                    return Task::none();
                }
                self.timing = Timing::Running;
                let (service, generation) = (Arc::clone(&self.service), self.generation);
                guard::perform(
                    async move {
                        service
                            .time_reading(storage_id, &path)
                            .await
                            .map_err(error::to_error_response)
                    },
                    || Err(guard::crashed()),
                    move |result| Message::Timed { generation, result },
                )
            }
            Request::QuitHolders => {
                let Check::Done { holders, .. } = &self.check else {
                    return Task::none();
                };
                if holders.is_empty() {
                    return Task::none();
                }
                let holders = holders.clone();
                self.check = Check::Running;
                let (service, generation) = (Arc::clone(&self.service), self.generation);
                let phone = self.phone.clone();
                guard::perform(
                    async move {
                        // Whatever happens to the quitting, it is checked again.
                        let _ = guard::caught(service.release(&holders)).await;
                        check_now(service, generation, phone).await
                    },
                    move || check_crashed(generation),
                    |message| message,
                )
            }
            Request::CopyReport => match &self.report {
                Report::Ready(text) => {
                    self.copied = true;
                    iced::clipboard::write(text.clone())
                }
                _ => Task::none(),
            },
            Request::CopyCliLine => match &self.cli {
                Some(path) => {
                    self.cli_copied = true;
                    iced::clipboard::write(mapper::cli_link_line(path))
                }
                None => Task::none(),
            },
            Request::OpenLink(link) => {
                self.service.open_link(link.url());
                Task::none()
            }
        }
    }

    fn start_check(&mut self) -> Task<Message> {
        self.check = Check::Running;
        let (service, generation) = (Arc::clone(&self.service), self.generation);
        let phone = self.phone.clone();
        guard::perform(
            check_now(service, generation, phone),
            move || check_crashed(generation),
            |message| message,
        )
    }

    fn start_report(&mut self) -> Task<Message> {
        self.report = Report::Gathering;
        let (service, generation) = (Arc::clone(&self.service), self.generation);
        let phone = mapper::ReportPhone {
            summary: self.phone.summary.clone(),
            abilities: self.phone.abilities,
            link: self.phone.link,
            reading: match &self.timing {
                Timing::Done(Ok(timing)) => Some(*timing),
                _ => None,
            },
        };
        // Taken now, on this thread: the history of what was shown up to the
        // moment the report was asked for.
        let problems = error::recent_problems();
        guard::perform(
            gather_report(service, generation, phone, problems),
            move || report_crashed(generation),
            |message| message,
        )
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
        let tab = |label: &'static str, tab: Tab| {
            button(text(label).size(14))
                .style(if self.tab == tab {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Message::Request(Request::Show(tab)))
        };
        let body: Element<'_, Message> = match self.tab {
            Tab::Shortcuts => self.shortcuts_tab(),
            Tab::Troubleshooting => self.troubleshooting_tab(),
            Tab::About => self.about_tab(),
            Tab::WhatsNew => self.whats_new_tab(),
        };

        container(
            column![
                row![
                    text(t("help.title")).size(20),
                    space::horizontal(),
                    button(text(t("label.done")))
                        .style(button::primary)
                        .on_press(Message::Request(Request::Close)),
                ]
                .align_y(Alignment::Center),
                row![
                    tab(t("help.tab.shortcuts"), Tab::Shortcuts),
                    tab(t("help.tab.troubleshooting"), Tab::Troubleshooting),
                    tab(t("help.tab.whats_new"), Tab::WhatsNew),
                    tab(t("help.tab.about"), Tab::About),
                ]
                .spacing(6),
                rule::horizontal(1),
                scrollable(container(body).padding([0, 12]).width(Length::Fill)).height(
                    Length::Fixed((window_height - AROUND_THE_LIST).clamp(LIST_LEAST, LIST_MOST))
                ),
            ]
            .spacing(12),
        )
        .padding(24)
        .width(Length::Fixed(620.0))
        .style(container::rounded_box)
        .into()
    }

    /// Every release, newest first; after an update, the ones since the
    /// version that ran before are marked new.
    fn whats_new_tab(&self) -> Element<'_, Message> {
        let mut content = column![].spacing(16);
        for release in whats_new::RELEASES {
            let is_new = self.new_since.is_some_and(|since| {
                dto::Version::parse(release.version).is_some_and(|version| version > since)
            });
            let mut heading =
                row![text(tf("help.release_version", &[("version", release.version)])).size(16)]
                    .spacing(8)
                    .align_y(Alignment::Center);
            if is_new {
                heading = heading.push(
                    container(text(t("help.new_badge")).size(11))
                        .padding([1, 6])
                        .style(container::primary),
                );
            }
            let mut notes = column![heading].spacing(6);
            for note in release.notes {
                notes = notes.push(
                    row![
                        text("•").size(14).style(text::secondary),
                        text(t(note)).size(14).width(Length::Fill)
                    ]
                    .spacing(8),
                );
            }
            content = content.push(notes);
        }
        content.into()
    }

    fn shortcuts_tab(&self) -> Element<'_, Message> {
        let key = |label: &str| {
            container(text(label.to_owned()).size(13))
                .padding([2, 8])
                .style(container::bordered_box)
        };
        let mut content = column![].spacing(10);
        for group in &self.shortcuts {
            content = content.push(text(&group.title).size(13).style(text::secondary));
            for line in &group.lines {
                let mut keys = row![].spacing(6).align_y(Alignment::Center);
                for (index, label) in line.keys.iter().enumerate() {
                    if index > 0 {
                        keys = keys.push(text(t("help.or")).size(12).style(text::secondary));
                    }
                    keys = keys.push(key(label));
                }
                content = content.push(
                    row![text(&line.label).size(14).width(Length::Fill), keys]
                        .align_y(Alignment::Center),
                );
            }
        }
        content = content.push(text(t("help.with_mouse")).size(13).style(text::secondary));
        for (gesture, does) in mapper::mouse_tips() {
            content = content.push(
                row![
                    text(gesture).size(14).width(Length::FillPortion(2)),
                    text(does)
                        .size(13)
                        .width(Length::FillPortion(5))
                        .style(text::secondary),
                ]
                .spacing(12),
            );
        }
        content.into()
    }

    /// What the open phone says it can do, and a timed reading of the folder
    /// its pane shows.
    fn phone_section(&self) -> Element<'_, Message> {
        let mut section =
            column![rule::horizontal(1), text(t("help.phone.title")).size(14)].spacing(6);
        if let Some(abilities) = &self.phone.abilities {
            let (can, cannot) = format::abilities_in_words(abilities);
            let line = |sentence: &'static str, what: Vec<&str>| {
                let what: Vec<String> = what.into_iter().map(str::to_owned).collect();
                text(tf(sentence, &[("what", &format::comma_list(&what))]))
                    .size(13)
                    .width(Length::Fill)
            };
            if !can.is_empty() {
                section = section.push(line(key("help.phone.can"), can));
            }
            if !cannot.is_empty() {
                section =
                    section.push(line(key("help.phone.cannot"), cannot).style(text::secondary));
            }
        }
        if let Some(slow) = format::slow_link_note(self.phone.link) {
            section = section.push(text(slow).size(13).width(Length::Fill));
        }
        if let Some((_, path)) = &self.phone.folder {
            let running = matches!(self.timing, Timing::Running);
            let mut timing = row![
                button(text(tf("help.phone.time", &[("path", path)])).size(13))
                    .style(button::secondary)
                    .on_press_maybe((!running).then_some(Message::Request(Request::TimeReading)))
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            let said = match &self.timing {
                Timing::Idle => None,
                Timing::Running => Some(t("help.phone.reading").to_owned()),
                Timing::Done(Ok(reading)) => Some(format::format_reading(reading)),
                Timing::Done(Err(error)) => {
                    Some(tf("help.phone.could_not", &[("reason", &error.title)]))
                }
            };
            if let Some(said) = said {
                timing = timing.push(text(said).size(13).style(text::secondary));
            }
            section = section.push(timing).push(
                text(t("help.phone.reading_note"))
                    .size(12)
                    .style(text::secondary),
            );
        }
        section.into()
    }

    fn troubleshooting_tab(&self) -> Element<'_, Message> {
        let mut content = column![
            text(t("help.trouble.title")).size(16),
            text(t("help.trouble.intro"))
                .size(13)
                .style(text::secondary),
        ]
        .spacing(8);

        match &self.check {
            Check::Idle | Check::Running => {
                content = content.push(
                    text(t("help.trouble.checking"))
                        .size(14)
                        .style(text::secondary),
                );
            }
            Check::Done { response, .. } => {
                content = content.push(diagnosis::checklist(&response.checks));
                let mut buttons = row![].spacing(10);
                if let Some(label) = &response.quit_label {
                    buttons = buttons.push(
                        button(text(label.clone()).size(13))
                            .style(button::primary)
                            .on_press(Message::Request(Request::QuitHolders)),
                    );
                }
                buttons = buttons.push(
                    button(text(t("label.check_again")).size(13))
                        .style(button::secondary)
                        .on_press(Message::Request(Request::CheckAgain)),
                );
                content = content.push(buttons);
            }
        }

        if self.phone.connected {
            content = content.push(self.phone_section());
        }
        content = content
            .push(rule::horizontal(1))
            .push(text(t("help.trouble.things")).size(14));
        for (number, step) in STEPS.into_iter().enumerate() {
            content = content.push(
                row![
                    text(format!("{}.", number + 1)).size(13).width(22),
                    text(t(step)).size(13).width(Length::Fill),
                ]
                .spacing(4),
            );
        }
        content
            .push(
                text(t("help.trouble.still_stuck"))
                    .size(13)
                    .style(text::secondary),
            )
            .into()
    }

    fn about_tab(&self) -> Element<'_, Message> {
        let mut content = column![
            row![
                text(english_only(format!(
                    "open-mtp-client {}",
                    env!("CARGO_PKG_VERSION")
                )))
                .size(16)
                .width(Length::Fill),
                button(text(t("help.about.coffee")).size(13))
                    .style(button::secondary)
                    .on_press(Message::Request(Request::OpenLink(Link::Support))),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            text(t("help.about.line")).size(13).style(text::secondary),
            rule::horizontal(1),
            text(t("help.about.privacy_title")).size(14),
            text(t(PRIVACY)).size(13),
            rule::horizontal(1),
        ]
        .spacing(8);
        if let Some(path) = &self.cli {
            let copy = if self.cli_copied {
                t("help.about.copied")
            } else {
                t("menu.item.copy")
            };
            content = content
                .push(text(t("help.about.cli_title")).size(14))
                .push(
                    text(tf(
                        "help.about.cli_text",
                        &[
                            ("cli", english_only("open-mtp-client-cli")),
                            ("help", english_only("open-mtp-client-cli --help")),
                            ("folder", english_only("/usr/local/bin")),
                            ("mkdir", english_only("sudo mkdir -p /usr/local/bin")),
                        ],
                    ))
                    .size(13)
                    .style(text::secondary),
                )
                .push(text(path.clone()).size(12).font(iced::Font::MONOSPACE))
                .push(
                    row![
                        container(
                            text(mapper::cli_link_line(path))
                                .size(12)
                                .font(iced::Font::MONOSPACE)
                        )
                        .padding(8)
                        .width(Length::Fill)
                        .style(container::bordered_box),
                        button(text(copy).size(13))
                            .style(button::secondary)
                            .on_press(Message::Request(Request::CopyCliLine)),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                )
                .push(rule::horizontal(1));
        }
        content = content.extend([
            text(t("help.about.diagnostics_title")).size(14).into(),
            text(t("help.about.diagnostics_text"))
                .size(13)
                .style(text::secondary)
                .into(),
        ]);

        match &self.report {
            Report::Idle | Report::Gathering => {
                content = content.push(
                    text(t("help.about.collecting"))
                        .size(13)
                        .style(text::secondary),
                );
            }
            Report::Ready(report) => {
                content = content.push(
                    container(text(report.clone()).size(12).font(iced::Font::MONOSPACE))
                        .padding(10)
                        .width(Length::Fill)
                        .style(container::bordered_box),
                );
                let copy = if self.copied {
                    t("help.about.copied")
                } else {
                    t("help.about.copy_report")
                };
                content = content.push(
                    row![
                        button(text(copy).size(13))
                            .style(button::primary)
                            .on_press(Message::Request(Request::CopyReport)),
                        button(text(t("help.about.report_issue")).size(13))
                            .style(button::secondary)
                            .on_press(Message::Request(Request::OpenLink(Link::NewIssue))),
                        button(text(t("help.about.project_page")).size(13))
                            .style(button::secondary)
                            .on_press(Message::Request(Request::OpenLink(Link::Project))),
                    ]
                    .spacing(10),
                );
            }
        }
        content.into()
    }
}

const AROUND_THE_LIST: f32 = 208.0;
const LIST_LEAST: f32 = 120.0;
const LIST_MOST: f32 = 400.0;

/// What to try when a phone won't connect, most likely first; the live
/// check above names what applies here.
const STEPS: [&str; 6] = [
    key("help.trouble.step_1"),
    key("help.trouble.step_2"),
    key("help.trouble.step_3"),
    key("help.trouble.step_4"),
    key("help.trouble.step_5"),
    key("help.trouble.step_6"),
];

/// Said in the About tab, and checked against what the app is built from.
const PRIVACY: &str = key("help.about.privacy");

/// Looks at the Mac and the USB bus. For someone who is connected there is
/// nothing to fix, so the findings are shown as facts and nothing is offered
/// to quit.
async fn check_now(service: Arc<dyn Service>, generation: u64, phone: Phone) -> Message {
    let diagnosis = service.checkup().await;
    let holders = if phone.connected {
        Vec::new()
    } else {
        diagnosis.holders.clone().unwrap_or_default()
    };
    Message::Checked {
        generation,
        diagnosis,
        holders,
    }
}

/// What a check that panicked says: nothing was found out, so the usual steps
/// are shown, with "Check again", rather than "Checking…" for ever.
fn check_crashed(generation: u64) -> Message {
    Message::Checked {
        generation,
        diagnosis: dto::Diagnosis::default(),
        holders: Vec::new(),
    }
}

/// What a report that panicked says, in the place of the report.
fn report_crashed(generation: u64) -> Message {
    let crashed = guard::crashed();
    Message::Report {
        generation,
        text: format!("{}\n{}", crashed.title, crashed.hint),
    }
}

fn respond(diagnosis: &dto::Diagnosis, phone: &Phone) -> DiagnosisResponse {
    if phone.connected {
        return diagnosis::to_connected_response(diagnosis, &phone.summary);
    }
    diagnosis::to_diagnosis_response(diagnosis)
}

/// Collects the diagnostics report.
async fn gather_report(
    service: Arc<dyn Service>,
    generation: u64,
    phone: mapper::ReportPhone,
    problems: Vec<error::Problem>,
) -> Message {
    let mac = service.mac().await;
    let diagnosis = service.checkup().await;
    Message::Report {
        generation,
        text: mapper::report_text(
            env!("CARGO_PKG_VERSION"),
            &mac,
            &phone,
            &diagnosis,
            &problems,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::handler::testing::{FakeHelp, drain};

    fn handler_with(fake: FakeHelp) -> (Handler, Arc<FakeHelp>) {
        let fake = Arc::new(fake);
        (Handler::new(Arc::clone(&fake) as Arc<dyn Service>), fake)
    }

    fn handler() -> (Handler, Arc<FakeHelp>) {
        handler_with(FakeHelp::default())
    }

    fn holder(name: &str) -> dto::Holder {
        dto::Holder {
            name: name.into(),
            restarts: false,
        }
    }

    /// A Mac where Photos is holding the phone and a Pixel is on USB.
    fn photos_in_the_way() -> FakeHelp {
        FakeHelp {
            diagnosis: dto::Diagnosis {
                holders: Some(vec![holder("Photos")]),
                phones: Some(vec!["Pixel 7".into()]),
                ..Default::default()
            },
            ..FakeHelp::default()
        }
    }

    fn request(handler: &mut Handler, request: Request) -> Task<Message> {
        handler.update(Message::Request(request))
    }

    /// Sends a request and feeds back whatever messages its work produces.
    async fn settle(handler: &mut Handler, request: Request) -> Vec<String> {
        let (messages, copied) = drain(handler.update(Message::Request(request))).await;
        for message in messages {
            let _ = handler.update(message);
        }
        copied
    }

    fn text_on_screen(handler: &Handler, wanted: &str) -> bool {
        iced_test::simulator(handler.view())
            .find(wanted.to_owned())
            .is_ok()
    }

    // What the phone can do, and a timed reading.

    fn connected_phone() -> Phone {
        Phone::new("Pixel 9, serial …1234, 1 storage", true).with(
            Some(dto::Abilities {
                copy: true,
                partial_reads: true,
                partial_reads_64: true,
                thumbnails: true,
                ..dto::Abilities::default()
            }),
            Some((5, "/DCIM".to_owned())),
        )
    }

    #[tokio::test]
    async fn troubleshooting_and_the_report_say_when_the_usb_link_is_slow() {
        let slow = "Connected at USB 2 speed (480 Mb/s): about 35 MB/s at most. A USB 3 cable and port would be faster.";
        let (mut help, _) = handler();
        help.open(connected_phone().with_link(Some(dto::UsbSpeed::High)));
        settle(&mut help, Request::Show(Tab::Troubleshooting)).await;
        assert!(text_on_screen(&help, slow));
        settle(&mut help, Request::Show(Tab::About)).await;
        let Report::Ready(report) = &help.report else {
            panic!("ready");
        };
        assert!(report.contains("USB link: USB 2, 480 Mb/s"), "{report}");

        let (mut fast, _) = handler();
        fast.open(connected_phone().with_link(Some(dto::UsbSpeed::Super)));
        settle(&mut fast, Request::Show(Tab::Troubleshooting)).await;
        assert!(
            !text_on_screen(&fast, slow),
            "a USB 3 link has nothing to say"
        );
    }

    #[tokio::test]
    async fn troubleshooting_says_what_the_phone_can_do_and_times_its_folder() {
        let (mut handler, service) = handler();
        handler.open(connected_phone());
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        assert!(text_on_screen(
            &handler,
            "It can copy on the phone, resume a stopped download, make its own thumbnails."
        ));
        assert!(text_on_screen(
            &handler,
            "It can't move on the phone, rename, read a folder's details at once, say when its files change."
        ));

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("Time reading “/DCIM”").unwrap();
            ui.into_messages().collect()
        };
        let task = handler.update(clicked.into_iter().next().unwrap());
        assert!(text_on_screen(&handler, "Reading…"));
        assert!(
            request(&mut handler, Request::TimeReading).units() == 0,
            "one reading at a time"
        );
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }

        assert_eq!(*service.readings.lock().unwrap(), [(5, "/DCIM".to_owned())]);
        assert!(text_on_screen(&handler, "312 items in 1.4 s"));

        settle(&mut handler, Request::Show(Tab::About)).await;
        let Report::Ready(report) = &handler.report else {
            panic!("ready");
        };
        assert!(
            report.contains("Phone operations: CopyObject yes, MoveObject no"),
            "{report}"
        );
        assert!(
            report.contains("Reading a folder: 312 items in 1.4 s"),
            "{report}"
        );
        assert!(
            !report.contains("DCIM"),
            "never a path in the report: {report}"
        );
    }

    #[tokio::test]
    async fn a_reading_that_fails_says_so_and_one_from_an_earlier_opening_is_dropped() {
        let (mut handler, _) = handler_with(FakeHelp {
            reading: Err(dto::Error::Disconnected),
            ..FakeHelp::default()
        });
        handler.open(connected_phone());
        let _ = request(&mut handler, Request::Show(Tab::Troubleshooting));
        let late = request(&mut handler, Request::TimeReading);
        let late = drain(late).await.0;

        handler.open(connected_phone());
        let _ = request(&mut handler, Request::Show(Tab::Troubleshooting));
        for message in late {
            let _ = handler.update(message);
        }
        assert!(
            matches!(handler.timing, Timing::Idle),
            "the old answer is dropped"
        );

        settle(&mut handler, Request::TimeReading).await;
        assert!(matches!(handler.timing, Timing::Done(Err(_))));
        let _ = handler.view();
    }

    // ---- service code that panics: the dialog is never left waiting ----

    #[tokio::test]
    async fn a_connection_check_that_panics_still_ends_with_the_checklist_and_check_again() {
        let (mut handler, _) = handler_with(FakeHelp {
            panics_in: Some("checkup"),
            ..FakeHelp::default()
        });
        handler.open(Phone::new("x", false));

        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        assert!(
            matches!(handler.check, Check::Done { .. }),
            "not left on Checking…"
        );
        assert!(text_on_screen(&handler, "Check again"));
    }

    #[tokio::test]
    async fn quitting_the_apps_when_that_panics_still_checks_again() {
        let (mut handler, fake) = handler_with(FakeHelp {
            panics_in: Some("release"),
            ..photos_in_the_way()
        });
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        settle(&mut handler, Request::QuitHolders).await;

        assert!(matches!(handler.check, Check::Done { .. }));
        assert_eq!(fake.checks.load(Ordering::SeqCst), 2, "checked again");
    }

    #[tokio::test]
    async fn a_report_that_panics_is_a_report_that_says_so_not_an_endless_gathering() {
        let (mut handler, _) = handler_with(FakeHelp {
            panics_in: Some("mac"),
            ..FakeHelp::default()
        });
        handler.open(Phone::new("x", false));

        settle(&mut handler, Request::Show(Tab::About)).await;

        assert!(
            matches!(&handler.report, Report::Ready(text) if text.contains("Something went wrong inside the app")),
            "not left on Gathering…"
        );
    }

    #[tokio::test]
    async fn a_timed_reading_that_panics_ends_with_the_error() {
        let (mut handler, _) = handler_with(FakeHelp {
            panics_in: Some("time_reading"),
            ..FakeHelp::default()
        });
        handler.open(connected_phone());
        let _ = request(&mut handler, Request::Show(Tab::Troubleshooting));

        settle(&mut handler, Request::TimeReading).await;

        assert!(
            matches!(&handler.timing, Timing::Done(Err(error)) if error.source == dto::Error::Crashed),
            "not left on Reading…"
        );
    }

    #[test]
    fn without_a_phone_or_its_folder_there_is_nothing_to_time() {
        let (mut handler, service) = handler();
        handler.open(Phone::new("not connected", false));
        let _ = request(&mut handler, Request::Show(Tab::Troubleshooting));
        assert!(!text_on_screen(&handler, "What this phone says it can do"));
        assert_eq!(request(&mut handler, Request::TimeReading).units(), 0);

        handler.open(Phone::new("Pixel 9", true).with(None, None));
        let _ = request(&mut handler, Request::Show(Tab::Troubleshooting));
        assert!(text_on_screen(&handler, "What this phone says it can do"));
        assert!(!text_on_screen(&handler, "Time reading"), "no folder shown");
        assert_eq!(request(&mut handler, Request::TimeReading).units(), 0);
        assert!(service.readings.lock().unwrap().is_empty());
    }

    #[test]
    fn starts_closed() {
        let (handler, _) = handler();

        assert!(!handler.is_open());
    }

    #[tokio::test]
    async fn about_shows_the_bundled_command_line_tool_and_copies_its_link_line() {
        let path = "/Applications/open-mtp-client.app/Contents/MacOS/open-mtp-client-cli";
        let (mut handler, _) = handler_with(FakeHelp {
            cli: Some(path.into()),
            ..FakeHelp::default()
        });
        handler.open(Phone::new("x", false));
        let _ = request(&mut handler, Request::Show(Tab::About));

        let line = mapper::cli_link_line(path);
        {
            let mut ui = iced_test::simulator(handler.view());
            assert!(ui.find("Command line").is_ok());
            assert!(ui.find(path).is_ok());
            assert!(ui.find(line.as_str()).is_ok());
        }

        let copied = handler.update(Message::Request(Request::CopyCliLine));
        let (_, clipboard) = crate::handler::testing::drain(copied).await;
        assert_eq!(clipboard, [line]);
        assert!(
            iced_test::simulator(handler.view())
                .find("Copied ✓")
                .is_ok()
        );

        // Run from Cargo, not the app: no such section, and nothing to copy.
        let (mut loose, _) = handler_with(FakeHelp::default());
        loose.open(Phone::new("x", false));
        let _ = request(&mut loose, Request::Show(Tab::About));
        assert!(
            iced_test::simulator(loose.view())
                .find("Command line")
                .is_err()
        );
        let nothing = loose.update(Message::Request(Request::CopyCliLine));
        assert!(crate::handler::testing::drain(nothing).await.1.is_empty());
    }

    #[test]
    fn opening_shows_the_shortcuts_first_whatever_was_open_before() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        let _ = request(&mut handler, Request::Show(Tab::About));

        handler.open(Phone::new("y", false));

        assert!(handler.is_open());
        assert_eq!(handler.tab, Tab::Shortcuts);
        assert!(!handler.copied);
        assert!(matches!(handler.check, Check::Idle));
        assert!(matches!(handler.report, Report::Idle));
        assert_eq!(handler.phone, Phone::new("y", false));
    }

    #[test]
    fn done_closes_it() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));

        let _ = request(&mut handler, Request::Close);

        assert!(!handler.is_open());
    }

    #[test]
    fn an_open_request_opens_it_keeping_the_last_known_phone() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("Pixel 9", false));
        handler.close();

        let _ = request(&mut handler, Request::Open);

        assert!(handler.is_open());
        assert_eq!(handler.phone, Phone::new("Pixel 9", false));
    }

    #[tokio::test]
    async fn the_troubleshooting_tab_checks_the_mac_when_first_shown() {
        let (mut handler, fake) = handler_with(photos_in_the_way());
        handler.open(Phone::new("x", false));

        let task = request(&mut handler, Request::Show(Tab::Troubleshooting));
        assert!(matches!(handler.check, Check::Running));
        assert!(text_on_screen(&handler, "Checking…"));
        let (messages, _) = drain(task).await;
        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(fake.checks.load(Ordering::SeqCst), 1);
        let Check::Done {
            response, holders, ..
        } = &handler.check
        else {
            panic!("the check should be done");
        };
        assert_eq!(holders, &[holder("Photos")]);
        assert_eq!(
            response.quit_label.as_deref(),
            Some("Quit Photos and try again")
        );
        assert!(text_on_screen(&handler, "Seen on USB: Pixel 7."));
        assert!(text_on_screen(&handler, "Photos is using your phone."));
    }

    #[tokio::test]
    async fn coming_back_to_the_tab_does_not_check_again_but_the_button_does() {
        let (mut handler, fake) = handler();
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;
        settle(&mut handler, Request::Show(Tab::Shortcuts)).await;

        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;
        assert_eq!(fake.checks.load(Ordering::SeqCst), 1, "kept, not repeated");

        settle(&mut handler, Request::CheckAgain).await;
        assert_eq!(fake.checks.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_mac_with_nothing_in_the_way_says_so_and_offers_no_quit() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        assert!(text_on_screen(
            &handler,
            "No other app is using your phone."
        ));
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Check again").is_ok());
        assert!(ui.find("Quit them and try again").is_err());
    }

    #[tokio::test]
    async fn quitting_the_apps_in_the_way_then_checks_again() {
        let (mut handler, fake) = handler_with(photos_in_the_way());
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;
        assert_eq!(fake.checks.load(Ordering::SeqCst), 1);

        let task = request(&mut handler, Request::QuitHolders);
        assert!(matches!(handler.check, Check::Running), "while it works");
        let (messages, _) = drain(task).await;
        for message in messages {
            let _ = handler.update(message);
        }

        assert_eq!(*fake.released.lock().unwrap(), [["Photos"]]);
        assert_eq!(
            fake.checks.load(Ordering::SeqCst),
            2,
            "and then it looks again"
        );
        assert!(matches!(handler.check, Check::Done { .. }));
    }

    #[tokio::test]
    async fn there_is_nothing_to_quit_before_a_check_or_when_nothing_was_found() {
        let (mut handler, fake) = handler();
        handler.open(Phone::new("x", false));

        // No check yet.
        let (messages, _) = drain(request(&mut handler, Request::QuitHolders)).await;
        assert!(messages.is_empty());
        // A check that found nothing.
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;
        let (messages, _) = drain(request(&mut handler, Request::QuitHolders)).await;

        assert!(messages.is_empty());
        assert!(fake.released.lock().unwrap().is_empty());
        assert!(matches!(handler.check, Check::Done { .. }));
    }

    #[tokio::test]
    async fn the_about_tab_collects_a_report_with_everything_in_it() {
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("Pixel 9, serial …1234, 2 storages", false));

        settle(&mut handler, Request::Show(Tab::About)).await;

        let Report::Ready(text) = &handler.report else {
            panic!("the report should be ready");
        };
        assert!(text.starts_with(&format!("open-mtp-client {}\n", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("macOS 15.1 (aarch64)"));
        assert!(text.contains("Phone: Pixel 9, serial …1234, 2 storages"));
        assert!(text.contains("Seen on USB: Pixel 7"));
        assert!(text.contains("Apps that can hold a phone: Photos"));
        // The whole report is what the preview shows, so it is what gets copied.
        let whole = text.clone();
        assert!(text_on_screen(&handler, &whole));
    }

    #[tokio::test]
    async fn the_report_lists_the_problems_shown_before_it_was_asked_for() {
        error::clear_recent();
        let _ = error::to_error_response(dto::Error::Timeout);
        let _ = error::to_error_response(dto::Error::PathNotFound("/DCIM/IMG_1.jpg".into()));
        let (mut handler, _) = handler();
        handler.open(Phone::new("Pixel 9", false));

        settle(&mut handler, Request::Show(Tab::About)).await;

        let Report::Ready(text) = &handler.report else {
            panic!("the report should be ready");
        };
        let at = text
            .find("Recent problems (this session, newest first):")
            .unwrap();
        let recent = &text[at..];
        assert!(
            recent.find("This folder is gone").unwrap()
                < recent.find("Your phone stopped responding").unwrap(),
            "{recent}"
        );
        assert!(!text.contains("IMG_1"), "titles only: {text}");
        assert!(
            text_on_screen(&handler, text),
            "shown in full before it is copied"
        );
    }

    #[tokio::test]
    async fn copying_puts_exactly_the_shown_report_on_the_clipboard() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("Pixel 9", false));
        settle(&mut handler, Request::Show(Tab::About)).await;
        let Report::Ready(shown) = &handler.report else {
            panic!("ready");
        };
        let shown = shown.clone();
        assert!(text_on_screen(&handler, "Copy report"));

        let copied = settle(&mut handler, Request::CopyReport).await;

        assert_eq!(copied, [shown]);
        assert!(text_on_screen(&handler, "Copied ✓"));
        assert!(!text_on_screen(&handler, "Copy report"));
    }

    #[tokio::test]
    async fn copying_before_the_report_is_ready_does_nothing() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));

        let copied = settle(&mut handler, Request::CopyReport).await;

        assert!(copied.is_empty());
        assert!(!handler.copied);
    }

    #[tokio::test]
    async fn changing_tabs_forgets_that_it_was_copied() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::About)).await;
        settle(&mut handler, Request::CopyReport).await;
        assert!(handler.copied);

        settle(&mut handler, Request::Show(Tab::Shortcuts)).await;
        settle(&mut handler, Request::Show(Tab::About)).await;

        assert!(!handler.copied);
    }

    #[tokio::test]
    async fn an_answer_that_arrives_after_the_dialog_was_closed_or_reopened_is_ignored() {
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("x", false));
        let (stale, _) = drain(request(&mut handler, Request::Show(Tab::Troubleshooting))).await;
        let (stale_report, _) = drain(request(&mut handler, Request::Show(Tab::About))).await;

        // Closed: nothing is taken in.
        handler.close();
        for message in stale.clone().into_iter().chain(stale_report.clone()) {
            let _ = handler.update(message);
        }
        assert!(matches!(handler.check, Check::Running));

        // Reopened: the old answers are about an earlier opening.
        handler.open(Phone::new("x", false));
        for message in stale.into_iter().chain(stale_report) {
            let _ = handler.update(message);
        }
        assert!(matches!(handler.check, Check::Idle));
        assert!(matches!(handler.report, Report::Idle));
    }

    #[test]
    fn links_open_through_the_service_and_only_the_fixed_ones() {
        let (mut handler, fake) = handler();
        handler.open(Phone::new("x", false));

        let _ = request(&mut handler, Request::OpenLink(Link::NewIssue));
        let _ = request(&mut handler, Request::OpenLink(Link::Project));
        let _ = request(&mut handler, Request::OpenLink(Link::Support));

        assert_eq!(
            *fake.links.lock().unwrap(),
            [
                "https://github.com/kidpeterpan/open-mtp-client/issues/new",
                "https://github.com/kidpeterpan/open-mtp-client",
                "https://buymeacoffee.com/kidpeterpan"
            ]
        );
    }

    #[test]
    fn the_shortcuts_tab_lists_every_group_and_line_with_its_keys() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        let mut ui = iced_test::simulator(handler.view());

        for wanted in [
            "Moving around",
            "Copying",
            "Go up one folder",
            "Add the selection to what is copied",
            "⇧⌘C",
            "⌘/",
            "With the mouse",
            "Right-click",
        ] {
            assert!(
                ui.find(wanted.to_owned()).is_ok(),
                "{wanted:?} is not shown"
            );
        }
    }

    #[test]
    fn the_tab_buttons_switch_tabs() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));

        for (label, tab) in [
            ("Troubleshooting", Tab::Troubleshooting),
            ("About", Tab::About),
            ("Shortcuts", Tab::Shortcuts),
        ] {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click(label.to_owned()).unwrap();
            let messages: Vec<Message> = ui.into_messages().collect();
            assert!(
                matches!(messages.as_slice(), [Message::Request(Request::Show(t))] if *t == tab),
                "{label}: {messages:?}"
            );
        }
    }

    #[test]
    fn done_sends_a_close() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        let mut ui = iced_test::simulator(handler.view());

        let _ = ui.click("Done".to_owned()).unwrap();

        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [Message::Request(Request::Close)]
        ));
    }

    #[tokio::test]
    async fn a_change_of_language_rewords_what_help_holds_and_leaves_the_report_in_english() {
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;
        settle(&mut handler, Request::Show(Tab::About)).await;
        handler.timing = Timing::Done(Err(error::to_error_response(dto::Error::Timeout)));
        let Report::Ready(report) = &handler.report else {
            panic!("the report should be ready");
        };
        let report = report.clone();
        let english_group = handler.shortcuts[0].title.clone();

        crate::i18n::set_language(dto::Language::Thai);
        handler.language_changed();

        assert_eq!(
            handler.shortcuts[0].title,
            crate::i18n::t("keymap.group.moving")
        );
        assert_ne!(handler.shortcuts[0].title, english_group);
        let Check::Done { response, .. } = &handler.check else {
            panic!("the check should still be there");
        };
        assert_eq!(
            response,
            &diagnosis::to_diagnosis_response(&photos_in_the_way().diagnosis),
            "the checklist is worded again, not asked again"
        );
        let Timing::Done(Err(timing_error)) = &handler.timing else {
            panic!("the timing's answer should stay");
        };
        assert_eq!(
            timing_error.title,
            error::to_error_response(dto::Error::Timeout).title
        );
        assert!(matches!(&handler.report, Report::Ready(text) if *text == report));
        handler.tab = Tab::Troubleshooting;
        assert!(text_on_screen(
            &handler,
            crate::i18n::t("help.trouble.title")
        ));
        assert!(!text_on_screen(&handler, "If your phone won't connect"));
    }

    #[tokio::test]
    async fn the_troubleshooting_steps_are_all_there() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        for step in STEPS {
            assert!(text_on_screen(&handler, t(step)), "{step:?} is not shown");
        }
    }

    #[tokio::test]
    async fn the_about_tab_states_the_privacy_position_and_offers_the_links() {
        let (mut handler, _) = handler();
        handler.open(Phone::new("x", false));
        settle(&mut handler, Request::Show(Tab::About)).await;

        let mut ui = iced_test::simulator(handler.view());

        assert!(ui.find(t(PRIVACY).to_owned()).is_ok());
        assert!(
            ui.find(format!("open-mtp-client {}", env!("CARGO_PKG_VERSION")))
                .is_ok()
        );
        let _ = ui.click("Report a problem on GitHub".to_owned()).unwrap();
        let _ = ui.click("Buy me a coffee".to_owned()).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(
            messages.as_slice(),
            [
                Message::Request(Request::OpenLink(Link::NewIssue)),
                Message::Request(Request::OpenLink(Link::Support))
            ]
        ));
    }

    #[tokio::test]
    async fn every_tab_draws_in_both_themes() {
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("x", false));
        for tab in [Tab::Shortcuts, Tab::Troubleshooting, Tab::About] {
            settle(&mut handler, Request::Show(tab)).await;
            for theme in [iced::Theme::Light, iced::Theme::Dark] {
                let mut ui = iced_test::simulator(handler.view());
                assert!(ui.snapshot(&theme).is_ok(), "{tab:?}");
            }
        }
    }

    /// The privacy statement says the app has no network code. The build is what
    /// makes that true, so it is checked here: no HTTP client or TLS library in
    /// the dependency tree, and tokio's networking left switched off.
    #[test]
    fn the_privacy_statement_is_true_of_what_the_app_is_built_from() {
        let lock = include_str!("../../../../Cargo.lock");
        let manifest = include_str!("../../../../Cargo.toml");
        let forbidden = [
            "reqwest",
            "hyper",
            "ureq",
            "isahc",
            "surf",
            "attohttpc",
            "curl",
            "curl-sys",
            "awc",
            "minreq",
            "h2",
            "tungstenite",
            "tokio-tungstenite",
            "native-tls",
            "openssl",
            "openssl-sys",
            "rustls",
            "hickory-resolver",
            "trust-dns-resolver",
            "mixpanel",
            "sentry",
            "posthog-rs",
        ];

        for name in forbidden {
            assert!(
                !lock.contains(&format!("name = \"{name}\"")),
                "{name} is in the dependency tree, so \"no network code\" would be false"
            );
        }
        let tokio_line = manifest
            .lines()
            .find(|line| line.starts_with("tokio = "))
            .expect("the workspace declares tokio");
        assert!(
            !tokio_line.contains("\"net\"") && !tokio_line.contains("\"full\""),
            "tokio's networking is enabled: {tokio_line}"
        );
    }

    #[tokio::test]
    async fn when_the_phone_is_connected_the_check_finds_no_fault_to_fix() {
        // Photos is running and a Pixel is on USB, as in a failed connect, but
        // this app is connected, so neither is a problem.
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("Pixel 9, serial …1234, 2 storages", true));

        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        assert!(text_on_screen(
            &handler,
            "Connected: Pixel 9, serial …1234, 2 storages."
        ));
        assert!(text_on_screen(&handler, "Seen on USB: Pixel 7."));
        assert!(!text_on_screen(&handler, "Photos is using your phone."));
        assert!(!text_on_screen(
            &handler,
            "It is plugged in but isn't offering file transfer. On the phone, swipe down, tap the USB notification and choose “File transfer”."
        ));
        let mut ui = iced_test::simulator(handler.view());
        assert!(ui.find("Quit Photos and try again".to_owned()).is_err());
        assert!(ui.find("Check again".to_owned()).is_ok());
    }

    #[tokio::test]
    async fn a_connected_phone_has_nothing_to_quit_even_when_asked() {
        let (mut handler, fake) = handler_with(photos_in_the_way());
        handler.open(Phone::new("Pixel 9", true));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        let (messages, _) = drain(request(&mut handler, Request::QuitHolders)).await;

        assert!(messages.is_empty());
        assert!(fake.released.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn checking_again_while_connected_stays_a_connected_check() {
        let (mut handler, _) = handler_with(photos_in_the_way());
        handler.open(Phone::new("Pixel 9", true));
        settle(&mut handler, Request::Show(Tab::Troubleshooting)).await;

        settle(&mut handler, Request::CheckAgain).await;

        assert!(text_on_screen(&handler, "Connected: Pixel 9."));
    }

    #[tokio::test]
    async fn the_report_names_the_phone_whether_or_not_it_is_connected() {
        for connected in [true, false] {
            let (mut handler, _) = handler();
            handler.open(Phone::new("Pixel 9, 1 storage", connected));

            settle(&mut handler, Request::Show(Tab::About)).await;

            let Report::Ready(text) = &handler.report else {
                panic!("ready");
            };
            assert!(
                text.contains("Phone: Pixel 9, 1 storage\n"),
                "{connected}: {text}"
            );
        }
    }

    // What's new.

    #[test]
    fn after_an_update_whats_new_opens_with_the_new_releases_marked() {
        let (mut handler, _) = handler();
        let older = dto::Version {
            major: 0,
            minor: 0,
            patch: 1,
        };

        handler.open_whats_new(Phone::new("x", false), older);

        assert!(handler.is_open());
        assert_eq!(handler.tab, Tab::WhatsNew);
        assert!(text_on_screen(
            &handler,
            &format!("Version {}", env!("CARGO_PKG_VERSION"))
        ));
        assert!(text_on_screen(&handler, "New"));
        assert!(text_on_screen(&handler, t(whats_new::RELEASES[0].notes[0])));
    }

    #[test]
    fn opened_by_hand_whats_new_marks_nothing_new() {
        let (mut handler, _) = handler();
        handler.open_whats_new(
            Phone::new("x", false),
            dto::Version {
                major: 0,
                minor: 0,
                patch: 1,
            },
        );
        handler.close();
        handler.open(Phone::new("x", false));

        let clicked: Vec<Message> = {
            let mut ui = iced_test::simulator(handler.view());
            let _ = ui.click("What's new").unwrap();
            ui.into_messages().collect()
        };
        for message in clicked {
            let _ = handler.update(message);
        }

        assert_eq!(handler.tab, Tab::WhatsNew);
        assert!(text_on_screen(&handler, t(whats_new::RELEASES[0].notes[0])));
        assert!(!text_on_screen(&handler, "New"));
    }
}
