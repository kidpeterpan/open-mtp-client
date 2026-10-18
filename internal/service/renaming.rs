//! The new names of several items renamed at once. Pure: the pane plans as
//! the user types, and only what the plan allows is sent to the phone or
//! the Mac, one rename per item, nothing ever overwritten.

use std::collections::HashMap;

use super::names::{self, Index};
use crate::dto::{Error, RenamePattern, RenameProblem, Renaming};

/// Where a file's name ends and its extension begins (`None`: no extension,
/// as for a folder or `.profile`).
fn split(name: &str, is_folder: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_folder => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// The new name of each of `items` (name, is it a folder), in the order
/// given, in a folder holding `folder`, and what would stop each.
pub fn plan(items: &[(String, bool)], folder: &[String], pattern: &RenamePattern) -> Vec<Renaming> {
    let last = match pattern {
        RenamePattern::Number { start, .. } => {
            u64::from(*start) + items.len().saturating_sub(1) as u64
        }
        _ => 0,
    };
    let width = last.to_string().len().max(3);
    let wanted: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(at, (name, is_folder))| {
            let (stem, extension) = split(name, *is_folder);
            let stem = match pattern {
                RenamePattern::Replace { find, with } if !find.is_empty() => {
                    stem.replace(find.as_str(), with)
                }
                RenamePattern::Replace { .. } => stem.to_owned(),
                RenamePattern::Add {
                    text,
                    at_end: false,
                } => format!("{text}{stem}"),
                RenamePattern::Add { text, at_end: true } => format!("{stem}{text}"),
                RenamePattern::Number { base, start } => {
                    let number = u64::from(*start) + at as u64;
                    format!("{} {number:0width$}", base.trim_end())
                }
            };
            format!("{stem}{extension}")
        })
        .collect();

    let index = Index::of(folder.iter().map(String::as_str));
    let mut counts: HashMap<String, usize> = HashMap::new();
    let checked: Vec<Result<String, RenameProblem>> = items
        .iter()
        .zip(&wanted)
        .map(|((from, _), to)| match names::validate(to) {
            Err(Error::InvalidName(problem)) => Err(RenameProblem::Invalid(problem)),
            Err(_) => Err(RenameProblem::Taken),
            // One that keeps its name is no one else's clash: an item given
            // that name finds it taken in the folder.
            Ok(to) if to == from => Ok(to.to_owned()),
            Ok(to) => {
                *counts.entry(to.to_lowercase()).or_default() += 1;
                // Its own name, in another case, is not taken.
                match index.find(to) {
                    Some(at) if !names::same(&folder[at], from) => Err(RenameProblem::Taken),
                    _ => Ok(to.to_owned()),
                }
            }
        })
        .collect();
    items
        .iter()
        .zip(wanted)
        .zip(checked)
        .map(|(((from, _), wanted), checked)| {
            let (to, problem) = match checked {
                Ok(to) if counts.get(&to.to_lowercase()).is_some_and(|&n| n > 1) => {
                    (to, Some(RenameProblem::Twice))
                }
                Ok(to) => (to, None),
                Err(problem) => (wanted, Some(problem)),
            };
            Renaming {
                from: from.clone(),
                to,
                problem,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::NameProblem;

    fn files(names: &[&str]) -> Vec<(String, bool)> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), false))
            .collect()
    }

    fn folder(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn new_names(plan: &[Renaming]) -> Vec<&str> {
        plan.iter().map(|renaming| renaming.to.as_str()).collect()
    }

    #[test]
    fn replacing_text_changes_the_name_and_keeps_the_extension() {
        let names = ["IMG_0001.jpg", "IMG_0002.JPG", "notes.txt"];
        let plan = super::plan(
            &files(&names),
            &folder(&names),
            &RenamePattern::Replace {
                find: "IMG_".into(),
                with: "Trip ".into(),
            },
        );

        assert_eq!(
            new_names(&plan),
            ["Trip 0001.jpg", "Trip 0002.JPG", "notes.txt"]
        );
        assert!(plan.iter().all(|renaming| renaming.problem.is_none()));
        assert_eq!(
            plan.iter().filter(|r| r.changes()).count(),
            2,
            "notes.txt keeps its name"
        );
        let jpg = super::plan(
            &files(&["a.jpg"]),
            &folder(&["a.jpg"]),
            &RenamePattern::Replace {
                find: "jpg".into(),
                with: "png".into(),
            },
        );
        assert_eq!(jpg[0].to, "a.jpg", "never the extension");
        let nothing = super::plan(
            &files(&["a.jpg"]),
            &folder(&["a.jpg"]),
            &RenamePattern::Replace {
                find: String::new(),
                with: "x".into(),
            },
        );
        assert_eq!(nothing[0].to, "a.jpg");
    }

    #[test]
    fn text_goes_before_or_after_the_name() {
        let items = vec![("a.jpg".to_owned(), false), ("Trip".to_owned(), true)];
        let all = folder(&["a.jpg", "Trip"]);
        let before = super::plan(
            &items,
            &all,
            &RenamePattern::Add {
                text: "2026 ".into(),
                at_end: false,
            },
        );
        let after = super::plan(
            &items,
            &all,
            &RenamePattern::Add {
                text: " (old)".into(),
                at_end: true,
            },
        );

        assert_eq!(new_names(&before), ["2026 a.jpg", "2026 Trip"]);
        assert_eq!(
            new_names(&after),
            ["a (old).jpg", "Trip (old)"],
            "a folder has no extension"
        );
    }

    #[test]
    fn numbering_counts_in_the_order_given_with_room_for_the_last() {
        let names = ["c.jpg", "a.png", ".hidden", "Folder"];
        let mut items = files(&names);
        items[3].1 = true;
        let numbered = super::plan(
            &items,
            &folder(&names),
            &RenamePattern::Number {
                base: "Trip ".into(),
                start: 1,
            },
        );

        assert_eq!(
            new_names(&numbered),
            ["Trip 001.jpg", "Trip 002.png", "Trip 003", "Trip 004"]
        );
        let many: Vec<String> = (0..1_200).map(|n| format!("{n}.jpg")).collect();
        let items: Vec<(String, bool)> = many.iter().map(|n| (n.clone(), false)).collect();
        let wide = super::plan(
            &items,
            &many,
            &RenamePattern::Number {
                base: "x".into(),
                start: 1,
            },
        );
        assert_eq!(wide[0].to, "x 0001.jpg");
        assert_eq!(wide[1_199].to, "x 1200.jpg");
    }

    #[test]
    fn a_name_taken_in_the_folder_or_given_twice_is_shown_before_anything_changes() {
        let all = folder(&["a.jpg", "b.jpg", "Trip a.jpg", "c.jpg"]);
        let plan = super::plan(
            &files(&["a.jpg", "b.jpg", "c.jpg"]),
            &all,
            &RenamePattern::Add {
                text: "Trip ".into(),
                at_end: false,
            },
        );

        assert_eq!(plan[0].problem, Some(RenameProblem::Taken));
        assert_eq!(plan[1].problem, None);
        assert!(!plan[0].changes() && plan[1].changes());

        let twice = super::plan(
            &files(&["a 1.jpg", "a_1.jpg", "b.jpg"]),
            &folder(&["a 1.jpg", "a_1.jpg", "b.jpg"]),
            &RenamePattern::Replace {
                find: "_".into(),
                with: " ".into(),
            },
        );
        assert_eq!(twice[0].problem, None, "a 1.jpg keeps its name");
        assert!(!twice[0].changes());
        assert_eq!(twice[1].problem, Some(RenameProblem::Taken), "by a 1.jpg");
    }

    #[test]
    fn two_items_given_one_name_are_both_held_back() {
        let plan = super::plan(
            &files(&["x-1.jpg", "x_1.jpg", "y.jpg"]),
            &folder(&["x-1.jpg", "x_1.jpg", "y.jpg"]),
            &RenamePattern::Replace {
                find: "1".into(),
                with: "2".into(),
            },
        );
        assert_eq!(plan[0].problem, None);

        let clash = super::plan(
            &files(&["A.jpg", "a.png"]),
            &folder(&["A.jpg", "a.png"]),
            &RenamePattern::Number {
                base: "Trip".into(),
                start: 1,
            },
        );
        assert!(
            clash.iter().all(|r| r.problem.is_none()),
            "different numbers"
        );
        let same = super::plan(
            &files(&["one.jpg", "two.JPG"]),
            &folder(&["one.jpg", "two.JPG"]),
            &RenamePattern::Replace {
                find: "one".into(),
                with: "two".into(),
            },
        );
        assert_eq!(
            same[0].problem,
            Some(RenameProblem::Taken),
            "two.JPG, ignoring case"
        );
        let given_twice = super::plan(
            &files(&["a1.jpg", "a2.jpg"]),
            &folder(&["a1.jpg", "a2.jpg"]),
            &RenamePattern::Replace {
                find: "a".into(),
                with: String::new(),
            },
        );
        assert!(given_twice.iter().all(|r| r.problem.is_none()));
        let collide = super::plan(
            &files(&["x a.jpg", "y a.jpg"]),
            &folder(&["x a.jpg", "y a.jpg"]),
            &RenamePattern::Number {
                base: "".into(),
                start: 1,
            },
        );
        assert_eq!(new_names(&collide), ["001.jpg", "002.jpg"]);
        let both = super::plan(
            &files(&["x.jpg", "y.jpg"]),
            &folder(&["x.jpg", "y.jpg"]),
            &RenamePattern::Add {
                text: String::new(),
                at_end: true,
            },
        );
        assert!(both.iter().all(|r| !r.changes()), "nothing to do");
        let doubled = super::plan(
            &files(&["x.jpg", "x.png"]),
            &folder(&["x.jpg", "x.png"]),
            &RenamePattern::Replace {
                find: "x".into(),
                with: "y".into(),
            },
        );
        assert!(
            doubled.iter().all(|r| r.problem.is_none()),
            "y.jpg and y.png differ"
        );
        let one_name = super::plan(
            &[("x".to_owned(), true), ("y".to_owned(), true)],
            &folder(&["x", "y"]),
            &RenamePattern::Replace {
                find: "x".into(),
                with: "y".into(),
            },
        );
        assert_eq!(
            one_name[0].problem,
            Some(RenameProblem::Taken),
            "y is there"
        );
        let twin = super::plan(
            &[("x1".to_owned(), true), ("x2".to_owned(), true)],
            &folder(&["x1", "x2"]),
            &RenamePattern::Replace {
                find: "1".into(),
                with: "2".into(),
            },
        );
        assert_eq!(twin[0].problem, Some(RenameProblem::Taken));
        let fresh = super::plan(
            &[("x1".to_owned(), true), ("y1".to_owned(), true)],
            &folder(&["x1", "y1"]),
            &RenamePattern::Number {
                base: "z".into(),
                start: 7,
            },
        );
        assert_eq!(new_names(&fresh), ["z 007", "z 008"]);
        let dup = super::plan(
            &[("p".to_owned(), true), ("q".to_owned(), true)],
            &folder(&["p", "q"]),
            &RenamePattern::Add {
                text: "-".into(),
                at_end: true,
            },
        );
        assert!(dup.iter().all(|r| r.problem.is_none()));
        let same_new = super::plan(
            &files(&["p.jpg", "P.png"]),
            &folder(&["p.jpg", "P.png"]),
            &RenamePattern::Replace {
                find: "p".into(),
                with: "q".into(),
            },
        );
        assert_eq!(
            new_names(&same_new),
            ["q.jpg", "P.png"],
            "case-sensitive find"
        );
        let pair = super::plan(
            &files(&["ab.jpg", "AB.jpg"]),
            &folder(&["ab.jpg", "AB.jpg"]),
            &RenamePattern::Add {
                text: "x".into(),
                at_end: false,
            },
        );
        assert_eq!(pair[0].problem, Some(RenameProblem::Twice));
        assert_eq!(pair[1].problem, Some(RenameProblem::Twice));
    }

    #[test]
    fn a_name_no_file_can_have_is_held_back_with_why() {
        let plan = super::plan(
            &files(&["a.jpg", "b.jpg"]),
            &folder(&["a.jpg", "b.jpg"]),
            &RenamePattern::Replace {
                find: "a".into(),
                with: "x/y".into(),
            },
        );
        assert_eq!(
            plan[0].problem,
            Some(RenameProblem::Invalid(NameProblem::IllegalCharacters))
        );
        assert_eq!(plan[0].to, "x/y.jpg", "shown as it would be");
        let empty = super::plan(
            &[("a".to_owned(), true)],
            &folder(&["a"]),
            &RenamePattern::Replace {
                find: "a".into(),
                with: String::new(),
            },
        );
        assert_eq!(
            empty[0].problem,
            Some(RenameProblem::Invalid(NameProblem::Empty))
        );
    }

    #[test]
    fn a_change_of_case_alone_is_a_rename() {
        let plan = super::plan(
            &files(&["img.jpg"]),
            &folder(&["img.jpg"]),
            &RenamePattern::Replace {
                find: "img".into(),
                with: "IMG".into(),
            },
        );
        assert_eq!(plan[0].to, "IMG.jpg");
        assert!(plan[0].changes());
    }

    #[test]
    fn planning_five_thousand_names_is_quick() {
        let names: Vec<String> = (0..5_000).map(|n| format!("IMG_{n:05}.jpg")).collect();
        let items: Vec<(String, bool)> = names.iter().map(|n| (n.clone(), false)).collect();
        let started = std::time::Instant::now();

        let plan = super::plan(
            &items,
            &names,
            &RenamePattern::Replace {
                find: "IMG_".into(),
                with: "Trip ".into(),
            },
        );

        assert!(plan.iter().all(Renaming::changes));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
    }
}
