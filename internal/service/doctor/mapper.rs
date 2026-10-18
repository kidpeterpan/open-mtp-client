//! USB descriptors → dto.

use super::known;
use super::service::UsbOwner;

/// The longest process name taken from the registry; anything longer is not
/// a name macOS wrote.
const NAME_MOST: usize = 64;

/// The exclusive owners in `ioreg -r -c IOUSBHostDevice -l -w0`'s text: each
/// `<class IOUSBHostDevice` line starts a device, and an owner recorded on it
/// or on one of its interfaces below is that device's. A device inside a
/// hub is printed twice (under the hub and as its own root): callers drop
/// the repeat.
pub fn owners_in_registry(text: &str) -> Vec<UsbOwner> {
    let mut found = Vec::new();
    let mut vendor: Option<u16> = None;
    let mut owners: Vec<(u32, String)> = Vec::new();
    let mut finish = |vendor: Option<u16>, owners: &mut Vec<(u32, String)>| {
        found.extend(owners.drain(..).map(|(pid, process)| UsbOwner {
            pid,
            process,
            vendor_id: vendor,
        }));
    };
    for line in text.lines() {
        let line = line.trim_start_matches([' ', '|']).trim();
        if line.starts_with("+-o ") {
            if line.contains("<class IOUSBHostDevice") {
                finish(vendor.take(), &mut owners);
            }
            continue;
        }
        let Some((key, value)) = property(line) else {
            continue;
        };
        match key {
            "idVendor" if vendor.is_none() => vendor = value.parse().ok(),
            "UsbExclusiveOwner" => owners.extend(owner_of(value)),
            _ => {}
        }
    }
    finish(vendor, &mut owners);
    found
}

/// A character that could make a shown name lie about itself: a control
/// character, a line or paragraph separator, a direction mark or override
/// (the handlers' `format::has_control` rule).
fn odd(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200E}' | '\u{200F}')
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// `"key" = value` → (key, value).
fn property(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('"')?;
    let (key, value) = rest.split_once("\" = ")?;
    Some((key, value.trim()))
}

/// `"pid 968, ptpcamerad"` → (968, "ptpcamerad"). The name is shown to the
/// user, so one with a control character, or too long, is not taken.
fn owner_of(value: &str) -> Option<(u32, String)> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let (pid, process) = inner.strip_prefix("pid ")?.split_once(", ")?;
    let process = process.trim();
    let fits =
        !process.is_empty() && process.chars().count() <= NAME_MOST && !process.chars().any(odd);
    let pid = pid.parse().ok()?;
    fits.then(|| (pid, process.to_owned()))
}

/// How a USB device is named when it looks like a phone: by the name it
/// reports ("Pixel 7"), else by its maker ("Samsung phone"). `None` when its
/// maker is not one that mostly makes phones.
pub fn phone_name(vendor_id: u16, product: Option<&str>) -> Option<String> {
    let maker = known::phone_vendor(vendor_id)?;
    let product = product.map(str::trim).filter(|name| !name.is_empty());
    Some(match product {
        Some(name) => name.to_owned(),
        None => format!("{maker} phone"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(pid: u32, process: &str, vendor_id: Option<u16>) -> UsbOwner {
        UsbOwner {
            pid,
            process: process.to_owned(),
            vendor_id,
        }
    }

    #[test]
    fn the_owner_is_read_from_the_line_ioreg_prints() {
        // A line as printed by `ioreg -r -c IOUSBHostDevice -n "Nintendo Switch" -l`.
        assert_eq!(
            owners_in_registry(r#""UsbExclusiveOwner" = "pid 968, ptpcamerad""#),
            [owner(968, "ptpcamerad", None)]
        );
    }

    #[test]
    fn each_owner_belongs_to_the_device_above_it_even_on_an_interface() {
        let text = r#"+-o Pixel 9@00100000  <class IOUSBHostDevice, id 0x100010f90, registered, matched, active, busy 0 (29 ms), retain 22>
    {
      "USB Product Name" = "Pixel 9"
      "UsbExclusiveOwner" = "pid 4021, Garmin Express"
      "idVendor" = 6353
      "locationID" = 1048576
    }
    
  +-o MTP@0  <class IOUSBHostInterface, id 0x100010f99, registered, matched, active, busy 0 (0 ms), retain 7>
      {
        "idVendor" = 1
        "UsbExclusiveOwner" = "pid 968, ptpcamerad"
      }
      
+-o Magic Keyboard@00200000  <class IOUSBHostDevice, id 0x100010fa0, registered, matched, active, busy 0 (1 ms), retain 20>
    {
      "idVendor" = 1452
    }
    
+-o Galaxy S24@00300000  <class IOUSBHostDevice, id 0x100010fb0, registered, matched, active, busy 0 (1 ms), retain 20>
  | {
  |   "UsbExclusiveOwner" = "pid 77, MacDroid"
  |   "idVendor" = 1256
  | }
"#;
        assert_eq!(
            owners_in_registry(text),
            [
                owner(4021, "Garmin Express", Some(6353)),
                owner(968, "ptpcamerad", Some(6353)),
                owner(77, "MacDroid", Some(1256)),
            ],
            "the interface's own idVendor doesn't move the device's"
        );
    }

    #[test]
    fn a_registry_without_owners_or_with_odd_ones_gives_none() {
        assert_eq!(owners_in_registry(""), []);
        for odd in [
            r#""UsbExclusiveOwner" = "968, ptpcamerad""#,
            r#""UsbExclusiveOwner" = "pid x, ptpcamerad""#,
            r#""UsbExclusiveOwner" = "pid 968, ""#,
            r#""UsbExclusiveOwner" = "pid 968 ptpcamerad""#,
            r#""UsbExclusiveOwner" = pid 968, ptpcamerad"#,
            "\"UsbExclusiveOwner\" = \"pid 968, evil\u{7}name\"",
            "\"UsbExclusiveOwner\" = \"pid 968, a\u{2028}b\"",
            "\"UsbExclusiveOwner\" = \"pid 968, \u{202e}evil\"",
            "\"UsbExclusiveOwner\" = \"pid 968, \u{2067}evil\"",
            r#""UsbExclusiveOwnerX" = "pid 968, ptpcamerad""#,
        ] {
            assert_eq!(owners_in_registry(odd), [], "{odd}");
        }
        let long = format!(r#""UsbExclusiveOwner" = "pid 1, {}""#, "a".repeat(65));
        assert_eq!(owners_in_registry(&long), []);
        let just = format!(r#""UsbExclusiveOwner" = "pid 1, {}""#, "a".repeat(64));
        assert_eq!(owners_in_registry(&just).len(), 1);
    }

    #[test]
    fn a_phone_is_named_by_what_it_reports() {
        assert_eq!(
            phone_name(0x18d1, Some("Pixel 7")).as_deref(),
            Some("Pixel 7")
        );
        assert_eq!(
            phone_name(0x04e8, Some("  SAMSUNG_Android ")).as_deref(),
            Some("SAMSUNG_Android")
        );
    }

    #[test]
    fn without_a_name_the_maker_stands_in() {
        assert_eq!(phone_name(0x04e8, None).as_deref(), Some("Samsung phone"));
        assert_eq!(
            phone_name(0x2717, Some("  ")).as_deref(),
            Some("Xiaomi phone")
        );
    }

    #[test]
    fn devices_from_other_makers_are_not_phones() {
        assert_eq!(phone_name(0x05ac, Some("Keyboard")), None);
    }
}
