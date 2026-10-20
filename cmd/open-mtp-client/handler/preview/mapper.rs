use internal::dto;

use crate::handler::format::{format_bytes, format_count};
use crate::i18n::{count, key, tf};

/// What phone files are fetched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Look,
    Open,
    Share,
}

impl Purpose {
    fn fetching(self) -> &'static str {
        match self {
            Purpose::Look => key("preview.fetching.look"),
            Purpose::Open => key("preview.fetching.open"),
            Purpose::Share => key("preview.fetching.share"),
        }
    }

    fn fetching_progress(self) -> &'static str {
        match self {
            Purpose::Look => key("preview.fetching_progress.look"),
            Purpose::Open => key("preview.fetching_progress.open"),
            Purpose::Share => key("preview.fetching_progress.share"),
        }
    }

    fn too_big_one(self) -> &'static str {
        match self {
            Purpose::Look => key("preview.too_big_one.look"),
            Purpose::Open => key("preview.too_big_one.open"),
            Purpose::Share => key("preview.too_big_one.share"),
        }
    }

    fn too_big_many(self) -> &'static str {
        match self {
            Purpose::Look => key("preview.too_big_many.look"),
            Purpose::Open => key("preview.too_big_many.open"),
            Purpose::Share => key("preview.too_big_many.share"),
        }
    }
}

/// What the banner says while phone files are fetched for `purpose`.
pub fn fetching_line(
    names: &[String],
    progress: Option<&dto::TransferProgress>,
    purpose: Purpose,
) -> String {
    let what = match names {
        [one] => one.clone(),
        many => count("preview.files", many.len(), &[]),
    };
    match progress.filter(|p| p.bytes_total > 0) {
        Some(p) => tf(
            purpose.fetching_progress(),
            &[
                ("what", &what),
                ("done", &format_bytes(p.bytes_done)),
                ("total", &format_bytes(p.bytes_total)),
            ],
        ),
        None => tf(purpose.fetching(), &[("what", &what)]),
    }
}

/// How far the fetch is, 0 to 1.
pub fn fraction(progress: Option<&dto::TransferProgress>) -> f32 {
    progress
        .filter(|p| p.bytes_total > 0)
        .map_or(0.0, |p| p.bytes_done as f32 / p.bytes_total as f32)
}

/// What was not shown and why, or `None` when everything was.
pub fn refusals(fetched: &dto::Fetched, purpose: Purpose) -> Option<String> {
    let mut lines = Vec::new();
    let most = format_bytes(dto::PREVIEW_LARGEST);
    if !fetched.too_big.is_empty() {
        lines.push(said_about(
            &fetched.too_big,
            purpose.too_big_one(),
            purpose.too_big_many(),
            &[("most", &most)],
        ));
    }
    if !fetched.folders.is_empty() {
        lines.push(said_about(
            &fetched.folders,
            key("preview.folder_one"),
            key("preview.folder_many"),
            &[],
        ));
    }
    if !fetched.failed.is_empty() {
        lines.push(said_about(
            &fetched.failed,
            key("preview.failed_one"),
            key("preview.failed_many"),
            &[],
        ));
    }
    if fetched.left_out > 0 {
        lines.push(tf(
            "preview.left_out",
            &[
                ("shown", &dto::PREVIEW_MOST.to_string()),
                ("left", &format_count(fetched.left_out)),
            ],
        ));
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

fn said_about(
    names: &[String],
    one: &'static str,
    many: &'static str,
    more_args: &[(&str, &str)],
) -> String {
    let mut args = more_args.to_vec();
    match names {
        [name] => {
            args.push(("name", name));
            tf(one, &args)
        }
        [first, rest @ ..] => {
            let more = format_count(rest.len());
            args.push(("first", first));
            args.push(("more", &more));
            tf(many, &args)
        }
        [] => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_says_what_is_fetched_and_how_far() {
        let progress = dto::TransferProgress {
            bytes_done: 1_000_000,
            bytes_total: 4_000_000,
            ..Default::default()
        };

        assert_eq!(
            fetching_line(&["a.mp4".into()], Some(&progress), Purpose::Look),
            "Getting a.mp4 from the phone for Quick Look… 1.0 MB of 4.0 MB"
        );
        assert_eq!(
            fetching_line(&["a".into(), "b".into()], None, Purpose::Look),
            "Getting 2 files from the phone for Quick Look…"
        );
        assert_eq!(
            fetching_line(&["a.pdf".into()], None, Purpose::Open),
            "Getting a.pdf from the phone to open it…"
        );
        assert_eq!(
            fetching_line(&["a.jpg".into(), "b.jpg".into()], None, Purpose::Share),
            "Getting 2 files from the phone to share…"
        );
        assert!((fraction(Some(&progress)) - 0.25).abs() < f32::EPSILON);
        assert_eq!(fraction(None), 0.0);
    }

    #[test]
    fn what_was_not_shown_is_said() {
        let fetched = dto::Fetched {
            too_big: vec!["huge.mov".into()],
            folders: vec!["Camera".into(), "Other".into()],
            failed: vec!["x.jpg".into()],
            left_out: 3,
            ..Default::default()
        };

        assert_eq!(
            refusals(&fetched, Purpose::Look).unwrap(),
            "“huge.mov” is too big to look at from the phone (1.0 GB at most): download it first. \
             “Camera” and 1 more are a folder: open it to look at what is inside. \
             “x.jpg” could not be fetched. \
             Only the first 20 are shown; 3 more were left out."
        );
        assert_eq!(refusals(&dto::Fetched::default(), Purpose::Look), None);
        let to_open = dto::Fetched {
            too_big: vec!["huge.mov".into()],
            ..Default::default()
        };
        assert_eq!(
            refusals(&to_open, Purpose::Open).unwrap(),
            "“huge.mov” is too big to open from the phone (1.0 GB at most): download it first."
        );
        assert_eq!(
            refusals(&to_open, Purpose::Share).unwrap(),
            "“huge.mov” is too big to share from the phone (1.0 GB at most): download it first."
        );
    }

    #[test]
    fn the_banner_and_the_refusals_are_whole_sentences_in_every_language() {
        use internal::dto::Language;

        let fetched = dto::Fetched {
            too_big: vec!["huge.mov".into(), "big.mov".into()],
            folders: vec!["Camera".into()],
            failed: vec!["x.jpg".into(), "y.jpg".into(), "z.jpg".into()],
            left_out: 1_234,
            ..Default::default()
        };
        let progress = dto::TransferProgress {
            bytes_done: 1_000_000,
            bytes_total: 4_000_000,
            ..Default::default()
        };

        for language in Language::ALL {
            crate::i18n::with_language(language, || {
                for purpose in [Purpose::Look, Purpose::Open, Purpose::Share] {
                    let said = refusals(&fetched, purpose).unwrap();
                    for word in ["huge.mov", "Camera", "x.jpg", "1.0 GB", "1,234", "2", "20"] {
                        assert!(said.contains(word), "{language:?} {purpose:?}: {said}");
                    }
                    assert!(!said.contains(['{', '}']), "{language:?}: {said}");
                    for names in [
                        vec!["a.mp4".to_owned()],
                        vec!["a".to_owned(), "b".to_owned()],
                    ] {
                        for progress in [None, Some(&progress)] {
                            let line = fetching_line(&names, progress, purpose);
                            assert!(!line.contains(['{', '}']), "{language:?}: {line}");
                            assert_eq!(line.contains("1.0 MB"), progress.is_some(), "{line}");
                        }
                    }
                }
            });
        }
    }
}
