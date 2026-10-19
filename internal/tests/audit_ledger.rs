use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use chrono::NaiveDate;

const LEDGER: &str = ".cargo/audit.toml";

const WORKFLOW: &str = ".github/workflows/security.yml";

const TRIGGER_PATHS: [&str; 4] = [
    "**/Cargo.toml",
    "Cargo.lock",
    ".github/workflows/security.yml",
    ".cargo/audit.toml",
];

const FONT_PARSER_ADVISORY: &str = "RUSTSEC-2026-0192";

const PLATFORM_SCOPES: [&str; 2] = ["macos-only", "any"];

const MAC_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

const ENTRY_KEYS: [&str; 9] = [
    "Crate",
    "Issue",
    "Accepted",
    "Review by",
    "Platform scope",
    "Reason",
    "Evidence",
    "Expect",
    "Remove when",
];

struct Entry {
    heading_id: String,
    quoted_id: String,
    fields: Vec<(String, String)>,
}

impl Entry {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

struct OpenEntry {
    heading_id: String,
    fields: Vec<(String, String)>,
}

struct Ledger {
    entries: Vec<Entry>,
    ids_outside_blocks: Vec<String>,
}

fn repository_file(relative_path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative_path);
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{relative_path} is missing or unreadable at {}: {error}",
            path.display()
        )
    })
}

fn heading_id(line: &str) -> Option<String> {
    let after_marker = line.strip_prefix("# --- ")?;
    let id = after_marker.split_whitespace().next()?;
    Some(id.to_owned())
}

fn quoted_id(line: &str) -> Option<String> {
    let after_opening_quote = line.strip_prefix('"')?;
    let id = after_opening_quote.split('"').next()?;
    Some(id.to_owned())
}

fn continuation_text(line: &str) -> Option<&str> {
    let after_marker = line.strip_prefix("#   ")?;
    Some(after_marker.trim())
}

fn field_start(line: &str) -> Option<(String, String)> {
    let after_marker = line.strip_prefix("# ")?;
    if after_marker.starts_with(' ') {
        return None;
    }
    let (key, value) = after_marker.split_once(':')?;
    Some((key.to_owned(), value.trim().to_owned()))
}

fn extend_last_field(entry: &mut OpenEntry, text: &str) {
    let heading_id = &entry.heading_id;
    let Some((_, value)) = entry.fields.last_mut() else {
        panic!("{heading_id} has a continuation line before any key: {text:?}");
    };
    value.push('\n');
    value.push_str(text);
}

fn parse_ledger(text: &str) -> Ledger {
    let mut entries = Vec::new();
    let mut ids_outside_blocks = Vec::new();
    let mut open_entry: Option<OpenEntry> = None;

    for line in text.lines().map(str::trim_start) {
        if let Some(id) = heading_id(line) {
            assert!(
                open_entry.is_none(),
                "{id} starts before the entry above it was closed by its quoted id"
            );
            open_entry = Some(OpenEntry {
                heading_id: id,
                fields: Vec::new(),
            });
            continue;
        }

        if let Some(id) = quoted_id(line) {
            match open_entry.take() {
                Some(entry) => entries.push(Entry {
                    heading_id: entry.heading_id,
                    quoted_id: id,
                    fields: entry.fields,
                }),
                None => ids_outside_blocks.push(id),
            }
            continue;
        }

        let Some(entry) = open_entry.as_mut() else {
            continue;
        };

        if let Some(text) = continuation_text(line) {
            extend_last_field(entry, text);
            continue;
        }

        let Some((key, value)) = field_start(line) else {
            panic!(
                "{} has a line that is not a field: {line:?}",
                entry.heading_id
            );
        };
        assert!(
            ENTRY_KEYS.contains(&key.as_str()),
            "{} has the unknown key {key:?}",
            entry.heading_id
        );
        entry.fields.push((key, value));
    }

    if let Some(entry) = open_entry {
        panic!("{} is never closed by a quoted id", entry.heading_id);
    }

    Ledger {
        entries,
        ids_outside_blocks,
    }
}

fn ledger() -> Ledger {
    parse_ledger(&repository_file(LEDGER))
}

fn all_digits(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_advisory_id(candidate: &str) -> bool {
    let Some(year_and_number) = candidate.strip_prefix("RUSTSEC-") else {
        return false;
    };
    let Some((year, number)) = year_and_number.split_once('-') else {
        return false;
    };
    all_digits(year, 4) && all_digits(number, 4)
}

fn is_issue_reference(candidate: &str) -> bool {
    candidate.strip_prefix('#').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn required_field<'entry>(entry: &'entry Entry, key: &str) -> &'entry str {
    let value = entry
        .field(key)
        .unwrap_or_else(|| panic!("{} has no {key}", entry.quoted_id));
    assert!(!value.is_empty(), "{} has an empty {key}", entry.quoted_id);
    value
}

#[test]
fn the_ledger_file_exists_and_holds_entries() {
    let ledger = ledger();

    assert!(!ledger.entries.is_empty(), "{LEDGER} holds no entries");
}

#[test]
fn ids_are_well_formed_unique_and_match_their_headings() {
    let ledger = ledger();

    assert!(
        ledger.ids_outside_blocks.is_empty(),
        "ids listed without a block: {:?}",
        ledger.ids_outside_blocks
    );

    let mut seen_ids = HashSet::new();
    for entry in &ledger.entries {
        assert!(
            is_advisory_id(&entry.quoted_id),
            "{:?} is not an advisory id",
            entry.quoted_id
        );
        assert_eq!(
            entry.heading_id, entry.quoted_id,
            "the heading and the quoted id of an entry differ"
        );
        assert!(
            seen_ids.insert(&entry.quoted_id),
            "{} is listed twice",
            entry.quoted_id
        );
    }
}

#[test]
fn every_entry_names_its_crate_issue_and_reason() {
    for entry in &ledger().entries {
        required_field(entry, "Crate");
        required_field(entry, "Reason");
        let issue = required_field(entry, "Issue");
        assert!(
            is_issue_reference(issue),
            "{} has the issue {issue:?}, expected # and digits",
            entry.quoted_id
        );
    }
}

fn on_one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn iso_date(entry: &Entry, key: &str) -> NaiveDate {
    let text = required_field(entry, key);
    let parsed = NaiveDate::parse_from_str(text, "%Y-%m-%d");
    match parsed {
        Ok(date) if text.len() == "YYYY-MM-DD".len() => date,
        _ => panic!(
            "{} has the {key} {text:?}, expected the date as YYYY-MM-DD",
            entry.quoted_id
        ),
    }
}

#[test]
fn every_entry_has_valid_dates_with_review_after_acceptance() {
    for entry in &ledger().entries {
        let accepted = iso_date(entry, "Accepted");
        let review_by = iso_date(entry, "Review by");
        assert!(
            review_by > accepted,
            "{} is reviewed on {review_by}, which is not after it was accepted on {accepted}",
            entry.quoted_id
        );
    }
}

#[test]
fn every_entry_has_scope_evidence_expectation_and_removal_condition() {
    for entry in &ledger().entries {
        let scope = required_field(entry, "Platform scope");
        assert!(
            PLATFORM_SCOPES.contains(&scope),
            "{} has the platform scope {scope:?}, expected one of {PLATFORM_SCOPES:?}",
            entry.quoted_id
        );
        required_field(entry, "Evidence");
        required_field(entry, "Expect");
        required_field(entry, "Remove when");
    }
}

#[test]
fn macos_only_entries_name_both_mac_targets_in_their_evidence() {
    for entry in &ledger().entries {
        if required_field(entry, "Platform scope") != "macos-only" {
            continue;
        }
        let evidence = required_field(entry, "Evidence");
        for target in MAC_TARGETS {
            assert!(
                evidence.contains(&format!("--target {target}")),
                "{} is accepted for macOS only but its evidence never uses --target {target}",
                entry.quoted_id
            );
        }
    }
}

#[test]
fn the_font_parser_advisory_is_accepted_with_the_routes_that_feed_it() {
    let ledger = ledger();
    let entry = ledger
        .entries
        .iter()
        .find(|entry| entry.quoted_id == FONT_PARSER_ADVISORY)
        .unwrap_or_else(|| panic!("{FONT_PARSER_ADVISORY} (ttf-parser) is not in the ledger"));

    let reason = on_one_line(required_field(entry, "Reason"));
    for route in ["system fonts", "Iced-Icons.ttf", "SVG"] {
        assert!(
            reason.contains(route),
            "the reason for {FONT_PARSER_ADVISORY} never mentions {route:?}"
        );
    }

    let evidence = on_one_line(required_field(entry, "Evidence"));
    for check in ["usvg", "forbid(unsafe_code)"] {
        assert!(
            evidence.contains(check),
            "the evidence for {FONT_PARSER_ADVISORY} never runs a check for {check:?}"
        );
    }
}

fn trigger_paths(workflow: &str, trigger: &str) -> Vec<String> {
    let trigger_line = format!("{trigger}:");
    let mut paths = Vec::new();
    let mut inside_trigger = false;
    let mut inside_paths = false;

    for line in workflow.lines().filter(|line| !line.trim().is_empty()) {
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        match indent {
            2 => {
                inside_trigger = text == trigger_line;
                inside_paths = false;
            }
            4 if inside_trigger => inside_paths = text == "paths:",
            6 if inside_paths => {
                if let Some(path) = text.strip_prefix("- ") {
                    paths.push(path.trim_matches('"').to_owned());
                }
            }
            _ => {}
        }
    }
    paths
}

#[test]
fn changes_to_the_ledger_start_the_security_check() {
    let workflow = repository_file(WORKFLOW);
    let mut expected = TRIGGER_PATHS.to_vec();
    expected.sort();

    for trigger in ["push", "pull_request"] {
        let mut paths = trigger_paths(&workflow, trigger);
        paths.sort();
        assert_eq!(
            paths, expected,
            "{WORKFLOW} must run on {trigger} for exactly these paths"
        );
    }
}

#[test]
fn the_workflow_has_no_second_list_of_ignored_advisories() {
    let workflow = repository_file(WORKFLOW);

    let ignore_inputs: Vec<&str> = workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("ignore:"))
        .collect();

    assert!(
        ignore_inputs.is_empty(),
        "{WORKFLOW} lists ignored advisories itself ({ignore_inputs:?}); {LEDGER} is the only list"
    );
}
