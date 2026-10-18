//! How long the phone gets to answer a request before it is given up on.
//!
//! A phone that goes silent never reports an error: Pixels do it, `mtp-rs`
//! documents it, and a request to one simply never comes back. Without a limit
//! the pane waits for ever with its dialog or its "Copying on the phone…" banner
//! up, and the only way out is pulling the cable. With one, the wait ends in
//! `Timeout`, which drops the wedged session (`device::Service::handle_error`)
//! and offers Reconnect, as a silent transfer does.
//!
//! The limit is per request, never for a whole operation: reading 20,000 files
//! is 20,000 requests and takes minutes, and every one of them is answered in
//! milliseconds. Two sizes: `ask` for a request that is answered at once, and
//! `long` for one that does a lot before it answers and reports nothing in
//! between (copying a folder on the phone, deleting one with everything in it,
//! renaming or moving one, which rewrites the path of everything inside),
//! which must not be mistaken for a silent one.

use std::future::Future;
use std::time::Duration;

use mtp_rs::mtp::{ListingItem, ObjectInfo, ObjectListing, Storage};

use crate::service::device::Activity;

/// Long enough for a busy phone (its media scanner running over a big card)
/// and short enough that a silent one is noticed.
pub(crate) const ASK: Duration = Duration::from_secs(60);

/// For a request that works for minutes without a word; see the module docs.
pub(crate) const LONG: Duration = Duration::from_secs(10 * 60);

/// "Silent" means nothing on the phone's session has made progress for the
/// limit (`Activity`), not that this request has waited that long: it may be
/// queued behind a transfer that is going fine.
#[derive(Debug, Clone)]
pub(crate) struct Limits {
    ask: Duration,
    long: Duration,
    activity: Activity,
}

/// Only tests build limits that count nothing but what they see: a request to
/// the phone is always made under the device's own (`Limits::tied_to`), so the
/// requests queued behind it know when it is busy and when it is silent.
#[cfg(test)]
impl Default for Limits {
    fn default() -> Self {
        Self::tied_to(Activity::default())
    }
}

impl Limits {
    /// The usual limits, counting what happens on `activity`: the phone's.
    pub(crate) fn tied_to(activity: Activity) -> Self {
        Self {
            ask: ASK,
            long: LONG,
            activity,
        }
    }

    /// Whether these limits count what happens on `activity`.
    #[cfg(test)]
    pub(crate) fn counts_on(&self, activity: &Activity) -> bool {
        self.activity.is(activity)
    }

    /// Other limits, so a test doesn't wait a minute for a silent phone.
    #[cfg(test)]
    pub(crate) fn new(ask: Duration, long: Duration) -> Self {
        Self::tied(Activity::default(), ask, long)
    }

    /// Other limits counting what happens on `activity`.
    #[cfg(test)]
    pub(crate) fn tied(activity: Activity, ask: Duration, long: Duration) -> Self {
        Self {
            ask,
            long,
            activity,
        }
    }

    /// A request the phone answers at once.
    pub(crate) async fn ask<T>(
        &self,
        work: impl Future<Output = Result<T, mtp_rs::Error>>,
    ) -> Result<T, mtp_rs::Error> {
        self.request(self.ask, false, work).await
    }

    /// A request that may work for minutes before it answers.
    pub(crate) async fn long<T>(
        &self,
        work: impl Future<Output = Result<T, mtp_rs::Error>>,
    ) -> Result<T, mtp_rs::Error> {
        self.request(self.long, true, work).await
    }

    /// A request whose work grows with what it touches, so it is quick for one
    /// file and long for a folder: renaming or moving a folder has the phone
    /// rewrite the path of everything inside it before it answers.
    pub(crate) async fn scaled<T>(
        &self,
        big: bool,
        work: impl Future<Output = Result<T, mtp_rs::Error>>,
    ) -> Result<T, mtp_rs::Error> {
        let limit = if big { self.long } else { self.ask };
        self.request(limit, big, work).await
    }

    /// The next item of a listing; a phone that stops describing its files
    /// ends the listing with `Timeout`.
    pub(crate) async fn next(
        &self,
        listing: &mut ObjectListing,
    ) -> Option<Result<ListingItem, mtp_rs::Error>> {
        match self.activity.within(self.ask, false, listing.next()).await {
            Some(item) => item,
            None => Some(Err(mtp_rs::Error::Timeout)),
        }
    }

    /// `work`, unless the phone goes `limit` without making progress; the wait
    /// then ends as the `Timeout` a slow USB transfer would report.
    async fn request<T>(
        &self,
        limit: Duration,
        long: bool,
        work: impl Future<Output = Result<T, mtp_rs::Error>>,
    ) -> Result<T, mtp_rs::Error> {
        match self.activity.within(limit, long, work).await {
            Some(result) => result,
            None => Err(mtp_rs::Error::Timeout),
        }
    }
}

/// Everything in a folder, described: what `Storage::list_objects` does, except
/// that each item gets its own limit rather than the whole folder having one
/// (a camera folder is thousands of requests). Items the phone refuses to
/// describe are left out, and a phone that describes none of what it listed has
/// failed rather than shown an empty folder.
pub(crate) async fn read_children(
    storage: &Storage,
    parent: Option<mtp_rs::mtp::ObjectHandle>,
    limits: &Limits,
) -> Result<Vec<ObjectInfo>, mtp_rs::Error> {
    let listing = limits.ask(storage.list_objects_stream(parent)).await?;
    read_all(listing, limits).await
}

/// Reads every item of a listing; see `read_children`.
pub(crate) async fn read_all(
    mut listing: ObjectListing,
    limits: &Limits,
) -> Result<Vec<ObjectInfo>, mtp_rs::Error> {
    let mut objects = Vec::with_capacity(listing.total());
    let mut first_refused = None;
    while let Some(item) = limits.next(&mut listing).await {
        match item? {
            ListingItem::Object(object) => objects.push(object),
            ListingItem::Skipped(skipped) => {
                first_refused.get_or_insert(skipped.error);
            }
        }
    }
    match first_refused {
        Some(err) if objects.is_empty() => Err(err),
        _ => Ok(objects),
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::time::Instant;

    use super::*;

    const SHORT: Duration = Duration::from_millis(40);

    fn limits() -> Limits {
        Limits::new(SHORT, SHORT * 5)
    }

    #[tokio::test]
    async fn a_request_that_is_answered_passes_through_whatever_it_says() {
        assert_eq!(limits().ask(async { Ok(7) }).await.unwrap(), 7);
        assert!(matches!(
            limits()
                .ask(async { Err::<(), _>(mtp_rs::Error::NotFound) })
                .await,
            Err(mtp_rs::Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_request_the_phone_never_answers_ends_as_a_timeout_at_the_limit() {
        let started = Instant::now();

        let result = limits().ask(pending::<Result<(), mtp_rs::Error>>()).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        let took = started.elapsed();
        assert!(took >= SHORT && took < SHORT * 5, "{took:?}");
    }

    #[tokio::test]
    async fn a_long_request_gets_longer_than_a_quick_one() {
        let started = Instant::now();

        let result = limits().long(pending::<Result<(), mtp_rs::Error>>()).await;

        assert!(matches!(result, Err(mtp_rs::Error::Timeout)));
        assert!(started.elapsed() >= SHORT * 5, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_request_that_scales_with_what_it_touches_is_quick_for_one_file_and_long_for_a_folder()
     {
        let started = Instant::now();
        let file = limits()
            .scaled(false, pending::<Result<(), mtp_rs::Error>>())
            .await;
        assert!(matches!(file, Err(mtp_rs::Error::Timeout)));
        assert!(started.elapsed() < SHORT * 4, "{:?}", started.elapsed());

        let started = Instant::now();
        let folder = limits()
            .scaled(true, pending::<Result<(), mtp_rs::Error>>())
            .await;
        assert!(matches!(folder, Err(mtp_rs::Error::Timeout)));
        assert!(started.elapsed() >= SHORT * 5, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_slow_answer_inside_the_limit_is_still_an_answer() {
        let result = limits()
            .ask(async {
                tokio::time::sleep(SHORT / 4).await;
                Ok(1)
            })
            .await;

        assert_eq!(result.unwrap(), 1);
    }

    #[test]
    fn the_real_limits_are_a_minute_and_ten() {
        let limits = Limits::default();

        assert_eq!(limits.ask, Duration::from_secs(60));
        assert_eq!(limits.long, Duration::from_secs(600));
        assert!(limits.long > limits.ask * 5, "long means minutes");
    }

    // ---- nothing talks to the phone without a limit ----

    /// The code of a source file, without its tests (which set things up with
    /// raw requests).
    fn code_of(source: &str) -> &str {
        source.split("#[cfg(test)]\nmod tests {").next().unwrap()
    }

    /// The lines of `source` that make `requests` of the phone without one of
    /// `limited_by` on the same line or just above it (rustfmt breaks long
    /// chains over a few lines).
    fn unlimited(source: &str, requests: &[&str], limited_by: &[&str]) -> Vec<String> {
        let lines: Vec<&str> = code_of(source).lines().collect();
        let mut found = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if !requests.iter().any(|request| code.contains(request)) {
                continue;
            }
            let nearby = &lines[at.saturating_sub(3)..=at];
            if !nearby
                .iter()
                .any(|near| limited_by.iter().any(|limit| near.contains(limit)))
            {
                found.push(format!("{}: {}", at + 1, line.trim()));
            }
        }
        found
    }

    #[test]
    fn every_request_in_the_explorer_has_a_limit() {
        let requests = [
            ".get_object_info(",
            ".list_objects(",
            ".list_objects_stream(",
            ".create_folder(",
            ".move_object(",
            ".copy_object(",
            ".rename(",
            ".delete(",
            ".storage(",
            ".objects.next(",
        ];
        // `raw.rs` is the one place that makes them as they are, for callers
        // that bound each one (`delete_object`, `place_one`).
        let files = [
            ("service.rs", include_str!("service.rs")),
            ("paste.rs", include_str!("paste.rs")),
            ("listing.rs", include_str!("listing.rs")),
            ("search.rs", include_str!("search.rs")),
        ];
        for (name, source) in files {
            let loose = unlimited(
                source,
                &requests,
                &[".ask(", ".long(", ".scaled(", "limits.next("],
            );
            assert!(
                loose.is_empty(),
                "{name} asks the phone with no limit, so a silent phone would hang it:\n{}",
                loose.join("\n")
            );
        }
    }

    #[test]
    fn every_request_the_transfers_plan_and_tidy_up_with_has_a_limit() {
        // Their copying has its own (`phone_answers`, `unless_stalled`); these
        // are the requests around it.
        let requests = [
            ".get_object_info(",
            ".list_objects(",
            ".list_objects_stream(",
            ".create_folder(",
            ".delete(",
        ];
        let files = [
            ("upload.rs", include_str!("../transfer/upload.rs")),
            ("service.rs", include_str!("../transfer/service.rs")),
        ];
        for (name, source) in files {
            let loose = unlimited(
                source,
                &requests,
                &[
                    "phone_answers(",
                    "unless_stalled(",
                    "read_children(",
                    ".ask(",
                    ".long(",
                ],
            );
            assert!(
                loose.is_empty(),
                "transfer/{name} asks the phone with no limit:\n{}",
                loose.join("\n")
            );
        }
    }

    #[test]
    fn the_check_itself_finds_a_request_with_no_limit_and_lets_a_limited_one_pass() {
        let code = "fn a() {\n    storage.get_object_info(h).await;\n}\n";
        assert_eq!(unlimited(code, &[".get_object_info("], &[".ask("]).len(), 1);

        let limited =
            "fn a() {\n    limits\n        .ask(storage.get_object_info(h))\n        .await;\n}\n";
        assert!(unlimited(limited, &[".get_object_info("], &[".ask("]).is_empty());

        // Tests set things up with raw requests: they are not held to it.
        let with_tests = "fn a() {}\n#[cfg(test)]\nmod tests {\n    s.get_object_info(h);\n}\n";
        assert!(unlimited(with_tests, &[".get_object_info("], &[".ask("]).is_empty());
    }
}
