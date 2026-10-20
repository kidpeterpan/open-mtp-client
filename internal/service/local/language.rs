pub trait Languages: Send + Sync {
    fn preferred(&self) -> Vec<String>;
}

#[derive(Default)]
pub struct SystemLanguages;

impl Languages for SystemLanguages {
    fn preferred(&self) -> Vec<String> {
        sys_locale::get_locales().collect()
    }
}

pub struct RecordingLanguages {
    given: Vec<String>,
}

impl RecordingLanguages {
    pub fn new(given: &[&str]) -> Self {
        Self {
            given: given.iter().map(|entry| entry.to_string()).collect(),
        }
    }
}

impl Languages for RecordingLanguages {
    fn preferred(&self) -> Vec<String> {
        self.given.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorder_gives_back_the_list_it_was_given_in_order() {
        let languages = RecordingLanguages::new(&["en-TH", "th-TH", "zh-Hans-CN"]);

        assert_eq!(languages.preferred(), ["en-TH", "th-TH", "zh-Hans-CN"]);
    }

    #[test]
    fn the_recorder_can_be_given_no_languages() {
        assert!(RecordingLanguages::new(&[]).preferred().is_empty());
    }

    #[test]
    #[ignore = "reads this Mac's real language preferences and prints them"]
    fn print_the_real_preferred_languages() {
        let started = std::time::Instant::now();
        let languages = SystemLanguages.preferred();
        println!("{languages:?} in {:?}", started.elapsed());

        let started = std::time::Instant::now();
        for _ in 0..1_000 {
            let _ = SystemLanguages.preferred();
        }
        println!("1,000 more reads took {:?}", started.elapsed());
    }
}
