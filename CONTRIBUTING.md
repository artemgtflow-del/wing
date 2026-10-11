# Contributing

## Build

```powershell
cargo build --release
cargo test --release
```

The bundled SQLite needs a C compiler. On a machine where the cargo home lives
under a path containing non-ASCII characters, `mingw-w64`'s linker silently fails
to find the `windows-*` import libraries. Point cargo at an ASCII path:

```powershell
New-Item -ItemType Junction -Path C:\cargohome -Target "$env:USERPROFILE\.cargo"
$env:CARGO_HOME = "C:\cargohome"
cargo build --release
```

## Testing against a real package

Never point the tests at your own profile:

```powershell
$env:WING_DATA_DIR = "C:\temp\wing-test"
wing sync
wing search powershell
```

`WING_DATA_DIR` relocates every file wing writes: index, state, manifest cache,
downloads and command links.

## Ground rules

- **wing never runs `winget.exe`.** It reads the WinGet index and the winget-pkgs
  manifests directly. Do not add a shelling-out fallback; if a data source cannot
  be read, fail with an explanation.
- **SHA-256 is checked before anything is executed.** Do not add a code path that
  skips it.
- **Uninstall must not report success unless the package is gone.** Both the
  `1605` msiexec code and the post-uninstall Add/Remove Programs re-check exist
  for this reason.
- Manifest data is untrusted input. Parse it defensively: manifests in
  winget-pkgs contain wrong ProductCodes, `InstallerType` in the wrong place, and
  `UninstallString` values that do not do what they say.

## Adding a message

User-facing strings live in the catalogue in `src/i18n.rs` and are emitted with
`ctx.t("key", &[args])`. Add the `ru-RU` side at the same time; the fallback is
English, so a missing translation silently degrades.

`--json` output is a contract and is not translated.

## Layout

```
src/
  main.rs        entry point, panic hook
  cli.rs         clap definitions
  commands.rs    command implementations and output formatting
  config.rs      config.toml
  i18n.rs        message catalogue
  index.rs       SQLite queries, search, manifest path reconstruction
  manifest.rs    manifest fetch, cache and YAML parsing
  resolve.rs     version / architecture / scope / dependency resolution
  install.rs     installer handlers, command links
  http.rs        HTTP client with parallel range downloads
  verify.rs      SHA-256
  arp.rs         Add/Remove Programs enumeration
  state.rs       wing's own install database
  sysutil.rs     elevation, PATH management, process helpers
  paths.rs       data directory layout
  vercmp.rs      version comparison
tests/
  smoke.rs       end-to-end tests that need a synced index
```

Unit tests live next to the code they cover; `tests/smoke.rs` only exercises the
binary and skips itself when no index is present.
