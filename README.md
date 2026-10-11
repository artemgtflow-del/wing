# wing

A very fast package manager for Windows that reads the WinGet package index
directly — no `winget.exe` process, no COM activation, no bundled CPython.

```
wing search powershell          # offline, ~2 ms
wing show Microsoft.PowerShell  # manifest straight from winget-pkgs
wing install ripgrep            # download + SHA-256 verify + silent install
```

## Why it is fast

`winget search` takes ~850 ms warm on a typical machine. Almost none of that is
useful work: it starts an embedded CPython interpreter and activates the WinGet
COM server before it looks at anything.

`wing` does this instead:

| layer | how it works |
|---|---|
| search | read-only, memory-mapped SQLite (`immutable=1`), no network, no subprocess |
| metadata | one HTTPS GET of the YAML manifest from `microsoft/winget-pkgs`, cached on disk |
| install | own pipeline: parallel range download → SHA-256 → silent install |

Cold start of the binary is ~2 ms.

## Install

```powershell
cargo build --release
.\target\release\wing.exe sync     # downloads the ~20 MB index once
```

Put `wing.exe` anywhere on your PATH.

## Commands

```
wing sync [--force]          download or refresh the package index
wing search <query>          offline search
    --id --exact             match identifiers only / exactly
    --tag <t> --command <c>  search by tag or by exposed command
    --pfn <p>                search by MSIX package family name
    --product-code <c>       search by MSI product code
wing show <id> [--version V] full metadata, all versions, installer URLs + hashes
wing install <id>            download, verify and install
    --version --scope --arch --interactive --force --no-deps --dry-run
wing uninstall <id> [-y] [--purge]
wing link <id>                 expose an installed package's commands on PATH
wing upgrade [<id>] [--all] [--dry-run]
wing list [--all] [--upgradable]
wing info                    where things live, how many packages are indexed
wing bench                   wing vs winget on this machine
wing completions <shell>     bash | zsh | fish | powershell | elvish
```

Global flags: `--json`, `--limit N`, `--verbose`, `--locale <tag>`.

Set `WING_DEBUG_URLS=1` to print every manifest URL that is fetched.

## Configuration

`%LOCALAPPDATA%\wing\config.toml`. Every key is optional:

```toml
locale = "ru-RU"              # en-US (default) or ru-RU; affects messages only
color = "auto"                # auto | always | never
prefetch = false              # warm the manifest cache for top search hits
trust_exe_installers = false  # run .exe installers without asking
trusted_publishers = []       # empty means no restriction
index_verify = true
```

Command line flags win over the file. `--locale` also selects the translation
used for messages; unknown keys fall back to English rather than printing a key.

Two environment variables exist for development and testing:

| variable | effect |
|---|---|
| `WING_DATA_DIR` | use a different state directory; tests rely on this to avoid touching a real profile |
| `WING_DEBUG_URLS` | print every manifest URL that is fetched |

## Where the data comes from

* **Index** — `https://cdn.winget.microsoft.com/cache/source.msix`, the official
  WinGet CDN artifact. It is a ZIP containing `Public/index.db`, a SQLite
  database with ~15 500 packages, ~66 700 versions and ~181 000 manifest rows.
  Stored at `%LOCALAPPDATA%\wing\index.db`.
* **Manifests** — `https://raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/<letter>/<Publisher>/<Name>/<Version>/<Name>.installer.yaml`.
  Cached at `%LOCALAPPDATA%\wing\manifests\`.
* **Installed packages** — the Add/Remove Programs registry keys, plus a small
  `state.db` for anything `wing` installed itself.

`wing` never writes to `%LOCALAPPDATA%\Microsoft\WinGet` and never invokes
`winget.exe`.

## Safety

* An artifact is executed only after its SHA-256 matches the manifest. A
  mismatch deletes the file and aborts.
* Machine-scope installs request UAC once; user-scope installs never do.
* Silent switches come only from the manifest, never invented.

## Installers handled

| kind | how |
|---|---|
| `msi` / `wix` | `msiexec /qn`, or `/passive` when the manifest only declares `silentWithProgress`; `/norestart`; `ALLUSERS` + `MSIINSTALLPERUSER` follow the scope |
| `exe` | manifest `InstallerSwitches` (Silent / SilentWithProgress / Interactive), Burn bundles get `/install /quiet /norestart` |
| `msix` / `appx` | `Add-AppxPackage` |
| `msu` | `wusa /quiet /norestart` |
| `zip`, `7z`, `tar.gz` | extracted to `%LOCALAPPDATA%\wing\apps\<id>`; commands are exposed through **hard links** in `%LOCALAPPDATA%\wing\bin`, which is added to the user PATH |

`InstallerType` and `InstallModes` are read from each entry of the `Installers`
list, since manifests frequently declare them per installer rather than at the
manifest root. Getting this right is what decides whether `/qn` or `/passive` is
used at all.

Some installers put their executable somewhere that is not on PATH and do not
record an `InstallLocation` — `nepnep.neofetch-win` is one, and its manifest
declares no `Commands:` at all, so the package installs but cannot be run. After
an installer-based install, `wing` looks for the executable in the usual
per-user and per-machine locations and hard-links it into `wing\bin`. The same
logic is available on demand:

```console
$ wing link nepnep.neofetch-win
  linked neofetch.exe -> C:\Users\...\AppData\Local\Programs\neofetch-win\neofetch.exe
```

Hard links are used rather than `.cmd` shims because `cmd.exe` parses batch
files in the OEM code page — a shim breaks as soon as the user profile path
contains non-ASCII characters.

`wing uninstall` follows the same rule in reverse. The order is deliberate:

1. **Portable archives** — the extracted tree and the links wing created are removed.
2. **MSI packages** — `msiexec /x <code>`, where the code comes from the Add/Remove
   Programs *registry key name*, not from `UninstallString`. Some manifests write
   `MsiExec.exe /I{...}` there, which is a repair, not a removal.
3. **Everything else** — the uninstall command recorded in Add/Remove Programs.
4. **The ProductCode recorded at install time**, if the registry entry is gone.

Two rules exist specifically so that "uninstall" never lies about success:

* msiexec exit code `1605` ("not installed") is an error, not a success — the
  package would still be on disk.
* after any uninstall, the Add/Remove Programs entry is re-checked, and a package
  that is still listed is reported as an error.

If an ARP uninstall command points at `winget.exe` (which is what winget writes for
its own portable packages), the underlying uninstaller is run instead — wing never
delegates to winget.

`wing upgrade` deliberately refuses to guess. A program is only reported as
upgradable when its installed version is actually known (an empty
`DisplayVersion` means "unknown", not "outdated") and when its name matches an
index entry exactly after normalisation — accepting "the first search hit"
upgraded whatever happened to share a name with an unrelated program.

## Layout

```
src/
  main.rs        entry point
  cli.rs         clap definitions
  commands.rs    command implementations and output formatting
  index.rs       SQLite queries, search, manifest path reconstruction
  manifest.rs    manifest fetch, cache and YAML parsing
  resolve.rs     version / architecture / scope / dependency resolution
  install.rs     installer handlers
  http.rs        HTTP client with parallel range downloads
  verify.rs      SHA-256
  arp.rs         Add/Remove Programs enumeration
  state.rs       wing's own install database
  sysutil.rs     elevation, PATH management, process helpers
  paths.rs       data directory layout
  vercmp.rs      version comparison
```

## Building

The `windows-gnu` toolchain needs a C compiler for the bundled SQLite. If your
cargo home lives under a path containing non-ASCII characters, `mingw-w64`'s
linker silently fails to find the `windows-*` import libraries — point cargo at
an ASCII path in that case:

```powershell
New-Item -ItemType Junction -Path C:\cargohome -Target "$env:USERPROFILE\.cargo"
$env:CARGO_HOME = "C:\cargohome"
cargo build --release
```

## Licence

MIT.