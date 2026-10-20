#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callee {
    pub name: String,
    pub path_prefix: Option<String>,
    pub is_method: bool,
    pub is_macro: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub line: usize,
    pub text: String,
    pub in_attribute: bool,
    pub callee: Option<Callee>,
    pub is_first_argument: bool,
    pub is_match_pattern: bool,
    pub is_english_only: bool,
}

#[derive(Debug, Default)]
pub struct Scanned {
    pub literals: Vec<Literal>,
    pub test_modules: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    CatalogKey,
    NotWording,
    Wording,
}

const CATALOG_CALLS: [&str; 4] = ["t", "tf", "count", "key"];

const SHOWN_AS_TEXT: [&str; 4] = ["text", "label", "text_input", "tooltip"];

const DEVELOPER_MACROS: [&str; 15] = [
    "panic",
    "unreachable",
    "unimplemented",
    "todo",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "eprintln",
    "eprint",
    "println",
    "print",
    "dbg",
];

const COMPILE_TIME_MACROS: [&str; 3] = ["include_str", "include_bytes", "env"];

const DEVELOPER_METHODS: [&str; 2] = ["expect", "expect_err"];

const PROCESS_METHODS: [&str; 5] = ["arg", "args", "env", "env_remove", "current_dir"];

const PATH_METHODS: [&str; 3] = ["join", "with_extension", "with_file_name"];

const PATH_CONSTRUCTORS: [(&str, &str); 7] = [
    ("Command", "new"),
    ("Path", "new"),
    ("PathBuf", "from"),
    ("OsStr", "new"),
    ("OsString", "from"),
    ("env", "var"),
    ("env", "var_os"),
];

const UNITS_AND_PRODUCT_NAMES: [&str; 13] = [
    "B",
    "KB",
    "MB",
    "GB",
    "TB",
    "USB",
    "USB 1",
    "USB 2",
    "USB 3",
    "USB 3 (10 Gb/s)",
    "MTP",
    "Android",
    "open-mtp-client",
];

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Punct(char),
}

struct Spanned {
    token: Token,
    line: usize,
}

struct Frame {
    open: char,
    callee: Option<Callee>,
    attribute_from: Option<usize>,
}

struct Skipping {
    base_depth: usize,
    first_token: usize,
    saw_equals: bool,
}

pub fn scan(source: &str) -> Scanned {
    let tokens = lex(source);
    let mut scanned = Scanned::default();
    let mut frames: Vec<Frame> = Vec::new();
    let mut skipping: Option<Skipping> = None;

    for (index, spanned) in tokens.iter().enumerate() {
        match &spanned.token {
            Token::Str(text) => {
                if skipping.is_none() {
                    scanned
                        .literals
                        .push(literal_at(&tokens, index, text, &frames));
                }
            }
            Token::Punct(opener @ ('(' | '[' | '{')) => {
                frames.push(open_frame(&tokens, index, *opener));
            }
            Token::Punct(closer @ (')' | ']' | '}')) => {
                let frame = frames.pop();
                let ends_cfg_test_attribute = frame
                    .as_ref()
                    .and_then(|frame| frame.attribute_from)
                    .is_some_and(|from| is_cfg_test(&tokens[from..index]));
                if ends_cfg_test_attribute && skipping.is_none() {
                    skipping = Some(Skipping {
                        base_depth: frames.len(),
                        first_token: index + 1,
                        saw_equals: false,
                    });
                    continue;
                }
                if let Some(skip) = &skipping {
                    let left_the_block = frames.len() < skip.base_depth;
                    let closed_the_item =
                        *closer == '}' && frames.len() == skip.base_depth && !skip.saw_equals;
                    if left_the_block || closed_the_item {
                        let skip = skipping.take().expect("skipping was just checked");
                        note_test_module(&tokens[skip.first_token..=index], &mut scanned);
                    }
                }
            }
            Token::Punct(';') => {
                if let Some(skip) = &skipping
                    && frames.len() == skip.base_depth
                {
                    let skip = skipping.take().expect("skipping was just checked");
                    note_test_module(&tokens[skip.first_token..=index], &mut scanned);
                }
            }
            Token::Punct('=') => {
                if let Some(skip) = &mut skipping
                    && frames.len() == skip.base_depth
                {
                    skip.saw_equals = true;
                }
            }
            Token::Punct(_) | Token::Ident(_) => {}
        }
    }
    scanned
}

pub fn classify(literal: &Literal) -> Kind {
    if literal.in_attribute || literal.is_match_pattern || literal.is_english_only {
        return Kind::NotWording;
    }
    if let Some(callee) = &literal.callee {
        if literal.is_first_argument && is_catalog_call(callee) {
            return Kind::CatalogKey;
        }
        if is_not_user_text(callee) {
            return Kind::NotWording;
        }
    }
    let spoken = without_placeholders(&literal.text);
    if !has_ascii_letter(&spoken) || is_constant_name(&spoken) || is_path_like(&spoken) {
        return Kind::NotWording;
    }
    let shown_as_text = literal.is_first_argument
        && literal.callee.as_ref().is_some_and(|callee| {
            !callee.is_macro && SHOWN_AS_TEXT.contains(&callee.name.as_str())
        });
    if !(shown_as_text || spoken.contains(' ') || starts_with_capital(&spoken)) {
        return Kind::NotWording;
    }
    if UNITS_AND_PRODUCT_NAMES.contains(&literal.text.as_str()) {
        return Kind::NotWording;
    }
    Kind::Wording
}

fn is_catalog_call(callee: &Callee) -> bool {
    !callee.is_method && !callee.is_macro && CATALOG_CALLS.contains(&callee.name.as_str())
}

fn is_not_user_text(callee: &Callee) -> bool {
    let name = callee.name.as_str();
    if callee.is_macro {
        return DEVELOPER_MACROS.contains(&name) || COMPILE_TIME_MACROS.contains(&name);
    }
    if callee.is_method {
        return DEVELOPER_METHODS.contains(&name)
            || PROCESS_METHODS.contains(&name)
            || PATH_METHODS.contains(&name);
    }
    let prefix = callee.path_prefix.as_deref().unwrap_or_default();
    PATH_CONSTRUCTORS.contains(&(prefix, name))
}

fn without_placeholders(text: &str) -> String {
    let mut spoken = String::new();
    let mut letters = text.chars().peekable();
    while let Some(letter) = letters.next() {
        match letter {
            '{' if letters.peek() == Some(&'{') => {
                letters.next();
                spoken.push('{');
            }
            '{' => {
                for inside in letters.by_ref() {
                    if inside == '}' {
                        break;
                    }
                }
            }
            other => spoken.push(other),
        }
    }
    spoken
}

fn is_constant_name(text: &str) -> bool {
    text.contains('_')
        && text
            .chars()
            .all(|letter| letter.is_ascii_uppercase() || letter.is_ascii_digit() || letter == '_')
}

fn is_path_like(text: &str) -> bool {
    text.contains('/') && !text.contains(char::is_whitespace)
}

fn has_ascii_letter(text: &str) -> bool {
    text.chars().any(|letter| letter.is_ascii_alphabetic())
}

fn starts_with_capital(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|letter| letter.is_ascii_uppercase())
}

fn literal_at(tokens: &[Spanned], index: usize, text: &str, frames: &[Frame]) -> Literal {
    let innermost = frames.last();
    let is_first_argument = index > 0
        && innermost.is_some_and(|frame| tokens[index - 1].token == Token::Punct(frame.open));
    Literal {
        line: tokens[index].line,
        text: text.to_string(),
        in_attribute: frames.iter().any(|frame| frame.attribute_from.is_some()),
        callee: callee_of_literal(frames),
        is_first_argument,
        is_match_pattern: is_match_pattern(tokens, index, frames),
        is_english_only: frames.iter().any(is_english_only_call),
    }
}

fn is_match_pattern(tokens: &[Spanned], index: usize, frames: &[Frame]) -> bool {
    let mut enclosing = frames.len();
    let mut depth = 0usize;
    let mut following = tokens[index + 1..].iter().peekable();
    while let Some(spanned) = following.next() {
        match &spanned.token {
            Token::Punct('(' | '[') => depth += 1,
            Token::Punct('{') if depth == 0 => return false,
            Token::Punct('{') => depth += 1,
            Token::Punct(')' | ']' | '}') if depth > 0 => depth -= 1,
            Token::Punct(')' | ']' | '}') if enclosing > 0 => enclosing -= 1,
            Token::Punct(')' | ']' | '}') => return false,
            Token::Punct(';') if depth == 0 => return false,
            Token::Punct(',') if depth == 0 && !inside_pattern_group(frames, enclosing) => {
                return false;
            }
            Token::Punct('=') if depth == 0 => {
                return matches!(
                    following.peek().map(|next| &next.token),
                    Some(Token::Punct('>'))
                );
            }
            _ => {}
        }
    }
    false
}

fn is_english_only_call(frame: &Frame) -> bool {
    frame
        .callee
        .as_ref()
        .is_some_and(|callee| callee.name == "english_only" && !callee.is_method)
}

fn inside_pattern_group(frames: &[Frame], enclosing: usize) -> bool {
    enclosing > 0 && matches!(frames[enclosing - 1].open, '(' | '[')
}

fn callee_of_literal(frames: &[Frame]) -> Option<Callee> {
    let innermost = frames.last()?;
    if innermost.callee.is_some() || innermost.open != '[' {
        return innermost.callee.clone();
    }
    let outer = frames.get(frames.len().checked_sub(2)?)?;
    if outer.open == '(' {
        outer.callee.clone()
    } else {
        None
    }
}

fn open_frame(tokens: &[Spanned], index: usize, open: char) -> Frame {
    let attribute_from = (open == '[' && follows_hash(tokens, index)).then_some(index + 1);
    let callee = callee_before(tokens, index, open);
    Frame {
        open,
        callee,
        attribute_from,
    }
}

fn follows_hash(tokens: &[Spanned], index: usize) -> bool {
    let before = |back: usize| index.checked_sub(back).map(|at| &tokens[at].token);
    matches!(
        (before(1), before(2)),
        (Some(Token::Punct('#')), _) | (Some(Token::Punct('!')), Some(Token::Punct('#')))
    )
}

fn callee_before(tokens: &[Spanned], index: usize, open: char) -> Option<Callee> {
    let before = |back: usize| index.checked_sub(back).map(|at| &tokens[at].token);
    let (name_at, is_macro) = match (before(1), before(2)) {
        (Some(Token::Punct('!')), Some(Token::Ident(_))) => (2, true),
        (Some(Token::Ident(_)), _) if open == '(' => (1, false),
        _ => return None,
    };
    let Some(Token::Ident(name)) = before(name_at) else {
        return None;
    };
    let is_method = matches!(before(name_at + 1), Some(Token::Punct('.')));
    let path_prefix = match (
        before(name_at + 1),
        before(name_at + 2),
        before(name_at + 3),
    ) {
        (Some(Token::Punct(':')), Some(Token::Punct(':')), Some(Token::Ident(prefix))) => {
            Some(prefix.clone())
        }
        _ => None,
    };
    Some(Callee {
        name: name.clone(),
        path_prefix,
        is_method,
        is_macro,
    })
}

fn is_cfg_test(attribute: &[Spanned]) -> bool {
    let shape: Vec<&Token> = attribute.iter().map(|spanned| &spanned.token).collect();
    let cfg = Token::Ident("cfg".into());
    let test = Token::Ident("test".into());
    shape == [&cfg, &Token::Punct('('), &test, &Token::Punct(')')]
}

fn note_test_module(item: &[Spanned], scanned: &mut Scanned) {
    let words: Vec<&Token> = item.iter().map(|spanned| &spanned.token).collect();
    let Some(position) = words
        .iter()
        .position(|token| **token == Token::Ident("mod".into()))
    else {
        return;
    };
    if let [Token::Ident(name), Token::Punct(';')] = words[position + 1..] {
        scanned.test_modules.push(name.clone());
    }
}

fn lex(source: &str) -> Vec<Spanned> {
    let letters: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;
    while index < letters.len() {
        let letter = letters[index];
        let next = letters.get(index + 1).copied();
        if letter == '\n' {
            line += 1;
            index += 1;
        } else if letter.is_whitespace() {
            index += 1;
        } else if letter == '/' && next == Some('/') {
            while index < letters.len() && letters[index] != '\n' {
                index += 1;
            }
        } else if letter == '/' && next == Some('*') {
            let (after, lines) = skip_block_comment(&letters, index);
            index = after;
            line += lines;
        } else if let Some(opening) = string_opening(&letters, index) {
            let start_line = line;
            let (text, after, lines) = read_string(&letters, opening);
            tokens.push(Spanned {
                token: Token::Str(text),
                line: start_line,
            });
            index = after;
            line += lines;
        } else if letter == '\'' {
            index = skip_char_or_lifetime(&letters, index);
        } else if letter.is_alphabetic() || letter == '_' {
            let start = index;
            while index < letters.len()
                && (letters[index].is_alphanumeric() || letters[index] == '_')
            {
                index += 1;
            }
            tokens.push(Spanned {
                token: Token::Ident(letters[start..index].iter().collect()),
                line,
            });
        } else if letter.is_ascii_digit() {
            while index < letters.len()
                && (letters[index].is_alphanumeric() || letters[index] == '_')
            {
                index += 1;
            }
        } else {
            tokens.push(Spanned {
                token: Token::Punct(letter),
                line,
            });
            index += 1;
        }
    }
    tokens
}

struct StringOpening {
    body_from: usize,
    hashes: Option<usize>,
}

fn string_opening(letters: &[char], index: usize) -> Option<StringOpening> {
    let letter = letters[index];
    if letter == '"' {
        return Some(StringOpening {
            body_from: index + 1,
            hashes: None,
        });
    }
    let after_prefix = match (letter, letters.get(index + 1)) {
        ('b', Some('"')) => {
            return Some(StringOpening {
                body_from: index + 2,
                hashes: None,
            });
        }
        ('b', Some('r')) => index + 2,
        ('r', _) => index + 1,
        _ => return None,
    };
    let hashes = letters[after_prefix..]
        .iter()
        .take_while(|at| **at == '#')
        .count();
    if letters.get(after_prefix + hashes) != Some(&'"') {
        return None;
    }
    Some(StringOpening {
        body_from: after_prefix + hashes + 1,
        hashes: Some(hashes),
    })
}

fn read_string(letters: &[char], opening: StringOpening) -> (String, usize, usize) {
    let mut text = String::new();
    let mut lines = 0;
    let mut index = opening.body_from;
    while index < letters.len() {
        let letter = letters[index];
        if letter == '\n' {
            lines += 1;
        }
        if let Some(hashes) = opening.hashes {
            let closes = letter == '"'
                && letters[index + 1..]
                    .iter()
                    .take_while(|at| **at == '#')
                    .count()
                    >= hashes;
            if closes {
                return (text, index + 1 + hashes, lines);
            }
            text.push(letter);
            index += 1;
            continue;
        }
        match letter {
            '"' => return (text, index + 1, lines),
            '\\' => {
                let (escaped, after, skipped_lines) = read_escape(letters, index + 1);
                text.push_str(&escaped);
                lines += skipped_lines;
                index = after;
            }
            other => {
                text.push(other);
                index += 1;
            }
        }
    }
    (text, index, lines)
}

fn read_escape(letters: &[char], index: usize) -> (String, usize, usize) {
    let Some(&letter) = letters.get(index) else {
        return (String::new(), index, 0);
    };
    match letter {
        'n' => ("\n".into(), index + 1, 0),
        't' => ("\t".into(), index + 1, 0),
        'r' => ("\r".into(), index + 1, 0),
        '0' => ("\0".into(), index + 1, 0),
        'u' => read_unicode_escape(letters, index + 1),
        'x' => {
            let digits: String = letters[index + 1..].iter().take(2).collect();
            let decoded = u8::from_str_radix(&digits, 16)
                .map(char::from)
                .unwrap_or('?');
            (decoded.to_string(), index + 3, 0)
        }
        '\n' => {
            let mut after = index;
            while after < letters.len() && letters[after].is_whitespace() {
                after += 1;
            }
            let skipped_lines = letters[index..after]
                .iter()
                .filter(|at| **at == '\n')
                .count();
            (String::new(), after, skipped_lines)
        }
        other => (other.to_string(), index + 1, 0),
    }
}

fn read_unicode_escape(letters: &[char], index: usize) -> (String, usize, usize) {
    if letters.get(index) != Some(&'{') {
        return ("?".into(), index, 0);
    }
    let close = letters[index..]
        .iter()
        .position(|at| *at == '}')
        .map_or(letters.len(), |offset| index + offset);
    let digits: String = letters[index + 1..close].iter().collect();
    let decoded = u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or('?');
    (decoded.to_string(), close + 1, 0)
}

fn skip_block_comment(letters: &[char], start: usize) -> (usize, usize) {
    let mut depth = 0;
    let mut lines = 0;
    let mut index = start;
    while index < letters.len() {
        let pair = (letters[index], letters.get(index + 1).copied());
        if pair == ('/', Some('*')) {
            depth += 1;
            index += 2;
        } else if pair == ('*', Some('/')) {
            depth -= 1;
            index += 2;
            if depth == 0 {
                break;
            }
        } else {
            if letters[index] == '\n' {
                lines += 1;
            }
            index += 1;
        }
    }
    (index, lines)
}

fn skip_char_or_lifetime(letters: &[char], start: usize) -> usize {
    let first = letters.get(start + 1).copied();
    if first == Some('\\') {
        let mut index = start + 3;
        while index < letters.len() && letters[index] != '\'' {
            index += 1;
        }
        return index + 1;
    }
    if letters.get(start + 2) == Some(&'\'') {
        return start + 3;
    }
    start + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        scan(source)
            .literals
            .into_iter()
            .map(|literal| literal.text)
            .collect()
    }

    fn kinds(source: &str) -> Vec<(String, Kind)> {
        scan(source)
            .literals
            .iter()
            .map(|literal| (literal.text.clone(), classify(literal)))
            .collect()
    }

    #[test]
    fn a_string_is_found_with_its_line() {
        let scanned = scan("fn f() {\n    let a = \"one\";\n    let b = \"two\";\n}\n");

        let found: Vec<(usize, &str)> = scanned
            .literals
            .iter()
            .map(|literal| (literal.line, literal.text.as_str()))
            .collect();
        assert_eq!(found, [(2, "one"), (3, "two")]);
    }

    #[test]
    fn strings_in_comments_are_not_strings() {
        let source = "// a \"line comment\"\n/* a \"block /* nested */ comment\" */\nlet a = \"real\";\n/// \"doc\"\n";

        assert_eq!(texts(source), ["real"]);
    }

    #[test]
    fn escapes_are_read_to_what_they_mean() {
        assert_eq!(
            texts(r#"let a = "tab\there \"quoted\" \u{e9} \\ end";"#),
            ["tab\there \"quoted\" é \\ end"]
        );
        assert_eq!(
            texts("let a = \"line one \\\n        continued\";"),
            ["line one continued"]
        );
    }

    #[test]
    fn raw_and_byte_strings_are_read() {
        let source =
            "let a = r\"raw \\n\";\nlet b = r#\"with \"quotes\" inside\"#;\nlet c = b\"bytes\";\n";

        assert_eq!(
            texts(source),
            ["raw \\n", "with \"quotes\" inside", "bytes"]
        );
    }

    #[test]
    fn a_character_or_a_lifetime_is_not_a_string() {
        let source = "fn f<'a>(x: &'a str) -> char { let c = '\"'; let d = '\\''; let e = '…'; \"after\".chars().next().unwrap() }";

        assert_eq!(texts(source), ["after"]);
    }

    #[test]
    fn a_line_after_a_multiline_string_is_counted_right() {
        let scanned = scan("let a = \"one\ntwo\";\nlet b = \"three\";\n");

        let lines: Vec<usize> = scanned
            .literals
            .iter()
            .map(|literal| literal.line)
            .collect();
        assert_eq!(lines, [1, 3]);
    }

    #[test]
    fn the_call_a_string_is_passed_to_is_known() {
        let scanned = scan(
            "fn f() { t(\"a.b\"); x.expect(\"msg\"); format!(\"fmt\"); Command::new(\"ls\"); p.args([\"-l\", \"-a\"]); }",
        );

        let seen: Vec<(String, String, bool, bool, bool)> = scanned
            .literals
            .iter()
            .map(|literal| {
                let callee = literal.callee.clone().expect("each is in a call");
                (
                    literal.text.clone(),
                    callee.name,
                    callee.is_method,
                    callee.is_macro,
                    literal.is_first_argument,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("a.b".into(), "t".into(), false, false, true),
                ("msg".into(), "expect".into(), true, false, true),
                ("fmt".into(), "format".into(), false, true, true),
                ("ls".into(), "new".into(), false, false, true),
                ("-l".into(), "args".into(), true, false, true),
                ("-a".into(), "args".into(), true, false, false),
            ]
        );
    }

    #[test]
    fn a_string_in_a_tuple_or_after_the_first_argument_is_not_a_first_argument() {
        let scanned = scan("tf(\"key\", &[(\"name\", value)]);");

        let first: Vec<(&str, bool)> = scanned
            .literals
            .iter()
            .map(|literal| (literal.text.as_str(), literal.is_first_argument))
            .collect();
        assert_eq!(first, [("key", true), ("name", true)]);
        assert_eq!(scanned.literals[1].callee, None);
    }

    #[test]
    fn a_catalog_key_is_told_from_wording() {
        let found = kinds(
            "fn f() { t(\"label.cancel\"); let a = \"Cancel\"; let b = \"copied {count} files\"; let c = x.count(\"Hello there\"); }",
        );

        assert_eq!(
            found,
            [
                ("label.cancel".into(), Kind::CatalogKey),
                ("Cancel".into(), Kind::Wording),
                ("copied {count} files".into(), Kind::Wording),
                ("Hello there".into(), Kind::Wording),
            ]
        );
    }

    #[test]
    fn wording_needs_a_letter_and_a_space_or_an_initial_capital() {
        let found = kinds(
            "let a = [\"\", \" \", \", \", \"…\", \"42\", \"jpg\", \"settings.json\", \"compact\", \"Home\", \"two words\", \" of \", \"DCIM\"];",
        );

        let wording: Vec<&str> = found
            .iter()
            .filter(|(_, kind)| *kind == Kind::Wording)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(wording, ["Home", "two words", " of ", "DCIM"]);
    }

    #[test]
    fn a_constant_name_a_path_and_an_environment_variable_are_not_wording() {
        let found = kinds(
            "fn f() { let a = [\"OPEN_MTP_CLIENT_VIRTUAL_DIR\", \"Contents/MacOS\", \"Open It\", \"ALL CAPS\"]; std::env::var_os(\"HOME\"); std::env::var(\"Home\"); }",
        );

        let wording: Vec<&str> = found
            .iter()
            .filter(|(_, kind)| *kind == Kind::Wording)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(wording, ["Open It", "ALL CAPS"]);
    }

    #[test]
    fn a_format_spec_with_no_words_of_its_own_is_not_wording() {
        let found = kinds(
            "let a = [\"{value} {unit}\", \"{major}.{minor}\", \"{:.1}\", \"{count} files\", \"{{literal}}\"];",
        );

        let wording: Vec<&str> = found
            .iter()
            .filter(|(_, kind)| *kind == Kind::Wording)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(wording, ["{count} files"]);
    }

    #[test]
    fn a_lowercase_word_given_straight_to_a_widget_is_wording() {
        let found = kinds(
            "fn f() { text(\"open\"); button.label(\"read-only\"); text_input(\"name\", v); other(\"open\"); text(\"×\"); text(\"\"); text(format!(\"{}\", n)); text(\"{count}\"); }",
        );

        let wording: Vec<&str> = found
            .iter()
            .filter(|(_, kind)| *kind == Kind::Wording)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(wording, ["open", "read-only", "name"]);
    }

    #[test]
    fn units_and_product_names_are_not_wording() {
        let found = kinds("let a = [\"KB\", \"GB\", \"USB\", \"open-mtp-client\"];");

        assert!(found.iter().all(|(_, kind)| *kind == Kind::NotWording));
    }

    #[test]
    fn what_a_developer_reads_is_not_wording() {
        let source = "fn f() { panic!(\"Bad state here\"); x.expect(\"It must be there\"); assert_eq!(a, b, \"They differ\"); eprintln!(\"Could not save\"); include_str!(\"Some Path\"); }";

        assert!(
            kinds(source)
                .iter()
                .all(|(_, kind)| *kind == Kind::NotWording)
        );
    }

    #[test]
    fn what_is_handed_to_a_process_or_a_path_is_not_wording() {
        let source = "fn f() { Command::new(\"Some Tool\"); c.arg(\"Two Words\"); c.args([\"Open Now\", \"Then Go\"]); p.join(\"Application Support\"); Path::new(\"Library Stuff\"); PathBuf::from(\"My Docs\"); }";

        assert!(
            kinds(source)
                .iter()
                .all(|(_, kind)| *kind == Kind::NotWording)
        );
    }

    #[test]
    fn a_name_matched_against_in_a_pattern_is_not_wording() {
        let source = "fn f(p: &str) { match p { \"Home Folder\" => 1, \"Other Place\" | \"Third Place\" => 2, _ => 3 }; match parts { [\"Users\", _, \"Desktop\" | \"Documents\", ..] | [\"Volumes\", ..] => 4, _ => 5 }; }";

        let found = kinds(source);

        assert!(
            found.iter().all(|(_, kind)| *kind == Kind::NotWording),
            "{found:?}"
        );
    }

    #[test]
    fn a_string_that_is_the_result_of_an_arm_or_a_comparison_is_still_wording() {
        let source = "fn f(p: u8) -> &'static str { let named = if p == 1 { \"One Thing\" } else { \"Other Thing\" }; match p { 1 => \"First Place\", _ => \"Second Place\" } }";

        let found = kinds(source);

        assert!(
            found.iter().all(|(_, kind)| *kind == Kind::Wording),
            "{found:?}"
        );
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn text_that_is_english_by_design_is_not_wording_however_deep_it_sits() {
        let source = "fn f() -> String { english_only(format!(\"Copy Object {}, Move Object {}\", a(\"Yes Please\"), b)) }";

        let found = kinds(source);

        assert_eq!(found.len(), 2);
        assert!(
            found.iter().all(|(_, kind)| *kind == Kind::NotWording),
            "{found:?}"
        );
    }

    #[test]
    fn a_string_in_an_attribute_is_not_wording() {
        let source = "#[serde(rename = \"Some Name\")]\nstruct S;\n#![allow(unused, reason = \"Because Tests\")]\n";

        assert!(
            kinds(source)
                .iter()
                .all(|(_, kind)| *kind == Kind::NotWording)
        );
    }

    #[test]
    fn a_test_module_is_left_out() {
        let source = "fn real() { let a = \"Visible one\"; }\n#[cfg(test)]\nmod tests {\n    fn t() { let b = \"Hidden one\"; if true { let c = \"Hidden two\"; } }\n}\nfn after() { let d = \"Visible two\"; }\n";

        assert_eq!(texts(source), ["Visible one", "Visible two"]);
    }

    #[test]
    fn several_test_modules_and_other_test_items_are_left_out() {
        let source = "#[cfg(test)]\nmod first_tests { fn a() { let x = \"Hidden a\"; } }\n#[cfg(test)]\nuse std::{io, fmt};\n#[cfg(test)]\nconst NAMES: [&str; 2] = [\"Hidden b\", \"Hidden c\"];\n#[cfg(test)]\npub fn helper() -> String { \"Hidden d\".into() }\n#[cfg(test)]\nimpl Thing { fn f(&self) { let y = \"Hidden e\"; } }\nfn shown() { let z = \"Shown\"; }\n";

        assert_eq!(texts(source), ["Shown"]);
    }

    #[test]
    fn a_test_only_statement_ends_with_its_block() {
        let source = "fn note() {\n    #[cfg(not(test))]\n    if let Ok(x) = LOCK.lock() { x.go(\"Kept one\"); }\n    #[cfg(test)]\n    LOCAL.with(|x| x.go(\"Hidden one\"))\n}\nfn after() { let a = \"Kept two\"; }\n";

        assert_eq!(texts(source), ["Kept one", "Kept two"]);
    }

    #[test]
    fn a_test_only_file_module_is_named() {
        let scanned = scan(
            "mod real;\n#[cfg(test)]\nmod checks;\n#[cfg(test)]\npub mod testing;\nfn f() { let a = \"After\"; }\n",
        );

        assert_eq!(scanned.test_modules, ["checks", "testing"]);
        assert_eq!(scanned.literals.len(), 1);
    }

    #[test]
    fn not_test_is_not_left_out() {
        let source = "#[cfg(not(test))]\nstatic RECENT: Mutex<u8> = Mutex::new(\"Shown one\");\n";

        assert_eq!(texts(source), ["Shown one"]);
    }
}
