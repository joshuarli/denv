# denv

A fast, minimal [direnv](https://direnv.net). Zero dependencies, single static Rust binary (378K stripped), direnv-compatible `.envrc` scripts.

direnv runs on every `cd`. It's an 8MB Go binary that has to initialize its runtime even when nothing changed. denv solves this by being small, doing as little work as possible on the noop path, and avoiding the subprocess entirely as much as possible in the hook.


## Install

Download to `~/.local/bin/denv` and add the hook for your shell:

**Fish** (`~/.config/fish/config.fish`):
```fish
denv hook fish | source
```

**Bash** (`~/.bashrc`):
```bash
eval "$(denv hook bash)"
```

**Zsh** (`~/.zshrc`):
```zsh
eval "$(denv hook zsh)"
```


## Usage

```
cd myproject
echo 'export SECRET=hunter2' > .envrc
denv allow    # trust and activate
# denv: +SECRET

cd ..
# denv: -SECRET
```

`.env` files load automatically (no trust needed). `.envrc` files are bash scripts and require `denv allow` since they execute code.

When both exist, `.env` is loaded after `.envrc` — `.env` wins on conflicts.

## Commands

| Command | Purpose |
|---|---|
| `denv allow` | Trust `.envrc`, activate |
| `denv deny` | Revoke trust, unload |
| `denv reload` | Force re-evaluate |

## How it works

On `cd`, the shell hook runs `denv export <shell>`. denv walks up from the current directory looking for `.envrc` or `.env`. If found and trusted, it spawns one bash subprocess to evaluate the script, diffs the environment before/after, and emits shell-appropriate commands (`set -gx`/`export`/`unset`) for the shell to source.

Per-shell state is tracked by PID so multiple terminals stay independent. When you leave a directory, previous values are restored exactly.

Editing `.envrc` changes its mtime, which invalidates trust until you re-run `denv allow`. This prevents stale or tampered scripts from running silently.

### Noop fast path

The common case — cd'ing within a project where nothing changed — is heavily optimized:

- **All shells**: The hook checks `__DENV_STATE` and uses `test -nt` against a sentinel file to detect edits and deletions. If the directory matches and no files changed, it returns immediately — **zero subprocesses, zero forks.** direnv spawns the full binary unconditionally on every prompt.
- **Binary** (when the subprocess *is* needed): 378K stripped Rust with fat LTO, `panic = "abort"`, zero runtime dependencies. direnv is 8MB of Go.

## direnv compat

`.envrc` scripts can use common direnv stdlib functions: `PATH_add`, `path_add`, `has`, `source_env`, `source_up`, `dotenv`, `strict_env`, `log_status`, and others.

`layout*` and some nontrivial or specialized things are not supported.
And of course, direnv-daemon specific stuff are not supported.
