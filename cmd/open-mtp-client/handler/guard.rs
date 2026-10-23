//! Background work that panics must still answer.
//!
//! A panic inside a `Task` ends that task without a word. Whatever was waiting
//! for its answer ("Loading…", "Connecting…", a paste in progress) would wait
//! for ever, and the crash banner is the only sign anything went wrong. These
//! helpers turn the panic into an answer the waiting screen already knows how
//! to show, `dto::Error::Crashed`. It is not a lost phone (`device_lost` is
//! false), so a bug never closes a session that is working.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use iced::Task;
use iced::futures::{FutureExt, Stream, StreamExt, stream};
use internal::dto;

use super::error::{self, ErrorResponse};

/// Runs `work`; `None` if it panicked. The panic hook has already written the
/// crash report by the time this returns.
pub async fn caught<T>(work: impl Future<Output = T>) -> Option<T> {
    AssertUnwindSafe(work).catch_unwind().await.ok()
}

/// Runs `work`, and says `Crashed` if it panicked.
pub async fn guarded<T>(
    work: impl Future<Output = Result<T, dto::Error>>,
) -> Result<T, dto::Error> {
    caught(work).await.unwrap_or(Err(dto::Error::Crashed))
}

/// `Task::perform` for work a screen waits on. If `work` panics, `on_panic()`
/// is its answer, and goes through `then` like any other, so what was waiting
/// ("Loading…", "Connecting…", a paste in progress) hears something.
///
/// This and [`run`] are the only ways to make a task: clippy refuses
/// `Task::perform`, `run`, `future` and `stream` anywhere else (`clippy.toml`),
/// so a task either has an answer for a panic or, for a stream, goes
/// through [`run_unguarded`], which says so where it is called.
#[allow(clippy::disallowed_methods)] // The one place the call is made.
pub fn perform<A, T>(
    work: impl Future<Output = A> + Send + 'static,
    on_panic: impl FnOnce() -> A + Send + 'static,
    then: impl FnOnce(A) -> T + Send + 'static,
) -> Task<T>
where
    A: Send + 'static,
    T: Send + 'static,
{
    Task::perform(
        async move { caught(work).await.unwrap_or_else(on_panic) },
        then,
    )
}

/// `Task::run` for a stream a screen waits on. If it panics, `on_panic()` is
/// the last item, and nothing comes after it.
#[allow(clippy::disallowed_methods)] // The one place the call is made.
pub fn run<A, T>(
    items: impl Stream<Item = A> + Send + 'static,
    on_panic: impl FnOnce() -> A + Send + 'static,
    then: impl Fn(A) -> T + Send + 'static,
) -> Task<T>
where
    A: Send + 'static,
    T: Send + 'static,
{
    Task::run(catching(items, on_panic), then)
}

/// `items`, ending with `on_panic()` instead of silence if polling one panics.
fn catching<A>(
    items: impl Stream<Item = A> + Send + 'static,
    on_panic: impl FnOnce() -> A + Send + 'static,
) -> impl Stream<Item = A> + Send {
    stream::unfold(
        (Box::pin(items), Some(on_panic)),
        |(mut items, on_panic)| async move {
            // `None` once the answer for a panic has been given: that ends it.
            let on_panic = on_panic?;
            match caught(items.next()).await {
                Some(Some(item)) => Some((item, (items, Some(on_panic)))),
                Some(None) => None,
                None => Some((on_panic(), (items, None))),
            }
        },
    )
}

/// `Task::run` with no answer for a panic. Only for a stream that catches a
/// panic itself, or that nothing waits on (the call sites say which): a
/// panic in it is otherwise only the crash banner.
#[allow(clippy::disallowed_methods)] // The one place the call is made.
pub fn run_unguarded<A, T>(
    items: impl Stream<Item = A> + Send + 'static,
    then: impl Fn(A) -> T + Send + 'static,
) -> Task<T>
where
    A: Send + 'static,
    T: Send + 'static,
{
    Task::run(items, then)
}

/// The answer for work that panicked, as a screen shows an error.
pub fn crashed() -> ErrorResponse {
    error::to_error_response(dto::Error::Crashed)
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt, stream};

    use super::*;
    use crate::handler::testing::drain;

    #[tokio::test]
    async fn the_guard_hands_back_what_the_work_answered() {
        assert_eq!(guarded(async { Ok::<_, dto::Error>(7) }).await, Ok(7));
        assert_eq!(
            guarded(async { Err::<u8, _>(dto::Error::Cancelled) }).await,
            Err(dto::Error::Cancelled)
        );
        assert_eq!(
            guarded(async { panic!("broke") as Result<u8, dto::Error> }).await,
            Err(dto::Error::Crashed)
        );
    }

    #[tokio::test]
    async fn caught_work_is_none_only_when_it_panicked() {
        assert_eq!(caught(async { 7 }).await, Some(7));
        assert_eq!(caught(async { panic!("broke") as u8 }).await, None);
    }

    #[tokio::test]
    async fn work_that_answers_is_passed_on_and_its_fallback_is_not_used() {
        let task = perform(async { 7 }, || 0, |answer| answer + 1);

        assert_eq!(drain(task).await.0, [8]);
    }

    #[tokio::test]
    async fn work_that_panics_is_answered_with_its_fallback_so_the_screen_hears_something() {
        let task = perform(async { panic!("broke") as u8 }, || 99, |answer| answer + 1);

        assert_eq!(
            drain(task).await.0,
            [100],
            "the fallback went through `then`"
        );
    }

    #[tokio::test]
    async fn a_stream_that_does_not_panic_passes_every_item_and_no_fallback() {
        let task = run(stream::iter(0u8..3), || 99, |item| item);

        assert_eq!(drain(task).await.0, [0, 1, 2]);
    }

    #[tokio::test]
    async fn a_stream_that_panics_ends_with_its_fallback_and_nothing_after() {
        let items = stream::iter(0u8..5).map(|n| {
            assert!(n != 2, "broke");
            n
        });

        let task = run(items, || 99, |item| item);

        assert_eq!(drain(task).await.0, [0, 1, 99]);
    }

    #[tokio::test]
    async fn the_unguarded_call_passes_every_item_through_and_is_named_so() {
        let items = run_unguarded(stream::iter(0u8..3), |item| item);

        assert_eq!(drain(items).await.0, [0, 1, 2]);
    }

    // Everything above rests on a panic unwinding to the guard. With
    // `panic = "abort"` it ends the whole process instead, `catch_unwind`
    // catches nothing, and these tests, which build with unwinding whatever
    // the profile says, would stay green. So the settings are read here.

    /// Whether a Cargo or cargo-config file makes a panic abort the process.
    fn aborts_on_panic(config: &str) -> bool {
        config.lines().any(|line| {
            let setting: String = line
                .split('#')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|ch| !ch.is_whitespace() && !matches!(ch, '"' | '\''))
                .collect();
            setting.contains("panic=abort")
        })
    }

    #[test]
    fn a_setting_that_aborts_on_panic_is_seen_however_it_is_written() {
        for config in [
            "[profile.release]\npanic = \"abort\"",
            "panic='abort'",
            "[build]\nrustflags = [\"-C\", \"panic=abort\"]",
            "rustflags = [\"-Cpanic=abort\"]",
            "\tRUSTFLAGS=\"-C panic=abort\" cargo build --release",
            "env:\n  RUSTFLAGS: -C panic=abort",
        ] {
            assert!(aborts_on_panic(config), "{config}");
        }
    }

    #[test]
    fn unwinding_and_comments_are_not_aborting() {
        for config in [
            "[profile.release]\npanic = \"unwind\"",
            "# panic = \"abort\"",
            "opt-level = 3 # panic = \"abort\" would end the process",
            "",
        ] {
            assert!(!aborts_on_panic(config), "{config}");
        }
    }

    #[test]
    fn the_app_unwinds_on_panic_so_the_guards_can_catch_it() {
        // Where a build of the app is set up: the manifest, cargo's own
        // config, and what builds the app users download (`make dmg`, run by
        // the release workflow, so a flag set in either would reach them).
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = vec![
            root.join("Cargo.toml"),
            root.join(".cargo/config.toml"),
            root.join(".cargo/config"),
            root.join("Makefile"),
        ];
        if let Ok(workflows) = std::fs::read_dir(root.join(".github/workflows")) {
            files.extend(workflows.flatten().map(|entry| entry.path()));
        }
        let mut read = 0;
        for file in files {
            let Ok(config) = std::fs::read_to_string(&file) else {
                continue; // Not every file is there.
            };
            read += 1;
            assert!(
                !aborts_on_panic(&config),
                "{} makes a panic abort the process: the guards in this module would catch \
                 nothing, and a panic would end the app with no answer for its screen",
                file.display()
            );
        }
        assert!(
            read >= 3,
            "the manifest, Makefile and workflows were not found"
        );
    }

    #[test]
    fn a_crash_is_worded_for_the_screen_and_is_not_a_lost_phone() {
        let response = crashed();

        assert_eq!(response.title, "Something went wrong inside the app");
        assert!(
            !response.device_lost,
            "a bug must not close a working phone"
        );
        assert!(!response.cancelled);
        assert_eq!(response.source, dto::Error::Crashed);
    }
}
