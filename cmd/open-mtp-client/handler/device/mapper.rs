use internal::dto;

use super::response::{DeviceResponse, PhoneResponse, StorageResponse};
use crate::handler::format::format_bytes;

pub fn to_device_response(device: &dto::Device) -> DeviceResponse {
    let details = [&device.manufacturer, &device.serial_number]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ");

    // Android reports its release ("15") as its version; anything else (a
    // camera's firmware, say) is shown as the version it is.
    let version = match (device.android, device.firmware_version.trim()) {
        (_, "") => String::new(),
        (true, release) => format!("Android {release}"),
        (false, other) => format!("Version {other}"),
    };
    DeviceResponse {
        key: device.key.clone(),
        name: device.model.clone(),
        version,
        battery: None,
        link: device.link,
        details,
        serial: device.serial_number.clone(),
        abilities: device.abilities,
        android: device.android,
    }
}

/// "Battery 76%", or `None` when the phone does not say.
pub fn battery_label(level: Option<u8>) -> Option<String> {
    level.map(|level| format!("Battery {level}%"))
}

/// The phones plugged in, named so that two can be told apart: two of the
/// same model get the last four characters of their serial number (no more,
/// as in a support report), or a number when they have none.
pub fn to_phone_responses(phones: &[dto::Phone]) -> Vec<PhoneResponse> {
    let base = |phone: &dto::Phone| {
        if phone.model.is_empty() {
            "Phone".to_owned()
        } else {
            phone.model.clone()
        }
    };
    phones
        .iter()
        .enumerate()
        .map(|(at, phone)| {
            let name = base(phone);
            let twins = phones.iter().filter(|other| base(other) == name).count();
            let name = if twins < 2 {
                name
            } else if phone.serial.is_empty() {
                format!("{name} ({})", at + 1)
            } else {
                let serial: Vec<char> = phone.serial.chars().collect();
                let tail: String = serial[serial.len().saturating_sub(4)..].iter().collect();
                format!("{name} (…{tail})")
            };
            PhoneResponse {
                key: phone.key.clone(),
                name,
            }
        })
        .collect()
}

pub fn to_storage_response(storage: &dto::Storage) -> StorageResponse {
    let used_fraction = if storage.capacity_bytes == 0 {
        0.0
    } else {
        storage.used_bytes() as f32 / storage.capacity_bytes as f32
    };

    StorageResponse {
        id: storage.id,
        name: storage.name.clone(),
        usage: format!(
            "{} free of {}",
            format_bytes(storage.free_bytes),
            format_bytes(storage.capacity_bytes)
        ),
        used_fraction,
        read_only: !storage.is_writable,
    }
}

/// The folders most phones have that people go to most, each with the
/// places it is found on different phones and Android versions, first first.
pub const PHONE_PLACES: &[(&str, &[&str])] = &[
    ("Camera", &["/DCIM/Camera"]),
    (
        "Screenshots",
        &["/DCIM/Screenshots", "/Pictures/Screenshots"],
    ),
    ("Downloads", &["/Download"]),
    (
        "WhatsApp media",
        &[
            "/Android/media/com.whatsapp/WhatsApp/Media",
            "/WhatsApp/Media",
        ],
    ),
    ("Music", &["/Music"]),
];

/// Every path worth asking about.
pub fn place_candidates() -> Vec<String> {
    PHONE_PLACES
        .iter()
        .flat_map(|(_, paths)| paths.iter().map(|path| (*path).to_owned()))
        .collect()
}

/// The places that are there, each at the first of its paths found.
pub fn to_places(present: &[String]) -> Vec<(String, String)> {
    PHONE_PLACES
        .iter()
        .filter_map(|(label, paths)| {
            paths
                .iter()
                .find(|path| present.iter().any(|found| found == *path))
                .map(|path| ((*label).to_owned(), (*path).to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_place_is_where_it_was_first_found_and_absent_ones_are_left_out() {
        let present: Vec<String> = ["/Pictures/Screenshots", "/DCIM/Camera", "/WhatsApp/Media"]
            .iter()
            .map(|path| (*path).to_owned())
            .collect();

        assert_eq!(
            to_places(&present),
            [
                ("Camera".to_owned(), "/DCIM/Camera".to_owned()),
                ("Screenshots".to_owned(), "/Pictures/Screenshots".to_owned()),
                ("WhatsApp media".to_owned(), "/WhatsApp/Media".to_owned()),
            ]
        );
        let both = vec![
            "/DCIM/Screenshots".to_owned(),
            "/Pictures/Screenshots".to_owned(),
        ];
        assert_eq!(
            to_places(&both)[0].1,
            "/DCIM/Screenshots",
            "the first of its places"
        );
        assert_eq!(place_candidates().len(), 7);
        assert!(to_places(&[]).is_empty());
    }

    #[test]
    fn storage_usage_reads_free_of_total() {
        let storage = dto::Storage {
            id: 1,
            name: "Internal".into(),
            capacity_bytes: 64_000_000_000,
            free_bytes: 16_000_000_000,
            is_writable: true,
        };

        let response = to_storage_response(&storage);

        assert_eq!(response.usage, "16.0 GB free of 64.0 GB");
        assert!((response.used_fraction - 0.75).abs() < f32::EPSILON);
        assert!(!response.read_only);
    }

    #[test]
    fn empty_storage_has_no_usage() {
        let storage = dto::Storage {
            id: 1,
            name: "SD card".into(),
            capacity_bytes: 0,
            free_bytes: 0,
            is_writable: false,
        };

        assert_eq!(to_storage_response(&storage).used_fraction, 0.0);
    }

    #[test]
    fn device_details_skip_missing_fields() {
        let device = dto::Device {
            abilities: dto::Abilities::default(),
            link: None,
            android: false,
            key: "k".into(),
            manufacturer: "Google".into(),
            model: "Pixel 9".into(),
            serial_number: String::new(),
            firmware_version: String::new(),
        };

        assert_eq!(to_device_response(&device).details, "Google");
    }

    fn plugged(model: &str, serial: &str) -> dto::Phone {
        dto::Phone {
            key: if serial.is_empty() {
                format!("usb@{model}")
            } else {
                serial.into()
            },
            manufacturer: "Google".into(),
            model: model.into(),
            serial: serial.into(),
        }
    }

    #[test]
    fn phones_are_told_apart_by_the_end_of_their_serial_when_named_alike() {
        let names = |phones: &[dto::Phone]| {
            to_phone_responses(phones)
                .into_iter()
                .map(|p| p.name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(&[
                plugged("Pixel 9", "AAA111"),
                plugged("Galaxy S24", "BBB222")
            ]),
            ["Pixel 9", "Galaxy S24"]
        );
        assert_eq!(
            names(&[plugged("Pixel 9", "AAA111"), plugged("Pixel 9", "BBB222")]),
            ["Pixel 9 (…A111)", "Pixel 9 (…B222)"]
        );
        assert_eq!(
            names(&[plugged("Pixel 9", ""), plugged("Pixel 9", "")]),
            ["Pixel 9 (1)", "Pixel 9 (2)"]
        );
        assert_eq!(names(&[plugged("", "AB")]), ["Phone"]);
        assert_eq!(
            to_phone_responses(&[plugged("Pixel 9", "AAA111")])[0].key,
            "AAA111"
        );
    }

    fn reporting(android: bool, version: &str) -> dto::Device {
        dto::Device {
            abilities: dto::Abilities::default(),
            link: None,
            key: "k".into(),
            manufacturer: "Google".into(),
            model: "Pixel 9".into(),
            serial_number: "ABC".into(),
            firmware_version: version.into(),
            android,
        }
    }

    #[test]
    fn android_says_its_release_and_anything_else_its_version() {
        let version = |android, version| to_device_response(&reporting(android, version)).version;

        assert_eq!(version(true, "15"), "Android 15");
        assert_eq!(version(true, " 14 "), "Android 14");
        assert_eq!(
            version(false, "1.10"),
            "Version 1.10",
            "a camera's firmware"
        );
        assert_eq!(version(true, ""), "");
        assert_eq!(version(false, "  "), "");
        assert_eq!(to_device_response(&reporting(true, "15")).battery, None);
    }

    #[test]
    fn the_battery_reads_as_a_percentage() {
        assert_eq!(battery_label(Some(76)).as_deref(), Some("Battery 76%"));
        assert_eq!(battery_label(Some(0)).as_deref(), Some("Battery 0%"));
        assert_eq!(battery_label(None), None);
    }
}
