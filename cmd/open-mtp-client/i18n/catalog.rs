use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Text(&'static str),
    Slot(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
    plain: Option<&'static str>,
}

impl Template {
    fn from_parts(parts: Vec<Part>) -> Template {
        let has_slot = parts.iter().any(|part| matches!(part, Part::Slot(_)));
        let plain = (!has_slot).then(|| joined_text(&parts));
        Template { parts, plain }
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    pub fn plain(&self) -> Option<&'static str> {
        self.plain
    }
}

fn joined_text(parts: &[Part]) -> &'static str {
    match parts {
        [] => "",
        [Part::Text(only)] => only,
        several => {
            let joined: String = several
                .iter()
                .map(|part| match part {
                    Part::Text(text) | Part::Slot(text) => *text,
                })
                .collect();
            Box::leak(joined.into_boxed_str())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    MissingSeparator,
    BadKey,
    DuplicateKey,
    BadBrace,
    CarriageReturn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub kind: ParseErrorKind,
}

pub struct Catalog {
    templates: HashMap<&'static str, Template>,
}

impl Catalog {
    pub fn parse(source: &'static str) -> Result<Catalog, ParseError> {
        let mut templates = HashMap::new();
        for (index, line) in source.split('\n').enumerate() {
            let at = |kind| ParseError {
                line: index + 1,
                kind,
            };
            if line.contains('\r') {
                return Err(at(ParseErrorKind::CarriageReturn));
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(" = ")
                .ok_or_else(|| at(ParseErrorKind::MissingSeparator))?;
            if !is_key(key) {
                return Err(at(ParseErrorKind::BadKey));
            }
            let template = parse_template(value).ok_or_else(|| at(ParseErrorKind::BadBrace))?;
            if templates.insert(key, template).is_some() {
                return Err(at(ParseErrorKind::DuplicateKey));
            }
        }
        Ok(Catalog { templates })
    }

    pub fn get(&self, key: &str) -> Option<&Template> {
        self.templates.get(key)
    }

    #[cfg(test)]
    pub fn keys(&self) -> Vec<&'static str> {
        self.templates.keys().copied().collect()
    }
}

fn is_key(key: &str) -> bool {
    let is_word_letter =
        |letter: char| letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_';
    key.split('.')
        .all(|word| !word.is_empty() && word.chars().all(is_word_letter))
}

fn is_slot_name(name: &str) -> bool {
    let mut letters = name.chars();
    let starts_right = letters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase());
    starts_right
        && letters
            .all(|letter| letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_')
}

fn parse_template(value: &'static str) -> Option<Template> {
    let bytes = value.as_bytes();
    let mut parts = Vec::new();
    let mut text_from = 0;
    let mut index = 0;
    while index < bytes.len() {
        let brace = bytes[index];
        let doubled = bytes.get(index + 1) == Some(&brace);
        match brace {
            b'{' | b'}' if doubled => {
                parts.push(Part::Text(&value[text_from..=index]));
                index += 2;
                text_from = index;
            }
            b'{' => {
                let close = index + 1 + value[index + 1..].find('}')?;
                let name = &value[index + 1..close];
                if !is_slot_name(name) {
                    return None;
                }
                if text_from < index {
                    parts.push(Part::Text(&value[text_from..index]));
                }
                parts.push(Part::Slot(name));
                index = close + 1;
                text_from = index;
            }
            b'}' => return None,
            _ => index += 1,
        }
    }
    if text_from < value.len() {
        parts.push(Part::Text(&value[text_from..]));
    }
    Some(Template::from_parts(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "# the first comment\n\
        \n\
        transfer.done.title = Copied {count} files\n\
        transfer.done.title.one = Copied 1 file\n\
        error.no_device.title = No device found\n\
        # a comment between messages\n\
        month.1 = January\n";

    fn parsed(source: &'static str) -> Catalog {
        match Catalog::parse(source) {
            Ok(catalog) => catalog,
            Err(error) => panic!("{source:?} should parse, got {error:?}"),
        }
    }

    fn refused(source: &'static str) -> ParseError {
        match Catalog::parse(source) {
            Ok(_) => panic!("{source:?} should not parse"),
            Err(error) => error,
        }
    }

    fn text_of(catalog: &Catalog, key: &str) -> &'static str {
        catalog
            .get(key)
            .unwrap_or_else(|| panic!("no key {key}"))
            .plain()
            .unwrap_or_else(|| panic!("{key} has slots"))
    }

    #[test]
    fn blank_and_comment_lines_are_ignored() {
        let mut keys = parsed(SOURCE).keys();
        keys.sort_unstable();

        assert_eq!(
            keys,
            [
                "error.no_device.title",
                "month.1",
                "transfer.done.title",
                "transfer.done.title.one"
            ]
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_catalog() {
        assert!(parsed("").keys().is_empty());
        assert!(parsed("# only a comment\n").keys().is_empty());
    }

    #[test]
    fn a_value_is_kept_exactly_as_written() {
        let catalog = parsed(
            "a.spaced =   two  spaces  \n\
             a.quoted = \"quoted\" and 'single'\n\
             a.equals = x = y\n\
             a.thai = สวัสดี ครับ\n",
        );

        assert_eq!(text_of(&catalog, "a.spaced"), "  two  spaces  ");
        assert_eq!(text_of(&catalog, "a.quoted"), "\"quoted\" and 'single'");
        assert_eq!(text_of(&catalog, "a.equals"), "x = y");
        assert_eq!(text_of(&catalog, "a.thai"), "สวัสดี ครับ");
    }

    #[test]
    fn a_file_without_a_final_line_break_still_reads_its_last_line() {
        assert_eq!(text_of(&parsed("a.last = end"), "a.last"), "end");
    }

    #[test]
    fn keys_are_lower_case_words_joined_by_underscores_and_dots() {
        for good in [
            "a = x",
            "month.1 = x",
            "error.no_device.title = x",
            "a_b.c_d = x",
        ] {
            assert!(Catalog::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "Error.title = x",
            "error.Title = x",
            "a..b = x",
            ".a = x",
            "a. = x",
            "a-b = x",
            "a b = x",
            "é = x",
            " = x",
        ] {
            let error = refused(bad);
            assert_eq!(error.kind, ParseErrorKind::BadKey, "{bad}");
            assert_eq!(error.line, 1, "{bad}");
        }
    }

    #[test]
    fn a_repeated_key_is_an_error_on_the_line_that_repeats_it() {
        let error = refused("a.x = one\n# note\na.x = two\n");

        assert_eq!(
            error,
            ParseError {
                line: 3,
                kind: ParseErrorKind::DuplicateKey
            }
        );
    }

    #[test]
    fn a_line_without_a_separator_is_an_error() {
        for bad in [
            "a.x",
            "a.x=value",
            "a.x =value",
            "a.x= value",
            "just words",
            "  ",
        ] {
            let error = refused(bad);
            assert_eq!(error.kind, ParseErrorKind::MissingSeparator, "{bad:?}");
        }
    }

    #[test]
    fn a_carriage_return_is_an_error() {
        let error = refused("a.x = one\r\na.y = two\r\n");

        assert_eq!(
            error,
            ParseError {
                line: 1,
                kind: ParseErrorKind::CarriageReturn
            }
        );
    }

    #[test]
    fn a_name_in_braces_is_a_slot() {
        let catalog = parsed("a.x = Copied {count} of {total_files} files\n");

        assert_eq!(
            catalog.get("a.x").unwrap().parts(),
            [
                Part::Text("Copied "),
                Part::Slot("count"),
                Part::Text(" of "),
                Part::Slot("total_files"),
                Part::Text(" files"),
            ]
        );
        assert_eq!(catalog.get("a.x").unwrap().plain(), None);
    }

    #[test]
    fn a_slot_may_start_or_end_the_value() {
        let catalog = parsed("a.x = {name}\na.y = {a}{b}\n");

        assert_eq!(catalog.get("a.x").unwrap().parts(), [Part::Slot("name")]);
        assert_eq!(
            catalog.get("a.y").unwrap().parts(),
            [Part::Slot("a"), Part::Slot("b")]
        );
    }

    #[test]
    fn doubled_braces_are_literal_braces() {
        let catalog = parsed("a.x = use {{name}} and {{ or }} here\na.y = {{{name}}}\n");

        assert_eq!(text_of(&catalog, "a.x"), "use {name} and { or } here");
        let joined: String = catalog
            .get("a.y")
            .unwrap()
            .parts()
            .iter()
            .map(|part| match part {
                Part::Text(text) => text.to_string(),
                Part::Slot(name) => format!("<{name}>"),
            })
            .collect();
        assert_eq!(joined, "{<name>}");
    }

    #[test]
    fn a_brace_that_is_not_a_slot_or_a_pair_is_an_error() {
        for bad in [
            "a.x = open {",
            "a.x = close }",
            "a.x = {name",
            "a.x = {}",
            "a.x = {Name}",
            "a.x = {1st}",
            "a.x = {two words}",
            "a.x = {na-me}",
            "a.x = {{{",
            "a.x = }}}",
        ] {
            let error = refused(bad);
            assert_eq!(error.kind, ParseErrorKind::BadBrace, "{bad}");
        }
    }

    #[test]
    fn a_count_variant_is_an_ordinary_key() {
        let catalog = parsed(SOURCE);

        assert_eq!(
            text_of(&catalog, "transfer.done.title.one"),
            "Copied 1 file"
        );
        assert!(catalog.get("transfer.done.title").is_some());
    }

    #[test]
    fn a_missing_key_is_none() {
        assert!(parsed(SOURCE).get("no.such.key").is_none());
    }

    #[test]
    fn every_piece_of_text_is_a_slice_of_the_source() {
        let source: &'static str =
            "a.x = Copied {count} files\na.y = no slots here\na.z = {name} done\n";
        let catalog = parsed(source);
        let inside = source.as_ptr() as usize..source.as_ptr() as usize + source.len();

        for key in catalog.keys() {
            for part in catalog.get(key).unwrap().parts() {
                let piece = match part {
                    Part::Text(piece) | Part::Slot(piece) => *piece,
                };
                let start = piece.as_ptr() as usize;
                assert!(
                    inside.contains(&start) && start + piece.len() <= inside.end,
                    "{key}: {piece:?} lies outside the source"
                );
            }
        }
        assert!(inside.contains(&(text_of(&catalog, "a.y").as_ptr() as usize)));
    }
}
