//! Where the window opens, and what of where it is now is worth keeping.
//! The window opens where it was left, at the size it was left, full
//! screen if it was, unless the screen it was on has gone.

use iced::window::{self, Mode, Position};
use iced::{Point, Size};
use internal::dto;

/// The window's size on a first run.
pub const FIRST_SIZE: Size = Size::new(1100.0, 720.0);
/// Smaller than this, the two panes and the sidebar don't fit.
pub const SMALLEST: Size = Size::new(720.0, 480.0);
/// How much of the title bar must be on a screen for the window to be put
/// back where it was: enough to grab it and drag it.
const GRAB_WIDTH: i32 = 100;
const GRAB_HEIGHT: i32 = 20;

/// The window as it should open: where `saved` says, if its title bar is on
/// one of `screens` (the main one first), else centred; at its saved size,
/// made to fit the largest screen; full screen if it was.
pub fn opening(saved: Option<dto::WindowPlace>, screens: &[dto::Screen]) -> window::Settings {
    let mut settings = window::Settings {
        size: FIRST_SIZE,
        min_size: Some(SMALLEST),
        position: Position::Centered,
        // A close is asked about first while a transfer runs (`App`).
        exit_on_close_request: false,
        ..window::Settings::default()
    };
    let Some(place) = saved else {
        return settings;
    };
    let room = largest(screens);
    let side = |saved: u32, room: Option<u32>, least: f32| {
        let fitted = room.map_or(saved, |room| saved.min(room));
        (fitted as f32).max(least)
    };
    settings.size = Size::new(
        side(place.width, room.map(|r| r.0), SMALLEST.width),
        side(place.height, room.map(|r| r.1), SMALLEST.height),
    );
    if can_grab(&place, settings.size, screens) {
        settings.position = Position::Specific(Point::new(place.x as f32, place.y as f32));
    }
    settings.fullscreen = place.fullscreen;
    settings
}

/// The width and height of the largest screen.
fn largest(screens: &[dto::Screen]) -> Option<(u32, u32)> {
    screens
        .iter()
        .max_by_key(|screen| u64::from(screen.width) * u64::from(screen.height))
        .map(|screen| (screen.width, screen.height))
}

/// Whether enough of the title bar of a window at `place`, `size` big, is on
/// one of `screens` to grab it.
fn can_grab(place: &dto::WindowPlace, size: Size, screens: &[dto::Screen]) -> bool {
    let (left, top) = (place.x, place.y);
    let right = left.saturating_add(size.width as i32);
    let grab_width = GRAB_WIDTH.min(size.width as i32);
    screens.iter().any(|screen| {
        let screen_right = screen.x.saturating_add(screen.width as i32);
        let screen_bottom = screen.y.saturating_add(screen.height as i32);
        let across = right.min(screen_right) - left.max(screen.x);
        let down = top.saturating_add(GRAB_HEIGHT).min(screen_bottom) - top.max(screen.y);
        across >= grab_width && down >= GRAB_HEIGHT
    })
}

/// What to keep of the window now: its place and size while it is a window;
/// while it is full screen, the place it goes back to, marked full screen;
/// while it is hidden (minimized), what was kept before.
pub fn kept(
    before: Option<dto::WindowPlace>,
    mode: Mode,
    position: Option<Point>,
    size: Size,
) -> Option<dto::WindowPlace> {
    match mode {
        Mode::Hidden => before,
        Mode::Fullscreen => before.map(|place| dto::WindowPlace {
            fullscreen: true,
            ..place
        }),
        Mode::Windowed => match position {
            Some(position) => Some(dto::WindowPlace {
                x: position.x.round() as i32,
                y: position.y.round() as i32,
                width: size.width.round().max(0.0) as u32,
                height: size.height.round().max(0.0) as u32,
                fullscreen: false,
            }),
            None => before,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the window is put: a point, or `None` for centred.
    fn at(settings: &window::Settings) -> Option<Point> {
        match settings.position {
            Position::Specific(point) => Some(point),
            Position::Centered => None,
            other => panic!("placed some other way: {other:?}"),
        }
    }

    fn place(x: i32, y: i32, width: u32, height: u32) -> dto::WindowPlace {
        dto::WindowPlace {
            x,
            y,
            width,
            height,
            fullscreen: false,
        }
    }

    /// A 1512×982 laptop (37-point menu bar), and a 2560×1440 screen to its
    /// right.
    fn screens() -> Vec<dto::Screen> {
        vec![
            dto::Screen {
                x: 0,
                y: 37,
                width: 1512,
                height: 945,
            },
            dto::Screen {
                x: 1512,
                y: -200,
                width: 2560,
                height: 1415,
            },
        ]
    }

    #[test]
    fn a_first_run_opens_centred_at_the_usual_size() {
        let settings = opening(None, &screens());

        assert_eq!(settings.size, FIRST_SIZE);
        assert_eq!(at(&settings), None);
        assert_eq!(settings.min_size, Some(SMALLEST));
        assert!(!settings.fullscreen);
    }

    #[test]
    fn closing_the_window_never_ends_the_app_by_itself() {
        assert!(!opening(None, &[]).exit_on_close_request);
        assert!(!opening(Some(place(0, 0, 900, 600)), &screens()).exit_on_close_request);
    }

    #[test]
    fn the_window_opens_where_it_was_left_on_either_screen() {
        let on_laptop = opening(Some(place(80, 60, 1300, 800)), &screens());
        assert_eq!(at(&on_laptop), Some(Point::new(80.0, 60.0)));
        assert_eq!(on_laptop.size, Size::new(1300.0, 800.0));

        let on_the_other = opening(Some(place(2000, -100, 1800, 1100)), &screens());
        assert_eq!(at(&on_the_other), Some(Point::new(2000.0, -100.0)));
        assert_eq!(on_the_other.size, Size::new(1800.0, 1100.0));
    }

    #[test]
    fn a_window_left_on_a_screen_that_has_gone_opens_centred() {
        let laptop_only = &screens()[..1];

        let settings = opening(Some(place(2000, -100, 1800, 1100)), laptop_only);

        assert_eq!(at(&settings), None);
        assert_eq!(
            settings.size,
            Size::new(1512.0, 945.0),
            "made to fit the screen there is"
        );
    }

    #[test]
    fn only_a_title_bar_that_can_be_grabbed_counts() {
        // 120 points of its title bar on the laptop: grabbable.
        let edge = opening(Some(place(-1180, 100, 1300, 800)), &screens());
        assert_eq!(at(&edge), Some(Point::new(-1180.0, 100.0)));
        // Only 30 points of it on the screen: not.
        let sliver = opening(Some(place(-1270, 100, 1300, 800)), &screens());
        assert_eq!(at(&sliver), None);
        // Its title bar below the bottom of every screen.
        let below = opening(Some(place(100, 975, 1300, 800)), &screens());
        assert_eq!(at(&below), None);
        // No screens known: never trusted.
        let blind = opening(Some(place(100, 100, 1300, 800)), &[]);
        assert_eq!(at(&blind), None);
        assert_eq!(blind.size, Size::new(1300.0, 800.0));
    }

    #[test]
    fn a_window_kept_smaller_than_the_panes_need_opens_at_the_smallest() {
        let settings = opening(Some(place(10, 50, 300, 250)), &screens());

        assert_eq!(settings.size, SMALLEST);
    }

    #[test]
    fn full_screen_is_kept_with_the_place_it_goes_back_to() {
        let mut was = place(80, 60, 1300, 800);
        was.fullscreen = true;

        let settings = opening(Some(was), &screens());

        assert!(settings.fullscreen);
        assert_eq!(settings.size, Size::new(1300.0, 800.0));
    }

    #[test]
    fn what_is_kept_follows_the_window_but_not_its_full_screen_size() {
        let before = Some(place(80, 60, 1300, 800));

        let moved = kept(
            before,
            Mode::Windowed,
            Some(Point::new(120.4, 70.6)),
            Size::new(1280.2, 790.0),
        );
        assert_eq!(moved, Some(place(120, 71, 1280, 790)));

        let full = kept(
            moved,
            Mode::Fullscreen,
            Some(Point::ORIGIN),
            Size::new(1512.0, 982.0),
        );
        assert_eq!(
            full,
            Some(dto::WindowPlace {
                fullscreen: true,
                ..place(120, 71, 1280, 790)
            }),
            "full screen keeps the place it goes back to"
        );

        assert_eq!(
            kept(full, Mode::Hidden, None, Size::ZERO),
            full,
            "minimized"
        );
        assert_eq!(
            kept(full, Mode::Windowed, None, Size::new(1.0, 1.0)),
            full,
            "no position: nothing new"
        );
        assert_eq!(kept(None, Mode::Fullscreen, None, Size::ZERO), None);
    }
}
