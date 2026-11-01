//! Packs this host's bundle (from `cargo xtask toolchain`) into a release download: the bundle,
//! THIRD-PARTY-NOTICES.txt and our licences are staged in target/dist/anygenesis-<version>-
//! <host>/, then turned into the Windows installer (Inno Setup 7, packaging/windows/) or the
//! Linux AppImage (appimagetool, packaging/linux/), its SHA-256 in target/dist/SHA256SUMS.
//! Nothing here is committed or uploaded; publishing is a separate, manual step
//! (docs/RELEASE.md).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anygenesis_patch::sha256;
use anyhow::{Context, Result, bail};

use crate::{
    notices,
    toolchain::{HOST, copy_tree, player_commit, remove_folder, run_tool},
    workspace_root,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Builds the release download from target/bundle/<host>/. `--dirty` allows a bundle built
/// from uncommitted changes, for trying the packaging out; such a download is never published.
///
/// # Errors
/// No bundle, a bundle from another version or commit, failing notices, or a failing step.
pub(crate) fn run(flag: Option<&str>) -> Result<ExitCode> {
    let allow_dirty = match flag {
        None => false,
        Some("--dirty") => true,
        Some(other) => bail!("usage: cargo xtask release [--dirty] (not `{other}`)"),
    };
    if cfg!(target_os = "macos") {
        bail!(
            "there is no macOS download yet: its app bundle, signing and notarisation come with \
             the builder app on macOS"
        );
    }
    if cfg!(all(windows, target_arch = "aarch64")) {
        bail!(
            "there is no Windows ARM64 download yet: its installer comes with the builder app \
             on Windows ARM64"
        );
    }
    let root = workspace_root()?;
    let bundle = root.join("target").join("bundle").join(HOST.name);
    check_bundle(&bundle, &player_commit(&root)?, allow_dirty)?;

    let releases = root.join("target").join("dist");
    let name = format!("anygenesis-{VERSION}-{}", HOST.name);
    let stage = releases.join(&name);
    remove_folder(&stage)?;
    copy_tree(&bundle, &stage)?;
    let notices = notices::text(&root, &stage)?;
    fs::write(stage.join("THIRD-PARTY-NOTICES.txt"), notices)
        .context("writing THIRD-PARTY-NOTICES.txt")?;
    for licence in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy_tree(&root.join(licence), &stage.join(licence))?;
    }

    let download = package(&root, &releases, &stage)?;
    let bytes = fs::read(&download).with_context(|| format!("reading {}", download.display()))?;
    let digest = sha256::hex_digest(&bytes);
    record_digest(&releases.join("SHA256SUMS"), &download, &digest)?;
    println!(
        "{} written: {:.1} MB, SHA-256 {digest}",
        download.display(),
        bytes.len() as f64 / 1_000_000.0
    );
    Ok(ExitCode::SUCCESS)
}

/// Checks that the bundle was built for this version from the checked-out, clean commit.
fn check_bundle(bundle: &Path, commit: &str, allow_dirty: bool) -> Result<()> {
    let manifest_path = bundle.join("toolchain").join("toolchain.txt");
    let manifest = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "reading {} (run `cargo xtask toolchain` first)",
            manifest_path.display()
        )
    })?;
    let field = |key: &str| {
        manifest
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .unwrap_or_default()
    };
    if field("anygenesis") != VERSION {
        bail!("the bundle is not version {VERSION}: rebuild it with `cargo xtask toolchain`");
    }
    if allow_dirty {
        return Ok(());
    }
    if commit.ends_with("-dirty") {
        bail!("the working tree has changes: commit them, rebuild the bundle, then release");
    }
    if field("player") != commit {
        bail!(
            "the bundle was built from {}, not the checked-out {commit}: rebuild it",
            field("player")
        );
    }
    Ok(())
}

/// Compiles the installer from `stage` with Inno Setup 7's ISCC (`ANYGENESIS_ISCC`, else its
/// per-user or per-machine install) and returns its path.
#[cfg(windows)]
fn package(root: &Path, releases: &Path, stage: &Path) -> Result<PathBuf> {
    let iscc = match env::var_os("ANYGENESIS_ISCC") {
        Some(path) => PathBuf::from(path),
        None => [
            env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("Programs")),
            env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            env::var_os("ProgramFiles").map(PathBuf::from),
        ]
        .into_iter()
        .flatten()
        .map(|programs| programs.join("Inno Setup 7").join("ISCC.exe"))
        .find(|path| path.is_file())
        .context("Inno Setup 7 is not installed (or set ANYGENESIS_ISCC to its ISCC.exe)")?,
    };
    let installer = releases.join(format!("AnyGenesis-{VERSION}-windows-x64-setup.exe"));
    let mut command = Command::new(iscc);
    command
        .arg("/Q")
        .arg(format!("/DAppVersion={VERSION}"))
        .arg(format!("/DSourceDir={}", stage.display()))
        .arg(format!("/DOutputDir={}", releases.display()))
        .arg(
            root.join("packaging")
                .join("windows")
                .join("anygenesis.iss"),
        );
    run_tool(&mut command, "compiling the installer")?;
    Ok(installer)
}

/// Lays out an AppDir from `stage` and turns it into an AppImage with appimagetool
/// (`ANYGENESIS_APPIMAGETOOL`, else on PATH), returning its path.
#[cfg(not(windows))]
fn package(root: &Path, releases: &Path, stage: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let app_dir = releases.join("AnyGenesis.AppDir");
    remove_folder(&app_dir)?;
    copy_tree(stage, &app_dir)?;
    let linux = root.join("packaging").join("linux");
    copy_tree(&linux.join("AppRun"), &app_dir.join("AppRun"))?;
    copy_tree(
        &linux.join("anygenesis.desktop"),
        &app_dir.join("anygenesis.desktop"),
    )?;
    copy_tree(
        &root
            .join("crates")
            .join("gui")
            .join("assets")
            .join("icon.png"),
        &app_dir.join("anygenesis.png"),
    )?;
    let app_run = app_dir.join("AppRun");
    fs::set_permissions(&app_run, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", app_run.display()))?;

    let image = releases.join(format!("AnyGenesis-{VERSION}-x86_64.AppImage"));
    let tool = env::var_os("ANYGENESIS_APPIMAGETOOL").unwrap_or_else(|| "appimagetool".into());
    let mut command = Command::new(tool);
    command
        .env("ARCH", "x86_64")
        // appimagetool is itself an AppImage; this runs it without FUSE (WSL has none).
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .arg(&app_dir)
        .arg(&image);
    run_tool(&mut command, "building the AppImage")?;
    Ok(image)
}

/// Writes `digest` for `download` into SHA256SUMS (`<digest>  <file name>` lines), keeping the
/// other downloads' lines.
fn record_digest(sums: &Path, download: &Path, digest: &str) -> Result<()> {
    let file_name = download
        .file_name()
        .context("the download has no file name")?
        .to_string_lossy()
        .into_owned();
    let old = fs::read_to_string(sums).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|line| line.split_once("  ").map(|(_, name)| name) != Some(&file_name))
        .map(str::to_owned)
        .collect();
    lines.push(format!("{digest}  {file_name}"));
    lines.sort_by_key(|line| line.split_once("  ").map(|(_, name)| name.to_owned()));
    fs::write(sums, lines.join("\n") + "\n").with_context(|| format!("writing {}", sums.display()))
}
