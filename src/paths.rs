use std::path::PathBuf;

/// Root of wing's state.
///
/// `WING_DATA_DIR` wins over the default location. This exists so tests can run
/// against a throwaway directory instead of the user's real profile.
pub fn data_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("WING_DATA_DIR") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(p) = dirs::data_local_dir() {
        p.join("wing")
    } else {
        PathBuf::from("wing-data")
    }
}

pub fn index_db() -> PathBuf {
    data_dir().join("index.db")
}

pub fn state_db() -> PathBuf {
    data_dir().join("state.db")
}

pub fn manifest_cache() -> PathBuf {
    data_dir().join("manifests")
}

pub fn downloads() -> PathBuf {
    data_dir().join("downloads")
}

pub fn apps() -> PathBuf {
    data_dir().join("apps")
}

pub fn bin() -> PathBuf {
    data_dir().join("bin")
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [data_dir(), manifest_cache(), downloads(), apps(), bin()] {
        std::fs::create_dir_all(d)?;
    }
    Ok(())
}
