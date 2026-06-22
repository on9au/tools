# git

Git-related utilities

## `cdgit`

Fuzzy-find a git repository under a configured root and print its path, so you
can quickly jump to any repo from the shell.

### Layout & config

`cdgit` expects repositories to be laid out as `<root>/<owner>/<repo>`, e.g.
`C:\git\GitHub\on9au\tools`. Point it at the root via the `CDGIT_ROOT`
environment variable (or `--root`):

```sh
# PowerShell
$env:CDGIT_ROOT = "C:\git\GitHub"
# bash/zsh
export CDGIT_ROOT="$HOME/git/GitHub"
```

### The `cg` shell command

A child process can't change its parent shell's directory, so `cdgit` only
*prints* the chosen path. Install the shell wrapper once to get a `cg` command
that actually `cd`s:

```powershell
# PowerShell ($PROFILE)
Invoke-Expression (cdgit init powershell | Out-String)
```

```bash
# bash/zsh (~/.bashrc, ~/.zshrc)
eval "$(cdgit init bash)"
```

```fish
# fish (~/.config/fish/config.fish)
cdgit init fish | source
```

### Usage

```sh
cg                 # interactive picker over every repo
cg tools           # jump straight there if the query is unambiguous, else pick
cdgit tools --list # print matching repos as a table (no cd)
cdgit init bash    # print the shell integration snippet
```

The interactive picker renders to stderr, so stdout only ever carries the
selected path — safe to capture in `cd "$(cdgit ...)"`.
