//! The built program, run as a script would run it, against a fake phone.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_open-mtp-client-cli"))
}

#[test]
fn the_program_lists_a_phone_folder_and_exits_with_codes_a_script_can_read() {
    let phone = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(phone.path().join("DCIM")).unwrap();
    std::fs::write(phone.path().join("DCIM/a.jpg"), b"jpeg").unwrap();

    let listed = cli()
        .arg("--virtual")
        .arg(phone.path())
        .args(["--json", "ls", "/DCIM"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let entries: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(entries[0]["name"], "a.jpg");

    let missing = cli()
        .arg("--virtual")
        .arg(phone.path())
        .args(["ls", "/Nowhere"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(66), "EX_NOINPUT");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/Nowhere"));

    let wrong = cli().arg("dance").output().unwrap();
    assert_eq!(wrong.status.code(), Some(64), "EX_USAGE");
}

#[test]
fn rm_in_a_script_asks_no_one_and_deletes_nothing_unless_told() {
    let phone = tempfile::tempdir().unwrap();
    std::fs::write(phone.path().join("notes.txt"), b"keep me").unwrap();

    // Stdin is not a terminal here, as in a script or a pipe.
    let refused = cli()
        .arg("--virtual")
        .arg(phone.path())
        .args(["rm", "/notes.txt"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(70), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert!(phone.path().join("notes.txt").exists());

    let deleted = cli()
        .arg("--virtual")
        .arg(phone.path())
        .args(["rm", "--yes", "/notes.txt"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(deleted.status.success(), "{deleted:?}");
    assert!(!phone.path().join("notes.txt").exists());
}
