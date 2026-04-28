# pdupload

`pdupload` is a small CLI for bulk-uploading NuGet packages and optionally rewriting their package versions before pushing them.

It is useful when the workflow is "find a pile of `.nupkg` files, adjust versions, then push all of them with `dotnet nuget push`".

## What it does

- Scans package directories for `.nupkg` files.
- Reads default settings from `pdupload.toml`.
- Supports exact version replacement with `--package-version`.
- Supports suffix-only rewrites with `--version-suffix` and `--prefix-version`.
- Rewrites dependency versions for other detected workspace packages by default.
- Pushes packages with `dotnet nuget push`.
- Can skip duplicates and perform dry runs.

## Defaults

By default, `pdupload` scans these directories relative to the current working directory:

- `bin/nuget`
- `package`

The API key defaults to the `packagingFeedKey` environment variable.

Config is loaded in this order:

1. `--config <path>`
2. `./pdupload.toml`
3. Windows: `%APPDATA%\pdupload\pdupload.toml`
4. Unix: `$XDG_CONFIG_HOME/pdupload/pdupload.toml`
5. Unix fallback: `$HOME/.config/pdupload/pdupload.toml`

Command-line values override config file values.

## Usage

Build and run the tool from the workspace root:

```powershell
cargo run -p pdupload -- --source https://feed.example/v3/index.json --package-version 1.2.3-ci.4
```

Rewrite only the suffix while preserving the existing prefix version:

```powershell
cargo run -p pdupload -- --source MyFeed --version-suffix=ci.4
```

When version rewriting is enabled, `pdupload` also rewrites dependency version attributes that point at other `.nupkg` files detected in the same scan roots. Pass `--no-rewrite-workspace-dependency-versions` to keep those dependency versions unchanged.

Rewrite only the suffix while forcing the prefix version:

```powershell
cargo run -p pdupload -- --source MyFeed --prefix-version 2.1.0 --version-suffix=ci.4
```

Use the default generated suffix format:

```powershell
cargo run -p pdupload -- --source MyFeed --version-suffix
```

Preview uploads without pushing:

```powershell
cargo run -p pdupload -- --source MyFeed --dry-run
```

Scan custom directories:

```powershell
cargo run -p pdupload -- --source MyFeed --directory bin/nuget --directory artifacts/packages --skip-duplicate
```

## Example config

```toml
source = "MyFeed"
api_key = "super-secret-key"
directories = ["bin/nuget", "package"]
prefix_version = "2.1.0"
version_suffix = ""
rewrite_workspace_dependency_versions = true
skip_duplicate = true
```
