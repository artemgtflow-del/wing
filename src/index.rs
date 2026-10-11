use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::vercmp;

/// Path segment token that identifies a classic `*.installer.yaml` manifest.
pub const INSTALLER_TOKEN: &str = "2035";

/// One row of the `manifest` table, with the repository path already rebuilt.
#[derive(Clone, Debug)]
pub struct ManifestRow {
    pub rowid: i64,
    pub id: String,
    pub name: String,
    pub version: String,
    /// `manifests/m/Microsoft/PowerShell/7.4.20.0`
    pub dir: String,
    /// manifest file base name - the package identifier, plus the moniker when present
    pub base: String,
    /// moniker such as `GPL.Shared.5.1` (empty for stable packages)
    pub moniker: String,
    /// path-parts token naming the manifest kind (`2035` = installer)
    pub kind_token: String,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub id: String,
    pub name: String,
    pub version: String,
}

struct Catalog {
    /// (rowid, package id)
    ids: Vec<(i64, String)>,
    /// (rowid, display name)
    names: Vec<(i64, String)>,
    /// (rowid, pre-normalised display name) - avoids re-normalising on every query
    norm_names: Vec<(i64, String)>,
}

pub struct Index {
    pub path: PathBuf,
    conn: Connection,
    catalog: OnceCell<Catalog>,
}

fn uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file:{s}?immutable=1")
    } else {
        format!("file:/{s}?immutable=1")
    }
}

impl Index {
    pub fn open(path: &Path) -> Result<Index> {
        if !path.exists() {
            bail!(
                "index not found at {}\nrun `wing sync` first (downloads ~20 MB from the WinGet CDN)",
                path.display()
            );
        }
        let conn = Connection::open_with_flags(
            &uri(path),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("opening {}", path.display()))?;

        let has: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='manifest'",
            [],
            |r| r.get(0),
        )?;
        if has == 0 {
            bail!(
                "{} is not a full WinGet index (missing `manifest` table). Run `wing sync`.",
                path.display()
            );
        }

        let _ = conn.pragma_update(None, "mmap_size", 512 * 1024 * 1024i64);
        let _ = conn.pragma_update(None, "cache_size", -32_000i64);

        Ok(Index {
            path: path.to_path_buf(),
            conn,
            catalog: OnceCell::new(),
        })
    }

    fn catalog(&self) -> &Catalog {
        self.catalog.get_or_init(|| {
            let mut ids = Vec::new();
            if let Ok(mut st) = self.conn.prepare("SELECT rowid, id FROM ids") {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                    for row in rows.flatten() {
                        ids.push(row);
                    }
                }
            }
            let mut names = Vec::new();
            if let Ok(mut st) = self.conn.prepare("SELECT rowid, name FROM names") {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                    for row in rows.flatten() {
                        names.push(row);
                    }
                }
            }
            let norm_names = names
                .iter()
                .map(|(r, n)| (*r, normalize(n)))
                .collect();
            Catalog { ids, names, norm_names }
        })
    }

    /// Number of manifest rows (one per package + version + architecture).
    pub fn manifest_count(&self) -> i64 {
        self.conn
            .query_row("SELECT count(*) FROM manifest", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Number of distinct packages.
    pub fn packages(&self) -> i64 {
        self.conn
            .query_row("SELECT count(DISTINCT id) FROM manifest", [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn catalog_ids(&self) -> Vec<(i64, String)> {
        self.catalog().ids.clone()
    }

    /// Split a package identifier into (publisher, name) using the first dot.
    pub fn split_id(id: &str) -> (String, String) {
        match id.split_once('.') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (String::new(), id.to_string()),
        }
    }

    /// All manifest rows for a package id, newest version first.
    pub fn manifests(&self, id: &str) -> Result<Vec<ManifestRow>> {
        let mut st = self.conn.prepare(
            "SELECT m.rowid, m.id, m.name, m.version, m.pathpart, m.moniker
             FROM manifest m JOIN ids i ON i.rowid = m.id
             WHERE i.id = ?1",
        )?;
        let rows = st.query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows.flatten() {
            let (rowid, _id_row, name_row, ver_row, pathpart, moniker_row) = row;
            let name: String = self
                .conn
                .query_row("SELECT name FROM names WHERE rowid=?1", [name_row], |r| r.get(0))
                .unwrap_or_else(|_| id.to_string());
            let version: String = self
                .conn
                .query_row("SELECT version FROM versions WHERE rowid=?1", [ver_row], |r| r.get(0))
                .unwrap_or_default();
            let moniker: String = moniker_row
                .and_then(|m| {
                    self.conn
                        .query_row("SELECT moniker FROM monikers WHERE rowid=?1", [m], |r| r.get(0))
                        .ok()
                })
                .unwrap_or_default();
            let (dir, kind_token) = self.resolve_path(id, &version, pathpart);
            out.push(ManifestRow {
                rowid,
                id: id.to_string(),
                name,
                version,
                dir,
                base: id.to_string(),
                moniker,
                kind_token,
            });
        }
        out.sort_by(|a, b| {
            vercmp::cmp(&b.version, &a.version)
                .then_with(|| a.name.cmp(&b.name))
        });

        // One package+version can have several manifest kinds (installer, msix,
        // zip...). Keep one row per version, preferring the classic installer one.
        out.dedup_by(|a, b| {
            if a.version == b.version {
                if a.kind_token == INSTALLER_TOKEN && b.kind_token != INSTALLER_TOKEN {
                    // keep `a`
                    return false;
                }
                return true;
            }
            false
        });
        Ok(out)
    }

    /// Rebuilds the winget-pkgs directory for a manifest row.
    fn resolve_path(&self, id: &str, version: &str, pathpart: i64) -> (String, String) {
        // Walk the `pathparts` linked list: manifests -> <l> -> Publisher -> Name -> Version -> token
        let mut parts: Vec<String> = Vec::new();
        let mut cur = pathpart;
        let mut guard = 0;
        while cur != 0 && guard < 32 {
            guard += 1;
            let row: Option<(Option<i64>, String)> = self
                .conn
                .query_row(
                    "SELECT parent, pathpart FROM pathparts WHERE rowid=?1",
                    [cur],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            match row {
                Some((parent, part)) => {
                    parts.push(part);
                    cur = parent.unwrap_or(0);
                }
                None => break,
            }
        }
        parts.reverse();
        if parts.len() >= 5 && parts[0] == "manifests" {
            // manifests/<l>/<Publisher>/<Name>[/<Moniker...>]/<Version>/<kind-token>
            let token = parts.pop().unwrap_or_default();
            (parts.join("/"), token)
        } else {
            // Fallback: derive from the package identifier.
            let (publisher, nm) = Index::split_id(id);
            let letter = publisher
                .chars()
                .next()
                .map(|c| c.to_ascii_lowercase().to_string())
                .unwrap_or_else(|| "0".into());
            (
                format!("manifests/{letter}/{publisher}/{nm}/{version}"),
                INSTALLER_TOKEN.to_string(),
            )
        }
    }

    /// Search across ids, display names, tags, commands, PFNs and product codes.
    pub fn search(
        &self,
        query: &str,
        filter: SearchFilter,
        exact: bool,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        let mut id_rowids: Vec<(u8, i64)> = Vec::new();

        match &filter {
            SearchFilter::Tag(tag) => {
                for r in self.ids_by_map("tags_map", "tags", "tag", tag)? {
                    id_rowids.push((3, r));
                }
            }
            SearchFilter::Command(cmd) => {
                for r in self.ids_by_map("commands_map", "commands", "command", cmd)? {
                    id_rowids.push((3, r));
                }
            }
            SearchFilter::Pfn(p) => {
                for r in self.ids_by_map("pfns_map", "pfns", "pfn", p)? {
                    id_rowids.push((3, r));
                }
            }
            SearchFilter::ProductCode(c) => {
                for r in self.ids_by_map("productcodes_map", "productcodes", "productcode", c)? {
                    id_rowids.push((3, r));
                }
            }
            SearchFilter::None | SearchFilter::Id => {
                let q = query.trim().to_lowercase();
                if q.is_empty() {
                    bail!("empty search query");
                }
                let qn = normalize(&q);
                for (rowid, id) in self.catalog_ids() {
                    let lid = id.to_lowercase();
                    let score = if lid == q {
                        0u8
                    } else if filter == SearchFilter::Id {
                        continue;
                    } else if lid.starts_with(&q) {
                        1
                    } else if exact && lid == q {
                        0
                    } else if !exact && lid.contains(&q) {
                        2
                    } else {
                        255
                    };
                    if score != 255 {
                        id_rowids.push((score, rowid));
                    }
                }
                if filter == SearchFilter::None && !exact {
                    // One batched query instead of one lookup per matching name.
                    let mut name_rows: Vec<i64> = Vec::new();
                    for (rowid, norm) in &self.catalog().norm_names {
                        if norm.contains(&qn) {
                            name_rows.push(*rowid);
                            if name_rows.len() >= 4000 {
                                break;
                            }
                        }
                    }
                    for r in self.ids_by_name_rows(&name_rows) {
                        id_rowids.push((2, r));
                    }
                }
            }
        }

        // Stable sort by score, then dedupe id rowids.
        id_rowids.sort_by_key(|(s, _)| *s);
        let mut seen = std::collections::HashSet::new();
        id_rowids.retain(|(_, r)| seen.insert(*r));

        // Resolve to (name, latest version).
        let meta = self.latest_for(id_rowids.iter().map(|(_, r)| *r).take(4000).collect::<Vec<_>>());

        let mut hits: Vec<Hit> = Vec::new();
        for (score, rowid) in id_rowids {
            let Some(id) = meta.0.get(&rowid) else { continue };
            let name = meta.1.get(&rowid).cloned().unwrap_or_else(|| id.clone());
            let version = meta.2.get(&rowid).cloned().unwrap_or_default();
            hits.push(Hit {
                id: id.clone(),
                name,
                version,
            });
            let _ = score;
            if hits.len() >= limit * 4 {
                break;
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Maps `names` rowids to `ids` rowids in a single query.
    fn ids_by_name_rows(&self, name_rows: &[i64]) -> Vec<i64> {
        let mut out = Vec::new();
        for chunk in name_rows.chunks(400) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT DISTINCT id FROM manifest WHERE name IN ({})",
                placeholders.join(",")
            );
            let Ok(mut st) = self.conn.prepare(&sql) else {
                continue;
            };
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            let Ok(rows) = st.query_map(params.as_slice(), |r| r.get::<_, i64>(0)) else {
                continue;
            };
            out.extend(rows.flatten());
        }
        out
    }

    fn ids_by_map(
        &self,
        map_table: &str,
        value_table: &str,
        value_col: &str,
        value: &str,
    ) -> Result<Vec<i64>> {
        let sql = format!(
            "SELECT DISTINCT m.id FROM {map_table} a
             JOIN {value_table} v ON v.rowid = a.{value_col}
             JOIN manifest m ON m.rowid = a.manifest
             WHERE v.{value_col} = ?1"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([value], |r| r.get::<_, i64>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Returns (ids, names, versions) keyed by manifest.id rowid, latest version wins.
    fn latest_for(&self, id_rowids: Vec<i64>) -> (HashMap<i64, String>, HashMap<i64, String>, HashMap<i64, String>) {
        let mut ids = HashMap::new();
        let mut names = HashMap::new();
        let mut versions: HashMap<i64, String> = HashMap::new();
        if id_rowids.is_empty() {
            return (ids, names, versions);
        }
        // Chunked IN() to stay below the SQLITE_MAX_VARIABLE_NUMBER limit.
        for chunk in id_rowids.chunks(400) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT m.id, m.name, m.version FROM manifest m WHERE m.id IN ({})",
                placeholders.join(",")
            );
            let Ok(mut st) = self.conn.prepare(&sql) else { continue };
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            let Ok(rows) = st.query_map(params.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
            }) else {
                continue;
            };
            for (id_row, name_row, ver_row) in rows.flatten() {
                let name: String = self
                    .conn
                    .query_row("SELECT name FROM names WHERE rowid=?1", [name_row], |r| r.get(0))
                    .unwrap_or_default();
                let version: String = self
                    .conn
                    .query_row("SELECT version FROM versions WHERE rowid=?1", [ver_row], |r| r.get(0))
                    .unwrap_or_default();
                ids.insert(id_row, String::new()); // placeholder, filled below
                names.insert(id_row, name);
                match versions.get(&id_row) {
                    Some(cur) if vercmp::cmp(&version, cur) != std::cmp::Ordering::Greater => {}
                    _ => {
                        versions.insert(id_row, version);
                    }
                }
            }
        }
        // map rowid -> package id
        let unresolved: Vec<i64> = ids.keys().copied().collect();
        if !unresolved.is_empty() {
            let placeholders: Vec<String> = (1..=unresolved.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT rowid, id FROM ids WHERE rowid IN ({})",
                placeholders.join(",")
            );
            if let Ok(mut st) = self.conn.prepare(&sql) {
                let params: Vec<&dyn rusqlite::ToSql> =
                    unresolved.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                if let Ok(rows) = st.query_map(params.as_slice(), |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                }) {
                    for (rowid, id) in rows.flatten() {
                        ids.insert(rowid, id);
                    }
                }
            }
        }
        (ids, names, versions)
    }

    /// Fast lookup of a single package by exact identifier.
    pub fn find_by_id(&self, id: &str) -> Result<Vec<ManifestRow>> {
        let mut rows = self.manifests(id)?;
        if rows.is_empty() {
            // Case-insensitive retry.
            let hit = self.search(id, SearchFilter::Id, true, 1)?;
            if let Some(h) = hit.first() {
                rows = self.manifests(&h.id)?;
            }
        }
        Ok(rows)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchFilter {
    None,
    Id,
    Tag(String),
    Command(String),
    Pfn(String),
    ProductCode(String),
}

pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

impl Index {
}