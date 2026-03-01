# denv

Minimal direnv for fish shell.

## Install

Download to `~/.local/bin/denv` and:

```
echo 'denv hook fish | source' >> ~/.config/fish/config.fish
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

On every `cd`, fish runs `denv export fish | source`. denv walks up from the current directory looking for `.envrc` or `.env`. If found and trusted, it spawns one bash subprocess to evaluate the script, diffs the environment before/after, and emits `set -gx`/`set -e` commands for fish to source.

Per-shell state is tracked by PID so multiple terminals stay independent. When you leave a directory, previous values are restored exactly.

Editing `.envrc` changes its mtime, which invalidates trust until you re-run `denv allow`. This prevents stale or tampered scripts from running silently.

## direnv compat

`.envrc` scripts can use common direnv stdlib functions: `PATH_add`, `path_add`, `has`, `source_env`, `source_up`, `dotenv`, `strict_env`, `log_status`, and others.
