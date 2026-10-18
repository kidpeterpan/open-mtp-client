//! WPD constants the `windows` crate does not project.
//!
//! windows-rs generates from Windows metadata, which covers every WPD `PROPERTYKEY`/`GUID`/`CLSID`/
//! interface we need — but **not** C preprocessor macros. The root object id is the one such macro,
//! so we hand-define it here (proven in the Phase 0 spike).

/// The root object id for any WPD device.
///
/// In `PortableDevice.h` this is `#define WPD_DEVICE_OBJECT_ID L"DEVICE"`, a macro windows-rs does
/// not project. It's the parent id passed to `EnumObjects` to list a device's top-level objects
/// (its storages / functional objects).
pub(crate) const WPD_DEVICE_OBJECT_ID: &str = "DEVICE";
