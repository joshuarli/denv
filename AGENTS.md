# denv — minimal direnv

Single-binary, zero-dependency direnv replacement. Fish, Bash, Zsh. Rust. macOS + Linux.

## Architecture

One source file (`src/main.rs`, ~780 lines), one subprocess (bash for `.envrc`/`.env` eval).

Shell integration: `denv hook <fish|bash|zsh>` prints the appropriate hook for each shell. Each hook sets `__DENV_SHELL` and `__DENV_PID`, then calls `denv export <shell>` on directory changes.

## Commands

| Command | Purpose |
|---|---|
| `denv allow` | Trust nearest `.envrc`, activate immediately |
| `denv deny` | Revoke trust, unload immediately |
| `denv export <fish\|bash\|zsh>` | Hot path — emit shell-specific set/export/unset to stdout |
| `denv reload` | Force re-evaluate (bypasses fast path) |
| `denv hook <fish\|bash\|zsh>` | Print the hook for the given shell |

## File discovery

`find_env_files(start)` walks up parent directories, stopping at the first directory containing `.envrc` or `.env` (or both). Returns an `EnvFiles { dir, envrc, dotenv }` struct.

## `.envrc` vs `.env`

| | `.envrc` | `.env` |
|---|---|---|
| Format | Bash script | `KEY=VALUE` lines |
| Execution | Sourced by bash subprocess | Parsed in Rust, injected as `export` statements |
| Trust | Requires `denv allow` | No trust needed (data, not code) |
| Alone | Requires allow | Loads automatically |
| Together | `.env` sourced after `.envrc` — `.env` wins on conflicts |

`.env` format supports: `KEY=VALUE`, `export KEY=VALUE`, `"double quoted"`, `'single quoted'`, `# comments`, blank lines.

## State layout (`~/.local/share/denv/`, overridable via `DENV_DATA_DIR`)

```
allow/{key}         # trust file. content = mtime as decimal string
active_{pid}        # per-shell state: loaded dir + previous values for restore
```

**Trust key**: hex-encoded absolute path of `.envrc` (no hashing, no collisions).

**Active file format**:
```
/absolute/path/to/dir
{envrc_mtime} {dotenv_mtime}
KEY=previous_value
KEY
```
Line 1: directory path. Line 2: space-separated mtimes (0 if file absent). Remaining: one per modified var. `KEY=val` = restore on unload, bare `KEY` = unset on unload. Newlines escaped as `\n`, backslashes as `\\`.

## Trust model

- `denv allow` stores `.envrc`'s current mtime in `allow/{key}`
- `is_allowed` checks: trust file exists AND stored mtime == current mtime
- Editing `.envrc` changes mtime → blocks until re-allowed
- `.env` files never require trust (parsed by denv, not executed as code)

## Prompt indicator variables

denv sets two environment variables for prompt integration:

| Variable | Meaning |
|---|---|
| `__DENV_DIR` | Set to the directory path when an `.envrc`/`.env` is found (active or blocked) |
| `__DENV_DIRTY` | Set to `1` when `.envrc` is blocked (needs `denv allow`) |

Both are cleared when leaving a directory with no env files. `__DENV_DIRTY` is cleared on successful activation.

Three prompt states (fish example):
```fish
if set -q __DENV_DIRTY
    # blocked, needs re-allow
else if set -q __DENV_DIR
    # active, env loaded
end
```

Bash/zsh equivalent: check `[ -n "$__DENV_DIRTY" ]` / `[ -n "$__DENV_DIR" ]`.

## `denv export <shell>` flow

1. Find nearest dir with `.envrc` or `.env` (walk up parents)
2. No env files found → restore previous values from active, clear state vars, done
3. Get mtimes cheaply via `stat`
4. **Fast path 1**: check `__DENV_STATE` env var (zero disk reads) — same dir + same mtimes → return
5. **Fast path 2**: load `active_{PID}` from disk (one read) — same dir + same mtimes → return
6. Active exists → restore all previous values first (handles dir-switch and mtime-change)
7. `.envrc` present + not allowed → set `__DENV_DIR` + `__DENV_DIRTY`, stderr warning, done
8. Parse `.env` (if present) in Rust, eval `.envrc` (if present) via bash with `.env` entries appended as `export` statements
9. Emit shell-appropriate set/export/unset commands, set `__DENV_DIR`, `__DENV_STATE`, clear `__DENV_DIRTY`, save active

The `Shell` enum (`Fish`, `Bash`, `Zsh`) dispatches output syntax at each call site. Fish uses `set -gx`/`set -e`. Bash and Zsh share POSIX syntax (`export K='V'`/`unset K`).

The `force` parameter (used by `reload`, `allow`, and `deny`) skips both fast paths.

On activation, a summary line is printed to stderr: `denv: +FOO +BAR`. On deactivation: `denv: -FOO -BAR`. Internal vars (`__DENV_*`) are excluded.

## `__DENV_STATE` fast path

`__DENV_STATE` is an environment variable containing `{envrc_mtime} {dotenv_mtime} {dir}`. It's set on activation and cleared on leave/block. Since it lives in the shell's environment (inherited by child processes), denv can check it without any disk I/O — just `env::var()`.

Fast path 2 (active file) is the fallback for the first `cd` after shell startup, before `__DENV_STATE` has been set by the hook. After that, the env var handles all subsequent checks.

Hot path cost (common case — same dir, nothing changed):
- `getcwd` + `stat` walk for `.envrc`/`.env` + `stat` for mtimes + env var compare → return
- **Zero file reads, zero file opens**

## Bash eval

A direnv stdlib compatibility layer (`DIRENV_STDLIB` const) is prepended before sourcing `.envrc`:

```bash
# stdlib functions available to .envrc scripts:
PATH_add()              # add dirs to PATH (relative paths resolved from .envrc dir)
path_add()              # add dirs to arbitrary path var (e.g. path_add PYTHONPATH .)
has()                   # check if command exists
watch_file()            # no-op (direnv compat)
source_env()            # source another file
source_env_if_exists()  # source another file if it exists
source_up()             # source .envrc from parent directory
source_up_if_exists()   # source_up, no error if missing
dotenv()                # source .env file with auto-export
dotenv_if_exists()      # dotenv, no error if missing
log_status()            # print status to stderr
log_error()             # print error to stderr
strict_env()            # set -euo pipefail
unstrict_env()          # set +euo pipefail
```

The bash subprocess runs with `current_dir` set to the `.envrc`'s directory, so relative paths in `PATH_add` resolve correctly.

Both stdout and stderr of the bash subprocess are duped from denv's stderr (the terminal), so `.envrc` output streams in real time. denv's own stdout is reserved for fish commands (`set -gx`/`set -e`), which the fish wrapper sources. The `env -0 > file` commands use explicit file redirects, unaffected by fd 1.

Full eval flow:
```bash
# {DIRENV_STDLIB functions}
env -0 > /tmp/denv_before_{pid}
. /path/to/.envrc          # if .envrc exists; output streams to terminal
export KEY1='val1'         # .env entries, bash-escaped
export KEY2='val2'
env -0 > /tmp/denv_after_{pid}
```

Parse null-separated `KEY=VALUE` pairs into HashMaps, diff them. Filtered vars: `_`, `SHLVL`, `PWD`, `OLDPWD`, `BASH_EXECUTION_STRING`.

`.env` entries are injected after `.envrc` sourcing so they override. Values are bash single-quote escaped (`'` → `'\''`) to prevent injection.

## Shell hooks

Each shell hook sets `__DENV_PID` (shell PID for isolated state) and `__DENV_SHELL` (tells `allow`/`deny`/`reload` which syntax to emit), then triggers `denv export <shell>` on directory changes.

**Fish** — triggers on `PWD` variable change:
```fish
function __denv_export --on-variable PWD
    set -gx __DENV_PID %self
    denv export fish | source
end
function denv --wraps denv
    ...
end
set -gx __DENV_PID %self
set -gx __DENV_SHELL fish
denv export fish | source
```

**Bash** — uses `PROMPT_COMMAND`:
```bash
__denv_export() { eval "$(command denv export bash)"; }
denv() {
    case "$1" in
        allow|deny|reload) eval "$(command denv "$@")" ;;
        *) command denv "$@" ;;
    esac
}
export __DENV_PID=$$
export __DENV_SHELL=bash
PROMPT_COMMAND="__denv_export${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
eval "$(command denv export bash)"
```

**Zsh** — uses `precmd_functions` via `add-zsh-hook`:
```zsh
__denv_export() { eval "$(command denv export zsh)"; }
denv() { ... }  # same wrapper as bash
export __DENV_PID=$$
export __DENV_SHELL=zsh
autoload -Uz add-zsh-hook
add-zsh-hook precmd __denv_export
eval "$(command denv export zsh)"
```

The `denv` wrapper function intercepts `allow`, `deny`, and `reload` — these commands emit shell commands to stdout, so the wrapper sources/evals their output directly. Other commands (`hook`, `export`) pass through unchanged.

## Key functions in `src/main.rs`

- `find_env_files(start)` — walk up parents for `.envrc` or `.env`, returns `EnvFiles { dir, envrc, dotenv }`
- `parse_dotenv(path)` — parse `.env` into `Vec<(String, String)>`
- `trust_key(path)` — hex-encode absolute path
- `is_allowed(envrc)` / `cmd_allow(envrc)` / `cmd_deny(envrc)` — trust management (`.envrc` only)
- `DIRENV_STDLIB` — bash function definitions prepended before `.envrc` sourcing
- `eval_env(dir, envrc, dotenv_entries, pid)` — bash subprocess with stdlib + `.envrc` + `.env` exports, returns `EnvDiff`
- `bash_escape(value)` — single-quote escaping for bash export injection
- `load_active(pid)` / `save_active(pid, state)` / `clear_active(pid)` — per-shell state
- `Shell` enum — `Fish`, `Bash`, `Zsh`; dispatches output syntax
- `write_shell_escaped(w, shell, value)` — single-quote escaping (fish: `\'`, bash/zsh: `'\''`)
- `emit_export(w, shell, key, value)` / `emit_unset(w, shell, key)` — shell-specific set/unset
- `emit_restore(prev, shell, out)` / `emit_diff(diff, shell, out)` — batch output
- `parse_denv_state(s)` — parse `__DENV_STATE` env var into `(envrc_mtime, dotenv_mtime, dir)`
- `cmd_export(pid, force, shell)` — main export logic with two-tier fast path
- `escape_newlines` / `unescape_newlines` — active file serialization

## Testing

Integration tests in `tests/integration.rs` (~63 tests). Each test gets an isolated temp dir (project + data dir via `DENV_DATA_DIR`) and a unique fake PID. Tests run the compiled binary as a subprocess. The test infra defaults `__DENV_SHELL=fish` so existing fish tests work unchanged.

Run: `cargo test`

Test categories:
- Core flow: allow, export, deny, leave, reload
- Trust: blocked without allow, deny revokes, mtime invalidation
- Fast path: env var fast path, active file fallback, mtime change detection
- `.env`: standalone, combined with `.envrc`, override precedence, comments/quotes, change detection
- Prompt indicators: `__DENV_DIR` set/cleared, `__DENV_DIRTY` on block/allow
- Direnv compat: PATH_add, source_env, dotenv, strict_env
- Summary: +NAME/-NAME printed on activate/deactivate/deny
- Edge cases: parent directory walk, PATH manipulation, script errors, unknown commands

## Build

```
just build       # debug build
just release     # optimized nightly build with build-std
just install     # release + copy to ~/.local/bin
```
