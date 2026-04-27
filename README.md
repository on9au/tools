# on9au's amazing cool tools

This repository contains various tools that I have created for my own use. They are not intended for public use and may not be well-documented or user-friendly. Use them at your own risk.

## Workspace shape

This workspace keeps the management surface in the `crates/tool/` area:

- `crates/tool/tool-core`: shared install-layout, binary install, and metadata logic.
- `crates/tool/toolctl`: the real end-user entrypoint.
- `crates/tool/tool-installer`: a low-level file installer that still exists, but is no longer the main UX.

`toolctl` is intended to be the first thing a user installs. After that, `toolctl` should be able to sync this repository, show installable tools, show commit/date-based versions, and install a selected tool from the repo itself.

## Recommended pattern

For this repo, the practical model is:

1. Each installable tool is a normal Cargo package with one or more binary targets.
2. `toolctl` keeps a local clone of this repo in its managed home directory.
3. `toolctl list available` discovers installable binaries from `cargo metadata`.
4. `toolctl versions <tool>` shows recent versions using git commit history for that tool's package directory.
5. `toolctl install <tool>` builds the selected commit in a temporary git worktree and installs the resulting binary.
6. Installed tool metadata is stored with commit and date so `toolctl list installed` can show what is actually on disk.

## Managed install root

Tool state is stored under:

- Windows: `%LOCALAPPDATA%\on9au-tools\bin`
- Unix: `$XDG_DATA_HOME/on9au-tools/bin`
- Fallback on Unix: `$HOME/.local/share/on9au-tools/bin`
- Override on all platforms: `ON9AU_TOOLS_HOME`

Managed binaries are installed into a PATH-friendly directory instead of the tool state directory:

- `ON9AU_TOOLS_BIN`, if you set it
- otherwise the directory containing `toolctl`, if that directory is already on `PATH`
- otherwise the Cargo bin directory, typically `$HOME/.cargo/bin` or `%USERPROFILE%\.cargo\bin`

That means installed tools should be directly invokable from your shell without going through `toolctl run`.

## Commands

Build the workspace:

```powershell
cargo build
```

Inspect the managed install location:

```powershell
cargo run -p toolctl -- doctor
```

Sync the repo cache that `toolctl` builds from:

```powershell
cargo run -p toolctl -- sync
```

List installed tools:

```powershell
cargo run -p toolctl -- list
```

List installable tools discovered from this repo:

```powershell
cargo run -p toolctl -- list available
```

List recent commit/date versions for one tool:

```powershell
cargo run -p toolctl -- versions some-tool
```

Install the latest version of a tool from this repo:

```powershell
cargo run -p toolctl -- install some-tool
```

Install a specific commit of a tool from this repo:

```powershell
cargo run -p toolctl -- install some-tool --version 0123abcd
```

Prompt for tool selection and version selection interactively:

```powershell
cargo run -p toolctl -- install --select-version
```

Run an installed tool through `toolctl`:

```powershell
cargo run -p toolctl -- run some-tool -- --help
```

## Package tools

`pdupload` helps with the annoying "rename the package version and manually push a pile of nupkgs" workflow.

It:

- scans `bin/nuget` and `package` by default
- reads defaults from `pdupload.toml` in the current working directory, then a global config file, or from `--config <path>`
- lets you override those search roots with repeated `--directory` arguments
- can rewrite the nuspec version inside each `.nupkg` before upload
- can rewrite only the prerelease suffix while keeping the package's existing prefix version, or overriding that prefix version explicitly
- pushes with `dotnet nuget push`
- defaults `--api-key` from the `packagingFeedKey` environment variable

Example:

```powershell
cargo run -p pdupload -- --source https://feed.example/v3/index.json --package-version 1.2.3-ci.4
```

Suffix-only version rewrite using the package's current prefix version:

```powershell
cargo run -p pdupload -- --source MyFeed --version-suffix=ci.4
```

Suffix-only version rewrite with an explicit prefix version:

```powershell
cargo run -p pdupload -- --source MyFeed --prefix-version 2.1.0 --version-suffix=ci.4
```

Default suffix mode, which uses `(prefix-version)-pre.<yyyyMMdd>.<token>`:

```powershell
cargo run -p pdupload -- --source MyFeed --version-suffix
```

Config file defaults with CLI overrides:

```toml
source = "MyFeed"
api_key = "super-secret-key"
directories = ["bin/nuget", "package"]
prefix_version = "2.1.0"
version_suffix = ""
skip_duplicate = true
```

By default, `pdupload` looks for config in this order:

- `--config <path>` if you pass it
- `./pdupload.toml`
- Windows: `%APPDATA%\pdupload\pdupload.toml`
- Unix: `$XDG_CONFIG_HOME/pdupload/pdupload.toml`
- Unix fallback: `$HOME/.config/pdupload/pdupload.toml`

Command-line arguments still win over config values.

Custom directories:

```powershell
cargo run -p pdupload -- --source MyFeed --directory bin/nuget --directory artifacts/packages --skip-duplicate
```

## Version model

The version model is repo-native rather than semver-heavy:

- available versions come from git commits affecting the tool's package directory
- installs record the exact commit hash and commit date
- `toolctl` can rebuild an older version by checking out that commit in a temporary worktree

That is a better fit for a personal tools repo where the source of truth is this repository history.
