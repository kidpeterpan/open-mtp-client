//! The connection checklist: what was found about why a phone won't connect,
//! as the checks to show. Shared by the device panel (after a failed connect)
//! and the Help dialog (on demand).

use iced::widget::{column, row, text};
use iced::{Element, Length};
use internal::dto;

/// One line of "what was checked": ticked when fine, crossed when it is why
/// the phone didn't connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResponse {
    pub ok: bool,
    pub text: String,
}

/// What was found out about a failed connection, ready to show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosisResponse {
    pub checks: Vec<CheckResponse>,
    /// Label of the button that quits the apps in the way; `None` when none is.
    pub quit_label: Option<String>,
}

/// "A", "A and B", "A, B and C".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn check(ok: bool, text: impl Into<String>) -> CheckResponse {
    CheckResponse {
        ok,
        text: text.into(),
    }
}

/// The checklist for someone whose phone is already connected. There is no
/// failure to explain, so what is found is a fact rather than a fault: the
/// phone is on USB because it is plugged in, and an app that can hold a phone
/// is not holding this one, because this app got in.
pub fn to_connected_response(diagnosis: &dto::Diagnosis, phone: &str) -> DiagnosisResponse {
    let mut checks = vec![check(true, format!("Connected: {phone}."))];
    if let Some(phones @ [_, ..]) = diagnosis.phones.as_deref() {
        checks.push(check(true, format!("Seen on USB: {}.", join_names(phones))));
    }
    DiagnosisResponse {
        checks,
        quit_label: None,
    }
}

/// The checklist shown under a failed connection, in the order a person would
/// try things: what is on the USB bus, then who else has the phone.
pub fn to_diagnosis_response(diagnosis: &dto::Diagnosis) -> DiagnosisResponse {
    let mut checks = Vec::new();

    match diagnosis.phones.as_deref() {
        Some([]) => checks.push(check(
            false,
            "No phone seen on USB. Try another cable (some only charge), another port, and no USB hub.",
        )),
        Some(phones) => {
            let (it, is) = if phones.len() == 1 {
                ("It is", "isn't")
            } else {
                ("They are", "aren't")
            };
            checks.push(check(true, format!("Seen on USB: {}.", join_names(phones))));
            checks.push(check(
                false,
                format!(
                    "{it} plugged in but {is} offering file transfer. On the phone, swipe down, tap the USB notification and choose “File transfer”."
                ),
            ));
        }
        None => {}
    }

    let mut quit_label = None;
    match diagnosis.holders.as_deref() {
        Some([]) if diagnosis.unknown_owners.is_empty() => {
            checks.push(check(true, "No other app is using your phone."));
        }
        Some([]) => {}
        Some(holders) => {
            let names: Vec<String> = holders.iter().map(|h| h.name.clone()).collect();
            let mut text = if diagnosis.owner_seen {
                // macOS's own word: it has the phone, not "may".
                let verb = if holders.len() == 1 { "has" } else { "have" };
                format!("macOS says {} {verb} your phone.", join_names(&names))
            } else {
                let verb = if holders.len() == 1 { "is" } else { "are" };
                format!("{} {verb} using your phone.", join_names(&names))
            };
            for holder in holders.iter().filter(|holder| holder.restarts) {
                text.push_str(&format!(
                    " {} starts again by itself, so if it comes back, quit it once more.",
                    holder.name
                ));
            }
            checks.push(check(false, text));
            quit_label = Some(match holders {
                [only] => format!("Quit {} and try again", only.name),
                _ => "Quit them and try again".to_owned(),
            });
        }
        None => {}
    }
    if let [_, ..] = diagnosis.unknown_owners.as_slice() {
        let quoted: Vec<String> = diagnosis
            .unknown_owners
            .iter()
            .map(|name| format!("“{name}”"))
            .collect();
        let (verb, it) = if quoted.len() == 1 {
            ("has", "it")
        } else {
            ("have", "them")
        };
        checks.push(check(
            false,
            format!(
                "macOS says {} {verb} your phone. Quit {it} (Activity Monitor finds it by that name), or restart your Mac if you don't know {it}, then try again.",
                join_names(&quoted)
            ),
        ));
    }

    DiagnosisResponse { checks, quit_label }
}

/// What was checked: ✓ where it was fine, ✗ where it is the problem.
pub fn checklist<'a, Message: 'a>(checks: &'a [CheckResponse]) -> Element<'a, Message> {
    column(checks.iter().map(|check| {
        let (mark, style): (_, fn(&iced::Theme) -> text::Style) = if check.ok {
            ("✓", text::success)
        } else {
            ("✗", text::danger)
        };
        row![
            text(mark).style(style).width(18),
            text(&check.text).size(14).width(Length::Fill)
        ]
        .spacing(6)
        .into()
    }))
    .spacing(8)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(name: &str, restarts: bool) -> dto::Holder {
        dto::Holder {
            name: name.to_owned(),
            restarts,
        }
    }

    fn diagnosis(holders: Option<Vec<dto::Holder>>, phones: Option<&[&str]>) -> dto::Diagnosis {
        dto::Diagnosis {
            holders,
            phones: phones.map(|names| names.iter().map(|n| (*n).to_owned()).collect()),
            ..Default::default()
        }
    }

    fn texts(response: &DiagnosisResponse) -> Vec<(bool, &str)> {
        response
            .checks
            .iter()
            .map(|c| (c.ok, c.text.as_str()))
            .collect()
    }

    #[test]
    fn names_are_joined_the_way_people_say_them() {
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(join_names(&[]), "");
        assert_eq!(join_names(&names(&["A"])), "A");
        assert_eq!(join_names(&names(&["A", "B"])), "A and B");
        assert_eq!(join_names(&names(&["A", "B", "C"])), "A, B and C");
    }

    #[test]
    fn nothing_checked_means_nothing_shown() {
        let response = to_diagnosis_response(&dto::Diagnosis::default());

        assert_eq!(response, DiagnosisResponse::default());
    }

    #[test]
    fn one_holder_is_named_and_can_be_quit() {
        let response = to_diagnosis_response(&diagnosis(Some(vec![holder("Photos", false)]), None));

        assert_eq!(texts(&response), [(false, "Photos is using your phone.")]);
        assert_eq!(
            response.quit_label.as_deref(),
            Some("Quit Photos and try again")
        );
    }

    #[test]
    fn the_owner_macos_records_is_said_as_fact() {
        let response = to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![holder("Garmin Express", false)]),
            owner_seen: true,
            ..Default::default()
        });

        assert_eq!(
            texts(&response),
            [(false, "macOS says Garmin Express has your phone.")]
        );
        assert_eq!(
            response.quit_label.as_deref(),
            Some("Quit Garmin Express and try again")
        );

        let two = to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![
                holder("macOS's camera service", true),
                holder("MacDroid", false),
            ]),
            owner_seen: true,
            ..Default::default()
        });
        assert!(two.checks[0].text.starts_with(
            "macOS says macOS's camera service and MacDroid have your phone. macOS's camera service starts again"
        ));
    }

    #[test]
    fn an_owner_the_app_does_not_know_gets_steps_and_no_quit_button() {
        let response = to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![]),
            owner_seen: true,
            unknown_owners: vec!["SyncMate".into()],
            ..Default::default()
        });

        assert_eq!(
            texts(&response),
            [(
                false,
                "macOS says “SyncMate” has your phone. Quit it (Activity Monitor finds it by that name), or restart your Mac if you don't know it, then try again."
            )],
            "never “no other app is using your phone”"
        );
        assert_eq!(response.quit_label, None);

        let mixed = to_diagnosis_response(&dto::Diagnosis {
            holders: Some(vec![holder("Photos", false)]),
            owner_seen: true,
            unknown_owners: vec!["SyncMate".into(), "Helper".into()],
            ..Default::default()
        });
        assert_eq!(mixed.checks.len(), 2);
        assert!(
            mixed.checks[1]
                .text
                .starts_with("macOS says “SyncMate” and “Helper” have your phone. Quit them")
        );
        assert_eq!(
            mixed.quit_label.as_deref(),
            Some("Quit Photos and try again"),
            "what it can quit, it offers to"
        );
    }

    #[test]
    fn several_holders_are_quit_together() {
        let response = to_diagnosis_response(&diagnosis(
            Some(vec![
                holder("Photos", false),
                holder("Preview", false),
                holder("Dropbox", false),
            ]),
            None,
        ));

        assert_eq!(
            texts(&response),
            [(false, "Photos, Preview and Dropbox are using your phone.")]
        );
        assert_eq!(
            response.quit_label.as_deref(),
            Some("Quit them and try again")
        );
    }

    #[test]
    fn a_holder_that_comes_back_says_so() {
        let response = to_diagnosis_response(&diagnosis(
            Some(vec![holder("macOS's camera service", true)]),
            None,
        ));

        assert_eq!(
            texts(&response),
            [(
                false,
                "macOS's camera service is using your phone. macOS's camera service starts again by itself, so if it comes back, quit it once more."
            )]
        );
    }

    #[test]
    fn no_holders_is_good_news_without_a_quit_button() {
        let response = to_diagnosis_response(&diagnosis(Some(vec![]), None));

        assert_eq!(
            texts(&response),
            [(true, "No other app is using your phone.")]
        );
        assert_eq!(response.quit_label, None);
    }

    #[test]
    fn a_phone_on_usb_that_is_not_found_is_not_offering_file_transfer() {
        let response = to_diagnosis_response(&diagnosis(None, Some(&["Pixel 7"])));

        assert_eq!(
            texts(&response),
            [
                (true, "Seen on USB: Pixel 7."),
                (
                    false,
                    "It is plugged in but isn't offering file transfer. On the phone, swipe down, tap the USB notification and choose “File transfer”."
                )
            ]
        );
    }

    #[test]
    fn several_phones_are_talked_about_in_the_plural() {
        let response = to_diagnosis_response(&diagnosis(None, Some(&["Pixel 7", "Galaxy S23"])));

        assert_eq!(
            response.checks[0].text,
            "Seen on USB: Pixel 7 and Galaxy S23."
        );
        assert!(
            response.checks[1]
                .text
                .starts_with("They are plugged in but aren't offering")
        );
    }

    #[test]
    fn no_phone_on_usb_points_at_the_cable() {
        let response = to_diagnosis_response(&diagnosis(None, Some(&[])));

        assert_eq!(
            texts(&response),
            [(
                false,
                "No phone seen on USB. Try another cable (some only charge), another port, and no USB hub."
            )]
        );
    }

    #[test]
    fn usb_is_listed_before_apps() {
        let response = to_diagnosis_response(&diagnosis(Some(vec![]), Some(&["Pixel 7"])));

        assert_eq!(response.checks.len(), 3);
        assert!(response.checks[0].text.starts_with("Seen on USB"));
        assert_eq!(response.checks[2].text, "No other app is using your phone.");
    }

    #[test]
    fn a_connected_phone_gets_no_faults_and_no_quit_button() {
        let response = to_connected_response(
            &diagnosis(
                Some(vec![holder("Photos", false), holder("Preview", false)]),
                Some(&["Pixel 7"]),
            ),
            "Pixel 9, serial …1234, 2 storages",
        );

        assert_eq!(
            texts(&response),
            [
                (true, "Connected: Pixel 9, serial …1234, 2 storages."),
                (true, "Seen on USB: Pixel 7."),
            ]
        );
        assert_eq!(response.quit_label, None);
    }

    #[test]
    fn a_connected_phone_with_nothing_else_found_is_just_connected() {
        let response = to_connected_response(&dto::Diagnosis::default(), "Pixel 9");

        assert_eq!(texts(&response), [(true, "Connected: Pixel 9.")]);
        let none_seen = to_connected_response(&diagnosis(Some(vec![]), Some(&[])), "Pixel 9");
        assert_eq!(texts(&none_seen), [(true, "Connected: Pixel 9.")]);
    }
}
