# denv

Minimal direnv.

direnv runs on every `cd` by nature. It's a larger binary and has a whole Go runtime to initialize. Even if it early exits in the noop case it still costs enough to feel the latency. We solve that by being extremely small and noop exiting as past as possible.


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

On every `cd`, the shell hook runs `denv export <shell>`. denv walks up from the current directory looking for `.envrc` or `.env`. If found and trusted, it spawns one bash subprocess to evaluate the script, diffs the environment before/after, and emits shell-appropriate commands (`set -gx`/`export`/`unset`) for the shell to source.

Per-shell state is tracked by PID so multiple terminals stay independent. When you leave a directory, previous values are restored exactly.

Editing `.envrc` changes its mtime, which invalidates trust until you re-run `denv allow`. This prevents stale or tampered scripts from running silently.

## direnv compat

`.envrc` scripts can use common direnv stdlib functions: `PATH_add`, `path_add`, `has`, `source_env`, `source_up`, `dotenv`, `strict_env`, `log_status`, and others.

`layout*` and some nontrivial or specialized things are not supported.
And of course, direnv-daemon specific stuff are not supported.
