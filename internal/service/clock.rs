//! This Mac's time zone. MTP dates are either zoneless phone-local times or
//! carry their own offset; both are shown in the Mac's local time.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Minutes east of UTC for the local zone at `unix_secs`, so daylight saving
/// is applied for that date rather than for today.
pub fn local_offset_minutes(unix_secs: i64) -> i16 {
    Timestamp::from_second(unix_secs)
        .ok()
        .and_then(|at| i16::try_from(TimeZone::system().to_offset(at).seconds() / 60).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_local_wall_clock() {
        let instant = 1_791_376_200;

        let offset = i64::from(local_offset_minutes(instant));

        let local = TimeZone::system().to_datetime(Timestamp::from_second(instant).unwrap());
        let shifted = instant + offset * 60;
        assert_eq!(shifted / 3600 % 24, i64::from(local.hour()));
        assert_eq!(shifted / 60 % 60, i64::from(local.minute()));
    }
}
