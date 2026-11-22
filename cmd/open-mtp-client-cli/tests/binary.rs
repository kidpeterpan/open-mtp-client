//! The built program, run as a script would run it, against a fake phone.

use std::process::Command;

fn cli(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_open-mtp-client-cli"));
    command.env("HOME", home);
    command
}

#[test]
fn the_program_lists_a_phone_folder_and_exits_with_codes_a_script_can_read() {
    let phone = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(phone.path().join("DCIM")).unwrap();
    std::fs::write(phone.path().join("DCIM/a.jpg"), b"jpeg").unwrap();

    let listed = cli(home.path())
        .arg("--virtual")
        .arg(phone.path())
        .args(["--json", "ls", "/DCIM"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let entries: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(entries[0]["name"], "a.jpg");

    let missing = cli(home.path())
        .arg("--virtual")
        .arg(phone.path())
        .args(["ls", "/Nowhere"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(66), "EX_NOINPUT");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/Nowhere"));

    let wrong = cli(home.path()).arg("dance").output().unwrap();
    assert_eq!(wrong.status.code(), Some(64), "EX_USAGE");
}

#[test]
fn rm_in_a_script_asks_no_one_and_deletes_nothing_unless_told() {
    let phone = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(phone.path().join("notes.txt"), b"keep me").unwrap();

    // Stdin is not a terminal here, as in a script or a pipe.
    let refused = cli(home.path())
        .arg("--virtual")
        .arg(phone.path())
        .args(["rm", "/notes.txt"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(70), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert!(phone.path().join("notes.txt").exists());

    let deleted = cli(home.path())
        .arg("--virtual")
        .arg(phone.path())
        .args(["rm", "--yes", "/notes.txt"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(deleted.status.success(), "{deleted:?}");
    assert!(!phone.path().join("notes.txt").exists());
}

#[test]
fn a_mirror_that_removes_is_refused_in_a_script_without_yes() {
    let phone = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(phone.path().join("Music")).unwrap();
    std::fs::write(phone.path().join("Music/extra.mp3"), b"only on the phone").unwrap();
    let home = tempfile::tempdir().unwrap();
    let mac_music = home.path().join("Music");
    std::fs::create_dir_all(&mac_music).unwrap();
    std::fs::write(mac_music.join("new.mp3"), b"new song").unwrap();
    let asked = |words: &[&str]| {
        let answer = cli(home.path())
            .arg("--virtual")
            .arg(phone.path())
            .args(["--json"])
            .args(words)
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&answer.stdout).unwrap()
    };
    let serial = asked(&["info"])["serial"].as_str().unwrap().to_owned();
    let storage = asked(&["storages"])[0].clone();
    internal::service::places::Service::new(
        internal::service::app_folder(home.path()).join("places.json"),
        home.path().to_path_buf(),
        home.path().join("Volumes"),
    )
    .save_mirrors(&[internal::dto::SavedMirror {
        phone: internal::dto::PinPhone {
            serial,
            storage_id: storage["id"].as_u64().unwrap(),
            storage_name: storage["name"].as_str().unwrap().to_owned(),
        },
        phone_folder: "/Music".into(),
        mac_folder: mac_music.to_string_lossy().into_owned(),
        direction: internal::dto::MirrorDirection::ToPhone,
        remove_extras: true,
    }])
    .unwrap();

    let refused = cli(home.path())
        .arg("--virtual")
        .arg(phone.path())
        .args(["mirror", "1"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert_eq!(refused.status.code(), Some(64), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("add --yes"));
    assert!(phone.path().join("Music/extra.mp3").exists());
    assert!(!phone.path().join("Music/new.mp3").exists());
}
