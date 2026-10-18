//! Wraps the file list and reports where a right-click happened: the cursor
//! and the list's bounds, both in window coordinates, so the menu can be
//! drawn at the cursor and kept inside the pane. iced's `mouse_area` tells
//! *that* a right-click happened but not where.
//!
//! It acts even when a row inside already handled the press (each row's
//! `mouse_area` says *which* entry was clicked), and it must sit outside the
//! list's `scrollable`, which shifts the cursor for its content. It never
//! captures the press, so as the backdrop of an open menu it can close that
//! menu while the same right-click opens a new one underneath.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Shell, Widget, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, mouse};

pub struct RightClick<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_press: Box<dyn Fn(Point, Rectangle) -> Message + 'a>,
}

pub fn right_click<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_press: impl Fn(Point, Rectangle) -> Message + 'a,
) -> RightClick<'a, Message, Theme, Renderer> {
    RightClick {
        content: content.into(),
        on_press: Box::new(on_press),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for RightClick<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
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

        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event
            && let Some(at) = cursor.position_over(layout.bounds())
        {
            shell.publish((self.on_press)(at, layout.bounds()));
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
        theme: &Theme,
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
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<RightClick<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(widget: RightClick<'a, Message, Theme, Renderer>) -> Self {
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
        At(Point, Rectangle),
        Left,
    }

    fn press(button: mouse::Button) -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(button))
    }

    fn list<'a>() -> Element<'a, Message> {
        let row = |name: &'static str| {
            mouse_area(container(text(name)).height(30).width(Length::Fill))
                .on_right_press(Message::Row(name))
                .on_press(Message::Left)
        };
        let rows = column![row("a"), row("b"), space().height(Length::Fill)];
        container(right_click(rows, Message::At)).padding(20).into()
    }

    #[test]
    fn a_right_click_on_a_row_reports_the_row_then_where() {
        let mut ui = simulator(list());
        ui.point_at(Point::new(50.0, 65.0));
        let _ = ui.simulate([press(mouse::Button::Right)]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert_eq!(messages[0], Message::Row("b"));
        let Message::At(at, bounds) = messages[1] else {
            panic!("{messages:?}");
        };
        assert_eq!(at, Point::new(50.0, 65.0), "window coordinates");
        assert_eq!((bounds.x, bounds.y), (20.0, 20.0), "inside the padding");
    }

    #[test]
    fn a_right_click_on_empty_space_reports_only_where() {
        let mut ui = simulator(list());
        ui.point_at(Point::new(50.0, 300.0));
        let _ = ui.simulate([press(mouse::Button::Right)]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::At(..)]),
            "{messages:?}"
        );
    }

    #[test]
    fn left_clicks_and_clicks_outside_pass_through() {
        let mut ui = simulator(list());
        ui.point_at(Point::new(50.0, 35.0));
        let _ = ui.simulate([press(mouse::Button::Left)]);
        ui.point_at(Point::new(5.0, 5.0));
        let _ = ui.simulate([press(mouse::Button::Right)]);

        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Left]);
    }
}
