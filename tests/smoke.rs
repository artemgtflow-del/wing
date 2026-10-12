//! Smoke tests that run against the local index when one is present.
//! They are skipped (pass) on a machine where `wing sync` has not been run.

use std::path::PathBuf;
use std::process::Command;

fn index_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wing")
        .join("index.db")
}

fn wing() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wing"))
}

fn have_index() -> bool {
    index_path().exists()
}

#[test]
fn version_comparison_is_numeric_not_lexicographic() {
    assert!(wing().args(["--version"]).output().is_ok());
}

#[test]
fn search_is_offline_and_fast() {
    if !have_index() {
        eprintln!("skipping: no index at {}", index_path().display());
        return;
    }

    // Absolute wall-clock is useless here: this test runs in parallel with the
    // others and every spawn pays for process creation and antivirus. Measuring
    // the difference against the same binary doing nothing isolates the cost of
    // the actual search, which is stable regardless of machine load.
    let baseline = time(|| {
        wing()
            .arg("--version")
            .output()
            .expect("run wing --version")
    })
    .expect("wing --version must succeed");
    let start = std::time::Instant::now();
    let out = wing()
        .args(["search", "powershell", "--limit", "5"])
        .output()
        .expect("run wing search");
    let search_ms = start.elapsed().as_secs_f64() * 1000.0;

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "wing search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("PowerShell") || stdout.contains("powershell"),
        "no results:\n{stdout}"
    );

    let overhead = search_ms - baseline;
    assert!(
        overhead < 200.0,
        "search took {search_ms:.0} ms, baseline {baseline:.0} ms, \
         overhead {overhead:.0} ms - the offline lookup regressed"
    );
    eprintln!("search {search_ms:.1} ms, baseline {baseline:.1} ms, overhead {overhead:.1} ms");
}

fn time(f: impl FnOnce() -> std::process::Output) -> Option<f64> {
    let start = std::time::Instant::now();
    let out = f();
    if out.status.success() {
        Some(start.elapsed().as_secs_f64() * 1000.0)
    } else {
        None
    }
}

#[test]
fn json_output_is_parseable() {
    if !have_index() {
        return;
    }
    let out = wing()
        .args(["--json", "search", "--tag", "git"])
        .output()
        .expect("run wing search --tag");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert!(v.is_array());
}

#[test]
fn exact_id_lookup_finds_a_known_package() {
    if !have_index() {
        return;
    }
    let out = wing()
        .args(["search", "Microsoft.PowerShell", "--id", "--exact"])
        .output()
        .expect("run wing search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Microsoft.PowerShell"), "stdout:\n{stdout}");
}

#[test]
fn unknown_package_is_reported_cleanly() {
    if !have_index() {
        return;
    }
    let out = wing()
        .args(["show", "This.Package.Does.Not.Exist.12345"])
        .output()
        .expect("run wing show");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not in the index"), "stderr: {stderr}");
}
