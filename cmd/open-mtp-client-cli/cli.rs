//! The commands: what is asked (`parse`), and doing it (`run`) with the
//! app's own services, so a copy from the command line is checked, resumed
//! and quarantined exactly as one from the window. Output is plain lines, or
//! JSON with `--json`; the exit code says what went wrong (sysexits.h).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use internal::dto;
use internal::service::{app_folder, device, explorer, transfer};
use serde_json::json;

pub const USAGE: &str = "usage: open-mtp-client-cli [options] <command>

commands:
  phones                          the phones plugged in
  storages                        the open phone's storages
  info                            the open phone: model, version, battery,
                                  USB link and what it can do
  ls [<path>]                     what is in a phone folder (default /)
  pull <phone path>... <Mac folder>
                                  copy phone files or folders to the Mac
  push <Mac path>... <phone folder>
                                  copy Mac files or folders to the phone
  mkdir <phone path>              make a phone folder (-p: and the ones on
                                  the way; one already there is fine)
  rm <phone path>...              delete from the phone, for good (asks,
                                  or --yes)
  mv <phone path>... <phone folder>
                                  move on the phone (by way of the Mac
                                  when the phone can't)
  check <phone path>... <Mac folder>
                                  compare phone files, byte for byte, with
                                  the files of the same names in the Mac
                                  folder; nothing is changed (exit 1 when
                                  any differs or is on one side only)
  mirrors                         the open phone's saved mirrors, numbered
  mirror <number | label>         run a mirror saved in the app; one that
                                  removes extras asks first (or --yes)
  import <Mac folder>             copy the photos and videos under /DCIM
                                  that are new to the Mac, as the app's
                                  Import does: what it imported before is
                                  left out, from the window or from here

options:
  --virtual <folder>   a fake phone backed by <folder>, instead of USB
  --phone <key>        the phone to open (its serial), when several are in
  --storage <id>       the storage (default: the first)
  --hidden             ls also shows hidden items
  --skip | --replace | --replace-older | --keep-both
                       push: what to do with names already on the phone
                       (--replace-older: only with a newer file)
  --move               pull, push: remove each original once its copy is
                       checked (push puts them in the Trash)
  -p                   mkdir, push: make the phone folders on the way
  -y, --yes            rm, mirror: remove without asking
  --by day|month|year-month
                       import: a folder a day, a month, or a year with a
                       folder a month (default: straight into the folder)
  --remove             import: remove from the phone each photo whose
                       copy was checked
  --dry-run            import, mirror: say what would be done, and do
                       nothing
  --json               answer in JSON
  -h, --help           this text

Progress goes to stderr; ctrl-c stops a copy at the next safe point, what
arrived kept for next time (exit 130).";

/// Exit codes, as sysexits.h names them.
pub const OK: i32 = 0;
/// Some files failed; the rest went.
pub const PARTLY: i32 = 1;
pub const USAGE_ERROR: i32 = 64;
pub const NOT_FOUND: i32 = 66;
pub const NO_PHONE: i32 = 69;
pub const CANT_CREATE: i32 = 73;
pub const TRY_AGAIN: i32 = 75;
pub const FAILED: i32 = 70;
/// Stopped with ctrl-c (128 + SIGINT, as shells report it).
pub const CANCELLED: i32 = 130;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Phones,
    Storages,
    Info,
    Ls(String),
    Pull { from: Vec<String>, into: PathBuf },
    Push { from: Vec<PathBuf>, into: String },
    Mkdir(String),
    Rm(Vec<String>),
    Mv { from: Vec<String>, into: String },
    Import { into: PathBuf },
    Check { from: Vec<String>, into: PathBuf },
    Mirrors,
    Mirror(Choice),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Number(usize),
    Label(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    pub virtual_dir: Option<PathBuf>,
    pub phone: Option<String>,
    pub storage: Option<u64>,
    pub hidden: bool,
    pub json: bool,
    pub conflict: Option<dto::Conflict>,
    /// `-p`: phone folders on the way are made.
    pub parents: bool,
    pub yes: bool,
    /// `--move`: originals go once their copies are checked.
    pub moving: bool,
    pub by: Option<dto::ImportFolders>,
    pub remove: bool,
    pub dry_run: bool,
}

/// Asks a yes-or-no question; `None` when there is no one to ask.
pub type Ask = Box<dyn FnMut(&str) -> Option<bool> + Send>;

/// Moves a Mac item to the Trash.
pub type Trash = Arc<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

/// What the commands need from outside them: someone to ask, how stderr is
/// shown, the run's stop switch, and where moved Mac items go. The program
/// gives the real ones; tests give their own.
pub struct World {
    /// Asks a yes-or-no question; `None` when there is no one to ask (not
    /// at a terminal).
    pub ask: Ask,
    /// Stderr is a terminal: progress is a bar redrawn in place, not lines.
    pub terminal: bool,
    /// Flipped by ctrl-c: a copy stops at its next safe point.
    pub cancel: dto::Cancel,
    pub trash: Option<Trash>,
    pub home: PathBuf,
}

/// What `args` ask for, or what is wrong with them.
pub fn parse(args: &[String]) -> Result<(Options, Command), String> {
    let mut options = Options::default();
    let mut words: Vec<&str> = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .map(String::as_str)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--virtual" => options.virtual_dir = Some(PathBuf::from(value("--virtual")?)),
            "--phone" => options.phone = Some(value("--phone")?.to_owned()),
            "--storage" => {
                let id = value("--storage")?;
                options.storage = Some(id.parse().map_err(|_| format!("not a storage id: {id}"))?);
            }
            "--hidden" => options.hidden = true,
            "--json" => options.json = true,
            "--skip" => options.conflict = Some(dto::Conflict::Skip),
            "--replace" => options.conflict = Some(dto::Conflict::Replace),
            "--replace-older" => options.conflict = Some(dto::Conflict::ReplaceOlder),
            "--keep-both" => options.conflict = Some(dto::Conflict::KeepBoth),
            "--move" => options.moving = true,
            "-p" => options.parents = true,
            "-y" | "--yes" => options.yes = true,
            "--by" => options.by = Some(import_layout(value("--by")?)?),
            "--remove" => options.remove = true,
            "--dry-run" => options.dry_run = true,
            "-h" | "--help" => return Ok((options, Command::Help)),
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(format!("unknown option: {flag}"));
            }
            word => words.push(word),
        }
    }
    let command = match words.as_slice() {
        ["phones"] => Command::Phones,
        ["storages"] => Command::Storages,
        ["info"] => Command::Info,
        ["ls"] => Command::Ls("/".to_owned()),
        ["ls", path] => Command::Ls(phone_path(path)?),
        ["pull", from @ .., into] if !from.is_empty() => Command::Pull {
            from: phone_paths(from)?,
            into: PathBuf::from(into),
        },
        ["push", from @ .., into] if !from.is_empty() => Command::Push {
            from: from.iter().map(PathBuf::from).collect(),
            into: phone_path(into)?,
        },
        ["mkdir", path] => Command::Mkdir(phone_path(path)?),
        ["rm", paths @ ..] if !paths.is_empty() => Command::Rm(phone_paths(paths)?),
        ["mv", from @ .., into] if !from.is_empty() => Command::Mv {
            from: phone_paths(from)?,
            into: phone_path(into)?,
        },
        ["import", into] => Command::Import {
            into: PathBuf::from(into),
        },
        ["check", from @ .., into] if !from.is_empty() => Command::Check {
            from: phone_paths(from)?,
            into: PathBuf::from(into),
        },
        ["mirrors"] => Command::Mirrors,
        ["mirror", which] => Command::Mirror(mirror_choice(which)?),
        [] => return Err("no command".to_owned()),
        [word, ..] => {
            return Err(format!(
                "not a command, or the wrong arguments for it: {word}"
            ));
        }
    };
    options_fit(&options, &command)?;
    Ok((options, command))
}

fn import_layout(word: &str) -> Result<dto::ImportFolders, String> {
    match word {
        "day" => Ok(dto::ImportFolders::Day),
        "month" => Ok(dto::ImportFolders::Month),
        "year-month" => Ok(dto::ImportFolders::YearMonth),
        other => Err(format!("not a layout: {other} (day, month or year-month)")),
    }
}

fn mirror_choice(word: &str) -> Result<Choice, String> {
    let Ok(number) = word.parse::<usize>() else {
        return Ok(Choice::Label(word.to_owned()));
    };
    if number == 0 {
        return Err("mirror numbers start at 1 (see: mirrors)".to_owned());
    }
    Ok(Choice::Number(number))
}

fn options_fit(options: &Options, command: &Command) -> Result<(), String> {
    let importing = matches!(command, Command::Import { .. });
    let mirroring = matches!(command, Command::Mirror(_));
    if importing && options.moving {
        return Err("import never moves: to free the phone, use --remove".to_owned());
    }
    if !importing && (options.by.is_some() || options.remove) {
        return Err("--by and --remove are only for import".to_owned());
    }
    if !importing && !mirroring && options.dry_run {
        return Err("--dry-run is only for import and mirror".to_owned());
    }
    Ok(())
}

fn phone_paths(paths: &[&str]) -> Result<Vec<String>, String> {
    paths.iter().map(|path| phone_path(path)).collect()
}

/// A phone path as the services take it: from the top, no trailing slash.
fn phone_path(path: &str) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err(format!("a phone path starts at the top (/): {path}"));
    }
    let trimmed = path.trim_end_matches('/');
    Ok(if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    })
}

/// The exit code for an error.
pub fn exit_code(error: &dto::Error) -> i32 {
    match error {
        dto::Error::NoDevice
        | dto::Error::NotConnected
        | dto::Error::Disconnected
        | dto::Error::MultipleDevices
        | dto::Error::PhoneUnplugged
        | dto::Error::DeviceInUse
        | dto::Error::AndroidFileTransferRunning
        | dto::Error::AllowStorageAccess => NO_PHONE,
        dto::Error::PathNotFound(_) => NOT_FOUND,
        dto::Error::Timeout => TRY_AGAIN,
        dto::Error::Cancelled => CANCELLED,
        dto::Error::NameTaken(_)
        | dto::Error::PermissionDenied
        | dto::Error::FolderAccessDenied(_)
        | dto::Error::MacDiskFull
        | dto::Error::NotEnoughRoom { .. }
        | dto::Error::VolumeReadOnly(_)
        | dto::Error::TooBigForVolume(_) => CANT_CREATE,
        _ => FAILED,
    }
}

/// Does what `args` ask, writing answers to `out` and problems to `err`, and
/// returns the exit code.
pub async fn run(
    args: &[String],
    out: &mut dyn Write,
    err: &mut (dyn Write + Send),
    world: &mut World,
) -> i32 {
    let (options, command) = match parse(args) {
        Ok(asked) => asked,
        Err(problem) => {
            let _ = writeln!(err, "{problem}\n\n{USAGE}");
            return USAGE_ERROR;
        }
    };
    if command == Command::Help {
        let _ = writeln!(out, "{USAGE}");
        return OK;
    }
    let err = Mutex::new(err);
    match carry_out(&options, &command, out, &err, world).await {
        Ok(code) => code,
        Err(error) => {
            let err = &mut *err.lock().unwrap();
            if options.json {
                let _ = writeln!(out, "{}", json!({ "error": error.to_string() }));
            }
            let _ = writeln!(err, "open-mtp-client-cli: {error}");
            if matches!(command, Command::Push { .. }) && matches!(error, dto::Error::NameTaken(_))
            {
                let _ = writeln!(
                    err,
                    "choose --skip, --replace, --replace-older or --keep-both"
                );
            }
            exit_code(&error)
        }
    }
}

/// Where phones come from: USB, or a fake phone in a folder.
fn source(options: &Options) -> device::Source {
    match &options.virtual_dir {
        Some(dir) => device::Source::Virtual(device::VirtualPhone::new(
            dir.clone(),
            fake_phone_serial(dir),
        )),
        None => device::Source::Usb,
    }
}

fn fake_phone_serial(folder: &Path) -> String {
    let whole_path = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    format!(
        "cli-{:016x}",
        fnv1a_64(whole_path.as_os_str().as_encoded_bytes())
    )
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

type Stderr<'a> = Mutex<&'a mut (dyn Write + Send)>;

async fn carry_out(
    options: &Options,
    command: &Command,
    out: &mut dyn Write,
    err: &Stderr<'_>,
    world: &mut World,
) -> Result<i32, dto::Error> {
    let device = Arc::new(device::Service::new(source(options)));
    if *command == Command::Phones {
        let phones = device.phones().await?;
        if options.json {
            let list: Vec<_> = phones
                .iter()
                .map(|phone| {
                    json!({
                        "key": phone.key,
                        "manufacturer": phone.manufacturer,
                        "model": phone.model,
                        "serial": phone.serial,
                    })
                })
                .collect();
            let _ = writeln!(out, "{}", json!(list));
        } else {
            for phone in &phones {
                let _ = writeln!(out, "{}\t{} {}", phone.key, phone.manufacturer, phone.model);
            }
        }
        return Ok(OK);
    }

    let phone = device.connect_to(options.phone.as_deref()).await?;
    if *command == Command::Info {
        let battery = device.battery().await?;
        info(&phone, battery, options, out);
        return Ok(OK);
    }
    let storages = device.storages().await?;
    if *command == Command::Storages {
        if options.json {
            let list: Vec<_> = storages
                .iter()
                .map(|storage| {
                    json!({
                        "id": storage.id,
                        "name": storage.name,
                        "capacity": storage.capacity_bytes,
                        "free": storage.free_bytes,
                    })
                })
                .collect();
            let _ = writeln!(out, "{}", json!(list));
        } else {
            for storage in &storages {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{} free of {}",
                    storage.id,
                    storage.name,
                    bytes(storage.free_bytes),
                    bytes(storage.capacity_bytes)
                );
            }
        }
        return Ok(OK);
    }
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let import_record = transfer::ImportRecord::at(app_folder(&world.home).join("imported"));
    let mut transfers =
        transfer::Service::new(device, Arc::clone(&explorer)).with_imports(import_record);
    if let Some(trash) = world.trash.clone() {
        transfers = transfers.with_trash(move |path: &Path| trash(path));
    }
    let printer = Progress::new(err, world.terminal);
    let progress = |report: dto::TransferProgress| printer.show(&report);
    let cancel = world.cancel.clone();
    let storage_names: Vec<(u64, String)> = storages
        .iter()
        .map(|storage| (storage.id, storage.name.clone()))
        .collect();
    let mirrors_here = mirrors_of_this_phone(&world.home, &phone.serial_number, &storage_names);
    match command {
        Command::Mirrors => {
            let phone_has_serial = !phone.serial_number.is_empty();
            return Ok(mirrors_report(
                &mirrors_here,
                phone_has_serial,
                options,
                out,
            ));
        }
        Command::Mirror(choice) => {
            let call = MirrorCall {
                choice,
                mirrors_here: &mirrors_here,
                world,
                err,
            };
            let code = mirror(&transfers, call, options, &progress, out).await;
            printer.done();
            return code;
        }
        _ => {}
    }
    let storage = match options.storage {
        Some(id) if storages.iter().any(|storage| storage.id == id) => id,
        Some(id) => return Err(dto::Error::PathNotFound(format!("storage {id}"))),
        None => storages.first().ok_or(dto::Error::AllowStorageAccess)?.id,
    };
    let code = match command {
        Command::Ls(path) => list(&explorer, storage, path, options, out).await,
        Command::Pull { from, into } => {
            let run = Run {
                storage,
                options,
                cancel: &cancel,
                progress: &progress,
            };
            pull(&transfers, &run, from, into, out).await
        }
        Command::Push { from, into } => {
            if options.parents {
                make_folders(&explorer, storage, into, true).await?;
            }
            let run = Run {
                storage,
                options,
                cancel: &cancel,
                progress: &progress,
            };
            push(&transfers, &run, from, into, out).await
        }
        Command::Mkdir(path) => {
            make_folders(&explorer, storage, path, options.parents).await?;
            if options.json {
                let _ = writeln!(out, "{}", json!({ "made": path }));
            } else {
                let _ = writeln!(out, "Made {path}");
            }
            Ok(OK)
        }
        Command::Rm(paths) => remove(&explorer, storage, paths, options, world, out).await,
        Command::Mv { from, into } => {
            let run = Run {
                storage,
                options,
                cancel: &cancel,
                progress: &progress,
            };
            move_on_phone(&explorer, &transfers, &run, from, into, out).await
        }
        Command::Import { into } => {
            let run = Run {
                storage,
                options,
                cancel: &cancel,
                progress: &progress,
            };
            let recorded = !phone.serial_number.is_empty();
            import(&transfers, &run, into, recorded, out).await
        }
        Command::Check { from, into } => {
            let run = Run {
                storage,
                options,
                cancel: &cancel,
                progress: &progress,
            };
            check(&transfers, &run, from, into, out).await
        }
        Command::Phones
        | Command::Storages
        | Command::Info
        | Command::Help
        | Command::Mirrors
        | Command::Mirror(_) => Ok(OK),
    };
    printer.done();
    code
}

/// What every copying command runs with.
struct Run<'a> {
    storage: u64,
    options: &'a Options,
    cancel: &'a dto::Cancel,
    progress: &'a (dyn Fn(dto::TransferProgress) + Send + Sync),
}

/// Copy progress on stderr: at a terminal a bar redrawn in place, else a
/// line each time the whole percent moves (so a script's log shows a copy
/// that stalled).
struct Progress<'a, 'w> {
    err: &'a Stderr<'w>,
    terminal: bool,
    /// The percent last shown, and whether every file was done then.
    shown: Mutex<Option<(u8, bool)>>,
}

impl<'a, 'w> Progress<'a, 'w> {
    fn new(err: &'a Stderr<'w>, terminal: bool) -> Self {
        Self {
            err,
            terminal,
            shown: Mutex::new(None),
        }
    }

    fn show(&self, report: &dto::TransferProgress) {
        let percent = progress_percent(report);
        // Also when the last file is done: its bytes reached 100% a moment
        // before it was counted.
        let all_done = report.files_total > 0 && report.files_done == report.files_total;
        let mut shown = self.shown.lock().unwrap();
        if *shown == Some((percent, all_done)) {
            return;
        }
        *shown = Some((percent, all_done));
        let line = progress_line(report, percent, self.terminal);
        let mut err = self.err.lock().unwrap();
        let _ = if self.terminal {
            write!(err, "\r{line}")
        } else {
            writeln!(err, "{line}")
        };
        let _ = err.flush();
    }

    /// Ends a bar drawn in place, so what follows starts on its own line.
    fn done(&self) {
        if self.terminal && self.shown.lock().unwrap().is_some() {
            let _ = writeln!(self.err.lock().unwrap());
        }
    }
}

/// How far a copy is, in whole percent of its bytes (of its files when it
/// has no bytes to count).
fn progress_percent(report: &dto::TransferProgress) -> u8 {
    let (done, total) = if report.bytes_total > 0 {
        (report.bytes_done, report.bytes_total)
    } else {
        (report.files_done as u64, report.files_total as u64)
    };
    if total == 0 {
        return 0;
    }
    (done.min(total).saturating_mul(100) / total) as u8
}

/// "37%  12 of 40 files"; at a terminal with a bar in front.
fn progress_line(report: &dto::TransferProgress, percent: u8, terminal: bool) -> String {
    let counts = format!(
        "{percent:>3}%  {} of {}",
        report.files_done,
        files(report.files_total)
    );
    if !terminal {
        return counts;
    }
    const WIDTH: usize = 24;
    let filled = usize::from(percent) * WIDTH / 100;
    format!(
        "[{}{}] {counts}",
        "#".repeat(filled),
        " ".repeat(WIDTH - filled)
    )
}

/// The open phone, as `info` says it.
fn info(phone: &dto::Device, battery: Option<u8>, options: &Options, out: &mut dyn Write) {
    let link = phone.link.map(link_label);
    let abilities = &phone.abilities;
    let can: Vec<(&str, bool)> = vec![
        ("copy on the phone", abilities.copy),
        ("move on the phone", abilities.moves),
        ("rename", abilities.rename),
        ("resume a stopped download", abilities.partial_reads),
        ("resume past 4 GB", abilities.partial_reads_64),
        ("its own thumbnails", abilities.thumbnails),
        ("a folder's details in one request", abilities.prop_list),
        ("say when files change", abilities.events),
    ];
    if options.json {
        let can_json: serde_json::Map<String, serde_json::Value> = can
            .iter()
            .map(|(what, yes)| ((*what).to_owned(), json!(yes)))
            .collect();
        let _ = writeln!(
            out,
            "{}",
            json!({
                "key": phone.key,
                "manufacturer": phone.manufacturer,
                "model": phone.model,
                "serial": phone.serial_number,
                "version": phone.firmware_version,
                "android": phone.android,
                "battery": battery,
                "usb": link,
                "can": can_json,
            })
        );
        return;
    }
    let version = if phone.android {
        format!("Android {}", phone.firmware_version)
    } else {
        format!("version {}", phone.firmware_version)
    };
    let _ = writeln!(out, "{} {}", phone.manufacturer, phone.model);
    let mut line = version;
    if let Some(level) = battery {
        line.push_str(&format!(" · battery {level}%"));
    }
    if let Some(link) = link {
        line.push_str(&format!(" · {link}"));
    }
    let _ = writeln!(out, "{line}");
    let _ = writeln!(out, "serial {}", phone.serial_number);
    let list = |yes: bool| {
        can.iter()
            .filter(|(_, can)| *can == yes)
            .map(|(what, _)| *what)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(out, "can: {}", list(true));
    let cannot = list(false);
    if !cannot.is_empty() {
        let _ = writeln!(out, "can't: {cannot}");
    }
}

fn link_label(link: dto::UsbSpeed) -> String {
    match link {
        dto::UsbSpeed::Low => "USB 1 (low speed)".to_owned(),
        dto::UsbSpeed::Full => "USB 1".to_owned(),
        dto::UsbSpeed::High => "USB 2".to_owned(),
        dto::UsbSpeed::Super => "USB 3".to_owned(),
        dto::UsbSpeed::SuperPlus => "USB 3.1 or faster".to_owned(),
    }
}

/// Makes the phone folder `path`; with `parents`, the folders on the way
/// too, and one already there is fine (`mkdir -p`). One listing per level,
/// from the folder memory where it can.
async fn make_folders(
    explorer: &explorer::Service,
    storage: u64,
    path: &str,
    parents: bool,
) -> Result<(), dto::Error> {
    let Some((parent, name)) = split(path) else {
        // The top is always there.
        return Ok(());
    };
    if !parents {
        explorer.make_folder(storage, &parent, &name).await?;
        return Ok(());
    }
    let mut here = "/".to_owned();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        let entries = explorer.list(storage, &here, true).await?;
        let next = child_path(&here, part);
        match entries.iter().find(|entry| entry.name == part) {
            Some(entry) if entry.is_folder => {}
            Some(_) => return Err(dto::Error::NameTaken(part.to_owned())),
            None => {
                explorer.make_folder(storage, &here, part).await?;
            }
        }
        here = next;
    }
    Ok(())
}

fn child_path(folder: &str, name: &str) -> String {
    if folder == "/" {
        format!("/{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// `paths` gathered by the folder they are in, keeping their order: one
/// request per folder.
fn by_folder(paths: &[String]) -> Result<Vec<(String, Vec<String>)>, dto::Error> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for path in paths {
        let Some((folder, name)) = split(path) else {
            return Err(dto::Error::General(
                "a file or a folder, not the whole storage".to_owned(),
            ));
        };
        match groups.iter_mut().find(|(group, _)| *group == folder) {
            Some((_, names)) => names.push(name),
            None => groups.push((folder, vec![name])),
        }
    }
    Ok(groups)
}

/// Deletes from the phone, for good: asks first unless `--yes`, and with no
/// one to ask (a script) refuses rather than guess.
async fn remove(
    explorer: &explorer::Service,
    storage: u64,
    paths: &[String],
    options: &Options,
    world: &mut World,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let groups = by_folder(paths)?;
    if !options.yes {
        let what = match paths {
            [one] => one.clone(),
            many => format!("{} items", many.len()),
        };
        match (world.ask)(&format!("Delete {what} from the phone for good? [y/N] ")) {
            Some(true) => {}
            Some(false) => {
                let _ = writeln!(out, "Nothing was deleted.");
                return Ok(OK);
            }
            None => {
                return Err(dto::Error::General(
                    "rm deletes for good: add --yes to do it without asking".to_owned(),
                ));
            }
        }
    }
    for (folder, names) in &groups {
        explorer.delete(storage, folder, names).await?;
    }
    if options.json {
        let _ = writeln!(out, "{}", json!({ "deleted": paths }));
    } else {
        let _ = writeln!(out, "Deleted {}", items(paths.len()));
    }
    Ok(OK)
}

fn items(count: usize) -> String {
    match count {
        1 => "1 item".to_owned(),
        n => format!("{n} items"),
    }
}

/// Moves on the phone, by the phone itself where it can, else by way of the
/// Mac (one file at a time, each original removed once its copy is checked).
async fn move_on_phone(
    explorer: &explorer::Service,
    transfers: &transfer::Service,
    run: &Run<'_>,
    from: &[String],
    into: &str,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let storage = run.storage;
    let (mut moved, mut failures) = (0, Vec::new());
    for (folder, names) in by_folder(from)? {
        let request = dto::PasteRequest {
            mode: dto::Paste::Move,
            from_storage: storage,
            from: folder,
            names,
            to_storage: storage,
            to: into.to_owned(),
        };
        match explorer.paste(&request).await {
            Ok(summary) => {
                moved += summary.done;
                failures.extend(summary.failures);
            }
            Err(dto::Error::Unsupported) => {
                let summary = transfers
                    .paste_through_mac(&request, false, run.cancel, run.progress)
                    .await?;
                moved += request.names.len() - summary.removal.map_or(0, |r| r.left.len());
                failures.extend(summary.failures);
            }
            Err(error) => return Err(error),
        }
    }
    let lines: Vec<String> = failures
        .iter()
        .map(|failure| format!("{}: {}", failure.label, failure.error))
        .collect();
    if run.options.json {
        let _ = writeln!(
            out,
            "{}",
            json!({ "moved": moved, "into": into, "failures": lines })
        );
    } else {
        let _ = writeln!(out, "Moved {} to {into}", items(moved));
        for line in &lines {
            let _ = writeln!(out, "failed: {line}");
        }
    }
    Ok(if lines.is_empty() { OK } else { PARTLY })
}

async fn list(
    explorer: &explorer::Service,
    storage: u64,
    path: &str,
    options: &Options,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let mut entries = explorer.list(storage, path, options.hidden).await?;
    explorer::sort(&mut entries, dto::Sort::default());
    if options.json {
        let list: Vec<_> = entries
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "folder": entry.is_folder,
                    "size": (!entry.is_folder).then_some(entry.size_bytes),
                    "modified": entry.modified.map(|when| date(&when)),
                })
            })
            .collect();
        let _ = writeln!(out, "{}", json!(list));
    } else {
        for entry in &entries {
            let size = if entry.is_folder {
                "-".to_owned()
            } else {
                bytes(entry.size_bytes)
            };
            let when = entry
                .modified
                .map_or_else(|| "-".to_owned(), |when| date(&when));
            let slash = if entry.is_folder { "/" } else { "" };
            let _ = writeln!(out, "{size}\t{when}\t{}{slash}", entry.name);
        }
    }
    Ok(OK)
}

/// Copies phone items to a Mac folder (one download per phone folder they
/// are in), or with `--move` removes each original once its copy is checked.
async fn pull(
    transfers: &transfer::Service,
    run: &Run<'_>,
    from: &[String],
    into: &Path,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let groups = by_folder(from)?;
    let destination = std::path::absolute(into)
        .map_err(|error| dto::Error::General(error.to_string()))?
        .to_string_lossy()
        .into_owned();
    let mode = if run.options.moving {
        dto::Paste::Move
    } else {
        dto::Paste::Copy
    };
    let mut total = dto::TransferSummary {
        destination: destination.clone(),
        ..dto::TransferSummary::default()
    };
    for (folder, names) in groups {
        let request = dto::DownloadRequest {
            storage_id: run.storage,
            source: folder,
            names,
            destination: destination.clone(),
        };
        let summary = transfers
            .download_as(mode, &request, run.cancel, run.progress)
            .await?;
        add_up(&mut total, summary);
    }
    let verb = if run.options.moving {
        "Moved"
    } else {
        "Copied"
    };
    Ok(report(&total, verb, run.options, out))
}

/// Adds one transfer's counts to the whole command's.
fn add_up(total: &mut dto::TransferSummary, summary: dto::TransferSummary) {
    total.files += summary.files;
    total.bytes += summary.bytes;
    total.already_there += summary.already_there;
    total.skipped += summary.skipped;
    total.replaced += summary.replaced;
    total.renamed += summary.renamed;
    total.failures.extend(summary.failures);
    if let Some(removal) = summary.removal {
        let kept = total.removal.get_or_insert_with(dto::Removal::default);
        kept.removed += removal.removed;
        kept.kept.extend(removal.kept);
        kept.left.extend(removal.left);
    }
}

async fn push(
    transfers: &transfer::Service,
    run: &Run<'_>,
    from: &[PathBuf],
    into: &str,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let (storage, options) = (run.storage, run.options);
    let sources = from
        .iter()
        .map(|path| std::path::absolute(path).map(|path| path.to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| dto::Error::General(error.to_string()))?;
    let request = dto::UploadRequest {
        sources,
        storage_id: storage,
        destination: into.to_owned(),
    };
    // Asked first, as the app asks: nothing is replaced unless told to.
    let taken = transfers.existing(&request).await?;
    let conflict = match (options.conflict, taken.first()) {
        (Some(conflict), _) => conflict,
        (None, None) => dto::Conflict::KeepBoth,
        (None, Some(name)) => return Err(dto::Error::NameTaken(name.clone())),
    };
    let mode = if options.moving {
        dto::Paste::Move
    } else {
        dto::Paste::Copy
    };
    let summary = transfers
        .upload_as(mode, &request, conflict, run.cancel, run.progress)
        .await?;
    let verb = if options.moving { "Moved" } else { "Sent" };
    Ok(report(&summary, verb, options, out))
}

/// The folder an item is in and its name; `None` for the top.
fn split(path: &str) -> Option<(String, String)> {
    let (folder, name) = path.rsplit_once('/')?;
    if name.is_empty() {
        return None;
    }
    Some((
        if folder.is_empty() {
            "/".to_owned()
        } else {
            folder.to_owned()
        },
        name.to_owned(),
    ))
}

/// What a copy did; the code is `PARTLY` when some files failed.
fn report(
    summary: &dto::TransferSummary,
    verb: &str,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    let failures: Vec<String> = summary
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.label, failure.error))
        .collect();
    if options.json {
        let _ = writeln!(
            out,
            "{}",
            json!({
                "files": summary.files,
                "bytes": summary.bytes,
                "already_there": summary.already_there,
                "skipped": summary.skipped,
                "replaced": summary.replaced,
                "renamed": summary.renamed,
                "failures": failures,
                "into": summary.destination,
            })
        );
    } else {
        let mut line = format!(
            "{verb} {} ({}) to {}",
            files(summary.files),
            bytes(summary.bytes),
            summary.destination
        );
        if summary.already_there > 0 {
            line.push_str(&format!(", {} already there", summary.already_there));
        }
        if summary.skipped > 0 {
            line.push_str(&format!(", {} skipped", summary.skipped));
        }
        let _ = writeln!(out, "{line}");
        for failure in &failures {
            let _ = writeln!(out, "failed: {failure}");
        }
    }
    if failures.is_empty() { OK } else { PARTLY }
}

async fn import(
    transfers: &transfer::Service,
    run: &Run<'_>,
    into: &Path,
    recorded: bool,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let request = dto::ImportRequest {
        storage_id: run.storage,
        destination: absolute_folder(into)?,
        folders: run.options.by.unwrap_or_default(),
        remove_from_phone: run.options.remove,
    };
    if run.options.dry_run {
        let preview = transfers
            .import_preview(&request, run.cancel, |_| {})
            .await?;
        return Ok(import_dry_run_report(&preview, run.options, out));
    }
    let summary = transfers.import(&request, run.cancel, run.progress).await?;
    Ok(import_report(&summary, recorded, run.options, out))
}

fn import_report(
    summary: &dto::TransferSummary,
    recorded: bool,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    let failures = failure_lines(summary);
    let removal = summary.removal.clone().unwrap_or_default();
    if options.json {
        let mut answer = json!({
            "files": summary.files,
            "bytes": summary.bytes,
            "imported_before": summary.already_there,
            "into": summary.destination,
            "recorded": recorded,
            "failures": failures,
        });
        if options.remove {
            let kept: Vec<_> = removal
                .kept
                .iter()
                .map(|kept| json!({ "item": kept.label, "why": why_words(&kept.why) }))
                .collect();
            answer["removed"] = json!(removal.removed);
            answer["freed"] = json!(removal.freed);
            answer["kept"] = json!(kept);
        }
        let _ = writeln!(out, "{answer}");
    } else {
        let _ = writeln!(out, "{}", import_headline(summary));
        if removal.removed > 0 {
            let _ = writeln!(
                out,
                "Removed {} from the phone ({} freed)",
                files(removal.removed),
                bytes(removal.freed)
            );
        }
        for kept in &removal.kept {
            let _ = writeln!(
                out,
                "kept on the phone: {}: {}",
                kept.label,
                why_words(&kept.why)
            );
        }
        for failure in &failures {
            let _ = writeln!(out, "failed: {failure}");
        }
        if !recorded {
            let _ = writeln!(out, "The phone has no serial number: nothing was recorded");
        }
    }
    if failures.is_empty() { OK } else { PARTLY }
}

fn import_headline(summary: &dto::TransferSummary) -> String {
    let imported_before = summary.already_there;
    let nothing_new = summary.files == 0 && summary.failures.is_empty();
    if nothing_new && imported_before == 0 {
        return "Nothing new to import".to_owned();
    }
    if nothing_new {
        return format!("Nothing new to import ({imported_before} imported before)");
    }
    let mut line = format!(
        "Imported {} ({}) to {}",
        files(summary.files),
        bytes(summary.bytes),
        summary.destination
    );
    if imported_before > 0 {
        line.push_str(&format!(", {imported_before} imported before"));
    }
    line
}

fn import_dry_run_report(
    preview: &dto::ImportPreview,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    if options.json {
        let answer = json!({
            "new_files": preview.new_files,
            "new_bytes": preview.new_bytes,
            "imported_before": preview.already,
        });
        let _ = writeln!(out, "{answer}");
        return OK;
    }
    let mut line = if preview.new_files == 0 {
        "Nothing new to import".to_owned()
    } else {
        let noun = if preview.new_files == 1 {
            "file"
        } else {
            "files"
        };
        format!(
            "{} new {noun} ({}) to import",
            preview.new_files,
            bytes(preview.new_bytes)
        )
    };
    if preview.already > 0 {
        line.push_str(&format!(", {} imported before", preview.already));
    }
    let _ = writeln!(out, "{line}");
    OK
}

fn why_words(why: &dto::Why) -> String {
    match why {
        dto::Why::Unchecked => {
            "the phone gives no size, so the copy could not be checked".to_owned()
        }
        dto::Why::Changed => "it changed on the phone after it was copied".to_owned(),
        dto::Why::NotEmpty => "a folder that still holds something".to_owned(),
        dto::Why::Refused(error) => format!("the phone would not remove it: {error}"),
        dto::Why::NotCopied => "it was not copied".to_owned(),
    }
}

async fn check(
    transfers: &transfer::Service,
    run: &Run<'_>,
    from: &[String],
    into: &Path,
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let against = absolute_folder(into)?;
    let mut total = dto::TransferSummary {
        compared: Some(dto::Compared::default()),
        ..dto::TransferSummary::default()
    };
    for (phone_folder, names) in by_folder(from)? {
        let request = dto::CheckRequest {
            storage_id: run.storage,
            phone_folder,
            mac_folder: against.clone(),
            names,
        };
        let summary = transfers
            .check_contents(&request, run.cancel, run.progress)
            .await?;
        add_up_check(&mut total, summary);
    }
    Ok(check_report(&total, &against, run.options, out))
}

fn add_up_check(total: &mut dto::TransferSummary, summary: dto::TransferSummary) {
    let compared = summary.compared.unwrap_or_default();
    let sum = total.compared.get_or_insert_with(dto::Compared::default);
    sum.identical += compared.identical;
    sum.differ += compared.differ;
    sum.missing += compared.missing;
    sum.unreadable += compared.unreadable;
    total.report.extend(summary.report);
}

fn check_report(
    summary: &dto::TransferSummary,
    against: &str,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    let compared = summary.compared.unwrap_or_default();
    if options.json {
        let files: Vec<_> = summary.report.iter().filter_map(check_line_json).collect();
        let answer = json!({
            "identical": compared.identical,
            "differ": compared.differ,
            "missing": compared.missing,
            "unreadable": compared.unreadable,
            "against": against,
            "files": files,
        });
        let _ = writeln!(out, "{answer}");
    } else {
        for line in summary.report.iter().filter_map(check_line_words) {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out, "{}", check_totals(&compared, against));
    }
    let all_identical = compared.differ + compared.missing + compared.unreadable == 0;
    if all_identical { OK } else { PARTLY }
}

fn check_line_words(line: &dto::FileReport) -> Option<String> {
    let path = &line.path;
    match &line.outcome {
        dto::FileOutcome::Differs { at } => {
            Some(format!("differs at byte {}: {path}", with_commas(at + 1)))
        }
        dto::FileOutcome::SizesDiffer { phone, mac } => Some(format!(
            "sizes differ: {path} (phone {}, Mac {})",
            bytes(*phone),
            bytes(*mac)
        )),
        dto::FileOutcome::NotOnMac => Some(format!("not on the Mac: {path}")),
        dto::FileOutcome::NotOnPhone => Some(format!("not on the phone: {path}")),
        dto::FileOutcome::Failed(error) => Some(format!("could not read: {path}: {error}")),
        _ => None,
    }
}

fn check_line_json(line: &dto::FileReport) -> Option<serde_json::Value> {
    let path = &line.path;
    match &line.outcome {
        dto::FileOutcome::Differs { at } => {
            Some(json!({ "path": path, "outcome": "differs", "at": at }))
        }
        dto::FileOutcome::SizesDiffer { phone, mac } => Some(json!({
            "path": path,
            "outcome": "sizes_differ",
            "phone": phone,
            "mac": mac,
        })),
        dto::FileOutcome::NotOnMac => Some(json!({ "path": path, "outcome": "not_on_mac" })),
        dto::FileOutcome::NotOnPhone => Some(json!({ "path": path, "outcome": "not_on_phone" })),
        dto::FileOutcome::Failed(error) => Some(json!({
            "path": path,
            "outcome": "unreadable",
            "error": error.to_string(),
        })),
        _ => None,
    }
}

fn check_totals(compared: &dto::Compared, against: &str) -> String {
    let checked = compared.total();
    if checked > 0 && compared.identical == checked {
        return format!("All {} identical to {against}", counted_files(checked));
    }
    let mut parts = vec![format!("{} identical", compared.identical)];
    match compared.differ {
        0 => {}
        1 => parts.push("1 differs".to_owned()),
        many => parts.push(format!("{many} differ")),
    }
    if compared.missing > 0 {
        parts.push(format!("{} on one side only", compared.missing));
    }
    if compared.unreadable > 0 {
        parts.push(format!("{} could not be read", compared.unreadable));
    }
    format!(
        "Checked {} against {against}: {}",
        counted_files(checked),
        parts.join(", ")
    )
}

fn counted_files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        many => format!("{} files", with_commas(many as u64)),
    }
}

fn with_commas(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        let left = digits.len() - at;
        if at > 0 && left.is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

struct MirrorCall<'a, 'w> {
    choice: &'a Choice,
    mirrors_here: &'a [(dto::SavedMirror, u64)],
    world: &'a mut World,
    err: &'a Stderr<'w>,
}

enum Chosen {
    One(dto::SavedMirror, u64),
    NotSaved,
    Several,
}

async fn mirror(
    transfers: &transfer::Service,
    call: MirrorCall<'_, '_>,
    options: &Options,
    progress: &(dyn Fn(dto::TransferProgress) + Send + Sync),
    out: &mut dyn Write,
) -> Result<i32, dto::Error> {
    let (saved, storage_id) = match choose_mirror(call.mirrors_here, call.choice) {
        Chosen::One(saved, storage_id) => (saved, storage_id),
        Chosen::NotSaved => {
            let why = format!(
                "no saved mirror {} for this phone",
                choice_words(call.choice)
            );
            return Ok(refuse(call.err, out, options, &why, NOT_FOUND));
        }
        Chosen::Several => {
            let why = format!(
                "two saved mirrors are called {}: use its number",
                choice_words(call.choice)
            );
            return Ok(refuse(call.err, out, options, &why, USAGE_ERROR));
        }
    };
    if removes_from_a_top_folder(&saved, &call.world.home) {
        let why = "a mirror never removes extras from your home folder or the whole disk";
        return Ok(refuse(call.err, out, options, why, USAGE_ERROR));
    }
    let label = saved.label();
    let cancel = call.world.cancel.clone();
    let mut request = dto::MirrorRequest {
        storage_id,
        phone_folder: saved.phone_folder.clone(),
        mac_folder: saved.mac_folder.clone(),
        direction: saved.direction,
        remove_extras: saved.remove_extras,
    };
    if options.dry_run {
        let preview = transfers.mirror_preview(&request, &cancel, |_| {}).await?;
        return Ok(mirror_dry_run_report(&saved, &preview, options, out));
    }
    if saved.remove_extras && !options.yes {
        let preview = transfers.mirror_preview(&request, &cancel, |_| {}).await?;
        match (call.world.ask)(&removal_question(&saved, preview.extras)) {
            Some(true) => {}
            Some(false) => {
                if options.json {
                    let _ = writeln!(out, "{}", json!({ "mirror": label, "changed": false }));
                } else {
                    let _ = writeln!(out, "Nothing was changed.");
                }
                return Ok(OK);
            }
            None => {
                let why = format!(
                    "mirror {} removes {}: add --yes to do it without asking",
                    choice_words(call.choice),
                    items(preview.extras)
                );
                return Ok(refuse(call.err, out, options, &why, USAGE_ERROR));
            }
        }
    }
    request.remove_extras = saved.remove_extras;
    let summary = transfers.mirror(&request, &cancel, progress).await?;
    Ok(mirror_report(&label, &summary, options, out))
}

fn mirrors_of_this_phone(
    home: &Path,
    serial: &str,
    storages: &[(u64, String)],
) -> Vec<(dto::SavedMirror, u64)> {
    if serial.is_empty() {
        return Vec::new();
    }
    let places = internal::service::places::Service::new(
        app_folder(home).join("places.json"),
        home.to_path_buf(),
        PathBuf::from("/Volumes"),
    );
    places
        .mirrors()
        .into_iter()
        .filter(|mirror| mirror.phone.serial == serial)
        .filter_map(|mirror| {
            let storage_id = mirror.phone.storage_among(storages)?;
            Some((mirror, storage_id))
        })
        .collect()
}

fn choose_mirror(mirrors_here: &[(dto::SavedMirror, u64)], choice: &Choice) -> Chosen {
    match choice {
        Choice::Number(number) => match mirrors_here.get(number - 1) {
            Some((saved, storage_id)) => Chosen::One(saved.clone(), *storage_id),
            None => Chosen::NotSaved,
        },
        Choice::Label(label) => {
            let called: Vec<_> = mirrors_here
                .iter()
                .filter(|(saved, _)| saved.label() == *label)
                .collect();
            match called.as_slice() {
                [] => Chosen::NotSaved,
                [(saved, storage_id)] => Chosen::One(saved.clone(), *storage_id),
                _ => Chosen::Several,
            }
        }
    }
}

fn choice_words(choice: &Choice) -> String {
    match choice {
        Choice::Number(number) => number.to_string(),
        Choice::Label(label) => format!("\"{label}\""),
    }
}

fn removes_from_a_top_folder(saved: &dto::SavedMirror, home: &Path) -> bool {
    let mac_folder = Path::new(&saved.mac_folder);
    let removes_on_mac = saved.remove_extras && saved.direction == dto::MirrorDirection::ToMac;
    removes_on_mac && (mac_folder == home || mac_folder == Path::new("/"))
}

fn removal_question(saved: &dto::SavedMirror, extras: usize) -> String {
    let that_are = if extras == 1 { "that is" } else { "that are" };
    let where_and_how = match saved.direction {
        dto::MirrorDirection::ToMac => format!("only in {} (to the Trash)", saved.mac_folder),
        dto::MirrorDirection::ToPhone => format!(
            "only on the phone in {} (deleted for good)",
            saved.phone_folder
        ),
    };
    format!(
        "Remove {} {that_are} {where_and_how}? [y/N] ",
        items(extras)
    )
}

fn refuse(err: &Stderr<'_>, out: &mut dyn Write, options: &Options, why: &str, code: i32) -> i32 {
    if options.json {
        let _ = writeln!(out, "{}", json!({ "error": why }));
    }
    let _ = writeln!(err.lock().unwrap(), "open-mtp-client-cli: {why}");
    code
}

fn direction_words(direction: dto::MirrorDirection) -> &'static str {
    match direction {
        dto::MirrorDirection::ToMac => "to the Mac",
        dto::MirrorDirection::ToPhone => "to the phone",
    }
}

fn direction_key(direction: dto::MirrorDirection) -> &'static str {
    match direction {
        dto::MirrorDirection::ToMac => "to_mac",
        dto::MirrorDirection::ToPhone => "to_phone",
    }
}

fn mirrors_report(
    listed: &[(dto::SavedMirror, u64)],
    phone_has_serial: bool,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    if options.json {
        let answer: Vec<_> = listed
            .iter()
            .enumerate()
            .map(|(at, (saved, _))| {
                json!({
                    "number": at + 1,
                    "label": saved.label(),
                    "direction": direction_key(saved.direction),
                    "removes_extras": saved.remove_extras,
                    "phone_folder": saved.phone_folder,
                    "mac_folder": saved.mac_folder,
                    "storage": saved.phone.storage_name,
                })
            })
            .collect();
        let _ = writeln!(out, "{}", json!(answer));
        return OK;
    }
    if !phone_has_serial {
        let _ = writeln!(
            out,
            "The phone has no serial number, so it has no saved mirrors"
        );
        return OK;
    }
    if listed.is_empty() {
        let _ = writeln!(out, "No saved mirrors for this phone");
        return OK;
    }
    for (at, (saved, _)) in listed.iter().enumerate() {
        let removes = if saved.remove_extras {
            ", removes extras"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{}  {}  {}{removes}",
            at + 1,
            saved.label(),
            direction_words(saved.direction)
        );
    }
    OK
}

fn mirror_report(
    label: &str,
    summary: &dto::TransferSummary,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    let failures = failure_lines(summary);
    let mirrored = summary.mirrored.clone().unwrap_or_default();
    if options.json {
        let answer = json!({
            "mirror": label,
            "copied": mirrored.copied,
            "replaced": mirrored.replaced,
            "removed": mirrored.removed,
            "kept": mirrored.kept,
            "conflicts": mirrored.conflicts,
            "failures": failures,
        });
        let _ = writeln!(out, "{answer}");
    } else {
        let parts: Vec<String> = [
            (mirrored.copied, "copied"),
            (mirrored.replaced, "replaced"),
            (mirrored.removed, "removed"),
            (mirrored.conflicts, "conflicts"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
        if parts.is_empty() {
            let _ = writeln!(out, "Already the same: {label}");
        } else {
            let _ = writeln!(out, "Mirrored {label}: {}", parts.join(", "));
        }
        for failure in &failures {
            let _ = writeln!(out, "failed: {failure}");
        }
    }
    if failures.is_empty() { OK } else { PARTLY }
}

fn mirror_dry_run_report(
    saved: &dto::SavedMirror,
    preview: &dto::MirrorPreview,
    options: &Options,
    out: &mut dyn Write,
) -> i32 {
    if options.json {
        let answer = json!({
            "new_files": preview.new_files,
            "new_bytes": preview.new_bytes,
            "changed_files": preview.changed_files,
            "changed_bytes": preview.changed_bytes,
            "extras": preview.extras,
            "conflicts": preview.conflicts,
        });
        let _ = writeln!(out, "{answer}");
        return OK;
    }
    let mut parts = Vec::new();
    if preview.new_files > 0 {
        parts.push(format!(
            "copy {} ({})",
            files(preview.new_files),
            bytes(preview.new_bytes)
        ));
    }
    if preview.changed_files > 0 {
        parts.push(format!(
            "replace {} ({})",
            preview.changed_files,
            bytes(preview.changed_bytes)
        ));
    }
    if saved.remove_extras && preview.extras > 0 {
        parts.push(format!("remove {}", preview.extras));
    }
    let label = saved.label();
    let mut line = if parts.is_empty() {
        format!("{label} is already the same")
    } else {
        format!("{label} would {}", parts.join(", "))
    };
    if preview.conflicts > 0 {
        line.push_str(&format!(", with {} conflicts", preview.conflicts));
    }
    let _ = writeln!(out, "{line}");
    OK
}

fn failure_lines(summary: &dto::TransferSummary) -> Vec<String> {
    summary
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.label, failure.error))
        .collect()
}

fn absolute_folder(folder: &Path) -> Result<String, dto::Error> {
    let whole_path =
        std::path::absolute(folder).map_err(|error| dto::Error::General(error.to_string()))?;
    Ok(whole_path.to_string_lossy().into_owned())
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

/// Bytes as a person reads them: 12.4 MB.
fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn date(when: &dto::Timestamp) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        when.year, when.month, when.day, when.hour, when.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn commands_and_options_are_read() {
        let (options, command) =
            parse(&args(&["--virtual", "/tmp/p", "--json", "ls", "/DCIM/"])).unwrap();
        assert_eq!(command, Command::Ls("/DCIM".into()));
        assert_eq!(options.virtual_dir, Some(PathBuf::from("/tmp/p")));
        assert!(options.json);

        let (options, command) = parse(&args(&[
            "push",
            "a.jpg",
            "b.jpg",
            "/DCIM",
            "--skip",
            "--storage",
            "7",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Push {
                from: vec!["a.jpg".into(), "b.jpg".into()],
                into: "/DCIM".into()
            }
        );
        assert_eq!(options.conflict, Some(dto::Conflict::Skip));
        assert_eq!(options.storage, Some(7));
        assert_eq!(parse(&args(&["ls"])).unwrap().1, Command::Ls("/".into()));
        assert_eq!(parse(&args(&["-h"])).unwrap().1, Command::Help);
        assert_eq!(parse(&args(&["phones"])).unwrap().1, Command::Phones);
        assert_eq!(
            parse(&args(&["pull", "/DCIM/a.jpg", "."])).unwrap().1,
            Command::Pull {
                from: vec!["/DCIM/a.jpg".into()],
                into: ".".into()
            }
        );
    }

    #[test]
    fn what_cannot_be_done_is_said_before_anything_is_tried() {
        for (words, said) in [
            (vec![], "no command"),
            (vec!["dance"], "not a command"),
            (vec!["ls", "DCIM"], "starts at the top"),
            (vec!["push", "/DCIM"], "not a command"),
            (vec!["--storage", "x", "ls"], "not a storage id"),
            (vec!["--phone"], "needs a value"),
            (vec!["--colour", "ls"], "unknown option"),
        ] {
            let problem = parse(&args(&words)).unwrap_err();
            assert!(problem.contains(said), "{words:?}: {problem}");
        }
    }

    #[test]
    fn each_error_has_its_exit_code() {
        assert_eq!(exit_code(&dto::Error::NoDevice), NO_PHONE);
        assert_eq!(exit_code(&dto::Error::AllowStorageAccess), NO_PHONE);
        assert_eq!(exit_code(&dto::Error::PathNotFound("/x".into())), NOT_FOUND);
        assert_eq!(exit_code(&dto::Error::Timeout), TRY_AGAIN);
        assert_eq!(exit_code(&dto::Error::NameTaken("a".into())), CANT_CREATE);
        assert_eq!(
            exit_code(&dto::Error::VolumeReadOnly(dto::Disk::Volume(
                "BACKUP".into()
            ))),
            CANT_CREATE
        );
        assert_eq!(
            exit_code(&dto::Error::TooBigForVolume(dto::Disk::Volume(
                "KINGSTON".into()
            ))),
            CANT_CREATE
        );
        assert_eq!(exit_code(&dto::Error::Unsupported), FAILED);
    }

    #[test]
    fn sizes_and_dates_read_as_a_person_reads_them() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(12_400_000), "12.4 MB");
        assert_eq!(bytes(3_000_000_000), "3.0 GB");
        let when = dto::Timestamp {
            year: 2026,
            month: 10,
            day: 7,
            hour: 9,
            minute: 5,
        };
        assert_eq!(date(&when), "2026-10-07 09:05");
        assert_eq!(split("/DCIM/a.jpg"), Some(("/DCIM".into(), "a.jpg".into())));
        assert_eq!(split("/a.jpg"), Some(("/".into(), "a.jpg".into())));
        assert_eq!(split("/"), None);
    }

    /// A fake phone with DCIM/Camera/a.jpg, notes.txt and a hidden file.
    fn phone() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("DCIM/Camera")).unwrap();
        std::fs::write(dir.path().join("DCIM/Camera/a.jpg"), b"jpeg").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join(".nomedia"), b"").unwrap();
        dir
    }

    fn world() -> World {
        World {
            ask: Box::new(|_| None),
            terminal: false,
            cancel: dto::Cancel::new(),
            trash: None,
            home: PathBuf::from("/nonexistent/open-mtp-client-cli-tests"),
        }
    }

    async fn ask(phone: &Path, words: &[&str]) -> (i32, String, String) {
        ask_in(phone, words, &mut world()).await
    }

    async fn ask_in(phone: &Path, words: &[&str], world: &mut World) -> (i32, String, String) {
        let mut all = vec!["--virtual".to_owned(), phone.to_string_lossy().into_owned()];
        all.extend(words.iter().map(|word| (*word).to_owned()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&all, &mut out, &mut err, world).await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    // The second pass: more commands, moves, progress and ctrl-c.

    #[test]
    fn the_new_commands_and_options_are_understood() {
        let (options, command) = parse(&args(&["-p", "mkdir", "/a/b"])).unwrap();
        assert!(options.parents);
        assert_eq!(command, Command::Mkdir("/a/b".into()));
        let (options, command) = parse(&args(&["rm", "--yes", "/a", "/b/"])).unwrap();
        assert!(options.yes);
        assert_eq!(command, Command::Rm(vec!["/a".into(), "/b".into()]));
        assert!(parse(&args(&["-y", "rm", "/a"])).unwrap().0.yes);
        assert_eq!(
            parse(&args(&["mv", "/a.jpg", "/b.jpg", "/Old"])).unwrap().1,
            Command::Mv {
                from: vec!["/a.jpg".into(), "/b.jpg".into()],
                into: "/Old".into()
            }
        );
        assert_eq!(parse(&args(&["info"])).unwrap().1, Command::Info);
        let (options, command) = parse(&args(&["--move", "pull", "/a", "/b", "."])).unwrap();
        assert!(options.moving);
        assert_eq!(
            command,
            Command::Pull {
                from: vec!["/a".into(), "/b".into()],
                into: ".".into()
            }
        );
        for (words, said) in [
            (vec!["rm"], "not a command"),
            (vec!["mkdir"], "not a command"),
            (vec!["mv", "/a"], "not a command"),
            (vec!["rm", "DCIM"], "starts at the top"),
        ] {
            let problem = parse(&args(&words)).unwrap_err();
            assert!(problem.contains(said), "{words:?}: {problem}");
        }
        assert_eq!(exit_code(&dto::Error::Cancelled), CANCELLED);
    }

    #[tokio::test]
    async fn mkdir_makes_a_folder_and_with_p_the_ones_on_the_way() {
        let phone = phone();

        let (code, out, _) = ask(phone.path(), &["mkdir", "/Backup"]).await;
        assert_eq!((code, out.as_str()), (OK, "Made /Backup\n"));
        assert!(phone.path().join("Backup").is_dir());

        let (code, _, err) = ask(phone.path(), &["mkdir", "/Backup"]).await;
        assert_eq!(code, CANT_CREATE, "taken: {err}");
        let (code, _, _) = ask(phone.path(), &["mkdir", "/No/Such"]).await;
        assert_eq!(code, NOT_FOUND, "without -p the folder above must be there");

        let (code, _, _) = ask(phone.path(), &["mkdir", "-p", "/DCIM/2026/October"]).await;
        assert_eq!(code, OK);
        assert!(phone.path().join("DCIM/2026/October").is_dir());
        let (code, _, _) = ask(phone.path(), &["mkdir", "-p", "/DCIM/2026"]).await;
        assert_eq!(code, OK, "already there is fine with -p");
        let (code, _, _) = ask(phone.path(), &["mkdir", "-p", "/notes.txt/x"]).await;
        assert_eq!(code, CANT_CREATE, "a file on the way");
        let (code, _, _) = ask(phone.path(), &["mkdir", "/"]).await;
        assert_eq!(code, OK, "the top is always there");
    }

    #[tokio::test]
    async fn rm_asks_first_refuses_in_a_script_and_with_yes_deletes_for_good() {
        let phone = phone();

        let (code, _, err) = ask(phone.path(), &["rm", "/notes.txt"]).await;
        assert_eq!(code, FAILED);
        assert!(err.contains("add --yes"), "{err}");
        assert!(
            phone.path().join("notes.txt").exists(),
            "no one to ask: nothing"
        );

        let asked = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&asked);
        let mut no = World {
            ask: Box::new(move |question| {
                heard.lock().unwrap().push(question.to_owned());
                Some(false)
            }),
            ..world()
        };
        let (code, out, _) = ask_in(phone.path(), &["rm", "/notes.txt"], &mut no).await;
        assert_eq!((code, out.as_str()), (OK, "Nothing was deleted.\n"));
        assert_eq!(
            *asked.lock().unwrap(),
            ["Delete /notes.txt from the phone for good? [y/N] "]
        );
        assert!(phone.path().join("notes.txt").exists());

        let mut yes = World {
            ask: Box::new(|_| Some(true)),
            ..world()
        };
        let (code, out, _) = ask_in(phone.path(), &["rm", "/notes.txt"], &mut yes).await;
        assert_eq!((code, out.as_str()), (OK, "Deleted 1 item\n"));
        assert!(!phone.path().join("notes.txt").exists());

        let (code, _, _) = ask(phone.path(), &["rm", "--yes", "/DCIM/Camera/a.jpg"]).await;
        assert_eq!(code, OK);
        assert!(!phone.path().join("DCIM/Camera/a.jpg").exists());
        let (code, _, _) = ask(phone.path(), &["rm", "--yes", "/gone.txt"]).await;
        assert_eq!(code, NOT_FOUND);
        let (code, _, _) = ask(phone.path(), &["rm", "--yes", "/"]).await;
        assert_eq!(code, FAILED, "never the whole storage");
    }

    #[tokio::test]
    async fn mv_moves_on_the_phone() {
        let phone = phone();
        std::fs::create_dir_all(phone.path().join("Old")).unwrap();

        let (code, out, _) = ask(
            phone.path(),
            &["mv", "/notes.txt", "/DCIM/Camera/a.jpg", "/Old"],
        )
        .await;

        assert_eq!((code, out.as_str()), (OK, "Moved 2 items to /Old\n"));
        assert!(phone.path().join("Old/notes.txt").exists());
        assert!(phone.path().join("Old/a.jpg").exists());
        assert!(!phone.path().join("notes.txt").exists());
        let (code, out, _) = ask(phone.path(), &["--json", "mv", "/Old/a.jpg", "/"]).await;
        assert_eq!(code, OK);
        assert!(out.contains("\"moved\":1"), "{out}");
    }

    #[tokio::test]
    async fn info_says_what_the_phone_is_and_can_do() {
        let phone = phone();

        let (code, out, _) = ask(phone.path(), &["info"]).await;

        assert_eq!(code, OK);
        assert!(out.lines().count() >= 4, "{out}");
        assert!(out.contains("serial "), "{out}");
        assert!(out.contains("can: "), "{out}");
        let (code, out, _) = ask(phone.path(), &["--json", "info"]).await;
        assert_eq!(code, OK);
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert!(parsed["can"]["rename"].is_boolean(), "{out}");
        assert!(parsed.get("battery").is_some());
    }

    #[tokio::test]
    async fn pull_takes_several_paths_and_moves_with_move() {
        let phone = phone();
        let mac = tempfile::tempdir().unwrap();
        let into = mac.path().to_string_lossy().into_owned();

        let (code, out, err) = ask(
            phone.path(),
            &["pull", "/notes.txt", "/DCIM/Camera/a.jpg", &into],
        )
        .await;
        assert_eq!(code, OK, "{err}");
        assert!(out.starts_with("Copied 2 files"), "{out}");
        assert!(mac.path().join("notes.txt").exists() && mac.path().join("a.jpg").exists());
        assert!(err.contains("100%"), "progress on stderr: {err}");

        let elsewhere = tempfile::tempdir().unwrap();
        let into = elsewhere.path().to_string_lossy().into_owned();
        let (code, out, _) = ask(phone.path(), &["--move", "pull", "/notes.txt", &into]).await;
        assert_eq!(code, OK);
        assert!(out.starts_with("Moved 1 file"), "{out}");
        assert!(elsewhere.path().join("notes.txt").exists());
        assert!(
            !phone.path().join("notes.txt").exists(),
            "the original went"
        );
    }

    #[tokio::test]
    async fn push_with_p_makes_the_folder_and_with_move_trashes_each_original() {
        let phone = phone();
        let mac = tempfile::tempdir().unwrap();
        let photo = mac.path().join("photo.jpg");
        std::fs::write(&photo, b"jpeg!").unwrap();
        let photo = photo.to_string_lossy().into_owned();

        let (code, _, _) = ask(phone.path(), &["push", &photo, "/New/Place"]).await;
        assert_eq!(code, NOT_FOUND, "no folder there without -p");
        let (code, _, _) = ask(phone.path(), &["push", "-p", &photo, "/New/Place"]).await;
        assert_eq!(code, OK);
        assert!(phone.path().join("New/Place/photo.jpg").exists());

        let (code, _, _) = ask(phone.path(), &["--move", "push", &photo, "/"]).await;
        assert_eq!(
            code, FAILED,
            "no Trash: a move is refused before anything is sent"
        );
        assert!(!phone.path().join("photo.jpg").exists());

        let bin = tempfile::tempdir().unwrap();
        let into = bin.path().to_path_buf();
        let mut trashing = World {
            trash: Some(Arc::new(move |path: &Path| {
                std::fs::rename(path, into.join(path.file_name().unwrap()))
            })),
            ..world()
        };
        let (code, out, _) = ask_in(
            phone.path(),
            &["--move", "push", &photo, "/"],
            &mut trashing,
        )
        .await;
        assert_eq!(code, OK);
        assert!(out.starts_with("Moved 1 file"), "{out}");
        assert!(phone.path().join("photo.jpg").exists());
        assert!(!mac.path().join("photo.jpg").exists());
        assert!(
            bin.path().join("photo.jpg").exists(),
            "in the (test's) Trash"
        );
    }

    #[tokio::test]
    async fn ctrl_c_stops_a_copy_with_exit_130() {
        let phone = phone();
        let mac = tempfile::tempdir().unwrap();
        let mut stopped = world();
        stopped.cancel.cancel();

        let (code, _, _) = ask_in(
            phone.path(),
            &["pull", "/DCIM", &mac.path().to_string_lossy()],
            &mut stopped,
        )
        .await;

        assert_eq!(code, CANCELLED);
    }

    #[test]
    fn progress_is_lines_in_a_script_and_a_bar_at_a_terminal() {
        let report = |done: u64, total: u64, files_done: usize| dto::TransferProgress {
            bytes_done: done,
            bytes_total: total,
            files_done,
            files_total: 40,
            ..dto::TransferProgress::default()
        };
        assert_eq!(progress_percent(&report(37, 100, 12)), 37);
        assert_eq!(
            progress_percent(&report(0, 0, 10)),
            25,
            "files when no bytes"
        );
        assert_eq!(progress_percent(&dto::TransferProgress::default()), 0);
        assert_eq!(progress_percent(&report(150, 100, 40)), 100);
        assert_eq!(
            progress_line(&report(37, 100, 12), 37, false),
            " 37%  12 of 40 files"
        );
        assert_eq!(
            progress_line(&report(50, 100, 20), 50, true),
            "[############            ]  50%  20 of 40 files"
        );

        let mut sink: Vec<u8> = Vec::new();
        {
            let err: Stderr<'_> = Mutex::new(&mut sink);
            let lines = Progress::new(&err, false);
            lines.show(&report(10, 100, 1));
            lines.show(&report(10, 100, 1));
            lines.show(&report(11, 100, 1));
            lines.show(&report(100, 100, 39));
            lines.show(&report(100, 100, 40));
            lines.done();
        }
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            " 10%  1 of 40 files\n 11%  1 of 40 files\n100%  39 of 40 files\n100%  40 of 40 files\n",
            "once per whole percent, and once more when the last file is done"
        );
        let mut sink: Vec<u8> = Vec::new();
        {
            let err: Stderr<'_> = Mutex::new(&mut sink);
            let bar = Progress::new(&err, true);
            bar.show(&report(100, 100, 40));
            bar.done();
        }
        let drawn = String::from_utf8(sink).unwrap();
        assert!(
            drawn.starts_with('\r') && drawn.ends_with('\n'),
            "{drawn:?}"
        );
    }

    #[tokio::test]
    async fn ls_lists_a_phone_folder_folders_first_and_hidden_only_when_asked() {
        let phone = phone();

        let (code, out, _) = ask(phone.path(), &["ls"]).await;
        assert_eq!(code, OK);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines[0].ends_with("\tDCIM/"));
        assert!(lines[1].starts_with("5 B\t") && lines[1].ends_with("\tnotes.txt"));

        let (_, out, _) = ask(phone.path(), &["--hidden", "ls", "/"]).await;
        assert!(out.contains(".nomedia"));
        let (_, out, _) = ask(phone.path(), &["--json", "ls", "/DCIM/Camera"]).await;
        let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(listed[0]["name"], "a.jpg");
        assert_eq!(listed[0]["size"], 4);
    }

    #[tokio::test]
    async fn storages_and_phones_are_listed() {
        let phone = phone();
        let (code, out, _) = ask(phone.path(), &["storages"]).await;
        assert_eq!(code, OK);
        assert!(out.contains("free of"), "{out}");
        let (code, out, _) = ask(phone.path(), &["--json", "phones"]).await;
        assert_eq!(code, OK);
        let phones: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(phones.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pull_copies_a_phone_file_to_the_mac_as_the_app_does() {
        let phone = phone();
        let mac = tempfile::tempdir().unwrap();
        let into = mac.path().to_string_lossy().into_owned();

        let (code, out, _) = ask(phone.path(), &["pull", "/DCIM/Camera/a.jpg", &into]).await;

        assert_eq!(code, OK, "{out}");
        assert!(out.starts_with("Copied 1 file (4 B) to "), "{out}");
        assert_eq!(std::fs::read(mac.path().join("a.jpg")).unwrap(), b"jpeg");
        let marked = std::process::Command::new("xattr")
            .arg("-p")
            .arg("com.apple.quarantine")
            .arg(mac.path().join("a.jpg"))
            .output()
            .unwrap();
        assert!(marked.status.success(), "quarantined, as from the window");

        let (code, out, _) = ask(phone.path(), &["pull", "/DCIM/Camera/a.jpg", &into]).await;
        assert_eq!(code, OK);
        assert!(
            out.contains("1 already there"),
            "a second time copies nothing: {out}"
        );
    }

    #[tokio::test]
    async fn a_pull_where_some_files_failed_says_which_and_ends_with_code_1() {
        use std::os::unix::fs::PermissionsExt;
        let phone = phone();
        std::fs::write(phone.path().join("DCIM/top.jpg"), b"top").unwrap();
        let mac = tempfile::tempdir().unwrap();
        // DCIM/Camera on the Mac won't take a.jpg; DCIM/top.jpg goes.
        let locked = mac.path().join("DCIM/Camera");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let into = mac.path().to_string_lossy().into_owned();

        let (code, out, _) = ask(phone.path(), &["pull", "/DCIM", &into]).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(code, PARTLY, "{out}");
        assert!(out.contains("failed: "), "{out}");
        assert_eq!(
            std::fs::read(mac.path().join("DCIM/top.jpg")).unwrap(),
            b"top"
        );
    }

    #[tokio::test]
    async fn push_asks_nothing_and_replaces_nothing_unless_told() {
        let phone = phone();
        let mac = tempfile::tempdir().unwrap();
        let notes = mac.path().join("notes.txt");
        std::fs::write(&notes, b"new notes").unwrap();
        let notes = notes.to_string_lossy().into_owned();

        let (code, _, err) = ask(phone.path(), &["push", &notes, "/"]).await;
        assert_eq!(code, CANT_CREATE, "notes.txt is taken");
        assert!(
            err.contains("notes.txt") && err.contains("--keep-both"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(phone.path().join("notes.txt")).unwrap(),
            b"hello"
        );

        let (code, _, _) = ask(phone.path(), &["--keep-both", "push", &notes, "/"]).await;
        assert_eq!(code, OK);
        assert_eq!(
            std::fs::read(phone.path().join("notes (1).txt")).unwrap(),
            b"new notes"
        );
        let (code, out, _) = ask(phone.path(), &["--json", "--replace", "push", &notes, "/"]).await;
        assert_eq!(code, OK);
        let sent: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(sent["replaced"], 1);
        assert_eq!(
            std::fs::read(phone.path().join("notes.txt")).unwrap(),
            b"new notes"
        );

        // The phone's is now as new as the Mac's: only an older one goes.
        std::fs::write(mac.path().join("notes.txt"), b"older notes").unwrap();
        std::fs::File::options()
            .write(true)
            .open(mac.path().join("notes.txt"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800))
            .unwrap();
        let (code, out, _) = ask(
            phone.path(),
            &["--json", "--replace-older", "push", &notes, "/"],
        )
        .await;
        assert_eq!(code, OK);
        let sent: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(sent["replaced"], 0);
        assert_eq!(
            std::fs::read(phone.path().join("notes.txt")).unwrap(),
            b"new notes",
            "the phone's newer file stays"
        );
    }

    async fn serial_of(phone: &Path) -> String {
        let (code, out, err) = ask(phone, &["--json", "info"]).await;
        assert_eq!(code, OK, "{err}");
        let info: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        info["serial"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn the_same_fake_phone_folder_is_the_same_phone() {
        let phone = phone();
        let other = self::phone();

        let first = serial_of(phone.path()).await;
        let again = serial_of(phone.path()).await;
        let elsewhere = serial_of(other.path()).await;

        assert_eq!(first, again);
        assert_ne!(first, elsewhere);
        let hex = first.strip_prefix("cli-").unwrap();
        assert_eq!(hex.len(), 16, "{first}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
    }

    fn camera_phone() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let camera = dir.path().join("DCIM/Camera");
        std::fs::create_dir_all(&camera).unwrap();
        std::fs::write(camera.join("a.jpg"), b"jpeg").unwrap();
        std::fs::write(camera.join("b.mp4"), b"movie").unwrap();
        std::fs::write(camera.join("notes.txt"), b"not a photo").unwrap();
        dir
    }

    fn at_home(home: &Path) -> World {
        World {
            home: home.to_path_buf(),
            ..world()
        }
    }

    async fn ask_at(phone: &Path, home: &Path, words: &[&str]) -> (i32, String, String) {
        ask_in(phone, words, &mut at_home(home)).await
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn import_record_folder(home: &Path) -> PathBuf {
        internal::service::app_folder(home).join("imported")
    }

    fn date_file(path: &Path, unix_seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_seconds))
            .unwrap();
    }

    const MARCH_14_2026_NOON_UTC: u64 = 1_773_489_600;

    #[test]
    fn import_and_its_options_are_understood() {
        let (options, command) = parse(&args(&["import", "/Users/me/Photos"])).unwrap();
        assert_eq!(
            command,
            Command::Import {
                into: "/Users/me/Photos".into()
            }
        );
        assert_eq!(options.by, None);
        assert!(!options.remove && !options.dry_run);

        for (layout, folders) in [
            ("day", dto::ImportFolders::Day),
            ("month", dto::ImportFolders::Month),
            ("year-month", dto::ImportFolders::YearMonth),
        ] {
            let (options, _) = parse(&args(&["import", "--by", layout, "x"])).unwrap();
            assert_eq!(options.by, Some(folders), "{layout}");
        }
        let (options, _) = parse(&args(&["--remove", "--dry-run", "import", "x"])).unwrap();
        assert!(options.remove && options.dry_run);

        for (words, said) in [
            (vec!["import", "--by", "week", "x"], "not a layout"),
            (vec!["--move", "import", "x"], "--remove"),
            (vec!["--by", "day", "pull", "/a", "x"], "only for import"),
            (vec!["--remove", "ls"], "only for import"),
            (vec!["--dry-run", "ls"], "--dry-run"),
            (vec!["import"], "not a command"),
            (vec!["import", "a", "b"], "not a command"),
        ] {
            let problem = parse(&args(&words)).unwrap_err();
            assert!(problem.contains(said), "{words:?}: {problem}");
        }
    }

    #[test]
    fn the_help_describes_import_and_its_options() {
        for named in [
            "import <Mac folder>",
            "--by day|month|year-month",
            "--remove",
            "--dry-run",
        ] {
            assert!(USAGE.contains(named), "{named}");
        }
    }

    #[tokio::test]
    async fn import_brings_over_only_photos_and_videos() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();

        let (code, out, err) =
            ask_at(phone.path(), home.path(), &["import", &text(photos.path())]).await;

        assert_eq!(code, OK, "{err}");
        assert!(out.starts_with("Imported 2 files ("), "{out}");
        assert!(photos.path().join("a.jpg").exists());
        assert!(photos.path().join("b.mp4").exists());
        assert!(!photos.path().join("notes.txt").exists());
    }

    #[tokio::test]
    async fn a_second_import_copies_nothing_and_says_so() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let into = text(photos.path());
        ask_at(phone.path(), home.path(), &["import", &into]).await;

        let (code, out, _) = ask_at(phone.path(), home.path(), &["import", &into]).await;

        assert_eq!(code, OK);
        assert_eq!(out, "Nothing new to import (2 imported before)\n");
    }

    #[tokio::test]
    async fn the_record_keeps_a_photo_moved_away_from_coming_back() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let into = text(photos.path());
        ask_at(phone.path(), home.path(), &["import", &into]).await;
        std::fs::remove_file(photos.path().join("a.jpg")).unwrap();

        let (code, _, _) = ask_at(phone.path(), home.path(), &["import", &into]).await;

        assert_eq!(code, OK);
        assert!(
            !photos.path().join("a.jpg").exists(),
            "remembered as imported"
        );
        let records: Vec<_> = std::fs::read_dir(import_record_folder(home.path()))
            .unwrap()
            .collect();
        assert_eq!(records.len(), 1);
    }

    #[tokio::test]
    async fn import_lays_photos_out_by_day_month_or_year_and_month() {
        for (layout, place) in [
            ("day", "2026-03-14/a.jpg"),
            ("month", "2026-03/a.jpg"),
            ("year-month", "2026/03/a.jpg"),
        ] {
            let phone = camera_phone();
            date_file(
                &phone.path().join("DCIM/Camera/a.jpg"),
                MARCH_14_2026_NOON_UTC,
            );
            let home = tempfile::tempdir().unwrap();
            let photos = tempfile::tempdir().unwrap();

            let (code, _, err) = ask_at(
                phone.path(),
                home.path(),
                &["import", "--by", layout, &text(photos.path())],
            )
            .await;

            assert_eq!(code, OK, "{layout}: {err}");
            assert!(photos.path().join(place).exists(), "{layout}");
        }
    }

    #[tokio::test]
    async fn import_with_remove_frees_the_phone_of_what_it_checked() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();

        let (code, out, err) = ask_at(
            phone.path(),
            home.path(),
            &["import", "--remove", &text(photos.path())],
        )
        .await;

        assert_eq!(code, OK, "{err}");
        assert!(photos.path().join("a.jpg").exists());
        assert!(!phone.path().join("DCIM/Camera/a.jpg").exists());
        assert!(phone.path().join("DCIM/Camera/notes.txt").exists());
        assert!(
            out.contains("Removed 2 files from the phone (") && out.contains(" freed)"),
            "{out}"
        );
    }

    fn kept(label: &str, why: dto::Why) -> dto::Kept {
        dto::Kept {
            label: label.to_owned(),
            why,
        }
    }

    #[test]
    fn each_photo_kept_on_the_phone_is_named_with_its_reason() {
        let reasons = [
            dto::Why::Unchecked,
            dto::Why::Changed,
            dto::Why::NotEmpty,
            dto::Why::Refused(dto::Error::PermissionDenied),
            dto::Why::NotCopied,
        ];
        let summary = dto::TransferSummary {
            files: 5,
            destination: "/Users/me/Photos".into(),
            removal: Some(dto::Removal {
                kept: reasons
                    .iter()
                    .enumerate()
                    .map(|(at, why)| kept(&format!("DCIM/{at}.jpg"), why.clone()))
                    .collect(),
                ..dto::Removal::default()
            }),
            ..dto::TransferSummary::default()
        };
        let removing = Options {
            remove: true,
            ..Options::default()
        };

        let mut out = Vec::new();
        import_report(&summary, true, &removing, &mut out);
        let printed = String::from_utf8(out).unwrap();

        for (at, why) in reasons.iter().enumerate() {
            let line = format!("kept on the phone: DCIM/{at}.jpg: {}", why_words(why));
            assert!(printed.contains(&line), "{line}\n{printed}");
        }
        let words: std::collections::HashSet<String> = reasons.iter().map(why_words).collect();
        assert_eq!(words.len(), reasons.len(), "each reason its own words");
        assert!(words.iter().all(|said| !said.is_empty()));

        let as_json = Options {
            json: true,
            ..removing
        };
        let mut out = Vec::new();
        import_report(&summary, true, &as_json, &mut out);
        let answer: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(answer["kept"].as_array().unwrap().len(), reasons.len());
        assert_eq!(answer["kept"][0]["item"], "DCIM/0.jpg");
        assert_eq!(answer["kept"][0]["why"], why_words(&dto::Why::Unchecked));
    }

    async fn transfers_on(phone: &Path, home: &Path) -> (transfer::Service, u64) {
        let device = Arc::new(device::Service::new(device::Source::Virtual(
            device::VirtualPhone::new(phone.to_path_buf(), fake_phone_serial(phone)),
        )));
        device.connect_to(None).await.unwrap();
        let storage = device.storages().await.unwrap()[0].id;
        let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
        let record = transfer::ImportRecord::at(import_record_folder(home));
        (
            transfer::Service::new(device, explorer).with_imports(record),
            storage,
        )
    }

    fn import_request(storage_id: u64, into: &Path) -> dto::ImportRequest {
        dto::ImportRequest {
            storage_id,
            destination: text(into),
            folders: dto::ImportFolders::None,
            remove_from_phone: false,
        }
    }

    #[tokio::test]
    async fn the_app_and_the_tool_agree_about_what_was_imported() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        ask_at(phone.path(), home.path(), &["import", &text(photos.path())]).await;

        let fresh = tempfile::tempdir().unwrap();
        let (transfers, storage) = transfers_on(phone.path(), home.path()).await;
        let preview = transfers
            .import_preview(
                &import_request(storage, fresh.path()),
                &dto::Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!((preview.already, preview.new_files), (2, 0));

        let other_phone = camera_phone();
        let other_home = tempfile::tempdir().unwrap();
        let (transfers, storage) = transfers_on(other_phone.path(), other_home.path()).await;
        let app_photos = tempfile::tempdir().unwrap();
        transfers
            .import(
                &import_request(storage, app_photos.path()),
                &dto::Cancel::new(),
                |_| {},
            )
            .await
            .unwrap();
        drop(transfers);
        let fresh = tempfile::tempdir().unwrap();
        let (code, out, err) = ask_at(
            other_phone.path(),
            other_home.path(),
            &["--json", "import", "--dry-run", &text(fresh.path())],
        )
        .await;
        assert_eq!(code, OK, "{err}");
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(answer["imported_before"], 2, "{out}");
        assert_eq!(answer["new_files"], 0, "{out}");
    }

    #[tokio::test]
    async fn a_dry_run_import_says_what_it_would_do_and_does_nothing() {
        let phone = camera_phone();
        std::fs::write(phone.path().join("DCIM/Camera/c.jpg"), b"third").unwrap();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let into = text(photos.path());

        let (code, out, _) =
            ask_at(phone.path(), home.path(), &["import", "--dry-run", &into]).await;

        assert_eq!(code, OK);
        assert!(
            out.starts_with("3 new files (") && out.trim_end().ends_with(") to import"),
            "{out}"
        );
        assert_eq!(std::fs::read_dir(photos.path()).unwrap().count(), 0);
        assert!(!import_record_folder(home.path()).exists());
        let (_, out, _) = ask_at(
            phone.path(),
            home.path(),
            &["--json", "import", "--dry-run", &into],
        )
        .await;
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(answer["new_files"], 3);
        assert_eq!(answer["new_bytes"], 14);
        assert_eq!(answer["imported_before"], 0);
    }

    #[tokio::test]
    async fn import_says_what_stopped_it() {
        let phone = camera_phone();
        let home = tempfile::tempdir().unwrap();
        let photos = tempfile::tempdir().unwrap();
        let into = text(photos.path());

        let (code, _, _) = ask_at(
            phone.path(),
            home.path(),
            &["import", &text(&photos.path().join("missing"))],
        )
        .await;
        assert_eq!(code, NOT_FOUND, "the Mac folder must be there");

        let no_dcim = tempfile::tempdir().unwrap();
        std::fs::write(no_dcim.path().join("notes.txt"), b"no photos here").unwrap();
        let (code, _, err) = ask_at(no_dcim.path(), home.path(), &["import", &into]).await;
        assert_eq!(code, NOT_FOUND);
        assert!(err.contains("/DCIM"), "{err}");

        let (code, _, _) = ask_at(
            phone.path(),
            home.path(),
            &["--storage", "999", "import", &into],
        )
        .await;
        assert_eq!(code, NOT_FOUND);

        let mut stopped = at_home(home.path());
        stopped.cancel.cancel();
        let (code, _, _) = ask_in(phone.path(), &["import", &into], &mut stopped).await;
        assert_eq!(code, CANCELLED);
        assert!(
            !import_record_folder(home.path()).exists(),
            "nothing recorded"
        );

        let (code, out, _) = ask_at(phone.path(), home.path(), &["--json", "import", &into]).await;
        assert_eq!(code, OK);
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        for key in [
            "files",
            "bytes",
            "imported_before",
            "into",
            "recorded",
            "failures",
        ] {
            assert!(answer.get(key).is_some(), "{key}: {out}");
        }
        assert_eq!(answer["recorded"], true);
        assert!(answer.get("removed").is_none(), "only with --remove");
    }

    #[test]
    fn import_reports_failures_and_a_phone_it_could_not_record() {
        let summary = dto::TransferSummary {
            files: 1,
            destination: "/Users/me/Photos".into(),
            failures: vec![dto::FileFailure {
                label: "DCIM/Camera/d.jpg".into(),
                error: dto::Error::PermissionDenied,
            }],
            ..dto::TransferSummary::default()
        };

        let mut out = Vec::new();
        let code = import_report(&summary, false, &Options::default(), &mut out);
        let printed = String::from_utf8(out).unwrap();

        assert_eq!(code, PARTLY);
        assert!(printed.contains("failed: DCIM/Camera/d.jpg: "), "{printed}");
        assert!(
            printed.contains("The phone has no serial number: nothing was recorded"),
            "{printed}"
        );
        let mut out = Vec::new();
        let json = Options {
            json: true,
            ..Options::default()
        };
        import_report(&summary, false, &json, &mut out);
        let answer: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(answer["recorded"], false);
    }

    #[test]
    fn a_folder_given_from_here_is_recorded_from_the_top() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            absolute_folder(Path::new("Photos")).unwrap(),
            text(&here.join("Photos"))
        );
        assert_eq!(
            absolute_folder(Path::new("/Users/me")).unwrap(),
            "/Users/me"
        );
    }

    fn five_photos_phone() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let camera = dir.path().join("DCIM/Camera");
        std::fs::create_dir_all(&camera).unwrap();
        for name in ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"] {
            std::fs::write(camera.join(name), format!("photo {name}")).unwrap();
        }
        dir
    }

    fn copy_folder(from: &Path, into: &Path) {
        std::fs::create_dir_all(into).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = into.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_folder(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn backup_of(phone: &Path, folders: &[&str]) -> tempfile::TempDir {
        let backup = tempfile::tempdir().unwrap();
        for folder in folders {
            let name = Path::new(folder).file_name().unwrap();
            copy_folder(&phone.join(folder), &backup.path().join(name));
        }
        backup
    }

    fn everything_in(folder: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                found.extend(everything_in(&entry.path()));
            }
            found.push((entry.path(), metadata.len(), metadata.modified().unwrap()));
        }
        found.sort();
        found
    }

    #[test]
    fn check_and_its_paths_are_understood() {
        let (_, command) = parse(&args(&["check", "/DCIM/Camera", "/Music/", "Backup"])).unwrap();
        assert_eq!(
            command,
            Command::Check {
                from: vec!["/DCIM/Camera".into(), "/Music".into()],
                into: "Backup".into()
            }
        );
        for (words, said) in [
            (vec!["check", "DCIM", "Backup"], "starts at the top"),
            (vec!["check", "Backup"], "not a command"),
            (vec!["--dry-run", "check", "/DCIM", "Backup"], "--dry-run"),
        ] {
            let problem = parse(&args(&words)).unwrap_err();
            assert!(problem.contains(said), "{words:?}: {problem}");
        }
        assert!(USAGE.contains("check <phone path>... <Mac folder>"));
    }

    #[tokio::test]
    async fn a_check_of_identical_copies_says_so_and_exits_0() {
        let phone = five_photos_phone();
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);

        let (code, out, err) = ask(
            phone.path(),
            &["check", "/DCIM/Camera", &text(backup.path())],
        )
        .await;

        assert_eq!(code, OK, "{err}");
        assert_eq!(
            out,
            format!("All 5 files identical to {}\n", text(backup.path()))
        );
        assert!(err.contains("100%"), "progress on stderr: {err}");
    }

    #[tokio::test]
    async fn a_check_names_each_file_that_differs_or_is_missing_and_exits_1() {
        let phone = five_photos_phone();
        let camera = phone.path().join("DCIM/Camera");
        std::fs::write(camera.join("b.jpg"), vec![0u8; 2_097_152]).unwrap();
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);
        let mut changed = vec![0u8; 2_097_152];
        changed[1_048_576] = 1;
        std::fs::write(backup.path().join("Camera/b.jpg"), changed).unwrap();
        std::fs::remove_file(backup.path().join("Camera/c.jpg")).unwrap();
        let phone_before = everything_in(phone.path());
        let mac_before = everything_in(backup.path());

        let (code, out, _) = ask(
            phone.path(),
            &["check", "/DCIM/Camera", &text(backup.path())],
        )
        .await;

        assert_eq!(code, PARTLY);
        assert!(
            out.contains("differs at byte 1,048,577: Camera/b.jpg\n"),
            "{out}"
        );
        assert!(out.contains("not on the Mac: Camera/c.jpg\n"), "{out}");
        assert!(
            out.ends_with(&format!(
                "Checked 5 files against {}: 3 identical, 1 differs, 1 on one side only\n",
                text(backup.path())
            )),
            "{out}"
        );
        assert_eq!(
            everything_in(phone.path()),
            phone_before,
            "nothing changed on the phone"
        );
        assert_eq!(
            everything_in(backup.path()),
            mac_before,
            "nothing changed on the Mac"
        );
    }

    #[tokio::test]
    async fn a_check_compares_sizes_first() {
        let phone = five_photos_phone();
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);
        std::fs::write(backup.path().join("Camera/e.jpg"), b"photo e.jpg, edited").unwrap();

        let (code, out, _) = ask(
            phone.path(),
            &["check", "/DCIM/Camera", &text(backup.path())],
        )
        .await;

        assert_eq!(code, PARTLY);
        assert!(
            out.contains("sizes differ: Camera/e.jpg (phone 11 B, Mac 19 B)\n"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn a_check_of_two_folders_counts_both() {
        let phone = five_photos_phone();
        std::fs::create_dir_all(phone.path().join("Music")).unwrap();
        std::fs::write(phone.path().join("Music/song.mp3"), b"la la").unwrap();
        let backup = backup_of(phone.path(), &["DCIM/Camera", "Music"]);

        let (code, out, _) = ask(
            phone.path(),
            &["check", "/DCIM/Camera", "/Music", &text(backup.path())],
        )
        .await;

        assert_eq!(code, OK);
        assert_eq!(
            out,
            format!("All 6 files identical to {}\n", text(backup.path()))
        );
    }

    fn report_line(path: &str, outcome: dto::FileOutcome) -> dto::FileReport {
        dto::FileReport {
            path: path.to_owned(),
            from: None,
            outcome,
            original: None,
        }
    }

    #[test]
    fn a_checks_report_words_every_verdict_in_text_and_json() {
        let summary = dto::TransferSummary {
            compared: Some(dto::Compared {
                identical: 1,
                differ: 2,
                missing: 2,
                unreadable: 1,
            }),
            report: vec![
                report_line("Camera/a.jpg", dto::FileOutcome::Identical),
                report_line("Camera/b.jpg", dto::FileOutcome::Differs { at: 1_048_576 }),
                report_line(
                    "Camera/e.jpg",
                    dto::FileOutcome::SizesDiffer {
                        phone: 2_100_000,
                        mac: 2_000_000,
                    },
                ),
                report_line("Camera/c.jpg", dto::FileOutcome::NotOnMac),
                report_line("z.jpg", dto::FileOutcome::NotOnPhone),
                report_line(
                    "Camera/f.jpg",
                    dto::FileOutcome::Failed(dto::Error::PermissionDenied),
                ),
            ],
            ..dto::TransferSummary::default()
        };

        let mut out = Vec::new();
        let code = check_report(&summary, "/Volumes/Backup", &Options::default(), &mut out);
        let printed = String::from_utf8(out).unwrap();

        assert_eq!(code, PARTLY);
        assert!(
            !printed.contains("Camera/a.jpg"),
            "identical files are not listed: {printed}"
        );
        for line in [
            "differs at byte 1,048,577: Camera/b.jpg",
            "sizes differ: Camera/e.jpg (phone 2.1 MB, Mac 2.0 MB)",
            "not on the Mac: Camera/c.jpg",
            "not on the phone: z.jpg",
            &format!(
                "could not read: Camera/f.jpg: {}",
                dto::Error::PermissionDenied
            ),
            "Checked 6 files against /Volumes/Backup: 1 identical, 2 differ, 2 on one side only, 1 could not be read",
        ] {
            assert!(printed.contains(line), "{line}\n{printed}");
        }

        let json = Options {
            json: true,
            ..Options::default()
        };
        let mut out = Vec::new();
        check_report(&summary, "/Volumes/Backup", &json, &mut out);
        let answer: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(answer["identical"], 1);
        assert_eq!(answer["differ"], 2);
        assert_eq!(answer["missing"], 2);
        assert_eq!(answer["unreadable"], 1);
        assert_eq!(answer["against"], "/Volumes/Backup");
        let listed = answer["files"].as_array().unwrap();
        assert_eq!(listed.len(), 5, "identical files are not listed");
        assert_eq!(
            listed[0],
            json!({"path": "Camera/b.jpg", "outcome": "differs", "at": 1_048_576})
        );
        assert_eq!(
            listed[1],
            json!({"path": "Camera/e.jpg", "outcome": "sizes_differ", "phone": 2_100_000, "mac": 2_000_000})
        );
        assert_eq!(
            listed[2],
            json!({"path": "Camera/c.jpg", "outcome": "not_on_mac"})
        );
        assert_eq!(
            listed[3],
            json!({"path": "z.jpg", "outcome": "not_on_phone"})
        );
        assert_eq!(listed[4]["outcome"], "unreadable");
        assert!(listed[4]["error"].is_string());

        let all_same = dto::TransferSummary {
            compared: Some(dto::Compared {
                identical: 5_000,
                ..dto::Compared::default()
            }),
            ..dto::TransferSummary::default()
        };
        let mut out = Vec::new();
        assert_eq!(
            check_report(&all_same, "/B", &Options::default(), &mut out),
            OK
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "All 5,000 files identical to /B\n"
        );
    }

    #[tokio::test]
    async fn a_check_says_what_it_could_not_find() {
        let phone = five_photos_phone();
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);
        let into = text(backup.path());

        let (code, _, _) = ask(phone.path(), &["check", "/Nowhere/x", &into]).await;
        assert_eq!(code, NOT_FOUND, "a phone folder that isn't there");

        let (code, out, _) = ask(phone.path(), &["check", "/DCIM/Camera/zzz.jpg", &into]).await;
        assert_eq!(code, PARTLY, "{out}");
        assert!(out.contains("not on the phone: zzz.jpg"), "{out}");

        let (code, _, _) = ask(
            phone.path(),
            &[
                "check",
                "/DCIM/Camera",
                &text(&backup.path().join("missing")),
            ],
        )
        .await;
        assert_eq!(code, NOT_FOUND, "a Mac folder that isn't there");

        let (code, _, _) = ask(
            phone.path(),
            &["--storage", "999", "check", "/DCIM/Camera", &into],
        )
        .await;
        assert_eq!(code, NOT_FOUND);

        let mut stopped = world();
        stopped.cancel.cancel();
        let (code, _, _) = ask_in(
            phone.path(),
            &["check", "/DCIM/Camera", &into],
            &mut stopped,
        )
        .await;
        assert_eq!(code, CANCELLED);
    }

    #[tokio::test]
    async fn a_check_of_5000_files_names_the_one_that_differs() {
        let phone = tempfile::tempdir().unwrap();
        let camera = phone.path().join("DCIM/Camera");
        std::fs::create_dir_all(&camera).unwrap();
        for number in 0..5_000 {
            std::fs::write(
                camera.join(format!("IMG_{number:04}.jpg")),
                format!("photo {number:04}"),
            )
            .unwrap();
        }
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);
        let into = text(backup.path());

        let (code, out, _) = ask(phone.path(), &["check", "/DCIM/Camera", &into]).await;
        assert_eq!(code, OK, "{out}");
        assert_eq!(out, format!("All 5,000 files identical to {into}\n"));

        std::fs::write(backup.path().join("Camera/IMG_2718.jpg"), b"photo 2719").unwrap();
        let (code, out, _) = ask(phone.path(), &["check", "/DCIM/Camera", &into]).await;
        assert_eq!(code, PARTLY);
        assert!(
            out.contains("differs at byte 10: Camera/IMG_2718.jpg"),
            "{out}"
        );
    }

    struct Mirrors {
        phone: tempfile::TempDir,
        home: tempfile::TempDir,
        mac: tempfile::TempDir,
        storage: (u64, String),
    }

    impl Mirrors {
        fn photos_on_mac(&self) -> PathBuf {
            self.mac.path().join("Phone photos")
        }

        fn music_on_mac(&self) -> PathBuf {
            self.mac.path().join("Music")
        }

        fn saved(
            &self,
            phone_folder: &str,
            mac_folder: &Path,
            direction: dto::MirrorDirection,
            remove_extras: bool,
        ) -> dto::SavedMirror {
            dto::SavedMirror {
                phone: dto::PinPhone {
                    serial: fake_phone_serial(self.phone.path()),
                    storage_id: self.storage.0,
                    storage_name: self.storage.1.clone(),
                },
                phone_folder: phone_folder.to_owned(),
                mac_folder: text(mac_folder),
                direction,
                remove_extras,
            }
        }

        fn keep(&self, mirrors: &[dto::SavedMirror]) {
            places_service(self.home.path())
                .save_mirrors(mirrors)
                .unwrap();
        }

        async fn ask(&self, words: &[&str]) -> (i32, String, String) {
            ask_at(self.phone.path(), self.home.path(), words).await
        }

        async fn ask_in(&self, words: &[&str], world: &mut World) -> (i32, String, String) {
            world.home = self.home.path().to_path_buf();
            ask_in(self.phone.path(), words, world).await
        }
    }

    fn places_service(home: &Path) -> internal::service::places::Service {
        internal::service::places::Service::new(
            app_folder(home).join("places.json"),
            home.to_path_buf(),
            home.join("Volumes"),
        )
    }

    async fn storage_of(phone: &Path) -> (u64, String) {
        let (_, out, _) = ask(phone, &["--json", "storages"]).await;
        let storages: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        (
            storages[0]["id"].as_u64().unwrap(),
            storages[0]["name"].as_str().unwrap().to_owned(),
        )
    }

    async fn two_saved_mirrors() -> Mirrors {
        let phone = camera_phone();
        std::fs::create_dir_all(phone.path().join("Music")).unwrap();
        std::fs::write(phone.path().join("Music/song.mp3"), b"la la").unwrap();
        std::fs::write(phone.path().join("Music/extra.mp3"), b"only on the phone").unwrap();
        let storage = storage_of(phone.path()).await;
        let mirrors = Mirrors {
            phone,
            home: tempfile::tempdir().unwrap(),
            mac: tempfile::tempdir().unwrap(),
            storage,
        };
        std::fs::create_dir_all(mirrors.photos_on_mac()).unwrap();
        std::fs::write(mirrors.photos_on_mac().join("old.jpg"), b"only on the Mac").unwrap();
        std::fs::create_dir_all(mirrors.music_on_mac()).unwrap();
        std::fs::write(mirrors.music_on_mac().join("song.mp3"), b"la la").unwrap();
        std::fs::write(mirrors.music_on_mac().join("new.mp3"), b"new song").unwrap();
        let another_phone = dto::SavedMirror {
            phone: dto::PinPhone {
                serial: "someone-else".into(),
                ..mirrors
                    .saved("/", Path::new("/"), dto::MirrorDirection::ToMac, false)
                    .phone
            },
            ..mirrors.saved(
                "/DCIM",
                &mirrors.mac.path().join("Theirs"),
                dto::MirrorDirection::ToMac,
                false,
            )
        };
        mirrors.keep(&[
            mirrors.saved(
                "/DCIM/Camera",
                &mirrors.photos_on_mac(),
                dto::MirrorDirection::ToMac,
                false,
            ),
            another_phone,
            mirrors.saved(
                "/Music",
                &mirrors.music_on_mac(),
                dto::MirrorDirection::ToPhone,
                true,
            ),
        ]);
        mirrors
    }

    fn asked_and_answering(answer: Option<bool>) -> (World, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&asked);
        let world = World {
            ask: Box::new(move |question| {
                heard.lock().unwrap().push(question.to_owned());
                answer
            }),
            ..world()
        };
        (world, asked)
    }

    fn recording_trash(bin: &Path) -> Trash {
        let bin = bin.to_path_buf();
        Arc::new(move |path: &Path| std::fs::rename(path, bin.join(path.file_name().unwrap())))
    }

    fn holds_file_named(folder: &Path, name: &str) -> bool {
        everything_in(folder)
            .iter()
            .any(|(path, _, _)| path.file_name().is_some_and(|file| file == name))
    }

    #[test]
    fn mirrors_and_mirror_are_understood() {
        assert_eq!(parse(&args(&["mirrors"])).unwrap().1, Command::Mirrors);
        assert_eq!(
            parse(&args(&["mirror", "2"])).unwrap().1,
            Command::Mirror(Choice::Number(2))
        );
        assert_eq!(
            parse(&args(&["mirror", "Camera → Phone photos"]))
                .unwrap()
                .1,
            Command::Mirror(Choice::Label("Camera → Phone photos".into()))
        );
        let (options, _) = parse(&args(&["mirror", "1", "--dry-run", "--yes"])).unwrap();
        assert!(options.dry_run && options.yes);
        for (words, said) in [
            (vec!["mirror", "0"], "start at 1"),
            (vec!["mirror"], "not a command"),
            (vec!["mirrors", "1"], "not a command"),
        ] {
            let problem = parse(&args(&words)).unwrap_err();
            assert!(problem.contains(said), "{words:?}: {problem}");
        }
        assert!(USAGE.contains("mirrors "));
        assert!(USAGE.contains("mirror <number | label>"));
    }

    #[tokio::test]
    async fn mirrors_lists_this_phones_saved_mirrors_numbered() {
        let mirrors = two_saved_mirrors().await;

        let (code, out, err) = mirrors.ask(&["mirrors"]).await;

        assert_eq!(code, OK, "{err}");
        assert_eq!(
            out,
            "1  Camera → Phone photos  to the Mac\n2  Music → Music  to the phone, removes extras\n"
        );
        let (_, out, _) = mirrors.ask(&["--json", "mirrors"]).await;
        let listed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);
        assert_eq!(
            listed[1],
            json!({
                "number": 2,
                "label": "Music → Music",
                "direction": "to_phone",
                "removes_extras": true,
                "phone_folder": "/Music",
                "mac_folder": text(&mirrors.music_on_mac()),
                "storage": mirrors.storage.1,
            })
        );

        let nobody = tempfile::tempdir().unwrap();
        let (code, out, _) = ask_at(mirrors.phone.path(), nobody.path(), &["mirrors"]).await;
        assert_eq!(
            (code, out.as_str()),
            (OK, "No saved mirrors for this phone\n")
        );
    }

    #[tokio::test]
    async fn a_mirror_that_removes_nothing_runs_without_asking() {
        let mirrors = two_saved_mirrors().await;
        let places_before =
            std::fs::read(app_folder(mirrors.home.path()).join("places.json")).unwrap();
        let (mut world, asked) = asked_and_answering(None);

        let (code, out, err) = mirrors.ask_in(&["mirror", "1"], &mut world).await;

        assert_eq!(code, OK, "{err}");
        assert!(
            out.starts_with("Mirrored Camera → Phone photos: 3 copied"),
            "{out}"
        );
        for name in ["a.jpg", "b.mp4", "notes.txt", "old.jpg"] {
            assert!(mirrors.photos_on_mac().join(name).exists(), "{name}");
        }
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read(app_folder(mirrors.home.path()).join("places.json")).unwrap(),
            places_before,
            "the saved mirrors are never changed"
        );
    }

    #[tokio::test]
    async fn a_mirror_that_removes_asks_first_and_no_changes_nothing() {
        let mirrors = two_saved_mirrors().await;
        let (mut world, asked) = asked_and_answering(Some(false));

        let (code, out, _) = mirrors.ask_in(&["mirror", "2"], &mut world).await;

        assert_eq!((code, out.as_str()), (OK, "Nothing was changed.\n"));
        let (mut world, _) = asked_and_answering(Some(false));
        let (_, out, _) = mirrors.ask_in(&["--json", "mirror", "2"], &mut world).await;
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            answer,
            json!({ "mirror": "Music → Music", "changed": false })
        );
        assert_eq!(
            *asked.lock().unwrap(),
            ["Remove 1 item that is only on the phone in /Music (deleted for good)? [y/N] "]
        );
        assert!(mirrors.phone.path().join("Music/extra.mp3").exists());
        assert!(!mirrors.phone.path().join("Music/new.mp3").exists());
    }

    #[tokio::test]
    async fn a_mirror_that_removes_is_refused_in_a_script_without_yes() {
        let mirrors = two_saved_mirrors().await;

        let (code, _, err) = mirrors.ask(&["mirror", "2"]).await;

        assert_eq!(code, USAGE_ERROR);
        assert!(
            err.contains("mirror 2 removes 1 item: add --yes to do it without asking"),
            "{err}"
        );
        assert!(mirrors.phone.path().join("Music/extra.mp3").exists());
        assert!(!mirrors.phone.path().join("Music/new.mp3").exists());

        std::fs::remove_file(mirrors.phone.path().join("Music/extra.mp3")).unwrap();
        let (code, _, _) = mirrors.ask(&["mirror", "2"]).await;
        assert_eq!(
            code, USAGE_ERROR,
            "refused even with nothing to remove today"
        );
        assert!(!mirrors.phone.path().join("Music/new.mp3").exists());

        let (code, out, _) = mirrors.ask(&["--json", "mirror", "2"]).await;
        assert_eq!(code, USAGE_ERROR);
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            answer["error"],
            "mirror 2 removes 0 items: add --yes to do it without asking"
        );
    }

    #[tokio::test]
    async fn a_mirror_with_yes_removes_the_extras_on_either_side() {
        let mirrors = two_saved_mirrors().await;
        mirrors.keep(&[
            mirrors.saved(
                "/DCIM/Camera",
                &mirrors.photos_on_mac(),
                dto::MirrorDirection::ToMac,
                true,
            ),
            mirrors.saved(
                "/Music",
                &mirrors.music_on_mac(),
                dto::MirrorDirection::ToPhone,
                true,
            ),
        ]);
        let bin = tempfile::tempdir().unwrap();
        let mut world = World {
            trash: Some(recording_trash(bin.path())),
            ..world()
        };

        let (code, out, err) = mirrors.ask_in(&["--yes", "mirror", "1"], &mut world).await;
        assert_eq!(code, OK, "{err}");
        assert!(out.contains("1 removed"), "{out}");
        assert!(!mirrors.photos_on_mac().join("old.jpg").exists());
        assert!(
            holds_file_named(bin.path(), "old.jpg"),
            "in the (test's) Trash"
        );

        let (code, _, err) = mirrors.ask_in(&["--yes", "mirror", "2"], &mut world).await;
        assert_eq!(code, OK, "{err}");
        assert!(!mirrors.phone.path().join("Music/extra.mp3").exists());
        assert!(mirrors.phone.path().join("Music/new.mp3").exists());
    }

    #[tokio::test]
    async fn a_mirror_is_chosen_by_its_label_and_a_wrong_choice_changes_nothing() {
        let mirrors = two_saved_mirrors().await;

        let (code, _, err) = mirrors.ask(&["mirror", "Camera → Phone photos"]).await;
        assert_eq!(code, OK, "{err}");
        assert!(mirrors.photos_on_mac().join("a.jpg").exists());

        let (code, _, err) = mirrors.ask(&["mirror", "Nowhere → Nothing"]).await;
        assert_eq!(code, NOT_FOUND);
        assert!(
            err.contains("no saved mirror \"Nowhere → Nothing\" for this phone"),
            "{err}"
        );
        let (code, _, err) = mirrors.ask(&["mirror", "9"]).await;
        assert_eq!(code, NOT_FOUND);
        assert!(err.contains("no saved mirror 9 for this phone"), "{err}");
        let (code, out, _) = mirrors.ask(&["--json", "mirror", "9"]).await;
        assert_eq!(code, NOT_FOUND);
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(answer["error"], "no saved mirror 9 for this phone");

        let elsewhere = mirrors.mac.path().join("Other/Phone photos");
        std::fs::create_dir_all(&elsewhere).unwrap();
        mirrors.keep(&[
            mirrors.saved(
                "/DCIM/Camera",
                &mirrors.photos_on_mac(),
                dto::MirrorDirection::ToMac,
                false,
            ),
            mirrors.saved(
                "/DCIM/Camera",
                &elsewhere,
                dto::MirrorDirection::ToMac,
                false,
            ),
        ]);
        let (code, _, err) = mirrors.ask(&["mirror", "Camera → Phone photos"]).await;
        assert_eq!(code, USAGE_ERROR);
        assert!(err.contains("use its number"), "{err}");
        assert!(!elsewhere.join("a.jpg").exists());
    }

    #[tokio::test]
    async fn a_dry_run_mirror_says_what_it_would_do_and_asks_nothing() {
        let mirrors = two_saved_mirrors().await;
        let (mut world, asked) = asked_and_answering(Some(true));

        let (code, out, err) = mirrors
            .ask_in(&["--dry-run", "mirror", "2"], &mut world)
            .await;

        assert_eq!(code, OK, "{err}");
        assert_eq!(out, "Music → Music would copy 1 file (8 B), remove 1\n");
        assert!(asked.lock().unwrap().is_empty());
        assert!(mirrors.phone.path().join("Music/extra.mp3").exists());
        assert!(!mirrors.phone.path().join("Music/new.mp3").exists());
        let (_, out, _) = mirrors.ask(&["--json", "--dry-run", "mirror", "2"]).await;
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(answer["new_files"], 1);
        assert_eq!(answer["extras"], 1);
    }

    #[tokio::test]
    async fn a_mirror_refuses_what_the_app_would_never_save() {
        let mirrors = two_saved_mirrors().await;
        for top in [mirrors.home.path(), Path::new("/")] {
            mirrors.keep(&[mirrors.saved("/DCIM/Camera", top, dto::MirrorDirection::ToMac, true)]);
            let (code, _, err) = mirrors.ask(&["--yes", "mirror", "1"]).await;
            assert_eq!(code, USAGE_ERROR, "{}", text(top));
            assert!(err.contains("home folder"), "{err}");
        }

        let mut gone = mirrors.saved(
            "/DCIM/Camera",
            &mirrors.photos_on_mac(),
            dto::MirrorDirection::ToMac,
            false,
        );
        gone.phone.storage_id = 999;
        gone.phone.storage_name = "Elsewhere".into();
        mirrors.keep(&[gone]);
        let (_, out, _) = mirrors.ask(&["mirrors"]).await;
        assert_eq!(out, "No saved mirrors for this phone\n");
        let (code, _, _) = mirrors.ask(&["mirror", "1"]).await;
        assert_eq!(code, NOT_FOUND, "its storage isn't on the phone");

        mirrors.keep(&[mirrors.saved(
            "/DCIM/Camera",
            &mirrors.mac.path().join("Gone"),
            dto::MirrorDirection::ToMac,
            false,
        )]);
        let (code, _, _) = mirrors.ask(&["mirror", "1"]).await;
        assert_eq!(code, NOT_FOUND, "its Mac folder is gone");

        mirrors.keep(&[mirrors.saved(
            "/DCIM/Camera",
            &mirrors.photos_on_mac(),
            dto::MirrorDirection::ToMac,
            false,
        )]);
        let (code, _, err) = mirrors.ask(&["--storage", "999", "mirror", "1"]).await;
        assert_eq!(code, OK, "the saved storage, not --storage: {err}");

        mirrors.keep(&[mirrors.saved(
            "/Music",
            &mirrors.music_on_mac(),
            dto::MirrorDirection::ToPhone,
            true,
        )]);
        let mut stopped = world();
        stopped.cancel.cancel();
        let (code, _, _) = mirrors
            .ask_in(&["--yes", "mirror", "1"], &mut stopped)
            .await;
        assert_eq!(code, CANCELLED);
        assert!(
            mirrors.phone.path().join("Music/extra.mp3").exists(),
            "nothing removed"
        );
    }

    #[test]
    fn import_says_nothing_new_and_counts_what_came_before() {
        let nothing = dto::TransferSummary::default();
        assert_eq!(import_headline(&nothing), "Nothing new to import");
        let some = dto::TransferSummary {
            files: 2,
            bytes: 12_400_000,
            already_there: 3,
            destination: "/Users/me/Photos".into(),
            ..dto::TransferSummary::default()
        };
        assert_eq!(
            import_headline(&some),
            "Imported 2 files (12.4 MB) to /Users/me/Photos, 3 imported before"
        );

        let dry_run = |new_files, new_bytes, already| {
            let mut out = Vec::new();
            let preview = dto::ImportPreview {
                new_files,
                new_bytes,
                already,
            };
            import_dry_run_report(&preview, &Options::default(), &mut out);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(dry_run(0, 0, 0), "Nothing new to import\n");
        assert_eq!(dry_run(1, 4, 0), "1 new file (4 B) to import\n");
        assert_eq!(
            dry_run(3, 8_100_000, 5),
            "3 new files (8.1 MB) to import, 5 imported before\n"
        );
    }

    fn camera_to_photos(direction: dto::MirrorDirection, remove_extras: bool) -> dto::SavedMirror {
        dto::SavedMirror {
            phone: dto::PinPhone {
                serial: "A".into(),
                storage_id: 1,
                storage_name: "Internal".into(),
            },
            phone_folder: "/DCIM/Camera".into(),
            mac_folder: "/Users/me/Phone photos".into(),
            direction,
            remove_extras,
        }
    }

    #[test]
    fn the_question_says_where_the_extras_go() {
        let to_mac = camera_to_photos(dto::MirrorDirection::ToMac, true);
        assert_eq!(
            removal_question(&to_mac, 2),
            "Remove 2 items that are only in /Users/me/Phone photos (to the Trash)? [y/N] "
        );
        let to_phone = camera_to_photos(dto::MirrorDirection::ToPhone, true);
        assert_eq!(
            removal_question(&to_phone, 1),
            "Remove 1 item that is only on the phone in /DCIM/Camera (deleted for good)? [y/N] "
        );
    }

    #[test]
    fn a_mirrors_report_says_what_it_did_or_that_nothing_needed_doing() {
        let report = |mirrored: dto::Mirrored, failures: Vec<dto::FileFailure>, json: bool| {
            let summary = dto::TransferSummary {
                mirrored: Some(mirrored),
                failures,
                ..dto::TransferSummary::default()
            };
            let options = Options {
                json,
                ..Options::default()
            };
            let mut out = Vec::new();
            let code = mirror_report("Camera → Phone photos", &summary, &options, &mut out);
            (code, String::from_utf8(out).unwrap())
        };

        assert_eq!(
            report(dto::Mirrored::default(), vec![], false),
            (OK, "Already the same: Camera → Phone photos\n".to_owned())
        );
        let busy = dto::Mirrored {
            copied: 3,
            replaced: 1,
            removed: 4,
            kept: 1,
            conflicts: 2,
        };
        let failed = vec![dto::FileFailure {
            label: "a.jpg".into(),
            error: dto::Error::PermissionDenied,
        }];
        let (code, out) = report(busy.clone(), failed.clone(), false);
        assert_eq!(code, PARTLY);
        assert!(
            out.starts_with(
                "Mirrored Camera → Phone photos: 3 copied, 1 replaced, 4 removed, 2 conflicts\n"
            ),
            "{out}"
        );
        assert!(out.contains("failed: a.jpg: "), "{out}");
        let (_, out) = report(busy, failed, true);
        let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(answer["mirror"], "Camera → Phone photos");
        assert_eq!(answer["kept"], 1);
        assert_eq!(answer["failures"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_dry_run_mirror_names_each_kind_of_change() {
        let dry_run = |saved: &dto::SavedMirror, preview: dto::MirrorPreview| {
            let mut out = Vec::new();
            mirror_dry_run_report(saved, &preview, &Options::default(), &mut out);
            String::from_utf8(out).unwrap()
        };
        let removing = camera_to_photos(dto::MirrorDirection::ToMac, true);
        let keeping = camera_to_photos(dto::MirrorDirection::ToMac, false);

        assert_eq!(
            dry_run(&removing, dto::MirrorPreview::default()),
            "Camera → Phone photos is already the same\n"
        );
        let changed = dto::MirrorPreview {
            changed_files: 1,
            changed_bytes: 2_000_000,
            extras: 3,
            conflicts: 2,
            ..dto::MirrorPreview::default()
        };
        assert_eq!(
            dry_run(&removing, changed.clone()),
            "Camera → Phone photos would replace 1 (2.0 MB), remove 3, with 2 conflicts\n"
        );
        assert_eq!(
            dry_run(&keeping, changed),
            "Camera → Phone photos would replace 1 (2.0 MB), with 2 conflicts\n",
            "extras are left alone by a mirror that doesn't remove them"
        );
    }

    #[test]
    fn a_phone_without_a_serial_number_matches_no_saved_mirror() {
        let home = tempfile::tempdir().unwrap();
        let mut saved = camera_to_photos(dto::MirrorDirection::ToMac, false);
        saved.phone.serial = String::new();
        places_service(home.path()).save_mirrors(&[saved]).unwrap();

        assert!(mirrors_of_this_phone(home.path(), "", &[(1, "Internal".into())]).is_empty());
    }

    #[tokio::test]
    async fn a_mirror_that_removes_runs_when_the_answer_is_yes() {
        let mirrors = two_saved_mirrors().await;
        let (mut world, asked) = asked_and_answering(Some(true));

        let (code, _, err) = mirrors.ask_in(&["mirror", "2"], &mut world).await;

        assert_eq!(code, OK, "{err}");
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert!(!mirrors.phone.path().join("Music/extra.mp3").exists());
        assert!(mirrors.phone.path().join("Music/new.mp3").exists());
    }

    #[test]
    fn a_phone_without_a_serial_number_has_no_saved_mirrors() {
        let mut out = Vec::new();
        assert_eq!(
            mirrors_report(&[], false, &Options::default(), &mut out),
            OK
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "The phone has no serial number, so it has no saved mirrors\n"
        );
    }

    fn phone_of_5000_photos() -> tempfile::TempDir {
        let phone = tempfile::tempdir().unwrap();
        let camera = phone.path().join("DCIM/Camera");
        std::fs::create_dir_all(&camera).unwrap();
        for number in 0..5_000 {
            std::fs::write(
                camera.join(format!("IMG_{number:04}.jpg")),
                format!("photo {number:04}"),
            )
            .unwrap();
        }
        phone
    }

    async fn seconds_for<Work: std::future::Future>(work: Work) -> f64 {
        let started = std::time::Instant::now();
        work.await;
        started.elapsed().as_secs_f64()
    }

    async fn best_of_two<Work, Run>(mut run: Run) -> f64
    where
        Run: FnMut() -> Work,
        Work: std::future::Future,
    {
        let first = seconds_for(run()).await;
        let second = seconds_for(run()).await;
        first.min(second)
    }

    #[tokio::test]
    #[ignore = "a timing for results.md: cargo test --release -p open-mtp-client-cli -- --ignored the_tool_adds_little --nocapture"]
    async fn the_tool_adds_little_to_the_services_work() {
        let phone = phone_of_5000_photos();
        let backup = backup_of(phone.path(), &["DCIM/Camera"]);
        let check_request = dto::CheckRequest {
            storage_id: storage_of(phone.path()).await.0,
            phone_folder: "/DCIM".into(),
            mac_folder: text(backup.path()),
            names: vec!["Camera".into()],
        };

        let import_by_service = best_of_two(|| async {
            let (home, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let (transfers, storage) = transfers_on(phone.path(), home.path()).await;
            let request = import_request(storage, into.path());
            transfers
                .import(&request, &dto::Cancel::new(), |_| {})
                .await
                .unwrap();
        })
        .await;
        let import_by_tool = best_of_two(|| async {
            let (home, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let (code, _, _) =
                ask_at(phone.path(), home.path(), &["import", &text(into.path())]).await;
            assert_eq!(code, OK);
        })
        .await;

        let check_by_service = best_of_two(|| async {
            let home = tempfile::tempdir().unwrap();
            let (transfers, _) = transfers_on(phone.path(), home.path()).await;
            let summary = transfers
                .check_contents(&check_request, &dto::Cancel::new(), |_| {})
                .await
                .unwrap();
            assert_eq!(summary.compared.unwrap().identical, 5_000);
        })
        .await;
        let check_by_tool = best_of_two(|| async {
            let (code, _, _) = ask(
                phone.path(),
                &["check", "/DCIM/Camera", &text(backup.path())],
            )
            .await;
            assert_eq!(code, OK);
        })
        .await;

        let mirror_by_service = best_of_two(|| async {
            let (home, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let (transfers, storage) = transfers_on(phone.path(), home.path()).await;
            let request = dto::MirrorRequest {
                storage_id: storage,
                phone_folder: "/DCIM/Camera".into(),
                mac_folder: text(into.path()),
                direction: dto::MirrorDirection::ToMac,
                remove_extras: false,
            };
            transfers
                .mirror(&request, &dto::Cancel::new(), |_| {})
                .await
                .unwrap();
        })
        .await;
        let mirror_by_tool = best_of_two(|| async {
            let (home, into) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let storage = storage_of(phone.path()).await;
            let mirrors = dto::SavedMirror {
                phone: dto::PinPhone {
                    serial: fake_phone_serial(phone.path()),
                    storage_id: storage.0,
                    storage_name: storage.1,
                },
                phone_folder: "/DCIM/Camera".into(),
                mac_folder: text(into.path()),
                direction: dto::MirrorDirection::ToMac,
                remove_extras: false,
            };
            places_service(home.path())
                .save_mirrors(&[mirrors])
                .unwrap();
            let (code, _, _) = ask_at(phone.path(), home.path(), &["mirror", "1"]).await;
            assert_eq!(code, OK);
        })
        .await;

        for (what, by_service, by_tool) in [
            ("import", import_by_service, import_by_tool),
            ("check", check_by_service, check_by_tool),
            ("mirror", mirror_by_service, mirror_by_tool),
        ] {
            println!(
                "{what}: service {by_service:.3} s, tool {by_tool:.3} s, ratio {:.2}",
                by_tool / by_service
            );
        }
    }

    #[tokio::test]
    async fn problems_end_with_a_code_that_says_what_went_wrong() {
        let phone = phone();
        let (code, _, err) = ask(phone.path(), &["ls", "/Nowhere"]).await;
        assert_eq!(code, NOT_FOUND, "{err}");
        let (code, out, _) = ask(phone.path(), &["--json", "ls", "/Nowhere"]).await;
        assert_eq!(code, NOT_FOUND);
        assert!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["error"].is_string());
        let (code, _, _) = ask(phone.path(), &["--storage", "999", "ls"]).await;
        assert_eq!(code, NOT_FOUND);
        let (code, _, err) = ask(phone.path(), &["pull", "/", "/tmp"]).await;
        assert_eq!(code, FAILED, "{err}");
        let (code, _, err) = ask(phone.path(), &["nonsense"]).await;
        assert_eq!(code, USAGE_ERROR);
        assert!(err.contains("usage:"));
        let (code, out, _) = ask(phone.path(), &["--help"]).await;
        assert_eq!(code, OK);
        assert!(out.starts_with("usage:"));
    }
}
