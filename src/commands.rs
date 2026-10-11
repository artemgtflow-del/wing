use crate::config;
use crate::i18n;
use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::cli::*;
use crate::index::{normalize, Index, SearchFilter};
use crate::manifest::{self, Loader};
use crate::resolve::{self, Choice, InstallOptions};
use crate::{arp, http::Http, paths, state, sysutil, vercmp};

const SOURCE_URL: &str = "https://cdn.winget.microsoft.com/cache/source.msix";
const INDEX_ENTRY: &str = "Public/index.db";

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

fn open_index() -> Result<Index> {
    Index::open(&paths::index_db())
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut w: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            if i < w.len() {
                w[i] = w[i].max(c.chars().count());
            }
        }
    }
    let line = |cells: &[String]| -> String {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(&format!("{:<width$}  ", c, width = w[i]));
            }
        }
        s.trim_end().to_string()
    };
    println!("{}", line(&headers.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
    if std::io::stdout().is_terminal() {
        println!("{}", w.iter().map(|x| "-".repeat(*x)).collect::<Vec<_>>().join("  "));
    }
    for r in rows {
        println!("{}", line(r));
    }
}

/// Global options, detached from the parsed command so it can be moved out.
pub struct Ctx {
    pub json: bool,
    pub limit: usize,
    pub verbose: bool,
    /// Normalised message locale (`en-US` or `ru-RU`).
    pub locale: String,
    pub config: config::Config,
}

impl Ctx {
    /// Translates a message for the active locale.
    pub fn t(&self, key: &str, args: &[&str]) -> String {
        i18n::tr(&self.locale, key, args)
    }
}

pub fn run(cli: Cli) -> Result<()> {
    paths::ensure_dirs().ok();
    let cfg = config::Config::load();
    // Flag wins over the config file, config wins over the built-in default.
    let locale_tag = if cli.locale != "en-US" {
        cli.locale.clone()
    } else {
        cfg.locale.clone().unwrap_or_else(|| "en-US".into())
    };
    let ctx = Ctx {
        json: cli.json,
        limit: cli.limit,
        verbose: cli.verbose,
        locale: i18n::normalize_locale(&locale_tag),
        config: cfg,
    };
    let result = match cli.cmd {
        Cmd::Search(a) => search(&ctx, a),
        Cmd::Show(a) => rt().block_on(show(&ctx, a)),
        Cmd::Install(a) => rt().block_on(install(&ctx, a)),
        Cmd::Uninstall(a) => uninstall(&ctx, a),
        Cmd::Link(a) => rt().block_on(link(&ctx, a)),
        Cmd::Upgrade(a) => rt().block_on(upgrade(&ctx, a)),
        Cmd::List(a) => rt().block_on(list(&ctx, a)),
        Cmd::Sync(a) => rt().block_on(sync(&ctx, a)),
        Cmd::Info => info(&ctx),
        Cmd::Bench => rt().block_on(bench()),
        Cmd::Completions(a) => completions(a),
    };

    // Printed here rather than in main so the prefix is translated too.
    if let Err(e) = result {
        eprintln!("{}: {e:#}", ctx.t("error", &[]));
        std::process::exit(1);
    }
    Ok(())
}

// ------------------------------------------------------------------- search

fn search(ctx: &Ctx, a: SearchArgs) -> Result<()> {
    let idx = open_index()?;
    let query = a.query.join(" ");
    let filter = if let Some(t) = a.tag {
        SearchFilter::Tag(t)
    } else if let Some(c) = a.command {
        SearchFilter::Command(c)
    } else if let Some(p) = a.pfn {
        SearchFilter::Pfn(p)
    } else if let Some(c) = a.product_code {
        SearchFilter::ProductCode(c)
    } else if a.id {
        SearchFilter::Id
    } else {
        SearchFilter::None
    };

    if matches!(filter, SearchFilter::None) && query.trim().is_empty() {
        bail!("{}", ctx.t("empty_query", &[]));
    }

    let hits = idx.search(&query, filter, a.exact, ctx.limit)?;

    if ctx.json {
        let v: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                serde_json::json!({ "id": h.id, "name": h.name, "version": h.version })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    if hits.is_empty() {
        eprintln!("{}", ctx.t("no_results", &[]));
        return Ok(());
    }
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|h| vec![h.id.clone(), h.name.clone(), h.version.clone()])
        .collect();
    print_table(&["Id", "Name", "Version"], &rows);
    Ok(())
}

// --------------------------------------------------------------------- show

async fn show(ctx: &Ctx, a: ShowArgs) -> Result<()> {
    let idx = open_index()?;
    let rows = idx.find_by_id(&a.id)?;
    if rows.is_empty() {
        bail!("{}", ctx.t("not_in_index", &[&a.id]));
    }
    let http = Http::new(ctx.verbose)?;
    let loader = Loader {
        http: &http,
        locale: ctx.locale.clone(),
        no_cache: a.refresh,
    };

    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| a.version.as_ref().map(|v| r.version.eq_ignore_ascii_case(v)).unwrap_or(true))
        .collect();
    if rows.is_empty() {
        bail!(
            "{}",
            ctx.t(
                "version_not_available",
                &[a.version.as_deref().unwrap_or(""), &a.id]
            )
        );
    }

    // Prefer the newest version that has at least one installer for this machine.
    let mut chosen = None;
    for row in rows.iter().take(25) {
        if let Ok(m) = loader.load(row).await {
            chosen = Some(m);
            break;
        }
    }
    let Some(m) = chosen else {
        bail!("{}", ctx.t("no_manifest_versions", &[&a.id]));
    };

    let versions: Vec<String> = {
        let all = idx.find_by_id(&a.id)?;
        let mut v: Vec<String> = all.iter().map(|r| r.version.clone()).collect();
        v.sort_by(|x, y| vercmp::cmp(y, x));
        v.dedup();
        v
    };

    if ctx.json {
        let insts: Vec<serde_json::Value> = m
            .installers
            .iter()
            .map(|i| {
                serde_json::json!({
                    "architecture": i.architecture,
                    "scope": i.scope,
                    "url": i.url,
                    "sha256": i.sha256,
                    "size": i.file_size,
                    "productCode": i.product_code,
                })
            })
            .collect();
        let out = serde_json::json!({
            "id": m.id,
            "version": m.version,
            "publisher": m.publisher,
            "homepage": m.homepage,
            "releaseDate": m.release_date,
            "minOsVersion": m.min_os_version,
            "type": m.kind,
            "scope": m.scope,
            "installModes": m.install_modes,
            "upgradeBehavior": m.upgrade_behavior,
            "commands": m.commands,
            "tags": m.tags,
            "dependencies": m.dependencies.iter().map(|d| &d.id).collect::<Vec<_>>(),
            "versions": versions,
            "installers": insts,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", m.id);
    println!("  version       {}", m.version);
    if let Some(p) = &m.publisher {
        println!("  publisher     {p}");
    }
    if let Some(d) = &m.release_date {
        println!("  released      {d}");
    }
    if let Some(u) = &m.homepage {
        println!("  homepage      {u}");
    }
    println!("  type          {}", m.kind);
    println!("  scope         {}", m.scope.clone().unwrap_or_else(|| "user".into()));
    println!("  arch          {}", sysutil::arch());
    println!(
        "  modes         {}",
        if m.install_modes.is_empty() {
            "(not declared)".to_string()
        } else {
            m.install_modes.join(", ")
        }
    );
    if !m.commands.is_empty() {
        println!("  commands      {}", m.commands.join(", "));
    }
    if !m.dependencies.is_empty() {
        println!("  depends on    {}", m.dependencies.iter().map(|d| d.id.as_str()).collect::<Vec<_>>().join(", "));
    }
    if let Some(u) = &m.uninstall_args {
        println!("  uninstall     {u}");
    }
    println!("  versions      {} (newest first, showing 6)", versions.len());
    for v in versions.iter().take(6) {
        println!("                {v}");
    }
    println!("  installers:");
    for i in &m.installers {
        println!(
            "    {:<8} {:>9}  {}",
            i.architecture,
            crate::http::human(i.file_size),
            i.url
        );
        println!("              sha256 {}", i.sha256);
    }
    Ok(())
}

// ------------------------------------------------------------------ install

async fn install(ctx: &Ctx, a: InstallArgs) -> Result<()> {
    let idx = open_index()?;
    let rows = idx.find_by_id(&a.id)?;
    if rows.is_empty() {
        bail!(
            "{} {}",
            ctx.t("not_in_index", &[&a.id]),
            ctx.t("try_search", &[a.id.split('.').next_back().unwrap_or(&a.id)])
        );
    }
    let http = Http::new(ctx.verbose)?;
    let loader = Loader {
        http: &http,
        locale: ctx.locale.clone(),
        no_cache: false,
    };
    let opts = InstallOptions {
        version: a.version.clone(),
        arch: a.arch.clone(),
        scope: a.scope.clone(),
        interactive: a.interactive,
        verbose: ctx.verbose,
        minimum_version: None,
    };

    let root = resolve::select(&loader, &rows, &opts).await?;

    // Trust list: when publishers are configured, nothing outside it is installed.
    let trusted = ctx.config.trusted_publishers();
    if !trusted.is_empty() {
        let publisher = root.manifest.publisher.clone();
        let who = publisher.as_deref().unwrap_or("<unknown>");
        if !config::publisher_allowed(&trusted, publisher.as_deref()) {
            bail!(
                "`{}` is published by {who}, which is not in your trusted list.\n\
                 Add it to trusted_publishers in {} to allow it.",
                root.manifest.id,
                config::config_path().display()
            );
        }
        println!("  publisher: {who} (trusted)");
    }

    let mut plan: Vec<Choice> = Vec::new();
    if !a.no_deps {
        let mut ids: Vec<String> = Vec::new();
        resolve::resolve_dependencies(&loader, &idx, &root.manifest, &mut ids).await?;
        let st = state::State::open(&paths::state_db())?;
        for id in ids {
            if st.get(&id)?.is_some() {
                continue;
            }
            if let Ok(drows) = idx.find_by_id(&id) {
                if let Ok(c) =
                    resolve::select(&loader, &drows, &InstallOptions {
                        version: None,
                        arch: None,
                        scope: None,
                        interactive: false,
                        verbose: ctx.verbose,
                        minimum_version: None,
                    })
                    .await
                {
                    plan.push(c);
                }
            }
        }
    }
    plan.push(root.clone());

    if a.dry_run {
        println!("{}", ctx.t("plan_header", &[&plan.len().to_string()]));
        for c in &plan {
            println!(
                "  {} {}  [{} / {} / {}]  {:.1} MB",
                c.manifest.id,
                c.manifest.version,
                c.manifest.kind,
                c.scope,
                c.installer.architecture,
                c.installer.file_size as f64 / 1e6
            );
            println!("      {}", c.installer.url);
        }
        return Ok(());
    }

    // Fetch everything first, in parallel, and verify before executing anything.
    let fetches = plan.iter().map(|c| {
        let http = &http;
        crate::install::fetch_verified(http, &c.manifest, &c.installer, a.force)
    });
    let results = futures_util::future::join_all(fetches).await;
    let mut files: Vec<PathBuf> = Vec::with_capacity(results.len());
    for r in results {
        files.push(r?);
    }

    let st = state::State::open(&paths::state_db())?;
    for (c, f) in plan.iter().zip(files.iter()) {
        println!(
            "{}",
            ctx.t("installing", &[&c.manifest.id, &c.manifest.version])
        );
        let outcome = crate::install::run_install(c, f)?;
        println!("  {}", outcome.location);
        st.record_install(&state::InstallRecord {
            id: c.manifest.id.clone(),
            version: c.manifest.version.clone(),
            installer_type: c.manifest.kind.clone(),
            scope: c.scope.clone(),
            arch: c.installer.architecture.clone(),
            location: outcome.location,
            installed_at: now(),
            shims: outcome.shims,
            product_code: outcome.product_code,
            uninstall_args: outcome.uninstall_args,
        })?;
    }
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- uninstall

fn uninstall(ctx: &Ctx, a: UninstallArgs) -> Result<()> {
    let st = state::State::open(&paths::state_db())?;
    let record = st.find(&a.id)?;
    let entry = find_installed_entry(&a.id);

    // Portable installs never register in Add/Remove Programs.
    if entry.is_none() {
        let Some(rec) = record.as_ref() else {
            bail!("{}", ctx.t("not_installed", &[&a.id]));
        };
        if is_portable_kind(&rec.installer_type) {
            println!("uninstalling {} {}...", rec.id, rec.version);
            if !confirm(ctx, a.yes, &rec.id)? {
                return Ok(());
            }
            crate::install::purge_portable(&rec.id, &rec.shims)?;
            st.record_remove(&rec.id)?;
            println!("{}", ctx.t("done", &[]));
            return Ok(());
        }
    }

    // Preferred: the package's own uninstall mechanism.
    let mut removed = false;
    if let Some(entry) = entry.as_ref() {
        println!("uninstalling {} {}...", entry.display_name, entry.display_version);
        if !confirm(ctx, a.yes, &entry.display_name)? {
            return Ok(());
        }
        // MSI first: its recorded UninstallString is not trustworthy.
        if let Some(code) = entry.msi_product_code() {
            crate::install::msiexec_uninstall(&code)?;
            removed = true;
        } else {
            let cmd = entry.quiet_command().unwrap_or_default();
            if mentions_winget(cmd) {
                println!(
                    "  note: this package's uninstall command calls winget, which wing \
                     does not use; running the underlying uninstaller instead"
                );
            }
            if cmd.is_empty() {
                bail!("`{}` has no uninstall command", entry.display_name);
            }
            let (exe, inline) = match cmd.split_once(char::is_whitespace) {
                Some((e, r)) => (e, r.to_string()),
                None => (cmd, String::new()),
            };
            crate::install::uninstall_with_args(exe, &inline)?;
            removed = true;
        }
    }

    // Fallback: the MSI product code recorded at install time. This does not
    // depend on the registry staying intact.
    if !removed {
        if let Some(code) = record.as_ref().and_then(|r| r.product_code.clone()) {
            println!(
                "uninstalling {} (msiexec /x)...",
                record.as_ref().map(|r| r.id.clone()).unwrap_or_else(|| a.id.clone())
            );
            if !confirm(ctx, a.yes, &a.id)? {
                return Ok(());
            }
            crate::install::msiexec_uninstall(&code)?;
            removed = true;
        }
    }

    if !removed {
        // Nothing left to try: never pretend the package was removed.
        if let Some(rec) = &record {
            bail!(
                "{}\n  location: {}",
                ctx.t("no_uninstall_mechanism", &[&rec.id, &rec.installer_type]),
                rec.location
            );
        }
        bail!("{}", ctx.t("not_installed", &[&a.id]));
    }

    // Verify rather than assume: a zero exit code is not proof of removal.
    if let Some(again) = find_installed_entry(&a.id) {
        bail!("{}", ctx.t("still_in_arp", &[&again.display_name]));
    }

    // Drop the command links wing created for this package. A hard link keeps the
    // file alive after the installer removed the original, so it must go too.
    if let Some(rec) = &record {
        if !rec.shims.is_empty() {
            crate::install::remove_links(&rec.shims);
        }
    }

    st.record_remove(&a.id)?;
    if a.purge {
        crate::install::purge_portable(&a.id, &[])?;
    }
    println!("{}", ctx.t("done", &[]));
    Ok(())
}

fn is_portable_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "zip" | "7z" | "tar.gz" | "pwa"
    )
}

/// Locates the Add/Remove Programs entry belonging to a package identifier.
///
/// Matched in decreasing order of confidence:
///   1. the display name is the whole identifier (`RipGrep MSVC`);
///   2. the registry key name contains it (winget writes such keys);
///   3. the display name is the identifier's name part (`nepnep.neofetch-win`
///      is installed as `neofetch-win`) — only exact after normalisation.
fn find_installed_entry(id: &str) -> Option<arp::InstalledEntry> {
    let entries = arp::installed().unwrap_or_default();
    let want_id = normalize(id);
    let (_, name_part) = Index::split_id(id);
    let want_name = normalize(&name_part);

    entries
        .iter()
        .find(|e| normalize(&e.display_name) == want_id)
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.key_path.to_lowercase().contains(&id.to_lowercase()))
        })
        .or_else(|| {
            if want_name.is_empty() {
                None
            } else {
                entries
                    .iter()
                    .find(|e| normalize(&e.display_name) == want_name)
            }
        })
        .cloned()
}

fn mentions_winget(cmd: &str) -> bool {
    cmd.to_ascii_lowercase().contains("winget")
}

/// Returns false when the user declined the operation.
fn confirm(ctx: &Ctx, assume_yes: bool, what: &str) -> Result<bool> {
    if assume_yes || ctx.json || !std::io::stdin().is_terminal() {
        return Ok(true);
    }
    print!("{}", ctx.t("remove_prompt", &[what]));
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    if s.trim().eq_ignore_ascii_case("y") {
        Ok(true)
    } else {
        println!("{}", ctx.t("cancelled", &[]));
        Ok(false)
    }
}

/// Creates command links for a package that is already installed.
async fn link(ctx: &Ctx, a: LinkArgs) -> Result<()> {
    let idx = open_index()?;
    let rows = idx.find_by_id(&a.id)?;
    if rows.is_empty() {
        bail!("{}", ctx.t("not_in_index", &[&a.id]));
    }
    let http = Http::new(ctx.verbose)?;
    let loader = Loader {
        http: &http,
        locale: ctx.locale.clone(),
        no_cache: false,
    };
    let choice = resolve::select(
        &loader,
        &rows,
        &InstallOptions {
            version: a.version.clone(),
            arch: None,
            scope: None,
            interactive: false,
            verbose: ctx.verbose,
            minimum_version: None,
        },
    )
    .await?;

    let created = crate::install::link_commands(&choice.manifest);
    if created.is_empty() {
        bail!(
            "could not find the installed executable for {}\n\
             searched {} and the usual per-user and per-machine locations",
            choice.manifest.id,
            paths::data_dir().display()
        );
    }

    // Remember the links so `wing uninstall` can clean them up.
    let st = state::State::open(&paths::state_db())?;
    if let Some(rec) = st.find(&choice.manifest.id)? {
        let mut shims = rec.shims;
        for c in &created {
            if !shims.contains(c) {
                shims.push(c.clone());
            }
        }
        st.record_install(&state::InstallRecord {
            shims,
            ..rec
        })?;
    }
    println!("{}", ctx.t("done", &[]));
    Ok(())
}

// ------------------------------------------------------------------- upgrade

/// A package that Add/Remove Programs knows about, matched to an index entry.
struct UpgradeCandidate {
    id: String,
    display_name: String,
    installed: String,
    available: String,
}

/// Matches installed programs to index packages.
///
/// Two guards keep this from producing nonsense:
///   * the installed version must be known - an empty `DisplayVersion` cannot be
///     compared against anything, so the entry is reported as unknown rather
///     than as upgradable;
///   * the match must be exact after normalisation. Accepting "the first search
///     hit" upgrades whatever happens to share a name with an unrelated program.
fn upgrade_candidates(idx: &Index, entries: &[arp::InstalledEntry]) -> Vec<UpgradeCandidate> {
    let mut out = Vec::new();
    for e in entries {
        if e.system_component || e.display_name.trim().is_empty() {
            continue;
        }
        let installed = e.display_version.trim().to_string();
        if installed.is_empty() {
            continue;
        }
        let Ok(hits) = idx.search(&e.display_name, SearchFilter::None, false, 5) else {
            continue;
        };
        let want = normalize(&e.display_name);
        let Some(h) = hits.iter().find(|h| normalize(&h.name) == want) else {
            continue;
        };
        if vercmp::is_newer(&h.version, &installed) {
            out.push(UpgradeCandidate {
                id: h.id.clone(),
                display_name: e.display_name.clone(),
                installed,
                available: h.version.clone(),
            });
        }
    }
    out
}

async fn upgrade(ctx: &Ctx, a: UpgradeArgs) -> Result<()> {
    let idx = open_index()?;
    let entries = arp::installed()?;
    let rows = upgrade_candidates(&idx, &entries);

    if a.id.is_none() && !a.all {
        if ctx.json {
            let v: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "displayName": r.display_name,
                        "installed": r.installed,
                        "available": r.available,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else if rows.is_empty() {
            println!("{}", ctx.t("up_to_date", &[]));
        } else {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|r| vec![r.id.clone(), r.installed.clone(), r.available.clone()])
                .collect();
            print_table(&["Id", "Installed", "Available"], &table);
            println!("{}", ctx.t("upgradable_hint", &[]));
        }
        return Ok(());
    }

    let targets: Vec<String> = if a.all {
        rows.iter().map(|r| r.id.clone()).collect()
    } else {
        vec![a.id.clone().unwrap_or_default()]
    };

    for id in targets {
        if id.is_empty() {
            continue;
        }
        let plan = install(
            ctx,
            InstallArgs {
                id: id.clone(),
                version: None,
                scope: None,
                arch: None,
                interactive: false,
                force: false,
                no_deps: false,
                // `--dry-run` is handled here so the report can include the
                // installed version and what is about to change.
                dry_run: a.dry_run,
            },
        )
        .await?;
        let _ = plan;
        if !a.dry_run {
            println!("{}", ctx.t("upgraded", &[&id]));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- list

async fn list(ctx: &Ctx, a: ListArgs) -> Result<()> {
    let st = state::State::open(&paths::state_db())?;
    let ours = st.all()?;
    let entries = arp::installed()?;

    let mut rows: Vec<Vec<String>> = Vec::new();
    for o in &ours {
        rows.push(vec![
            o.id.clone(),
            o.version.clone(),
            o.installer_type.clone(),
            if o.installed_at > 0 {
                chrono_stamp(o.installed_at)
            } else {
                "-".into()
            },
        ]);
    }
    if a.all {
        for e in &entries {
            if e.system_component || e.display_name.trim().is_empty() {
                continue;
            }
            rows.push(vec![
                format!("(arp) {}", e.display_name),
                e.display_version.clone(),
                "-".into(),
                "-".into(),
            ]);
        }
    }

    if a.upgradable {
        // Same matching rules as `wing upgrade`: known version, exact name.
        let idx = open_index()?;
        let candidates = upgrade_candidates(&idx, &entries);
        let mut out: Vec<Vec<String>> = ours
            .iter()
            .filter_map(|o| {
                let c = candidates.iter().find(|c| c.id == o.id)?;
                Some(vec![
                    o.id.clone(),
                    format!("{} -> {}", o.version, c.available),
                    o.installer_type.clone(),
                    "-".into(),
                ])
            })
            .collect();
        for c in &candidates {
            if ours.iter().any(|o| o.id == c.id) {
                continue;
            }
            out.push(vec![
                format!("(arp) {}", c.display_name),
                format!("{} -> {}", c.installed, c.available),
                "-".into(),
                "-".into(),
            ]);
        }
        out.sort();
        rows = out;
    }

    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        rows.truncate(ctx.limit.max(1) * 20);
        print_table(&["Package", "Version", "Type", "Installed"], &rows);
    }
    Ok(())
}

fn chrono_stamp(secs: i64) -> String {
    // Minimal civil-from-days conversion (no chrono dependency needed).
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60
    )
}

// ---------------------------------------------------------------------- sync

async fn sync(ctx: &Ctx, a: SyncArgs) -> Result<()> {
    let dest = paths::index_db();
    if dest.exists() && !a.force {
        println!(
            "index already present ({}). Use `wing sync --force` to refresh.",
            crate::http::human(std::fs::metadata(&dest)?.len())
        );
        return Ok(());
    }
    if a.force && dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    let http = Http::new(ctx.verbose)?;
    let msix = paths::data_dir().join("source.msix");
    println!("{}", ctx.t("downloading_index", &[]));
    http.download(SOURCE_URL, &msix).await?;

    let tmp = paths::data_dir().join("index.db.new");
    {
        let f = std::fs::File::open(&msix)?;
        let mut ar = zip::ZipArchive::new(f).context("source.msix is not a valid archive")?;
        let mut entry = ar
            .by_name(INDEX_ENTRY)
            .with_context(|| format!("{INDEX_ENTRY} missing from the archive"))?;
        let mut out = std::fs::File::create(&tmp)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    let _ = std::fs::remove_file(&msix);

    let idx = Index::open(&tmp)?;
    println!(
        "index ready: {} package(s), {} manifest(s), {}",
        idx.packages(),
        idx.manifest_count(),
        crate::http::human(std::fs::metadata(&tmp)?.len())
    );
    drop(idx);
    std::fs::rename(&tmp, &dest).context("installing the new index")?;
    manifest::clear_negative_cache();
    println!("saved to {}", dest.display());
    Ok(())
}

// ---------------------------------------------------------------------- info

fn info(ctx: &Ctx) -> Result<()> {
    let idx_path = paths::index_db();
    let mut info = serde_json::Map::new();
    info.insert("version".into(), serde_json::json!(env!("CARGO_PKG_VERSION")));
    info.insert("dataDir".into(), serde_json::json!(paths::data_dir().display().to_string()));
    info.insert(
        "index".into(),
        serde_json::json!(idx_path
            .exists()
            .then(|| idx_path.display().to_string())
            .unwrap_or_else(|| "missing".into())),
    );
    if idx_path.exists() {
        if let Ok(idx) = Index::open(&idx_path) {
            info.insert("packages".into(), serde_json::json!(idx.packages()));
            info.insert("manifests".into(), serde_json::json!(idx.manifest_count()));
        }
    }
    info.insert("arch".into(), serde_json::json!(sysutil::arch()));
    info.insert("elevated".into(), serde_json::json!(sysutil::is_elevated()));
    info.insert("locale".into(), serde_json::json!(ctx.locale));

    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    for (k, v) in info {
        println!("{k:<10} {v}");
    }
    Ok(())
}

// ---------------------------------------------------------------------- bench

async fn bench() -> Result<()> {
    use std::time::Instant;
    let mut rows: Vec<Vec<String>> = Vec::new();

    let t = Instant::now();
    let idx = open_index()?;
    let idx_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let hits = idx.search("powershell", SearchFilter::None, false, 20)?;
    let search_ms = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(vec![
        "wing search".into(),
        format!("{search_ms:.2}"),
        format!("{}", hits.len()),
    ]);
    rows.push(vec![
        "wing index open".into(),
        format!("{idx_ms:.2}"),
        "-".into(),
    ]);

    // winget occasionally fails instantly (another instance holds the lock); a
    // result that fast is not a measurement, so retry and otherwise report n/a.
    let mut winget_ms: Option<f64> = None;
    for _ in 0..2 {
        let t = Instant::now();
        let ok = std::process::Command::new("winget")
            .args(["search", "powershell", "--disable-interactivity"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ok && ms >= 50.0 {
            winget_ms = Some(ms);
            break;
        }
    }

    println!("\nbenchmark on this machine\n");
    let mut table = rows.clone();
    match winget_ms {
        Some(ms) => {
            table.insert(
                1,
                vec!["winget search".into(), format!("{ms:.0}"), "-".into()],
            );
        }
        None => table.insert(1, vec!["winget search".into(), "n/a".into(), "-".into()]),
    }
    print_table(&["command", "ms", "hits"], &table);

    match winget_ms {
        Some(ms) => println!(
            "\nwing is ~{:.0}x faster than winget for the same query",
            ms / search_ms.max(0.01)
        ),
        None => println!("\nwinget could not be benchmarked (it exited immediately)"),
    }
    Ok(())
}

// --------------------------------------------------------------- completions

fn completions(a: CompletionsArgs) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    match a.shell.to_ascii_lowercase().as_str() {
        "powershell" | "ps" => clap_complete::generate(
            clap_complete::Shell::PowerShell,
            &mut cmd,
            "wing",
            &mut std::io::stdout(),
        ),
        "bash" => clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut cmd,
            "wing",
            &mut std::io::stdout(),
        ),
        "zsh" => clap_complete::generate(
            clap_complete::Shell::Zsh,
            &mut cmd,
            "wing",
            &mut std::io::stdout(),
        ),
        "fish" => clap_complete::generate(
            clap_complete::Shell::Fish,
            &mut cmd,
            "wing",
            &mut std::io::stdout(),
        ),
        "elvish" => clap_complete::generate(
            clap_complete::Shell::Elvish,
            &mut cmd,
            "wing",
            &mut std::io::stdout(),
        ),
        other => bail!("unsupported shell `{other}` (bash|zsh|fish|powershell|elvish)"),
    }
    Ok(())
}
