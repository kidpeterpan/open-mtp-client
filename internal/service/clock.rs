//! This Mac's time zone. MTP dates are either zoneless phone-local times or
//! carry their own offset; both are shown in the Mac's local time.

use chrono::{Local, Offset, TimeZone};

/// Minutes east of UTC for the local zone at `unix_secs`, so daylight saving
/// is applied for that date rather than for today.
pub fn local_offset_minutes(unix_secs: i64) -> i16 {
    Local
        .timestamp_opt(unix_secs, 0)
        .single()
        .and_then(|time| i16::try_from(time.offset().fix().local_minus_utc() / 60).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use chrono::Timelike;

    use super::*;

    #[test]
    fn matches_the_local_wall_clock() {
        let instant = 1_791_376_200;

        let offset = i64::from(local_offset_minutes(instant));

        let local = Local.timestamp_opt(instant, 0).unwrap();
        let shifted = instant + offset * 60;
        assert_eq!((shifted / 3600 % 24) as u32, local.hour());
        assert_eq!((shifted / 60 % 60) as u32, local.minute());
    }
}
