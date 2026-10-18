//! Finder-style name order: case is ignored and runs of digits compare as
//! numbers, so `IMG_2` comes before `IMG_10` (plain text order puts `IMG_10`
//! first, which is how a camera folder ends up looking scrambled).
//!
//! It allocates nothing: the sort calls it `n log n` times, and a camera
//! folder holds tens of thousands of names.

use std::cmp::Ordering;

/// Orders two names; see the module docs. Names equal in every way that
/// matters to a person (`a.jpg` / `A.JPG`, `x1` / `x01`) are still told apart,
/// so the order never depends on how the phone happened to list them.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a, b);
    // `x1` and `x01` are the same number; the one written with fewer zeros
    // goes first, but only once everything else has been compared.
    let mut zeros = Ordering::Equal;

    loop {
        match (left.chars().next(), right.chars().next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, leading_x) = number(&mut left);
                let (y, leading_y) = number(&mut right);
                match x.len().cmp(&y.len()).then_with(|| x.cmp(y)) {
                    Ordering::Equal => {}
                    different => return different,
                }
                if zeros == Ordering::Equal {
                    zeros = leading_x.cmp(&leading_y);
                }
            }
            (Some(x), Some(y)) => {
                match lower(x).cmp(&lower(y)) {
                    Ordering::Equal => {}
                    different => return different,
                }
                left = &left[x.len_utf8()..];
                right = &right[y.len_utf8()..];
            }
        }
    }
    zeros.then_with(|| a.cmp(b))
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Takes the run of digits off the front of `rest`: its significant digits
/// (without leading zeros, borrowed from the name), and how many zeros it had.
fn number<'a>(rest: &mut &'a str) -> (&'a str, usize) {
    // ASCII digits are one byte each, so a byte count is a position.
    let length = rest.bytes().take_while(u8::is_ascii_digit).count();
    let digits = &rest[..length];
    *rest = &rest[length..];
    let significant = digits.trim_start_matches('0');
    (significant, digits.len() - significant.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    #[test]
    fn numbers_count_as_numbers() {
        assert_eq!(
            sorted(&["IMG_10.jpg", "IMG_2.jpg", "IMG_1.jpg", "IMG_100.jpg"]),
            ["IMG_1.jpg", "IMG_2.jpg", "IMG_10.jpg", "IMG_100.jpg"]
        );
    }

    #[test]
    fn case_is_ignored() {
        // `B` and `b` tie, and are then told apart the same way every time.
        assert_eq!(sorted(&["b", "A", "c", "B"]), ["A", "B", "b", "c"]);
        assert_eq!(
            sorted(&["Zebra", "apple", "Mango"]),
            ["apple", "Mango", "Zebra"]
        );
    }

    #[test]
    fn several_numbers_in_one_name_are_each_compared_as_numbers() {
        assert_eq!(
            sorted(&["s10e2", "s2e10", "s2e2", "s10e10"]),
            ["s2e2", "s2e10", "s10e2", "s10e10"]
        );
    }

    #[test]
    fn a_number_is_never_compared_with_text() {
        // Digits sort before letters, as in plain text.
        assert_eq!(sorted(&["a", "1", "b", "2"]), ["1", "2", "a", "b"]);
        assert_eq!(natural_cmp("a1", "ab"), Ordering::Less);
    }

    #[test]
    fn a_shorter_name_that_is_a_prefix_goes_first() {
        assert_eq!(
            sorted(&["IMG_1.jpg", "IMG_1", "IMG_"]),
            ["IMG_", "IMG_1", "IMG_1.jpg"]
        );
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a", ""), Ordering::Greater);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn leading_zeros_do_not_change_the_number_but_break_a_tie() {
        assert_eq!(natural_cmp("x01", "x2"), Ordering::Less, "1 < 2");
        assert_eq!(natural_cmp("x10", "x09"), Ordering::Greater, "10 > 9");
        // Same number: fewer zeros first, but only after the rest is compared.
        assert_eq!(sorted(&["x001", "x1", "x01"]), ["x1", "x01", "x001"]);
        assert_eq!(
            natural_cmp("x01a", "x1b"),
            Ordering::Less,
            "the rest decides first"
        );
        assert_eq!(natural_cmp("x1b", "x01a"), Ordering::Greater);
    }

    #[test]
    fn zero_is_a_number_too() {
        assert_eq!(
            sorted(&["a10", "a0", "a00", "a1"]),
            ["a0", "a00", "a1", "a10"]
        );
    }

    #[test]
    fn numbers_too_big_for_any_integer_still_compare() {
        let big = "9".repeat(60);
        let bigger = format!("1{}", "0".repeat(60));
        let name = |n: &str| format!("f{n}.bin");

        assert_eq!(natural_cmp(&name(&big), &name(&bigger)), Ordering::Less);
        assert_eq!(natural_cmp(&name(&bigger), &name(&big)), Ordering::Greater);
        assert_eq!(natural_cmp(&name(&big), &name(&big)), Ordering::Equal);
    }

    #[test]
    fn names_equal_but_for_case_are_still_told_apart() {
        assert_ne!(natural_cmp("a.jpg", "A.JPG"), Ordering::Equal);
        assert_eq!(
            natural_cmp("a.jpg", "A.JPG"),
            natural_cmp("a.jpg", "A.JPG").reverse().reverse()
        );
        assert_eq!(
            natural_cmp("a.jpg", "A.JPG"),
            natural_cmp("A.JPG", "a.jpg").reverse()
        );
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn non_latin_names_sort_by_letter_without_losing_their_numbers() {
        assert_eq!(sorted(&["รูป10", "รูป2", "รูป1"]), ["รูป1", "รูป2", "รูป10"]);
        assert_eq!(
            sorted(&["éb", "Éa"]),
            ["Éa", "éb"],
            "case-insensitive for accents too"
        );
        assert_eq!(sorted(&["😀2", "😀10"]), ["😀2", "😀10"]);
    }

    #[test]
    fn only_ascii_digits_are_numbers() {
        // Thai and Arabic-Indic digits stay ordinary characters.
        assert_eq!(natural_cmp("a๑๐", "a๒"), "a๑๐".cmp("a๒"));
    }

    #[test]
    fn the_order_is_total_and_consistent() {
        let names = [
            "",
            "a",
            "A",
            "a1",
            "a01",
            "a2",
            "a10",
            "b",
            "IMG_1",
            "img_1",
            "IMG_01",
            "IMG_10",
            "x1b",
            "x01a",
            "😀",
            "รูป1",
            "a1.",
            "a1.jpg",
        ];
        for a in names {
            assert_eq!(natural_cmp(a, a), Ordering::Equal, "{a:?} with itself");
            for b in names {
                assert_eq!(
                    natural_cmp(a, b),
                    natural_cmp(b, a).reverse(),
                    "{a:?} vs {b:?} is not antisymmetric"
                );
                for c in names {
                    if natural_cmp(a, b) != Ordering::Greater
                        && natural_cmp(b, c) != Ordering::Greater
                    {
                        assert_ne!(
                            natural_cmp(a, c),
                            Ordering::Greater,
                            "{a:?} <= {b:?} <= {c:?} but not {a:?} <= {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sorting_never_panics_on_awkward_input() {
        let names = [
            "",
            "0",
            "00",
            "٣",
            "\u{0301}",
            "a\u{0}b",
            "9999999999999999999999",
            "🙂🙂",
        ];
        for a in names {
            for b in names {
                let _ = natural_cmp(a, b);
            }
        }
    }

    #[test]
    fn accented_letters_are_not_folded_into_their_plain_letter() {
        // A known limit: Finder would put `É` beside `E`; this compares the
        // lowercase letters as they are, so it lands after `Z`. Proper
        // collation needs a Unicode collation library, which is not worth a
        // dependency here. This pins the behaviour so a change is deliberate.
        assert_eq!(
            sorted(&["Zebra", "Éclair", "Apple"]),
            ["Apple", "Zebra", "Éclair"]
        );
    }
}
