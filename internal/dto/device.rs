/// A phone being plugged in or unplugged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChange {
    Plugged { serial: Option<String> },
    Unplugged { serial: Option<String> },
}

/// Something changed on the connected phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentChange {
    /// Files or folders were added, removed or changed.
    Files,
    /// A storage came or went (e.g. an SD card), or its free space changed.
    Storages,
    /// The battery level changed.
    Battery,
}

/// A phone plugged in, as USB describes it; nothing has been asked of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    /// Which phone this is: its serial number, or where it is plugged in for
    /// one that reports none.
    pub key: String,
    pub manufacturer: String,
    pub model: String,
    /// Empty when the phone reports none.
    pub serial: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The `Phone::key` of the phone this session is with.
    pub key: String,
    pub manufacturer: String,
    pub model: String,
    pub serial_number: String,
    /// What the device reports as its version; an Android phone reports its
    /// Android release ("15").
    pub firmware_version: String,
    /// An Android phone or tablet (its MTP server says so).
    pub android: bool,
    /// The MTP operations the phone says it supports.
    pub abilities: Abilities,
    /// How fast its USB link is, as macOS negotiated it (the slowest of the
    /// port, the cable and the phone); `None` when not known (a virtual
    /// phone).
    pub link: Option<UsbSpeed>,
}

/// A USB link's speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsbSpeed {
    /// USB 1 low speed: 1.5 Mb/s.
    Low,
    /// USB 1 full speed: 12 Mb/s.
    Full,
    /// USB 2: 480 Mb/s.
    High,
    /// USB 3 (5 Gb/s).
    Super,
    /// USB 3 at 10 Gb/s.
    SuperPlus,
}

impl UsbSpeed {
    /// The link's rate in megabits a second.
    pub fn megabits(self) -> u32 {
        match self {
            UsbSpeed::Low => 1,
            UsbSpeed::Full => 12,
            UsbSpeed::High => 480,
            UsbSpeed::Super => 5_000,
            UsbSpeed::SuperPlus => 10_000,
        }
    }

    /// Slower than USB 3: a USB 2 cable or port in the way.
    pub fn is_slow(self) -> bool {
        self < UsbSpeed::Super
    }
}

/// What the phone says it can do, from the list of operations it gave when
/// it was opened (no request asks for it). A phone may claim more than it
/// does; this is what explains a paste going through the Mac or a slow
/// folder in a support report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Abilities {
    /// CopyObject: a copy made on the phone by itself.
    pub copy: bool,
    /// MoveObject: a move made on the phone by itself.
    pub moves: bool,
    pub rename: bool,
    /// GetPartialObject or GetPartialObject64: part of a file can be read,
    /// so a stopped download resumes.
    pub partial_reads: bool,
    /// GetPartialObject64: part of a file 4 GB or more can be read.
    pub partial_reads_64: bool,
    /// GetThumb: the phone gives its own thumbnails.
    pub thumbnails: bool,
    /// GetObjectPropList: a folder's details in one request (item 39).
    pub prop_list: bool,
    /// The phone sends events (files changed, battery).
    pub events: bool,
}
