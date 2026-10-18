//! Select what the other pane's folder lacks, or what it already has
//! (Edit › Select › Not in Other Pane / Also in Other Pane): the new photos
//! to copy, or what is backed up and can go. Mirror's rule, on what both
//! panes have listed already: a name matches ignoring case (APFS does), two
//! folders of one name are the same, and two files are the same when their
//! sizes agree and their dates are within a minute (the panes keep dates to
//! the minute; an unknown size or date on either side forgives, as Mirror's
//! does). One pass over each folder, no request to the phone.

use std::collections::HashMap;

use internal::dto;

/// Which rows to select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    /// Not in the other folder, or different there.
    Missing,
    /// In the other folder as it is here.
    Same,
}

/// What MTP says for a size it doesn't know ("4 GB or more").
const UNKNOWN_SIZE: u64 = 0xFFFF_FFFF;

/// The other folder's entries by name in lower case.
pub struct Other<'a> {
    by_name: HashMap<String, &'a dto::Entry>,
}

impl<'a> Other<'a> {
    pub fn new(entries: &'a [dto::Entry]) -> Self {
        Self {
            by_name: entries
                .iter()
                .map(|entry| (entry.name.to_lowercase(), entry))
                .collect(),
        }
    }

    /// Whether the other folder holds `entry` as it is.
    pub fn has(&self, entry: &dto::Entry) -> bool {
        self.by_name
            .get(&entry.name.to_lowercase())
            .is_some_and(|there| is_same(entry, there))
    }
}

/// Whether `a` and `b` (of one name) are the same item.
fn is_same(a: &dto::Entry, b: &dto::Entry) -> bool {
    match (a.is_folder, b.is_folder) {
        (true, true) => true,
        (false, false) => {
            let known = |size| size != UNKNOWN_SIZE;
            let sizes =
                !known(a.size_bytes) || !known(b.size_bytes) || a.size_bytes == b.size_bytes;
            let dates = match (a.modified, b.modified) {
                (Some(x), Some(y)) => (minutes(x) - minutes(y)).abs() <= 1,
                _ => true,
            };
            sizes && dates
        }
        // A file where a folder is, or the reverse.
        _ => false,
    }
}

/// Minutes since 1970 for a wall-clock time (no zone: both panes show
/// wall-clock times).
fn minutes(at: dto::Timestamp) -> i64 {
    // Days from the civil date (Howard Hinnant's algorithm).
    let (month, day) = (i64::from(at.month), i64::from(at.day));
    let year = i64::from(at.year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_era_days - 719_468;
    days * 1_440 + i64::from(at.hour) * 60 + i64::from(at.minute)
}

/// The names among `rows` (in their order) to select for `want`.
pub fn pick<'r>(
    rows: impl IntoIterator<Item = &'r dto::Entry>,
    other: &Other<'_>,
    want: Against,
) -> Vec<String> {
    rows.into_iter()
        .filter(|entry| other.has(entry) == (want == Against::Same))
        .map(|entry| entry.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: u16, month: u8, day: u8, hour: u8, minute: u8) -> dto::Timestamp {
        dto::Timestamp {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    fn file(name: &str, size: u64, modified: Option<dto::Timestamp>) -> dto::Entry {
        dto::Entry {
            id: 0,
            name: name.into(),
            is_folder: false,
            size_bytes: size,
            modified,
        }
    }

    fn folder(name: &str) -> dto::Entry {
        dto::Entry {
            is_folder: true,
            ..file(name, 0, None)
        }
    }

    #[test]
    fn minutes_count_across_days_months_leap_years_and_eras() {
        assert_eq!(minutes(at(1970, 1, 1, 0, 0)), 0);
        assert_eq!(minutes(at(1970, 1, 2, 0, 1)), 1_441);
        assert_eq!(
            minutes(at(2024, 3, 1, 0, 0)) - minutes(at(2024, 2, 28, 23, 59)),
            1 + 1_440,
            "29 February in a leap year"
        );
        assert_eq!(
            minutes(at(2023, 3, 1, 0, 0)) - minutes(at(2023, 2, 28, 23, 59)),
            1
        );
        assert_eq!(
            minutes(at(2027, 1, 1, 0, 0)) - minutes(at(2026, 12, 31, 23, 59)),
            1,
            "new year"
        );
        assert_eq!(minutes(at(2000, 3, 1, 0, 0)) / 1_440, 11_017);
        assert_eq!(
            minutes(at(2100, 3, 1, 0, 0)) / 1_440,
            47_541,
            "no 29 Feb 2100"
        );
    }

    #[test]
    fn a_file_is_the_same_by_name_in_any_case_size_and_date_within_a_minute() {
        let noon = Some(at(2026, 6, 14, 12, 0));
        let here = file("IMG_1.jpg", 10, noon);
        let same = |other: dto::Entry| Other::new(std::slice::from_ref(&other)).has(&here);

        assert!(same(file("img_1.JPG", 10, noon)), "case");
        assert!(
            same(file("IMG_1.jpg", 10, Some(at(2026, 6, 14, 12, 1)))),
            "a minute"
        );
        assert!(same(file("IMG_1.jpg", 10, Some(at(2026, 6, 14, 11, 59)))));
        assert!(
            !same(file("IMG_1.jpg", 10, Some(at(2026, 6, 14, 12, 2)))),
            "two"
        );
        assert!(!same(file("IMG_1.jpg", 11, noon)), "another size");
        assert!(
            same(file("IMG_1.jpg", UNKNOWN_SIZE, noon)),
            "4 GB or more forgives"
        );
        assert!(same(file("IMG_1.jpg", 10, None)), "no date forgives");
        assert!(!same(folder("IMG_1.jpg")), "a folder of that name");
        assert!(!same(file("IMG_2.jpg", 10, noon)), "another name");
        assert!(
            !Other::new(&[folder("DCIM")]).has(&file("DCIM", 0, None)),
            "a file where a folder is"
        );
        assert!(
            Other::new(&[folder("dcim")]).has(&folder("DCIM")),
            "folders by name"
        );
        let unknown_here = file("IMG_1.jpg", UNKNOWN_SIZE, noon);
        assert!(Other::new(&[file("IMG_1.jpg", 10, noon)]).has(&unknown_here));
    }

    #[test]
    fn missing_and_same_split_the_rows_between_them_in_their_order() {
        let day = Some(at(2026, 6, 14, 12, 0));
        let rows = [
            folder("Trip"),
            file("a.jpg", 1, day),
            file("b.jpg", 2, day),
            file("c.jpg", 3, day),
        ];
        let backup = [
            file("A.JPG", 1, day),
            file("c.jpg", 30, day),
            folder("trip"),
        ];
        let other = Other::new(&backup);

        assert_eq!(pick(&rows, &other, Against::Missing), ["b.jpg", "c.jpg"]);
        assert_eq!(pick(&rows, &other, Against::Same), ["Trip", "a.jpg"]);
        assert!(pick(&[] as &[dto::Entry], &other, Against::Missing).is_empty());
    }

    #[test]
    fn five_thousand_rows_are_picked_in_one_pass() {
        let day = Some(at(2026, 6, 14, 12, 0));
        let rows: Vec<dto::Entry> = (0..5_000)
            .map(|i| file(&format!("IMG_{i:04}.jpg"), 100, day))
            .collect();
        let backup: Vec<dto::Entry> = (0..4_000)
            .map(|i| file(&format!("img_{i:04}.jpg"), 100, day))
            .collect();

        let started = std::time::Instant::now();
        let other = Other::new(&backup);
        let missing = pick(&rows, &other, Against::Missing);
        let took = started.elapsed();

        assert_eq!(missing.len(), 1_000);
        assert_eq!(missing[0], "IMG_4000.jpg");
        eprintln!("5,000 rows against 4,000 in {took:?}");
        assert!(took < std::time::Duration::from_millis(200), "{took:?}");
    }
}
