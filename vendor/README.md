# vendor/mtp-rs

A copy of [mtp-rs](https://github.com/vdavid/mtp-rs) **0.33.0** (MIT OR
Apache-2.0, by David Veszelovszki), used through `[patch.crates-io]` in the
workspace `Cargo.toml`. It is not part of the workspace (`exclude = ["vendor"]`),
so `make lint`, `make test` and `make coverage` leave it alone.

## Why

mtp-rs 0.33 (the latest release) can read a device property only on its
low-level `PtpSession`, which an open `MtpDevice` keeps private, so the battery
level could not be read. `mtp-rs.patch` adds, and nothing else:

- `MtpDevice::battery_level()`: one `GetDevicePropValue(BatteryLevel)`, asked only
  when the device lists BatteryLevel (else `None` without a request); a value
  above 100 is `None`.
- `MtpDevice::is_android()`: the device's vendor extension names "android.com"
  (Android then reports its release, "15", as the device version).
- `MtpDevice::supports_operation(code)`: whether the device lists an operation
  code in its DeviceInfo (no request); `Capabilities` doesn't tell
  GetPartialObject64 from GetPartialObject, or say anything of
  GetObjectPropList.
- The virtual device: `VirtualDeviceConfig::battery_level` (advertised, and
  answered, only when `Some`) and `android`, and the hook
  `set_virtual_battery_level(serial, level)`, which also queues the
  `DevicePropChanged` event Android sends when its battery changes.

Hardening against a phone that sends bad data (a phone is untrusted input; each
has a test in the crate, run by `make test-vendor`):

- `ResponseContainer::from_bytes` refuses a length under the 12-byte header
  (it wrapped, and panicked in `with_capacity`, for a length of 0, 4 or 8).
- A buffered reply (`execute_with_receive`, the session-less GetDeviceInfo) is
  limited (`MAX_BUFFERED_BYTES` 256 MiB, 16 MiB for DeviceInfo): a data header
  can say 4 GB, and a device that never sends a response could grow the buffer
  for ever. The running total counts headers, so empty containers end too.
- `ReceiveStream::next_chunk` ends with an error after `MOST_EMPTY_READS` (8)
  empty reads in a row, where a "4 GB or more" header followed by nothing but
  empty packets kept it, and the session lock, for ever.
- `unpack_string` turns a lone UTF-16 surrogate into U+FFFD where it was an
  error that failed the listing of the whole folder.

Known and left (not reached by this app): `FileDownload::collect` sizes its
buffer by the phone's size, `collect_objects_recursive` has no visited set, and
the cancel and reset drains in `transport/nusb.rs` stop only on an idle timeout.

Every changed file starts with a "Modified by open-mtp-client" line. Besides
the patch, the `[[example]]` and `[[test]]` sections were removed from
`Cargo.toml` (`examples/` and `tests/` were not copied).

## Upgrading

1. Copy the new release's `src/`, `Cargo.toml`, `README.md` and both licence
   files over `vendor/mtp-rs/` (from `~/.cargo/registry/src/*/mtp-rs-<version>`).
2. Remove the `[[example]]` and `[[test]]` sections from its `Cargo.toml`.
3. `cd vendor/mtp-rs && patch -p1 < ../mtp-rs.patch`, and fix what no longer applies.
4. `make lint test coverage test-vendor`.

If a release adds a way to read the battery, drop this folder and the
`[patch.crates-io]` lines instead. `mtp-rs.patch` is written to be offered
upstream; it has not been sent.

`src/transport/virtual_device/CLAUDE.md` is the crate author's own notes on
their code, not instructions for this project.
