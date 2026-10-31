//! The builder's view-model: plain state and actions, tested without a window. The GPUI
//! view only draws it and forwards clicks.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anygenesis_patch::{
    Pack, Patch,
    market::{self, Index, IndexPack, Status},
};
use anygenesis_rom::{Regions, Rom, RomError, RomWarning, Sha1Digest};

use crate::{
    progress::{self, Line, Phase, Progress},
    settings::Settings,
};

/// The three steps in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Rom,
    Patches,
    Build,
}

impl Step {
    pub const ALL: [Self; 3] = [Self::Rom, Self::Patches, Self::Build];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Rom => "ROM",
            Self::Patches => "Patches & Mods",
            Self::Build => "Build",
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// How a step shows in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Current,
    Done,
    Todo { locked: bool },
}

/// The ROM's header, as the ROM card shows it.
#[derive(Debug, Clone)]
pub struct RomInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub title: String,
    pub sha1: Sha1Digest,
    /// Label and value pairs, in the card's order.
    pub fields: Vec<(&'static str, String)>,
    pub size: String,
    pub warnings: Vec<Notice>,
}

/// A notice with a bold lead and a plain rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub lead: String,
    pub rest: String,
}

/// The ROM step's state.
#[derive(Debug, Clone)]
pub enum RomState {
    Empty,
    Loaded(RomInfo),
    /// A file AnyGenesis cannot run: a title, RomError's own text, and "name · size".
    Error {
        title: String,
        message: String,
        file: String,
    },
}

/// Where a patch in the list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    File,
    Folder,
    Market,
}

/// One patch on this computer.
#[derive(Debug, Clone)]
pub struct LocalPatch {
    pub path: PathBuf,
    pub name: String,
    pub kind: String,
    pub origin: Origin,
    pub on: bool,
    /// Whether the pack carries C code, which needs the trust notice.
    pub runs_code: bool,
}

impl LocalPatch {
    pub fn meta(&self) -> String {
        let origin = match self.origin {
            Origin::File => "from file",
            Origin::Folder => "in the patches folder",
            Origin::Market => "from the Marketplace",
        };
        format!("{} \u{b7} {origin}", self.kind)
    }
}

/// The Patches step's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Patches,
    Mods,
}

/// Which pane a narrow window shows on the Patches tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Local,
    Marketplace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Settings,
    About,
    /// The install confirmation for [`Model::confirm`].
    Install,
}

/// The Build step's state.
#[derive(Debug, Clone)]
pub enum Build {
    Ready,
    Running {
        progress: Progress,
        started: Instant,
    },
    Done {
        exe: PathBuf,
        took: Duration,
    },
    Failed {
        phase: Phase,
        took: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub text: String,
    pub error: bool,
}

/// What the primary button in the bottom bar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAction {
    Go(Step),
    Build,
    Play,
}

/// The primary button: its label, and its action when enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    pub label: &'static str,
    pub action: Option<NextAction>,
}

/// The Marketplace's index for this run.
#[derive(Debug, Clone)]
pub enum Market {
    /// Not asked for yet: the index is fetched when the Patches step first opens.
    Idle,
    Loading,
    /// Why the index could not be fetched or read; installed packs still work.
    Offline(String),
    Ready(Index),
}

/// One Marketplace pack for the loaded ROM.
#[derive(Debug, Clone)]
pub struct MarketEntry {
    pub pack: IndexPack,
    pub status: Status,
}

#[derive(Debug)]
pub struct Model {
    pub step: Step,
    pub rom: RomState,
    pub patches: Vec<LocalPatch>,
    pub market: Market,
    /// Where the Marketplace installs packs; none when the per-user folder is unknown.
    pub market_root: Option<PathBuf>,
    /// The pack whose install is waiting for the user's yes.
    pub confirm: Option<IndexPack>,
    /// The id of the pack being installed.
    pub installing: Option<String>,
    /// Why the last install or removal failed.
    pub market_error: Option<String>,
    pub tab: Tab,
    pub pane: Pane,
    pub build: Build,
    pub log: Vec<LogLine>,
    pub log_open: bool,
    pub overlay: Option<Overlay>,
    /// Why the last patch file could not be added.
    pub patch_error: Option<String>,
    pub settings: Settings,
}

impl Model {
    pub fn new(settings: Settings) -> Self {
        Self {
            step: Step::Rom,
            rom: RomState::Empty,
            patches: Vec::new(),
            market: Market::Idle,
            market_root: None,
            confirm: None,
            installing: None,
            market_error: None,
            tab: Tab::Patches,
            pane: Pane::Local,
            build: Build::Ready,
            log: Vec::new(),
            log_open: true,
            overlay: None,
            patch_error: None,
            settings,
        }
    }

    pub fn rom(&self) -> Option<&RomInfo> {
        match &self.rom {
            RomState::Loaded(info) => Some(info),
            _ => None,
        }
    }

    pub fn is_building(&self) -> bool {
        matches!(self.build, Build::Running { .. })
    }

    /// Loads the ROM at `path`, with the packs for it found under `folders` listed, and goes
    /// back to the ROM step. Ignored while a build runs.
    pub fn load_rom(&mut self, path: &Path, folders: &[(PathBuf, Origin)]) {
        if self.is_building() {
            return;
        }
        self.step = Step::Rom;
        self.patches.clear();
        self.patch_error = None;
        self.market_error = None;
        self.confirm = None;
        self.build = Build::Ready;
        self.log.clear();
        self.rom = match Rom::load(path) {
            Ok(rom) => {
                self.patches = scan(folders, rom.sha1());
                RomState::Loaded(rom_info(path, &rom))
            }
            Err(error) => rom_error(path, &error),
        };
    }

    /// Lists the packs under `folders` again after an install or a removal, keeping the
    /// added files, the order, and which patches are on; new packs go last, on.
    pub fn rescan(&mut self, folders: &[(PathBuf, Origin)]) {
        let Some(rom) = self.rom().map(|rom| rom.sha1) else {
            return;
        };
        let mut found = scan(folders, rom);
        let mut patches = Vec::new();
        for patch in self.patches.drain(..) {
            if patch.origin == Origin::File {
                patches.push(patch);
            } else if let Some(at) = found.iter().position(|new| new.path == patch.path) {
                let mut new = found.remove(at);
                new.on = patch.on;
                patches.push(new);
            }
        }
        patches.extend(found);
        self.patches = patches;
    }

    /// The Marketplace's packs for the loaded ROM, in index order.
    pub fn market_entries(&self) -> Vec<MarketEntry> {
        let (Market::Ready(index), Some(rom)) = (&self.market, self.rom()) else {
            return Vec::new();
        };
        index
            .for_rom(rom.sha1)
            .into_iter()
            .map(|pack| MarketEntry {
                pack: pack.clone(),
                status: self
                    .market_root
                    .as_deref()
                    .map_or(Status::Absent, |root| market::status(root, pack)),
            })
            .collect()
    }

    /// Steps after the first open once a ROM is loaded.
    pub fn can_go(&self, step: Step) -> bool {
        step == Step::Rom || self.rom().is_some()
    }

    pub fn go(&mut self, step: Step) {
        if self.can_go(step) {
            self.step = step;
            self.overlay = None;
        }
    }

    pub fn back(&mut self) {
        let previous = match self.step {
            Step::Rom | Step::Patches => Step::Rom,
            Step::Build => Step::Patches,
        };
        self.go(previous);
    }

    pub fn step_state(&self, step: Step) -> StepState {
        let loaded = self.rom().is_some();
        if step == self.step {
            StepState::Current
        } else if step < self.step || (step == Step::Rom && loaded) {
            StepState::Done
        } else {
            StepState::Todo { locked: !loaded }
        }
    }

    pub fn enabled_patches(&self) -> impl Iterator<Item = &LocalPatch> {
        self.patches.iter().filter(|patch| patch.on)
    }

    /// The apply-order number of the patch at `index`, if it is on.
    pub fn order(&self, index: usize) -> Option<usize> {
        let patch = self.patches.get(index)?;
        patch.on.then(|| {
            self.patches[..index]
                .iter()
                .filter(|other| other.on)
                .count()
                + 1
        })
    }

    /// Adds the patch file at `path` to the end of the list, unless it is listed already.
    pub fn add_patch(&mut self, path: &Path) {
        let Some(rom) = self.rom() else {
            return;
        };
        if self.patches.iter().any(|patch| patch.path == path) {
            self.patch_error = None;
            return;
        }
        match local_patch(path, rom.sha1, Origin::File) {
            Ok(patch) => {
                self.patches.push(patch);
                self.patch_error = None;
            }
            Err(message) => self.patch_error = Some(message),
        }
    }

    pub fn toggle_patch(&mut self, index: usize) {
        if let Some(patch) = self.patches.get_mut(index) {
            patch.on = !patch.on;
        }
    }

    pub fn remove_patch(&mut self, index: usize) {
        if index < self.patches.len() {
            self.patches.remove(index);
        }
    }

    /// Moves the patch at `from` to `to`, shifting the ones between.
    pub fn move_patch(&mut self, from: usize, to: usize) {
        if from != to && from < self.patches.len() && to < self.patches.len() {
            let patch = self.patches.remove(from);
            self.patches.insert(to, patch);
        }
    }

    /// What the Build step's summary says about the patches.
    pub fn patch_summary(&self) -> String {
        let names: Vec<&str> = self.enabled_patches().map(|p| p.name.as_str()).collect();
        if names.is_empty() {
            "No patches, original game".into()
        } else {
            names.join(", then ")
        }
    }

    /// The game program's file name, as anygenesis names it.
    pub fn exe_name(&self) -> String {
        let Some(rom) = self.rom() else {
            return String::new();
        };
        let names: Vec<&str> = self.enabled_patches().map(|p| p.name.as_str()).collect();
        exe_name(&rom.path, &names)
    }

    /// The arguments for `anygenesis`: the ROM, the progress lines, the patches in order,
    /// and the output folder.
    pub fn build_arguments(&self) -> Vec<OsString> {
        let Some(rom) = self.rom() else {
            return Vec::new();
        };
        let mut arguments = vec![rom.path.clone().into_os_string(), "--progress".into()];
        for patch in self.enabled_patches() {
            arguments.push("--patch".into());
            arguments.push(patch.path.clone().into_os_string());
        }
        arguments.push("--out".into());
        arguments.push(self.settings.games_folder().into_os_string());
        arguments
    }

    /// Starts a build at `now`.
    pub fn start_build(&mut self, now: Instant) {
        if self.rom().is_some() && !self.is_building() {
            self.build = Build::Running {
                progress: Progress::default(),
                started: now,
            };
            self.log.clear();
            self.log_open = true;
        }
    }

    /// Stops the running build; the view kills the child.
    pub fn cancel_build(&mut self) {
        if self.is_building() {
            self.build = Build::Ready;
            self.log.clear();
        }
    }

    /// Takes one output line of the running build at `now`.
    pub fn on_line(&mut self, line: &str, now: Instant) {
        let Build::Running { progress, started } = &mut self.build else {
            return;
        };
        let elapsed = now.saturating_duration_since(*started);
        match Line::parse(line) {
            Line::Phase(phase) => progress.enter(phase, elapsed),
            Line::Compiled { done, total } => progress.compiled(done, total, elapsed),
            Line::Done(exe) => {
                self.build = Build::Done {
                    exe: PathBuf::from(exe),
                    took: elapsed,
                };
            }
            Line::Error(message) => self.log.push(LogLine {
                text: message,
                error: true,
            }),
            Line::Text(text) if !text.trim().is_empty() => {
                self.log.push(LogLine { text, error: false });
            }
            Line::Text(_) => {}
        }
    }

    /// The build's process ended at `now`; a build that never reported its program failed.
    pub fn on_exit(&mut self, code: Option<i32>, now: Instant) {
        if let Build::Running { progress, started } = &self.build {
            let took = now.saturating_duration_since(*started);
            if !self.log.iter().any(|line| line.error) {
                let text = match code {
                    Some(code) => format!("anygenesis stopped with exit code {code}"),
                    None => "anygenesis was stopped".to_owned(),
                };
                self.log.push(LogLine { text, error: true });
            }
            self.build = Build::Failed {
                phase: progress.phase,
                took,
            };
        }
    }

    /// The failure card's text.
    pub fn failure_text(&self) -> Option<String> {
        let Build::Failed { phase, took } = &self.build else {
            return None;
        };
        let advice = if self.enabled_patches().next().is_some() {
            "Try turning patches off, or copy the log if you report the problem."
        } else {
            "Copy the log if you report the problem."
        };
        Some(format!(
            "{} failed after {}. {advice}",
            phase.failure(),
            progress::clock(*took)
        ))
    }

    pub fn log_text(&self) -> String {
        self.log
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn next(&self) -> Next {
        let go = |label, step| Next {
            label,
            action: self.can_go(step).then_some(NextAction::Go(step)),
        };
        match (self.step, &self.build) {
            (Step::Rom, _) => go("Continue to Patches", Step::Patches),
            (Step::Patches, _) => go("Continue to Build", Step::Build),
            (Step::Build, Build::Ready) => Next {
                label: "Build game",
                action: Some(NextAction::Build),
            },
            (Step::Build, Build::Running { .. }) => Next {
                label: "Press Start",
                action: None,
            },
            (Step::Build, Build::Done { .. }) => Next {
                label: "Press Start",
                action: Some(NextAction::Play),
            },
            (Step::Build, Build::Failed { .. }) => Next {
                label: "Try again",
                action: Some(NextAction::Build),
            },
        }
    }

    /// The bottom bar's status line.
    pub fn status(&self) -> String {
        let Some(rom) = self.rom() else {
            return "No cartridge yet".into();
        };
        let count = self.enabled_patches().count();
        let patches = format!(
            "{} \u{b7} {count} {} on",
            rom.title,
            if count == 1 { "patch" } else { "patches" }
        );
        match (self.step, &self.build) {
            (Step::Rom, _) => format!("{} \u{b7} {}", rom.title, rom.size),
            (Step::Build, Build::Running { .. }) => {
                "Building\u{2026} you can keep using your computer".into()
            }
            (Step::Build, Build::Done { .. }) => {
                format!("Saved to {}", self.settings.games_label())
            }
            _ => patches,
        }
    }
}

/// The game program's file name: the ROM file's stem with each patch's name in brackets,
/// odd characters replaced. The same rule as anygenesis's own `exe_name`, so the Build step
/// shows the name before the build reports it.
fn exe_name(rom_path: &Path, patches: &[&str]) -> String {
    let stem = rom_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = patches
        .iter()
        .fold(stem, |name, patch| format!("{name} ({patch})"))
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || " ._-()".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim();
    format!(
        "{}{}",
        if name.is_empty() { "game" } else { name },
        std::env::consts::EXE_SUFFIX
    )
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `n` with thousands separators.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A ROM size the way the design writes it: KiB or MiB when whole, else bytes.
fn size_label(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB && bytes.is_multiple_of(MIB / 2) {
        let mib = bytes as f64 / MIB as f64;
        if bytes.is_multiple_of(MIB) {
            format!("{mib:.0} MiB")
        } else {
            format!("{mib:.1} MiB")
        }
    } else if bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{} bytes", grouped(bytes))
    }
}

fn regions_label(regions: Option<Regions>, code: &str) -> String {
    let Some(regions) = regions else {
        return if code.trim().is_empty() {
            "Unknown".into()
        } else {
            format!("Unknown ({})", code.trim())
        };
    };
    let names: Vec<&str> = [
        (regions.japan, "Japan"),
        (regions.asia, "Asia"),
        (regions.americas, "USA"),
        (regions.europe, "Europe"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if names.is_empty() {
        "None".into()
    } else {
        names.join(", ")
    }
}

/// Header text as shown: runs of spaces collapsed, a dash when empty.
fn shown(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        "\u{2013}".into()
    } else {
        text
    }
}

fn rom_info(path: &Path, rom: &Rom) -> RomInfo {
    let header = rom.header();
    let serial = shown(&header.serial);
    let revision = serial
        .rsplit_once('-')
        .map(|(_, revision)| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "\u{2013}".into());
    let size = size_label(rom.bytes().len());
    let checksum = if rom.has_valid_checksum() {
        "Matches"
    } else {
        "Doesn't match"
    };
    RomInfo {
        path: path.to_owned(),
        file_name: file_name(path),
        title: rom.title(),
        sha1: rom.sha1(),
        fields: vec![
            ("Domestic title", shown(&header.domestic_title)),
            ("Overseas title", shown(&header.overseas_title)),
            ("Product code", serial),
            ("Revision", revision),
            (
                "Regions",
                regions_label(header.regions, &header.region_code),
            ),
            ("Size", size.clone()),
            ("Copyright", shown(&header.copyright)),
            ("Checksum", checksum.into()),
        ],
        size,
        warnings: rom.warnings().iter().map(warning_notice).collect(),
    }
}

fn notice(lead: &str, rest: &str) -> Notice {
    Notice {
        lead: lead.into(),
        rest: rest.into(),
    }
}

fn warning_notice(warning: &RomWarning) -> Notice {
    match warning {
        RomWarning::ChecksumMismatch { .. } => notice(
            "This cartridge's checksum is a bit off.",
            "Lots of dumps and hacks are like this. You can still build it.",
        ),
        RomWarning::MissingSignature => notice(
            "This cartridge's header doesn't start with SEGA.",
            "A console with TMSS would refuse it, but you can still build it.",
        ),
        RomWarning::RomEndMismatch { .. } => notice(
            "The header's ROM size doesn't match the file.",
            "AnyGenesis goes by the file. You can still build it.",
        ),
        RomWarning::UnknownRegion { .. } => notice(
            "The region code is one we don't know.",
            "You can still build it.",
        ),
        other => notice(&format!("{}.", capitalised(&other.to_string())), ""),
    }
}

fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

fn rom_error(path: &Path, error: &RomError) -> RomState {
    let title = match error {
        RomError::InterleavedSmd { .. } => "Whoops, wrong cartridge format",
        RomError::TooLarge { .. } => "This cartridge needs a mapper",
        RomError::Io { .. } => "That file wouldn't open",
        _ => "This file isn't a cartridge image",
    };
    let file = match fs::metadata(path) {
        Ok(metadata) => format!(
            "{} \u{b7} {} bytes",
            file_name(path),
            grouped(metadata.len() as usize)
        ),
        Err(_) => file_name(path),
    };
    RomState::Error {
        title: title.into(),
        message: format!("{}.", capitalised(&error.to_string())),
        file,
    }
}

/// The list entry for the patch at `path`; a pack must be for the ROM with `rom`'s SHA-1.
fn local_patch(path: &Path, rom: Sha1Digest, origin: Origin) -> Result<LocalPatch, String> {
    let patch = Patch::load(path)
        .map_err(|error| format!("{} couldn't be added: {}.", file_name(path), error))?;
    let (kind, runs_code) = match &patch {
        Patch::Pack(pack) if pack.rom != rom => {
            return Err(format!(
                "{} is for another ROM (SHA-1 {}), not this one.",
                pack.name, pack.rom
            ));
        }
        Patch::Pack(pack) if !pack.code.sources.is_empty() => ("Code patch".to_owned(), true),
        Patch::Pack(_) => ("Data patch".to_owned(), false),
        Patch::File { file, .. } => (format!("{} patch", file.format()), false),
    };
    Ok(LocalPatch {
        path: path.to_owned(),
        name: patch.name().to_owned(),
        kind,
        origin,
        on: true,
        runs_code,
    })
}

/// The packs for the ROM with SHA-1 `rom` in every folder of `folders`, in their order.
fn scan(folders: &[(PathBuf, Origin)], rom: Sha1Digest) -> Vec<LocalPatch> {
    folders
        .iter()
        .flat_map(|(folder, origin)| folder_patches(folder, rom, *origin))
        .collect()
}

/// The packs under `folder` (searched through its subfolders) made for the ROM with SHA-1
/// `rom`, in path order. Packs that do not load are skipped.
fn folder_patches(folder: &Path, rom: Sha1Digest, origin: Origin) -> Vec<LocalPatch> {
    let mut packs = Vec::new();
    collect_packs(folder, 0, &mut packs);
    packs.sort();
    packs
        .iter()
        .filter(|path| Pack::load(path).is_ok_and(|pack| pack.rom == rom))
        .filter_map(|path| local_patch(path, rom, origin).ok())
        .collect()
}

fn collect_packs(folder: &Path, depth: usize, packs: &mut Vec<PathBuf>) {
    const MAX_DEPTH: usize = 6;
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < MAX_DEPTH {
            collect_packs(&path, depth + 1, packs);
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
        {
            packs.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic 1 KiB image with a valid-looking header (no game data).
    fn image(serial: &str, region: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x400];
        let mut put = |offset: usize, width: usize, text: &str| {
            let mut field = text.as_bytes().to_vec();
            field.resize(width, b' ');
            bytes[offset..offset + width].copy_from_slice(&field);
        };
        put(0x100, 16, "SEGA MEGA DRIVE");
        put(0x110, 16, "(C)TEST 2026.OCT");
        put(0x120, 48, "TEST  GAME");
        put(0x150, 48, "TEST GAME   OVERSEAS");
        put(0x180, 14, serial);
        put(0x1F0, 3, region);
        bytes[0x1A4..0x1A8].copy_from_slice(&0x3FFu32.to_be_bytes());
        bytes[0x18E..0x190].copy_from_slice(&0x1234u16.to_be_bytes());
        bytes
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir()
                .join(format!("anygenesis-gui-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder).expect("a scratch folder");
            Self(folder)
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("a folder");
            }
            fs::write(&path, bytes).expect("a file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn loaded(scratch: &Scratch) -> Model {
        let rom = scratch.write("test game.bin", &image("GM 00001009-01", "JUE"));
        let mut model = Model::new(Settings::default());
        model.load_rom(&rom, &[]);
        model
    }

    #[test]
    fn test_rom_header_fields_and_warnings() {
        let scratch = Scratch::new("header");
        let model = loaded(&scratch);
        let rom = model.rom().expect("loaded");
        assert_eq!(rom.title, "TEST GAME OVERSEAS");
        assert_eq!(rom.file_name, "test game.bin");
        let field = |name| {
            rom.fields
                .iter()
                .find(|(label, _)| *label == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(field("Domestic title"), Some("TEST GAME"));
        assert_eq!(field("Product code"), Some("GM 00001009-01"));
        assert_eq!(field("Revision"), Some("01"));
        assert_eq!(field("Regions"), Some("Japan, USA, Europe"));
        assert_eq!(field("Size"), Some("1 KiB"));
        assert_eq!(field("Checksum"), Some("Doesn't match"));
        assert_eq!(rom.fields.len(), 8);
        assert_eq!(rom.sha1.to_string().len(), 40);
        assert_eq!(rom.warnings.len(), 1);
        assert!(rom.warnings[0].lead.contains("checksum"));
        assert_eq!(model.status(), "TEST GAME OVERSEAS \u{b7} 1 KiB");
    }

    #[test]
    fn test_unrunnable_images_show_rom_error_texts() {
        let scratch = Scratch::new("errors");
        let mut model = Model::new(Settings::default());
        let odd = scratch.write("odd.bin", &[0; 0x401]);
        model.load_rom(&odd, &[]);
        let RomState::Error {
            title,
            message,
            file,
        } = &model.rom
        else {
            panic!("an odd size is refused");
        };
        assert_eq!(title, "This file isn't a cartridge image");
        assert!(
            message.starts_with("The image is 1025 bytes, an odd size"),
            "{message}"
        );
        assert!(message.ends_with('.'));
        assert_eq!(file, "odd.bin \u{b7} 1,025 bytes");
        let large = scratch.write("big.bin", &vec![0; 0x40_0002]);
        model.load_rom(&large, &[]);
        assert!(matches!(&model.rom, RomState::Error { title, .. } if title.contains("mapper")));
        model.load_rom(&scratch.0.join("missing.bin"), &[]);
        assert!(matches!(&model.rom, RomState::Error { file, .. } if file == "missing.bin"));
        assert!(!model.can_go(Step::Patches));
        assert_eq!(model.next().action, None);
    }

    #[test]
    fn test_steps_unlock_once_a_rom_is_loaded() {
        let mut model = Model::new(Settings::default());
        assert_eq!(
            model.step_state(Step::Patches),
            StepState::Todo { locked: true }
        );
        model.go(Step::Build);
        assert_eq!(model.step, Step::Rom);
        assert_eq!(model.status(), "No cartridge yet");
        let scratch = Scratch::new("steps");
        let mut model = loaded(&scratch);
        assert_eq!(model.step_state(Step::Rom), StepState::Current);
        assert_eq!(model.next().action, Some(NextAction::Go(Step::Patches)));
        model.go(Step::Build);
        assert_eq!(model.step_state(Step::Rom), StepState::Done);
        assert_eq!(model.step_state(Step::Patches), StepState::Done);
        assert_eq!(model.next().label, "Build game");
        model.back();
        assert_eq!(model.step, Step::Patches);
        assert_eq!(
            model.step_state(Step::Build),
            StepState::Todo { locked: false }
        );
        model.go(Step::Rom);
        assert_eq!(model.step_state(Step::Rom), StepState::Current);
    }

    #[test]
    fn test_patches_add_order_toggle_and_reorder() {
        let scratch = Scratch::new("patches");
        let mut model = loaded(&scratch);
        // A minimal IPS file: header, one record writing 0x4E71 at 0x200, EOF.
        let ips = [b"PATCH".as_slice(), &[0, 2, 0, 0, 2, 0x4E, 0x71], b"EOF"].concat();
        let first = scratch.write("first.ips", &ips);
        let second = scratch.write("second.ips", &ips);
        model.add_patch(&first);
        model.add_patch(&second);
        model.add_patch(&first);
        assert_eq!(model.patches.len(), 2);
        assert_eq!(model.patches[0].kind, "IPS patch");
        assert_eq!(model.patches[0].meta(), "IPS patch \u{b7} from file");
        assert_eq!(model.patch_summary(), "first, then second");
        assert_eq!(
            model.exe_name(),
            format!("test game (first) (second){}", std::env::consts::EXE_SUFFIX)
        );
        model.move_patch(1, 0);
        assert_eq!(model.patches[0].name, "second");
        model.toggle_patch(0);
        assert_eq!((model.order(0), model.order(1)), (None, Some(1)));
        assert_eq!(model.patch_summary(), "first");
        let arguments = model.build_arguments();
        let text: Vec<String> = arguments
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(text[1], "--progress");
        assert_eq!(text[2], "--patch");
        assert!(text[3].ends_with("first.ips"));
        assert_eq!(text[4], "--out");
        assert_eq!(text.len(), 6);
        model.remove_patch(0);
        model.remove_patch(5);
        assert_eq!(model.patches.len(), 1);
        let junk = scratch.write("junk.ips", b"not a patch");
        model.add_patch(&junk);
        assert!(
            model
                .patch_error
                .as_deref()
                .is_some_and(|e| e.starts_with("junk.ips couldn't be added"))
        );
        assert_eq!(model.patches.len(), 1);
    }

    #[test]
    fn test_folder_scan_lists_only_packs_for_this_rom() {
        let scratch = Scratch::new("folder");
        let bytes = image("GM 00000000-00", "U");
        let rom_path = scratch.write("game.bin", &bytes);
        let sha1 = Sha1Digest::of(&bytes);
        let mine = format!(
            "name = \"Mine\"\nrom = \"{sha1}\"\n\n[[patch]]\noffset = 0x200\nbytes = \"4E71\"\n"
        );
        let other = format!(
            "name = \"Other\"\nrom = \"{}\"\n\n[[patch]]\noffset = 0x200\nbytes = \"4E71\"\n",
            "0".repeat(40)
        );
        scratch.write("patches/b/mine.toml", mine.as_bytes());
        scratch.write("patches/a/other.toml", other.as_bytes());
        scratch.write("patches/broken.toml", b"name = ");
        let mut model = Model::new(Settings::default());
        model.load_rom(&rom_path, &[(scratch.0.join("patches"), Origin::Folder)]);
        assert_eq!(model.patches.len(), 1, "{:?}", model.patches);
        assert_eq!(model.patches[0].name, "Mine");
        assert_eq!(model.patches[0].origin, Origin::Folder);
        assert_eq!(model.patches[0].kind, "Data patch");
        model.add_patch(&scratch.0.join("patches/a/other.toml"));
        assert!(
            model
                .patch_error
                .as_deref()
                .is_some_and(|e| e.contains("another ROM"))
        );
    }

    #[test]
    fn test_market_install_lists_the_pack_and_rescan_keeps_choices() {
        let scratch = Scratch::new("market");
        let bytes = image("GM 00000000-00", "U");
        let rom_path = scratch.write("game.bin", &bytes);
        let pack_text = format!(
            "name = \"Wide\"\nrom = \"{}\"\n\n[[patch]]\noffset = 0x200\nbytes = \"4E71\"\n",
            Sha1Digest::of(&bytes)
        );
        scratch.write("repo/Game/wide/pack.toml", pack_text.as_bytes());
        let pack = IndexPack {
            id: "Game/wide".to_owned(),
            name: "Wide".to_owned(),
            description: None,
            rom: Sha1Digest::of(&bytes).to_string(),
            runs_code: false,
            files: vec![market::IndexFile {
                path: market::PACK_FILE.to_owned(),
                size: pack_text.len() as u64,
                sha256: anygenesis_patch::sha256::hex_digest(pack_text.as_bytes()),
            }],
        };
        let mut other = pack.clone();
        other.id = "Other/wide".to_owned();
        other.rom = "0".repeat(40);
        let index = Index {
            format: market::INDEX_FORMAT,
            commit: "a".repeat(40),
            packs: vec![pack.clone(), other],
        };
        let root = scratch.0.join("installed");
        let folders = [(root.clone(), Origin::Market)];
        let mut model = Model::new(Settings::default());
        model.market = Market::Ready(index.clone());
        model.market_root = Some(root.clone());
        model.load_rom(&rom_path, &folders);
        let added = scratch.write("added.ips", b"PATCH\x00\x02\x00\x00\x01\x4EEOF");
        model.add_patch(&added);
        model.toggle_patch(0);
        let entries = model.market_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, Status::Absent);
        let source = market::LocalSource {
            root: scratch.0.join("repo"),
        };
        market::install(&root, &index, &pack, &source).expect("the pack installs");
        model.rescan(&folders);
        assert_eq!(model.patches.len(), 2, "{:?}", model.patches);
        assert!(!model.patches[0].on, "the added file stays off");
        assert_eq!(model.patches[1].origin, Origin::Market);
        assert!(model.patches[1].on, "a new pack comes on");
        assert!(matches!(
            model.market_entries()[0].status,
            Status::Installed { .. }
        ));
        market::remove(&root, &pack).expect("the pack is removed");
        model.rescan(&folders);
        assert_eq!(model.patches.len(), 1);
        assert_eq!(model.patches[0].origin, Origin::File);
    }

    #[test]
    fn test_build_runs_through_progress_lines_to_done() {
        let scratch = Scratch::new("build");
        let mut model = loaded(&scratch);
        model.go(Step::Build);
        let start = Instant::now();
        model.start_build(start);
        assert!(model.is_building());
        assert_eq!(
            model.next(),
            Next {
                label: "Press Start",
                action: None
            }
        );
        assert_eq!(
            model.status(),
            "Building\u{2026} you can keep using your computer"
        );
        model.on_line("ag-progress read", start);
        model.on_line("TEST GAME (SHA-1 x): building in y with Zig 0.17.0", start);
        model.on_line("", start);
        model.on_line("ag-progress compile 3 10", start + Duration::from_secs(5));
        let Build::Running { progress, .. } = &model.build else {
            panic!("still running");
        };
        assert_eq!(progress.title(), "Compiling 3 of 10 files");
        assert_eq!(model.log.len(), 1);
        model.on_line(
            "ag-done /games/test game.exe",
            start + Duration::from_secs(70),
        );
        let Build::Done { exe, took } = &model.build else {
            panic!("done");
        };
        assert_eq!(exe, Path::new("/games/test game.exe"));
        assert_eq!(*took, Duration::from_secs(70));
        model.on_exit(Some(0), start + Duration::from_secs(71));
        assert!(matches!(model.build, Build::Done { .. }));
        assert_eq!(model.next().action, Some(NextAction::Play));
        assert!(model.status().starts_with("Saved to Documents"));
    }

    #[test]
    fn test_build_failure_and_cancel() {
        let scratch = Scratch::new("fail");
        let mut model = loaded(&scratch);
        let start = Instant::now();
        model.start_build(start);
        model.on_line("ag-progress link", start);
        model.on_line("ag-error linking the game: undefined symbol", start);
        model.on_exit(Some(1), start + Duration::from_secs(123));
        assert!(matches!(
            model.build,
            Build::Failed {
                phase: Phase::Link,
                ..
            }
        ));
        assert_eq!(
            model.failure_text().as_deref(),
            Some("Linking failed after 2:03. Copy the log if you report the problem.")
        );
        assert_eq!(model.log.iter().filter(|line| line.error).count(), 1);
        model.go(Step::Build);
        assert_eq!(model.next().label, "Try again");
        model.start_build(start);
        model.on_exit(None, start);
        assert_eq!(
            model.log.last().map(|l| l.text.as_str()),
            Some("anygenesis was stopped")
        );
        model.start_build(start);
        model.cancel_build();
        assert!(matches!(model.build, Build::Ready));
        assert!(model.log.is_empty());
    }

    #[test]
    fn test_size_labels_and_grouping() {
        assert_eq!(size_label(512 * 1024), "512 KiB");
        assert_eq!(size_label(1024 * 1024), "1 MiB");
        assert_eq!(size_label(1536 * 1024), "1.5 MiB");
        assert_eq!(size_label(524_800), "524,800 bytes");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(12), "12");
    }

    #[test]
    fn test_exe_name_matches_anygenesis() {
        let name = exe_name(Path::new("roms/s1built.bin"), &["Fixes", "cheats: 1/2"]);
        assert_eq!(
            name.strip_suffix(std::env::consts::EXE_SUFFIX),
            Some("s1built (Fixes) (cheats_ 1_2)")
        );
        assert_eq!(
            exe_name(Path::new(""), &[]),
            format!("game{}", std::env::consts::EXE_SUFFIX)
        );
    }
}
