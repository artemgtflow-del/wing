# Contributing a change

Thank you for taking the time.

## Before you start

Open an issue first for anything that changes behaviour. The two rules below are
load bearing, so proposals that break them will not be merged:

- `wing` must never run `winget.exe`.
- An artifact must never be executed before its SHA-256 matches the manifest.

## Making the change

1. Branch off `main`.
2. Keep the change focused. Unrelated cleanups belong in a separate commit.
3. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and
   `cargo test --release`.
4. Add or update tests. Code that parses untrusted manifest data is expected to
   have unit tests covering the awkward shapes.
5. Add a `CHANGELOG.md` entry under *Unreleased*.

## Messages

User-facing strings go through the catalogue in `src/i18n.rs` as
`(key, en-US, ru-RU)` and are emitted with `ctx.t("key", &[args])`. Add the
Russian side in the same commit; the fallback is English, so a missing
translation degrades silently. `--json` output is a contract and is not
translated.

## Testing against real packages

```powershell
$env:WING_DATA_DIR = "C:\temp\wing-test"
wing sync
wing search <query>
wing install <small-package> --dry-run
```

If you do run a real install, uninstall it afterwards and confirm nothing is
left behind: the executable, the Add/Remove Programs entry, and any hard link in
`%LOCALAPPDATA%\wing\bin`.
