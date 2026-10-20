use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use iced::{Element, Rectangle, Theme};
use iced_test::Simulator;
use iced_test::selector::Candidate;
use internal::dto::Language;

use super::SOURCES;
use super::catalog::Catalog;
use crate::placement::SMALLEST;

const ONE_LINE_AT_MOST: f32 = 27.0;
const LABEL_PREFIXES: [&str; 3] = ["label.", "help.tab.", "places.tag."];
const KIND_WORDS: &str = "label.kind.";

pub fn folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/language-check")
}

pub fn clear_folder() {
    let _ = fs::remove_dir_all(folder());
}

fn picture_font(language: Language) -> iced::Font {
    match language {
        Language::Thai => iced::Font::with_name("Thonburi"),
        _ => iced::Font::DEFAULT,
    }
}

fn folder_name(language: Language) -> &'static str {
    match language {
        Language::English => "en",
        Language::Thai => "th",
        Language::Japanese => "ja",
        Language::ChineseSimplified => "zh-Hans",
    }
}

fn source_index(language: Language) -> usize {
    Language::ALL
        .iter()
        .position(|candidate| *candidate == language)
        .expect("every language has a file")
}

fn labels_with_keys(language: Language, wanted: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let catalog = Catalog::parse(SOURCES[source_index(language)]).expect("the file parses");
    catalog
        .keys()
        .into_iter()
        .filter(|key| wanted(key))
        .filter_map(|key| catalog.get(key)?.plain())
        .map(str::to_owned)
        .collect()
}

fn one_line_labels(language: Language) -> BTreeSet<String> {
    labels_with_keys(language, |key| {
        LABEL_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
    })
}

fn labels_to_see_whole(language: Language) -> BTreeSet<String> {
    labels_with_keys(language, |key| {
        LABEL_PREFIXES.iter().any(|prefix| key.starts_with(prefix)) && !key.starts_with(KIND_WORDS)
    })
}

fn catalog_areas() -> BTreeSet<&'static str> {
    Catalog::parse(SOURCES[0])
        .expect("the English file parses")
        .keys()
        .into_iter()
        .filter_map(|key| key.split('.').next())
        .collect()
}

fn looks_like_a_key(text: &str, areas: &BTreeSet<&'static str>) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() >= 2
        && areas.contains(parts[0])
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.chars().all(|letter| {
                    letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_'
                })
        })
}

struct Drawn {
    content: String,
    bounds: Rectangle,
    visible_bounds: Option<Rectangle>,
}

impl Drawn {
    fn is_cut_off(&self) -> bool {
        self.visible_bounds.is_some_and(|visible| {
            visible.width < self.bounds.width - 1.0 || visible.height < self.bounds.height - 1.0
        })
    }
}

fn texts_drawn<Message>(ui: &mut Simulator<'_, Message>) -> Vec<Drawn> {
    let mut found = Vec::new();
    let _ = ui.find(|candidate: Candidate<'_>| -> Option<()> {
        if let Candidate::Text {
            content,
            bounds,
            visible_bounds,
            ..
        } = candidate
        {
            found.push(Drawn {
                content: content.to_owned(),
                bounds,
                visible_bounds,
            });
        }
        None
    });
    found
}

pub fn look_at<'a, Message: 'a>(
    language: Language,
    screen: &str,
    element: Element<'a, Message>,
) -> Vec<String> {
    let settings = iced::Settings {
        default_font: picture_font(language),
        ..iced::Settings::default()
    };
    let mut ui = Simulator::with_size(settings, SMALLEST, element);
    let texts = texts_drawn(&mut ui);
    let labels = one_line_labels(language);
    let to_see_whole = labels_to_see_whole(language);
    let areas = catalog_areas();
    let mut problems = Vec::new();

    for drawn in &texts {
        let said = drawn.content.as_str();
        if labels.contains(said) && drawn.bounds.height > ONE_LINE_AT_MOST {
            problems.push(format!(
                "{language:?} {screen}: “{said}” is meant for one line and is {:.0} high",
                drawn.bounds.height
            ));
        }
        if to_see_whole.contains(said) && drawn.is_cut_off() {
            problems.push(format!(
                "{language:?} {screen}: “{said}” is meant to be read whole and is cut off"
            ));
        }
        if looks_like_a_key(said, &areas) {
            problems.push(format!(
                "{language:?} {screen}: “{said}” is a message key, not wording"
            ));
        }
    }

    let picture = folder().join(folder_name(language)).join(screen);
    match ui.snapshot(&Theme::Light) {
        Ok(snapshot) => {
            if let Err(error) = snapshot.matches_image(&picture) {
                problems.push(format!(
                    "{language:?} {screen}: the picture was not saved: {error}"
                ));
            }
        }
        Err(error) => problems.push(format!("{language:?} {screen}: no picture: {error}")),
    }
    problems
}
