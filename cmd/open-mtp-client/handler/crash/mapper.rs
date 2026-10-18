use chrono::{DateTime, Local};
use internal::dto;

use super::response::Banner;

/// "18 Oct 2026, 08:30" in the Mac's time zone.
pub fn when(at_ms: u64) -> String {
    i64::try_from(at_ms)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map_or_else(
            || "at an unknown time".to_owned(),
            |at| {
                at.with_timezone(&Local)
                    .format("%-d %b %Y, %H:%M")
                    .to_string()
            },
        )
}

/// What to say at start-up about reports an earlier run left (`notes` newest
/// first, none of them told yet); `None` when there are none.
pub fn earlier(notes: &[dto::CrashNote]) -> Option<Banner> {
    let newest = notes.first()?;
    let title = match notes.len() {
        1 => format!(
            "open-mtp-client quit unexpectedly last time ({}).",
            when(newest.at_ms)
        ),
        n => format!(
            "open-mtp-client quit unexpectedly {n} times since you last looked (latest {}).",
            when(newest.at_ms)
        ),
    };
    Some(Banner {
        title,
        hint: "Transfers that were not finished, if any, are offered below. A report was saved on this Mac; it goes nowhere unless you send it.".to_owned(),
        can_show: newest.file.is_some(),
    })
}

/// What to say when the app's own code failed while it kept running.
pub fn stopped(note: &dto::CrashNote) -> Banner {
    let saved = if note.file.is_some() {
        "A report was saved on this Mac; it goes nowhere unless you send it."
    } else {
        "A report could not be saved."
    };
    Banner {
        title: format!(
            "Something inside open-mtp-client stopped working ({}).",
            when(note.at_ms)
        ),
        hint: format!(
            "The app is still running, but what it was doing may not have finished. If a pane or a dialog seems stuck, close it, or quit and open the app again. {saved}"
        ),
        can_show: note.file.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;

    use super::*;

    fn note(at_ms: u64, file: bool) -> dto::CrashNote {
        dto::CrashNote {
            at_ms,
            location: "internal/service/x.rs:1:1".to_owned(),
            reported: false,
            file: file.then(|| PathBuf::from("/crashes/crash-1.txt")),
        }
    }

    fn local_ms(hour: u32, minute: u32) -> u64 {
        Local
            .with_ymd_and_hms(2026, 10, 18, hour, minute, 0)
            .unwrap()
            .timestamp_millis()
            .try_into()
            .unwrap()
    }

    #[test]
    fn a_time_reads_as_the_macs_own_clock() {
        assert_eq!(when(local_ms(8, 30)), "18 Oct 2026, 08:30");
        assert_eq!(when(local_ms(23, 5)), "18 Oct 2026, 23:05");
        assert_eq!(when(u64::MAX), "at an unknown time");
    }

    #[test]
    fn nothing_is_said_when_nothing_happened() {
        assert_eq!(earlier(&[]), None);
    }

    #[test]
    fn one_crash_is_said_with_its_time_and_several_with_a_count() {
        let one = earlier(&[note(local_ms(8, 30), true)]).unwrap();
        assert_eq!(
            one.title,
            "open-mtp-client quit unexpectedly last time (18 Oct 2026, 08:30)."
        );
        assert!(one.can_show);

        let three = earlier(&[
            note(local_ms(9, 0), true),
            note(local_ms(8, 0), true),
            note(local_ms(7, 0), true),
        ])
        .unwrap();
        assert_eq!(
            three.title,
            "open-mtp-client quit unexpectedly 3 times since you last looked (latest 18 Oct 2026, 09:00)."
        );
    }

    #[test]
    fn a_report_that_is_not_there_cannot_be_shown() {
        assert!(!earlier(&[note(1, false)]).unwrap().can_show);
        let stopped = stopped(&note(local_ms(8, 31), false));
        assert!(!stopped.can_show);
        assert!(stopped.hint.contains("could not be saved"));
    }

    #[test]
    fn a_failure_in_a_running_app_says_it_may_not_have_finished() {
        let banner = stopped(&note(local_ms(8, 31), true));

        assert_eq!(
            banner.title,
            "Something inside open-mtp-client stopped working (18 Oct 2026, 08:31)."
        );
        assert!(banner.hint.contains("still running"));
        assert!(banner.hint.contains("may not have finished"));
        assert!(banner.can_show);
    }

    #[test]
    fn the_words_hold_no_place_in_the_code_and_no_path() {
        for banner in [earlier(&[note(1, true)]).unwrap(), stopped(&note(1, true))] {
            let all = format!("{} {}", banner.title, banner.hint);
            assert!(!all.contains("internal/"), "{all}");
            assert!(!all.contains("/crashes"), "{all}");
            assert!(!all.contains(".rs"), "{all}");
        }
    }
}
