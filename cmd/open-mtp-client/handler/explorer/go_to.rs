//! Go to a folder by typing its path (⇧⌘G, Finder's Go to Folder). Pure: how
//! what is typed becomes a folder, and which folder names it could go on
//! with. The names come from what the pane already knows (the folder shown,
//! the phone's remembered folders, the Mac's disk); nothing here asks the
//! phone.

/// Folder names offered under the field at most.
pub const SUGGESTIONS: usize = 8;

/// What the dialog says under the names.
pub const HINT: &str =
    "A path from the top (/), from here, or ~ for home. ↑ ↓ pick a folder; Return goes.";

/// The folder `typed` names, as an absolute path: from the top when it
/// starts with `/`, from `home` with `~` (where there is one), else from
/// `current`; `.` and `..` are followed and repeated or trailing `/` dropped.
pub fn resolve(current: &str, typed: &str, home: Option<&str>) -> String {
    let typed = typed.trim();
    let start = match typed {
        "~" => home.unwrap_or("/").to_owned(),
        _ if typed.starts_with("~/") => match home {
            Some(home) => format!("{home}/{}", &typed[2..]),
            None => typed[1..].to_owned(),
        },
        _ if typed.starts_with('/') => typed.to_owned(),
        _ => format!("{current}/{typed}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in start.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// What is typed, split where the last name begins: the folder it is in (as
/// typed, with its `/`) and the start of the name being typed.
pub fn split(typed: &str) -> (&str, &str) {
    match typed.rfind('/') {
        Some(slash) => (&typed[..=slash], &typed[slash + 1..]),
        None => ("", typed),
    }
}

/// The names in `names` that go on from `partial`, ignoring case, in name
/// order, at most `SUGGESTIONS`; hidden ones only once a `.` is typed.
pub fn suggestions<'a>(names: &'a [String], partial: &str) -> Vec<&'a str> {
    let wanted = partial.to_lowercase();
    let mut found: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| partial.starts_with('.') || !name.starts_with('.'))
        .filter(|name| name.to_lowercase().starts_with(&wanted))
        .collect();
    found.sort_by_cached_key(|name| name.to_lowercase());
    found.truncate(SUGGESTIONS);
    found
}

/// `typed` with the name being typed replaced by `name` and a `/`, ready for
/// the next.
pub fn complete(typed: &str, name: &str) -> String {
    let (folder, _) = split(typed);
    format!("{folder}{name}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_path_is_read_from_the_top_from_home_or_from_here() {
        let home = Some("/Users/me");
        assert_eq!(resolve("/DCIM", "/Music", home), "/Music");
        assert_eq!(resolve("/DCIM", "Camera", home), "/DCIM/Camera");
        assert_eq!(resolve("/DCIM", "Camera/", home), "/DCIM/Camera");
        assert_eq!(resolve("/", "DCIM//Camera/./", home), "/DCIM/Camera");
        assert_eq!(resolve("/DCIM/Camera", "..", home), "/DCIM");
        assert_eq!(resolve("/DCIM", "../../..", home), "/");
        assert_eq!(resolve("/x", "~", home), "/Users/me");
        assert_eq!(resolve("/x", "~/Desktop", home), "/Users/me/Desktop");
        assert_eq!(
            resolve("/x", "~/Desktop", None),
            "/Desktop",
            "the phone has no home"
        );
        assert_eq!(resolve("/x", "~", None), "/");
        assert_eq!(resolve("/DCIM", "  ", home), "/DCIM", "nothing typed: here");
    }

    #[test]
    fn the_name_being_typed_is_what_follows_the_last_slash() {
        assert_eq!(split("/DCIM/Cam"), ("/DCIM/", "Cam"));
        assert_eq!(split("/DCIM/"), ("/DCIM/", ""));
        assert_eq!(split("Cam"), ("", "Cam"));
        assert_eq!(complete("/DCIM/cam", "Camera"), "/DCIM/Camera/");
        assert_eq!(complete("Do", "Download"), "Download/");
    }

    #[test]
    fn suggestions_go_on_from_what_is_typed_ignoring_case() {
        let names: Vec<String> = [
            "Camera",
            "camera roll",
            ".thumbnails",
            "Screenshots",
            "Cache",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();

        assert_eq!(
            suggestions(&names, "ca"),
            ["Cache", "Camera", "camera roll"]
        );
        assert_eq!(
            suggestions(&names, ""),
            ["Cache", "Camera", "camera roll", "Screenshots"]
        );
        assert_eq!(
            suggestions(&names, "."),
            [".thumbnails"],
            "hidden once a dot is typed"
        );
        assert!(suggestions(&names, "x").is_empty());
        let many: Vec<String> = (0..20).map(|n| format!("Folder {n:02}")).collect();
        assert_eq!(suggestions(&many, "f").len(), SUGGESTIONS);
    }
}
