use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use serde::Deserialize;
use tempfile::TempDir;
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;
use walkdir::WalkDir;
use xmltree::{Element, EmitterConfig, XMLNode};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const DEFAULT_PACKAGE_DIRECTORIES: &[&str] = &["bin/nuget", "package"];
const DEFAULT_SUFFIX_PREFIX: &str = "pre";
const DEFAULT_DATE_FORMAT: &[FormatItem<'static>] = format_description!("[year][month][day]");
const DEFAULT_SUFFIX_TOKEN_WIDTH: usize = 8;
const DEFAULT_CONFIG_FILE_NAME: &str = "pdupload.toml";
const DEFAULT_CONFIG_DIRECTORY_NAME: &str = "pdupload";

/// Uploads one or more NuGet packages to a feed, optionally overriding their package version.
#[derive(Debug, Parser)]
#[command(
    name = "pdupload",
    version,
    about = "Upload multiple .nupkg files to a NuGet feed with an optional package version override"
)]
struct Cli {
    /// Path to a TOML config file that supplies default options.
    #[arg(long)]
    config: Option<PathBuf>,
    /// NuGet source name or URL to upload packages to.
    #[arg(long, short = 's')]
    source: Option<String>,
    /// API key used by dotnet nuget push. Defaults to the packagingFeedKey environment variable.
    #[arg(long, env = "packagingFeedKey")]
    api_key: Option<String>,
    /// Directory to scan for nupkg files. Repeat to scan multiple roots.
    #[arg(long = "directory", short = 'd')]
    directories: Vec<PathBuf>,
    /// Override the package version inside each nupkg before upload.
    #[arg(long)]
    package_version: Option<String>,
    /// Rewrite only the package version suffix, preserving the package's current prefix version.
    ///
    /// Passing `--version-suffix` with no value defaults to `pre.<yyyyMMdd>.<token>`.
    #[arg(long, num_args = 0..=1, require_equals = true)]
    version_suffix: Option<Option<String>>,
    /// Override the prefix version used when composing a suffix-based package version.
    #[arg(long)]
    prefix_version: Option<String>,
    /// Keep detected workspace package dependency versions unchanged when rewriting package versions.
    #[arg(long)]
    no_rewrite_workspace_dependency_versions: bool,
    /// Skip duplicate packages when the feed already contains the package.
    #[arg(long)]
    skip_duplicate: bool,
    /// Print the planned uploads without pushing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageUpload {
    source_path: PathBuf,
    upload_path: PathBuf,
    package_id: Option<String>,
    package_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageIdentity {
    package_id: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionRewrite {
    Exact(String),
    Suffix {
        prefix_version: Option<String>,
        suffix: String,
    },
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct PduploadConfig {
    source: Option<String>,
    api_key: Option<String>,
    directories: Option<Vec<PathBuf>>,
    package_version: Option<String>,
    version_suffix: Option<String>,
    prefix_version: Option<String>,
    rewrite_workspace_dependency_versions: Option<bool>,
    skip_duplicate: Option<bool>,
    dry_run: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedOptions {
    source: String,
    api_key: Option<String>,
    directories: Vec<PathBuf>,
    version_rewrite: Option<VersionRewrite>,
    rewrite_workspace_dependency_versions: bool,
    skip_duplicate: bool,
    dry_run: bool,
}

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();
    validate_version_arguments(&cli)?;
    let config = load_config(&cli)?;
    let resolved = resolve_options(&cli, config.as_ref())?;
    let package_paths = discover_package_paths(&resolved.directories)?;
    if package_paths.is_empty() {
        bail!(
            "no .nupkg files were found under {}",
            display_paths(&resolved.directories)
        );
    }

    let temp_dir = if resolved.version_rewrite.is_some() {
        Some(TempDir::new().context("failed to create temporary repack directory")?)
    } else {
        None
    };

    let uploads = prepare_uploads(
        &package_paths,
        resolved.version_rewrite.as_ref(),
        resolved.rewrite_workspace_dependency_versions,
        temp_dir.as_ref(),
    )?;
    print_upload_plan(&uploads, &resolved.source);

    if resolved.dry_run {
        return Ok(());
    }

    let api_key = resolved.api_key.as_deref().ok_or_else(|| {
        anyhow!("missing API key; pass --api-key or set the packagingFeedKey environment variable")
    })?;

    for upload in &uploads {
        push_package(upload, &resolved.source, api_key, resolved.skip_duplicate)?;
    }

    Ok(())
}

fn load_config(cli: &Cli) -> Result<Option<PduploadConfig>> {
    let config_path = match &cli.config {
        Some(path) => Some(path.clone()),
        None => discover_config_path(),
    };

    let Some(config_path) = config_path else {
        return Ok(None);
    };

    if !config_path.exists() {
        if cli.config.is_some() {
            bail!(
                "configured config file does not exist: {}",
                config_path.display()
            );
        }
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config = toml::from_str::<PduploadConfig>(&contents)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    Ok(Some(config))
}

fn discover_config_path() -> Option<PathBuf> {
    candidate_config_paths()
        .into_iter()
        .find(|path| path.exists())
}

fn candidate_config_paths() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from(DEFAULT_CONFIG_FILE_NAME)];

    if let Some(global_path) = global_config_path() {
        candidates.push(global_path);
    }

    candidates
}

fn global_config_path() -> Option<PathBuf> {
    if cfg!(windows)
        && let Some(app_data) = env::var_os("APPDATA")
    {
        return Some(
            PathBuf::from(app_data)
                .join(DEFAULT_CONFIG_DIRECTORY_NAME)
                .join(DEFAULT_CONFIG_FILE_NAME),
        );
    }

    if let Some(xdg_config_home) = env::var_os("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(xdg_config_home)
                .join(DEFAULT_CONFIG_DIRECTORY_NAME)
                .join(DEFAULT_CONFIG_FILE_NAME),
        );
    }

    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join(DEFAULT_CONFIG_DIRECTORY_NAME)
            .join(DEFAULT_CONFIG_FILE_NAME)
    })
}

fn resolve_options(cli: &Cli, config: Option<&PduploadConfig>) -> Result<ResolvedOptions> {
    let source = cli
        .source
        .clone()
        .or_else(|| config.and_then(|config| config.source.clone()))
        .ok_or_else(|| anyhow!("missing source; pass --source or set source in pdupload.toml"))?;

    let api_key = cli
        .api_key
        .clone()
        .or_else(|| config.and_then(|config| config.api_key.clone()));

    let directories = resolve_search_directories(cli, config)?;
    let version_rewrite = resolve_version_rewrite(cli, config)?;
    let rewrite_workspace_dependency_versions = if cli.no_rewrite_workspace_dependency_versions {
        false
    } else {
        config
            .and_then(|config| config.rewrite_workspace_dependency_versions)
            .unwrap_or(true)
    };
    let skip_duplicate = cli.skip_duplicate
        || config
            .and_then(|config| config.skip_duplicate)
            .unwrap_or(false);
    let dry_run = cli.dry_run || config.and_then(|config| config.dry_run).unwrap_or(false);

    Ok(ResolvedOptions {
        source,
        api_key,
        directories,
        version_rewrite,
        rewrite_workspace_dependency_versions,
        skip_duplicate,
        dry_run,
    })
}

/// Resolves the set of directories to scan for nupkg files.
fn resolve_search_directories(cli: &Cli, config: Option<&PduploadConfig>) -> Result<Vec<PathBuf>> {
    if !cli.directories.is_empty() {
        let mut resolved = Vec::with_capacity(cli.directories.len());
        for directory in &cli.directories {
            if !directory.exists() {
                bail!(
                    "configured directory does not exist: {}",
                    directory.display()
                );
            }
            if !directory.is_dir() {
                bail!(
                    "configured path is not a directory: {}",
                    directory.display()
                );
            }
            resolved.push(directory.clone());
        }
        return Ok(resolved);
    }

    if let Some(config_directories) = config.and_then(|config| config.directories.as_ref()) {
        let mut resolved = Vec::with_capacity(config_directories.len());
        for directory in config_directories {
            if !directory.exists() {
                bail!(
                    "configured directory does not exist: {}",
                    directory.display()
                );
            }
            if !directory.is_dir() {
                bail!(
                    "configured path is not a directory: {}",
                    directory.display()
                );
            }
            resolved.push(directory.clone());
        }
        return Ok(resolved);
    }

    let defaults = DEFAULT_PACKAGE_DIRECTORIES
        .iter()
        .map(PathBuf::from)
        .filter(|directory| directory.exists() && directory.is_dir())
        .collect::<Vec<_>>();

    if defaults.is_empty() {
        bail!(
            "none of the default package directories exist: {}",
            DEFAULT_PACKAGE_DIRECTORIES.join(", ")
        );
    }

    Ok(defaults)
}

/// Discovers nupkg files from the configured directory roots.
fn discover_package_paths(directories: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut package_paths = Vec::new();

    for directory in directories {
        for entry in WalkDir::new(directory) {
            let entry = entry.with_context(|| {
                format!("failed to walk package directory {}", directory.display())
            })?;

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            if is_uploadable_package(path) {
                package_paths.push(path.to_path_buf());
            }
        }
    }

    package_paths.sort();
    package_paths.dedup();
    Ok(package_paths)
}

/// Prepares uploadable package paths, rewriting versions into temporary nupkgs when requested.
fn prepare_uploads(
    package_paths: &[PathBuf],
    version_rewrite: Option<&VersionRewrite>,
    rewrite_workspace_dependency_versions: bool,
    temp_dir: Option<&TempDir>,
) -> Result<Vec<PackageUpload>> {
    let workspace_dependency_versions = if rewrite_workspace_dependency_versions {
        version_rewrite
            .map(|version_rewrite| {
                collect_workspace_dependency_versions(package_paths, version_rewrite)
            })
            .transpose()?
    } else {
        None
    };

    package_paths
        .iter()
        .map(|package_path| match version_rewrite {
            Some(version_rewrite) => repack_package_with_version(
                package_path,
                version_rewrite,
                workspace_dependency_versions.as_ref(),
                temp_dir
                    .expect("temp directory must exist when version rewriting is enabled")
                    .path(),
            ),
            None => Ok(PackageUpload {
                source_path: package_path.clone(),
                upload_path: package_path.clone(),
                package_id: None,
                package_version: None,
            }),
        })
        .collect()
}

/// Rewrites the nuspec version inside one nupkg and returns the path of the repacked upload file.
fn repack_package_with_version(
    package_path: &Path,
    version_rewrite: &VersionRewrite,
    workspace_dependency_versions: Option<&HashMap<String, String>>,
    output_directory: &Path,
) -> Result<PackageUpload> {
    let source_file = File::open(package_path)
        .with_context(|| format!("failed to open {}", package_path.display()))?;
    let mut archive = ZipArchive::new(source_file).with_context(|| {
        format!(
            "failed to read {} as a nupkg archive",
            package_path.display()
        )
    })?;

    let mut package_identity = None;
    let output_path = output_directory.join(temp_package_name(package_path)?);
    let output_file = File::create(&output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut writer = ZipWriter::new(output_file);

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).with_context(|| {
            format!(
                "failed to read zip entry {index} from {}",
                package_path.display()
            )
        })?;
        let entry_name = entry.name().to_string();
        let options = SimpleFileOptions::default()
            .compression_method(normalize_compression(entry.compression()));

        if entry.is_dir() {
            writer
                .add_directory(&entry_name, options)
                .with_context(|| {
                    format!("failed to add directory {entry_name} to repacked package")
                })?;
            continue;
        }

        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).with_context(|| {
            format!(
                "failed to read {entry_name} from {}",
                package_path.display()
            )
        })?;

        writer
            .start_file(&entry_name, options)
            .with_context(|| format!("failed to add {entry_name} to repacked package"))?;

        if entry_name.ends_with(".nuspec") {
            let updated_nuspec =
                rewrite_nuspec_version(&bytes, version_rewrite, workspace_dependency_versions)?;
            package_identity = Some(updated_nuspec.identity.clone());
            writer
                .write_all(&updated_nuspec.contents)
                .with_context(|| {
                    format!(
                        "failed to write updated nuspec for {}",
                        package_path.display()
                    )
                })?;
        } else {
            writer
                .write_all(&bytes)
                .with_context(|| format!("failed to copy {entry_name} into repacked package"))?;
        }
    }

    writer.finish().with_context(|| {
        format!(
            "failed to finalize repacked package {}",
            output_path.display()
        )
    })?;

    let package_identity = package_identity.ok_or_else(|| {
        anyhow!(
            "package {} does not contain a .nuspec file",
            package_path.display()
        )
    })?;

    let renamed_output_path = output_directory.join(package_file_name(&package_identity));
    if renamed_output_path != output_path {
        fs::rename(&output_path, &renamed_output_path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                output_path.display(),
                renamed_output_path.display()
            )
        })?;
    }

    Ok(PackageUpload {
        source_path: package_path.to_path_buf(),
        upload_path: renamed_output_path,
        package_id: Some(package_identity.package_id),
        package_version: Some(package_identity.version),
    })
}

/// Pushes one package to the configured NuGet source.
fn push_package(
    upload: &PackageUpload,
    source: &str,
    api_key: &str,
    skip_duplicate: bool,
) -> Result<()> {
    let mut command = Command::new("dotnet");
    command
        .arg("nuget")
        .arg("push")
        .arg(&upload.upload_path)
        .arg("--source")
        .arg(source)
        .arg("--api-key")
        .arg(api_key);

    if skip_duplicate {
        command.arg("--skip-duplicate");
    }

    let status = command.status().with_context(|| {
        format!(
            "failed to invoke dotnet nuget push for {}",
            upload.upload_path.display()
        )
    })?;

    if !status.success() {
        bail!(
            "dotnet nuget push failed for {}",
            upload.upload_path.display()
        )
    }

    Ok(())
}

/// Rewrites the package version inside a nuspec XML document.
fn rewrite_nuspec_version(
    nuspec_bytes: &[u8],
    version_rewrite: &VersionRewrite,
    workspace_dependency_versions: Option<&HashMap<String, String>>,
) -> Result<RewrittenNuspec> {
    let mut root = Element::parse(Cursor::new(nuspec_bytes))
        .context("failed to parse nuspec XML while rewriting package version")?;
    let metadata = child_element_mut(&mut root, "metadata")?
        .ok_or_else(|| anyhow!("nuspec does not contain a metadata element"))?;

    let package_id = child_text(metadata, "id")?
        .ok_or_else(|| anyhow!("nuspec metadata does not contain an id element"))?;
    let current_version = child_text(metadata, "version")?
        .ok_or_else(|| anyhow!("nuspec metadata does not contain a version element"))?;
    let package_version = resolve_package_version(&current_version, version_rewrite)?;
    set_child_text(metadata, "version", &package_version)?;
    if let Some(workspace_dependency_versions) = workspace_dependency_versions {
        rewrite_dependency_versions(metadata, workspace_dependency_versions);
    }

    let mut contents = Vec::new();
    root.write_with_config(
        &mut contents,
        EmitterConfig::new()
            .perform_indent(true)
            .write_document_declaration(false),
    )
    .context("failed to serialize rewritten nuspec XML")?;

    Ok(RewrittenNuspec {
        identity: PackageIdentity {
            package_id,
            version: package_version.to_string(),
        },
        contents,
    })
}

fn collect_workspace_dependency_versions(
    package_paths: &[PathBuf],
    version_rewrite: &VersionRewrite,
) -> Result<HashMap<String, String>> {
    let mut workspace_dependency_versions = HashMap::with_capacity(package_paths.len());

    for package_path in package_paths {
        let identity = read_package_identity_with_rewritten_version(package_path, version_rewrite)?;
        workspace_dependency_versions.insert(identity.package_id, identity.version);
    }

    Ok(workspace_dependency_versions)
}

fn read_package_identity_with_rewritten_version(
    package_path: &Path,
    version_rewrite: &VersionRewrite,
) -> Result<PackageIdentity> {
    let source_file = File::open(package_path)
        .with_context(|| format!("failed to open {}", package_path.display()))?;
    let mut archive = ZipArchive::new(source_file).with_context(|| {
        format!(
            "failed to read {} as a nupkg archive",
            package_path.display()
        )
    })?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).with_context(|| {
            format!(
                "failed to read zip entry {index} from {}",
                package_path.display()
            )
        })?;

        if entry.is_dir() || !entry.name().ends_with(".nuspec") {
            continue;
        }

        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).with_context(|| {
            format!(
                "failed to read {} from {}",
                entry.name(),
                package_path.display()
            )
        })?;

        return rewritten_package_identity(&bytes, version_rewrite);
    }

    Err(anyhow!(
        "package {} does not contain a .nuspec file",
        package_path.display()
    ))
}

fn rewritten_package_identity(
    nuspec_bytes: &[u8],
    version_rewrite: &VersionRewrite,
) -> Result<PackageIdentity> {
    let mut root = Element::parse(Cursor::new(nuspec_bytes))
        .context("failed to parse nuspec XML while reading package identity")?;
    let metadata = child_element_mut(&mut root, "metadata")?
        .ok_or_else(|| anyhow!("nuspec does not contain a metadata element"))?;

    let package_id = child_text(metadata, "id")?
        .ok_or_else(|| anyhow!("nuspec metadata does not contain an id element"))?;
    let current_version = child_text(metadata, "version")?
        .ok_or_else(|| anyhow!("nuspec metadata does not contain a version element"))?;

    Ok(PackageIdentity {
        package_id,
        version: resolve_package_version(&current_version, version_rewrite)?,
    })
}

fn rewrite_dependency_versions(
    element: &mut Element,
    workspace_dependency_versions: &HashMap<String, String>,
) {
    if element.name == "dependency"
        && let Some(dependency_id) = element.attributes.get("id")
        && let Some(rewritten_version) = workspace_dependency_versions.get(dependency_id)
    {
        element
            .attributes
            .insert("version".to_string(), rewritten_version.clone());
    }

    for child in &mut element.children {
        if let XMLNode::Element(child_element) = child {
            rewrite_dependency_versions(child_element, workspace_dependency_versions);
        }
    }
}

fn validate_version_arguments(cli: &Cli) -> Result<()> {
    if cli.package_version.is_some() && cli.version_suffix.is_some() {
        bail!("--package-version and --version-suffix are mutually exclusive")
    }

    if cli.package_version.is_some() && cli.prefix_version.is_some() {
        bail!("--package-version and --prefix-version are mutually exclusive")
    }

    Ok(())
}

fn resolve_version_rewrite(
    cli: &Cli,
    config: Option<&PduploadConfig>,
) -> Result<Option<VersionRewrite>> {
    if let Some(package_version) = &cli.package_version {
        return Ok(Some(VersionRewrite::Exact(package_version.clone())));
    }

    let prefix_version = cli.prefix_version.clone();

    match &cli.version_suffix {
        Some(Some(suffix)) => Ok(Some(VersionRewrite::Suffix {
            prefix_version,
            suffix: suffix.clone(),
        })),
        Some(None) => Ok(Some(VersionRewrite::Suffix {
            prefix_version,
            suffix: default_version_suffix()?,
        })),
        None if prefix_version.is_some() => Ok(Some(VersionRewrite::Suffix {
            prefix_version,
            suffix: default_version_suffix()?,
        })),
        None => resolve_version_rewrite_from_config(config),
    }
}

fn resolve_version_rewrite_from_config(
    config: Option<&PduploadConfig>,
) -> Result<Option<VersionRewrite>> {
    let Some(config) = config else {
        return Ok(None);
    };

    match (
        &config.package_version,
        &config.version_suffix,
        &config.prefix_version,
    ) {
        (Some(_), Some(_), _) => {
            bail!("config file cannot set both package_version and version_suffix")
        }
        (Some(_), _, Some(_)) => {
            bail!("config file cannot set both package_version and prefix_version")
        }
        (Some(package_version), None, None) => {
            Ok(Some(VersionRewrite::Exact(package_version.clone())))
        }
        (None, Some(version_suffix), prefix_version) if version_suffix.trim().is_empty() => {
            Ok(Some(VersionRewrite::Suffix {
                prefix_version: prefix_version.clone(),
                suffix: default_version_suffix()?,
            }))
        }
        (None, Some(version_suffix), prefix_version) => Ok(Some(VersionRewrite::Suffix {
            prefix_version: prefix_version.clone(),
            suffix: version_suffix.clone(),
        })),
        (None, None, Some(prefix_version)) => Ok(Some(VersionRewrite::Suffix {
            prefix_version: Some(prefix_version.clone()),
            suffix: default_version_suffix()?,
        })),
        (None, None, None) => Ok(None),
    }
}

fn resolve_package_version(
    current_version: &str,
    version_rewrite: &VersionRewrite,
) -> Result<String> {
    match version_rewrite {
        VersionRewrite::Exact(version) => Ok(version.clone()),
        VersionRewrite::Suffix {
            prefix_version,
            suffix,
        } => {
            let prefix = prefix_version
                .as_deref()
                .unwrap_or_else(|| version_prefix(current_version));
            Ok(format!("{prefix}-{suffix}"))
        }
    }
}

fn version_prefix(version: &str) -> &str {
    let without_build_metadata = version
        .split_once('+')
        .map(|(prefix, _)| prefix)
        .unwrap_or(version);

    without_build_metadata
        .split_once('-')
        .map(|(prefix, _)| prefix)
        .unwrap_or(without_build_metadata)
}

fn default_version_suffix() -> Result<String> {
    let current_time = OffsetDateTime::now_utc();
    let current_date = current_time
        .format(DEFAULT_DATE_FORMAT)
        .context("failed to format the default package version suffix date")?;
    let token = default_suffix_token(current_time.unix_timestamp_nanos(), process::id());
    Ok(format!("{DEFAULT_SUFFIX_PREFIX}.{current_date}.{token}"))
}

fn default_suffix_token(unix_timestamp_nanos: i128, process_id: u32) -> String {
    let mixed = (unix_timestamp_nanos as u128) ^ u128::from(process_id);
    let lower_bits = (mixed & 0xffff_ffff) as u32;
    format!("{lower_bits:0width$x}", width = DEFAULT_SUFFIX_TOKEN_WIDTH)
}

/// Prints the final set of packages that will be uploaded.
fn print_upload_plan(uploads: &[PackageUpload], source: &str) {
    println!("source: {source}");
    for upload in uploads {
        match (&upload.package_id, &upload.package_version) {
            (Some(package_id), Some(package_version)) => println!(
                "upload {} as {} {}",
                upload.source_path.display(),
                package_id,
                package_version
            ),
            _ => println!("upload {}", upload.upload_path.display()),
        }
    }
}

fn child_element_mut<'a>(element: &'a mut Element, name: &str) -> Result<Option<&'a mut Element>> {
    Ok(element.children.iter_mut().find_map(|child| match child {
        XMLNode::Element(child_element) if child_element.name == name => Some(child_element),
        _ => None,
    }))
}

fn child_text(element: &Element, name: &str) -> Result<Option<String>> {
    Ok(element.children.iter().find_map(|child| match child {
        XMLNode::Element(child_element) if child_element.name == name => {
            child_element.get_text().map(|text| text.trim().to_string())
        }
        _ => None,
    }))
}

fn set_child_text(element: &mut Element, name: &str, value: &str) -> Result<()> {
    let child = child_element_mut(element, name)?
        .ok_or_else(|| anyhow!("nuspec metadata does not contain a {name} element"))?;
    child.children.clear();
    child.children.push(XMLNode::Text(value.to_string()));
    Ok(())
}

fn normalize_compression(method: CompressionMethod) -> CompressionMethod {
    match method {
        CompressionMethod::Stored | CompressionMethod::Deflated => method,
        _ => CompressionMethod::Deflated,
    }
}

fn temp_package_name(package_path: &Path) -> Result<String> {
    let base_name = package_path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("{} does not have a valid file name", package_path.display()))?;
    Ok(format!("{base_name}.tmp.nupkg"))
}

fn package_file_name(identity: &PackageIdentity) -> String {
    format!("{}.{}.nupkg", identity.package_id, identity.version)
}

fn is_uploadable_package(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };

    file_name.ends_with(".nupkg") && !file_name.ends_with(".symbols.nupkg")
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug)]
struct RewrittenNuspec {
    identity: PackageIdentity,
    contents: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_SUFFIX_TOKEN_WIDTH, PackageIdentity, PduploadConfig, VersionRewrite,
        candidate_config_paths, child_text, default_suffix_token, default_version_suffix,
        display_paths, global_config_path, is_uploadable_package, package_file_name,
        resolve_options, resolve_version_rewrite_from_config, rewrite_nuspec_version,
        version_prefix,
    };
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
    use xmltree::Element;

    fn cli_with_defaults() -> super::Cli {
        super::Cli {
            config: None,
            source: None,
            api_key: None,
            directories: Vec::new(),
            package_version: None,
            version_suffix: None,
            prefix_version: None,
            no_rewrite_workspace_dependency_versions: false,
            skip_duplicate: false,
            dry_run: false,
        }
    }

    #[test]
    fn symbols_packages_are_skipped() {
        assert!(is_uploadable_package(Path::new("a.1.2.3.nupkg")));
        assert!(!is_uploadable_package(Path::new("a.1.2.3.symbols.nupkg")));
    }

    #[test]
    fn rewritten_nuspec_uses_overridden_version() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>Example.Package</id>
    <version>1.0.0</version>
  </metadata>
</package>"#;

        let rewritten = rewrite_nuspec_version(
            nuspec.as_bytes(),
            &VersionRewrite::Exact("2.5.0".to_string()),
            None,
        )
        .unwrap();
        let root = Element::parse(rewritten.contents.as_slice()).unwrap();
        let metadata = root.get_child("metadata").unwrap();

        assert_eq!(rewritten.identity.package_id, "Example.Package");
        assert_eq!(rewritten.identity.version, "2.5.0");
        assert_eq!(
            child_text(metadata, "version").unwrap().as_deref(),
            Some("2.5.0")
        );
    }

    #[test]
    fn rewritten_nuspec_can_replace_only_the_version_suffix() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>Example.Package</id>
    <version>1.0.0-alpha.4</version>
  </metadata>
</package>"#;

        let rewritten = rewrite_nuspec_version(
            nuspec.as_bytes(),
            &VersionRewrite::Suffix {
                prefix_version: None,
                suffix: "pre.20260427".to_string(),
            },
            None,
        )
        .unwrap();

        assert_eq!(rewritten.identity.version, "1.0.0-pre.20260427");
    }

    #[test]
    fn rewritten_nuspec_can_use_explicit_prefix_version_with_suffix() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>Example.Package</id>
    <version>1.0.0-alpha.4</version>
  </metadata>
</package>"#;

        let rewritten = rewrite_nuspec_version(
            nuspec.as_bytes(),
            &VersionRewrite::Suffix {
                prefix_version: Some("2.1.0".to_string()),
                suffix: "pre.20260427".to_string(),
            },
            None,
        )
        .unwrap();

        assert_eq!(rewritten.identity.version, "2.1.0-pre.20260427");
    }

    #[test]
    fn rewritten_nuspec_updates_detected_workspace_dependency_versions() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
    <metadata>
        <id>Example.Package</id>
        <version>1.0.0</version>
        <dependencies>
            <group targetFramework="net8.0">
                <dependency id="B" version="[1.0.0]" />
                <dependency id="C" version="2.0.0" />
                <dependency id="External" version="9.9.9" />
            </group>
        </dependencies>
    </metadata>
</package>"#;

        let rewritten = rewrite_nuspec_version(
            nuspec.as_bytes(),
            &VersionRewrite::Exact("2.5.0".to_string()),
            Some(&HashMap::from([
                ("B".to_string(), "2.5.0".to_string()),
                ("C".to_string(), "3.1.0".to_string()),
            ])),
        )
        .unwrap();
        let root = Element::parse(rewritten.contents.as_slice()).unwrap();
        let metadata = root.get_child("metadata").unwrap();
        let dependencies = metadata.get_child("dependencies").unwrap();
        let group = dependencies.get_child("group").unwrap();

        assert_eq!(dependency_version(group, "B"), Some("2.5.0"));
        assert_eq!(dependency_version(group, "C"), Some("3.1.0"));
        assert_eq!(dependency_version(group, "External"), Some("9.9.9"));
    }

    #[test]
    fn package_file_name_uses_id_and_version() {
        let identity = PackageIdentity {
            package_id: "Example.Package".to_string(),
            version: "3.1.4".to_string(),
        };

        assert_eq!(package_file_name(&identity), "Example.Package.3.1.4.nupkg");
    }

    #[test]
    fn display_paths_joins_multiple_paths() {
        let display = display_paths(&[PathBuf::from("bin/nuget"), PathBuf::from("package")]);
        assert_eq!(display, "bin/nuget, package");
    }

    #[test]
    fn version_prefix_removes_prerelease_and_build_metadata() {
        assert_eq!(version_prefix("1.2.3-alpha.1+build.5"), "1.2.3");
        assert_eq!(version_prefix("2.0.0"), "2.0.0");
    }

    #[test]
    fn default_version_suffix_uses_pre_prefix() {
        let suffix = default_version_suffix().unwrap();
        let parts = suffix.split('.').collect::<Vec<_>>();

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "pre");
        assert_eq!(parts[1].len(), 8);
        assert_eq!(parts[2].len(), DEFAULT_SUFFIX_TOKEN_WIDTH);
    }

    #[test]
    fn default_suffix_token_is_zero_padded_hex() {
        let token = default_suffix_token(0x1234_abcd, 0x42);
        assert_eq!(token.len(), DEFAULT_SUFFIX_TOKEN_WIDTH);
        assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn config_can_supply_default_source_and_flags() {
        let cli = cli_with_defaults();
        let temp_dir = tempdir().unwrap();
        let config = PduploadConfig {
            source: Some("MyFeed".to_string()),
            api_key: Some("secret".to_string()),
            directories: Some(vec![temp_dir.path().to_path_buf()]),
            package_version: None,
            version_suffix: Some("ci.4".to_string()),
            prefix_version: None,
            rewrite_workspace_dependency_versions: None,
            skip_duplicate: Some(true),
            dry_run: Some(true),
        };

        let resolved = resolve_options(&cli, Some(&config)).unwrap();

        assert_eq!(resolved.source, "MyFeed");
        assert_eq!(resolved.api_key.as_deref(), Some("secret"));
        assert_eq!(
            resolved.version_rewrite,
            Some(VersionRewrite::Suffix {
                prefix_version: None,
                suffix: "ci.4".to_string(),
            })
        );
        assert!(resolved.rewrite_workspace_dependency_versions);
        assert!(resolved.skip_duplicate);
        assert!(resolved.dry_run);
    }

    #[test]
    fn cli_overrides_config_values() {
        let temp_dir = tempdir().unwrap();
        let cli = super::Cli {
            source: Some("CliFeed".to_string()),
            api_key: Some("cli-key".to_string()),
            directories: vec![temp_dir.path().to_path_buf()],
            package_version: Some("2.0.0".to_string()),
            prefix_version: None,
            no_rewrite_workspace_dependency_versions: true,
            skip_duplicate: true,
            dry_run: true,
            ..cli_with_defaults()
        };
        let config = PduploadConfig {
            source: Some("ConfigFeed".to_string()),
            api_key: Some("config-key".to_string()),
            directories: None,
            package_version: Some("1.0.0".to_string()),
            version_suffix: None,
            prefix_version: None,
            rewrite_workspace_dependency_versions: Some(true),
            skip_duplicate: Some(false),
            dry_run: Some(false),
        };

        let resolved = resolve_options(&cli, Some(&config)).unwrap();

        assert_eq!(resolved.source, "CliFeed");
        assert_eq!(resolved.api_key.as_deref(), Some("cli-key"));
        assert_eq!(
            resolved.version_rewrite,
            Some(VersionRewrite::Exact("2.0.0".to_string()))
        );
        assert!(!resolved.rewrite_workspace_dependency_versions);
        assert!(resolved.skip_duplicate);
        assert!(resolved.dry_run);
    }

    #[test]
    fn config_rejects_conflicting_version_fields() {
        let config = PduploadConfig {
            source: None,
            api_key: None,
            directories: None,
            package_version: Some("1.0.0".to_string()),
            version_suffix: Some("ci.1".to_string()),
            prefix_version: None,
            rewrite_workspace_dependency_versions: None,
            skip_duplicate: None,
            dry_run: None,
        };

        let error = resolve_version_rewrite_from_config(Some(&config)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot set both package_version and version_suffix")
        );
    }

    #[test]
    fn candidate_config_paths_start_with_local_file() {
        let candidates = candidate_config_paths();

        assert_eq!(candidates.first(), Some(&PathBuf::from("pdupload.toml")));
    }

    #[test]
    fn config_can_supply_prefix_version_for_suffix_mode() {
        let config = PduploadConfig {
            source: None,
            api_key: None,
            directories: None,
            package_version: None,
            version_suffix: Some("ci.7".to_string()),
            prefix_version: Some("5.4.3".to_string()),
            rewrite_workspace_dependency_versions: None,
            skip_duplicate: None,
            dry_run: None,
        };

        let rewrite = resolve_version_rewrite_from_config(Some(&config)).unwrap();
        assert_eq!(
            rewrite,
            Some(VersionRewrite::Suffix {
                prefix_version: Some("5.4.3".to_string()),
                suffix: "ci.7".to_string(),
            })
        );
    }

    #[test]
    fn config_rejects_conflicting_package_and_prefix_versions() {
        let config = PduploadConfig {
            source: None,
            api_key: None,
            directories: None,
            package_version: Some("1.0.0".to_string()),
            version_suffix: None,
            prefix_version: Some("2.0.0".to_string()),
            rewrite_workspace_dependency_versions: None,
            skip_duplicate: None,
            dry_run: None,
        };

        let error = resolve_version_rewrite_from_config(Some(&config)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot set both package_version and prefix_version")
        );
    }

    #[test]
    fn global_config_path_uses_platform_convention() {
        if let Some(path) = global_config_path() {
            assert!(path.ends_with(Path::new("pdupload/pdupload.toml")));
        }
    }

    fn dependency_version<'a>(group: &'a Element, dependency_id: &str) -> Option<&'a str> {
        group.children.iter().find_map(|child| match child {
            xmltree::XMLNode::Element(element)
                if element.name == "dependency"
                    && element.attributes.get("id").map(String::as_str) == Some(dependency_id) =>
            {
                element.attributes.get("version").map(String::as_str)
            }
            _ => None,
        })
    }
}
