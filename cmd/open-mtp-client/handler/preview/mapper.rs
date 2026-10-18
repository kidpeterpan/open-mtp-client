use internal::dto;

use crate::handler::format::{format_bytes, format_count};

/// What phone files are fetched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Look,
    Open,
    Share,
}

impl Purpose {
    /// "Getting a.jpg from the phone …"
    fn why(self) -> &'static str {
        match self {
            Purpose::Look => "for Quick Look",
            Purpose::Open => "to open it",
            Purpose::Share => "to share",
        }
    }

    /// "… is too big to … from the phone"
    fn doing(self) -> &'static str {
        match self {
            Purpose::Look => "look at",
            Purpose::Open => "open",
            Purpose::Share => "share",
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
        many => format!("{} files", format_count(many.len())),
    };
    let why = purpose.why();
    match progress.filter(|p| p.bytes_total > 0) {
        Some(p) => format!(
            "Getting {what} from the phone {why}… {} of {}",
            format_bytes(p.bytes_done),
            format_bytes(p.bytes_total)
        ),
        None => format!("Getting {what} from the phone {why}…"),
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
    let doing = purpose.doing();
    let mut lines = Vec::new();
    // "“a.jpg”" and "is", or "“a.jpg” and 2 more" and "are".
    let named = |names: &[String]| match names {
        [one] => (format!("“{one}”"), "is"),
        [first, rest @ ..] => (format!("“{first}” and {} more", rest.len()), "are"),
        [] => (String::new(), ""),
    };
    if !fetched.too_big.is_empty() {
        let (who, is) = named(&fetched.too_big);
        lines.push(format!(
            "{who} {is} too big to {doing} from the phone ({} at most): download it first.",
            format_bytes(dto::PREVIEW_LARGEST)
        ));
    }
    if !fetched.folders.is_empty() {
        let (who, is) = named(&fetched.folders);
        lines.push(format!(
            "{who} {is} a folder: open it to look at what is inside."
        ));
    }
    if !fetched.failed.is_empty() {
        let (who, _) = named(&fetched.failed);
        lines.push(format!("{who} could not be fetched."));
    }
    if fetched.left_out > 0 {
        lines.push(format!(
            "Only the first {} are shown; {} more were left out.",
            dto::PREVIEW_MOST,
            format_count(fetched.left_out)
        ));
    }
    (!lines.is_empty()).then(|| lines.join(" "))
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
}
