//! Display-ready view models: every string here is shown as-is.

use internal::dto;

#[derive(Debug, Clone)]
pub struct DeviceResponse {
    /// Which phone: the `dto::Phone::key` it was opened as.
    pub key: String,
    pub name: String,
    pub details: String,
    /// "Android 15", or the version a device that is not Android reports;
    /// empty when there is none.
    pub version: String,
    /// "Battery 76%"; `None` for a phone that does not say.
    pub battery: Option<String>,
    /// The USB link, as macOS negotiated it; `None` when not known.
    pub link: Option<dto::UsbSpeed>,
    /// Not shown; tells which phone an unplug was about.
    pub serial: String,
    /// Not shown here: what the phone says it can do, for Help and Get Info.
    pub abilities: dto::Abilities,
    /// It is Android, which keeps apps' own folders from computers.
    pub android: bool,
    pub source: dto::Device,
    pub battery_level: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct StorageResponse {
    pub id: u64,
    pub name: String,
    pub usage: String,
    /// 0.0 (empty) to 1.0 (full), for the usage bar.
    pub used_fraction: f32,
    pub read_only: bool,
    pub source: dto::Storage,
}

/// A phone plugged in, for the list in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneResponse {
    pub key: String,
    /// Its model, with the end of its serial number when another phone
    /// plugged in has the same name.
    pub name: String,
    pub source: dto::Phone,
}
