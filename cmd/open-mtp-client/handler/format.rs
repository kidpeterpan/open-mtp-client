//! Formatting shared by every handler.

use chrono::{Datelike, NaiveDate};
use internal::dto::{
    Abilities, DateRange, Day, DaySpan, FileKind, ReadTiming, Timestamp, UsbSpeed,
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A kind of file, as the search and the pane's filter offer it; `None` is
/// every kind.
pub fn kind_label(kind: Option<FileKind>) -> &'static str {
    match kind {
        None => "All",
        Some(FileKind::Photo) => "Photos",
        Some(FileKind::Video) => "Videos",
        Some(FileKind::Music) => "Music",
        Some(FileKind::Document) => "Documents",
        Some(FileKind::Archive) => "Archives",
    }
}

/// A date filter's choice, as the pane's filter and the search offer it;
/// `None` is any date.
pub fn date_label(range: Option<DateRange>) -> &'static str {
    match range {
        None => "Any date",
        Some(DateRange::Today) => "Today",
        Some(DateRange::Yesterday) => "Yesterday",
        Some(DateRange::LastWeek) => "Last 7 days",
        Some(DateRange::LastMonth) => "Last 30 days",
        Some(DateRange::ThisYear) => "This year",
        Some(DateRange::OlderThanAYear) => "Over a year ago",
    }
}

/// The days `range` keeps, counted back from `today` (the Mac's): files are
/// compared by the day the pane shows, so a phone's zoneless time is never
/// moved by a time zone.
pub fn date_span(range: DateRange, today: NaiveDate) -> DaySpan {
    let day = |date: NaiveDate| Day {
        year: u16::try_from(date.year()).unwrap_or(0),
        month: date.month() as u8,
        day: date.day() as u8,
    };
    let back = |days: u64| today.checked_sub_days(chrono::Days::new(days)).map(day);
    match range {
        DateRange::Today => DaySpan {
            from: Some(day(today)),
            until: Some(day(today)),
        },
        DateRange::Yesterday => DaySpan {
            from: back(1),
            until: back(1),
        },
        DateRange::LastWeek => DaySpan {
            from: back(6),
            until: Some(day(today)),
        },
        DateRange::LastMonth => DaySpan {
            from: back(29),
            until: Some(day(today)),
        },
        DateRange::ThisYear => DaySpan {
            from: today.with_ordinal(1).map(day),
            until: None,
        },
        DateRange::OlderThanAYear => DaySpan {
            from: None,
            until: today
                .checked_sub_months(chrono::Months::new(12))
                .and_then(|year_ago| year_ago.pred_opt())
                .map(day),
        },
    }
}

/// A USB link as the sidebar names it: "USB 2", "USB 3".
pub fn link_label(link: UsbSpeed) -> &'static str {
    match link {
        UsbSpeed::Low | UsbSpeed::Full => "USB 1",
        UsbSpeed::High => "USB 2",
        UsbSpeed::Super => "USB 3",
        UsbSpeed::SuperPlus => "USB 3 (10 Gb/s)",
    }
}

/// For a link slower than USB 3, what it means for copying and what would
/// help; `None` for a fast link or one not known.
pub fn slow_link_note(link: Option<UsbSpeed>) -> Option<String> {
    let link = link.filter(|link| link.is_slow())?;
    let most = match link {
        UsbSpeed::High => "about 35 MB/s",
        UsbSpeed::Full => "about 1 MB/s",
        _ => "well under 1 MB/s",
    };
    Some(format!(
        "Connected at {} speed ({} Mb/s): {most} at most. A USB 3 cable and port would be faster.",
        link_label(link),
        link.megabits()
    ))
}

/// Today on the Mac.
pub fn local_today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// A count with thousands separators: 3,400.
/// Whether `text` holds a character that would change what it says once
/// pasted or read line by line: a control character (C0 or C1, a tab and a
/// line break among them), a Unicode line or paragraph separator, or a
/// character that turns the direction of text (which can make a path read
/// as another).
pub fn has_control(text: &str) -> bool {
    text.chars().any(is_odd)
}

fn is_odd(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200E}' | '\u{200F}')
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// `text` with those characters written out (`\t`, `\n`, `\u{202e}`), so a
/// name a phone gave can't add a line or a column, or turn round, where it
/// is written down.
pub fn escape_odd(text: &str) -> String {
    text.chars()
        .map(|c| {
            if is_odd(c) {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Decimal units, matching how phones and macOS Finder report storage.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Finder-style, relative to `today`: "Today, 12:33", "Yesterday, 09:05",
/// "3 Aug, 11:40" this year, "11 Jul 2025" before. Shown as the device wrote it
/// when the date is not a real calendar day.
pub fn format_timestamp(time: &Timestamp, today: NaiveDate) -> String {
    let month = MONTHS
        .get(usize::from(time.month).wrapping_sub(1))
        .copied()
        .unwrap_or("???");
    let clock = format!("{:02}:{:02}", time.hour, time.minute);

    let Some(date) = NaiveDate::from_ymd_opt(
        i32::from(time.year),
        u32::from(time.month),
        u32::from(time.day),
    ) else {
        return format!("{} {month} {}, {clock}", time.day, time.year);
    };

    if date == today {
        format!("Today, {clock}")
    } else if Some(date) == today.pred_opt() {
        format!("Yesterday, {clock}")
    } else if date.year() == today.year() {
        format!("{} {month}, {clock}", time.day)
    } else {
        format!("{} {month} {}", time.day, time.year)
    }
}

/// "8 s", "3 min", "1 h 5 min": rounded, for estimates and elapsed time.
pub fn format_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", (secs + 30) / 60),
        _ => {
            let minutes = (secs + 30) / 60;
            match minutes % 60 {
                0 => format!("{} h", minutes / 60),
                rest => format!("{} h {rest} min", minutes / 60),
            }
        }
    }
}

/// How long something took, to the second: "11 s", "1 min 11 s",
/// "2 h 5 min" (seconds stop counting past an hour).
pub fn format_elapsed(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => match secs % 60 {
            0 => format!("{} min", secs / 60),
            rest => format!("{} min {rest} s", secs / 60),
        },
        _ => {
            let minutes = secs / 60;
            match minutes % 60 {
                0 => format!("{} h", minutes / 60),
                rest => format!("{} h {rest} min", minutes / 60),
            }
        }
    }
}

/// What a phone can and can't do, in plain words: (can, can't).
pub fn abilities_in_words(abilities: &Abilities) -> (Vec<&'static str>, Vec<&'static str>) {
    let resume = if abilities.partial_reads_64 {
        (true, "resume a stopped download")
    } else if abilities.partial_reads {
        (true, "resume a stopped download under 4 GB")
    } else {
        (false, "resume a stopped download")
    };
    let all = [
        (abilities.copy, "copy on the phone"),
        (abilities.moves, "move on the phone"),
        (abilities.rename, "rename"),
        resume,
        (abilities.thumbnails, "make its own thumbnails"),
        (abilities.prop_list, "read a folder's details at once"),
        (abilities.events, "say when its files change"),
    ];
    let pick = |wanted: bool| {
        all.iter()
            .filter(|(can, _)| *can == wanted)
            .map(|(_, words)| *words)
            .collect()
    };
    (pick(true), pick(false))
}

/// The operations a phone lists, by their MTP names, for a support report.
pub fn operations_line(abilities: &Abilities) -> String {
    let yes = |on: bool| if on { "yes" } else { "no" };
    format!(
        "CopyObject {}, MoveObject {}, rename {}, GetPartialObject {}, GetPartialObject64 {}, GetThumb {}, GetObjectPropList {}, events {}",
        yes(abilities.copy),
        yes(abilities.moves),
        yes(abilities.rename),
        yes(abilities.partial_reads),
        yes(abilities.partial_reads_64),
        yes(abilities.thumbnails),
        yes(abilities.prop_list),
        yes(abilities.events),
    )
}

/// "312 items in 1.4 s".
pub fn format_reading(timing: &ReadTiming) -> String {
    let items = match timing.items {
        1 => "1 item".to_owned(),
        n => format!("{} items", format_count(n)),
    };
    let secs = timing.took.as_secs_f64();
    let took = if secs < 1.0 {
        format!("{} ms", timing.took.as_millis())
    } else {
        format!("{secs:.1} s")
    };
    format!("{items} in {took}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn how_long_something_took_is_said_to_the_second_under_an_hour() {
        use super::format_elapsed;
        assert_eq!(format_elapsed(0), "0 s");
        assert_eq!(format_elapsed(11), "11 s");
        assert_eq!(format_elapsed(60), "1 min");
        assert_eq!(format_elapsed(71), "1 min 11 s");
        assert_eq!(format_elapsed(3_599), "59 min 59 s");
        assert_eq!(format_elapsed(3_600), "1 h");
        assert_eq!(format_elapsed(7_559), "2 h 5 min", "seconds stop counting");
    }

    #[test]
    fn odd_characters_are_found_and_written_out() {
        use super::{escape_odd, has_control};
        assert!(!has_control("IMG_1.jpg"));
        assert!(!has_control("ภาพ ๑.jpg"), "Thai is not odd");
        for odd in [
            "a\tb",
            "a\nb",
            "a\u{2028}b",
            "a\u{202e}b",
            "a\u{2067}b",
            "a\u{85}b",
        ] {
            assert!(has_control(odd), "{odd:?}");
        }
        assert_eq!(escape_odd("a\tb\nc"), "a\\tb\\nc");
        assert_eq!(escape_odd("\u{202e}gpj.exe"), "\\u{202e}gpj.exe");
        assert_eq!(escape_odd("ภาพ.jpg"), "ภาพ.jpg");
    }

    fn able() -> Abilities {
        Abilities {
            copy: true,
            moves: false,
            rename: true,
            partial_reads: true,
            partial_reads_64: false,
            thumbnails: true,
            prop_list: false,
            events: true,
        }
    }

    #[test]
    fn abilities_read_as_what_the_phone_can_and_cannot_do() {
        let (can, cannot) = abilities_in_words(&able());
        assert_eq!(
            can,
            [
                "copy on the phone",
                "rename",
                "resume a stopped download under 4 GB",
                "make its own thumbnails",
                "say when its files change"
            ]
        );
        assert_eq!(
            cannot,
            ["move on the phone", "read a folder's details at once"]
        );

        let big = Abilities {
            partial_reads_64: true,
            ..able()
        };
        assert!(
            abilities_in_words(&big)
                .0
                .contains(&"resume a stopped download")
        );
        let none = Abilities::default();
        assert!(abilities_in_words(&none).0.is_empty());
        assert!(
            abilities_in_words(&none)
                .1
                .contains(&"resume a stopped download")
        );
    }

    #[test]
    fn the_report_names_each_operation() {
        assert_eq!(
            operations_line(&able()),
            "CopyObject yes, MoveObject no, rename yes, GetPartialObject yes, GetPartialObject64 no, GetThumb yes, GetObjectPropList no, events yes"
        );
    }

    #[test]
    fn a_reading_says_how_many_and_how_long() {
        let timing = |items, ms| ReadTiming {
            items,
            took: std::time::Duration::from_millis(ms),
        };
        assert_eq!(format_reading(&timing(312, 1_430)), "312 items in 1.4 s");
        assert_eq!(format_reading(&timing(1, 85)), "1 item in 85 ms");
        assert_eq!(format_reading(&timing(5_001, 999)), "5,001 items in 999 ms");
    }

    use super::*;

    #[test]
    fn format_bytes_picks_a_readable_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_500), "1.5 KB");
        assert_eq!(format_bytes(64_000_000_000), "64.0 GB");
    }

    fn at(year: u16, month: u8, day: u8) -> Timestamp {
        Timestamp {
            year,
            month,
            day,
            hour: 9,
            minute: 5,
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
    }

    fn on(year: u16, month: u8, day: u8) -> Day {
        Day { year, month, day }
    }

    #[test]
    fn a_link_is_named_and_a_slow_one_says_what_would_help() {
        assert_eq!(link_label(UsbSpeed::High), "USB 2");
        assert_eq!(link_label(UsbSpeed::Super), "USB 3");
        assert_eq!(link_label(UsbSpeed::SuperPlus), "USB 3 (10 Gb/s)");
        assert_eq!(link_label(UsbSpeed::Full), "USB 1");
        assert_eq!(
            slow_link_note(Some(UsbSpeed::High)).as_deref(),
            Some(
                "Connected at USB 2 speed (480 Mb/s): about 35 MB/s at most. A USB 3 cable and port would be faster."
            )
        );
        assert!(
            slow_link_note(Some(UsbSpeed::Full))
                .unwrap()
                .contains("USB 1 speed (12 Mb/s): about 1 MB/s")
        );
        assert!(
            slow_link_note(Some(UsbSpeed::Low))
                .unwrap()
                .contains("well under 1 MB/s")
        );
        assert_eq!(slow_link_note(Some(UsbSpeed::Super)), None);
        assert_eq!(slow_link_note(None), None, "not known: nothing said");
    }

    #[test]
    fn each_date_choice_keeps_the_days_counted_back_from_today() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let span = |range| date_span(range, today);

        assert_eq!(
            span(DateRange::Today),
            DaySpan {
                from: Some(on(2026, 3, 1)),
                until: Some(on(2026, 3, 1))
            }
        );
        assert_eq!(
            span(DateRange::Yesterday),
            DaySpan {
                from: Some(on(2026, 2, 28)),
                until: Some(on(2026, 2, 28))
            },
            "across the end of a month"
        );
        assert_eq!(span(DateRange::LastWeek).from, Some(on(2026, 2, 23)));
        assert_eq!(span(DateRange::LastWeek).until, Some(on(2026, 3, 1)));
        assert_eq!(span(DateRange::LastMonth).from, Some(on(2026, 1, 31)));
        assert_eq!(
            span(DateRange::ThisYear),
            DaySpan {
                from: Some(on(2026, 1, 1)),
                until: None
            }
        );
        assert_eq!(
            span(DateRange::OlderThanAYear),
            DaySpan {
                from: None,
                until: Some(on(2025, 2, 28))
            }
        );
        let leap = NaiveDate::from_ymd_opt(2028, 2, 29).unwrap();
        assert_eq!(
            date_span(DateRange::OlderThanAYear, leap).until,
            Some(on(2027, 2, 27)),
            "a year before the 29th of February is the 28th"
        );
        assert_eq!(date_label(None), "Any date");
        assert_eq!(date_label(Some(DateRange::LastWeek)), "Last 7 days");
        assert!(local_today().year() >= 2026);
    }

    #[test]
    fn today_and_yesterday_show_only_the_time() {
        assert_eq!(format_timestamp(&at(2026, 10, 16), today()), "Today, 09:05");
        assert_eq!(
            format_timestamp(&at(2026, 10, 15), today()),
            "Yesterday, 09:05"
        );
    }

    #[test]
    fn this_year_drops_the_year() {
        assert_eq!(format_timestamp(&at(2026, 8, 3), today()), "3 Aug, 09:05");
    }

    #[test]
    fn other_years_drop_the_time() {
        assert_eq!(format_timestamp(&at(2025, 7, 11), today()), "11 Jul 2025");
    }

    #[test]
    fn yesterday_works_across_a_year_boundary() {
        let new_year = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();

        assert_eq!(
            format_timestamp(&at(2026, 12, 31), new_year),
            "Yesterday, 09:05"
        );
    }

    #[test]
    fn an_impossible_date_is_shown_as_written() {
        assert_eq!(
            format_timestamp(&at(2026, 2, 30), today()),
            "30 Feb 2026, 09:05"
        );
    }

    #[test]
    fn durations_round_to_a_readable_unit() {
        assert_eq!(format_duration(0), "0 s");
        assert_eq!(format_duration(59), "59 s");
        assert_eq!(format_duration(89), "1 min");
        assert_eq!(format_duration(90), "2 min");
        assert_eq!(format_duration(3600), "1 h");
        assert_eq!(format_duration(3900), "1 h 5 min");
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(7), "7");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(3_400), "3,400");
        assert_eq!(format_count(12_345), "12,345");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }
}
