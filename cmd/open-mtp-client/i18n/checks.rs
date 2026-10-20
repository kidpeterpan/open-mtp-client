use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use internal::dto::Language;

use super::SOURCES;
use super::catalog::{Catalog, Part, Template};
use super::scan::{Kind, Literal, Scanned, classify, scan};

const SEED_KEYS: [&str; 2] = ["settings.language.title", "label.cancel"];
const LONGEST_THAI_RUN_IN_A_MESSAGE: usize = 20;
const LONGEST_THAI_RUN_IN_A_LABEL: usize = 14;
const LONGEST_THAI_RUN_IN_A_TITLE: usize = 12;
const LABEL_PREFIXES: [&str; 2] = ["label.", "button."];
const FILES_THAT_ARE_ONLY_TESTS: [&str; 4] = [
    "handler/testing.rs",
    "i18n/checks.rs",
    "i18n/scan.rs",
    "i18n/screens.rs",
];
const PRODUCT_PHRASES: &[&str] = &[
    "github.com/kidpeterpan/open-mtp-client",
    "open-mtp-client-cli",
    "open-mtp-client",
    "mtp-rs",
    "Android File Transfer Agent",
    "Android File Transfer",
    "Image Capture",
    "Activity Monitor",
    "Photos",
    "Garmin Express",
    "MightyText",
    "Commander One",
    "MacDroid",
    "DaVinci Resolve",
    "Nikon Transfer",
    "IMG_2026…2133.jpg",
    "--replace-older",
    "Android/data",
    "Android/obb",
    "list, pull, push",
    "JSON",
    "AirDrop",
    "Glass",
    "Basso",
    "Dock",
    "Page Up",
    "Page Down",
    "Home",
    "End",
    "mkdir -p",
    "--yes",
    "--move",
    "ctrl-c",
    "exit 130",
    "stderr",
    "Application Support",
    "crashes",
];
const LATIN_WORDS_THAT_STAY: &[&str] = &[
    "Android", "macOS", "Mac", "Finder", "USB", "MTP", "KB", "MB", "GB", "TB", "B", "s", "Mb",
    "Gb", "NTFS", "FAT", "APFS", "exFAT", "SD", "MS", "DOS", "Return", "App", "Rust", "iced",
    "DCIM", "Esc", "I", "WhatsApp", "Terminal", "sudo", "Samsung", "Smart", "Switch", "Google",
    "Drive", "Dropbox", "Photos", "Image", "Capture", "File", "Transfer", "GitHub", "esc", "X",
    "info", "rm", "mv", "pull",
];

fn catalogs() -> Vec<(Language, Catalog)> {
    Language::ALL
        .into_iter()
        .zip(SOURCES)
        .map(|(language, source)| match Catalog::parse(source) {
            Ok(catalog) => (language, catalog),
            Err(error) => panic!("{language:?} does not parse: {error:?}"),
        })
        .collect()
}

fn english(catalogs: &[(Language, Catalog)]) -> &Catalog {
    &catalogs[0].1
}

fn others(catalogs: &[(Language, Catalog)]) -> &[(Language, Catalog)] {
    &catalogs[1..]
}

fn template<'catalog>(catalog: &'catalog Catalog, key: &str) -> &'catalog Template {
    catalog
        .get(key)
        .unwrap_or_else(|| panic!("{key} is listed but cannot be read"))
}

fn slots_of(template: &Template) -> BTreeSet<&'static str> {
    template
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Slot(name) => Some(*name),
            Part::Text(_) => None,
        })
        .collect()
}

fn is_thai(letter: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&letter)
}

fn longest_thai_run(template: &Template) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for part in template.parts() {
        let Part::Text(text) = part else {
            current = 0;
            continue;
        };
        for letter in text.chars() {
            if is_thai(letter) {
                current += 1;
                longest = longest.max(current);
            } else {
                current = 0;
            }
        }
    }
    longest
}

fn is_line_break(letter: char) -> bool {
    matches!(letter, '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}')
}

const MODIFIER_KEYS: [char; 4] = ['⌘', '⌃', '⌥', '⇧'];

fn without_shortcuts(text: &str) -> String {
    let mut kept = String::new();
    let mut letters = text.chars().peekable();
    while let Some(letter) = letters.next() {
        if !MODIFIER_KEYS.contains(&letter) {
            kept.push(letter);
            continue;
        }
        while letters
            .next_if(|next| MODIFIER_KEYS.contains(next))
            .is_some()
        {}
        letters.next_if(char::is_ascii_alphabetic);
        kept.push(' ');
    }
    kept
}

fn latin_words_left_in(template: &Template) -> Vec<String> {
    let mut words = Vec::new();
    for part in template.parts() {
        let Part::Text(text) = part else {
            continue;
        };
        let mut rest = without_shortcuts(text);
        for phrase in PRODUCT_PHRASES.iter() {
            rest = rest.replace(phrase, " ");
        }
        words.extend(
            rest.split(|letter: char| !letter.is_ascii_alphabetic())
                .filter(|word| !word.is_empty())
                .filter(|word| !LATIN_WORDS_THAT_STAY.contains(word))
                .map(str::to_string),
        );
    }
    words
}

fn nothing_found(problems: Vec<String>) {
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn the_four_files_parse() {
    assert_eq!(catalogs().len(), 4);
}

#[test]
fn the_seed_messages_exist_in_all_four_languages() {
    let catalogs = catalogs();
    let mut problems = Vec::new();

    for (language, catalog) in &catalogs {
        for key in SEED_KEYS {
            if catalog.get(key).is_none() {
                problems.push(format!("{language:?} lacks {key}"));
            }
        }
    }

    nothing_found(problems);
}

#[derive(Debug, PartialEq, Eq)]
struct Problem {
    language: Language,
    key: String,
    what: &'static str,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?} {}: {}", self.language, self.key, self.what)
    }
}

fn problem(language: Language, key: &str, what: &'static str) -> Problem {
    Problem {
        language,
        key: key.to_owned(),
        what,
    }
}

fn check_catalogs(files: &[(Language, Catalog)]) -> Vec<Problem> {
    let (english_language, english) = &files[0];
    let english_keys: BTreeSet<&str> = english
        .keys()
        .into_iter()
        .filter(|key| !key.ends_with(".one"))
        .collect();
    let mut problems = Vec::new();

    for key in english.keys() {
        if let Some(plain) = key.strip_suffix(".one")
            && english.get(plain).is_none()
        {
            problems.push(problem(
                *english_language,
                key,
                "has no plain message beside it",
            ));
        }
    }
    for (language, catalog) in files {
        for key in catalog.keys() {
            let template = template(catalog, key);
            if template.parts().is_empty() {
                problems.push(problem(*language, key, "is empty"));
            }
            if template.parts().iter().any(|part| match part {
                Part::Text(text) => text.chars().any(is_line_break),
                Part::Slot(_) => false,
            }) {
                problems.push(problem(*language, key, "holds a line break"));
            }
            if language != english_language && key.ends_with(".one") {
                problems.push(problem(
                    *language,
                    key,
                    "is a count variant, only English has them",
                ));
            }
        }
    }
    for (language, catalog) in &files[1..] {
        let keys: BTreeSet<&str> = catalog.keys().into_iter().collect();
        for missing in english_keys.difference(&keys) {
            problems.push(problem(*language, missing, "is missing"));
        }
        for extra in keys.difference(&english_keys) {
            if !extra.ends_with(".one") {
                problems.push(problem(*language, extra, "is not in English"));
            }
        }
        for key in &english_keys {
            if let Some(found) = catalog.get(key)
                && slots_of(found) != slots_of(template(english, key))
            {
                problems.push(problem(*language, key, "has other slots than English"));
            }
        }
    }
    problems
}

#[test]
fn the_four_files_agree_on_their_messages_slots_and_form() {
    let problems: Vec<String> = check_catalogs(&catalogs())
        .iter()
        .map(Problem::to_string)
        .collect();

    nothing_found(problems);
}

fn fabricated(english: &'static str, thai: &'static str) -> Vec<(Language, Catalog)> {
    let without_count_variants: &'static str = Box::leak(
        english
            .lines()
            .filter(|line| {
                !line
                    .split(" = ")
                    .next()
                    .is_some_and(|key| key.ends_with(".one"))
            })
            .map(|line| format!("{line}\n"))
            .collect::<String>()
            .into_boxed_str(),
    );
    let parse = |source| Catalog::parse(source).expect("the fabricated file parses");
    vec![
        (Language::English, parse(english)),
        (Language::Thai, parse(thai)),
        (Language::Japanese, parse(without_count_variants)),
        (Language::ChineseSimplified, parse(without_count_variants)),
    ]
}

#[test]
fn a_key_missing_in_thai_is_reported_with_the_key_and_the_language() {
    let files = fabricated("a.first = A\nb.second = B\n", "a.first = ก\n");

    assert_eq!(
        check_catalogs(&files),
        [problem(Language::Thai, "b.second", "is missing")]
    );
    assert_eq!(
        check_catalogs(&files)[0].to_string(),
        "Thai b.second: is missing"
    );
}

#[test]
fn a_changed_slot_is_reported_with_the_key() {
    let files = fabricated("a.x = Copied {count} files\n", "a.x = คัดลอก {number} ไฟล์\n");

    assert_eq!(
        check_catalogs(&files),
        [problem(
            Language::Thai,
            "a.x",
            "has other slots than English"
        )]
    );
}

#[test]
fn an_empty_value_is_reported() {
    let files = fabricated("a.x = Hello\n", "a.x = \n");

    assert_eq!(
        check_catalogs(&files),
        [problem(Language::Thai, "a.x", "is empty")]
    );
}

#[test]
fn a_key_only_another_language_has_and_a_count_variant_there_are_reported() {
    let files = fabricated("a.x = Hello\n", "a.x = สวัสดี\nb.y = ก\na.x.one = สวัสดี\n");

    assert_eq!(
        check_catalogs(&files),
        [
            problem(
                Language::Thai,
                "a.x.one",
                "is a count variant, only English has them"
            ),
            problem(Language::Thai, "b.y", "is not in English"),
        ]
    );
}

#[test]
fn a_count_variant_without_its_plain_message_is_reported_in_english() {
    let files = fabricated("a.x.one = One\n", "");

    assert!(check_catalogs(&files).contains(&problem(
        Language::English,
        "a.x.one",
        "has no plain message beside it"
    )));
}

#[test]
fn a_complete_set_reports_nothing() {
    let files = fabricated(
        "a.x = Copied {count} files\na.x.one = Copied 1 file\nb.y = Hello\n",
        "a.x = คัดลอก {count} ไฟล์\nb.y = สวัสดี\n",
    );

    assert_eq!(check_catalogs(&files), []);
}

#[test]
fn thai_has_a_space_between_phrases_so_a_line_can_break() {
    let catalogs = catalogs();
    let thai = &catalogs[1].1;
    let mut problems = Vec::new();

    for key in thai.keys() {
        let is_label = LABEL_PREFIXES.iter().any(|prefix| key.starts_with(prefix));
        let most = if is_label {
            LONGEST_THAI_RUN_IN_A_LABEL
        } else if key.ends_with(".title") {
            LONGEST_THAI_RUN_IN_A_TITLE
        } else {
            LONGEST_THAI_RUN_IN_A_MESSAGE
        };
        let run = longest_thai_run(template(thai, key));
        if run > most {
            problems.push(format!(
                "Thai {key}: {run} Thai characters in a row, at most {most}"
            ));
        }
    }

    nothing_found(problems);
}

#[test]
fn no_other_language_has_english_words_left_in_it() {
    let catalogs = catalogs();
    let mut problems = Vec::new();

    for (language, catalog) in others(&catalogs) {
        for key in catalog.keys() {
            let left = latin_words_left_in(template(catalog, key));
            if !left.is_empty() {
                problems.push(format!("{language:?} {key}: {}", left.join(" ")));
            }
        }
    }

    nothing_found(problems);
}

#[test]
fn english_in_another_language_is_found_but_product_names_and_units_are_not() {
    let source: &'static str = "a.x = สวัสดี Cancel the copy\n\
        a.y = เปิด open-mtp-client บน Mac และ Android กับ open-mtp-client-cli\n\
        a.z = {count} MB/s ผ่าน USB\n\
        a.w = ความเร็ว {speed}\n\
        a.v = กด ⌘C แล้ว ⇧⌘N หรือ ⌥⌘C\n\
        a.u = กด ⌘Copy\n";
    let catalog = Catalog::parse(source).unwrap();

    assert_eq!(
        latin_words_left_in(template(&catalog, "a.x")),
        ["Cancel", "the", "copy"]
    );
    for key in ["a.y", "a.z", "a.w", "a.v"] {
        assert!(
            latin_words_left_in(template(&catalog, key)).is_empty(),
            "{key}"
        );
    }
    assert_eq!(
        latin_words_left_in(template(&catalog, "a.u")),
        ["opy"],
        "only the one key letter after a modifier is a shortcut"
    );
}

#[test]
fn a_thai_run_stops_at_a_space_a_slot_or_any_other_letter() {
    let source: &'static str =
        "a.x = กขคง จฉชซ\na.y = กขคง{n}จฉชซ\na.z = กขคงabcจฉชซ\na.w = กขคงจฉชซญฎ\n";
    let catalog = Catalog::parse(source).unwrap();
    let runs: Vec<usize> = ["a.x", "a.y", "a.z", "a.w"]
        .iter()
        .map(|key| longest_thai_run(template(&catalog, key)))
        .collect();

    assert_eq!(runs, [4, 4, 4, 10]);
}

fn collect_sources(root: &Path, directory: &Path, found: &mut Vec<(String, String)>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            collect_sources(root, &path, found);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("inside the app's folder")
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        found.push((relative, source));
    }
}

fn app_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    collect_sources(root, root, &mut found);
    found.sort();
    found
}

fn scanned_app() -> Vec<(String, Scanned)> {
    app_sources()
        .into_iter()
        .filter(|(file, _)| !FILES_THAT_ARE_ONLY_TESTS.contains(&file.as_str()))
        .map(|(file, source)| (file, scan(&source)))
        .collect()
}

fn messages_missing_from(source: &str, english: &Catalog) -> Vec<String> {
    scan(source)
        .literals
        .iter()
        .filter(|literal| classify(literal) == Kind::CatalogKey)
        .filter(|literal| english.get(&literal.text).is_none())
        .map(|literal| format!("line {}: {:?}", literal.line, literal.text))
        .collect()
}

fn wording_in(scanned: &Scanned) -> Vec<&Literal> {
    scanned
        .literals
        .iter()
        .filter(|literal| classify(literal) == Kind::Wording)
        .collect()
}

#[test]
fn every_message_the_code_asks_for_exists_in_english() {
    let catalogs = catalogs();
    let mut problems = Vec::new();

    for (file, source) in app_sources() {
        if FILES_THAT_ARE_ONLY_TESTS.contains(&file.as_str()) {
            continue;
        }
        for missing in messages_missing_from(&source, english(&catalogs)) {
            problems.push(format!("{file} {missing} is not in en.txt"));
        }
    }

    nothing_found(problems);
}

#[test]
fn the_scan_for_messages_finds_one_the_english_file_lacks() {
    let catalogs = catalogs();
    let source = "fn f() { t(\"label.cancel\"); tf(\"no.such.message\", &[]); count(\"label.also_missing\", 2, &[]); key(\"label.cancel\"); }";

    assert_eq!(
        messages_missing_from(source, english(&catalogs)),
        [
            "line 1: \"no.such.message\"",
            "line 1: \"label.also_missing\""
        ]
    );
}

#[test]
fn only_the_known_files_are_test_only_modules() {
    let mut declared: Vec<String> = scanned_app()
        .into_iter()
        .flat_map(|(file, scanned)| {
            let folder = Path::new(&file)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            scanned.test_modules.into_iter().map(move |module| {
                folder
                    .join(format!("{module}.rs"))
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    declared.sort();

    let mut known: Vec<String> = FILES_THAT_ARE_ONLY_TESTS
        .iter()
        .map(|file| file.to_string())
        .collect();
    known.sort();
    assert_eq!(
        declared, known,
        "a test-only module the wording scan would read, or one it skips that no longer exists"
    );
}

fn readme() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn the_readme_tells_people_how_to_correct_a_word_or_add_a_language() {
    let readme = readme();
    let section = readme
        .split("\n## ")
        .find(|part| part.starts_with("Languages"))
        .expect("README.md has a `## Languages` section");
    let mut missing = Vec::new();

    for needed in [
        "cmd/open-mtp-client/i18n/locales/",
        "en.txt",
        "th.txt",
        "ja.txt",
        "zh-Hans.txt",
        "{name}",
        "space between phrases",
        "make test",
    ] {
        if !section.contains(needed) {
            missing.push(format!("the Languages section does not mention {needed}"));
        }
    }
    let table_row = readme
        .lines()
        .find(|line| line.contains("More languages (Thai first)"))
        .expect("the feature table has a row for languages");
    if !table_row.contains('✅') {
        missing.push(format!(
            "the feature table does not show it done: {table_row}"
        ));
    }

    nothing_found(missing);
}

const AUDITED_WIDTHS: &[(&str, usize)] = &[
    ("app.rs", 1),
    ("handler/device/handler.rs", 4),
    ("handler/diagnosis.rs", 1),
    ("handler/explorer/handler.rs", 3),
    ("handler/explorer/rename_many.rs", 1),
    ("handler/help/handler.rs", 2),
    ("handler/import/handler.rs", 1),
    ("handler/info/handler.rs", 3),
    ("handler/mirror/handler.rs", 2),
    ("handler/settings/handler.rs", 1),
    ("handler/space/handler.rs", 1),
    ("handler/transfer/handler.rs", 1),
];

fn before_the_tests(source: &str) -> &str {
    source
        .find("#[cfg(test)]\nmod tests {")
        .map_or(source, |end| &source[..end])
}

fn numeric_widths_in(source: &str) -> usize {
    let code = before_the_tests(source);
    [".width(", ".min_width(", ".max_width("]
        .iter()
        .map(|call| {
            code.match_indices(call)
                .filter(|(at, _)| {
                    let after = code[at + call.len()..].trim_start();
                    let after = after.strip_prefix("Length::Fixed(").unwrap_or(after);
                    after.starts_with(|first: char| first.is_ascii_digit())
                })
                .count()
        })
        .sum()
}

#[test]
#[ignore = "prints how many numeric widths each file has, for the audited list"]
fn print_the_numeric_widths() {
    for (file, source) in app_sources() {
        let found = numeric_widths_in(&source);
        if found > 0 && !FILES_THAT_ARE_ONLY_TESTS.contains(&file.as_str()) {
            println!("    (\"{file}\", {found}),");
        }
    }
}

#[test]
fn a_fixed_width_is_looked_at_before_it_is_added() {
    let allowed: BTreeMap<&str, usize> = AUDITED_WIDTHS.iter().copied().collect();
    let mut problems = Vec::new();

    for (file, source) in app_sources() {
        if FILES_THAT_ARE_ONLY_TESTS.contains(&file.as_str()) {
            continue;
        }
        let found = numeric_widths_in(&source);
        let audited = allowed.get(file.as_str()).copied().unwrap_or(0);
        if found != audited {
            problems.push(format!(
                "{file} has {found} numeric widths and {audited} were looked at: draw it in every language (language_check) and record the verdict in research.md"
            ));
        }
    }

    nothing_found(problems);
}

#[test]
fn a_numeric_width_is_found_outside_the_tests_only() {
    let source = "fn view() { a.width(18); b.max_width(Length::Fixed(480.0)); c.width(Length::Fill); d.width(x); e.min_width( 3 ); }\n#[cfg(test)]\nmod tests {\n fn t() { a.width(5); }\n}\n";

    assert_eq!(numeric_widths_in(source), 3);
}

const HANDLERS_TOLD_NOTHING: &[(&str, &str)] = &[];

fn handler_folders() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("handler");
    fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()))
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn handlers_told_in(app: &str) -> BTreeSet<String> {
    let aliases: BTreeMap<&str, &str> = app
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("use crate::handler::")?;
            let (folder, alias) = line.trim_end_matches(';').split_once(" as ")?;
            Some((alias, folder))
        })
        .collect();
    let field_types: BTreeMap<&str, &str> = app
        .lines()
        .filter_map(|line| {
            let (field, kind) = line.trim().strip_suffix("::Handler,")?.split_once(": ")?;
            Some((field, kind))
        })
        .collect();
    let body_start = app
        .find("fn apply_language(")
        .expect("App has apply_language");
    let body = &app[body_start..];
    let body = &body[..body.find("\n    }\n").expect("apply_language ends")];
    body.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("self.")?
                .strip_suffix(".language_changed();")
        })
        .filter_map(|field| field_types.get(field).copied())
        .map(|kind| aliases.get(kind).copied().unwrap_or(kind).to_string())
        .collect()
}

#[test]
fn every_handler_is_told_when_the_language_changes() {
    let app = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("app.rs"))
        .expect("app.rs can be read");
    let told = handlers_told_in(&app);
    let mut problems = Vec::new();

    for folder in handler_folders() {
        let excused = HANDLERS_TOLD_NOTHING
            .iter()
            .any(|(excused, _)| *excused == folder);
        match (told.contains(&folder), excused) {
            (false, false) => problems.push(format!(
                "handler/{folder} is not told in App::apply_language, and is not excused"
            )),
            (true, true) => problems.push(format!(
                "handler/{folder} is told in App::apply_language, so it is not excused any more"
            )),
            _ => {}
        }
    }
    for folder in &told {
        if !handler_folders().contains(folder) {
            problems.push(format!("{folder} is told but is not a handler folder"));
        }
    }

    nothing_found(problems);
}

#[test]
fn a_handler_nobody_tells_is_found() {
    let app = "use crate::handler::space as freeing;\n    local_pane: explorer::Handler,\n    space: freeing::Handler,\n    help: help::Handler,\n    fn apply_language(&mut self) {\n        self.local_pane.language_changed();\n        self.space.language_changed();\n    }\n";

    let told: Vec<String> = handlers_told_in(app).into_iter().collect();

    assert_eq!(
        told,
        ["explorer", "space"],
        "help is not told, the alias is read"
    );
}

#[test]
fn no_wording_is_hard_coded_in_the_app() {
    let mut problems = Vec::new();

    for (file, scanned) in scanned_app() {
        for literal in wording_in(&scanned) {
            problems.push(format!(
                "{file}:{}: {:?} is wording: give it a key in the four files, or mark it english_only if it is meant to stay English",
                literal.line, literal.text
            ));
        }
    }

    nothing_found(problems);
}

#[test]
#[ignore = "prints every hard-coded word the scan finds, with its file and line"]
fn print_every_hard_coded_word() {
    println!();
    for (file, scanned) in scanned_app() {
        for literal in wording_in(&scanned) {
            println!("{file}:{}: {:?}", literal.line, literal.text);
        }
    }
}

#[test]
#[ignore = "prints every hard-coded word in the library crate, which the app's scan does not cover"]
fn print_every_hard_coded_word_in_the_library() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../internal");
    let mut found = Vec::new();
    collect_sources(&root, &root, &mut found);
    found.sort();
    let scanned: Vec<(String, Scanned)> = found
        .iter()
        .map(|(file, source)| (file.clone(), scan(source)))
        .collect();
    let test_only: BTreeSet<String> = scanned
        .iter()
        .flat_map(|(file, scanned)| {
            let folder = Path::new(file)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            scanned.test_modules.iter().map(move |module| {
                folder
                    .join(format!("{module}.rs"))
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    for (file, scanned) in &scanned {
        if file.starts_with("tests/") || test_only.contains(file) {
            continue;
        }
        for literal in wording_in(scanned) {
            println!("{file}:{}: {:?}", literal.line, literal.text);
        }
    }
}
