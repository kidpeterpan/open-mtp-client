use mtp_rs::OperationCode;
use mtp_rs::mtp::{
    Capabilities, DeviceEvent, DeviceInfo, HotplugEvent, MtpDeviceInfo, StorageInfo,
};

use crate::dto::{Abilities, ContentChange, Device, DeviceChange, Phone, Storage, UsbSpeed};

pub fn to_device(
    key: &str,
    info: &DeviceInfo,
    android: bool,
    abilities: Abilities,
    link: Option<UsbSpeed>,
) -> Device {
    Device {
        android,
        abilities,
        link,
        key: key.to_owned(),
        manufacturer: info.manufacturer.clone(),
        model: info.model.clone(),
        serial_number: info.serial_number.clone(),
        firmware_version: info.device_version.clone(),
    }
}

/// What the phone can do, from its capabilities and, for what they don't
/// tell apart, whether it lists an operation (`supports`).
pub fn to_abilities(capabilities: &Capabilities, supports: impl Fn(u16) -> bool) -> Abilities {
    Abilities {
        copy: capabilities.can_copy,
        moves: capabilities.can_move,
        rename: capabilities.can_rename,
        partial_reads: capabilities.supports_partial_download,
        partial_reads_64: supports(u16::from(OperationCode::GetPartialObject64)),
        thumbnails: capabilities.supports_thumbnails,
        prop_list: supports(u16::from(OperationCode::GetObjectPropList)),
        events: capabilities.supports_events,
    }
}

/// A USB link's speed as the app names it.
pub fn to_link(speed: Option<mtp_rs::UsbSpeed>) -> Option<UsbSpeed> {
    speed.map(|speed| match speed {
        mtp_rs::UsbSpeed::Low => UsbSpeed::Low,
        mtp_rs::UsbSpeed::Full => UsbSpeed::Full,
        mtp_rs::UsbSpeed::High => UsbSpeed::High,
        mtp_rs::UsbSpeed::Super => UsbSpeed::Super,
        mtp_rs::UsbSpeed::SuperPlus => UsbSpeed::SuperPlus,
    })
}

/// A phone as USB lists it. One without a serial number is told apart by
/// where it is plugged in.
pub fn to_phone(info: &MtpDeviceInfo) -> Phone {
    phone(
        info.serial_number.as_deref(),
        info.manufacturer.as_deref(),
        info.product.as_deref(),
        info.location_id,
    )
}

fn phone(
    serial: Option<&str>,
    manufacturer: Option<&str>,
    product: Option<&str>,
    location_id: u64,
) -> Phone {
    let serial = serial.unwrap_or_default().to_owned();
    Phone {
        key: phone_key(&serial, location_id),
        manufacturer: manufacturer.unwrap_or_default().to_owned(),
        model: product.unwrap_or_default().to_owned(),
        serial,
    }
}

pub fn phone_key(serial: &str, location_id: u64) -> String {
    if serial.is_empty() {
        format!("usb@{location_id:x}")
    } else {
        serial.to_owned()
    }
}

pub fn to_storage(info: &StorageInfo) -> Storage {
    Storage {
        id: info.id.0,
        name: info.description.clone(),
        capacity_bytes: info.total_capacity,
        free_bytes: info.free_space,
        is_writable: info.is_writable,
    }
}

pub fn to_device_change(event: HotplugEvent) -> DeviceChange {
    match event {
        HotplugEvent::Arrived(info) => change(true, info.serial_number),
        HotplugEvent::Left(info) => change(false, info.serial_number),
    }
}

fn change(arrived: bool, serial: Option<String>) -> DeviceChange {
    if arrived {
        DeviceChange::Plugged { serial }
    } else {
        DeviceChange::Unplugged { serial }
    }
}

/// What a device event means for what's on screen; `None` when nothing
/// shown has changed.
const DEVICE_PROP_CHANGED: u16 = 0x4006;
const BATTERY_LEVEL: u32 = 0x5001;

pub fn to_content_change(event: &DeviceEvent) -> Option<ContentChange> {
    match event {
        DeviceEvent::ObjectAdded { .. }
        | DeviceEvent::ObjectRemoved { .. }
        | DeviceEvent::ObjectInfoChanged { .. } => Some(ContentChange::Files),
        DeviceEvent::StoreAdded { .. }
        | DeviceEvent::StoreRemoved { .. }
        | DeviceEvent::StorageInfoChanged { .. }
        // After a reset nothing cached can be trusted; reread everything.
        | DeviceEvent::DeviceReset => Some(ContentChange::Storages),
        // mtp-rs passes DevicePropChanged through as unknown; Android sends it
        // for BatteryLevel when its battery level changes.
        DeviceEvent::Unknown {
            code: DEVICE_PROP_CHANGED,
            params: [BATTERY_LEVEL, ..],
        } => Some(ContentChange::Battery),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn abilities_come_from_the_capabilities_and_the_operations_listed() {
        let capabilities = mtp_rs::mtp::Capabilities {
            can_upload: true,
            can_delete: true,
            can_rename: false,
            can_move: true,
            can_copy: false,
            can_create_folder: true,
            supports_partial_download: true,
            supports_thumbnails: false,
            supports_events: true,
        };
        let lists_prop_list = |code| code == 0x9805;

        let abilities = to_abilities(&capabilities, lists_prop_list);

        assert_eq!(
            abilities,
            Abilities {
                copy: false,
                moves: true,
                rename: false,
                partial_reads: true,
                partial_reads_64: false,
                thumbnails: false,
                prop_list: true,
                events: true,
            }
        );
        assert!(to_abilities(&capabilities, |code| code == 0x95C1).partial_reads_64);
    }

    use mtp_rs::mtp::{ObjectHandle, StorageId};

    use super::*;

    #[test]
    fn plugging_and_unplugging_keep_the_serial() {
        assert_eq!(
            change(true, Some("ABC".into())),
            DeviceChange::Plugged {
                serial: Some("ABC".into())
            }
        );
        assert_eq!(
            change(false, None),
            DeviceChange::Unplugged { serial: None }
        );
    }

    #[test]
    fn object_events_are_file_changes() {
        let handle = ObjectHandle(3);
        assert_eq!(
            to_content_change(&DeviceEvent::ObjectAdded { handle }),
            Some(ContentChange::Files)
        );
        assert_eq!(
            to_content_change(&DeviceEvent::ObjectRemoved { handle }),
            Some(ContentChange::Files)
        );
        assert_eq!(
            to_content_change(&DeviceEvent::ObjectInfoChanged { handle }),
            Some(ContentChange::Files)
        );
    }

    #[test]
    fn a_battery_property_change_is_a_battery_change_and_other_properties_are_not() {
        let changed = |params: [u32; 3]| {
            to_content_change(&DeviceEvent::Unknown {
                code: 0x4006,
                params,
            })
        };

        assert_eq!(changed([0x5001, 0, 0]), Some(ContentChange::Battery));
        assert_eq!(changed([0x5003, 0, 0]), None, "another property");
        assert_eq!(
            to_content_change(&DeviceEvent::Unknown {
                code: 0x4007,
                params: [0x5001, 0, 0],
            }),
            None,
            "another event"
        );
    }

    #[test]
    fn storage_events_and_resets_reread_the_storages() {
        let storage_id = StorageId(1);
        assert_eq!(
            to_content_change(&DeviceEvent::StoreAdded { storage_id }),
            Some(ContentChange::Storages)
        );
        assert_eq!(
            to_content_change(&DeviceEvent::StoreRemoved { storage_id }),
            Some(ContentChange::Storages)
        );
        assert_eq!(
            to_content_change(&DeviceEvent::StorageInfoChanged { storage_id }),
            Some(ContentChange::Storages)
        );
        assert_eq!(
            to_content_change(&DeviceEvent::DeviceReset),
            Some(ContentChange::Storages)
        );
        assert_eq!(to_content_change(&DeviceEvent::DeviceInfoChanged), None);
    }

    #[test]
    fn a_usb_phone_is_known_by_its_serial_or_else_where_it_is_plugged_in() {
        let pixel = phone(
            Some("39021FDH"),
            Some("Google"),
            Some("Pixel 9"),
            0x0011_0000,
        );
        let nameless = phone(None, None, None, 0x0011_0000);

        assert_eq!(pixel.key, "39021FDH");
        assert_eq!(
            (pixel.manufacturer.as_str(), pixel.model.as_str()),
            ("Google", "Pixel 9")
        );
        assert_eq!(nameless.key, "usb@110000");
        assert_eq!(nameless.serial, "");
        assert_eq!(phone(Some(""), None, None, 0x2a).key, "usb@2a");
    }

    #[test]
    fn each_usb_speed_is_named_as_the_app_names_it() {
        assert_eq!(to_link(None), None);
        for (theirs, ours) in [
            (mtp_rs::UsbSpeed::Low, UsbSpeed::Low),
            (mtp_rs::UsbSpeed::Full, UsbSpeed::Full),
            (mtp_rs::UsbSpeed::High, UsbSpeed::High),
            (mtp_rs::UsbSpeed::Super, UsbSpeed::Super),
            (mtp_rs::UsbSpeed::SuperPlus, UsbSpeed::SuperPlus),
        ] {
            assert_eq!(to_link(Some(theirs)), Some(ours));
        }
        assert!(UsbSpeed::High.is_slow() && UsbSpeed::Full.is_slow());
        assert!(!UsbSpeed::Super.is_slow() && !UsbSpeed::SuperPlus.is_slow());
        assert_eq!(
            [
                UsbSpeed::Low,
                UsbSpeed::Full,
                UsbSpeed::High,
                UsbSpeed::Super,
                UsbSpeed::SuperPlus
            ]
            .map(UsbSpeed::megabits),
            [1, 12, 480, 5_000, 10_000]
        );
    }
}
