//! The commands: what is asked (`parse`), and doing it (`run`) with the
//! app's own services, so a copy from the command line is checked, resumed
//! and quarantined exactly as one from the window. Output is plain lines, or
//! JSON with `--json`; the exit code says what went wrong (sysexits.h).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use internal::dto;
use internal::service::{device, explorer, transfer};
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
  -y, --yes            rm: delete without asking
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
    Help,
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
    /// `--yes`: rm doesn't ask.
    pub yes: bool,
    /// `--move`: originals go once their copies are checked.
    pub moving: bool,
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
    /// Where `push --move` puts each original; `None` refuses the move.
    pub trash: Option<Trash>,
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
        [] => return Err("no command".to_owned()),
        [word, ..] => {
            return Err(format!(
                "not a command, or the wrong arguments for it: {word}"
            ));
        }
    };
    Ok((options, command))
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
    static NEXT: AtomicU32 = AtomicU32::new(1);
    match &options.virtual_dir {
        Some(dir) => device::Source::Virtual(device::VirtualPhone::new(
            dir.clone(),
            format!(
                "cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        )),
        None => device::Source::Usb,
    }
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
    let storage = match options.storage {
        Some(id) if storages.iter().any(|storage| storage.id == id) => id,
        Some(id) => return Err(dto::Error::PathNotFound(format!("storage {id}"))),
        None => storages.first().ok_or(dto::Error::AllowStorageAccess)?.id,
    };
    let explorer = Arc::new(explorer::Service::new(Arc::clone(&device)));
    let mut transfers = transfer::Service::new(device, Arc::clone(&explorer));
    if let Some(trash) = world.trash.clone() {
        transfers = transfers.with_trash(move |path: &Path| trash(path));
    }
    let printer = Progress::new(err, world.terminal);
    let progress = |report: dto::TransferProgress| printer.show(&report);
    let cancel = world.cancel.clone();
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
        Command::Phones | Command::Storages | Command::Info | Command::Help => Ok(OK),
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

    /// A world with no one to ask, progress as lines, and no Trash.
    fn world() -> World {
        World {
            ask: Box::new(|_| None),
            terminal: false,
            cancel: dto::Cancel::new(),
            trash: None,
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
