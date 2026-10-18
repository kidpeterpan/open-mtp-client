//! Name rules shared by the device and the Mac, so a folder made on one side
//! can always be copied to the other.

use crate::dto::{Error, NameProblem};

/// Characters a name typed for a new or renamed item may not have: the ones FAT
/// (an SD card) rejects, plus `/` and NUL. This only limits what the user types: the names of
/// files that already exist are copied exactly as they are, never changed.
const ILLEGAL: &[char] = &['/', '\\', '?', '*', ':', '|', '"', '<', '>', '\0'];

/// Returns the name to use: trimmed, or why it can't be used.
pub fn validate(name: &str) -> Result<&str, Error> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidName(NameProblem::Empty));
    }
    if name == "." || name == ".." {
        return Err(Error::InvalidName(NameProblem::Reserved));
    }
    if name.contains(ILLEGAL) {
        return Err(Error::InvalidName(NameProblem::IllegalCharacters));
    }
    Ok(name)
}

/// Whether a name reported by the device can be used as one path component
/// on the Mac. A device is not trusted: `../x`, `/etc/x` or `a/b` would
/// otherwise let a copy write outside the folder the user chose. Everything
/// else is a legal file name on macOS and is kept as it is, `?`, `"`, `:`, `#`,
/// emoji and `\` included (a backslash is only a separator on Windows, and
/// refusing it stopped a whole copy for one oddly named file).
pub fn is_safe_component(name: &str) -> bool {
    use std::path::{Component, Path};

    if name.is_empty() || name.contains(['/', '\0']) {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// `name`, or `name (n)` for n > 0: the number goes before a file's extension
/// (`IMG_001 (1).jpg`) and at the end of a folder's name. A leading dot is part
/// of the name, not an extension.
pub fn numbered(name: &str, n: usize, is_folder: bool) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_folder => {
            format!("{} ({n}){}", &name[..dot], &name[dot..])
        }
        _ => format!("{name} ({n})"),
    }
}

/// The first of `name (1)`, `name (2)`… that no name in `taken` has, ignoring
/// case as phones and the Mac do.
pub fn free_name(taken: &[String], name: &str, is_folder: bool) -> String {
    (1..)
        .map(|n| numbered(name, n, is_folder))
        .find(|candidate| !taken.iter().any(|t| same(t, candidate)))
        .unwrap_or_else(|| name.to_owned())
}

/// Android's shared storage and macOS's default APFS both ignore case, so
/// "Photos" and "photos" collide.
pub fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Finds a name among many the way `same` would, without comparing it with
/// each: a folder of 5,000 files asked about 5,000 times is 25 million
/// comparisons, each of which builds two strings.
pub struct Index(std::collections::HashMap<String, usize>);

impl Index {
    /// An index of `names`. Where two collide, the first is the one found, as
    /// a search from the start would find it.
    pub fn of<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut found = std::collections::HashMap::new();
        for (position, name) in names.into_iter().enumerate() {
            found.entry(name.to_lowercase()).or_insert(position);
        }
        Self(found)
    }

    /// Where in the names `name` is, ignoring case.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.0.get(&name.to_lowercase()).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_finds_a_name_ignoring_case() {
        let index = Index::of(["Camera", "notes.TXT", "Música"]);

        assert_eq!(index.find("camera"), Some(0));
        assert_eq!(index.find("NOTES.txt"), Some(1));
        assert_eq!(index.find("música"), Some(2));
        assert_eq!(index.find("ghost"), None);
        assert_eq!(Index::of([]).find("a"), None);
    }

    #[test]
    fn where_two_names_collide_the_first_is_found_as_a_search_would() {
        let names = ["a.jpg", "B.jpg", "A.JPG", "b.JPG"];
        let index = Index::of(names);

        for name in ["a.jpg", "A.jpg", "b.jpg"] {
            let searched = names.iter().position(|other| same(other, name));
            assert_eq!(index.find(name), searched, "{name}");
        }
        assert_eq!(index.find("a.jpg"), Some(0));
        assert_eq!(index.find("b.jpg"), Some(1));
    }

    #[test]
    fn asking_a_big_index_for_every_name_is_quick() {
        // 5,000 names asked 5,000 times: a search each time was 25 million
        // comparisons, each building two strings.
        let names: Vec<String> = (0..5_000).map(|i| format!("IMG_{i:05}.jpg")).collect();
        let index = Index::of(names.iter().map(String::as_str));
        let started = std::time::Instant::now();

        for (position, name) in names.iter().enumerate() {
            assert_eq!(index.find(&name.to_lowercase()), Some(position));
        }

        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_plain_name_is_kept() {
        assert_eq!(validate("Holiday 2026"), Ok("Holiday 2026"));
    }

    #[test]
    fn surrounding_spaces_are_trimmed() {
        assert_eq!(validate("  Holiday  "), Ok("Holiday"));
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(validate("   "), Err(Error::InvalidName(NameProblem::Empty)));
    }

    #[test]
    fn dot_names_are_reserved() {
        assert_eq!(
            validate("."),
            Err(Error::InvalidName(NameProblem::Reserved))
        );
        assert_eq!(
            validate(".."),
            Err(Error::InvalidName(NameProblem::Reserved))
        );
        assert_eq!(validate(".nomedia"), Ok(".nomedia"));
    }

    #[test]
    fn every_illegal_character_is_refused() {
        for c in ILLEGAL {
            assert_eq!(
                validate(&format!("a{c}b")),
                Err(Error::InvalidName(NameProblem::IllegalCharacters)),
                "{c} should be refused"
            );
        }
    }

    #[test]
    fn names_collide_regardless_of_case() {
        assert!(same("Photos", "photos"));
        assert!(!same("Photos", "Photo"));
    }

    #[test]
    fn ordinary_device_names_are_safe() {
        for name in ["IMG_001.jpg", ".nomedia", "a b", "ภาพ.jpg", "...", "x.."] {
            assert!(is_safe_component(name), "{name:?} should be safe");
        }
    }

    #[test]
    fn names_with_odd_characters_are_safe_to_copy_as_they_are() {
        for name in [
            "what?.jpg",
            "say \"hi\".txt",
            "100% sure.txt",
            "file #1.jpg",
            "a:b.txt",
            "a*b",
            "<tag>",
            "a|b",
            "a\\b.txt",
            "😀 trip 🌴.jpg",
            "e\u{301}.txt",
            " leading and trailing ",
            "ends with a dot.",
            "--",
            "~",
            "$HOME",
            "`cmd`",
            "-rf",
        ] {
            assert!(is_safe_component(name), "{name:?} should be safe");
        }
    }

    #[test]
    fn a_percent_sign_can_be_typed_in_a_new_name() {
        assert_eq!(validate("100% sure"), Ok("100% sure"));
        assert_eq!(validate("file #1"), Ok("file #1"));
        assert_eq!(validate("😀 trip"), Ok("😀 trip"));
    }

    #[test]
    fn names_that_could_escape_the_folder_are_unsafe() {
        for name in ["", ".", "..", "../x", "a/b", "/etc/passwd", "a\0b", "./x"] {
            assert!(!is_safe_component(name), "{name:?} should be unsafe");
        }
    }

    #[test]
    fn numbering_keeps_a_files_extension_and_ends_a_folders_name() {
        assert_eq!(numbered("IMG_001.jpg", 0, false), "IMG_001.jpg");
        assert_eq!(numbered("IMG_001.jpg", 2, false), "IMG_001 (2).jpg");
        assert_eq!(numbered("archive.tar.gz", 1, false), "archive.tar (1).gz");
        assert_eq!(numbered("README", 1, false), "README (1)");
        assert_eq!(numbered(".nomedia", 1, false), ".nomedia (1)");
        assert_eq!(
            numbered("v1.2", 1, true),
            "v1.2 (1)",
            "a folder has no extension"
        );
    }

    #[test]
    fn a_free_name_skips_every_taken_one_ignoring_case() {
        let taken = vec!["a.jpg".to_owned(), "A (1).JPG".to_owned()];

        assert_eq!(free_name(&taken, "a.jpg", false), "a (2).jpg");
        assert_eq!(free_name(&[], "DCIM", true), "DCIM (1)");
        assert_eq!(
            free_name(&["x".to_owned()], "y", false),
            "y (1)",
            "a free name still gets its number"
        );
    }
}
