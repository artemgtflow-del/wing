use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
use winreg::RegKey;

/// True when the current process has administrator rights.
pub fn is_elevated() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags("", KEY_READ | KEY_WRITE)
        .is_ok()
}

/// Makes sure `%LOCALAPPDATA%\wing\bin` is on the user PATH so that
/// `Commands:` from the manifests become runnable.
pub fn ensure_user_path() -> Result<bool> {
    let bin = crate::paths::bin();
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    let current: String = key.get_value("Path").unwrap_or_default();
    let already = current
        .split(';')
        .any(|p| p.trim().eq_ignore_ascii_case(&bin.to_string_lossy()));
    if already {
        return Ok(false);
    }
    let mut new = current.trim_end_matches(';').to_string();
    if !new.is_empty() {
        new.push(';');
    }
    new.push_str(&bin.to_string_lossy());
    key.set_value("Path", &new)?;
    // Tell running programs about the change (WM_SETTINGCHANGE) without a reboot.
    let _ = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -Namespace W -Name E -MemberDefinition '[DllImport(\"user32.dll\")] \
             public static extern IntPtr SendMessageTimeout(IntPtr h, uint m, IntPtr w, IntPtr l, \
             uint f, uint t, out IntPtr r);';\
             $r=[IntPtr]::Zero;\
             [W.E]::SendMessageTimeout([IntPtr]0xffff,0x1A,[IntPtr]::Zero,[IntPtr]::Zero,2,2000,[ref]$r) | Out-Null",
        ])
        .status();
    Ok(true)
}

/// Launches `program` with UAC elevation and waits for it.
///
/// The command line is staged in a temporary `.cmd` so that paths containing
/// spaces survive both the batch parser and the UAC hand-off.
pub fn run_elevated(program: &Path, args: &[String]) -> Result<()> {
    let mut script = String::from("@echo off\r\n");
    let mut line = format!("\"{}\"", program.display());
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    script.push_str(&line);
    script.push_str("\r\nexit /b %errorlevel%\r\n");

    let tmp = crate::paths::data_dir().join("elevated.cmd");
    std::fs::write(&tmp, &script)?;

    let ps = format!(
        "$r = Start-Process -FilePath '{}' -Verb RunAs -Wait -PassThru; exit $r.ExitCode",
        tmp.display().to_string().replace('\'', "''")
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .status()
        .context("launching elevated process")?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        bail!("elevated process failed with status {status}");
    }
    Ok(())
}

fn quote_arg(a: &str) -> String {
    let escaped = a.replace('%', "%%");
    if escaped.contains(' ') || escaped.contains('"') {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// Runs a program, capturing nothing, returning the exit code.
pub fn run(program: &Path, args: &[String]) -> Result<i32> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {}", program.display()))?;
    Ok(status.code().unwrap_or(-1))
}

/// Current machine architecture: `x64`, `x86`, `arm64`.
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        other => other,
    }
}
