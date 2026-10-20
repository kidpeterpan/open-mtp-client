mod catalog;
#[cfg(test)]
mod checks;
#[cfg(test)]
mod scan;
#[cfg(test)]
pub mod screens;

#[cfg(test)]
use std::cell::Cell;
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};

use internal::dto::Language;

use catalog::{Catalog, ParseError, Part, Template};

const SOURCES: [&str; 4] = [
    include_str!("locales/en.txt"),
    include_str!("locales/th.txt"),
    include_str!("locales/ja.txt"),
    include_str!("locales/zh-Hans.txt"),
];

#[cfg(not(test))]
static ACTIVE: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static ACTIVE: Cell<Language> = const { Cell::new(Language::English) };
}

pub struct Catalogs {
    by_language: Vec<Catalog>,
}

#[derive(Debug)]
pub struct LoadError {
    pub language: Language,
    pub error: ParseError,
}

impl Catalogs {
    pub fn parse(sources: [&'static str; 4]) -> Result<Catalogs, LoadError> {
        let mut by_language = Vec::with_capacity(sources.len());
        for (language, source) in Language::ALL.into_iter().zip(sources) {
            let catalog = Catalog::parse(source).map_err(|error| LoadError { language, error })?;
            by_language.push(catalog);
        }
        Ok(Catalogs { by_language })
    }

    pub fn text(&self, language: Language, key: &str) -> Option<&'static str> {
        self.template(language, key)?.plain()
    }

    #[cfg(test)]
    pub fn has_own(&self, language: Language, key: &str) -> bool {
        self.own(language).get(key).is_some()
    }

    pub fn fill(&self, language: Language, key: &str, args: &[(&str, &str)]) -> Option<String> {
        let template = self.template(language, key)?;
        Some(fill_slots(key, template, args))
    }

    pub fn count(
        &self,
        language: Language,
        key: &str,
        number: usize,
        args: &[(&str, &str)],
    ) -> Option<String> {
        let for_one = (number == 1)
            .then(|| self.own(language).get(&[key, ".one"].concat()))
            .flatten();
        let template = match for_one {
            Some(template) => template,
            None => self.template(language, key)?,
        };
        let grouped_number = grouped(number);
        let mut with_count = args.to_vec();
        if has_slot(template, "count") {
            with_count.push(("count", &grouped_number));
        }
        Some(fill_slots(key, template, &with_count))
    }

    fn own(&self, language: Language) -> &Catalog {
        &self.by_language[usize::from(index_of(language))]
    }

    fn template(&self, language: Language, key: &str) -> Option<&Template> {
        self.own(language)
            .get(key)
            .or_else(|| self.own(Language::English).get(key))
    }
}

fn has_slot(template: &Template, name: &str) -> bool {
    template
        .parts()
        .iter()
        .any(|part| matches!(part, Part::Slot(slot) if *slot == name))
}

fn fill_slots(key: &str, template: &Template, args: &[(&str, &str)]) -> String {
    for (name, _) in args {
        debug_assert!(has_slot(template, name), "{key} has no slot {{{name}}}");
    }
    let mut filled = String::new();
    for part in template.parts() {
        match part {
            Part::Text(text) => filled.push_str(text),
            Part::Slot(name) => match args.iter().find(|(given, _)| given == name) {
                Some((_, value)) => filled.push_str(value),
                None => {
                    debug_assert!(false, "{key} has no value for {{{name}}}");
                    filled.push_str(&format!("{{{name}}}"));
                }
            },
        }
    }
    filled
}

pub(crate) fn embedded() -> &'static Catalogs {
    static EMBEDDED: OnceLock<Catalogs> = OnceLock::new();
    EMBEDDED.get_or_init(|| match Catalogs::parse(SOURCES) {
        Ok(catalogs) => catalogs,
        Err(LoadError { language, error }) => {
            panic!("the embedded {language:?} language file does not parse: {error:?}")
        }
    })
}

fn absent(key: &'static str) -> &'static str {
    debug_assert!(
        false,
        "no wording for {key} (it is missing, or it has slots: use tf)"
    );
    key
}

pub fn t(key: &'static str) -> &'static str {
    embedded()
        .text(language(), key)
        .unwrap_or_else(|| absent(key))
}

pub const fn key(key: &'static str) -> &'static str {
    key
}

pub fn english_only<Text>(text: Text) -> Text {
    text
}

pub fn english(key: &'static str) -> &'static str {
    embedded()
        .text(Language::English, key)
        .unwrap_or_else(|| absent(key))
}

pub fn tf(key: &'static str, args: &[(&str, &str)]) -> String {
    tf_in(language(), key, args)
}

pub fn tf_in(language: Language, key: &'static str, args: &[(&str, &str)]) -> String {
    embedded()
        .fill(language, key, args)
        .unwrap_or_else(|| absent(key).to_string())
}

pub fn count(key: &'static str, number: usize, args: &[(&str, &str)]) -> String {
    count_in(language(), key, number, args)
}

pub fn count_in(
    language: Language,
    key: &'static str,
    number: usize,
    args: &[(&str, &str)],
) -> String {
    embedded()
        .count(language, key, number, args)
        .unwrap_or_else(|| absent(key).to_string())
}

pub fn grouped(number: usize) -> String {
    let digits = number.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn index_of(language: Language) -> u8 {
    match language {
        Language::English => 0,
        Language::Thai => 1,
        Language::Japanese => 2,
        Language::ChineseSimplified => 3,
    }
}

#[cfg(not(test))]
fn language_at(index: u8) -> Language {
    match index {
        1 => Language::Thai,
        2 => Language::Japanese,
        3 => Language::ChineseSimplified,
        _ => Language::English,
    }
}

pub fn language() -> Language {
    #[cfg(not(test))]
    return language_at(ACTIVE.load(Ordering::Relaxed));
    #[cfg(test)]
    ACTIVE.with(Cell::get)
}

pub fn set_language(language: Language) {
    #[cfg(not(test))]
    ACTIVE.store(index_of(language), Ordering::Relaxed);
    #[cfg(test)]
    ACTIVE.with(|active| active.set(language));
}

#[cfg(test)]
pub fn with_language<Output>(chosen: Language, work: impl FnOnce() -> Output) -> Output {
    struct Restore(Language);

    impl Drop for Restore {
        fn drop(&mut self) {
            set_language(self.0);
        }
    }

    let _restore = Restore(language());
    set_language(chosen);
    work()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGLISH: &str = "greeting = Hello\n\
        only_in_english = Only English\n\
        hello.name = Hello {name}\n\
        pair = {first} and {second}, {first} again\n\
        items = {count} items\n\
        items.one = 1 item\n\
        braces = write {{name}} to name it\n";
    const THAI: &str = "greeting = สวัสดี\n\
        hello.name = สวัสดี {name}\n\
        pair = {first} กับ {second}\n\
        items = {count} รายการ\n\
        braces = เขียน {{name}} เพื่อตั้งชื่อ\n";
    const JAPANESE: &str = "greeting = こんにちは\n";
    const CHINESE: &str = "greeting = 你好\n";

    fn fixture() -> Catalogs {
        Catalogs::parse([ENGLISH, THAI, JAPANESE, CHINESE]).expect("the fixture parses")
    }

    #[test]
    fn a_message_is_the_active_languages_text() {
        let catalogs = fixture();

        assert_eq!(catalogs.text(Language::English, "greeting"), Some("Hello"));
        assert_eq!(catalogs.text(Language::Thai, "greeting"), Some("สวัสดี"));
        assert_eq!(
            catalogs.text(Language::Japanese, "greeting"),
            Some("こんにちは")
        );
        assert_eq!(
            catalogs.text(Language::ChineseSimplified, "greeting"),
            Some("你好")
        );
    }

    #[test]
    fn a_message_missing_in_the_active_language_shows_the_english_text() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.text(Language::Thai, "only_in_english"),
            Some("Only English")
        );
        assert_eq!(
            catalogs.fill(Language::Japanese, "hello.name", &[("name", "Mai")]),
            Some("Hello Mai".to_string())
        );
    }

    #[test]
    fn a_message_is_in_a_language_only_when_that_languages_own_file_has_it() {
        let catalogs = fixture();

        assert!(catalogs.has_own(Language::English, "only_in_english"));
        assert!(!catalogs.has_own(Language::Thai, "only_in_english"));
        assert!(catalogs.has_own(Language::Thai, "greeting"));
        assert!(!catalogs.has_own(Language::Thai, "no.such.key"));
    }

    #[test]
    fn a_message_missing_everywhere_is_none() {
        let catalogs = fixture();

        assert_eq!(catalogs.text(Language::Thai, "no.such.key"), None);
        assert_eq!(catalogs.fill(Language::Thai, "no.such.key", &[]), None);
        assert_eq!(catalogs.count(Language::Thai, "no.such.key", 1, &[]), None);
    }

    #[test]
    fn slots_are_filled_in_any_order_and_as_often_as_they_appear() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.fill(
                Language::English,
                "pair",
                &[("second", "B"), ("first", "A")]
            ),
            Some("A and B, A again".to_string())
        );
        assert_eq!(
            catalogs.fill(Language::Thai, "pair", &[("first", "A"), ("second", "B")]),
            Some("A กับ B".to_string())
        );
    }

    #[test]
    fn a_value_with_braces_is_inserted_exactly_as_it_is() {
        let catalogs = fixture();

        for odd in ["{x}", "{{y}}", "{name}", "{", "}}", "{first} {second}"] {
            assert_eq!(
                catalogs.fill(Language::English, "hello.name", &[("name", odd)]),
                Some(format!("Hello {odd}")),
                "{odd}"
            );
        }
    }

    #[test]
    fn doubled_braces_in_a_message_come_out_as_single_braces() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.text(Language::English, "braces"),
            Some("write {name} to name it")
        );
        assert_eq!(
            catalogs.text(Language::Thai, "braces"),
            Some("เขียน {name} เพื่อตั้งชื่อ")
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "hello.name has no value for {name}")]
    fn a_slot_without_a_value_is_a_failure_not_a_blank() {
        fixture().fill(Language::English, "hello.name", &[]);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "hello.name has no slot {surname}")]
    fn a_value_without_a_slot_is_a_failure_not_ignored() {
        fixture().fill(
            Language::English,
            "hello.name",
            &[("name", "Mai"), ("surname", "Lee")],
        );
    }

    #[test]
    fn one_uses_the_count_variant_where_the_language_has_it() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.count(Language::English, "items", 1, &[]),
            Some("1 item".to_string())
        );
        assert_eq!(
            catalogs.count(Language::English, "items", 2, &[]),
            Some("2 items".to_string())
        );
        assert_eq!(
            catalogs.count(Language::English, "items", 0, &[]),
            Some("0 items".to_string())
        );
    }

    #[test]
    fn a_language_without_a_count_variant_uses_the_plain_message_for_one() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.count(Language::Thai, "items", 1, &[]),
            Some("1 รายการ".to_string())
        );
        assert_eq!(
            catalogs.count(Language::Thai, "items", 2, &[]),
            Some("2 รายการ".to_string())
        );
    }

    #[test]
    fn the_count_is_available_to_the_message_with_digit_grouping() {
        let catalogs = fixture();

        assert_eq!(
            catalogs.count(Language::English, "items", 1_234_567, &[]),
            Some("1,234,567 items".to_string())
        );
        assert_eq!(
            catalogs.count(Language::Thai, "items", 5_000, &[]),
            Some("5,000 รายการ".to_string())
        );
    }

    #[test]
    fn a_counted_message_takes_other_values_too() {
        let catalogs = Catalogs::parse([
            "moved = Moved {count} files to {folder}\nmoved.one = Moved 1 file to {folder}\n",
            "",
            "",
            "",
        ])
        .unwrap();

        assert_eq!(
            catalogs.count(Language::English, "moved", 1, &[("folder", "Trip")]),
            Some("Moved 1 file to Trip".to_string())
        );
        assert_eq!(
            catalogs.count(Language::English, "moved", 3, &[("folder", "{x}")]),
            Some("Moved 3 files to {x}".to_string())
        );
    }

    #[test]
    fn digits_are_grouped_in_threes() {
        for (number, text) in [
            (0, "0"),
            (7, "7"),
            (999, "999"),
            (1_000, "1,000"),
            (12_345, "12,345"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(grouped(number), text);
        }
    }

    #[test]
    fn the_language_is_english_until_one_is_set() {
        assert_eq!(language(), Language::English);
    }

    #[test]
    fn with_language_sets_the_language_for_the_closure_and_gives_it_back_after() {
        let inside = with_language(Language::Thai, language);

        assert_eq!(inside, Language::Thai);
        assert_eq!(language(), Language::English);
    }

    #[test]
    fn with_language_gives_the_language_back_even_when_the_closure_panics() {
        let outcome = std::panic::catch_unwind(|| {
            with_language(Language::Japanese, || panic!("a test's own panic"))
        });

        assert!(outcome.is_err());
        assert_eq!(language(), Language::English);
    }

    #[test]
    fn with_language_nests_and_each_level_restores_the_one_before() {
        with_language(Language::Thai, || {
            with_language(Language::ChineseSimplified, || {
                assert_eq!(language(), Language::ChineseSimplified);
            });
            assert_eq!(language(), Language::Thai);
        });
        assert_eq!(language(), Language::English);
    }

    #[test]
    fn set_language_inside_with_language_is_undone_with_it() {
        with_language(Language::English, || {
            set_language(Language::Japanese);
            assert_eq!(language(), Language::Japanese);
        });

        assert_eq!(language(), Language::English);
    }

    #[test]
    fn a_language_chosen_in_one_test_is_not_seen_by_another_thread() {
        with_language(Language::Thai, || {
            let seen = std::thread::spawn(language).join().unwrap();

            assert_eq!(seen, Language::English);
            assert_eq!(language(), Language::Thai);
        });
    }

    fn written_in(index: usize, key: &str) -> &'static str {
        Catalog::parse(SOURCES[index])
            .expect("the language file parses")
            .get(key)
            .and_then(Template::plain)
            .expect("the language file has the message")
    }

    #[test]
    fn the_embedded_files_answer_for_the_active_language() {
        for (index, language) in Language::ALL.into_iter().enumerate() {
            let said = with_language(language, || t("label.cancel"));

            assert_eq!(said, written_in(index, "label.cancel"), "{language:?}");
        }
    }

    #[test]
    fn the_free_functions_fill_and_count_in_the_active_language() {
        let thai = written_in(1, "label.cancel");

        with_language(Language::Thai, || {
            assert_eq!(tf("label.cancel", &[]), thai);
            assert_eq!(count("label.cancel", 3, &[]), thai);
        });
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "no wording for no.such.key")]
    fn a_key_the_english_file_lacks_fails_loudly_in_a_debug_build() {
        t("no.such.key");
    }

    #[test]
    fn a_catalog_that_does_not_parse_names_the_language_and_the_line() {
        let outcome = Catalogs::parse([ENGLISH, "greeting = ok\nbroken line\n", JAPANESE, CHINESE]);

        let error = outcome.err().expect("a broken Thai file is refused");
        assert_eq!(error.language, Language::Thai);
        assert_eq!(error.error.line, 2);
    }
}
