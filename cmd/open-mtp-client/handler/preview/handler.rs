//! Quick Look from either pane, as Space does in Finder, and opening a file
//! in its Mac app, as a double-click does. Mac files go straight there; phone
//! files are fetched to the Mac first (a locked copy, marked as come from
//! elsewhere), with a banner below the panes saying how far it has got
//! (Escape stops it).

use std::sync::{Arc, Mutex};

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::widget::{button, column, container, progress_bar, row, space, text};
use iced::{Alignment, Element, Length, Task};
use internal::dto;

use super::mapper;
use crate::handler::error::{self, ErrorResponse};

pub type Progress = Box<dyn Fn(dto::TransferProgress) + Send + Sync>;

#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// Fetches phone files to the Mac to be looked at.
    async fn fetch(
        &self,
        request: dto::PreviewRequest,
        cancel: dto::Cancel,
        progress: Progress,
    ) -> Result<dto::Fetched, dto::Error>;

    /// Shows Mac files in Quick Look, or closes it if it is showing.
    fn look(&self, paths: &[String]) -> Result<dto::Look, dto::Error>;

    /// Opens a Mac file in its app.
    fn open(&self, path: &str) -> Result<(), dto::Error>;

    /// Shows the Mac's share menu for Mac files, beside `at` (points from
    /// the window's top left; `None`: its middle).
    fn share(&self, paths: &[String], at: Option<(f32, f32)>) -> Result<(), dto::Error>;
}

/// What a fetch is for, and for a share, where its menu shows.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Then {
    Look,
    Open,
    Share(Option<(f32, f32)>),
}

impl Then {
    fn purpose(self) -> mapper::Purpose {
        match self {
            Then::Look => mapper::Purpose::Look,
            Then::Open => mapper::Purpose::Open,
            Then::Share(_) => mapper::Purpose::Share,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// How far fetch `fetch` has got; one of an earlier fetch is dropped.
    Progress {
        fetch: u64,
        progress: dto::TransferProgress,
    },
    Fetched {
        fetch: u64,
        result: Result<dto::Fetched, ErrorResponse>,
    },
    /// The banner's ×.
    Dismiss,
}

impl Message {
    /// The error this message carries if it means the phone is gone.
    pub fn device_lost(&self) -> Option<&ErrorResponse> {
        match self {
            Message::Fetched {
                result: Err(error), ..
            } if error.device_lost => Some(error),
            _ => None,
        }
    }
}

struct Fetching {
    /// Which fetch this is: answers of one stopped earlier are dropped.
    id: u64,
    then: Then,
    names: Vec<String>,
    cancel: dto::Cancel,
    progress: Option<dto::TransferProgress>,
}

pub struct Handler {
    service: Arc<dyn Service>,
    fetching: Option<Fetching>,
    /// What was not shown, or why nothing could be.
    notice: Option<String>,
    fetches: u64,
}

impl Handler {
    pub fn new(service: Arc<dyn Service>) -> Self {
        Self {
            service,
            fetching: None,
            notice: None,
            fetches: 0,
        }
    }

    /// Phone files are being fetched (the phone's one session is in use).
    pub fn is_fetching(&self) -> bool {
        self.fetching.is_some()
    }

    /// Whether there is anything to show below the panes.
    pub fn is_visible(&self) -> bool {
        self.fetching.is_some() || self.notice.is_some()
    }

    /// Stops a fetch under way; what has arrived stays for next time.
    pub fn cancel(&mut self) {
        if let Some(fetching) = self.fetching.take() {
            fetching.cancel.cancel();
        }
    }

    /// Shows Mac files (folders too, as Finder does), or closes Quick Look.
    pub fn mac(&mut self, paths: Vec<String>) -> Task<Message> {
        self.notice = None;
        if paths.is_empty() {
            return Task::none();
        }
        if let Err(err) = self.service.look(&paths) {
            self.notice = Some(error::to_error_response(err).title);
        }
        Task::none()
    }

    /// Fetches phone files and shows them; Space again while fetching stops.
    pub fn phone(&mut self, request: dto::PreviewRequest) -> Task<Message> {
        if self.fetching.is_some() {
            self.cancel();
            self.notice = None;
            return Task::none();
        }
        self.fetch(request, Then::Look)
    }

    /// Opens a Mac file in its app.
    pub fn mac_open(&mut self, path: String) -> Task<Message> {
        self.notice = None;
        if let Err(err) = self.service.open(&path) {
            self.notice = Some(error::to_error_response(err).title);
        }
        Task::none()
    }

    /// Fetches a phone file and opens it in its app (a fetch under way for
    /// something else gives way).
    pub fn phone_open(&mut self, request: dto::PreviewRequest) -> Task<Message> {
        self.cancel();
        self.fetch(request, Then::Open)
    }

    /// Shows the Mac's share menu for Mac files.
    pub fn mac_share(&mut self, paths: Vec<String>, at: Option<(f32, f32)>) -> Task<Message> {
        self.notice = None;
        if let Err(err) = self.service.share(&paths, at) {
            self.notice = Some(error::to_error_response(err).title);
        }
        Task::none()
    }

    /// Fetches phone files and shows the share menu for the copies (a fetch
    /// under way for something else gives way).
    pub fn phone_share(
        &mut self,
        request: dto::PreviewRequest,
        at: Option<(f32, f32)>,
    ) -> Task<Message> {
        self.cancel();
        self.fetch(request, Then::Share(at))
    }

    fn fetch(&mut self, request: dto::PreviewRequest, then: Then) -> Task<Message> {
        self.notice = None;
        if request.names.is_empty() {
            return Task::none();
        }
        let cancel = dto::Cancel::new();
        self.fetches += 1;
        let fetch = self.fetches;
        self.fetching = Some(Fetching {
            id: fetch,
            then,
            names: request.names.clone(),
            cancel: cancel.clone(),
            progress: None,
        });
        let service = Arc::clone(&self.service);
        Task::run(
            iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
                let updates = Mutex::new(output.clone());
                let progress: Progress = Box::new(move |progress| {
                    if let Ok(mut sender) = updates.lock() {
                        let _ = sender.try_send(Message::Progress { fetch, progress });
                    }
                });
                let result = match service.fetch(request, cancel, progress).await {
                    // Stopped: nothing to show and nothing to say.
                    Err(dto::Error::Cancelled) => Ok(dto::Fetched::default()),
                    other => other.map_err(error::to_error_response),
                };
                let _ = output.send(Message::Fetched { fetch, result }).await;
            }),
            |message| message,
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Progress { fetch, progress } => {
                if let Some(fetching) = self.fetching.as_mut()
                    && fetching.id == fetch
                {
                    fetching.progress = Some(progress);
                }
            }
            Message::Fetched { fetch, result } => {
                // A fetch stopped (esc, or Space again) has nothing more to
                // say, even when another has started since.
                let Some(fetching) = self.fetching.take_if(|fetching| fetching.id == fetch) else {
                    return Task::none();
                };
                match result {
                    Ok(fetched) => {
                        self.notice = mapper::refusals(&fetched, fetching.then.purpose());
                        let done = match (fetching.then, fetched.paths.first()) {
                            (_, None) => Ok(()),
                            (Then::Look, Some(_)) => self.service.look(&fetched.paths).map(|_| ()),
                            (Then::Open, Some(path)) => self.service.open(path),
                            (Then::Share(at), Some(_)) => self.service.share(&fetched.paths, at),
                        };
                        if let Err(err) = done {
                            self.notice = Some(error::to_error_response(err).title);
                        }
                    }
                    Err(error) => self.notice = Some(error.title),
                }
            }
            Message::Dismiss => self.notice = None,
        }
        Task::none()
    }

    /// The banner below the panes: how far a fetch is, or what was not shown.
    pub fn view(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match (&self.fetching, &self.notice) {
            (Some(fetching), _) => column![
                row![
                    text(mapper::fetching_line(
                        &fetching.names,
                        fetching.progress.as_ref(),
                        fetching.then.purpose(),
                    ))
                    .size(13),
                    space::horizontal(),
                    text("esc to stop").size(12).style(text::secondary),
                ]
                .align_y(Alignment::Center),
                progress_bar(0.0..=1.0, mapper::fraction(fetching.progress.as_ref())).girth(4),
            ]
            .spacing(6)
            .into(),
            (None, Some(notice)) => row![
                text(notice).size(13).width(Length::Fill),
                button(text("×").size(13))
                    .style(button::text)
                    .on_press(Message::Dismiss),
            ]
            .align_y(Alignment::Center)
            .into(),
            (None, None) => return space().into(),
        };
        container(content)
            .padding([8, 12])
            .width(Length::Fill)
            .style(container::rounded_box)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::testing::{FakePreview, drain};

    fn request(names: &[&str]) -> dto::PreviewRequest {
        dto::PreviewRequest {
            storage_id: 5,
            folder: "/DCIM".into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    fn shows(handler: &Handler, words: &str) -> bool {
        iced_test::simulator(handler.view()).find(words).is_ok()
    }

    async fn settle(handler: &mut Handler, task: Task<Message>) {
        for message in drain(task).await.0 {
            let _ = handler.update(message);
        }
    }

    #[test]
    fn mac_files_go_straight_to_quick_look() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());

        let _ = handler.mac(vec!["/Users/me/a.jpg".into(), "/Users/me/Folder".into()]);
        let _ = handler.mac(Vec::new());

        assert_eq!(
            *service.looks.lock().unwrap(),
            [vec![
                "/Users/me/a.jpg".to_owned(),
                "/Users/me/Folder".to_owned()
            ]]
        );
        assert!(!handler.is_visible());
    }

    #[tokio::test]
    async fn phone_files_are_fetched_with_a_banner_then_looked_at() {
        let service = Arc::new(FakePreview {
            updates: vec![dto::TransferProgress {
                bytes_done: 1_000_000,
                bytes_total: 4_000_000,
                ..Default::default()
            }],
            ..FakePreview::default()
        });
        let mut handler = Handler::new(service.clone());

        let task = handler.phone(request(&["a.jpg"]));
        assert!(handler.is_fetching());
        assert!(shows(
            &handler,
            "Getting a.jpg from the phone for Quick Look…"
        ));
        let messages = drain(task).await.0;
        let _ = handler.update(messages[0].clone());
        assert!(shows(
            &handler,
            "Getting a.jpg from the phone for Quick Look… 1.0 MB of 4.0 MB"
        ));
        for message in messages.into_iter().skip(1) {
            let _ = handler.update(message);
        }

        assert!(!handler.is_fetching());
        assert_eq!(*service.fetches.lock().unwrap(), [request(&["a.jpg"])]);
        assert_eq!(
            *service.looks.lock().unwrap(),
            [vec!["/tmp/preview/a.jpg".to_owned()]]
        );
        assert!(!handler.is_visible(), "nothing left to say");
        let _ = handler.phone(request(&[]));
        assert!(!handler.is_fetching());
    }

    #[tokio::test]
    async fn what_was_not_shown_is_said_until_dismissed() {
        let mut handler = Handler::new(Arc::new(FakePreview::fetching(Ok(dto::Fetched {
            too_big: vec!["huge.mov".into()],
            ..dto::Fetched::default()
        }))));

        let task = handler.phone(request(&["huge.mov"]));
        settle(&mut handler, task).await;

        assert!(shows(
            &handler,
            "“huge.mov” is too big to look at from the phone (1.0 GB at most): download it first."
        ));
        let _ = handler.update(Message::Dismiss);
        assert!(!handler.is_visible());
    }

    #[tokio::test]
    async fn space_again_or_escape_stops_the_fetch_and_its_late_answer_is_ignored() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());
        let task = handler.phone(request(&["a.jpg"]));
        let cancel = handler.fetching.as_ref().unwrap().cancel.clone();

        let again = handler.phone(request(&["a.jpg"]));
        assert_eq!(again.units(), 0);
        settle(&mut handler, task).await;

        assert!(cancel.is_cancelled());
        assert!(
            service.looks.lock().unwrap().is_empty(),
            "stopped: nothing shown"
        );
        assert!(!handler.is_visible());

        let _ = handler.phone(request(&["a.jpg"]));
        handler.cancel();
        assert!(!handler.is_fetching());
    }

    #[tokio::test]
    async fn the_late_answer_of_a_stopped_fetch_does_not_take_the_next_ones_place() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());
        let first = handler.phone(request(&["a.jpg"]));
        let _ = handler.phone(request(&["a.jpg"]));
        let second = handler.phone(request(&["b.jpg"]));
        let first = drain(first).await.0;
        let second = drain(second).await.0;

        for message in first {
            let _ = handler.update(message);
        }
        assert!(handler.is_fetching(), "the second is still under way");
        assert!(service.looks.lock().unwrap().is_empty());
        for message in second {
            let _ = handler.update(message);
        }

        assert!(!handler.is_fetching());
        assert_eq!(service.looks.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_fetch_that_fails_or_a_look_that_cannot_open_says_why() {
        let mut lost = Handler::new(Arc::new(FakePreview::fetching(Err(
            dto::Error::Disconnected,
        ))));
        let task = lost.phone(request(&["a.jpg"]));
        let messages = drain(task).await.0;
        assert!(
            messages.iter().any(|m| m.device_lost().is_some()),
            "the app is told"
        );
        for message in messages {
            let _ = lost.update(message);
        }
        assert!(shows(
            &lost,
            &error::to_error_response(dto::Error::Disconnected).title
        ));

        let mut stopped = Handler::new(Arc::new(FakePreview::fetching(Err(dto::Error::Cancelled))));
        let task = stopped.phone(request(&["a.jpg"]));
        settle(&mut stopped, task).await;
        assert!(!stopped.is_visible(), "a stop says nothing");

        let mut broken = Handler::new(Arc::new(FakePreview {
            look_fails: true,
            ..FakePreview::default()
        }));
        let _ = broken.mac(vec!["/a.jpg".into()]);
        assert!(broken.is_visible());
        let task = broken.phone(request(&["a.jpg"]));
        settle(&mut broken, task).await;
        assert!(broken.is_visible());
        assert!(Message::Dismiss.device_lost().is_none());
    }

    #[tokio::test]
    async fn a_file_is_opened_in_its_app_from_either_side() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());

        let _ = handler.mac_open("/Users/me/a.pdf".into());
        let task = handler.phone_open(request(&["a.jpg"]));
        assert!(shows(&handler, "Getting a.jpg from the phone to open it…"));
        settle(&mut handler, task).await;

        assert_eq!(
            *service.opened.lock().unwrap(),
            ["/Users/me/a.pdf", "/tmp/preview/a.jpg"]
        );
        assert!(
            service.looks.lock().unwrap().is_empty(),
            "opened, not looked at"
        );
        assert!(!handler.is_visible());
    }

    #[tokio::test]
    async fn opening_takes_over_from_a_look_under_way() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());
        let look = handler.phone(request(&["a.jpg"]));
        let looked_cancel = handler.fetching.as_ref().unwrap().cancel.clone();

        let open = handler.phone_open(request(&["b.pdf"]));
        for message in drain(look).await.0 {
            let _ = handler.update(message);
        }
        assert!(handler.is_fetching(), "the look's answer is not the open's");
        settle(&mut handler, open).await;

        assert!(looked_cancel.is_cancelled());
        assert!(service.looks.lock().unwrap().is_empty());
        assert_eq!(service.opened.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn files_are_shared_from_either_side_the_phones_once_fetched() {
        let service = Arc::new(FakePreview::default());
        let mut handler = Handler::new(service.clone());

        let _ = handler.mac_share(vec!["/Users/me/a.pdf".into()], Some((10.0, 20.0)));
        let look = handler.phone(request(&["x.jpg"]));
        let looked_cancel = handler.fetching.as_ref().unwrap().cancel.clone();
        let task = handler.phone_share(request(&["a.jpg", "b.jpg"]), Some((30.0, 40.0)));
        assert!(
            looked_cancel.is_cancelled(),
            "a share takes over from a look"
        );
        assert!(shows(&handler, "Getting 2 files from the phone to share…"));
        for message in drain(look).await.0 {
            let _ = handler.update(message);
        }
        settle(&mut handler, task).await;

        assert_eq!(
            *service.shared.lock().unwrap(),
            [
                (vec!["/Users/me/a.pdf".to_owned()], Some((10.0, 20.0))),
                (vec!["/tmp/preview/a.jpg".to_owned()], Some((30.0, 40.0)))
            ]
        );
        assert!(
            service.looks.lock().unwrap().is_empty(),
            "shared, not looked at"
        );
        assert!(!handler.is_visible());
    }

    #[tokio::test]
    async fn a_share_that_cannot_show_or_a_file_too_big_says_so() {
        let failing = Arc::new(FakePreview {
            look_fails: true,
            ..FakePreview::default()
        });
        let mut handler = Handler::new(failing);
        let _ = handler.mac_share(vec!["/Users/me/a.pdf".into()], None);
        assert!(handler.is_visible());
        let task = handler.phone_share(request(&["a.jpg"]), None);
        settle(&mut handler, task).await;
        assert!(handler.is_visible());

        let mut big = Handler::new(Arc::new(FakePreview::fetching(Ok(dto::Fetched {
            too_big: vec!["huge.mov".into()],
            ..dto::Fetched::default()
        }))));
        let task = big.phone_share(request(&["huge.mov"]), None);
        settle(&mut big, task).await;
        assert!(shows(
            &big,
            "“huge.mov” is too big to share from the phone (1.0 GB at most): download it first."
        ));
    }

    #[tokio::test]
    async fn a_file_no_app_opens_says_so() {
        let service = Arc::new(FakePreview {
            look_fails: true,
            ..FakePreview::default()
        });
        let mut handler = Handler::new(service);

        let _ = handler.mac_open("/Users/me/a.xyz".into());
        assert!(handler.is_visible());
        let task = handler.phone_open(request(&["a.xyz"]));
        settle(&mut handler, task).await;
        assert!(handler.is_visible());
    }
}
