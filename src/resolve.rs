use anyhow::{bail, Result};

use crate::vercmp;

use crate::index::ManifestRow;
use crate::manifest::{Installer, Manifest};
use crate::sysutil;

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub version: Option<String>,
    pub arch: Option<String>,
    pub scope: Option<String>,
    pub interactive: bool,
    pub verbose: bool,
    /// Lowest acceptable version (a dependency's `MinimumVersion`). The newest
    /// version at or above it wins.
    pub minimum_version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Choice {
    pub row: ManifestRow,
    pub manifest: Manifest,
    pub installer: Installer,
    pub mode: String,
    pub scope: String,
    pub verbose: bool,
}

fn os_build() -> u32 {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
            KEY_READ | KEY_WOW64_64KEY,
        )
        .ok()
        .and_then(|k| k.get_value::<String, _>("CurrentBuildNumber").ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// MinimumOSVersion looks like `10.0.17763.0`; only the build matters to us.
fn manifest_min_build(m: &Manifest) -> Option<u32> {
    let s = m.min_os_version.as_ref()?;
    let parts: Vec<&str> = s.split('.').collect();
    parts.get(2).and_then(|b| b.parse::<u32>().ok())
}

fn arch_rank(installer_arch: &str, wanted: &str) -> Option<u8> {
    match (installer_arch, wanted) {
        (a, w) if a.eq_ignore_ascii_case(w) => Some(0),
        ("neutral", _) => Some(1),
        _ => None,
    }
}

/// Walks candidate manifests newest-first and returns the first installable one.
pub async fn select(
    loader: &crate::manifest::Loader<'_>,
    rows: &[ManifestRow],
    opts: &InstallOptions,
) -> Result<Choice> {
    if rows.is_empty() {
        bail!("package not found in the index");
    }
    let build = os_build();
    let mut candidates: Vec<&ManifestRow> = Vec::new();
    for r in rows {
        if let Some(v) = &opts.version {
            if !r.version.eq_ignore_ascii_case(v) {
                continue;
            }
        }
        if let Some(min) = &opts.minimum_version {
            // Older than the floor: not a candidate at all.
            if vercmp::cmp(&r.version, min) == std::cmp::Ordering::Less {
                continue;
            }
        }
        candidates.push(r);
    }
    if candidates.is_empty() {
        if let Some(min) = &opts.minimum_version {
            bail!(
                "{} has no version at or above the required minimum {min}",
                rows[0].id
            );
        }
        bail!(
            "version {} is not available for {}",
            opts.version.as_deref().unwrap_or("?"),
            rows[0].id
        );
    }

    let mut last_error: Option<String> = None;
    for row in candidates {
        let manifest = match loader.load(row).await {
            Ok(m) => m,
            Err(e) => {
                last_error = Some(format!("{} {}: {e:#}", row.id, row.version));
                continue;
            }
        };
        if let Some(min) = manifest_min_build(&manifest) {
            if build != 0 && min > build {
                last_error = Some(format!(
                    "{} {} requires Windows build {min} (this machine: {build})",
                    row.id, row.version
                ));
                continue;
            }
        }
        if let Some(c) = pick_installer(&manifest, opts, &row.version)? {
            return Ok(Choice {
                row: row.clone(),
                installer: c.installer,
                mode: c.mode,
                scope: c.scope,
                manifest,
                verbose: opts.verbose,
            });
        }
    }

    match last_error {
        Some(e) => bail!("no installable version found ({e})"),
        None => bail!(
            "no installer for {} matches arch={} scope={} mode={}",
            rows[0].id,
            opts.arch.as_deref().unwrap_or(sysutil::arch()),
            opts.scope.as_deref().unwrap_or("any"),
            if opts.interactive {
                "interactive"
            } else {
                "silent"
            }
        ),
    }
}

struct Picked {
    installer: Installer,
    mode: String,
    scope: String,
}

fn pick_installer(m: &Manifest, opts: &InstallOptions, version: &str) -> Result<Option<Picked>> {
    let wanted_arch = opts.arch.clone().unwrap_or_else(|| sysutil::arch().into());
    let declared = &m.install_modes;
    let has = |mode: &str| declared.iter().any(|x| x == mode);
    let mode = if opts.interactive {
        "interactive"
    } else if has("silent") {
        "silent"
    } else if has("silentWithProgress") {
        // Respect the manifest: it declares progress UI is allowed.
        "silentWithProgress"
    } else if is_quiet_by_default(m) {
        "silent"
    } else {
        "interactive"
    };

    let mut best: Option<(u8, u8, &Installer)> = None;
    for inst in &m.installers {
        let Some(arank) = arch_rank(&inst.architecture, &wanted_arch) else {
            continue;
        };
        if m.kind == "msix" {
            // MSIX is scope-less.
            best = Some((arank, 0, inst));
            break;
        }
        let want_scope = opts
            .scope
            .clone()
            .or_else(|| m.scope.clone())
            .unwrap_or_else(|| "user".into());
        let srank = match inst.scope.as_deref().unwrap_or(&want_scope) {
            s if s.eq_ignore_ascii_case(&want_scope) => 0u8,
            _ => 1,
        };
        // A silent run needs switches; otherwise we have to go interactive.
        if mode == "silent" && m.switches.for_mode("silent").is_none() && !is_quiet_by_default(m) {
            continue;
        }
        let cand = (arank, srank, inst);
        best = match best {
            None => Some(cand),
            Some((ba, bs, _)) if (arank, srank) < (ba, bs) => Some(cand),
            keep => keep,
        };
    }

    let Some((_, srank, inst)) = best else {
        return Ok(None);
    };
    let scope = inst.scope.clone().unwrap_or_else(|| {
        opts.scope
            .clone()
            .unwrap_or_else(|| m.scope.clone().unwrap_or_else(|| "user".into()))
    });
    let _ = (srank, version);
    Ok(Some(Picked {
        installer: inst.clone(),
        mode: mode.to_string(),
        scope,
    }))
}

/// Installer types that need no silent switches of their own.
fn is_quiet_by_default(m: &Manifest) -> bool {
    let k = m.kind.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "msix" | "msu" | "zip" | "tar.gz" | "pwa" | "msi" | "wix"
    )
}

/// Breadth-first dependency expansion. Fills `already` with the package ids that
/// must be installed before `root`, dependencies first.
pub async fn resolve_dependencies(
    loader: &crate::manifest::Loader<'_>,
    index: &crate::index::Index,
    root: &Manifest,
    already: &mut Vec<String>,
) -> Result<()> {
    let mut queue: std::collections::VecDeque<Manifest> = std::collections::VecDeque::new();
    queue.push_back(root.clone());

    while let Some(current) = queue.pop_front() {
        if already.len() > 200 {
            break;
        }
        for dep in &current.dependencies {
            if already.iter().any(|a| a.eq_ignore_ascii_case(&dep.id)) {
                continue;
            }
            let Ok(rows) = index.find_by_id(&dep.id) else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            // A dependency may pin a floor (`MinimumVersion`), so the newest
            // version is not always acceptable - and a pinned version is not
            // necessarily installable either.
            let wanted = dep.version.clone();
            let sub = select(
                loader,
                &rows,
                &InstallOptions {
                    version: None,
                    arch: None,
                    scope: None,
                    interactive: false,
                    verbose: false,
                    minimum_version: wanted.clone(),
                },
            )
            .await;
            match sub {
                Ok(sub) => {
                    already.push(sub.row.id.clone());
                    queue.push_back(sub.manifest);
                }
                Err(e) => {
                    // Fall back to the newest version rather than dropping the
                    // dependency silently; newer is normally acceptable.
                    if wanted.is_some() {
                        eprintln!(
                            "  note: {} could not be resolved at the requested minimum \
                             version ({e:#}), trying the newest available",
                            dep.id
                        );
                        if let Ok(sub) = select(
                            loader,
                            &rows,
                            &InstallOptions {
                                version: None,
                                arch: None,
                                scope: None,
                                interactive: false,
                                verbose: false,
                                minimum_version: None,
                            },
                        )
                        .await
                        {
                            already.push(sub.row.id.clone());
                            queue.push_back(sub.manifest);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
