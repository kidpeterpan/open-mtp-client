//! Wraps the file list for the lasso (drag a box to select) and for drags
//! coming in from the other pane. It reports where the left button went down,
//! in its own coordinates, and whether something inside (a row, a tile, the
//! scrollbar) already took that press; while a lasso is running it reports
//! every pointer movement, even outside it, so the box can grow past the edge
//! and the list scroll; while a drag from the other pane is under way it says
//! when the pointer comes in and goes out. It draws the box over the list.
//!
//! Like `right_click`, it must sit outside the list's `scrollable` (which
//! shifts the cursor for its content) and never captures anything, so rows
//! keep working underneath.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, overlay, renderer};
use iced::{Border, Color, Element, Event, Length, Point, Rectangle, Size, Vector, mouse};

type OnPress<'a, Message> = Box<dyn Fn(Point, Size, bool) -> Message + 'a>;
type OnMove<'a, Message> = Box<dyn Fn(Point, Size) -> Message + 'a>;
type OnHover<'a, Message> = Box<dyn Fn(bool) -> Message + 'a>;

pub struct Lasso<'a, Message, Renderer = iced::Renderer> {
    content: Element<'a, Message, iced::Theme, Renderer>,
    on_press: OnPress<'a, Message>,
    on_move: Option<OnMove<'a, Message>>,
    on_hover: Option<OnHover<'a, Message>>,
    rectangle: Option<Rectangle>,
}

/// `on_press(at, size, taken)`: the left button went down at `at` over a list
/// of `size`; `taken` says a widget inside handled the press.
pub fn lasso<'a, Message, Renderer>(
    content: impl Into<Element<'a, Message, iced::Theme, Renderer>>,
    on_press: impl Fn(Point, Size, bool) -> Message + 'a,
) -> Lasso<'a, Message, Renderer> {
    Lasso {
        content: content.into(),
        on_press: Box::new(on_press),
        on_move: None,
        on_hover: None,
        rectangle: None,
    }
}

impl<'a, Message, Renderer> Lasso<'a, Message, Renderer> {
    /// While a lasso runs: every movement, where the pointer is and how big
    /// the list is.
    pub fn on_move(mut self, on_move: impl Fn(Point, Size) -> Message + 'a) -> Self {
        self.on_move = Some(Box::new(on_move));
        self
    }

    /// While a drag comes in from elsewhere: the pointer came in (`true`) or
    /// went out (`false`). Said on the change only.
    pub fn on_hover(mut self, on_hover: impl Fn(bool) -> Message + 'a) -> Self {
        self.on_hover = Some(Box::new(on_hover));
        self
    }

    /// The box to draw, in this widget's coordinates.
    pub fn rectangle(mut self, rectangle: Option<Rectangle>) -> Self {
        self.rectangle = rectangle;
        self
    }
}

/// Whether the pointer was inside when last looked at, while hover is
/// reported; `None` when it isn't, so a new drag reports afresh.
#[derive(Default)]
struct State {
    inside: Option<bool>,
}

impl<Message, Renderer> Widget<Message, iced::Theme, Renderer> for Lasso<'_, Message, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        let bounds = layout.bounds();
        let local = |at: Point| Point::new(at.x - bounds.x, at.y - bounds.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(at) = cursor.position_over(bounds) {
                    let taken = shell.is_event_captured();
                    shell.publish((self.on_press)(local(at), bounds.size(), taken));
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(on_move) = &self.on_move {
                    shell.publish(on_move(local(*position), bounds.size()));
                }
            }
            _ => {}
        }

        let state = tree.state.downcast_mut::<State>();
        match &self.on_hover {
            Some(on_hover) => {
                if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                    let inside = bounds.contains(*position);
                    if state.inside != Some(inside) {
                        state.inside = Some(inside);
                        shell.publish(on_hover(inside));
                    }
                }
            }
            None => state.inside = None,
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let Some(rectangle) = self.rectangle else {
            return;
        };
        let bounds = layout.bounds();
        // Neutral, as Finder's: in the accent colour it vanished over the
        // rows it had just selected.
        let ink = theme.extended_palette().background.base.text;
        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: bounds.x + rectangle.x,
                        y: bounds.y + rectangle.y,
                        ..rectangle
                    },
                    border: Border {
                        color: Color { a: 0.55, ..ink },
                        width: 1.0,
                        radius: 2.0.into(),
                    },
                    ..renderer::Quad::default()
                },
                Color { a: 0.12, ..ink },
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Renderer> From<Lasso<'a, Message, Renderer>>
    for Element<'a, Message, iced::Theme, Renderer>
where
    Message: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(widget: Lasso<'a, Message, Renderer>) -> Self {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::{column, container, mouse_area, space, text};
    use iced_test::simulator;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Row(&'static str),
        Pressed(Point, bool),

        Moved(Point, Size),
        Hover(bool),
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn moved(x: f32, y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(x, y),
        })
    }

    /// Two rows that take presses, and empty space under them, inside a
    /// 20 px padding.
    fn list<'a>(tracking: bool, hover: bool) -> Element<'a, Message> {
        let row = |name: &'static str| {
            mouse_area(container(text(name)).height(30).width(Length::Fill))
                .on_press(Message::Row(name))
        };
        let rows = column![row("a"), row("b"), space().height(Length::Fill)];
        let mut wrapped = lasso(rows, |at, _, taken| Message::Pressed(at, taken));
        if tracking {
            wrapped = wrapped.on_move(Message::Moved);
        }
        if hover {
            wrapped = wrapped.on_hover(Message::Hover);
        }
        container(wrapped).padding(20).into()
    }

    #[test]
    fn a_press_on_a_row_says_the_row_took_it_and_where_in_the_list() {
        let mut ui = simulator(list(false, false));
        ui.point_at(Point::new(50.0, 65.0));
        let _ = ui.simulate([press()]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [
                Message::Row("b"),
                Message::Pressed(Point::new(30.0, 45.0), true)
            ]
        );
    }

    #[test]
    fn a_press_on_empty_space_is_nobodys() {
        let mut ui = simulator(list(false, false));
        ui.point_at(Point::new(50.0, 300.0));
        let _ = ui.simulate([press()]);
        ui.point_at(Point::new(5.0, 5.0));
        let _ = ui.simulate([press()]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Pressed(Point::new(30.0, 280.0), false)],
            "and a press outside says nothing"
        );
    }

    #[test]
    fn movements_are_reported_only_while_tracking_even_outside() {
        let mut quiet = simulator(list(false, false));
        let _ = quiet.simulate([moved(50.0, 50.0)]);
        assert_eq!(quiet.into_messages().count(), 0);

        let mut ui = simulator(list(true, false));
        let _ = ui.simulate([moved(10.0, 5.0)]);
        let messages: Vec<_> = ui.into_messages().collect();
        let [Message::Moved(at, size)] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(*at, Point::new(-10.0, -15.0), "above and left of it");
        assert!(size.height > 0.0);
    }

    #[test]
    fn hover_is_said_on_the_change_only() {
        let mut ui = simulator(list(false, true));
        let _ = ui.simulate([
            moved(5.0, 5.0),
            moved(6.0, 6.0),
            moved(50.0, 50.0),
            moved(60.0, 70.0),
            moved(1.0, 1.0),
        ]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [
                Message::Hover(false),
                Message::Hover(true),
                Message::Hover(false)
            ]
        );
    }

    #[test]
    fn the_box_is_drawn_over_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let hash = dir.path().join("box");
        let looks = |rectangle: Option<Rectangle>| {
            let content: Element<'_, Message> = container(space()).width(200).height(200).into();
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                Size::new(200.0, 200.0),
                lasso(content, |at, _, taken| Message::Pressed(at, taken)).rectangle(rectangle),
            );
            ui.snapshot(&iced::Theme::Light)
                .unwrap()
                .matches_hash(&hash)
                .unwrap()
        };

        assert!(looks(None), "writes the empty look");
        assert!(!looks(Some(Rectangle::new(
            Point::new(20.0, 20.0),
            Size::new(80.0, 60.0)
        ))));
    }
}
