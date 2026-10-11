use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "wing",
    version,
    about = "A very fast package manager built directly on the WinGet package index",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long, global = true)]
    pub json: bool,

    /// Maximum number of rows for list-like commands.
    #[arg(long, global = true, default_value_t = 20)]
    pub limit: usize,

    /// Show download progress and extra diagnostics.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,

    /// Locale used when fetching manifest translations.
    #[arg(long, global = true, default_value = "en-US")]
    pub locale: String,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Search the local index (offline, no network).
    Search(SearchArgs),
    /// Show full metadata for a package.
    Show(ShowArgs),
    /// Download, verify and install a package.
    Install(InstallArgs),
    /// Remove a package.
    Uninstall(UninstallArgs),
    /// Expose the commands of an installed package on PATH.
    Link(LinkArgs),
    /// Report or perform available upgrades.
    Upgrade(UpgradeArgs),
    /// List installed packages.
    List(ListArgs),
    /// Download or refresh the package index.
    Sync(SyncArgs),
    /// Show information about this wing installation.
    Info,
    /// Compare wing against winget.
    Bench,
    /// Print a shell completion script.
    Completions(CompletionsArgs),
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Free-text query.
    pub query: Vec<String>,

    /// Match only package identifiers.
    #[arg(long)]
    pub id: bool,
    /// Search within a tag.
    #[arg(long)]
    pub tag: Option<String>,
    /// Search by command name exposed by the package.
    #[arg(long)]
    pub command: Option<String>,
    /// Search by MSIX package family name.
    #[arg(long)]
    pub pfn: Option<String>,
    /// Search by MSI product code.
    #[arg(long)]
    pub product_code: Option<String>,
    /// Exact match only.
    #[arg(long)]
    pub exact: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    pub id: String,
    #[arg(long)]
    pub version: Option<String>,
    /// Fetch the manifest even if it is cached.
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    pub id: String,
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub scope: Option<String>,
    #[arg(long)]
    pub arch: Option<String>,
    /// Run the installer interactively even if silent mode is available.
    #[arg(long)]
    pub interactive: bool,
    /// Re-download even when a verified copy is cached.
    #[arg(long)]
    pub force: bool,
    /// Do not install dependencies.
    #[arg(long)]
    pub no_deps: bool,
    /// Print the plan without doing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct UninstallArgs {
    pub id: String,
    /// Also delete files the installer left behind.
    #[arg(long)]
    pub purge: bool,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct LinkArgs {
    pub id: String,
    #[arg(long)]
    pub version: Option<String>,
}

#[derive(Args, Debug)]
pub struct UpgradeArgs {
    /// Package to upgrade (omit for a report of everything upgradable).
    pub id: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Only packages that have a newer version available.
    #[arg(long)]
    pub upgradable: bool,
    /// Include everything from Add/Remove Programs, not just what wing installed.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Delete the cached index first.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct CompletionsArgs {
    pub shell: String,
}