# tool

`tool` is the management surface for this repository's installable binaries.

The directory contains the packages that discover tools from this repo, build them from git history, and install the resulting binaries into a managed location.

## Packages

- `toolctl`: the main user-facing CLI.
- `tool-core`: shared install layout, metadata, and binary install helpers.
- `tool-installer`: a low-level binary installer built on `tool-core`.

## Managed layout

Managed tool state is stored under:

- Windows: `%LOCALAPPDATA%\on9au-tools`
- Unix: `$XDG_DATA_HOME/on9au-tools`
- Unix fallback: `$HOME/.local/share/on9au-tools`
- Override: `ON9AU_TOOLS_HOME`

Managed binaries are installed into:

- `ON9AU_TOOLS_BIN`, if set
- otherwise the directory containing `toolctl`, when that directory is already on `PATH`
- otherwise the Cargo bin directory, typically `%USERPROFILE%\.cargo\bin` or `$HOME/.cargo/bin`

## toolctl workflow

`toolctl` treats this repository as the source of truth for installable tools.

The practical flow is:

1. Sync the repository into a local cache.
2. Discover installable binaries from Cargo metadata.
3. List recent commit-based versions for a tool.
4. Build a selected version in a temporary worktree.
5. Install the resulting binary and record metadata.

## Common commands

Inspect the managed install layout:

```powershell
cargo run -p toolctl -- doctor
```

Sync the local repository cache:

```powershell
cargo run -p toolctl -- sync
```

List installed tools:

```powershell
cargo run -p toolctl -- list
```

List tools available from the repo:

```powershell
cargo run -p toolctl -- list available
```

Show recent versions for one tool:

```powershell
cargo run -p toolctl -- versions some-tool
```

Install the latest version of a tool:

```powershell
cargo run -p toolctl -- install some-tool
```

Install a specific commit:

```powershell
cargo run -p toolctl -- install some-tool --version 0123abcd
```

Prompt for tool and version selection:

```powershell
cargo run -p toolctl -- install --select-version
```

Run an installed tool:

```powershell
cargo run -p toolctl -- run some-tool -- --help
```

## tool-installer

`tool-installer` is a narrower helper that installs one existing binary into the managed bin directory.

Example:

```powershell
cargo run -p tool-installer -- install .\target\release\some-tool.exe
```

Use `--copy` when you do not want it to attempt a hard link first.
