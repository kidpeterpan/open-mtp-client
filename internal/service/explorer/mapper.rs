use mtp_rs::mtp::{DateTime, ObjectInfo, UtcOffset};

use crate::dto::{Entry, Timestamp};

/// `local_offset` gives the Mac's UTC offset in minutes at a unix time.
pub fn to_entry(object: &ObjectInfo, local_offset: impl Fn(i64) -> i16) -> Entry {
    Entry {
        id: object.handle.0,
        name: object.filename.clone(),
        is_folder: object.is_folder(),
        size_bytes: if object.is_folder() { 0 } else { object.size },
        modified: object
            .modified
            .or(object.created)
            .map(|time| to_timestamp(time, &local_offset)),
    }
}

/// A date that carries its own offset is converted to the Mac's local time;
/// a zoneless one (what Android writes) is already phone-local and shown as is.
fn to_timestamp(time: DateTime, local_offset: impl Fn(i64) -> i16) -> Timestamp {
    let local = time
        .offset
        .and_then(|_| time.to_unix_seconds())
        .and_then(|secs| {
            let offset = UtcOffset::from_minutes(local_offset(secs))?;
            DateTime::from_unix_seconds(secs, offset)
        })
        .unwrap_or(time);

    Timestamp {
        year: local.year,
        month: local.month,
        day: local.day,
        hour: local.hour,
        minute: local.minute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANGKOK: i16 = 7 * 60;

    fn date(hour: u8, offset: Option<UtcOffset>) -> DateTime {
        DateTime {
            year: 2026,
            month: 10,
            day: 16,
            hour,
            minute: 44,
            second: 0,
            offset,
        }
    }

    #[test]
    fn a_dated_utc_time_is_shown_in_local_time() {
        let stamp = to_timestamp(date(5, Some(UtcOffset::UTC)), |_| BANGKOK);

        assert_eq!((stamp.day, stamp.hour, stamp.minute), (16, 12, 44));
    }

    #[test]
    fn local_conversion_can_cross_midnight() {
        let stamp = to_timestamp(date(20, Some(UtcOffset::UTC)), |_| BANGKOK);

        assert_eq!((stamp.day, stamp.hour), (17, 3));
    }

    #[test]
    fn a_zoneless_time_is_kept_as_written() {
        let stamp = to_timestamp(date(5, None), |_| BANGKOK);

        assert_eq!(stamp.hour, 5);
    }
}
