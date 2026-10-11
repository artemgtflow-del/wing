# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added
- `WING_DATA_DIR` to relocate all state, which lets tests run without touching a
  real user profile.
- `config.toml` in the data directory: `locale`, `color`, `prefetch`,
  `trust_exe_installers`, `trusted_publishers`, `index_verify`.
- Message catalogue with `en-US` and `ru-RU`, selected with `--locale`.
- `wing link <id>` exposes an installed package's commands on `PATH`.
- Automatic command linking after MSI/EXE installs, for packages whose manifest
  declares no `Commands:` and whose installer adds nothing to `PATH`.

### Fixed
- `upgrade` no longer reports programs whose installed version is unknown as
  upgradable; an empty `DisplayVersion` meant "unknown", not "outdated".
- Matching an installed program to an index package is now an exact match after
  normalisation. The previous fallback (accepting the first search hit) could
  match an unrelated program with a similar name.
- `list --upgradable` shares the matching logic with `upgrade` instead of
  re-implementing it per row.
- Dependency `MinimumVersion` is honoured; previously the newest version was
  always chosen. If no acceptable version can be installed, the resolver falls
  back to the newest available and says so.
- `/L*v <file>` is passed to `msiexec` as two arguments. As a single argument the
  OS quoted it and `msiexec` rejected the whole command line (exit 1639).
- `InstallerType` and `InstallModes` are read from each `Installers` entry when
  absent at the manifest root; without this a silent install could not be
  selected and the installer ran with a full UI.
- `UninstallString` recorded by some MSI packages is `MsiExec.exe /I{...}`, which
  is a repair rather than a removal. MSI packages are now removed with
  `msiexec /x` using the ProductCode from the Add/Remove Programs key name.
- `msiexec` exit code `1605` is an error, not a success.
- `uninstall` re-checks Add/Remove Programs afterwards and fails if the package
  is still registered.
- Hard links created in `wing\bin` survive removal of the original file, so they
  are deleted together with the package.
- Archive installs use hard links rather than `.cmd` shims, which `cmd.exe`
  parses in the OEM code page and which broke on non-ASCII profile paths.

### Notes
- The manifest for `nepnep.neofetch-win` in winget-pkgs declares a ProductCode
  that does not match the shipped MSI. `wing` works around this through the
  Add/Remove Programs key rather than trusting the manifest alone.
