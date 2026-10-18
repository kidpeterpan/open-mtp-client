//! The panic hook is the process's own, so this runs alone in its own process
//! (an integration test) and in one test function, in order: the hook, the
//! count of panics and the cap on reports are all global.

use std::path::PathBuf;

use internal::service::crash::{Crashes, panics_so_far};

fn panics_in_a_thread(name: &str, work: impl FnOnce() + Send + 'static) {
    let result = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .unwrap()
        .join();
    assert!(result.is_err(), "the thread was meant to panic");
}

fn files(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_panic_leaves_a_report_and_the_process_goes_on() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("crashes");
    let crashes = Crashes::new(dir.clone());
    assert!(crashes.last_panic().is_none());
    crashes.install("9.9.9");
    assert_eq!(panics_so_far(), 0);

    // A panic with a message, in a thread of its own.
    panics_in_a_thread("worker", || {
        panic!("could not read /Users/someone/Photos/trip 2026.jpg");
    });

    assert_eq!(panics_so_far(), 1);
    let notes = crashes.notes();
    assert_eq!(notes.len(), 1);
    assert!(!notes[0].reported);
    assert!(
        notes[0].location.starts_with("internal/tests/crash.rs:"),
        "{}",
        notes[0].location
    );
    let file = notes[0].file.clone().unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("open-mtp-client crash report\nVersion: 9.9.9\n"));
    assert!(text.contains("\nThread: worker\n"));
    assert!(text.contains("could not read /Users/someone/Photos/trip 2026.jpg"));
    assert!(text.contains("Backtrace:"));
    // What the app shows of it is where, never what.
    assert_eq!(crashes.last_panic(), Some(notes[0].clone()));
    assert!(!notes[0].location.contains("Photos"));

    // A panic that is not a message, and one that is a built String.
    panics_in_a_thread("odd", || std::panic::panic_any(7u8));
    panics_in_a_thread("built", || {
        let name = String::from("x");
        panic!("{name} is wrong");
    });
    assert_eq!(panics_so_far(), 3);
    let texts: Vec<String> = files(&dir)
        .iter()
        .map(|name| std::fs::read_to_string(dir.join(name)).unwrap())
        .collect();
    assert!(texts.iter().any(|t| t.contains("Message:\n(no message)\n")));
    assert!(texts.iter().any(|t| t.contains("Message:\nx is wrong\n")));

    // Told: the files stay, renamed, and a later panic is a new report.
    crashes.mark_reported();
    assert!(crashes.unreported().is_empty());
    assert!(files(&dir).iter().all(|name| name.starts_with("reported-")));
    panics_in_a_thread("after", || panic!("again"));
    assert_eq!(crashes.unreported().len(), 1);

    // A panic in a loop fills no disk: five reports a run, the rest counted.
    for _ in 0..8 {
        panics_in_a_thread("loop", || panic!("and again"));
    }
    assert_eq!(panics_so_far(), 12);
    assert_eq!(files(&dir).len(), 5);
    let last = crashes.last_panic().unwrap();
    assert_eq!(last.file, None, "the cap was passed, so no file");
    assert!(last.location.starts_with("internal/tests/crash.rs:"));
}
