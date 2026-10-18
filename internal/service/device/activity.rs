//! When the phone last made progress, as everything sharing its one session
//! sees it.
//!
//! A phone has a single session, so requests queue: while a 3 GB video is going
//! over, a listing asked for in the meantime waits its turn. Giving up on a
//! request because *it* has waited a minute would call a busy phone silent.
//! What shows a phone is silent is that nothing on the session has moved for
//! that long: no answer to any request, no chunk of any transfer.
//!
//! Whoever hears from the phone calls `heard`. A request that works for minutes
//! without a word (copying a folder) says so with `working`, so the requests
//! behind it are not left to take that for silence until its own limit is up.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub(crate) struct Activity(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    /// What the times below count from.
    since: Instant,
    /// Milliseconds since `since` at which something was last heard.
    heard: AtomicU64,
    /// Milliseconds since `since` until which a long request that said nothing
    /// is expected to go on working; zero when there is none.
    busy_until: AtomicU64,
    /// Long requests in flight.
    working: AtomicUsize,
    /// How many times the phone has been heard from, for tests that check a
    /// transfer reports each chunk (a timestamp is too coarse to tell).
    #[cfg(test)]
    times_heard: AtomicUsize,
}

impl Default for Activity {
    fn default() -> Self {
        Self(Arc::new(Inner {
            since: Instant::now(),
            heard: AtomicU64::new(0),
            busy_until: AtomicU64::new(0),
            working: AtomicUsize::new(0),
            #[cfg(test)]
            times_heard: AtomicUsize::new(0),
        }))
    }
}

/// A long request in flight; see `Activity::working`.
pub(crate) struct Working(Activity);

impl Drop for Working {
    fn drop(&mut self) {
        let inner = &self.0.0;
        if inner.working.fetch_sub(1, Ordering::AcqRel) == 1 {
            inner.busy_until.store(0, Ordering::Release);
        }
    }
}

impl Activity {
    fn now(&self) -> u64 {
        u64::try_from(self.0.since.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The phone answered something, or a chunk of a transfer went over.
    pub(crate) fn heard(&self) {
        self.0.heard.fetch_max(self.now(), Ordering::AcqRel);
        #[cfg(test)]
        self.0.times_heard.fetch_add(1, Ordering::AcqRel);
    }

    /// Whether `other` is this very clock.
    #[cfg(test)]
    pub(crate) fn is(&self, other: &Activity) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    #[cfg(test)]
    pub(crate) fn times_heard(&self) -> usize {
        self.0.times_heard.load(Ordering::Acquire)
    }

    /// A request that works for up to `limit` without a word is going in. The
    /// requests behind it are not to give up on the phone before that is over.
    pub(crate) fn working(&self, limit: Duration) -> Working {
        let until = self
            .now()
            .saturating_add(u64::try_from(limit.as_millis()).unwrap_or(u64::MAX));
        self.0.working.fetch_add(1, Ordering::AcqRel);
        self.0.busy_until.fetch_max(until, Ordering::AcqRel);
        Working(self.clone())
    }

    /// How long the phone has been quiet to a request that began at `mark`
    /// (from `now`): since the later of the last thing heard and the request's
    /// own start, and not at all while a long request is expected to be working.
    fn quiet(&self, mark: u64) -> Duration {
        let now = self.now();
        if now < self.0.busy_until.load(Ordering::Acquire) {
            return Duration::ZERO;
        }
        let last = self.0.heard.load(Ordering::Acquire).max(mark);
        Duration::from_millis(now.saturating_sub(last))
    }

    /// Waits for `work`, unless the phone goes `limit` without making progress
    /// (see the module docs), when it gives up and returns `None`. Whatever
    /// `work` says, it was heard from the phone. A long request (`long`) tells
    /// the others it is working, for up to `limit`.
    pub(crate) async fn within<T>(
        &self,
        limit: Duration,
        long: bool,
        work: impl Future<Output = T>,
    ) -> Option<T> {
        let mark = self.now();
        let _working = long.then(|| self.working(limit));
        tokio::pin!(work);
        // Look often enough to notice within a fraction of the limit.
        let look_every = (limit / 4).max(Duration::from_millis(10));
        loop {
            tokio::select! {
                out = &mut work => {
                    self.heard();
                    return Some(out);
                }
                () = tokio::time::sleep(look_every) => {
                    if self.quiet(mark) >= limit {
                        return None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    const SHORT: Duration = Duration::from_millis(60);

    #[tokio::test]
    async fn an_answer_is_returned_and_counts_as_hearing_from_the_phone() {
        let activity = Activity::default();
        let before = activity.0.heard.load(Ordering::Acquire);
        tokio::time::sleep(Duration::from_millis(15)).await;

        let out = activity.within(SHORT, false, async { 7 }).await;

        assert_eq!(out, Some(7));
        assert!(activity.0.heard.load(Ordering::Acquire) > before);
    }

    #[tokio::test]
    async fn a_phone_that_never_answers_is_given_up_on_after_the_limit() {
        let started = Instant::now();

        let out = Activity::default()
            .within(SHORT, false, pending::<()>())
            .await;

        assert_eq!(out, None);
        let took = started.elapsed();
        assert!(took >= SHORT && took < SHORT * 3, "{took:?}");
    }

    #[tokio::test]
    async fn the_limit_runs_from_the_request_not_from_when_the_phone_was_last_heard() {
        // The phone has been idle for longer than the limit; a new request
        // still gets its whole limit.
        let activity = Activity::default();
        tokio::time::sleep(SHORT * 2).await;
        let started = Instant::now();

        let out = activity.within(SHORT, false, pending::<()>()).await;

        assert_eq!(out, None);
        assert!(started.elapsed() >= SHORT, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_wait_goes_on_as_long_as_something_else_on_the_session_makes_progress() {
        let activity = Activity::default();
        let other = activity.clone();
        let progress = tokio::spawn(async move {
            // A transfer's chunks, for several times the limit.
            for _ in 0..20 {
                tokio::time::sleep(SHORT / 4).await;
                other.heard();
            }
        });
        let started = Instant::now();

        let out = activity
            .within(SHORT, false, async {
                tokio::time::sleep(SHORT * 4).await;
                1
            })
            .await;

        progress.await.unwrap();
        assert_eq!(out, Some(1), "waited its turn");
        assert!(started.elapsed() >= SHORT * 4);
    }

    #[tokio::test]
    async fn once_the_progress_stops_the_wait_is_given_up_after_the_limit() {
        let activity = Activity::default();
        let other = activity.clone();
        let progress = tokio::spawn(async move {
            for _ in 0..8 {
                tokio::time::sleep(SHORT / 4).await;
                other.heard();
            }
        });
        let started = Instant::now();

        let out = activity.within(SHORT, false, pending::<()>()).await;

        progress.await.unwrap();
        assert_eq!(out, None);
        // About the 2 limits' worth the progress lasted, then a limit of quiet.
        assert!(started.elapsed() >= SHORT * 2, "{:?}", started.elapsed());
        assert!(started.elapsed() < SHORT * 6, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_long_request_in_flight_keeps_the_ones_behind_it_waiting_until_its_own_limit() {
        let activity = Activity::default();
        let long = SHORT * 5;
        let behind = activity.clone();
        // The long request says nothing for its whole limit.
        let copying = tokio::spawn({
            let activity = activity.clone();
            async move { activity.within(long, true, pending::<()>()).await }
        });
        tokio::time::sleep(SHORT / 3).await;
        let started = Instant::now();

        // A quick request behind it, with a limit well under the long one's.
        let out = behind.within(SHORT, false, pending::<()>()).await;

        assert_eq!(out, None);
        assert!(
            started.elapsed() >= long - SHORT,
            "it waited until the long one's limit: {:?}",
            started.elapsed()
        );
        assert_eq!(copying.await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_long_requests_mark_is_gone_when_it_is_finished() {
        let activity = Activity::default();
        let out = activity.within(SHORT * 5, true, async { 2 }).await;
        assert_eq!(out, Some(2));
        assert_eq!(activity.0.working.load(Ordering::Acquire), 0);
        assert_eq!(activity.0.busy_until.load(Ordering::Acquire), 0);

        // So a quick request after it is not kept waiting for its limit.
        let started = Instant::now();
        let out = activity.within(SHORT, false, pending::<()>()).await;
        assert_eq!(out, None);
        assert!(started.elapsed() < SHORT * 3);
    }

    #[test]
    fn two_long_requests_at_once_leave_the_mark_until_both_are_done() {
        let activity = Activity::default();

        let first = activity.working(Duration::from_secs(60));
        let second = activity.working(Duration::from_secs(30));
        assert_eq!(activity.quiet(0), Duration::ZERO);
        drop(first);
        assert_eq!(activity.0.working.load(Ordering::Acquire), 1);
        assert!(
            activity.0.busy_until.load(Ordering::Acquire) > 0,
            "one is still working"
        );
        drop(second);

        assert_eq!(activity.0.busy_until.load(Ordering::Acquire), 0);
    }
}
