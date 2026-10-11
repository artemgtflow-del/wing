use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

pub struct InstallRecord {
    pub id: String,
    pub version: String,
    pub installer_type: String,
    pub scope: String,
    pub arch: String,
    pub location: String,
    pub installed_at: i64,
    /// names of the command links wing created in %LOCALAPPDATA%\wing\bin
    pub shims: Vec<String>,
    /// MSI product code, so uninstall does not depend on Add/Remove Programs
    pub product_code: Option<String>,
    /// `Microsoft.UninstallArguments` from the manifest
    pub uninstall_args: Option<String>,
}

pub struct State {
    conn: Connection,
}

impl State {
    pub fn open(path: &Path) -> Result<State> {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS installs(
                id            TEXT PRIMARY KEY,
                version       TEXT NOT NULL,
                installer_type TEXT,
                scope         TEXT,
                arch          TEXT,
                location      TEXT,
                installed_at  INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);",
        )?;
        // Lightweight forward-compatible migrations.
        ensure_columns(&conn, "installs", &["shims", "product_code", "uninstall_args"])?;
        Ok(State { conn })
    }

    pub fn record_install(&self, r: &InstallRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO installs(id, version, installer_type, scope, arch, location, installed_at,
                                  shims, product_code, uninstall_args)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                version=excluded.version, installer_type=excluded.installer_type,
                scope=excluded.scope, arch=excluded.arch, location=excluded.location,
                installed_at=excluded.installed_at, shims=excluded.shims,
                product_code=excluded.product_code, uninstall_args=excluded.uninstall_args",
            params![
                r.id,
                r.version,
                r.installer_type,
                r.scope,
                r.arch,
                r.location,
                r.installed_at,
                r.shims.join(","),
                r.product_code,
                r.uninstall_args,
            ],
        )?;
        Ok(())
    }

    pub fn record_remove(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM installs WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<String>> {
        let mut st = self
            .conn
            .prepare("SELECT version FROM installs WHERE id = ?1")?;
        let mut rows = st.query(params![id])?;
        Ok(match rows.next()? {
            Some(r) => Some(r.get(0)?),
            None => None,
        })
    }

    pub fn all(&self) -> Result<Vec<InstallRecord>> {
        let mut st = self.conn.prepare(
            "SELECT id, version, installer_type, scope, arch, location, installed_at, shims,
                    product_code, uninstall_args
             FROM installs ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            let shims: String = r.get(7).unwrap_or_default();
            Ok(InstallRecord {
                id: r.get(0)?,
                version: r.get(1)?,
                installer_type: r.get(2)?,
                scope: r.get(3)?,
                arch: r.get(4)?,
                location: r.get(5)?,
                installed_at: r.get(6)?,
                shims: shims
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string())
                    .collect(),
                product_code: r.get(8).unwrap_or(None),
                uninstall_args: r.get(9).unwrap_or(None),
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// Returns the tracked record for a package, if wing installed it.
    pub fn find(&self, id: &str) -> Result<Option<InstallRecord>> {
        Ok(self
            .all()?
            .into_iter()
            .find(|r| r.id.eq_ignore_ascii_case(id)))
    }
}

fn ensure_columns(conn: &Connection, table: &str, wanted: &[&str]) -> Result<()> {
    let existing: Vec<String> = {
        let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?;
        rows.flatten().collect()
    };
    for col in wanted {
        if !existing.iter().any(|c| c == col) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} TEXT"), [])?;
        }
    }
    Ok(())
}