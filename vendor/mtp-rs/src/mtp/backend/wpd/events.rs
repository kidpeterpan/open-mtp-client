//! The WPD device-event callback: the one COM object WPD calls back *into*.
//!
//! Everything else in this backend calls COM *out*; this is the inverse. We implement
//! [`IPortableDeviceEventCallback`] via windows-rs `#[implement]` and hand WPD an interface pointer
//! through [`IPortableDevice::Advise`](windows::Win32::Devices::PortableDevices::IPortableDevice).
//! WPD then invokes [`OnEvent`](WpdEventSink_Impl::OnEvent) on *its own* RPC threads — never the
//! actor/worker thread — so the sink carries only `Send` state:
//!
//! - a [`futures::channel::mpsc::UnboundedSender`] the event flows out on (unbounded + non-blocking:
//!   `OnEvent` must never block a WPD RPC thread, and device events are rare), and
//! - the **shared** [`IdMap`] (`Arc<Mutex<…>>`, also owned by the worker) so the WPD object-id
//!   string an event carries is interned into the *same* reverse map the worker resolves handles
//!   from. Without this, a token handed out in a [`DeviceEvent`] couldn't later be turned back into
//!   a WPD id for a follow-up `object_info`/`download`.
//!
//! `OnEvent` returns `S_OK` unconditionally and never lets a panic cross the COM/FFI boundary
//! (`catch_unwind`), as a panic unwinding into WPD's RPC machinery is undefined behavior.

use super::ids::IdMap;
use super::props::take_pwstr;
use crate::mtp::{DeviceEvent, ObjectHandle};
use futures::channel::mpsc::UnboundedSender;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use windows::core::{implement, Ref, GUID};
use windows::Win32::Devices::PortableDevices::{
    IPortableDeviceEventCallback, IPortableDeviceEventCallback_Impl, IPortableDeviceValues,
    WPD_EVENT_DEVICE_RESET, WPD_EVENT_OBJECT_ADDED, WPD_EVENT_OBJECT_REMOVED,
    WPD_EVENT_OBJECT_UPDATED, WPD_EVENT_PARAMETER_EVENT_ID, WPD_OBJECT_ID,
};

/// The COM event sink WPD calls back into. Holds only `Send` state (see the module docs).
#[implement(IPortableDeviceEventCallback)]
pub(crate) struct WpdEventSink {
    /// Where mapped [`DeviceEvent`]s are pushed (drained by `WpdBackend::next_event`). Unbounded so
    /// `OnEvent` never blocks a WPD RPC thread.
    tx: UnboundedSender<DeviceEvent>,
    /// The same reverse map the worker resolves handles from, so an event's WPD object-id string is
    /// interned where a later `object_info(handle)` can find it. Locked only for the brief intern.
    ids: Arc<Mutex<IdMap>>,
}

impl WpdEventSink {
    pub(crate) fn new(tx: UnboundedSender<DeviceEvent>, ids: Arc<Mutex<IdMap>>) -> Self {
        Self { tx, ids }
    }

    /// Read the WPD event GUID from the parameter bag, map it, and forward the event (if any).
    fn dispatch(&self, params: &IPortableDeviceValues) {
        // SAFETY: `params` is the live event parameter bag WPD handed `OnEvent`.
        let Ok(guid) = (unsafe { params.GetGuidValue(&WPD_EVENT_PARAMETER_EVENT_ID) }) else {
            return; // no event id → nothing actionable
        };
        if let Some(event) = self.map_event(guid, params) {
            // Receiver dropped (backend closed) → nothing to do.
            let _ = self.tx.unbounded_send(event);
        }
    }

    /// Intern the event's `WPD_OBJECT_ID` into the shared map and return its handle token.
    fn object_handle(&self, params: &IPortableDeviceValues) -> Option<ObjectHandle> {
        // SAFETY: `params` is live; `GetStringValue` hands us a COM-allocated PWSTR to free.
        let id = unsafe {
            params
                .GetStringValue(&WPD_OBJECT_ID)
                .map(|p| take_pwstr(p))
                .ok()
        }?;
        if id.is_empty() {
            return None;
        }
        // Lock only for the intern; never held across COM or `.await` (this isn't async anyway).
        Some(self.ids.lock().expect("idmap poisoned").object(&id))
    }

    /// Map a WPD event GUID (+ its parameter bag) to a neutral [`DeviceEvent`].
    ///
    /// Object events resolve their affected handle from `WPD_OBJECT_ID`; if that's missing the event
    /// is dropped (returns `None`). Unmapped GUIDs become [`DeviceEvent::Unknown`] carrying the low
    /// 16 bits of the GUID's first field as a best-effort code (WPD events have no MTP `u16` code).
    fn map_event(&self, guid: GUID, params: &IPortableDeviceValues) -> Option<DeviceEvent> {
        if guid == WPD_EVENT_OBJECT_ADDED {
            Some(DeviceEvent::ObjectAdded {
                handle: self.object_handle(params)?,
            })
        } else if guid == WPD_EVENT_OBJECT_REMOVED {
            Some(DeviceEvent::ObjectRemoved {
                handle: self.object_handle(params)?,
            })
        } else if guid == WPD_EVENT_OBJECT_UPDATED {
            Some(DeviceEvent::ObjectInfoChanged {
                handle: self.object_handle(params)?,
            })
        } else if guid == WPD_EVENT_DEVICE_RESET {
            Some(DeviceEvent::DeviceReset)
        } else {
            Some(DeviceEvent::Unknown {
                #[allow(clippy::cast_possible_truncation)]
                code: (guid.data1 & 0xFFFF) as u16,
                params: [0, 0, 0],
            })
        }
    }
}

impl IPortableDeviceEventCallback_Impl for WpdEventSink_Impl {
    /// WPD's entry point, invoked on a WPD RPC thread. Always returns `S_OK`; never unwinds into COM.
    fn OnEvent(&self, peventparameters: Ref<IPortableDeviceValues>) -> windows::core::Result<()> {
        // A panic crossing the COM/FFI boundary is UB; swallow it and still report success.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if let Ok(params) = peventparameters.ok() {
                self.dispatch(params);
            }
        }));
        Ok(())
    }
}
