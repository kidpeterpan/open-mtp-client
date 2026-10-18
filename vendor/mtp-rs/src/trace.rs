//! Internal diagnostic-logging shim.
//!
//! [`diag_debug!`] and [`diag_trace!`] emit `tracing` events when the `tracing`
//! feature is enabled, and compile to nothing otherwise while still borrowing
//! their arguments (so they never trip an unused-variable warning in the default,
//! dependency-free build). Use plain format-string syntax:
//!
//! ```ignore
//! diag_debug!("cancel wedged the device (txn={}), resetting", txn);
//! diag_trace!("drained {} packets ({} bytes)", packets, bytes);
//! ```
//!
//! Reporters capture the cancel/reset path (issue #18) by building with
//! `--features tracing` and running under a subscriber with
//! `RUST_LOG=mtp_rs=debug` (the CLI wires one up for its `-v` flags).

/// Emit a `debug`-level diagnostic (notable events: cancel steps, wedge
/// detection, reset). No-op unless the `tracing` feature is on.
#[cfg(feature = "tracing")]
macro_rules! diag_debug {
    ($($arg:tt)*) => { ::tracing::debug!($($arg)*) };
}

/// Emit a `trace`-level diagnostic (fine-grained: per-operation execution).
/// No-op unless the `tracing` feature is on.
#[cfg(feature = "tracing")]
macro_rules! diag_trace {
    ($($arg:tt)*) => { ::tracing::trace!($($arg)*) };
}

#[cfg(not(feature = "tracing"))]
macro_rules! diag_debug {
    // Borrow the args via `format_args!` so they count as used, then discard.
    // Optimizes away to nothing; keeps the default build warning-clean.
    ($($arg:tt)*) => {{ let _ = ::core::format_args!($($arg)*); }};
}

#[cfg(not(feature = "tracing"))]
macro_rules! diag_trace {
    ($($arg:tt)*) => {{ let _ = ::core::format_args!($($arg)*); }};
}
