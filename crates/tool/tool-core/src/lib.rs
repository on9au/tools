use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const TOOLS_HOME_DIR_NAME: &str = "on9au-tools";
const BIN_DIR_NAME: &str = "bin";
const METADATA_DIR_NAME: &str = "installed";
const REPO_DIR_NAME: &str = "repo";
const WORKTREES_DIR_NAME: &str = "worktrees";
const TOOLS_BIN_ENV_VAR: &str = "ON9AU_TOOLS_BIN";

/// Describes where managed binaries and metadata live on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    home_dir: PathBuf,
    bin_dir: PathBuf,
    metadata_dir: PathBuf,
    repo_dir: PathBuf,
    worktrees_dir: PathBuf,
}

/// Describes the repo-derived version that was installed for a managed tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledToolRecord {
    pub tool_name: String,
    pub package_name: String,
    pub commit: String,
    pub commit_date: String,
    pub installed_at: String,
    pub repository_url: String,
}

impl InstallLayout {
    /// Discovers the install root for managed tools.
    ///
    /// Resolution order:
    /// 1. `ON9AU_TOOLS_HOME`
    /// 2. `%LOCALAPPDATA%\\on9au-tools` on Windows
    /// 3. `$XDG_DATA_HOME/on9au-tools` on Unix
    /// 4. `$HOME/.local/share/on9au-tools` on Unix
    ///
    /// Managed binaries are installed into:
    /// 1. `ON9AU_TOOLS_BIN`
    /// 2. the current executable directory, if it is already on `PATH`
    /// 3. the Cargo bin directory (typically `~/.cargo/bin`)
    pub fn discover() -> Result<Self> {
        let home_dir = resolve_tools_home_dir()?;
        let bin_dir = resolve_managed_bin_dir()?;

        Ok(Self::for_paths(home_dir, bin_dir))
    }

    /// Builds a layout from an explicit home directory.
    pub fn for_home(home_dir: PathBuf) -> Self {
        let bin_dir = home_dir.join(BIN_DIR_NAME);
        Self::for_paths(home_dir, bin_dir)
    }

    /// Builds a layout from explicit home and binary directories.
    pub fn for_paths(home_dir: PathBuf, bin_dir: PathBuf) -> Self {
        let metadata_dir = home_dir.join(METADATA_DIR_NAME);
        let repo_dir = home_dir.join(REPO_DIR_NAME);
        let worktrees_dir = home_dir.join(WORKTREES_DIR_NAME);
        Self {
            home_dir,
            bin_dir,
            metadata_dir,
            repo_dir,
            worktrees_dir,
        }
    }

    /// Returns the root directory that stores managed tool data.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Returns the directory that contains managed executables.
    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    /// Returns the directory that stores installed tool metadata.
    pub fn metadata_dir(&self) -> &Path {
        &self.metadata_dir
    }

    /// Returns the local clone directory for the source tools repository.
    pub fn repo_dir(&self) -> &Path {
        &self.repo_dir
    }

    /// Returns the directory that stores temporary git worktrees.
    pub fn worktrees_dir(&self) -> &Path {
        &self.worktrees_dir
    }

    /// Creates the on-disk directories required by the layout.
    pub fn ensure_exists(&self) -> Result<()> {
        for directory in self.managed_directories() {
            fs::create_dir_all(directory).with_context(|| {
                format!(
                    "failed to create managed directory at {}",
                    directory.display()
                )
            })?;
        }
        Ok(())
    }

    fn managed_directories(&self) -> [&Path; 4] {
        [
            self.home_dir.as_path(),
            self.bin_dir.as_path(),
            self.metadata_dir.as_path(),
            self.worktrees_dir.as_path(),
        ]
    }

    /// Returns the filesystem path for a managed binary name.
    pub fn binary_path(&self, binary_name: &str) -> PathBuf {
        self.bin_dir.join(with_exe_suffix(binary_name))
    }

    /// Returns the metadata file path for a managed binary name.
    pub fn metadata_path(&self, binary_name: &str) -> PathBuf {
        self.metadata_dir.join(format!("{binary_name}.json"))
    }

    /// Lists all managed binaries currently present in the install bin directory.
    pub fn managed_binaries(&self) -> Result<Vec<PathBuf>> {
        if !self.bin_dir.exists() {
            return Ok(Vec::new());
        }

        let mut binaries = fs::read_dir(&self.bin_dir)
            .with_context(|| format!("failed to read {}", self.bin_dir.display()))?
            .map(|entry| -> Result<Option<PathBuf>> {
                let entry = entry.with_context(|| {
                    format!("failed to read an entry from {}", self.bin_dir.display())
                })?;
                let file_type = entry
                    .file_type()
                    .with_context(|| format!("failed to inspect {}", entry.path().display()))?;

                if file_type.is_file() {
                    Ok(Some(entry.path()))
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

        binaries.sort();
        Ok(binaries)
    }

    /// Reads all installed tool metadata records.
    pub fn installed_records(&self) -> Result<Vec<InstalledToolRecord>> {
        if !self.metadata_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = fs::read_dir(&self.metadata_dir)
            .with_context(|| format!("failed to read {}", self.metadata_dir.display()))?
            .map(|entry| -> Result<Option<InstalledToolRecord>> {
                let entry = entry.with_context(|| {
                    format!(
                        "failed to read an entry from {}",
                        self.metadata_dir.display()
                    )
                })?;

                if !entry
                    .file_type()
                    .with_context(|| format!("failed to inspect {}", entry.path().display()))?
                    .is_file()
                {
                    return Ok(None);
                }

                let path = entry.path();
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let record = serde_json::from_str::<InstalledToolRecord>(&contents)
                    .with_context(|| format!("failed to parse {}", path.display()))?;

                Ok(Some(record))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

        records.sort_by(|left, right| left.tool_name.cmp(&right.tool_name));
        Ok(records)
    }

    /// Writes the metadata record for one installed tool.
    pub fn write_installed_record(&self, record: &InstalledToolRecord) -> Result<()> {
        self.ensure_exists()?;

        let path = self.metadata_path(&record.tool_name);
        let contents = serde_json::to_string_pretty(record)
            .with_context(|| format!("failed to serialize metadata for {}", record.tool_name))?;

        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Installs one binary file into the managed bin directory.
pub fn install_binary_file(
    layout: &InstallLayout,
    source: &Path,
    binary_name: &str,
    always_copy: bool,
) -> Result<PathBuf> {
    layout.ensure_exists()?;

    let destination = layout.binary_path(binary_name);
    remove_existing_destination(&destination)?;

    if always_copy {
        copy_binary(source, &destination)?;
    } else if let Err(error) = fs::hard_link(source, &destination) {
        copy_binary(source, &destination)
            .with_context(|| format!("hard link failed first: {error}"))?;
    }

    Ok(destination)
}

fn remove_existing_destination(destination: &Path) -> Result<()> {
    if destination.exists() {
        fs::remove_file(destination)
            .with_context(|| format!("failed to replace {}", destination.display()))?;
    }

    Ok(())
}

/// Returns the managed binary's canonical display name without platform suffix.
pub fn display_name(binary_path: &Path) -> Result<String> {
    let file_name = binary_path
        .file_name()
        .ok_or_else(|| anyhow!("{} does not have a file name", binary_path.display()))?;

    let file_name = file_name.to_string_lossy();
    if cfg!(windows) {
        Ok(file_name.trim_end_matches(".exe").to_string())
    } else {
        Ok(file_name.to_string())
    }
}

/// Normalizes a binary name so it matches the current platform's executable suffix.
pub fn with_exe_suffix(binary_name: &str) -> OsString {
    if cfg!(windows) && !binary_name.ends_with(env::consts::EXE_SUFFIX) {
        return OsString::from(format!("{binary_name}{}", env::consts::EXE_SUFFIX));
    }

    OsString::from(binary_name)
}

fn copy_binary(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy binary from {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn resolve_tools_home_dir() -> Result<PathBuf> {
    if let Some(home_dir) = env::var_os("ON9AU_TOOLS_HOME") {
        return Ok(PathBuf::from(home_dir));
    }

    if cfg!(windows) {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(local_app_data).join(TOOLS_HOME_DIR_NAME));
        }
    } else {
        if let Some(xdg_data_home) = env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg_data_home).join(TOOLS_HOME_DIR_NAME));
        }

        if let Some(home) = env::var_os("HOME") {
            return Ok(PathBuf::from(home)
                .join(".local")
                .join("share")
                .join(TOOLS_HOME_DIR_NAME));
        }
    }

    bail!("could not determine a tools home directory; set ON9AU_TOOLS_HOME")
}

fn resolve_managed_bin_dir() -> Result<PathBuf> {
    if let Some(bin_dir) = env::var_os(TOOLS_BIN_ENV_VAR) {
        return Ok(PathBuf::from(bin_dir));
    }

    if let Some(current_exe_bin_dir) = current_exe_bin_dir()? {
        if is_directory_on_path(&current_exe_bin_dir)
            && !looks_like_cargo_target_bin_dir(&current_exe_bin_dir)
        {
            return Ok(current_exe_bin_dir);
        }
    }

    cargo_bin_dir()
}

fn current_exe_bin_dir() -> Result<Option<PathBuf>> {
    let current_exe = env::current_exe().context("failed to resolve current executable path")?;
    Ok(current_exe.parent().map(Path::to_path_buf))
}

fn cargo_bin_dir() -> Result<PathBuf> {
    if let Some(cargo_home) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(cargo_home).join(BIN_DIR_NAME));
    }

    user_home_dir().map(|home_dir| home_dir.join(".cargo").join(BIN_DIR_NAME))
}

fn user_home_dir() -> Result<PathBuf> {
    if let Some(home_dir) = env::var_os("HOME") {
        return Ok(PathBuf::from(home_dir));
    }

    if cfg!(windows) {
        if let Some(user_profile) = env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(user_profile));
        }
    }

    bail!("could not determine a home directory for the Cargo bin path")
}

fn is_directory_on_path(directory: &Path) -> bool {
    let Ok(path_value) = env::var_os("PATH").ok_or(()) else {
        return false;
    };

    env::split_paths(&path_value).any(|path_entry| same_directory(&path_entry, directory))
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (canonicalize_path(left), canonicalize_path(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

fn looks_like_cargo_target_bin_dir(directory: &Path) -> bool {
    matches!(
        (directory.file_name(), directory.parent().and_then(Path::file_name)),
        (Some(profile), Some(parent))
            if matches!(profile.to_str(), Some("debug" | "release"))
                && parent == "target"
    )
}

fn canonicalize_path(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        InstallLayout, display_name, install_binary_file, is_directory_on_path,
        looks_like_cargo_target_bin_dir, same_directory, with_exe_suffix,
    };
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn explicit_home_builds_bin_directory() {
        let layout = InstallLayout::for_home(PathBuf::from("/tmp/on9au-tools"));
        assert_eq!(layout.bin_dir(), Path::new("/tmp/on9au-tools/bin"));
    }

    #[test]
    fn binary_path_uses_platform_suffix() {
        let layout = InstallLayout::for_home(PathBuf::from("/tmp/on9au-tools"));
        let path = layout.binary_path("sample-tool");
        assert_eq!(
            path,
            Path::new("/tmp/on9au-tools/bin").join(with_exe_suffix("sample-tool"))
        );
    }

    #[test]
    fn display_name_removes_windows_suffix_only() {
        let file_name = if cfg!(windows) {
            Path::new("sample-tool.exe")
        } else {
            Path::new("sample-tool")
        };

        assert_eq!(display_name(file_name).unwrap(), "sample-tool");
    }

    #[test]
    fn suffix_helper_is_idempotent() {
        let first = with_exe_suffix("sample-tool");
        let second = with_exe_suffix(first.to_string_lossy().as_ref());

        if cfg!(windows) {
            assert_eq!(
                second.to_string_lossy(),
                format!("sample-tool{}", env::consts::EXE_SUFFIX)
            );
        } else {
            assert_eq!(second.to_string_lossy(), "sample-tool");
        }
    }

    #[test]
    fn ensure_exists_creates_metadata_and_worktree_directories() {
        let layout = InstallLayout::for_home(unique_temp_path("layout"));
        layout.ensure_exists().unwrap();

        assert!(layout.bin_dir().exists());
        assert!(layout.metadata_dir().exists());
        assert!(layout.worktrees_dir().exists());

        fs::remove_dir_all(layout.home_dir()).unwrap();
    }

    #[test]
    fn install_binary_file_places_binary_in_bin_dir() {
        let root = unique_temp_path("install-binary");
        let layout = InstallLayout::for_home(root.clone());
        layout.ensure_exists().unwrap();

        let source = root.join(with_exe_suffix("sample-source"));
        fs::write(&source, b"hello world").unwrap();

        let destination = install_binary_file(&layout, &source, "sample-tool", true).unwrap();
        assert!(destination.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"hello world");

        fs::remove_dir_all(layout.home_dir()).unwrap();
    }

    #[test]
    fn for_paths_uses_explicit_bin_directory() {
        let layout = InstallLayout::for_paths(
            PathBuf::from("/tmp/on9au-home"),
            PathBuf::from("/tmp/shared-bin"),
        );

        assert_eq!(layout.home_dir(), Path::new("/tmp/on9au-home"));
        assert_eq!(layout.bin_dir(), Path::new("/tmp/shared-bin"));
    }

    #[test]
    fn same_directory_matches_identical_paths() {
        assert!(same_directory(
            Path::new("/tmp/example"),
            Path::new("/tmp/example")
        ));
    }

    #[test]
    fn missing_path_variable_is_not_treated_as_on_path() {
        assert!(!is_directory_on_path(Path::new(
            "/tmp/example-that-probably-is-not-on-path"
        )));
    }

    #[test]
    fn cargo_target_bin_directory_is_rejected_as_managed_bin_target() {
        assert!(looks_like_cargo_target_bin_dir(Path::new("target/debug")));
        assert!(looks_like_cargo_target_bin_dir(Path::new("target/release")));
        assert!(!looks_like_cargo_target_bin_dir(Path::new(".cargo/bin")));
    }

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("on9au-tools-{prefix}-{stamp}"))
    }
}
