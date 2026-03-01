# denv — minimal direnv for fish

Single-binary, zero-dependency direnv replacement. Fish shell only. Rust. macOS + Linux.

## Architecture

One source file (`src/main.rs`, ~680 lines), one subprocess (bash for `.envrc`/`.env` eval).

Fish integration: `denv hook fish | source` in `config.fish`. The hook fires `denv export fish` on every `PWD` change.

## Commands

| Command | Purpose |
|---|---|
| `denv allow` | Trust nearest `.envrc`, activate immediately |
| `denv deny` | Revoke trust, unload immediately |
| `denv export fish` | Hot path — emit `set -gx`/`set -e` to stdout |
| `denv reload` | Force re-evaluate (bypasses fast path) |
| `denv hook fish` | Print the fish hook function |

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

denv sets two fish variables for prompt integration:

| Variable | Meaning |
|---|---|
| `__DENV_DIR` | Set to the directory path when an `.envrc`/`.env` is found (active or blocked) |
| `__DENV_DIRTY` | Set to `1` when `.envrc` is blocked (needs `denv allow`) |

Both are cleared when leaving a directory with no env files. `__DENV_DIRTY` is cleared on successful activation.

Three prompt states:
```fish
if set -q __DENV_DIRTY
    # blocked, needs re-allow
else if set -q __DENV_DIR
    # active, env loaded
end
```

These are regular fish variables set by `denv export fish | source` — zero cost to check at prompt time.

## `denv export fish` flow

1. Find nearest dir with `.envrc` or `.env` (walk up parents)
2. No env files found → restore previous values from active, clear state vars, done
3. Get mtimes cheaply via `stat`
4. **Fast path 1**: check `__DENV_STATE` env var (zero disk reads) — same dir + same mtimes → return
5. **Fast path 2**: load `active_{PID}` from disk (one read) — same dir + same mtimes → return
6. Active exists → restore all previous values first (handles dir-switch and mtime-change)
7. `.envrc` present + not allowed → set `__DENV_DIR` + `__DENV_DIRTY`, stderr warning, done
8. Parse `.env` (if present) in Rust, eval `.envrc` (if present) via bash with `.env` entries appended as `export` statements
9. Emit `set -gx` / `set -e`, set `__DENV_DIR`, `__DENV_STATE`, clear `__DENV_DIRTY`, save active

The `force` parameter (used by `reload`, `allow`, and `deny`) skips both fast paths.

On activation, a summary line is printed to stderr: `denv: +FOO +BAR`. On deactivation: `denv: -FOO -BAR`. Internal vars (`__DENV_*`) are excluded.

## `__DENV_STATE` fast path

`__DENV_STATE` is a fish variable containing `{envrc_mtime} {dotenv_mtime} {dir}`. It's set on activation and cleared on leave/block. Since it lives in the shell's environment (inherited by child processes), denv can check it without any disk I/O — just `env::var()`.

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

Full eval flow:
```bash
# {DIRENV_STDLIB functions}
env -0 > /tmp/denv_before_{pid}
. /path/to/.envrc          # if .envrc exists
export KEY1='val1'         # .env entries, bash-escaped
export KEY2='val2'
env -0 > /tmp/denv_after_{pid}
```

Parse null-separated `KEY=VALUE` pairs into HashMaps, diff them. Filtered vars: `_`, `SHLVL`, `PWD`, `OLDPWD`, `BASH_EXECUTION_STRING`.

`.env` entries are injected after `.envrc` sourcing so they override. Values are bash single-quote escaped (`'` → `'\''`) to prevent injection.

Error reporting shows stdout when stderr is empty (handles scripts that do `exec 2>&1`).

## Fish hook

```fish
function __denv_export --on-variable PWD
    set -gx __DENV_PID %self
    denv export fish | source
end
function denv --wraps denv
    set -gx __DENV_PID %self
    switch "$argv[1]"
        case allow deny reload
            command denv $argv | source
        case '*'
            command denv $argv
    end
end
set -gx __DENV_PID %self
denv export fish | source
```

`__DENV_PID` is fish's `%self` (shell PID), passed via env var so each shell gets isolated active state.

The `denv` wrapper function intercepts `allow`, `deny`, and `reload` — these commands emit fish `set` commands to stdout, so the wrapper sources their output directly. Other commands (`hook`, `export`) pass through unchanged. This means one subprocess per command, never two.

## Key functions in `src/main.rs`

- `find_env_files(start)` — walk up parents for `.envrc` or `.env`, returns `EnvFiles { dir, envrc, dotenv }`
- `parse_dotenv(path)` — parse `.env` into `Vec<(String, String)>`
- `trust_key(path)` — hex-encode absolute path
- `is_allowed(envrc)` / `cmd_allow(envrc)` / `cmd_deny(envrc)` — trust management (`.envrc` only)
- `DIRENV_STDLIB` — bash function definitions prepended before `.envrc` sourcing
- `eval_env(dir, envrc, dotenv_entries, pid)` — bash subprocess with stdlib + `.envrc` + `.env` exports, returns `EnvDiff`
- `bash_escape(value)` — single-quote escaping for bash export injection
- `load_active(pid)` / `save_active(pid, state)` / `clear_active(pid)` — per-shell state
- `emit_fish_restore(prev)` / `emit_fish_diff(diff)` — fish output
- `print_diff_summary(diff)` / `print_unload_summary(prev)` — `+NAME`/`-NAME` to stderr
- `parse_denv_state(s)` — parse `__DENV_STATE` env var into `(envrc_mtime, dotenv_mtime, dir)`
- `cmd_export_fish(pid, force)` — main export logic with two-tier fast path
- `fish_escape(value)` — single-quote escaping for fish
- `escape_newlines` / `unescape_newlines` — active file serialization

## Testing

Integration tests in `tests/integration.rs` (49 tests). Each test gets an isolated temp dir (project + data dir via `DENV_DATA_DIR`) and a unique fake PID. Tests run the compiled binary as a subprocess.

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
