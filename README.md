# on9au's amazing cool tools

This repository contains various tools that I have created for my own use. They are not intended for public use and may not be well-documented or user-friendly. Use them at your own risk.

## Workspace overview

This workspace is split into a few tool areas under `crates/`:

- `crates/tool/`: the tool management surface, including `toolctl`
- `crates/package-definitions/`: package-related utilities such as `pdupload`
- `crates/migration/`: migration helpers such as `upstream-migration-planner`

The top-level README is only a map of the workspace. Tool-specific usage and flags should live in each tool area's own README.

## Start with toolctl

`toolctl` is the main end-user entrypoint for this repository.

Use it to sync this repository locally, discover installable tools, inspect recent commit-based versions, and install managed binaries.

Common commands:

```powershell
cargo run -p toolctl -- doctor
cargo run -p toolctl -- sync
cargo run -p toolctl -- list available
cargo run -p toolctl -- versions some-tool
cargo run -p toolctl -- install some-tool
```

For the full management model, install layout, and `toolctl` or `tool-installer` usage, see [crates/tool/README.md](crates/tool/README.md).

## Tool documentation

Use the README in each tool area for details:

- [crates/tool/README.md](crates/tool/README.md): tool management utilities, especially `toolctl`
- [crates/package-definitions/README.md](crates/package-definitions/README.md): package utilities, currently `pdupload`
- [crates/migration/README.md](crates/migration/README.md): migration utilities, currently `upstream-migration-planner`

## Build

Build the workspace:

```powershell
cargo build
```

## Version model

The version model is repo-native rather than semver-heavy:

- available versions come from git commits affecting the tool's package directory
- installs record the exact commit hash and commit date
- `toolctl` can rebuild an older version by checking out that commit in a temporary worktree

That is a better fit for a personal tools repo where the source of truth is this repository history.
