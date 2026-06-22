# on9au's amazing cool tools

This repository contains various tools that I have created for my own use. Originally meant to be for private use, until i recommended one of my tools to someone lmao

## Workspace overview

This workspace is split into tool areas under `crates/`:

The top-level README is only a map of the workspace. Tool-specific usage and flags should live in each tool area's own README.

## Install

`toolctl` is the main entrypoint for this repository, and can be used to install other tools in this repository.

Run the following command to install `toolctl`:

```sh
cargo install --git https://github.com/on9au/tools.git toolctl
```

## Start with toolctl

`toolctl` is the main end-user entrypoint for this repository.

Use it to sync this repository locally, discover installable tools, inspect recent commit-based versions, and install managed binaries.

Common commands:

```sh
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
- [crates/git/README.md](crates/git/README.md): git utilities, currently `cdgit`

## Build

Build the workspace:

```sh
cargo build
```

## Version model

The version model is repo-native rather than semver-heavy:

- available versions come from git commits affecting the tool's package directory
- installs record the exact commit hash and commit date
- `toolctl` can rebuild an older version by checking out that commit in a temporary worktree
