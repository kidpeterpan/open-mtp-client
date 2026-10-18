//! Places in the sidebar a drag can be dropped on: a Mac place or pin, a
//! phone storage, a phone pin or one of the phone's usual places. The sidebar's
//! handlers draw each through `drop_spot`, which (only while something is
//! being dragged, so moving over the sidebar otherwise publishes nothing)
//! says when the pointer comes and goes and outlines the one it is over;
//! `app.rs` keeps which one that is and drops there on release.

use iced::widget::{container, mouse_area, space, stack};
use iced::{Border, Element, Length, Theme};
use internal::dto;

/// Where a sidebar entry goes, and where a drop on it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// A Mac folder.
    Mac(String),
    /// A folder on a storage of the open phone.
    Phone { storage_id: u64, path: String },
}

/// Where a drag comes from: the folder its items are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Mac { folder: String },
    Phone { storage_id: u64, folder: String },
}

/// What a drag in progress means for the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarDrop {
    /// The spot the pointer is over, if any.
    pub over: Option<Spot>,
    pub from: Origin,
}

impl SidebarDrop {
    /// Whether `spot` can take this drag: from the Mac, anywhere; from the
    /// phone, the Mac or the same storage (another storage of the phone
    /// would mean a copy the phone may not do by itself). Never the folder
    /// the items are in already, where a drop would do nothing.
    pub fn accepts(&self, spot: &Spot) -> bool {
        match (&self.from, spot) {
            (Origin::Mac { folder }, Spot::Mac(path)) => folder != path,
            (Origin::Mac { .. }, Spot::Phone { .. }) => true,
            (Origin::Phone { .. }, Spot::Mac(_)) => true,
            (
                Origin::Phone { storage_id, folder },
                Spot::Phone {
                    storage_id: to,
                    path,
                },
            ) => storage_id == to && folder != path,
        }
    }

    /// Finder's rule: a drop on the same disk (or the same phone storage)
    /// moves, ⌥ copies; a drop anywhere else copies, ⌘ moves.
    pub fn mode(&self, spot: &Spot, command: bool, option: bool) -> dto::Paste {
        let same_place = match (&self.from, spot) {
            (Origin::Mac { folder }, Spot::Mac(path)) => mac_disk(folder) == mac_disk(path),
            (Origin::Phone { storage_id, .. }, Spot::Phone { storage_id: to, .. }) => {
                storage_id == to
            }
            _ => false,
        };
        let moves = if same_place { !option } else { command };
        if moves {
            dto::Paste::Move
        } else {
            dto::Paste::Copy
        }
    }
}

/// The disk a Mac path is on, as far as its path tells: a disk mounted at
/// `/Volumes/<name>` is its own, everything else is the start-up disk.
pub fn mac_disk(path: &str) -> &str {
    let Some(rest) = path.strip_prefix("/Volumes/") else {
        return "/";
    };
    let name = rest.split('/').next().unwrap_or(rest);
    &path[.."/Volumes/".len() + name.len()]
}

/// `content` as a spot a drag can be dropped on. With no drag on, or a spot
/// this drag can't use, it is `content` as it was. `hover(spot, true)` is
/// sent when the pointer comes onto it and `hover(spot, false)` when it
/// leaves, naming the spot so the leaving of one can't undo the coming onto
/// its neighbour, whichever is heard first.
pub fn drop_spot<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
    spot: Spot,
    drop: Option<&SidebarDrop>,
    hover: impl Fn(Spot, bool) -> Message,
) -> Element<'a, Message> {
    let Some(drop) = drop.filter(|drop| drop.accepts(&spot)) else {
        return content.into();
    };
    let over = drop.over.as_ref() == Some(&spot);
    // The outline is a layer over the entry (whose button would paint over
    // a border of its own container), so lighting it moves nothing.
    let outline = container(space())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            border: Border {
                color: if over {
                    theme.extended_palette().primary.strong.color
                } else {
                    iced::Color::TRANSPARENT
                },
                width: 2.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        });
    mouse_area(stack![content.into(), outline])
        .on_enter(hover(spot.clone(), true))
        .on_exit(hover(spot, false))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{button, column, text};
    use iced_test::simulator;

    fn phone(storage_id: u64) -> Spot {
        Spot::Phone {
            storage_id,
            path: "/DCIM".into(),
        }
    }

    fn from_mac(folder: &str) -> SidebarDrop {
        SidebarDrop {
            over: None,
            from: Origin::Mac {
                folder: folder.into(),
            },
        }
    }

    fn from_phone(storage_id: u64, folder: &str) -> SidebarDrop {
        SidebarDrop {
            over: None,
            from: Origin::Phone {
                storage_id,
                folder: folder.into(),
            },
        }
    }

    #[test]
    fn a_mac_drag_goes_anywhere_and_a_phone_drag_to_the_mac_or_its_own_storage() {
        let mac = from_mac("/Users/me/Desktop");
        assert!(mac.accepts(&Spot::Mac("/Users/me".into())));
        assert!(mac.accepts(&phone(1)));
        assert!(
            !mac.accepts(&Spot::Mac("/Users/me/Desktop".into())),
            "the folder its items are in"
        );

        let phone_drag = from_phone(1, "/Download");
        assert!(phone_drag.accepts(&Spot::Mac("/Users/me".into())));
        assert!(phone_drag.accepts(&phone(1)));
        assert!(!phone_drag.accepts(&phone(2)), "another storage");
        assert!(
            !from_phone(1, "/DCIM").accepts(&phone(1)),
            "the folder its items are in"
        );
    }

    #[test]
    fn the_same_disk_moves_unless_option_and_anywhere_else_copies_unless_command() {
        use dto::Paste::{Copy, Move};
        let mac = from_mac("/Users/me/Desktop");
        let home = Spot::Mac("/Users/me".into());
        let disk = Spot::Mac("/Volumes/Backup/Photos".into());
        assert_eq!(mac.mode(&home, false, false), Move);
        assert_eq!(mac.mode(&home, false, true), Copy);
        assert_eq!(mac.mode(&disk, false, false), Copy);
        assert_eq!(mac.mode(&disk, true, false), Move);
        assert_eq!(mac.mode(&phone(1), false, false), Copy);
        assert_eq!(mac.mode(&phone(1), true, false), Move);
        assert_eq!(
            from_mac("/Volumes/Backup").mode(&disk, false, false),
            Move,
            "within one disk"
        );

        let phone_drag = from_phone(1, "/Download");
        assert_eq!(phone_drag.mode(&phone(1), false, false), Move);
        assert_eq!(phone_drag.mode(&phone(1), false, true), Copy);
        assert_eq!(phone_drag.mode(&home, false, false), Copy);
        assert_eq!(phone_drag.mode(&home, true, false), Move);
    }

    #[test]
    fn a_path_under_volumes_is_on_its_own_disk() {
        assert_eq!(mac_disk("/Users/me"), "/");
        assert_eq!(mac_disk("/"), "/");
        assert_eq!(mac_disk("/Volumes/Backup"), "/Volumes/Backup");
        assert_eq!(mac_disk("/Volumes/Backup/a/b"), "/Volumes/Backup");
        assert_ne!(mac_disk("/Volumes/Back"), mac_disk("/Volumes/Backup"));
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Heard {
        Hover(Spot, bool),
        Pressed,
    }

    #[test]
    fn a_spot_says_when_a_drag_comes_and_goes_and_only_while_one_is_on() {
        let spot = Spot::Mac("/Users/me".into());
        let entry = || button(text("Home")).on_press(Heard::Pressed);
        let drop = from_mac("/Users/me/Desktop");

        let mut ui = simulator(column![drop_spot(
            entry(),
            spot.clone(),
            Some(&drop),
            Heard::Hover
        )]);
        let at = ui.find("Home").unwrap().bounds().center();
        ui.point_at(at);
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: at,
        })]);
        ui.point_at(iced::Point::new(at.x, at.y + 500.0));
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: iced::Point::new(at.x, at.y + 500.0),
        })]);
        let heard: Vec<Heard> = ui.into_messages().collect();
        assert_eq!(
            heard,
            [
                Heard::Hover(spot.clone(), true),
                Heard::Hover(spot.clone(), false)
            ]
        );

        // No drag, or a spot the drag can't use: the entry as it was.
        for drop in [None, Some(&from_mac("/Users/me"))] {
            let mut ui = simulator(column![drop_spot(
                entry(),
                spot.clone(),
                drop,
                Heard::Hover
            )]);
            let at = ui.find("Home").unwrap().bounds().center();
            ui.point_at(at);
            ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: at,
            })]);
            assert_eq!(ui.into_messages().count(), 0);
        }
    }
}
