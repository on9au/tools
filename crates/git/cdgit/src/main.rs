//! cdgit — fuzzy-find a git repo under a configured root and print its path.
//!
//! A child process cannot change its parent shell's working directory, so this
//! tool only *resolves and prints* a repository path on stdout. The actual `cd`
//! is performed by a tiny shell wrapper (à la `zoxide`/`fzf`), which you install
//! once with `cdgit init <shell>`:
//!
//! ```powershell
//! Invoke-Expression (cdgit init powershell | Out-String)   # PowerShell
//! ```
//! ```bash
//! eval "$(cdgit init bash)"                                 # bash/zsh
//! ```
//!
//! Repositories are discovered under `$CDGIT_ROOT`, assuming the layout
//! `<root>/<owner>/<repo>` (e.g. `C:\git\GitHub\on9au\tools`).
//!
//! The interactive picker renders to stderr, so stdout only ever carries the
//! chosen path — safe to capture in `cd "$(cdgit ...)"`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::Table;
use dialoguer::FuzzySelect;

const ROOT_ENV_VAR: &str = "CDGIT_ROOT";

/// Fuzzy-find a git repo under `$CDGIT_ROOT` and print its path.
#[derive(Debug, Parser)]
#[command(name = "cdgit", version, about, long_about = None)]
struct Cli {
    /// Fuzzy query matched against `owner/repo`.
    ///
    /// If exactly one repo matches, its path is printed directly; otherwise an
    /// interactive picker (pre-filtered to the matches) is shown.
    query: Option<String>,

    /// List matching repos as a table instead of selecting one.
    #[arg(short, long)]
    list: bool,

    /// Git root to scan (defaults to `$CDGIT_ROOT`).
    #[arg(long, env = ROOT_ENV_VAR)]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the shell integration snippet for the given shell.
    ///
    /// Add it to your shell profile to get a `cg` command that actually changes
    /// directory, e.g. `eval "$(cdgit init bash)"`.
    Init {
        /// The shell to emit an integration snippet for.
        shell: Shell,
    },
}

/// Shells that `cdgit init` can emit an integration snippet for.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Shell {
    Powershell,
    Bash,
    Zsh,
    Fish,
}

/// A repository discovered under the configured root.
struct Repo {
    owner: String,
    name: String,
    path: PathBuf,
}

impl Repo {
    /// The `owner/repo` label matched against queries and shown in the picker.
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("cdgit: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    if let Some(Command::Init { shell }) = cli.command {
        print!("{}", init_snippet(shell));
        return Ok(ExitCode::SUCCESS);
    }

    let root = cli
        .root
        .with_context(|| format!("no git root configured; set {ROOT_ENV_VAR} or pass --root"))?;
    if !root.is_dir() {
        bail!("git root `{}` is not a directory", root.display());
    }

    let repos = scan(&root)?;
    if repos.is_empty() {
        bail!(
            "no `<owner>/<repo>` directories found under `{}`",
            root.display()
        );
    }

    let matches: Vec<&Repo> = match &cli.query {
        Some(query) => repos
            .iter()
            .filter(|repo| is_match(&repo.slug(), query))
            .collect(),
        None => repos.iter().collect(),
    };

    if matches.is_empty() {
        // Safe to unwrap: `matches` is only empty when a query was supplied.
        bail!("no repo matches `{}`", cli.query.unwrap());
    }

    if cli.list {
        print_table(&matches);
        return Ok(ExitCode::SUCCESS);
    }

    let chosen = if matches.len() == 1 {
        matches[0]
    } else {
        match select(&matches)? {
            Some(index) => matches[index],
            None => {
                eprintln!("cdgit: selection cancelled");
                return Ok(ExitCode::FAILURE);
            }
        }
    };

    // The path is the only thing on stdout, so the shell wrapper can capture it.
    println!("{}", chosen.path.display());
    Ok(ExitCode::SUCCESS)
}

/// Discover repositories laid out as `<root>/<owner>/<repo>`, sorted by slug.
fn scan(root: &Path) -> Result<Vec<Repo>> {
    let mut repos = Vec::new();
    for owner_dir in subdirectories(root)? {
        let owner = directory_name(&owner_dir);
        for repo_dir in subdirectories(&owner_dir)? {
            let name = directory_name(&repo_dir);
            repos.push(Repo {
                owner: owner.clone(),
                name,
                path: repo_dir,
            });
        }
    }

    repos.sort_by_key(|repo| repo.slug().to_lowercase());
    Ok(repos)
}

/// List the immediate subdirectories of `dir` (non-recursive).
fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("failed to read `{}`", dir.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read an entry in `{}`", dir.display()))?;
        if entry
            .file_type()
            .with_context(|| format!("failed to inspect `{}`", entry.path().display()))?
            .is_dir()
        {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// The final path component as a lossy string (the owner or repo name).
fn directory_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Case-insensitive subsequence match: every char of `query` appears in
/// `haystack`, in order (the same loose matching a fuzzy finder uses).
fn is_match(haystack: &str, query: &str) -> bool {
    let mut needle = query.chars().flat_map(char::to_lowercase).peekable();
    if needle.peek().is_none() {
        return true;
    }
    for candidate in haystack.chars().flat_map(char::to_lowercase) {
        if needle.peek().is_some_and(|wanted| *wanted == candidate) {
            needle.next();
        }
    }
    needle.peek().is_none()
}

/// Show an interactive fuzzy picker over the matches (rendered on stderr).
fn select(matches: &[&Repo]) -> Result<Option<usize>> {
    let items: Vec<String> = matches.iter().map(|repo| repo.slug()).collect();
    FuzzySelect::new()
        .with_prompt("Select a repo")
        .items(&items)
        .default(0)
        .interact_opt()
        .context("interactive selection failed")
}

/// Print an aligned table of the matching repos and their paths.
fn print_table(matches: &[&Repo]) {
    let mut table = Table::new();
    table.set_header(vec!["OWNER", "REPO", "PATH"]);
    for repo in matches {
        table.add_row(vec![
            repo.owner.clone(),
            repo.name.clone(),
            repo.path.display().to_string(),
        ]);
    }
    println!("{table}");
}

/// The shell integration snippet defining a `cg` command for the given shell.
fn init_snippet(shell: Shell) -> &'static str {
    match shell {
        Shell::Powershell => {
            "function cg {\n    \
                 $dir = cdgit @args\n    \
                 if ($LASTEXITCODE -eq 0 -and $dir) { Set-Location $dir }\n\
             }\n"
        }
        Shell::Bash | Shell::Zsh => {
            "cg() {\n    \
                 local dir\n    \
                 dir=\"$(cdgit \"$@\")\" && [ -n \"$dir\" ] && cd \"$dir\"\n\
             }\n"
        }
        Shell::Fish => {
            "function cg\n    \
                 set -l dir (cdgit $argv)\n    \
                 and test -n \"$dir\"\n    \
                 and cd $dir\n\
             end\n"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_match;

    #[test]
    fn subsequence_matches_in_order() {
        assert!(is_match("on9au/tools", "tools"));
        assert!(is_match("on9au/tools", "o9t"));
        assert!(is_match("on9au/tools", "au/to"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_match("on9au/Tools", "TOOLS"));
        assert!(is_match("On9au/Tools", "on9"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(is_match("anything", ""));
    }

    #[test]
    fn out_of_order_or_missing_chars_do_not_match() {
        assert!(!is_match("on9au/tools", "sloot"));
        assert!(!is_match("on9au/tools", "xyz"));
    }
}
