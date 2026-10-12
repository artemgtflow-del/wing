use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::http::Http;
use crate::manifest::{Installer, Manifest};
use crate::paths;
use crate::resolve::Choice;
use crate::sysutil;
use crate::verify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handler {
    Msi,
    Exe,
    Msix,
    Msu,
    Zip,
    SevenZip,
    TarGz,
}

pub fn handler_for(m: &Manifest, inst: &Installer) -> Handler {
    let ext = Path::new(&inst.url)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "msi" => Handler::Msi,
        "msix" | "msixbundle" | "appx" | "appxbundle" => Handler::Msix,
        "msu" => Handler::Msu,
        "zip" => Handler::Zip,
        "7z" => Handler::SevenZip,
        "gz" | "bz2" | "xz" | "tar" => Handler::TarGz,
        "exe" | "bat" | "cmd" | "com" => Handler::Exe,
        _ => match m.kind.to_ascii_lowercase().as_str() {
            "wix" | "msi" => Handler::Msi,
            "msix" => Handler::Msix,
            "msu" => Handler::Msu,
            "zip" => Handler::Zip,
            "tar.gz" => Handler::TarGz,
            _ => Handler::Exe,
        },
    }
}

fn dest_for(m: &Manifest, inst: &Installer) -> PathBuf {
    let name = inst.file_name();
    paths::downloads().join(format!("{}--{}", sanitize(&m.id), name))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Downloads and verifies every artifact before anything is executed.
pub async fn fetch_verified(
    http: &Http,
    m: &Manifest,
    inst: &Installer,
    force: bool,
) -> Result<PathBuf> {
    let dest = dest_for(m, inst);
    if dest.exists() && !force {
        let cached = verify::sha256_file(&dest)
            .map(|h| h.eq_ignore_ascii_case(&inst.sha256))
            .unwrap_or(false);
        if cached {
            return Ok(dest);
        }
        let _ = std::fs::remove_file(&dest);
    }
    let _ = std::fs::create_dir_all(paths::downloads());
    eprintln!("  fetching {}", inst.url);
    http.download(&inst.url, &dest).await?;
    verify::verify(&dest, &inst.sha256)?;
    Ok(dest)
}

/// Result of running an installer.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub location: String,
    /// names of the command links wing created in %LOCALAPPDATA%\wing\bin
    pub shims: Vec<String>,
    /// MSI product code, used later by `wing uninstall`
    pub product_code: Option<String>,
    /// `Microsoft.UninstallArguments` from the manifest locale
    pub uninstall_args: Option<String>,
}

/// Installs a verified artifact.
pub fn run_install(choice: &Choice, file: &Path) -> Result<Outcome> {
    let m = &choice.manifest;
    let inst = &choice.installer;
    let mut outcome = match handler_for(m, inst) {
        Handler::Msi => install_msi(m, choice, file)?,
        Handler::Msix => install_msix(file)?,
        Handler::Msu => install_msu(file)?,
        Handler::Exe => install_exe(m, choice, file)?,
        Handler::Zip => install_zip(m, file)?,
        Handler::SevenZip => install_sevenz(m, file)?,
        Handler::TarGz => install_targz(m, file)?,
    };
    outcome.product_code = m.product_code.clone().or_else(|| inst.product_code.clone());
    outcome.uninstall_args = m.uninstall_args.clone();
    // Installer-based packages often land somewhere that is not on PATH and do
    // not declare `Commands:`; make their executable reachable anyway.
    if !matches!(
        handler_for(m, inst),
        Handler::Zip | Handler::SevenZip | Handler::TarGz
    ) {
        let linked = link_commands(m);
        if !linked.is_empty() {
            outcome.shims.extend(linked);
        }
    }
    Ok(outcome)
}

fn split_switches(s: &str) -> Vec<String> {
    s.split_whitespace().map(|x| x.to_string()).collect()
}

fn needs_admin(choice: &Choice) -> bool {
    choice.scope.eq_ignore_ascii_case("machine")
        || choice.manifest.elevation_required
        || choice.installer.elevation_required
}

fn elevate(program: &Path, args: &[String]) -> Result<()> {
    if sysutil::is_elevated() {
        sysutil::run(program, args).map(|_| ())
    } else {
        eprintln!("  requesting administrator rights...");
        sysutil::run_elevated(program, args)
    }
}

/// Runs an installer and normalises the "needs a reboot" exit codes.
fn check_exit(code: i32) -> Result<()> {
    match code {
        0 | 1641 | 3010 => Ok(()),
        c => bail!("installer exited with code {c}"),
    }
}

/// Builds the msiexec command line without running it.
///
/// Split out from [`install_msi`] so the argument construction - which is where
/// silent mode, scope and the logging switch interact - can be tested without
/// touching the machine.
pub fn msi_command(
    m: &Manifest,
    choice: &Choice,
    file: &Path,
    log: &Path,
) -> (String, Vec<String>) {
    let mut args: Vec<String> = vec!["/i".into(), file.display().to_string(), "/norestart".into()];
    match choice.mode.as_str() {
        "silent" => args.push("/qn".into()),
        "silentWithProgress" => args.push("/passive".into()),
        _ => {}
    }
    if choice.scope.eq_ignore_ascii_case("machine") {
        args.push("ALLUSERS=1".into());
    } else {
        args.push("ALLUSERS=2".into());
        args.push("MSIINSTALLPERUSER=1".into());
    }
    if let Some(sw) = m.switches.for_mode(&choice.mode) {
        args.extend(split_switches(sw));
    }
    // `/L*v` and its file must be separate arguments: a single argument holding
    // both gets quoted by the OS, and msiexec then rejects the command line
    // with exit code 1639 and prints its usage text.
    args.push("/L*v".into());
    args.push(log.display().to_string());
    ("msiexec.exe".into(), args)
}

fn install_msi(m: &Manifest, choice: &Choice, file: &Path) -> Result<Outcome> {
    let log = paths::data_dir().join(format!("{}-install.log", sanitize(&m.id)));
    let (program_name, args) = msi_command(m, choice, file, &log);

    let program = Path::new(&program_name);
    if choice.verbose {
        eprintln!("  running: {} {}", program.display(), args.join(" "));
    }
    if needs_admin(choice) {
        elevate(program, &args)?;
    } else {
        let code = sysutil::run(program, &args)?;
        if let Err(e) = check_exit(code) {
            bail!("{e}\n  full log: {}", log.display());
        }
    }
    println!("  msiexec log: {}", log.display());
    Ok(Outcome {
        location: format!("{} {} installed via MSI", m.id, m.version),
        ..Default::default()
    })
}

fn install_msix(file: &Path) -> Result<Outcome> {
    let script = format!(
        "$ErrorActionPreference='Stop'; Add-AppxPackage -Path '{}'",
        file.display().to_string().replace('\'', "''")
    );
    let code = sysutil::run(
        Path::new("powershell.exe"),
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script,
        ],
    )?;
    if code != 0 {
        bail!("Add-AppxPackage failed with exit code {code}");
    }
    Ok(Outcome {
        location: format!("MSIX package registered from {}", file.display()),
        ..Default::default()
    })
}

fn install_msu(file: &Path) -> Result<Outcome> {
    let args = vec![
        file.display().to_string(),
        "/quiet".into(),
        "/norestart".into(),
    ];
    elevate(Path::new("wusa.exe"), &args)?;
    Ok(Outcome {
        location: format!("update installed from {}", file.display()),
        ..Default::default()
    })
}

/// Builds the EXE installer command line without running it.
pub fn exe_command(m: &Manifest, choice: &Choice, file: &Path) -> (String, Vec<String>) {
    let mut args: Vec<String> = match choice.mode.as_str() {
        "silent" => split_switches(m.switches.for_mode("silent").unwrap_or("")),
        "silentWithProgress" => {
            split_switches(m.switches.for_mode("silentWithProgress").unwrap_or(""))
        }
        _ => vec![],
    };
    if m.kind == "burn" || choice.installer.nested_installer_type.as_deref() == Some("burn") {
        // Burn decides between install and uninstall from the command line, so a
        // stale `/install` or `/quiet` from the manifest has to be replaced
        // rather than appended to.
        args.retain(|a| a != "/install");
        args.push("/install".into());
        args.retain(|a| a != "/passive" && a != "/quiet");
        args.push(
            if choice.mode == "interactive" {
                "/passive"
            } else {
                "/quiet"
            }
            .into(),
        );
        args.push("/norestart".into());
    }
    (file.display().to_string(), args)
}

fn install_exe(m: &Manifest, choice: &Choice, file: &Path) -> Result<Outcome> {
    let (program_name, args) = exe_command(m, choice, file);
    if choice.verbose {
        eprintln!("  running: {program_name} {}", args.join(" "));
    }
    let program = Path::new(&program_name);
    if needs_admin(choice) {
        elevate(program, &args)?;
    } else {
        check_exit(sysutil::run(program, &args)?)?;
    }
    Ok(Outcome {
        location: format!("{} {} installed", m.id, m.version),
        ..Default::default()
    })
}

// ------------------------------------------------------------- archive installs

fn target_dir(m: &Manifest) -> PathBuf {
    paths::apps().join(sanitize(&m.id))
}

fn sanitize_entry(p: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(p).components() {
        match c {
            std::path::Component::Normal(s) => out.push(s),
            std::path::Component::CurDir => {}
            _ => return None, // reject ParentDir / RootDir / Prefix (zip-slip)
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn install_zip(m: &Manifest, file: &Path) -> Result<Outcome> {
    let dest = target_dir(m);
    let _ = std::fs::create_dir_all(&dest);
    let f = std::fs::File::open(file)?;
    let mut ar = zip::ZipArchive::new(f).context("opening zip archive")?;
    for i in 0..ar.len() {
        let mut entry = ar.by_index(i)?;
        let Some(rel) = sanitize_entry(entry.name()) else {
            continue;
        };
        let out = dest.join(&rel);
        if entry.is_dir() {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(p) = out.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut o = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut o)?;
    }
    Ok(finalize_portable(m, &dest))
}

fn install_sevenz(m: &Manifest, file: &Path) -> Result<Outcome> {
    let dest = target_dir(m);
    let _ = std::fs::create_dir_all(&dest);
    sevenz_rust2::decompress_file(file, &dest).map_err(|e| anyhow!("7z: {e}"))?;
    Ok(finalize_portable(m, &dest))
}

fn install_targz(m: &Manifest, file: &Path) -> Result<Outcome> {
    let dest = target_dir(m);
    let _ = std::fs::create_dir_all(&dest);
    let f = std::fs::File::open(file)?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(dec);
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let Some(rel) = path.to_str().and_then(sanitize_entry) else {
            continue;
        };
        let out = dest.join(&rel);
        if entry.header().entry_type().is_dir() {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(p) = out.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut o = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut o)?;
    }
    Ok(finalize_portable(m, &dest))
}

/// Creates command shims in %LOCALAPPDATA%\wing\bin and puts it on the user PATH.
///
/// Hard links are used first: they are transparent to the shell and immune to the
/// OEM code page problem that breaks `.cmd` shims when a user path is non-ASCII.
fn finalize_portable(_m: &Manifest, dir: &Path) -> Outcome {
    let mut exes: Vec<PathBuf> = Vec::new();
    collect_exes(dir, &mut exes, 0);
    let bin = paths::bin();
    let _ = std::fs::create_dir_all(&bin);
    let mut made = 0;
    let mut fallbacks = 0;
    let mut created: Vec<String> = Vec::new();
    for exe in exes.iter().take(64) {
        let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.eq_ignore_ascii_case("uninstall") || stem.eq_ignore_ascii_case("setup") {
            continue;
        }
        let link = bin.join(format!("{stem}.exe"));
        let _ = std::fs::remove_file(&link);
        if std::fs::hard_link(exe, &link).is_ok() {
            made += 1;
            created.push(format!("{stem}.exe"));
        } else if copy_ascii_shim(exe, &bin, stem) {
            fallbacks += 1;
            created.push(format!("{stem}.cmd"));
        }
    }
    match sysutil::ensure_user_path() {
        Ok(true) => eprintln!("  added {} to your PATH", bin.display()),
        Ok(false) => {}
        Err(e) => eprintln!("  warning: could not update PATH: {e}"),
    }
    let total = made + fallbacks;
    Outcome {
        location: format!(
            "extracted to {} ({total} command{} available in {})",
            dir.display(),
            if total == 1 { "" } else { "s" },
            bin.display()
        ),
        shims: created,
        ..Default::default()
    }
}

/// Removes a portable install that wing performed itself: the command links it
/// created and the extracted tree. Used by `wing uninstall` when the package
/// never registered itself in Add/Remove Programs.
pub fn purge_portable(id: &str, shims: &[String]) -> Result<()> {
    remove_links(shims);
    let dir = target_dir_for_id(id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    println!("removed {}", dir.display());
    Ok(())
}

fn target_dir_for_id(id: &str) -> PathBuf {
    paths::apps().join(
        id.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>(),
    )
}

/// Last resort for files that cannot be hard linked. The path is emitted through
/// `%LOCALAPPDATA%` so the generated batch file stays pure ASCII, because cmd.exe
/// parses batch files in the OEM code page.
fn copy_ascii_shim(exe: &Path, bin: &Path, stem: &str) -> bool {
    let exe_s = exe.to_string_lossy();
    let local = dirs::data_local_dir().map(|p| p.to_string_lossy().to_string());
    let (prefix, rest) = match local {
        Some(l) if exe_s.starts_with(&l) => {
            let rest = exe_s[l.len()..].to_string();
            (l, rest)
        }
        _ => return false,
    };
    let cmd = format!("@echo off\r\n\"{prefix}%PATHSEP%{rest}\" %*\r\n").replace("%PATHSEP%", "\\");
    std::fs::write(bin.join(format!("{stem}.cmd")), cmd).is_ok()
}

fn collect_exes(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 4 || out.len() > 256 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_exes(&p, out, depth + 1);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("exe"))
            == Some(true)
        {
            out.push(p);
        }
    }
}

/// Removes command links previously created in %LOCALAPPDATA%\wing\bin.
pub fn remove_links(shims: &[String]) {
    let bin = paths::bin();
    let mut removed = 0;
    for s in shims {
        for cand in [
            bin.join(s),
            bin.join(format!("{s}.exe")),
            bin.join(format!("{s}.cmd")),
        ] {
            if cand.exists() && std::fs::remove_file(&cand).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        println!("  removed {removed} command link(s)");
    }
}

/// Removes an MSI package by product code, independent of the registry state.
pub fn msiexec_uninstall(product_code: &str) -> Result<()> {
    let code = product_code.trim().trim_matches(['{', '}']).to_string();
    if code.is_empty() {
        bail!("empty MSI product code");
    }
    let args = vec![
        "/x".to_string(),
        format!("{{{code}}}"),
        "/norestart".to_string(),
        "/qn".to_string(),
    ];
    let program = Path::new("msiexec.exe");
    let exit = if sysutil::is_elevated() {
        sysutil::run(program, &args)?
    } else {
        elevate(program, &args)?;
        0
    };
    match exit {
        0 => Ok(()),
        3010 => {
            println!("  uninstalled, a reboot is pending");
            Ok(())
        }
        // 1605 means "not installed" - reporting success here would silently
        // leave the program in place, so it is an error.
        1605 => bail!(
            "msiexec reports product {{{code}}} as not installed.\n\
             The ProductCode in the manifest does not match an installed product, \
             so the package was left untouched."
        ),
        c => bail!("msiexec /x exited with code {c}"),
    }
}

/// Locates the executable of an already-installed package so it can be exposed
/// on PATH.
///
/// Not every manifest declares `Commands:`, and some installers (this MSI is a
/// real example) neither add themselves to PATH nor record an `InstallLocation`.
/// Rather than leaving such a package unreachable, look in the conventional
/// per-user and per-machine locations.
pub fn locate_executable(m: &Manifest) -> Option<PathBuf> {
    let (_, name) = crate::index::Index::split_id(&m.id);
    let names: Vec<String> = [m.id.as_str(), name.as_str()]
        .into_iter()
        .chain(m.commands.iter().map(|s| s.as_str()))
        .map(|s| s.to_string())
        .collect();

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        roots.push(local.join("Programs"));
        roots.push(local.clone());
    }
    if let Some(pf) = dirs::data_dir().filter(|_| false) {
        roots.push(pf);
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(v) = std::env::var(var) {
            roots.push(PathBuf::from(v));
        }
    }

    // 1. An executable named after a declared command wins.
    for root in &roots {
        for name in &names {
            for dir in [root.clone(), root.join(name)] {
                if !dir.is_dir() {
                    continue;
                }
                for cmd in m.commands.iter().chain(std::iter::once(&name.clone())) {
                    let candidate = dir.join(format!("{cmd}.exe"));
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // 2. Otherwise a single top-level executable in a directory named after the
    //    package, ignoring setup/uninstall helpers.
    for root in &roots {
        for name in &names {
            let dir = root.join(name);
            if !dir.is_dir() {
                continue;
            }
            let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("exe")
                        })
                        .collect()
                })
                .unwrap_or_default();
            found.retain(|p| {
                !p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| {
                        let s = s.to_ascii_lowercase();
                        s == "setup" || s == "uninstall" || s == "unins000"
                    })
                    .unwrap_or(false)
            });
            if found.len() == 1 {
                return found.pop();
            }
            if found.len() > 1 {
                found.sort();
                return found.pop();
            }
        }
    }
    None
}

/// Creates hard links in %LOCALAPPDATA%\wing\bin for the commands a package
/// provides. Returns the names of the links that were created.
pub fn link_commands(m: &Manifest) -> Vec<String> {
    let Some(exe) = locate_executable(m) else {
        return Vec::new();
    };
    let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    if stem.eq_ignore_ascii_case("setup") || stem.eq_ignore_ascii_case("uninstall") {
        return Vec::new();
    }

    let bin = paths::bin();
    if std::fs::create_dir_all(&bin).is_err() {
        return Vec::new();
    }

    let mut created = Vec::new();
    let mut targets: Vec<String> = m.commands.clone();
    if targets.is_empty() || !targets.iter().any(|c| c == stem) {
        targets.push(stem.to_string());
    }
    for target in targets {
        let link = bin.join(format!("{target}.exe"));
        let _ = std::fs::remove_file(&link);
        if std::fs::hard_link(&exe, &link).is_ok() {
            created.push(format!("{target}.exe"));
        }
    }
    if !created.is_empty() {
        match sysutil::ensure_user_path() {
            Ok(true) => eprintln!("  added {} to your PATH", bin.display()),
            Ok(false) => {}
            Err(e) => eprintln!("  warning: could not update PATH: {e}"),
        }
        println!("  linked {} -> {}", created.join(", "), exe.display());
    }
    created
}

// ------------------------------------------------------------------ uninstall

/// Runs an uninstall command taken from Add/Remove Programs.
/// `msiexec` and `wusa` trigger their own UAC prompt for machine-context work,
/// so a direct run is attempted first and elevation is only the fallback.
pub fn uninstall_with_args(program: &str, args: &str) -> Result<()> {
    let mut parts = program.trim().splitn(2, char::is_whitespace);
    let exe = parts
        .next()
        .filter(|s| !s.is_empty())
        .context("empty uninstall command")?;
    if exe.to_ascii_lowercase().contains("winget") {
        bail!("refusing to delegate to winget ({exe}); this package is managed elsewhere");
    }
    let mut argv: Vec<String> = parts
        .next()
        .map(|r| r.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    argv.extend(split_switches(args));

    let direct = sysutil::run(Path::new(exe), &argv);
    match direct {
        Ok(0) => Ok(()),
        Ok(code) if code == 3010 || code == 1605 => Ok(()), // reboot pending / already gone
        Ok(code) => {
            eprintln!("  uninstall returned {code}, retrying elevated...");
            sysutil::run_elevated(Path::new(exe), &argv)
        }
        Err(e) => {
            eprintln!("  {} ({e}), retrying elevated...", exe);
            sysutil::run_elevated(Path::new(exe), &argv)
        }
    }
}
// --- command construction and selection tests ---------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::ManifestRow;
    use crate::manifest::{Dependency, InstallerSwitches};
    use crate::resolve::Choice;
    use std::path::Path;

    fn manifest(kind: &str, switches: InstallerSwitches, modes: &[&str]) -> Manifest {
        Manifest {
            id: "Publisher.Name".into(),
            version: "1.2.3".into(),
            publisher: Some("Example".into()),
            homepage: None,
            min_os_version: None,
            release_date: None,
            default_locale: None,
            dependencies: Vec::new(),
            tags: Vec::new(),
            commands: Vec::new(),
            upgrade_behavior: None,
            installer_type: Some(kind.into()),
            scope: None,
            install_modes: modes.iter().map(|s| s.to_string()).collect(),
            install_locations: Vec::new(),
            elevation_required: false,
            installers: vec![],
            switches,
            product_code: None,
            upgrade_code: None,
            uninstall_args: None,
            kind: kind.into(),
            url_dir: String::new(),
            url_base: String::new(),
        }
    }

    fn installer(arch: &str, scope: Option<&str>) -> Installer {
        Installer {
            architecture: arch.into(),
            installer_type: None,
            install_modes: Vec::new(),
            scope: scope.map(|s| s.into()),
            url: "https://example.invalid/setup.exe".into(),
            sha256: "AABB".into(),
            product_code: None,
            upgrade_code: None,
            msix_family_name: None,
            nested_installer_type: None,
            display_name: None,
            elevation_required: false,
            file_size: 0,
        }
    }

    fn row() -> ManifestRow {
        ManifestRow {
            rowid: 1,
            id: "Publisher.Name".into(),
            name: "Name".into(),
            version: "1.2.3".into(),
            dir: String::new(),
            base: "Publisher.Name".into(),
            moniker: String::new(),
            kind_token: "2035".into(),
        }
    }

    fn choice(m: &Manifest, inst: Installer, mode: &str, scope: &str) -> Choice {
        Choice {
            row: row(),
            manifest: m.clone(),
            installer: inst,
            mode: mode.into(),
            scope: scope.into(),
            verbose: false,
        }
    }

    fn sw(silent: &str, silent_with_progress: &str, interactive: &str) -> InstallerSwitches {
        InstallerSwitches {
            silent: (!silent.is_empty()).then(|| silent.into()),
            silent_with_progress: (!silent_with_progress.is_empty())
                .then(|| silent_with_progress.into()),
            interactive: (!interactive.is_empty()).then(|| interactive.into()),
        }
    }

    // ------------------------------------------------------------------ msiexec

    #[test]
    fn msi_logging_switch_is_a_separate_argument() {
        // Regression: as one argument the OS quotes it and msiexec rejects the whole
        // command line with 1639, printing its usage instead of installing.
        let m = manifest("msi", InstallerSwitches::default(), &["silent"]);
        let c = choice(&m, installer("x64", Some("user")), "silent", "user");
        let log = Path::new("C:\\tmp\\name with spaces.log");
        let (program, args) = msi_command(&m, &c, Path::new("C:\\tmp\\app.msi"), log);

        assert_eq!(program, "msiexec.exe");
        assert!(args.contains(&"/L*v".to_string()));
        let log_idx = args.iter().position(|a| a == "/L*v").unwrap();
        assert_eq!(
            args[log_idx + 1],
            log.display().to_string(),
            "the log path must be its own argument, not glued to the switch"
        );
        // Nothing may contain an embedded space in a switch-value position.
        assert!(!args.iter().any(|a| a.starts_with("/L*v ")));
    }

    #[test]
    fn msi_silent_uses_qn_and_progress_uses_passive() {
        let m = manifest("msi", InstallerSwitches::default(), &["silent"]);
        let file = Path::new("C:\\tmp\\app.msi");

        let silent = choice(&m, installer("x64", Some("user")), "silent", "user");
        let (_, a) = msi_command(&m, &silent, file, Path::new("l.log"));
        assert!(a.contains(&"/qn".to_string()));
        assert!(!a.contains(&"/passive".to_string()));

        let progress = choice(
            &m,
            installer("x64", Some("user")),
            "silentWithProgress",
            "user",
        );
        let (_, a) = msi_command(&m, &progress, file, Path::new("l.log"));
        assert!(a.contains(&"/passive".to_string()));
        assert!(!a.contains(&"/qn".to_string()));
    }

    #[test]
    fn msi_scope_maps_to_allusers() {
        let m = manifest("msi", InstallerSwitches::default(), &["silent"]);
        let file = Path::new("C:\\tmp\\app.msi");

        let machine = choice(&m, installer("x64", Some("machine")), "silent", "machine");
        let (_, a) = msi_command(&m, &machine, file, Path::new("l.log"));
        assert!(a.contains(&"ALLUSERS=1".to_string()));
        assert!(!a.contains(&"MSIINSTALLPERUSER=1".to_string()));

        let user = choice(&m, installer("x64", Some("user")), "silent", "user");
        let (_, a) = msi_command(&m, &user, file, Path::new("l.log"));
        assert!(a.contains(&"ALLUSERS=2".to_string()));
        assert!(a.contains(&"MSIINSTALLPERUSER=1".to_string()));
    }

    #[test]
    fn msi_manifest_switches_are_appended_verbatim() {
        let m = manifest("wix", sw("/qn REBOOT=0", "", "/qb"), &["silent"]);
        let c = choice(&m, installer("x64", Some("user")), "silent", "user");
        let (_, args) = msi_command(&m, &c, Path::new("a.msi"), Path::new("l.log"));
        assert!(args.contains(&"/qn".to_string()) && args.contains(&"REBOOT=0".to_string()));
    }

    #[test]
    fn msi_never_repeats_reboot_property() {
        // /norestart already means REBOOT=ReallySuppress; passing both made the
        // command line carry the property twice.
        let m = manifest("msi", InstallerSwitches::default(), &["silent"]);
        let c = choice(&m, installer("x64", Some("user")), "silent", "user");
        let (_, args) = msi_command(&m, &c, Path::new("a.msi"), Path::new("l.log"));
        assert_eq!(
            args.iter().filter(|a| a.contains("ReallySuppress")).count(),
            0
        );
        assert!(args.contains(&"/norestart".to_string()));
    }

    // ------------------------------------------------------------------- exe

    #[test]
    fn exe_uses_the_switch_for_the_selected_mode() {
        let m = manifest(
            "inno",
            sw("/VERYSILENT /NORESTART /SP-", "/SILENT /SP-", ""),
            &["silent", "silentWithProgress"],
        );
        let file = Path::new("C:\\tmp\\setup.exe");

        let silent = choice(&m, installer("x64", None), "silent", "user");
        let (_, args) = exe_command(&m, &silent, file);
        assert!(args.contains(&"/VERYSILENT".to_string()));

        let progress = choice(&m, installer("x64", None), "silentWithProgress", "user");
        let (_, args) = exe_command(&m, &progress, file);
        assert!(args.contains(&"/SILENT".to_string()));
        assert!(!args.contains(&"/VERYSILENT".to_string()));
    }

    #[test]
    fn exe_interactive_passes_no_switches() {
        let m = manifest("inno", sw("/VERYSILENT", "/SILENT", ""), &["interactive"]);
        let c = choice(&m, installer("x64", None), "interactive", "user");
        let (_, args) = exe_command(&m, &c, Path::new("setup.exe"));
        assert!(
            args.is_empty(),
            "interactive mode must not invent switches: {args:?}"
        );
    }

    #[test]
    fn burn_arguments_do_not_contradict_each_other() {
        // The manifest may already carry /install and /passive; a Burn bundle acts
        // on whichever it sees last, so duplicates are removed before ours are added.
        let m = manifest(
            "burn",
            sw("/install /passive", "/install /passive", ""),
            &["silent"],
        );
        let c = choice(&m, installer("x64", None), "silent", "machine");
        let (_, args) = exe_command(&m, &c, Path::new("setup.exe"));

        assert_eq!(args.iter().filter(|a| *a == "/install").count(), 1);
        assert_eq!(args.iter().filter(|a| *a == "/quiet").count(), 1);
        assert_eq!(args.iter().filter(|a| *a == "/passive").count(), 0);
        assert!(args.contains(&"/norestart".to_string()));
    }

    // ------------------------------------------------------- dependency ordering

    #[test]
    fn dependency_minimum_version_is_carried_on_the_dependency() {
        let m = manifest("msi", InstallerSwitches::default(), &["silent"]);
        let with_dep = Manifest {
            dependencies: vec![Dependency {
                id: "Publisher.Deps".into(),
                version: Some("2.5.0".into()),
            }],
            ..m
        };
        assert_eq!(with_dep.dependencies[0].version.as_deref(), Some("2.5.0"));
    }
}
