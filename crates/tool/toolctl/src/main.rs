use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use dialoguer::{MultiSelect, Select, theme::ColorfulTheme};
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
    /// One or more tool names to install. Use `all` to install every available tool.
    tool_names: Vec<String>,
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
            print_section_title("Installed Tools");
            for binary in self.layout.managed_binaries()? {
                println!("{}", display_name(&binary)?);
            }
            return Ok(ExitCode::SUCCESS);
        }

        print_section_title("Installed Tools");
        let mut table = new_table(["Tool", "Commit", "Date", "Package"]);
        for record in records {
            table.add_row(vec![
                record.tool_name,
                short_commit(&record.commit).to_string(),
                record.commit_date,
                record.package_name,
            ]);
        }
        println!("{table}");

        Ok(ExitCode::SUCCESS)
    }

    /// Prints all installable tools discovered from the repo catalog.
    fn list_available_tools(&self) -> Result<ExitCode> {
        let catalog = repo_catalog(&self.layout)?;
        print_section_title("Available Tools");
        let mut table = new_table(["Tool", "Latest", "Date", "Package"]);
        for tool in catalog {
            table.add_row(vec![
                tool.binary_name,
                short_commit(&tool.latest_version.commit).to_string(),
                tool.latest_version.commit_date,
                tool.package_name,
            ]);
        }
        println!("{table}");

        Ok(ExitCode::SUCCESS)
    }

    /// Prints install layout details and whether the expected directories exist.
    fn doctor(&self) -> Result<ExitCode> {
        let binaries = self.layout.managed_binaries()?;
        let records = self.layout.installed_records()?;

        print_section_title("Toolctl Doctor");
        let mut table = new_table(["Check", "Value"]);
        table.add_row(vec![
            "home".to_string(),
            self.layout.home_dir().display().to_string(),
        ]);
        table.add_row(vec![
            "bin".to_string(),
            self.layout.bin_dir().display().to_string(),
        ]);
        table.add_row(vec![
            "metadata".to_string(),
            self.layout.metadata_dir().display().to_string(),
        ]);
        table.add_row(vec![
            "repo cache".to_string(),
            self.layout.repo_dir().display().to_string(),
        ]);
        table.add_row(vec![
            "worktrees".to_string(),
            self.layout.worktrees_dir().display().to_string(),
        ]);
        table.add_row(vec![
            "managed binaries".to_string(),
            binaries.len().to_string(),
        ]);
        table.add_row(vec![
            "installed metadata records".to_string(),
            records.len().to_string(),
        ]);
        table.add_row(vec![
            "bin directory exists".to_string(),
            self.layout.bin_dir().exists().to_string(),
        ]);
        table.add_row(vec![
            "repo cache exists".to_string(),
            self.layout.repo_dir().exists().to_string(),
        ]);
        println!("{table}");

        Ok(ExitCode::SUCCESS)
    }

    /// Syncs the source tools repository and prints its local path.
    fn sync_and_report(&self) -> Result<ExitCode> {
        self.sync_repo()?;
        print_section_title("Repository Sync");
        println!("Synced {}", self.layout.repo_dir().display());
        Ok(ExitCode::SUCCESS)
    }

    /// Lists recent commit-based versions for one tool.
    fn list_versions(&self, versions_args: VersionsArgs) -> Result<ExitCode> {
        self.sync_repo()?;

        let tool = find_tool(&self.layout, &versions_args.tool_name)?;
        print_section_title(&format!("Versions for {}", tool.binary_name));
        let mut table = new_table(["Commit", "Date", "Summary"]);
        for version in tool_versions(
            self.layout.repo_dir(),
            &tool.package_dir,
            versions_args.limit,
        )? {
            table.add_row(vec![version.commit, version.commit_date, version.summary]);
        }
        println!("{table}");

        Ok(ExitCode::SUCCESS)
    }

    /// Builds and installs one or more tools from the source repo.
    fn install_tool(&self, install_args: InstallArgs) -> Result<ExitCode> {
        self.prepare_repo_cache(install_args.no_sync)?;

        let catalog = repo_catalog(&self.layout)?;
        let tools = select_install_tools(&catalog, &install_args.tool_names)?;
        if tools.len() > 1 && install_args.select_version {
            bail!("--select-version can only be used when installing exactly one tool")
        }

        print_section_title("Install Plan");
        let mut plan_table = new_table(["Tool", "Target Version", "Package"]);
        let mut install_rows = Vec::new();

        for tool in tools {
            let version = select_install_version(&self.layout, &tool, &install_args)?;
            plan_table.add_row(vec![
                tool.binary_name.clone(),
                format!("{} {}", short_commit(&version.commit), version.commit_date),
                tool.package_name.clone(),
            ]);
            install_rows.push((tool, version));
        }

        println!("{plan_table}");

        let mut summary_table = new_table(["Tool", "Installed", "Package", "Path"]);
        for (tool, version) in install_rows {
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
            summary_table.add_row(vec![
                record.tool_name,
                short_commit(&record.commit).to_string(),
                record.package_name,
                destination.display().to_string(),
            ]);
        }

        print_section_title("Installed Tools");
        println!("{summary_table}");

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

fn select_install_tools(catalog: &[RepoTool], tool_names: &[String]) -> Result<Vec<RepoTool>> {
    if catalog.is_empty() {
        bail!("no installable tools were found in the repository catalog")
    }

    if tool_names.is_empty() {
        return select_tools_prompt(catalog);
    }

    if tool_names.iter().any(|tool_name| is_all_target(tool_name)) {
        return Ok(catalog.to_vec());
    }

    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for tool_name in tool_names {
        if !seen.insert(tool_name.to_ascii_lowercase()) {
            continue;
        }
        selected.push(select_named_tool(catalog, tool_name)?);
    }

    Ok(selected)
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
    if is_all_target(tool_name) {
        bail!("`all` selects multiple tools and is only valid with `toolctl install`")
    }

    catalog
        .iter()
        .find(|tool| tool.binary_name.eq_ignore_ascii_case(tool_name))
        .cloned()
        .ok_or_else(|| anyhow!("unknown tool '{tool_name}'"))
}

fn select_tools_prompt(catalog: &[RepoTool]) -> Result<Vec<RepoTool>> {
    let mut options = vec![format!("all tools [{} available]", catalog.len())];
    options.extend(catalog.iter().map(format_tool_option));

    let selections = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select tool(s) to install")
        .items(&options)
        .interact()
        .context("failed to read tool selections")?;

    if selections.is_empty() {
        bail!("no tools were selected")
    }

    if selections.contains(&0) {
        return Ok(catalog.to_vec());
    }

    Ok(selections
        .into_iter()
        .map(|index| catalog[index - 1].clone())
        .collect())
}

fn select_version_prompt(repo_dir: &Path, tool: &RepoTool) -> Result<ToolVersion> {
    let versions = tool_versions(repo_dir, &tool.package_dir, 10)?;
    if versions.is_empty() {
        bail!("no versions were found for {}", tool.binary_name)
    }

    let options = versions
        .iter()
        .map(format_version_option)
        .collect::<Vec<_>>();

    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a version")
        .items(&options)
        .default(0)
        .interact()
        .context("failed to read version selection")?;
    Ok(versions[index].clone())
}

fn format_tool_option(tool: &RepoTool) -> String {
    format!(
        "{}  {}  {}  {}",
        tool.binary_name,
        short_commit(&tool.latest_version.commit),
        tool.latest_version.commit_date,
        tool.package_name
    )
}

fn format_version_option(version: &ToolVersion) -> String {
    format!(
        "{}  {}  {}",
        short_commit(&version.commit),
        version.commit_date,
        version.summary
    )
}

fn is_all_target(tool_name: &str) -> bool {
    tool_name.eq_ignore_ascii_case("all")
}

fn print_section_title(title: &str) {
    println!("\n== {title} ==");
}

fn new_table<const N: usize>(headers: [&str; N]) -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.into_iter().map(|header| {
            Cell::new(header)
                .fg(Color::Cyan)
                .add_attribute(Attribute::Bold)
        }));
    table
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
        DEFAULT_VERSION_LIMIT, RepoTool, ToolVersion, built_binary_path,
        ensure_built_binary_exists, format_tool_option, format_version_option, is_all_target,
        parse_version_line, sanitize_name, short_commit,
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

    #[test]
    fn all_target_is_case_insensitive() {
        assert!(is_all_target("all"));
        assert!(is_all_target("ALL"));
        assert!(!is_all_target("pdupload"));
    }

    #[test]
    fn format_tool_option_includes_tool_metadata() {
        let option = format_tool_option(&RepoTool {
            package_name: "crates/tool/example".to_string(),
            binary_name: "example".to_string(),
            manifest_path: Path::new("C:/tmp/Cargo.toml").to_path_buf(),
            package_dir: Path::new("C:/tmp").to_path_buf(),
            latest_version: ToolVersion {
                commit: "1234567890abcdef".to_string(),
                commit_date: "2026-04-29".to_string(),
                summary: "Add example".to_string(),
            },
        });

        assert!(option.contains("example"));
        assert!(option.contains("12345678"));
        assert!(option.contains("2026-04-29"));
        assert!(option.contains("crates/tool/example"));
    }

    #[test]
    fn format_version_option_includes_summary() {
        let option = format_version_option(&ToolVersion {
            commit: "1234567890abcdef".to_string(),
            commit_date: "2026-04-29".to_string(),
            summary: "Ship install flow".to_string(),
        });

        assert!(option.contains("12345678"));
        assert!(option.contains("2026-04-29"));
        assert!(option.contains("Ship install flow"));
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
