#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    Thai,
    Japanese,
    ChineseSimplified,
}

impl Language {
    pub const ALL: [Language; 4] = [
        Language::English,
        Language::Thai,
        Language::Japanese,
        Language::ChineseSimplified,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Thai => "th",
            Language::Japanese => "ja",
            Language::ChineseSimplified => "zh-Hans",
        }
    }

    pub fn from_code(code: &str) -> Option<Language> {
        Language::ALL
            .into_iter()
            .find(|language| language.code() == code)
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Thai => "ไทย",
            Language::Japanese => "日本語",
            Language::ChineseSimplified => "简体中文",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguageChoice {
    #[default]
    FollowMac,
    Chosen(Language),
}

impl LanguageChoice {
    pub fn resolve(self, preferred: &[String]) -> Language {
        match self {
            LanguageChoice::Chosen(language) => language,
            LanguageChoice::FollowMac => preferred
                .iter()
                .find_map(|tag| offered_language(tag))
                .unwrap_or(Language::English),
        }
    }
}

fn offered_language(tag: &str) -> Option<Language> {
    let mut subtags = tag.split(['-', '_']);
    let primary = subtags.next()?.to_ascii_lowercase();
    match primary.as_str() {
        "en" => Some(Language::English),
        "th" => Some(Language::Thai),
        "ja" => Some(Language::Japanese),
        "zh" => simplified_chinese(subtags.next()),
        _ => None,
    }
}

fn simplified_chinese(script_or_region: Option<&str>) -> Option<Language> {
    let subtag = script_or_region?;
    let is_script = subtag.len() == 4;
    let offered = if is_script {
        subtag.eq_ignore_ascii_case("Hans")
    } else {
        subtag.eq_ignore_ascii_case("CN")
    };
    offered.then_some(Language::ChineseSimplified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(list: &[&str]) -> Language {
        let preferred: Vec<String> = list.iter().map(|entry| entry.to_string()).collect();
        LanguageChoice::FollowMac.resolve(&preferred)
    }

    #[test]
    fn the_four_languages_are_listed_in_the_order_settings_shows_them() {
        assert_eq!(
            Language::ALL,
            [
                Language::English,
                Language::Thai,
                Language::Japanese,
                Language::ChineseSimplified
            ]
        );
    }

    #[test]
    fn each_language_has_the_code_the_settings_file_uses() {
        let codes: Vec<&str> = Language::ALL
            .iter()
            .map(|language| language.code())
            .collect();
        assert_eq!(codes, ["en", "th", "ja", "zh-Hans"]);
    }

    #[test]
    fn a_code_reads_back_as_the_language_that_wrote_it() {
        for language in Language::ALL {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
    }

    #[test]
    fn an_unknown_code_is_no_language() {
        for code in ["", "klingon", "EN", "zh", "zh-Hant", "system", " en"] {
            assert_eq!(Language::from_code(code), None, "{code:?}");
        }
    }

    #[test]
    fn each_language_is_named_in_its_own_writing() {
        let names: Vec<&str> = Language::ALL
            .iter()
            .map(|language| language.native_name())
            .collect();
        assert_eq!(names, ["English", "ไทย", "日本語", "简体中文"]);
    }

    #[test]
    fn the_default_choice_follows_the_mac() {
        assert_eq!(LanguageChoice::default(), LanguageChoice::FollowMac);
    }

    #[test]
    fn a_list_led_by_an_offered_language_gives_that_language() {
        assert_eq!(resolved(&["en-TH", "th-TH"]), Language::English);
        assert_eq!(resolved(&["th-TH"]), Language::Thai);
        assert_eq!(resolved(&["ja-JP"]), Language::Japanese);
    }

    #[test]
    fn the_first_offered_entry_wins_over_one_that_is_not_offered() {
        assert_eq!(resolved(&["fr-FR", "th-TH"]), Language::Thai);
        assert_eq!(
            resolved(&["de-DE", "fr-FR", "ja-JP", "th-TH"]),
            Language::Japanese
        );
    }

    #[test]
    fn simplified_chinese_is_offered_by_script_or_by_mainland_region() {
        assert_eq!(resolved(&["zh-Hans-CN"]), Language::ChineseSimplified);
        assert_eq!(resolved(&["zh-Hans"]), Language::ChineseSimplified);
        assert_eq!(resolved(&["zh-CN"]), Language::ChineseSimplified);
    }

    #[test]
    fn traditional_and_unmarked_chinese_are_skipped_not_guessed() {
        for list in [["zh-Hant-TW"], ["zh-Hant"], ["zh-TW"], ["zh-HK"], ["zh"]] {
            assert_eq!(resolved(&list), Language::English, "{list:?}");
        }
        assert_eq!(resolved(&["zh-TW", "th-TH"]), Language::Thai);
    }

    #[test]
    fn a_list_with_nothing_offered_gives_english() {
        assert_eq!(resolved(&["fr-FR"]), Language::English);
        assert_eq!(resolved(&[]), Language::English);
    }

    #[test]
    fn a_chosen_language_wins_over_any_list() {
        let preferred = vec!["th-TH".to_string(), "ja-JP".to_string()];
        for language in Language::ALL {
            assert_eq!(
                LanguageChoice::Chosen(language).resolve(&preferred),
                language
            );
            assert_eq!(LanguageChoice::Chosen(language).resolve(&[]), language);
        }
    }
}
