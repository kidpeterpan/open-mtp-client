//! MTP/PTP data structures for device, storage, and object information.
//!
//! This module provides high-level structures for parsing protocol responses:
//! - [`DeviceInfo`]: Device capabilities and identification
//! - [`StorageInfo`]: Storage characteristics and capacity
//! - [`ObjectInfo`]: File/folder metadata
//! - [`DevicePropDesc`]: Device property descriptors
//! - [`PropertyValue`]: Property values of various types

mod device;
mod objects;
mod properties;
mod storage;

// Re-export all public types for backward compatibility
pub use device::{DeviceInfo, StorageInfo};
pub use objects::ObjectInfo;
pub use properties::{DevicePropDesc, PropertyFormType, PropertyRange, PropertyValue};
pub use storage::{
    AccessCapability, AssociationType, FilesystemType, ProtectionStatus, StorageType,
};
