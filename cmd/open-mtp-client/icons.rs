//! The app's icons: Lucide (ISC), a subset of its icon font in
//! `assets/fonts/lucide.ttf`, drawn as text so they take the text's colour and
//! size and cost no new dependency (iced's `svg` feature is off). Regenerate the
//! subset with `scripts/make_icon_font.py` after adding a name to `USED` there.
//!
//! One glyph per name, so a view says `icons::icon(icons::CHECK, 14.0)` and a
//! test finds it by `icons::CHECK`.

use iced::widget::{Text, text};
use iced::{Font, alignment};

/// The family name inside the font file; `main.rs` loads the bytes.
pub const FONT: Font = Font::with_name("lucide");

/// The font file itself, for `main.rs` to register.
pub const FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/lucide.ttf");

pub const FOLDER: char = '\u{e0d7}';
pub const FILE: char = '\u{e0c0}';
pub const FILE_TEXT: char = '\u{e0cc}';
pub const FILE_IMAGE: char = '\u{e31c}';
pub const FILE_VIDEO: char = '\u{e321}';
pub const FILE_AUDIO: char = '\u{e31a}';
pub const FILE_ARCHIVE: char = '\u{e30d}';
pub const PACKAGE: char = '\u{e129}';

pub const BACK: char = '\u{e06e}';
pub const FORWARD: char = '\u{e06f}';
pub const UP: char = '\u{e04a}';
pub const REFRESH: char = '\u{e145}';
pub const LIST: char = '\u{e106}';
pub const GRID: char = '\u{e0ff}';
/// Between two crumbs of a path.
pub const CRUMB: char = '\u{e06f}';

pub const CLOSE: char = '\u{e1b2}';
pub const CHECK: char = '\u{e06c}';
pub const EQUAL: char = '\u{e1bd}';
pub const MINUS: char = '\u{e11c}';
pub const WARNING: char = '\u{e193}';

pub const HOUSE: char = '\u{e0f5}';
pub const MONITOR: char = '\u{e11d}';
pub const DOWNLOAD: char = '\u{e0b2}';
pub const IMAGE: char = '\u{e0f6}';
pub const MUSIC: char = '\u{e122}';
pub const CAMERA: char = '\u{e064}';
pub const CHAT: char = '\u{e116}';
pub const HARD_DRIVE: char = '\u{e0ed}';
pub const SYNC_FOLDER: char = '\u{e4c9}';
pub const IMPORT: char = '\u{e53c}';
pub const CLEAN: char = '\u{e666}';
pub const UNPLUG: char = '\u{e45d}';

/// One icon, as a square of `size` points so rows of them line up.
pub fn icon<'a>(glyph: char, size: f32) -> Text<'a> {
    #[cfg(test)]
    load_for_tests();
    text(glyph.to_string())
        .font(FONT)
        .size(size)
        .line_height(1.0)
        .width(size)
        .height(size)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
}

/// `iced_test`'s simulator is built without the font `main.rs` registers, and
/// text in a family the font system does not have sends it looking through every
/// font on the machine, under one lock, every time: the whole suite ran three
/// times slower. Registering the file once, the way the app does, gives the tests
/// the app's own lookup.
#[cfg(test)]
fn load_for_tests() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        iced::advanced::graphics::text::font_system()
            .write()
            .expect("the font system")
            .load_font(FONT_BYTES.into());
    });
}

/// The icon of a usual folder, from where it is: Camera, Screenshots, Downloads,
/// Music and the like (on the phone or the Mac); any other folder is a folder.
/// Names are the English ones the platforms use for the path, not what the
/// sidebar shows, so it holds in every language.
pub fn folder_at(path: &str) -> char {
    let path = path.to_ascii_lowercase();
    if path.contains("whatsapp") {
        return CHAT;
    }
    match path.trim_end_matches('/').rsplit('/').next().unwrap_or("") {
        "camera" | "dcim" => CAMERA,
        "screenshots" | "pictures" | "photos" => IMAGE,
        "download" | "downloads" => DOWNLOAD,
        "music" => MUSIC,
        "desktop" => MONITOR,
        _ => FOLDER,
    }
}

/// The icon for a result mark the view models carry as plain text ("✓" copied,
/// "=" the same, "↻" replaced, "–" skipped, "✕" or "✗" failed, "⚠" some did not
/// go), so the mappers and their tests keep saying it in words while the screen
/// draws it. Anything else is shown as the text it is.
pub fn mark<'a>(mark: &str, size: f32) -> Text<'a> {
    let glyph = match mark {
        "✓" => CHECK,
        "✕" | "✗" => CLOSE,
        "=" => EQUAL,
        "–" => MINUS,
        "↻" => REFRESH,
        "⚠" => WARNING,
        other => return text(other.to_owned()).size(size).width(size),
    };
    icon(glyph, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon the app names is a glyph the shipped font has: a name that
    /// slipped past the subset would draw as an empty box.
    #[test]
    fn every_named_icon_is_in_the_font_file() {
        let font = FONT_BYTES;
        let cmap = glyphs_in(font);
        for glyph in [
            FOLDER,
            FILE,
            FILE_TEXT,
            FILE_IMAGE,
            FILE_VIDEO,
            FILE_AUDIO,
            FILE_ARCHIVE,
            PACKAGE,
            BACK,
            FORWARD,
            UP,
            REFRESH,
            LIST,
            GRID,
            CRUMB,
            CLOSE,
            CHECK,
            EQUAL,
            MINUS,
            WARNING,
            HOUSE,
            MONITOR,
            DOWNLOAD,
            IMAGE,
            MUSIC,
            CAMERA,
            CHAT,
            HARD_DRIVE,
            SYNC_FOLDER,
            IMPORT,
            CLEAN,
            UNPLUG,
        ] {
            assert!(cmap.contains(&(glyph as u32)), "U+{:04X}", glyph as u32);
        }
    }

    #[test]
    fn a_usual_folder_gets_its_own_icon_and_any_other_a_folder() {
        assert_eq!(folder_at("/DCIM/Camera"), CAMERA);
        assert_eq!(folder_at("/Pictures/Screenshots"), IMAGE);
        assert_eq!(folder_at("/Download"), DOWNLOAD);
        assert_eq!(folder_at("/Users/me/Downloads/"), DOWNLOAD);
        assert_eq!(folder_at("/Music"), MUSIC);
        assert_eq!(folder_at("/Users/me/Desktop"), MONITOR);
        assert_eq!(
            folder_at("/Android/media/com.whatsapp/WhatsApp/Media"),
            CHAT
        );
        assert_eq!(folder_at("/Documents"), FOLDER);
        assert_eq!(folder_at("/"), FOLDER);
    }

    #[test]
    fn a_result_mark_is_drawn_as_its_icon_and_an_unknown_one_as_text() {
        for known in ["✓", "✕", "✗", "=", "–", "↻", "⚠"] {
            let _ = mark(known, 12.0);
        }
        let _ = mark("?", 12.0);
    }

    /// The code points a TrueType font's format-4 `cmap` covers: enough to see
    /// whether a glyph is there, without a font-parsing dependency.
    fn glyphs_in(font: &[u8]) -> Vec<u32> {
        let u16_at = |at: usize| u16::from_be_bytes([font[at], font[at + 1]]) as usize;
        let tables = u16_at(4);
        let cmap = (0..tables)
            .map(|n| 12 + n * 16)
            .find(|&at| &font[at..at + 4] == b"cmap")
            .map(|at| u32::from_be_bytes(font[at + 8..at + 12].try_into().unwrap()) as usize)
            .expect("a cmap table");
        let subtables = u16_at(cmap + 2);
        let mut found = Vec::new();
        for n in 0..subtables {
            let offset = cmap
                + u32::from_be_bytes(
                    font[cmap + 4 + n * 8 + 4..cmap + 4 + n * 8 + 8]
                        .try_into()
                        .unwrap(),
                ) as usize;
            if u16_at(offset) != 4 {
                continue;
            }
            let segments = u16_at(offset + 6) / 2;
            let ends = offset + 14;
            let starts = ends + segments * 2 + 2;
            for s in 0..segments {
                let (end, start) = (u16_at(ends + s * 2), u16_at(starts + s * 2));
                if start != 0xFFFF {
                    found.extend((start..=end).map(|c| c as u32));
                }
            }
        }
        found
    }
}
