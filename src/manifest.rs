use anyhow::{anyhow, bail, Context, Result};
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::http::Http;
use crate::index::ManifestRow;
use crate::paths;

pub const RAW_BASE: &str = "https://raw.githubusercontent.com/microsoft/winget-pkgs/master/";
pub const GITHUB_API: &str = "https://api.github.com/repos/microsoft/winget-pkgs/contents/";

/// Suffix candidates for the installer manifest, in preference order.
const INSTALLER_KINDS: &[&str] = &[
    "installer",
    "msix",
    "zip",
    "tar.gz",
    "wix",
    "burn",
    "inno",
    "nullsoft",
    "msu",
    "pwa",
];

#[derive(Clone, Debug)]
// Fields mirror the manifest and ARP schemas. Some surface only in --json or are
// kept so future commands (trust lists, diagnostics) do not re-parse the source.
#[allow(dead_code)]
pub struct Installer {
    pub architecture: String,
    /// per-installer `InstallerType` (msi, exe, zip, msix, …)
    pub installer_type: Option<String>,
    /// per-installer `InstallModes`
    pub install_modes: Vec<String>,
    pub scope: Option<String>,
    pub url: String,
    pub sha256: String,
    pub product_code: Option<String>,
    pub upgrade_code: Option<String>,
    pub msix_family_name: Option<String>,
    pub nested_installer_type: Option<String>,
    pub display_name: Option<String>,
    pub elevation_required: bool,
    pub file_size: u64,
}

impl Installer {
    /// Best-effort file name from the download URL.
    pub fn file_name(&self) -> String {
        let path = self.url.split(['?', '#']).next().unwrap_or(&self.url);
        path.rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("installer.bin")
            .to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub struct InstallerSwitches {
    pub silent: Option<String>,
    pub silent_with_progress: Option<String>,
    pub interactive: Option<String>,
}

impl InstallerSwitches {
    pub fn for_mode(&self, mode: &str) -> Option<&str> {
        match mode {
            "silent" => self
                .silent
                .as_deref()
                .or(self.silent_with_progress.as_deref()),
            "silentWithProgress" => self
                .silent_with_progress
                .as_deref()
                .or(self.silent.as_deref()),
            _ => self.interactive.as_deref(),
        }
    }
}

#[derive(Clone, Debug)]
// Fields mirror the manifest and ARP schemas. Some surface only in --json or are
// kept so future commands (trust lists, diagnostics) do not re-parse the source.
#[allow(dead_code)]
pub struct Dependency {
    pub id: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
// Fields mirror the manifest and ARP schemas. Some surface only in --json or are
// kept so future commands (trust lists, diagnostics) do not re-parse the source.
#[allow(dead_code)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub publisher: Option<String>,
    pub homepage: Option<String>,
    pub min_os_version: Option<String>,
    pub release_date: Option<String>,
    pub default_locale: Option<String>,
    pub dependencies: Vec<Dependency>,
    pub tags: Vec<String>,
    pub commands: Vec<String>,
    pub upgrade_behavior: Option<String>,
    pub installer_type: Option<String>,
    pub scope: Option<String>,
    pub install_modes: Vec<String>,
    pub install_locations: Vec<String>,
    pub elevation_required: bool,
    pub installers: Vec<Installer>,
    pub switches: InstallerSwitches,
    pub product_code: Option<String>,
    pub upgrade_code: Option<String>,
    pub uninstall_args: Option<String>,
    pub kind: String,
    pub url_dir: String,
    pub url_base: String,
}

// ---------------------------------------------------------------- YAML helpers

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Sequence(a) => a.first().and_then(as_text),
        _ => None,
    }
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(Value::String(key.to_string()))
}

fn get_str(v: &Value, key: &str) -> Option<String> {
    field(v, key).and_then(as_text)
}

fn get_bool(v: &Value, key: &str) -> bool {
    match field(v, key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "elevateself" | "always"
        ),
        _ => false,
    }
}

fn get_seq(v: &Value, key: &str) -> Vec<Value> {
    match field(v, key) {
        Some(Value::Sequence(a)) => a.clone(),
        Some(other) => vec![other.clone()],
        None => vec![],
    }
}

fn get_str_list(v: &Value, key: &str) -> Vec<String> {
    get_seq(v, key).iter().filter_map(as_text).collect()
}

fn get_str_nested(v: &Value, parent: &str, key: &str) -> Option<String> {
    field(v, parent).and_then(|p| get_str(p, key))
}

fn parse_yaml(src: &str) -> Option<Value> {
    serde_yaml::from_str(src).ok()
}

// ---------------------------------------------------------------- cache

fn cache_path(url: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    paths::manifest_cache().join(format!("{:x}.yaml", h.finalize()))
}

/// Fetch a repo file, honouring the on-disk cache.
/// Returns `Ok(None)` on HTTP 404 so the caller can try the next candidate.
pub async fn fetch_file(http: &Http, url: &str, use_cache: bool) -> Result<Option<String>> {
    if std::env::var_os("WING_DEBUG_URLS").is_some() {
        eprintln!("  -> GET {url}");
    }
    let cp = cache_path(url);
    if use_cache {
        if let Ok(s) = std::fs::read_to_string(&cp) {
            if !s.is_empty() {
                return Ok(Some(s));
            }
        }
    }
    match http.get_string(url).await {
        Ok(body) => {
            if let Some(parent) = cp.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&cp, &body);
            Ok(Some(body))
        }
        Err(e) if e.not_found => {
            if let Some(parent) = cp.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&cp, "");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Drops negative (empty) cache entries so a freshly synced index can be retried.
pub fn clear_negative_cache() {
    if let Ok(dir) = std::fs::read_dir(paths::manifest_cache()) {
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "yaml").unwrap_or(false) {
                if let Ok(md) = std::fs::metadata(&p) {
                    if md.len() == 0 {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- parsing

pub fn parse_manifest(
    row: &ManifestRow,
    default_yaml: &str,
    installer_yaml: &str,
    locale_yaml: Option<&str>,
) -> Result<Manifest> {
    let def: Value = serde_yaml::from_str(default_yaml).context("parsing default manifest")?;
    let ins: Value = serde_yaml::from_str(installer_yaml).context("parsing installer manifest")?;

    let mut installers = Vec::new();
    for item in get_seq(&ins, "Installers") {
        let Some(url) = get_str(&item, "InstallerUrl") else {
            continue;
        };
        let sha = get_str(&item, "InstallerSha256").unwrap_or_default();
        if sha.is_empty() {
            bail!(
                "manifest {}/{} has an installer without InstallerSha256",
                row.id,
                row.version
            );
        }
        installers.push(Installer {
            architecture: get_str(&item, "Architecture").unwrap_or_else(|| "neutral".into()),
            installer_type: get_str(&item, "InstallerType"),
            install_modes: get_str_list(&item, "InstallModes"),
            url,
            sha256: sha.to_ascii_uppercase(),
            scope: get_str(&item, "Scope"),
            product_code: get_str(&item, "ProductCode"),
            upgrade_code: get_str(&item, "UpgradeCode"),
            msix_family_name: get_str(&item, "MsixFamilyName"),
            nested_installer_type: get_str(&item, "NestedInstallerType"),
            display_name: get_str(&item, "DisplayName"),
            elevation_required: get_bool(&item, "ElevationRequirement"),
            file_size: get_str(&item, "InstallerSizeInBytes")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0),
        });
    }
    if installers.is_empty() {
        bail!("manifest {}/{} declares no installers", row.id, row.version);
    }

    // `InstallerType` frequently lives on each installer entry rather than on the
    // manifest root, so fall back to the first one. Getting this right is what
    // decides whether a silent install is even possible.
    let kind = get_str(&ins, "InstallerType")
        .or_else(|| installers.iter().find_map(|i| i.installer_type.clone()))
        .or_else(|| get_str(&ins, "ManifestType"))
        .or_else(|| get_str(&def, "ManifestType"))
        .unwrap_or_else(|| "installer".into());

    let install_modes = {
        let top = get_str_list(&ins, "InstallModes");
        if top.is_empty() {
            installers
                .iter()
                .find(|i| !i.install_modes.is_empty())
                .map(|i| i.install_modes.clone())
                .unwrap_or_default()
        } else {
            top
        }
    };

    let switches = match field(&ins, "InstallerSwitches") {
        Some(Value::String(s)) => InstallerSwitches {
            silent: Some(s.clone()),
            silent_with_progress: Some(s.clone()),
            interactive: None,
        },
        Some(Value::Mapping(_)) => InstallerSwitches {
            silent: get_str_nested(&ins, "InstallerSwitches", "Silent"),
            silent_with_progress: get_str_nested(&ins, "InstallerSwitches", "SilentWithProgress"),
            interactive: get_str_nested(&ins, "InstallerSwitches", "Interactive"),
        },
        _ => InstallerSwitches::default(),
    };

    let mut dependencies = Vec::new();
    for d in get_seq(&def, "Dependencies") {
        match &d {
            Value::String(s) => dependencies.push(Dependency {
                id: s.clone(),
                version: None,
            }),
            Value::Mapping(_) => {
                if let Some(pid) = get_str(&d, "PackageIdentifier") {
                    dependencies.push(Dependency {
                        id: pid,
                        version: get_str(&d, "MinimumVersion"),
                    });
                }
            }
            _ => {}
        }
    }

    let loc = locale_yaml.and_then(parse_yaml);
    let uninstall_args = loc
        .as_ref()
        .and_then(|v| get_str(v, "Microsoft.UninstallArguments"));

    // ProductCode and UpgradeCode are frequently declared per installer rather
    // than at the manifest root; `wing uninstall` needs one of them to remove
    // the package without relying on the registry staying intact.
    let product_code = get_str(&ins, "ProductCode")
        .or_else(|| installers.iter().find_map(|i| i.product_code.clone()));
    let upgrade_code = get_str(&ins, "UpgradeCode")
        .or_else(|| installers.iter().find_map(|i| i.upgrade_code.clone()));

    Ok(Manifest {
        id: row.id.clone(),
        version: row.version.clone(),
        publisher: loc.as_ref().and_then(|v| get_str(v, "Publisher")),
        homepage: loc.as_ref().and_then(|v| get_str(v, "PackageUrl")),
        min_os_version: get_str(&def, "MinimumOSVersion"),
        release_date: get_str(&ins, "ReleaseDate").or_else(|| get_str(&def, "ReleaseDate")),
        default_locale: get_str(&def, "DefaultLocale"),
        dependencies,
        tags: get_str_list(&def, "Tags"),
        commands: get_str_list(&def, "Commands"),
        upgrade_behavior: get_str(&ins, "UpgradeBehavior"),
        installer_type: Some(kind.clone()),
        scope: get_str(&ins, "Scope"),
        install_modes,
        install_locations: get_str_list(&ins, "InstallLocations"),
        elevation_required: get_bool(&ins, "ElevationRequirement"),
        installers,
        switches,
        product_code,
        upgrade_code,
        uninstall_args,
        kind,
        url_dir: row.dir.clone(),
        url_base: row.base.clone(),
    })
}

// ---------------------------------------------------------------- loader

pub struct Loader<'a> {
    pub http: &'a Http,
    pub locale: String,
    pub no_cache: bool,
}

impl<'a> Loader<'a> {
    /// Candidate file stems. Most manifests are named `<id>.<kind>.yaml`, but
    /// packages published under a moniker can carry it in the file name.
    fn stems(row: &ManifestRow) -> Vec<String> {
        let mut v = vec![row.base.clone()];
        if !row.moniker.is_empty() {
            let mono = row.moniker.replace('/', ".");
            let with = format!("{}.{mono}", row.base);
            if with != row.base {
                v.push(with);
            }
        }
        v
    }

    pub async fn load(&self, row: &ManifestRow) -> Result<Manifest> {
        let dir = format!("{}{}/", RAW_BASE, row.dir);
        let stems = Self::stems(row);

        let mut default_yaml = None;
        for stem in &stems {
            let url = format!("{dir}{stem}.yaml");
            if let Some(body) = fetch_file(self.http, &url, !self.no_cache).await? {
                default_yaml = Some(body);
                break;
            }
        }
        let default_yaml = default_yaml.ok_or_else(|| {
            anyhow!(
                "default manifest not found for {} {} in {dir}",
                row.id,
                row.version
            )
        })?;

        let (kind, installer_yaml, used_stem) = self.pick_installer_manifest(&dir, &stems).await?;
        let locale_yaml = self.fetch_locale(&dir, &used_stem, &kind).await?;

        // NOTE: `kind` here is the manifest *file* suffix (`installer`, `msix`, ...).
        // The meaningful type for installation comes from the manifest's own
        // `InstallerType` field and is kept in `m.kind`.
        parse_manifest(row, &default_yaml, &installer_yaml, locale_yaml.as_deref())
    }

    async fn pick_installer_manifest(
        &self,
        dir: &str,
        stems: &[String],
    ) -> Result<(String, String, String)> {
        for stem in stems {
            for kind in INSTALLER_KINDS {
                let url = format!("{dir}{stem}.{kind}.yaml");
                if let Some(body) = fetch_file(self.http, &url, !self.no_cache).await? {
                    return Ok(((*kind).to_string(), body, stem.clone()));
                }
            }
        }

        // The deterministic name did not work: ask the GitHub contents API what
        // files actually live in this manifest directory.
        let api_dir = dir
            .trim_start_matches(RAW_BASE)
            .trim_end_matches('/')
            .to_string();
        let names = list_dir(self.http, &api_dir).await.unwrap_or_default();
        if names.is_empty() {
            bail!(
                "no installer manifest for {} in {dir} (tried {} name patterns)",
                api_dir,
                stems.len() * INSTALLER_KINDS.len()
            );
        }
        for kind in INSTALLER_KINDS {
            let suffix = format!(".{kind}.yaml");
            for n in &names {
                if n.ends_with(&suffix) && !n.contains(".locale.") {
                    let url = format!("{dir}{n}");
                    if let Some(body) = fetch_file(self.http, &url, !self.no_cache).await? {
                        let stem = n.trim_end_matches(&suffix).to_string();
                        return Ok(((*kind).to_string(), body, stem));
                    }
                }
            }
        }
        bail!("no installer manifest in {api_dir}")
    }

    async fn fetch_locale(&self, dir: &str, stem: &str, kind: &str) -> Result<Option<String>> {
        let locales: Vec<String> = if self.locale == "en-US" {
            vec!["en-US".into()]
        } else {
            vec![self.locale.clone(), "en-US".into()]
        };
        for loc in locales {
            for tmpl in [
                format!("{dir}{stem}.{kind}.locale.{loc}.yaml"),
                format!("{dir}{stem}.locale.{loc}.yaml"),
            ] {
                if let Some(body) = fetch_file(self.http, &tmpl, !self.no_cache).await? {
                    return Ok(Some(body));
                }
            }
        }
        Ok(None)
    }
}

/// Fallback: list manifest file names in a directory through the GitHub API.
pub async fn list_dir(http: &Http, dir: &str) -> Result<Vec<String>> {
    let url = format!("{GITHUB_API}{dir}");
    let body = http
        .get_string_with_header(&url, "application/vnd.github+json")
        .await?;
    let v: serde_json::Value =
        serde_json::from_str(&body).context("unexpected GitHub API response")?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::ManifestRow;

    fn row() -> ManifestRow {
        ManifestRow {
            rowid: 1,
            id: "Publisher.Name".into(),
            name: "Name".into(),
            version: "1.2.3".into(),
            dir: "manifests/p/Publisher/Name/1.2.3".into(),
            base: "Publisher.Name".into(),
            moniker: String::new(),
            kind_token: "2035".into(),
        }
    }

    const DEFAULT: &str = "PackageIdentifier: Publisher.Name
PackageVersion: 1.2.3
DefaultLocale: en-US
ManifestType: default
ManifestVersion: 1.6.0
MinimumOSVersion: 10.0.17763.0
Tags:
- utility
Commands:
- name
Dependencies:
- PackageIdentifier: Publisher.Deps
  MinimumVersion: 2.0.0
";

    fn parse(default_yaml: &str, installer_yaml: &str) -> crate::manifest::Manifest {
        parse_manifest(&row(), default_yaml, installer_yaml, None).expect("parse")
    }

    #[test]
    fn installer_type_declared_per_installer() {
        // `InstallerType` sits inside each Installers entry, not at the root. If this
        // is missed the tool thinks the package is a generic installer and loses the
        // ability to choose a silent mode.
        let installer = "PackageIdentifier: Publisher.Name
PackageVersion: 1.2.3
Installers:
- InstallerLocale: en-US
  Architecture: x64
  InstallerType: msi
  InstallerUrl: https://example.invalid/app.msi
  InstallerSha256: AABB
  InstallModes:
  - silentWithProgress
  ProductCode: '{11111111-2222-3333-4444-555555555555}'
ManifestType: installer
ManifestVersion: 1.5.0
";
        let m = parse(DEFAULT, installer);
        assert_eq!(m.kind, "msi");
        assert_eq!(m.install_modes, vec!["silentWithProgress"]);
        assert_eq!(
            m.product_code.as_deref(),
            Some("{11111111-2222-3333-4444-555555555555}")
        );
    }

    #[test]
    fn installer_type_at_root_is_preferred() {
        let installer = "PackageIdentifier: Publisher.Name
InstallerType: zip
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.zip
  InstallerSha256: AABB
ManifestType: installer
";
        let m = parse(DEFAULT, installer);
        assert_eq!(m.kind, "zip");
    }

    #[test]
    fn switches_as_mapping_and_as_string() {
        let mapped = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.exe
  InstallerSha256: AABB
InstallerSwitches:
  Silent: -s
  SilentWithProgress: /S
  Interactive: ''
";
        let m = parse(DEFAULT, mapped);
        assert_eq!(m.switches.silent.as_deref(), Some("-s"));
        assert_eq!(m.switches.silent_with_progress.as_deref(), Some("/S"));

        let bare = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.exe
  InstallerSha256: AABB
InstallerSwitches: /quiet
";
        let m = parse(DEFAULT, bare);
        assert_eq!(m.switches.silent.as_deref(), Some("/quiet"));
        assert_eq!(m.switches.for_mode("silent"), Some("/quiet"));
        assert_eq!(m.switches.for_mode("interactive"), None);
    }

    #[test]
    fn dependencies_with_and_without_version() {
        let installer = "InstallerType: msi
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.msi
  InstallerSha256: AABB
";
        let m = parse(DEFAULT, installer);
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].id, "Publisher.Deps");
        assert_eq!(m.dependencies[0].version.as_deref(), Some("2.0.0"));
        assert_eq!(m.commands, vec!["name"]);
        assert_eq!(m.tags, vec!["utility"]);
        assert_eq!(m.min_os_version.as_deref(), Some("10.0.17763.0"));
    }

    #[test]
    fn sha256_is_required_and_normalised() {
        let no_sha = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.exe
";
        let err = parse_manifest(&row(), DEFAULT, no_sha, None).unwrap_err();
        assert!(err.to_string().contains("InstallerSha256"), "{err}");

        let lower = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.exe
  InstallerSha256: aabbcc
";
        let m = parse(DEFAULT, lower);
        assert_eq!(m.installers[0].sha256, "AABBCC");
    }

    #[test]
    fn manifest_without_installers_is_rejected() {
        let empty = "InstallerType: msi
ManifestType: installer
";
        let err = parse_manifest(&row(), DEFAULT, empty, None).unwrap_err();
        assert!(err.to_string().contains("no installers"), "{err}");
    }

    #[test]
    fn locale_file_supplies_uninstall_arguments() {
        let installer = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/app.exe
  InstallerSha256: AABB
";
        let locale = "PackageIdentifier: Publisher.Name
Publisher: Example Ltd
PackageUrl: https://example.invalid
Microsoft.UninstallArguments: '/VERYSILENT'
";
        let m = parse_manifest(&row(), DEFAULT, installer, Some(locale)).expect("parse");
        assert_eq!(m.uninstall_args.as_deref(), Some("/VERYSILENT"));
        assert_eq!(m.publisher.as_deref(), Some("Example Ltd"));
    }

    #[test]
    fn installer_file_name_survives_query_strings() {
        let installer = "InstallerType: exe
Installers:
- Architecture: x64
  InstallerUrl: https://example.invalid/download/app.exe?token=abc
  InstallerSha256: AABB
";
        let m = parse(DEFAULT, installer);
        assert_eq!(m.installers[0].file_name(), "app.exe");
    }
}
