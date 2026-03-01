# denv — minimal direnv for fish

Single-binary, zero-dependency direnv replacement. Fish shell only. Rust. macOS + Linux.

## Architecture

One source file (`src/main.rs`, ~500 lines), one subprocess (bash for `.envrc`/`.env` eval).

Fish integration: `denv hook fish | source` in `config.fish`. The hook fires `denv export fish` on every `PWD` change.

## Commands

| Command | Purpose |
|---|---|
| `denv allow` | Trust nearest `.envrc`, activate immediately |
| `denv deny` | Revoke trust |
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
2. Load `active_{PID}`
3. No env files found → restore previous values from active, clear `__DENV_DIR`/`__DENV_DIRTY`, done
4. Same dir + same mtimes as active → emit nothing (fast path)
5. Active exists → restore all previous values first (handles dir-switch and mtime-change)
6. `.envrc` present + not allowed → set `__DENV_DIR` + `__DENV_DIRTY`, stderr warning, done
7. Parse `.env` (if present) in Rust, eval `.envrc` (if present) via bash with `.env` entries appended as `export` statements
8. Emit `set -gx` / `set -e`, set `__DENV_DIR`, clear `__DENV_DIRTY`, save active

The `force` parameter (used by `reload` and `allow`) skips the fast path in step 4.

## Bash eval

```bash
env -0 > /tmp/denv_before_{pid}
. /path/to/.envrc          # if .envrc exists
export KEY1='val1'         # .env entries, bash-escaped
export KEY2='val2'
env -0 > /tmp/denv_after_{pid}
```

Parse null-separated `KEY=VALUE` pairs into HashMaps, diff them. Filtered vars: `_`, `SHLVL`, `PWD`, `OLDPWD`, `BASH_EXECUTION_STRING`.

`.env` entries are injected after `.envrc` sourcing so they override. Values are bash single-quote escaped (`'` → `'\''`) to prevent injection.

## Fish hook

```fish
function __denv_export --on-variable PWD
    set -gx __DENV_PID %self
    denv export fish | source
end
set -gx __DENV_PID %self
denv export fish | source
```

`__DENV_PID` is fish's `%self` (shell PID), passed via env var so each shell gets isolated active state.

## Key functions in `src/main.rs`

- `find_env_files(start)` — walk up parents for `.envrc` or `.env`, returns `EnvFiles { dir, envrc, dotenv }`
- `parse_dotenv(path)` — parse `.env` into `Vec<(String, String)>`
- `trust_key(path)` — hex-encode absolute path
- `is_allowed(envrc)` / `cmd_allow(envrc)` / `cmd_deny(envrc)` — trust management (`.envrc` only)
- `eval_env(envrc, dotenv_entries, pid)` — bash subprocess with `.envrc` + `.env` exports, returns `EnvDiff`
- `bash_escape(value)` — single-quote escaping for bash export injection
- `load_active(pid)` / `save_active(pid, state)` / `clear_active(pid)` — per-shell state
- `emit_fish_restore(prev)` / `emit_fish_diff(diff)` — fish output
- `cmd_export_fish(pid, force)` — main export logic
- `fish_escape(value)` — single-quote escaping for fish
- `escape_newlines` / `unescape_newlines` — active file serialization

## Testing

Integration tests in `tests/integration.rs` (35 tests). Each test gets an isolated temp dir (project + data dir via `DENV_DATA_DIR`) and a unique fake PID. Tests run the compiled binary as a subprocess.

Run: `cargo test`

Test categories:
- Core flow: allow, export, deny, leave, reload
- Trust: blocked without allow, deny revokes, mtime invalidation
- Fast path: no output on same mtime
- `.env`: standalone, combined with `.envrc`, override precedence, comments/quotes, change detection
- Prompt indicators: `__DENV_DIR` set/cleared, `__DENV_DIRTY` on block/allow
- Edge cases: parent directory walk, PATH manipulation, script errors, unknown commands

## Build

```
just build       # debug build
just release     # optimized nightly build with build-std
just install     # release + copy to ~/.local/bin
```
