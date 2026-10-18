//! A file name on one line, cut in the middle when it is too long for its
//! room: "IMG_2026…082133.jpg" keeps the start, the end and the extension,
//! where a cut at the right would leave only the start (a camera's names
//! differ at the end). iced has no ellipsis, so the cut is made while laying
//! out, measured with the renderer's own text, and kept until the name or
//! the room changes; only the rows on screen are laid out, so only they pay.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self, paragraph};
use iced::advanced::widget::{self, Tree, Widget, tree};
use iced::widget::text::{Format, Style};
use iced::{Element, Length, Pixels, Rectangle, Size, Theme, mouse};

/// What stands for the part of a name left out.
const CUT: char = '…';

/// An extension this long or shorter is kept whole at the end.
const LONGEST_EXTENSION: usize = 8;

/// `name` shortened to `keep` characters plus the mark, keeping its start and
/// its end with the extension; the whole name when it has no more than that.
pub fn middle_cut(name: &str, keep: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= keep {
        return name.to_owned();
    }
    let extension = name
        .rfind('.')
        .filter(|&at| at > 0)
        .map(|at| name[at..].chars().count())
        .filter(|&length| length <= LONGEST_EXTENSION)
        .unwrap_or(0);
    // The end gets half, and never less than the extension and a few
    // characters before it, while there is room; the start always keeps a
    // couple, so the name is still recognisable.
    let tail = (keep / 2).max((extension + 4).min(keep - keep.min(2)));
    let head = keep - tail;
    let mut cut: String = chars[..head].iter().collect();
    cut.push(CUT);
    cut.extend(&chars[chars.len() - tail..]);
    cut
}

/// Whether `name` at `size` points probably won't fit `room` points, so its
/// whole name is worth showing on hover. An estimate (an average letter is
/// a little over half as wide as it is tall), made where the view is built,
/// before anything is measured: a tooltip on a name that just fits costs
/// nothing but the tooltip.
pub fn probably_cut(name: &str, size: f32, room: f32) -> bool {
    name.chars().count() as f32 * size * 0.55 > room
}

pub struct MiddleText<'a> {
    name: &'a str,
    size: Pixels,
    style: fn(&Theme) -> Style,
}

/// A file name, cut in the middle if it doesn't fit.
pub fn middle_text<'a>(
    name: &'a str,
    size: impl Into<Pixels>,
    style: fn(&Theme) -> Style,
) -> MiddleText<'a> {
    MiddleText {
        name,
        size: size.into(),
        style,
    }
}

struct State<P: text::Paragraph> {
    /// What is drawn.
    shown: paragraph::Plain<P>,
    /// Used to measure each try.
    measuring: paragraph::Plain<P>,
    /// The name and the room the cut was made for.
    made_for: Option<(String, u32)>,
}

impl<P: text::Paragraph> Default for State<P> {
    fn default() -> Self {
        Self {
            shown: paragraph::Plain::default(),
            measuring: paragraph::Plain::default(),
            made_for: None,
        }
    }
}

impl MiddleText<'_> {
    fn format<Font>(&self) -> Format<Font> {
        Format {
            size: Some(self.size),
            wrapping: text::Wrapping::None,
            ..Format::default()
        }
    }

    /// The longest cut of the name that fits `room` points, measured with the
    /// renderer: a binary search over how many characters are kept.
    fn fit<P: text::Paragraph>(
        &self,
        measuring: &mut paragraph::Plain<P>,
        font: P::Font,
        room: f32,
    ) -> String {
        let mut width = |content: &str| {
            let _ = measuring.update(text::Text {
                content,
                bounds: Size::INFINITE,
                size: self.size,
                line_height: text::LineHeight::default(),
                font,
                align_x: text::Alignment::Default,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::default(),
                wrapping: text::Wrapping::None,
            });
            measuring.min_width()
        };
        if width(self.name) <= room {
            return self.name.to_owned();
        }
        let (mut fits, mut too_long) = (0, self.name.chars().count());
        while too_long - fits > 1 {
            let middle = (fits + too_long) / 2;
            if width(&middle_cut(self.name, middle)) <= room {
                fits = middle;
            } else {
                too_long = middle;
            }
        }
        middle_cut(self.name, fits)
    }
}

impl<Message, Renderer> Widget<Message, Theme, Renderer> for MiddleText<'_>
where
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let room = limits.max().width;
        let key = (self.name.to_owned(), room.to_bits());
        let content = if state.made_for.as_ref() == Some(&key) {
            state.shown.content().to_owned()
        } else {
            let content = self.fit(&mut state.measuring, renderer.default_font(), room);
            state.made_for = Some(key);
            content
        };
        widget::text::layout(&mut state.shown, renderer, limits, &content, self.format())
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        widget::text::draw(
            renderer,
            defaults,
            layout.bounds(),
            state.shown.raw(),
            (self.style)(theme),
            viewport,
        );
    }

    /// The whole name, whatever is shown: what finding text by name looks for.
    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        operation.text(None, layout.bounds(), self.name);
    }
}

impl<'a, Message, Renderer> From<MiddleText<'a>> for Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer + 'a,
{
    fn from(text: MiddleText<'a>) -> Self {
        Element::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_too_long_for_its_room_is_probably_cut() {
        assert!(probably_cut(
            "IMG_20260614_082133_HDR_PORTRAIT.jpg",
            13.0,
            120.0
        ));
        assert!(!probably_cut("a.jpg", 13.0, 120.0));
        assert!(!probably_cut("", 13.0, 0.0));
    }

    #[test]
    fn a_long_name_keeps_its_start_its_end_and_its_extension() {
        let name = "IMG_20260614_082133.jpg";
        assert_eq!(middle_cut(name, 30), name, "short enough: whole");
        assert_eq!(middle_cut(name, 23), name);
        assert_eq!(middle_cut(name, 16), "IMG_2026…2133.jpg");
        assert_eq!(
            middle_cut(name, 6),
            "IM….jpg",
            "little room: the start's first letters and the extension"
        );
        assert_eq!(middle_cut(name, 1), "I…");
        assert_eq!(middle_cut(name, 0), "…");
        assert_eq!(middle_cut("no extension at all here", 10), "no ex… here");
        assert_eq!(
            middle_cut("a very long name indeed.abcdefg", 14),
            "a …deed.abcdefg",
            "an eight-letter extension is kept whole with letters before it"
        );
        assert_eq!(
            middle_cut("archive.tar.verylongextension", 12),
            "archiv…ension",
            "an extension too long to keep whole is cut like the rest"
        );
        assert_eq!(
            middle_cut("ภาพถ่ายวันเกิดของคุณยาย.jpg", 12),
            "ภาพถ…ณยาย.jpg",
            "by character"
        );
    }
}
