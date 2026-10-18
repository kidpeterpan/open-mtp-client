//! The Mac's screens, for putting the window back where it was: a place
//! saved on a screen that has since been unplugged must not open the window
//! where nobody can see it.
//!
//! Read from Core Graphics, whose display bounds are already in points from
//! the main screen's top left (as windows are placed) and which answers on
//! any thread. AppKit's `NSScreen.screens` was tried first: its array is
//! Swift's, whose `count` returns a signed integer where AppKit declares an
//! unsigned one, and objc2's debug check refuses the call.

use crate::dto::Screen;

/// The bounds of each screen, the main screen (the one with the menu bar)
/// first.
pub fn screens() -> Vec<Screen> {
    let mut displays = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: Core Graphics fills at most `displays.len()` ids and says how
    // many in `count`; both point at live memory of the right size.
    let status =
        unsafe { CGGetActiveDisplayList(displays.len() as u32, displays.as_mut_ptr(), &mut count) };
    if status != 0 {
        return Vec::new();
    }
    // SAFETY: a plain query with no arguments.
    let main = unsafe { CGMainDisplayID() };
    let mut ids: Vec<u32> = displays[..(count as usize).min(displays.len())].to_vec();
    ids.sort_by_key(|id| *id != main);
    ids.into_iter()
        // SAFETY: each id came from Core Graphics just now; an id that has
        // gone since gives an empty rectangle, which is left out.
        .map(|id| to_screen(unsafe { CGDisplayBounds(id) }))
        .filter(|screen| screen.width > 0 && screen.height > 0)
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

fn to_screen(bounds: CGRect) -> Screen {
    Screen {
        x: bounds.origin.x.round() as i32,
        y: bounds.origin.y.round() as i32,
        width: bounds.size.width.max(0.0).round() as u32,
        height: bounds.size.height.max(0.0).round() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_become_whole_points() {
        let screen = to_screen(CGRect {
            origin: CGPoint {
                x: -1000.4,
                y: -1440.0,
            },
            size: CGSize {
                width: 2560.0,
                height: 1439.6,
            },
        });

        assert_eq!(
            screen,
            Screen {
                x: -1000,
                y: -1440,
                width: 2560,
                height: 1440
            }
        );
        assert_eq!(
            to_screen(CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: -5.0,
                    height: 10.0
                },
            })
            .width,
            0
        );
    }

    // Reads the Mac's real screens: a query, nothing changes.
    #[test]
    fn the_main_screen_comes_first_at_the_top_left() {
        let screens = screens();

        assert!(!screens.is_empty(), "a Mac running tests has a screen");
        assert_eq!((screens[0].x, screens[0].y), (0, 0), "{screens:?}");
        assert!(
            screens
                .iter()
                .all(|screen| screen.width >= 200 && screen.height >= 200)
        );
    }
}
