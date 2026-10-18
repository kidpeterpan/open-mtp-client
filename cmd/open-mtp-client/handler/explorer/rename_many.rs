//! Renaming several items at once, as Finder's Rename does with more than one
//! selected: replace text, add text, or one name and a number. The dialog
//! shows what every item would be called and holds back the lot while any
//! new name is taken or refused; then each item is renamed in turn, one
//! request each, never over anything, and one ⌘Z puts every name back.

use std::sync::Arc;

use iced::widget::{Id, column, container, radio, row, text, text_input};
use iced::{Alignment, Element, Length};
use internal::dto;

use super::handler::{Message, Service};
use super::request::{ManyField, ManyMode, Request};
use crate::handler::error::{self, ErrorResponse};
use crate::handler::format::format_count;

/// Rows of new names the dialog shows: enough to see the pattern, never a
/// row per item of a big selection.
pub const PREVIEW_ROWS: usize = 6;

/// The dialog's state: what is renamed, how, and what that would give.
#[derive(Debug, Clone, PartialEq)]
pub struct Many {
    /// The selected items (name, is a folder), in the order the pane shows.
    pub items: Vec<(String, bool)>,
    /// Every name in the folder, shown or not: what a new name must not be.
    pub folder: Vec<String>,
    pub mode: ManyMode,
    pub find: String,
    pub with: String,
    pub text: String,
    pub at_end: bool,
    pub base: String,
    pub start: String,
    /// What each item would be called now.
    pub plan: Vec<dto::Renaming>,
    /// How many are done, while it runs.
    pub done: Option<usize>,
}

impl Many {
    pub fn new(items: Vec<(String, bool)>, folder: Vec<String>) -> Self {
        Self {
            items,
            folder,
            mode: ManyMode::Replace,
            find: String::new(),
            with: String::new(),
            text: String::new(),
            at_end: false,
            base: String::new(),
            start: "1".to_owned(),
            plan: Vec::new(),
            done: None,
        }
    }

    /// The pattern the fields describe; `None` while the number to start at
    /// isn't a whole number.
    pub fn pattern(&self) -> Option<dto::RenamePattern> {
        Some(match self.mode {
            ManyMode::Replace => dto::RenamePattern::Replace {
                find: self.find.clone(),
                with: self.with.clone(),
            },
            ManyMode::Add => dto::RenamePattern::Add {
                text: self.text.clone(),
                at_end: self.at_end,
            },
            ManyMode::Number => dto::RenamePattern::Number {
                base: self.base.clone(),
                start: self.start.trim().parse().ok()?,
            },
        })
    }

    pub fn set(&mut self, field: ManyField, value: String) {
        match field {
            ManyField::Find => self.find = value,
            ManyField::With => self.with = value,
            ManyField::Text => self.text = value,
            ManyField::Base => self.base = value,
            ManyField::Start => self.start = value,
        }
    }

    /// Plans again with `service`'s rules: after every keystroke, one pass.
    pub fn replan(&mut self, service: &dyn Service) {
        self.plan = match self.pattern() {
            Some(pattern) => service.plan_renames(&self.items, &self.folder, &pattern),
            None => Vec::new(),
        };
    }

    /// The renames to make: those that change a name.
    pub fn renames(&self) -> Vec<(String, String)> {
        self.plan
            .iter()
            .filter(|renaming| renaming.changes())
            .map(|renaming| (renaming.from.clone(), renaming.to.clone()))
            .collect()
    }

    /// Nothing held back, and something to do.
    pub fn ready(&self) -> bool {
        self.plan.iter().all(|renaming| renaming.problem.is_none())
            && self.plan.iter().any(dto::Renaming::changes)
    }
}

/// Why a new name is held back, in a few words.
pub fn problem_words(problem: dto::RenameProblem) -> &'static str {
    match problem {
        dto::RenameProblem::Invalid(dto::NameProblem::Empty) => "no name",
        dto::RenameProblem::Invalid(dto::NameProblem::IllegalCharacters) => {
            "has a character names can't have"
        }
        dto::RenameProblem::Invalid(dto::NameProblem::Reserved) => "not a name",
        dto::RenameProblem::Taken => "taken in this folder",
        dto::RenameProblem::Twice => "given to two items",
    }
}

/// What the dialog says under the preview: how many change, or why none can.
pub fn summary(many: &Many) -> String {
    if many.pattern().is_none() {
        return "Start at a whole number.".to_owned();
    }
    let held = many.plan.iter().filter(|r| r.problem.is_some()).count();
    let changes = many.plan.iter().filter(|r| r.changes()).count();
    match (held, changes) {
        (0, 0) => "Nothing would change yet.".to_owned(),
        (0, n) => format!("{} will be renamed.", items(n)),
        (n, _) => format!(
            "{} can't be renamed so: change the pattern to rename any.",
            items(n)
        ),
    }
}

fn items(n: usize) -> String {
    match n {
        1 => "1 item".to_owned(),
        n => format!("{} items", format_count(n)),
    }
}

/// The rows to show: the held-back ones first (they are what needs fixing),
/// then the rest in order, at most `PREVIEW_ROWS`, and how many are left out.
pub fn preview(plan: &[dto::Renaming]) -> (Vec<&dto::Renaming>, usize) {
    let shown: Vec<&dto::Renaming> = plan
        .iter()
        .filter(|r| r.problem.is_some())
        .chain(plan.iter().filter(|r| r.problem.is_none()))
        .take(PREVIEW_ROWS)
        .collect();
    let left = plan.len() - shown.len();
    (shown, left)
}

/// "Rename 12 items".
pub fn title(many: &Many) -> String {
    format!("Rename {}", items(many.items.len()))
}

/// How a run of renames went.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Each rename made: the old name and the name it has now.
    pub done: Vec<(String, String)>,
    /// Items the volume refused, and why.
    pub failures: Vec<(String, ErrorResponse)>,
    /// What stopped the run before the end: a cancel or a lost phone.
    pub stopped: Option<ErrorResponse>,
}

/// Renames each pair in turn, saying after each how many are done. A refusal
/// is that item's; a lost phone or a cancel stops the rest.
pub async fn rename_each(
    service: Arc<dyn Service>,
    volume_id: u64,
    folder: String,
    renames: Vec<(String, String)>,
    cancel: dto::Cancel,
    progress: impl Fn(usize),
) -> Outcome {
    let mut outcome = Outcome {
        done: Vec::new(),
        failures: Vec::new(),
        stopped: None,
    };
    for (at, (from, to)) in renames.into_iter().enumerate() {
        if cancel.is_cancelled() {
            outcome.stopped = Some(error::to_error_response(dto::Error::Cancelled));
            break;
        }
        match service.rename(volume_id, &folder, &from, &to).await {
            Ok(name) => outcome.done.push((from, name)),
            Err(err) => {
                let response = error::to_error_response(err);
                if response.device_lost {
                    outcome.stopped = Some(response);
                    break;
                }
                outcome.failures.push((from, response));
            }
        }
        progress(at + 1);
    }
    outcome
}

/// The banner a run leaves: how many were renamed, and what wasn't.
pub fn notice(outcome: &Outcome, asked: usize) -> super::response::NoticeResponse {
    let renamed = outcome.done.len();
    let title = if renamed == asked {
        format!("Renamed {}", items(renamed))
    } else {
        format!("Renamed {} of {}", format_count(renamed), items(asked))
    };
    let mut details: Vec<String> = outcome
        .failures
        .iter()
        .take(3)
        .map(|(name, error)| format!("“{name}”: {}", error.title))
        .collect();
    if outcome.failures.len() > 3 {
        details.push(format!("and {} more", outcome.failures.len() - 3));
    }
    if let Some(stopped) = &outcome.stopped {
        details.push(stopped.title.clone());
    }
    super::response::NoticeResponse {
        title,
        problem: renamed < asked,
        details,
    }
}

/// The dialog's fields and preview, above its buttons.
pub fn fields<'a>(many: &'a Many, busy: bool, first: Id) -> Element<'a, Message> {
    let send = |request| Message::Request(request);
    let mode = |label, choice| {
        radio(label, choice, Some(many.mode), move |m| {
            send(Request::RenameMode(m))
        })
        .size(14)
        .text_size(13)
    };
    let input = |hint: &'a str, value: &'a str, field: ManyField| {
        text_input(hint, value)
            .padding(6)
            .on_input_maybe(
                (!busy).then_some(move |value| send(Request::RenameField(field, value))),
            )
            .on_submit(send(Request::Submit))
    };
    let inputs: Element<'a, Message> = match many.mode {
        ManyMode::Replace => row![
            input("Find", &many.find, ManyField::Find).id(first),
            input("Replace with", &many.with, ManyField::With),
        ]
        .spacing(8)
        .into(),
        ManyMode::Add => column![
            input("Text to add", &many.text, ManyField::Text).id(first),
            row![
                radio("before the name", false, Some(many.at_end), |end| send(
                    Request::RenameAtEnd(end)
                ))
                .size(14)
                .text_size(13),
                radio("after the name", true, Some(many.at_end), |end| send(
                    Request::RenameAtEnd(end)
                ))
                .size(14)
                .text_size(13),
            ]
            .spacing(16),
        ]
        .spacing(8)
        .into(),
        ManyMode::Number => row![
            input("Name", &many.base, ManyField::Base).id(first),
            container(input("Start at", &many.start, ManyField::Start)).width(90),
        ]
        .spacing(8)
        .into(),
    };

    let (shown, left) = preview(&many.plan);
    let mut rows = column![].spacing(3);
    for renaming in shown {
        let new = match renaming.problem {
            Some(problem) => text(format!("{} — {}", renaming.to, problem_words(problem)))
                .size(12)
                .style(text::danger),
            None => text(&renaming.to).size(12),
        };
        rows = rows.push(
            row![
                container(text(&renaming.from).size(12).style(text::secondary))
                    .width(Length::FillPortion(1))
                    .clip(true),
                text("→").size(12).style(text::secondary),
                container(new).width(Length::FillPortion(1)).clip(true),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    if left > 0 {
        rows = rows.push(
            text(format!("and {} more", format_count(left)))
                .size(12)
                .style(text::secondary),
        );
    }
    let status = match many.done {
        Some(done) => format!(
            "Renaming {} of {}…",
            format_count(done),
            format_count(many.renames_planned())
        ),
        None => summary(many),
    };

    column![
        row![
            mode("Replace text", ManyMode::Replace),
            mode("Add text", ManyMode::Add),
            mode("Name and number", ManyMode::Number),
        ]
        .spacing(14),
        inputs,
        container(rows)
            .padding(8)
            .width(Length::Fill)
            .style(container::bordered_box),
        text(status).size(12).style(text::secondary),
    ]
    .spacing(10)
    .into()
}

impl Many {
    /// How many renames the run was started with.
    pub fn renames_planned(&self) -> usize {
        self.plan.iter().filter(|r| r.changes()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renaming(from: &str, to: &str, problem: Option<dto::RenameProblem>) -> dto::Renaming {
        dto::Renaming {
            from: from.into(),
            to: to.into(),
            problem,
        }
    }

    fn many(plan: Vec<dto::Renaming>) -> Many {
        let mut many = Many::new(vec![("a.jpg".into(), false)], vec!["a.jpg".into()]);
        many.plan = plan;
        many
    }

    #[test]
    fn the_fields_make_the_pattern() {
        let mut many = Many::new(Vec::new(), Vec::new());
        many.set(ManyField::Find, "IMG_".into());
        many.set(ManyField::With, "Trip ".into());
        assert_eq!(
            many.pattern(),
            Some(dto::RenamePattern::Replace {
                find: "IMG_".into(),
                with: "Trip ".into()
            })
        );
        many.mode = ManyMode::Add;
        many.set(ManyField::Text, "x".into());
        many.at_end = true;
        assert_eq!(
            many.pattern(),
            Some(dto::RenamePattern::Add {
                text: "x".into(),
                at_end: true
            })
        );
        many.mode = ManyMode::Number;
        many.set(ManyField::Base, "Trip".into());
        many.set(ManyField::Start, " 7 ".into());
        assert_eq!(
            many.pattern(),
            Some(dto::RenamePattern::Number {
                base: "Trip".into(),
                start: 7
            })
        );
        many.set(ManyField::Start, "seven".into());
        assert_eq!(many.pattern(), None);
        assert_eq!(summary(&many), "Start at a whole number.");
    }

    #[test]
    fn it_is_ready_only_with_something_to_do_and_nothing_held_back() {
        assert!(many(vec![renaming("a", "b", None)]).ready());
        assert!(
            !many(vec![renaming("a", "a", None)]).ready(),
            "nothing changes"
        );
        let held = many(vec![
            renaming("a", "b", None),
            renaming("c", "d", Some(dto::RenameProblem::Taken)),
        ]);
        assert!(!held.ready());
        assert_eq!(held.renames(), [("a".to_owned(), "b".to_owned())]);
        assert_eq!(
            summary(&held),
            "1 item can't be renamed so: change the pattern to rename any."
        );
        assert_eq!(
            summary(&many(vec![
                renaming("a", "b", None),
                renaming("c", "d", None)
            ])),
            "2 items will be renamed."
        );
        assert_eq!(summary(&many(vec![])), "Nothing would change yet.");
    }

    #[test]
    fn the_preview_puts_what_needs_fixing_first_and_stays_short() {
        let mut plan: Vec<dto::Renaming> = (0..5_000)
            .map(|n| renaming(&format!("{n}.jpg"), &format!("x {n}.jpg"), None))
            .collect();
        plan[4_000].problem = Some(dto::RenameProblem::Twice);

        let (shown, left) = preview(&plan);

        assert_eq!(shown.len(), PREVIEW_ROWS);
        assert_eq!(shown[0].from, "4000.jpg");
        assert_eq!(shown[1].from, "0.jpg");
        assert_eq!(left, 5_000 - PREVIEW_ROWS);
    }

    #[test]
    fn every_problem_has_its_words() {
        for problem in [
            dto::RenameProblem::Invalid(dto::NameProblem::Empty),
            dto::RenameProblem::Invalid(dto::NameProblem::IllegalCharacters),
            dto::RenameProblem::Invalid(dto::NameProblem::Reserved),
            dto::RenameProblem::Taken,
            dto::RenameProblem::Twice,
        ] {
            assert!(!problem_words(problem).is_empty());
        }
    }

    #[test]
    fn the_banner_says_how_many_and_what_was_not_renamed() {
        let all = Outcome {
            done: vec![("a".into(), "b".into()), ("c".into(), "d".into())],
            failures: Vec::new(),
            stopped: None,
        };
        let banner = notice(&all, 2);
        assert_eq!(banner.title, "Renamed 2 items");
        assert!(!banner.problem && banner.details.is_empty());

        let refused = |name: &str| {
            (
                name.to_owned(),
                error::to_error_response(dto::Error::NameTaken(name.into())),
            )
        };
        let some = Outcome {
            done: vec![("a".into(), "b".into())],
            failures: (0..5).map(|n| refused(&format!("{n}.jpg"))).collect(),
            stopped: Some(error::to_error_response(dto::Error::Disconnected)),
        };
        let banner = notice(&some, 9);
        assert_eq!(banner.title, "Renamed 1 of 9 items");
        assert!(banner.problem);
        assert_eq!(banner.details.len(), 5, "three, the rest counted, the stop");
        assert!(banner.details[0].starts_with("“0.jpg”: "));
        assert_eq!(banner.details[3], "and 2 more");
    }
}
