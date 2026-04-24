use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use tool_core::{
    InstallLayout, InstalledToolRecord, display_name, install_binary_file, with_exe_suffix,
};

const REPOSITORY_URL: &str = "https://github.com/on9au/tools.git";
const DEFAULT_BRANCH: &str = "main";
const INTERNAL_PACKAGES: &[&str] = &["tool-core", "tool-installer", "toolctl"];
const DEFAULT_VERSION_LIMIT: usize = 10;

/// Controls and runs binaries managed by this repository.
#[derive(Debug, Parser)]
#[command(
    name = "toolctl",
    version,
    about = "Manage installed personal tool binaries"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Lists installed tools or installable tools from the repo catalog.
    List(ListArgs),
    /// Prints the install layout and basic health information.
    Doctor,
    /// Syncs the source tools repository into the local cache.
    Sync,
    /// Lists available versions for one tool based on repo commits.
    Versions(VersionsArgs),
    /// Builds and installs a tool from this repository.
    Install(InstallArgs),
    /// Runs a managed binary by name and forwards the remaining arguments.
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Whether to list installed tools or installable tools from the source repo.
    #[arg(value_enum, default_value_t = ListTarget::Installed)]
    target: ListTarget,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ListTarget {
    Installed,
    Available,
}

#[derive(Debug, Args)]
struct VersionsArgs {
    /// The tool name whose repo versions should be listed.
    tool_name: String,
    /// The number of versions to show.
    #[arg(long, default_value_t = DEFAULT_VERSION_LIMIT)]
    limit: usize,
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// The tool name to install. If omitted, toolctl prompts with the available list.
    tool_name: Option<String>,
    /// The commit-ish to build. Defaults to the latest commit affecting the tool.
    #[arg(long)]
    version: Option<String>,
    /// Prompt with recent versions if no explicit version is supplied.
    #[arg(long)]
    select_version: bool,
    /// Skip repo sync and use the existing local clone as-is.
    #[arg(long)]
    no_sync: bool,
    /// Always copy the built binary instead of trying a hard link first.
    #[arg(long)]
    copy: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The managed binary name to launch.
    binary_name: String,
    /// Extra arguments forwarded to the managed binary.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

#[derive(Debug, Clone)]
struct RepoTool {
    package_name: String,
    binary_name: String,
    manifest_path: PathBuf,
    package_dir: PathBuf,
    latest_version: ToolVersion,
}

#[derive(Debug, Clone)]
struct ToolVersion {
    commit: String,
    commit_date: String,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

struct ToolctlContext {
    layout: InstallLayout,
}

fn main() -> ExitCode {
    match try_main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let context = ToolctlContext::new()?;
    context.execute(cli.command)
}

impl ToolctlContext {
    fn new() -> Result<Self> {
        Ok(Self {
            layout: InstallLayout::discover()?,
        })
    }

    fn execute(&self, command: Commands) -> Result<ExitCode> {
        match command {
            Commands::List(list_args) => self.list_tools(list_args),
            Commands::Doctor => self.doctor(),
            Commands::Sync => self.sync_and_report(),
            Commands::Versions(versions_args) => self.list_versions(versions_args),
            Commands::Install(install_args) => self.install_tool(install_args),
            Commands::Run(run_args) => self.run_binary(run_args),
        }
    }

    /// Prints installed tools or installable repo tools.
    fn list_tools(&self, list_args: ListArgs) -> Result<ExitCode> {
        match list_args.target {
            ListTarget::Installed => self.list_installed_tools(),
            ListTarget::Available => {
                self.sync_repo()?;
                self.list_available_tools()
            }
        }
    }

    /// Prints all installed managed tools.
    fn list_installed_tools(&self) -> Result<ExitCode> {
        let records = self.layout.installed_records()?;
        if records.is_empty() {
            for binary in self.layout.managed_binaries()? {
                println!("{}", display_name(&binary)?);
            }
            return Ok(ExitCode::SUCCESS);
        }

        for record in records {
            println!(
                "{}\t{}\t{}\t{}",
                record.tool_name,
                short_commit(&record.commit),
                record.commit_date,
                record.package_name
            );
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Prints all installable tools discovered from the repo catalog.
    fn list_available_tools(&self) -> Result<ExitCode> {
        let catalog = repo_catalog(&self.layout)?;
        for tool in catalog {
            println!(
                "{}\t{}\t{}\t{}",
                tool.binary_name,
                short_commit(&tool.latest_version.commit),
                tool.latest_version.commit_date,
                tool.package_name
            );
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Prints install layout details and whether the expected directories exist.
    fn doctor(&self) -> Result<ExitCode> {
        let binaries = self.layout.managed_binaries()?;
        let records = self.layout.installed_records()?;

        println!("home: {}", self.layout.home_dir().display());
        println!("bin: {}", self.layout.bin_dir().display());
        println!("metadata: {}", self.layout.metadata_dir().display());
        println!("repo cache: {}", self.layout.repo_dir().display());
        println!("worktrees: {}", self.layout.worktrees_dir().display());
        println!("managed binaries: {}", binaries.len());
        println!("installed metadata records: {}", records.len());
        println!("bin directory exists: {}", self.layout.bin_dir().exists());
        println!("repo cache exists: {}", self.layout.repo_dir().exists());

        Ok(ExitCode::SUCCESS)
    }

    /// Syncs the source tools repository and prints its local path.
    fn sync_and_report(&self) -> Result<ExitCode> {
        self.sync_repo()?;
        println!("synced {}", self.layout.repo_dir().display());
        Ok(ExitCode::SUCCESS)
    }

    /// Lists recent commit-based versions for one tool.
    fn list_versions(&self, versions_args: VersionsArgs) -> Result<ExitCode> {
        self.sync_repo()?;

        let tool = find_tool(&self.layout, &versions_args.tool_name)?;
        for version in tool_versions(
            self.layout.repo_dir(),
            &tool.package_dir,
            versions_args.limit,
        )? {
            println!(
                "{}\t{}\t{}",
                version.commit, version.commit_date, version.summary
            );
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Builds and installs one tool from the source repo.
    fn install_tool(&self, install_args: InstallArgs) -> Result<ExitCode> {
        self.prepare_repo_cache(install_args.no_sync)?;

        let catalog = repo_catalog(&self.layout)?;
        let tool = select_install_tool(&catalog, install_args.tool_name.as_deref())?;
        let version = select_install_version(&self.layout, &tool, &install_args)?;

        let destination = build_and_install(&self.layout, &tool, &version, install_args.copy)?;
        let record = InstalledToolRecord {
            tool_name: tool.binary_name.clone(),
            package_name: tool.package_name.clone(),
            commit: version.commit.clone(),
            commit_date: version.commit_date.clone(),
            installed_at: iso_like_timestamp()?,
            repository_url: REPOSITORY_URL.to_string(),
        };
        self.layout.write_installed_record(&record)?;

        println!(
            "installed {} at {} ({})",
            record.tool_name,
            short_commit(&record.commit),
            destination.display()
        );

        Ok(ExitCode::SUCCESS)
    }

    /// Runs a managed binary and returns its exit code.
    fn run_binary(&self, run_args: RunArgs) -> Result<ExitCode> {
        let binary_path = self.layout.binary_path(&run_args.binary_name);
        if !binary_path.exists() {
            bail!(
                "managed binary '{}' was not found at {}",
                run_args.binary_name,
                binary_path.display()
            );
        }

        let status = Command::new(&binary_path)
            .args(run_args.args)
            .status()
            .with_context(|| format!("failed to launch {}", binary_path.display()))?;

        Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
    }

    fn prepare_repo_cache(&self, no_sync: bool) -> Result<()> {
        if no_sync {
            self.layout.ensure_exists()?;
            ensure_repo_cache_exists(&self.layout)
        } else {
            self.sync_repo()
        }
    }

    fn sync_repo(&self) -> Result<()> {
        sync_repo(&self.layout)
    }
}

fn select_install_tool(catalog: &[RepoTool], tool_name: Option<&str>) -> Result<RepoTool> {
    match tool_name {
        Some(tool_name) => select_named_tool(catalog, tool_name),
        None => select_tool_prompt(catalog),
    }
}

fn select_install_version(
    layout: &InstallLayout,
    tool: &RepoTool,
    install_args: &InstallArgs,
) -> Result<ToolVersion> {
    match install_args.version.as_deref() {
        Some(reference) => resolve_version_reference(layout.repo_dir(), reference),
        None if install_args.select_version => select_version_prompt(layout.repo_dir(), tool),
        None => Ok(tool.latest_version.clone()),
    }
}

fn ensure_repo_cache_exists(layout: &InstallLayout) -> Result<()> {
    if layout.repo_dir().join(".git").exists() {
        return Ok(());
    }

    bail!(
        "repo cache is missing at {}; run `toolctl sync` or omit --no-sync",
        layout.repo_dir().display()
    )
}

fn sync_repo(layout: &InstallLayout) -> Result<()> {
    layout.ensure_exists()?;

    if layout.repo_dir().join(".git").exists() {
        run_status(
            Command::new("git")
                .arg("-C")
                .arg(layout.repo_dir())
                .arg("fetch")
                .arg("--all")
                .arg("--tags")
                .arg("--prune"),
            "fetch tool repository",
        )?;
        run_status(
            Command::new("git")
                .arg("-C")
                .arg(layout.repo_dir())
                .arg("checkout")
                .arg(DEFAULT_BRANCH),
            "checkout default branch",
        )?;
        run_status(
            Command::new("git")
                .arg("-C")
                .arg(layout.repo_dir())
                .arg("pull")
                .arg("--ff-only")
                .arg("origin")
                .arg(DEFAULT_BRANCH),
            "pull latest default branch",
        )?;
    } else {
        run_status(
            Command::new("git")
                .arg("clone")
                .arg("--branch")
                .arg(DEFAULT_BRANCH)
                .arg("--single-branch")
                .arg(REPOSITORY_URL)
                .arg(layout.repo_dir()),
            "clone tool repository",
        )?;
    }

    Ok(())
}

fn repo_catalog(layout: &InstallLayout) -> Result<Vec<RepoTool>> {
    let metadata_text = run_captured(
        Command::new("cargo")
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .arg("--no-deps")
            .arg("--manifest-path")
            .arg(layout.repo_dir().join("Cargo.toml")),
        "read cargo metadata",
    )?;

    let metadata = serde_json::from_str::<CargoMetadata>(&metadata_text)
        .context("failed to parse cargo metadata output")?;

    let mut tools = Vec::new();
    for package in metadata.packages {
        if INTERNAL_PACKAGES.contains(&package.name.as_str()) {
            continue;
        }

        let package_dir = package
            .manifest_path
            .parent()
            .ok_or_else(|| {
                anyhow!(
                    "{} does not have a parent directory",
                    package.manifest_path.display()
                )
            })?
            .to_path_buf();

        for target in package.targets {
            if !target.kind.iter().any(|kind| kind == "bin") {
                continue;
            }

            if INTERNAL_PACKAGES.contains(&target.name.as_str()) {
                continue;
            }

            let latest_version = latest_version_for_path(layout.repo_dir(), &package_dir)?;
            tools.push(RepoTool {
                package_name: package.name.clone(),
                binary_name: target.name,
                manifest_path: package.manifest_path.clone(),
                package_dir: package_dir.clone(),
                latest_version,
            });
        }
    }

    tools.sort_by(|left, right| left.binary_name.cmp(&right.binary_name));
    Ok(tools)
}

fn find_tool(layout: &InstallLayout, tool_name: &str) -> Result<RepoTool> {
    let catalog = repo_catalog(layout)?;
    select_named_tool(&catalog, tool_name)
}

fn select_named_tool(catalog: &[RepoTool], tool_name: &str) -> Result<RepoTool> {
    catalog
        .iter()
        .find(|tool| tool.binary_name == tool_name)
        .cloned()
        .ok_or_else(|| anyhow!("unknown tool '{tool_name}'"))
}

fn select_tool_prompt(catalog: &[RepoTool]) -> Result<RepoTool> {
    if catalog.is_empty() {
        bail!("no installable tools were found in the repository catalog")
    }

    let options = catalog
        .iter()
        .map(|tool| {
            format!(
                "{} [{} {}]",
                tool.binary_name,
                short_commit(&tool.latest_version.commit),
                tool.latest_version.commit_date
            )
        })
        .collect::<Vec<_>>();

    let index = prompt_for_index("Select a tool to install", &options)?;
    Ok(catalog[index].clone())
}

fn select_version_prompt(repo_dir: &Path, tool: &RepoTool) -> Result<ToolVersion> {
    let versions = tool_versions(repo_dir, &tool.package_dir, 10)?;
    if versions.is_empty() {
        bail!("no versions were found for {}", tool.binary_name)
    }

    let options = versions
        .iter()
        .map(|version| {
            format!(
                "{} {} {}",
                short_commit(&version.commit),
                version.commit_date,
                version.summary
            )
        })
        .collect::<Vec<_>>();

    let index = prompt_for_index("Select a version", &options)?;
    Ok(versions[index].clone())
}

fn prompt_for_index(prompt: &str, options: &[String]) -> Result<usize> {
    println!("{prompt}:");
    for (index, option) in options.iter().enumerate() {
        println!("  {}. {}", index + 1, option);
    }

    print!("> ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .context("failed to read selection")?;

    let selection = buffer
        .trim()
        .parse::<usize>()
        .context("selection must be a number")?;

    if selection == 0 || selection > options.len() {
        bail!("selection must be between 1 and {}", options.len())
    }

    Ok(selection - 1)
}

fn tool_versions(repo_dir: &Path, package_dir: &Path, limit: usize) -> Result<Vec<ToolVersion>> {
    let log_output = run_captured(
        Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .arg("log")
            .arg(format!("-n{limit}"))
            .arg("--format=%H%x09%cs%x09%s")
            .arg("--")
            .arg(package_dir),
        "read tool versions from git history",
    )?;

    let versions = log_output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_version_line)
        .collect::<Result<Vec<_>>>()?;

    Ok(versions)
}

fn latest_version_for_path(repo_dir: &Path, package_dir: &Path) -> Result<ToolVersion> {
    tool_versions(repo_dir, package_dir, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no git history was found for {}", package_dir.display()))
}

fn resolve_version_reference(repo_dir: &Path, reference: &str) -> Result<ToolVersion> {
    let output = run_captured(
        Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .arg("show")
            .arg("--no-patch")
            .arg("--format=%H%x09%cs%x09%s")
            .arg(reference),
        "resolve git reference",
    )?;

    let line = output
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| anyhow!("git returned no commit metadata for {reference}"))?;

    parse_version_line(line)
}

fn parse_version_line(line: &str) -> Result<ToolVersion> {
    let mut parts = line.splitn(3, '\t');
    let commit = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing commit in git log output"))?;
    let commit_date = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing commit date in git log output"))?;
    let summary = parts.next().unwrap_or_default();

    Ok(ToolVersion {
        commit: commit.to_string(),
        commit_date: commit_date.to_string(),
        summary: summary.to_string(),
    })
}

fn build_and_install(
    layout: &InstallLayout,
    tool: &RepoTool,
    version: &ToolVersion,
    always_copy: bool,
) -> Result<PathBuf> {
    let worktree_path = create_worktree(layout, &tool.binary_name, &version.commit)?;
    let install_result = (|| -> Result<PathBuf> {
        let manifest_path = worktree_manifest_path(layout, &worktree_path, &tool.manifest_path)?;
        run_status(
            Command::new("cargo")
                .current_dir(&worktree_path)
                .arg("build")
                .arg("--release")
                .arg("--manifest-path")
                .arg(&manifest_path)
                .arg("--bin")
                .arg(&tool.binary_name),
            &format!(
                "build {} at {}",
                tool.binary_name,
                short_commit(&version.commit)
            ),
        )?;

        let built_binary = built_binary_path(&worktree_path, &tool.binary_name);
        ensure_built_binary_exists(&built_binary)?;

        install_binary_file(layout, &built_binary, &tool.binary_name, always_copy)
    })();

    let cleanup_result = remove_worktree(layout.repo_dir(), &worktree_path);
    match (install_result, cleanup_result) {
        (Ok(destination), Ok(())) => Ok(destination),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(install_error), Err(cleanup_error)) => Err(install_error)
            .with_context(|| format!("temporary worktree cleanup also failed: {cleanup_error:#}")),
    }
}

fn worktree_manifest_path(
    layout: &InstallLayout,
    worktree_path: &Path,
    manifest_path: &Path,
) -> Result<PathBuf> {
    let manifest_relative = manifest_path
        .strip_prefix(layout.repo_dir())
        .with_context(|| {
            format!(
                "{} is not inside {}",
                manifest_path.display(),
                layout.repo_dir().display()
            )
        })?;
    Ok(worktree_path.join(manifest_relative))
}

fn built_binary_path(worktree_path: &Path, binary_name: &str) -> PathBuf {
    worktree_path
        .join("target")
        .join("release")
        .join(with_exe_suffix(binary_name))
}

fn ensure_built_binary_exists(binary_path: &Path) -> Result<()> {
    if !binary_path.exists() {
        bail!("built binary was not found at {}", binary_path.display())
    }

    Ok(())
}

fn create_worktree(layout: &InstallLayout, tool_name: &str, reference: &str) -> Result<PathBuf> {
    layout.ensure_exists()?;

    let worktree_path = layout.worktrees_dir().join(format!(
        "{}-{}-{}-{}",
        sanitize_name(tool_name),
        short_commit(reference),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before unix epoch")?
            .as_secs()
    ));

    run_status(
        Command::new("git")
            .arg("-C")
            .arg(layout.repo_dir())
            .arg("worktree")
            .arg("add")
            .arg("--detach")
            .arg(&worktree_path)
            .arg(reference),
        "create temporary git worktree",
    )?;

    Ok(worktree_path)
}

fn remove_worktree(repo_dir: &Path, worktree_path: &Path) -> Result<()> {
    run_status(
        Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(worktree_path),
        "remove temporary git worktree",
    )
}

fn run_status(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to {description}"))?;

    if !status.success() {
        bail!("failed to {description}")
    }

    Ok(())
}

fn run_captured(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("failed to {description}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to {description}: {}", stderr.trim())
    }

    String::from_utf8(output.stdout).context("command output was not valid UTF-8")
}

fn short_commit(commit: &str) -> &str {
    let end = commit.len().min(8);
    &commit[..end]
}

fn sanitize_name(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn iso_like_timestamp() -> Result<String> {
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs();
    Ok(unix_seconds.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_VERSION_LIMIT, built_binary_path, ensure_built_binary_exists, parse_version_line,
        sanitize_name, short_commit,
    };
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_version_line_extracts_commit_date_and_summary() {
        let version = parse_version_line("abcdef123456\t2026-04-24\tAdd tool installer").unwrap();

        assert_eq!(version.commit, "abcdef123456");
        assert_eq!(version.commit_date, "2026-04-24");
        assert_eq!(version.summary, "Add tool installer");
    }

    #[test]
    fn short_commit_uses_eight_characters_when_available() {
        assert_eq!(short_commit("1234567890abcdef"), "12345678");
        assert_eq!(short_commit("1234"), "1234");
    }

    #[test]
    fn sanitize_name_replaces_non_ascii_alphanumeric_characters() {
        assert_eq!(sanitize_name("tool name/alpha"), "tool-name-alpha");
    }

    #[test]
    fn built_binary_path_targets_release_directory() {
        let path = built_binary_path(Path::new("C:/tmp/worktree"), "toolctl");
        assert!(path.ends_with(if cfg!(windows) {
            Path::new("target/release/toolctl.exe")
        } else {
            Path::new("target/release/toolctl")
        }));
    }

    #[test]
    fn ensure_built_binary_exists_rejects_missing_files() {
        let missing = unique_temp_path("missing-binary");
        let error = ensure_built_binary_exists(&missing).unwrap_err();
        assert!(error.to_string().contains("built binary was not found"));
    }

    #[test]
    fn default_version_limit_is_positive() {
        assert!(DEFAULT_VERSION_LIMIT > 0);
    }

    fn unique_temp_path(prefix: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("on9au-tools-toolctl-{prefix}-{stamp}"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        path
    }
}
