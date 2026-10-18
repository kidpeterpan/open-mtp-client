use serde::{Deserialize, Serialize};

use crate::dto::{MOST_MIRRORS, MOST_PINS, MirrorDirection, Pin, PinPhone, PinSide, SavedMirror};

const VERSION: u32 = 1;

/// The pins as they are written down.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacesFile {
    version: u32,
    pins: Vec<PinFile>,
    mirrors: Vec<MirrorFile>,
}

impl Default for PlacesFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            pins: Vec::new(),
            mirrors: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct MirrorFile {
    serial: String,
    storage_id: u64,
    storage_name: String,
    phone_folder: String,
    mac_folder: String,
    /// "to_mac" or "to_phone".
    direction: String,
    remove_extras: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct PinFile {
    side: String,
    path: String,
    label: String,
    serial: Option<String>,
    storage_id: Option<u64>,
    storage_name: Option<String>,
}

pub fn to_file(pins: &[Pin], mirrors: &[SavedMirror]) -> PlacesFile {
    PlacesFile {
        version: VERSION,
        mirrors: mirrors
            .iter()
            .map(|mirror| MirrorFile {
                serial: mirror.phone.serial.clone(),
                storage_id: mirror.phone.storage_id,
                storage_name: mirror.phone.storage_name.clone(),
                phone_folder: mirror.phone_folder.clone(),
                mac_folder: mirror.mac_folder.clone(),
                direction: match mirror.direction {
                    MirrorDirection::ToMac => "to_mac",
                    MirrorDirection::ToPhone => "to_phone",
                }
                .to_owned(),
                remove_extras: mirror.remove_extras,
            })
            .collect(),
        pins: pins
            .iter()
            .map(|pin| PinFile {
                side: match pin.side {
                    PinSide::Mac => "mac",
                    PinSide::Phone => "phone",
                }
                .to_owned(),
                path: pin.path.clone(),
                label: pin.label.clone(),
                serial: pin.phone.as_ref().map(|p| p.serial.clone()),
                storage_id: pin.phone.as_ref().map(|p| p.storage_id),
                storage_name: pin.phone.as_ref().map(|p| p.storage_name.clone()),
            })
            .collect(),
    }
}

/// The pins read back. What a file holds is not trusted: a path that is not
/// absolute, a side not known, or a phone pin without its phone is left out,
/// and so is anything past `MOST_PINS`.
pub fn to_pins(file: &PlacesFile) -> Vec<Pin> {
    file.pins
        .iter()
        .filter(|pin| pin.path.starts_with('/'))
        .filter_map(|pin| {
            let side = match pin.side.as_str() {
                "mac" => PinSide::Mac,
                "phone" => PinSide::Phone,
                _ => return None,
            };
            let phone = match side {
                PinSide::Mac => None,
                PinSide::Phone => Some(PinPhone {
                    serial: pin.serial.clone().filter(|s| !s.is_empty())?,
                    storage_id: pin.storage_id?,
                    storage_name: pin.storage_name.clone().unwrap_or_default(),
                }),
            };
            Some(Pin {
                side,
                path: pin.path.clone(),
                label: pin.label.clone(),
                phone,
            })
        })
        .take(MOST_PINS)
        .collect()
}

/// The saved mirrors read back: both folders absolute, a phone by serial and
/// a direction known, or left out; at most `MOST_MIRRORS`.
pub fn to_mirrors(file: &PlacesFile) -> Vec<SavedMirror> {
    file.mirrors
        .iter()
        .filter(|mirror| {
            mirror.phone_folder.starts_with('/')
                && mirror.mac_folder.starts_with('/')
                && !mirror.serial.is_empty()
        })
        .filter_map(|mirror| {
            let direction = match mirror.direction.as_str() {
                "to_mac" => MirrorDirection::ToMac,
                "to_phone" => MirrorDirection::ToPhone,
                _ => return None,
            };
            Some(SavedMirror {
                phone: PinPhone {
                    serial: mirror.serial.clone(),
                    storage_id: mirror.storage_id,
                    storage_name: mirror.storage_name.clone(),
                },
                phone_folder: mirror.phone_folder.clone(),
                mac_folder: mirror.mac_folder.clone(),
                direction,
                remove_extras: mirror.remove_extras,
            })
        })
        .take(MOST_MIRRORS)
        .collect()
}
