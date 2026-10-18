//! open-mtp-client-cli: the phone from the command line, for scripts. The
//! same services as the app (the same checks, resume and quarantine); see
//! `cli.rs` for what it does.

mod cli;

use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Ctrl-C stops a copy at its next safe point instead of killing it
    // mid-request (which can wedge a phone and loses the part's stamp).
    let cancel = internal::dto::Cancel::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.cancel();
        }
    });
    let mut world = cli::World {
        ask: Box::new(ask),
        terminal: std::io::stderr().is_terminal(),
        cancel,
        trash: Some(Arc::new(internal::service::local::system_trash)),
    };
    let code = cli::run(
        &args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        &mut world,
    )
    .await;
    std::process::exit(code);
}

/// Asks on the terminal; `None` when the program isn't run from one.
fn ask(question: &str) -> Option<bool> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let mut err = std::io::stderr();
    let _ = write!(err, "{question}");
    let _ = err.flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}
