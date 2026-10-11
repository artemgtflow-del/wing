use anyhow::Result;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
use winreg::RegKey;

#[derive(Clone, Debug)]
pub struct InstalledEntry {
    pub key_path: String,
    /// last segment of `key_path`; for MSI packages this is the ProductCode
    pub key_name: String,
    pub display_name: String,
    pub display_version: String,
    pub publisher: String,
    pub uninstall_string: String,
    pub quiet_uninstall_string: String,
    pub install_location: String,
    pub system_component: bool,
    pub windows_installer: bool,
}

impl InstalledEntry {
    pub fn quiet_command(&self) -> Option<&str> {
        if !self.quiet_uninstall_string.is_empty() {
            Some(&self.quiet_uninstall_string)
        } else if !self.uninstall_string.is_empty() {
            Some(&self.uninstall_string)
        } else {
            None
        }
    }

    /// MSI packages are best removed with `msiexec /x <product code>` taken from
    /// the registry key name. The `UninstallString` these packages record is
    /// unreliable - some manifests write `/I` (repair) where `/X` belongs.
    pub fn msi_product_code(&self) -> Option<String> {
        if !self.windows_installer {
            return None;
        }
        let name = self.key_name.trim_matches(['{', '}']);
        if name.len() == 36 && name.chars().all(|c| c == '-' || c.is_ascii_hexdigit()) {
            Some(name.to_ascii_uppercase())
        } else {
            None
        }
    }
}

const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL_KEY_WOW: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// Enumerates every Add/Remove Programs entry (native + WOW64 + per-user).
pub fn installed() -> Result<Vec<InstalledEntry>> {
    let mut out = Vec::new();
    collect(&RegKey::predef(HKEY_LOCAL_MACHINE), UNINSTALL_KEY, &mut out);
    collect(
        &RegKey::predef(HKEY_LOCAL_MACHINE),
        UNINSTALL_KEY_WOW,
        &mut out,
    );
    collect(&RegKey::predef(HKEY_CURRENT_USER), UNINSTALL_KEY, &mut out);
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(out)
}

fn collect(root: &RegKey, sub: &str, out: &mut Vec<InstalledEntry>) {
    let Ok(key) = root.open_subkey_with_flags(sub, KEY_READ | KEY_WOW64_64KEY) else {
        return;
    };
    for name in key.enum_keys().flatten() {
        let Ok(k) = key.open_subkey_with_flags(&name, KEY_READ | KEY_WOW64_64KEY) else {
            continue;
        };
        let get = |n: &str| -> String { k.get_value::<String, _>(n).unwrap_or_default() };
        let Some(display_name) = non_empty(k.get_value::<String, _>("DisplayName").ok()) else {
            continue;
        };
        let system_component = k
            .get_value::<u32, _>("SystemComponent")
            .map(|v| v == 1)
            .unwrap_or(false);
        out.push(InstalledEntry {
            key_path: format!("{sub}\\{name}"),
            key_name: name.clone(),
            display_name,
            display_version: get("DisplayVersion"),
            publisher: get("Publisher"),
            uninstall_string: get("UninstallString"),
            quiet_uninstall_string: get("QuietUninstallString"),
            install_location: get("InstallLocation"),
            system_component,
            windows_installer: k
                .get_value::<u32, _>("WindowsInstaller")
                .map(|v| v == 1)
                .unwrap_or(false),
        });
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}