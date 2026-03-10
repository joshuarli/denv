use std::borrow::Cow;
use std::cmp::Ordering;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// --- Shell ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shell {
    Fish,
    Bash,
    Zsh,
}

impl Shell {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "fish" => Some(Self::Fish),
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            _ => None,
        }
    }
}

// --- Find env files ---

struct EnvFiles {
    dir: PathBuf,
    envrc: Option<(PathBuf, u64)>,
    dotenv: Option<(PathBuf, u64)>,
}

fn find_env_files(start: &Path) -> Option<EnvFiles> {
    let mut buf = start.to_path_buf();
    loop {
        buf.push(".envrc");
        let envrc_meta = fs::metadata(&buf).ok().filter(|m| m.is_file());
        let envrc = envrc_meta.as_ref().map(|_| buf.clone());
        buf.pop();

        buf.push(".env");
        let dotenv_meta = fs::metadata(&buf).ok().filter(|m| m.is_file());
        let dotenv = dotenv_meta.as_ref().map(|_| buf.clone());
        buf.pop();

        if envrc_meta.is_some() || dotenv_meta.is_some() {
            return Some(EnvFiles {
                dir: buf,
                envrc: envrc_meta.map(|m| {
                    (
                        envrc.expect("envrc set when envrc_meta set"),
                        m.mtime() as u64,
                    )
                }),
                dotenv: dotenv_meta.map(|m| {
                    (
                        dotenv.expect("dotenv set when dotenv_meta set"),
                        m.mtime() as u64,
                    )
                }),
            });
        }
        if !buf.pop() {
            return None;
        }
    }
}

// --- Trust ---

fn trust_key(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn data_dir() -> Result<PathBuf, String> {
    if let Some(d) = env::var_os("DENV_DATA_DIR") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(d).join("denv"));
    }
    let home = env::var_os("HOME").ok_or("HOME, XDG_DATA_HOME, and DENV_DATA_DIR are all unset")?;
    Ok(PathBuf::from(home).join(".local/share/denv"))
}

fn allow_dir() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("allow"))
}

fn mtime_of(path: &Path) -> io::Result<u64> {
    Ok(path.metadata()?.mtime() as u64)
}

fn is_allowed(envrc: &Path) -> bool {
    let key = trust_key(envrc);
    let trust_file = match allow_dir() {
        Ok(d) => d.join(&key),
        Err(_) => return false,
    };
    let stored = match fs::read_to_string(&trust_file) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let current = match mtime_of(envrc) {
        Ok(m) => m,
        Err(_) => return false,
    };
    stored.trim().parse::<u64>() == Ok(current)
}

fn cmd_allow(envrc: &Path) -> Result<(), String> {
    let key = trust_key(envrc);
    let dir = allow_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create allow dir: {e}"))?;
    let mtime = mtime_of(envrc).map_err(|e| format!("failed to read .envrc mtime: {e}"))?;
    fs::write(dir.join(&key), mtime.to_string())
        .map_err(|e| format!("failed to write trust file: {e}"))?;
    eprintln!("denv: allowed {}", envrc.display());
    Ok(())
}

fn cmd_deny(envrc: &Path) -> Result<(), String> {
    let key = trust_key(envrc);
    let trust_file = allow_dir()?.join(&key);
    match fs::remove_file(&trust_file) {
        Ok(_) => eprintln!("denv: denied {}", envrc.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("denv: not currently allowed");
        }
        Err(e) => return Err(format!("failed to remove trust file: {e}")),
    }
    Ok(())
}

// --- Active state ---

struct ActiveState {
    dir: PathBuf,
    envrc_mtime: u64,
    dotenv_mtime: u64,
    prev: Vec<PrevVar>,
}

enum PrevVar {
    Restore(String, String),
    Unset(String),
}

fn escape_newlines(s: &str) -> Cow<'_, str> {
    if !s.as_bytes().iter().any(|&b| b == b'\\' || b == b'\n') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'\\' => {
                out.push_str(&s[start..i]);
                out.push_str("\\\\");
                start = i + 1;
            }
            b'\n' => {
                out.push_str(&s[start..i]);
                out.push_str("\\n");
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push_str(&s[start..]);
    Cow::Owned(out)
}

fn unescape_newlines(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

fn active_path(pid: &str) -> Result<PathBuf, String> {
    Ok(data_dir()?.join(format!("active_{pid}")))
}

fn load_active(pid: &str) -> Option<ActiveState> {
    let content = fs::read_to_string(active_path(pid).ok()?).ok()?;
    let mut lines = content.lines();
    let dir = PathBuf::from(lines.next()?);
    let mtimes = lines.next()?;
    let (envrc_mtime, dotenv_mtime) = mtimes.split_once(' ')?;
    let envrc_mtime: u64 = envrc_mtime.parse().ok()?;
    let dotenv_mtime: u64 = dotenv_mtime.parse().ok()?;
    let mut prev = Vec::new();
    for line in lines {
        if let Some(eq) = line.find('=') {
            let key = &line[..eq];
            let val = unescape_newlines(&line[eq + 1..]);
            prev.push(PrevVar::Restore(key.to_string(), val.into_owned()));
        } else if !line.is_empty() {
            prev.push(PrevVar::Unset(line.to_string()));
        }
    }
    Some(ActiveState {
        dir,
        envrc_mtime,
        dotenv_mtime,
        prev,
    })
}

fn save_active(pid: &str, state: &ActiveState) -> Result<(), String> {
    let dir = data_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create data dir: {e}"))?;
    let mut buf = String::new();
    buf.push_str(&state.dir.to_string_lossy());
    buf.push('\n');
    // write! to String is infallible
    write!(buf, "{} {}", state.envrc_mtime, state.dotenv_mtime).unwrap();
    buf.push('\n');
    for pv in &state.prev {
        match pv {
            PrevVar::Restore(k, v) => {
                buf.push_str(k);
                buf.push('=');
                buf.push_str(&escape_newlines(v));
                buf.push('\n');
            }
            PrevVar::Unset(k) => {
                buf.push_str(k);
                buf.push('\n');
            }
        }
    }
    fs::write(dir.join(format!("active_{pid}")), buf)
        .map_err(|e| format!("failed to write active file: {e}"))?;
    Ok(())
}

fn clear_active(pid: &str) {
    if let Ok(path) = active_path(pid) {
        let _ = fs::remove_file(path);
    }
}

// --- .env parser ---

fn parse_dotenv(content: &str) -> Vec<(&str, Cow<'_, str>)> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = line[..eq].trim();
        let val = line[eq + 1..].trim();
        let val: Cow<'_, str> = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
            let inner = &val[1..val.len() - 1];
            if inner.contains('\\') {
                let mut out = String::with_capacity(inner.len());
                let mut chars = inner.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        match chars.next() {
                            Some('n') => out.push('\n'),
                            Some('t') => out.push('\t'),
                            Some('\\') => out.push('\\'),
                            Some('"') => out.push('"'),
                            Some('$') => out.push('$'),
                            Some(other) => {
                                out.push('\\');
                                out.push(other);
                            }
                            None => out.push('\\'),
                        }
                    } else {
                        out.push(c);
                    }
                }
                Cow::Owned(out)
            } else {
                Cow::Borrowed(inner)
            }
        } else if val.len() >= 2 && val.starts_with('\'') && val.ends_with('\'') {
            Cow::Borrowed(&val[1..val.len() - 1])
        } else {
            Cow::Borrowed(val)
        };
        if !key.is_empty() {
            entries.push((key, val));
        }
    }
    entries
}

// --- Bash eval ---

fn parse_env_null(data: &[u8]) -> Vec<(&str, &str)> {
    let mut entries = Vec::new();
    for entry in data.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(entry) else {
            continue;
        };
        if let Some(eq) = s.find('=') {
            let key = &s[..eq];
            if !matches!(
                key,
                "_" | "SHLVL" | "PWD" | "OLDPWD" | "BASH_EXECUTION_STRING"
            ) {
                entries.push((key, &s[eq + 1..]));
            }
        }
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    entries
}

fn push_bash_escaped(out: &mut String, value: &str) {
    out.push('\'');
    let bytes = value.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\'' {
            out.push_str(&value[start..i]);
            out.push_str("'\\''");
            start = i + 1;
        }
    }
    out.push_str(&value[start..]);
    out.push('\'');
}

struct EnvDiff {
    set: Vec<(String, String)>,
    unset: Vec<String>,
}

// direnv stdlib compat — prepended before sourcing .envrc
const DIRENV_STDLIB: &str = r#"
PATH_add() {
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    export PATH="$p:$PATH"
  done
}
path_add() {
  local var="$1"; shift
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    eval "export $var=\"$p:\${$var}\""
  done
}
PATH_rm() {
  local new_path p pattern
  for pattern in "$@"; do
    new_path=
    local IFS=:
    for p in $PATH; do
      case "$p" in
        $pattern) ;;
        *) new_path="${new_path:+$new_path:}$p" ;;
      esac
    done
    export PATH="$new_path"
  done
}
path_rm() {
  local var="$1"; shift
  local new_path p pattern val
  for pattern in "$@"; do
    eval "val=\$$var"
    new_path=
    local IFS=:
    for p in $val; do
      case "$p" in
        $pattern) ;;
        *) new_path="${new_path:+$new_path:}$p" ;;
      esac
    done
    eval "export $var=\"\$new_path\""
  done
}
MANPATH_add() {
  local p
  for p in "$@"; do
    [ "${p#/}" = "$p" ] && p="$PWD/$p"
    export MANPATH="$p${MANPATH:+:$MANPATH}"
  done
}
has() { command -v "$1" >/dev/null 2>&1; }
watch_file() { :; }
watch_dir() { :; }
expand_path() {
  case "$1" in
    ~/*) echo "$HOME/${1#~/}" ;;
    /*)  echo "$1" ;;
    *)   echo "$PWD/$1" ;;
  esac
}
find_up() {
  local file="$1" dir="$PWD"
  while [ "$dir" != "/" ]; do
    if [ -e "$dir/$file" ]; then echo "$dir/$file"; return 0; fi
    dir="${dir%/*}"
    [ -z "$dir" ] && dir="/"
  done
  return 1
}
env_vars_required() {
  local var _v rc=0
  for var in "$@"; do
    eval "_v=\"\${$var-}\""
    [ -n "$_v" ] || { log_error "$var is required"; rc=1; }
  done
  unset _v
  return $rc
}
load_prefix() {
  local p="${1%/}"
  [ "${p#/}" = "$p" ] && p="$PWD/$p"
  PATH_add "$p/bin" "$p/sbin"
  [ -d "$p/include" ]    && path_add CPATH "$p/include"
  [ -d "$p/lib" ]        && path_add PKG_CONFIG_PATH "$p/lib/pkgconfig" \
                         && path_add LIBRARY_PATH "$p/lib" \
                         && path_add DYLD_LIBRARY_PATH "$p/lib" \
                         && path_add LD_LIBRARY_PATH "$p/lib"
  [ -d "$p/lib64" ]      && path_add PKG_CONFIG_PATH "$p/lib64/pkgconfig" \
                         && path_add LIBRARY_PATH "$p/lib64" \
                         && path_add DYLD_LIBRARY_PATH "$p/lib64" \
                         && path_add LD_LIBRARY_PATH "$p/lib64"
  [ -d "$p/share/man" ]  && MANPATH_add "$p/share/man"
  return 0
}
source_env() { [ -f "$1" ] && . "$1"; }
source_env_if_exists() { [ -f "$1" ] && . "$1" || :; }
source_up() {
  local d="$PWD"
  while d="$(dirname "$d")" && [ "$d" != "/" ]; do
    if [ -f "$d/.envrc" ]; then . "$d/.envrc"; return; fi
  done
}
source_up_if_exists() { source_up 2>/dev/null || :; }
dotenv() {
  local f="${1:-.env}"
  [ -f "$f" ] || return 1
  set -a; . "$f"; set +a
}
dotenv_if_exists() { dotenv "${1:-.env}" 2>/dev/null || :; }
log_status() { echo "denv: $*" >&2; }
log_error() { echo "denv: error: $*" >&2; }
strict_env() { set -euo pipefail; }
unstrict_env() { set +euo pipefail; }
"#;

fn diff_sorted_env(before: &[(&str, &str)], after: &[(&str, &str)]) -> EnvDiff {
    let (mut bi, mut ai) = (0, 0);
    let mut set = Vec::new();
    let mut unset = Vec::new();
    while bi < before.len() && ai < after.len() {
        match before[bi].0.cmp(after[ai].0) {
            Ordering::Less => {
                unset.push(before[bi].0.to_owned());
                bi += 1;
            }
            Ordering::Greater => {
                set.push((after[ai].0.to_owned(), after[ai].1.to_owned()));
                ai += 1;
            }
            Ordering::Equal => {
                if before[bi].1 != after[ai].1 {
                    set.push((after[ai].0.to_owned(), after[ai].1.to_owned()));
                }
                bi += 1;
                ai += 1;
            }
        }
    }
    for b in &before[bi..] {
        unset.push(b.0.to_owned());
    }
    for a in &after[ai..] {
        set.push((a.0.to_owned(), a.1.to_owned()));
    }
    EnvDiff { set, unset }
}

fn eval_env(
    dir: &Path,
    envrc: Option<&Path>,
    dotenv_entries: &[(&str, Cow<'_, str>)],
    pid: &str,
) -> Result<EnvDiff, String> {
    let data = data_dir()?;
    fs::create_dir_all(&data).map_err(|e| format!("create data dir: {e}"))?;
    let before_path = format!("{}/before_{pid}", data.display());
    let after_path = format!("{}/after_{pid}", data.display());

    let mut script = String::with_capacity(DIRENV_STDLIB.len() + 256);
    script.push_str(DIRENV_STDLIB);
    writeln!(script, "env -0 > '{}'", before_path).unwrap();
    if let Some(envrc) = envrc {
        writeln!(script, ". '{}'", envrc.display()).unwrap();
    }
    for (k, v) in dotenv_entries {
        write!(script, "export {}=", k).unwrap();
        push_bash_escaped(&mut script, v);
        script.push('\n');
    }
    write!(script, "env -0 > '{}'", after_path).unwrap();

    // Dup stderr as bash's stdout so .envrc output streams to terminal.
    // Our stdout may be a pipe (fish sources it), so we can't inherit it.
    // env -0 writes to files via explicit redirects, unaffected by fd 1.
    let stderr_dup = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("dup stderr: {e}"))?;

    let status = Command::new("bash")
        .arg("-e")
        .arg("-c")
        .arg(&script)
        .current_dir(dir)
        .stdout(stderr_dup)
        .stderr(
            io::stderr()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|e| format!("dup stderr for child: {e}"))?,
        )
        .status()
        .map_err(|e| format!("failed to run bash: {e}"))?;

    if !status.success() {
        let _ = fs::remove_file(&before_path);
        let _ = fs::remove_file(&after_path);
        return Err(".envrc evaluation failed".to_string());
    }

    let before_data = fs::read(&before_path).map_err(|e| format!("read before env: {e}"))?;
    let after_data = fs::read(&after_path).map_err(|e| format!("read after env: {e}"))?;
    let _ = fs::remove_file(&before_path);
    let _ = fs::remove_file(&after_path);

    let before = parse_env_null(&before_data);
    let after = parse_env_null(&after_data);

    Ok(diff_sorted_env(&before, &after))
}

// --- Shell output ---

fn write_shell_escaped(w: &mut impl Write, shell: Shell, value: &str) {
    let _ = w.write_all(b"'");
    let bytes = value.as_bytes();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\'' {
            let _ = w.write_all(&bytes[start..i]);
            match shell {
                Shell::Fish => {
                    let _ = w.write_all(b"\\'");
                }
                Shell::Bash | Shell::Zsh => {
                    let _ = w.write_all(b"'\\''");
                }
            }
            start = i + 1;
        }
    }
    let _ = w.write_all(&bytes[start..]);
    let _ = w.write_all(b"'");
}

fn emit_export(w: &mut impl Write, shell: Shell, key: &str, value: &str) {
    match shell {
        Shell::Fish => {
            let _ = write!(w, "set -gx {key} ");
            write_shell_escaped(w, shell, value);
            let _ = writeln!(w, ";");
        }
        Shell::Bash | Shell::Zsh => {
            let _ = write!(w, "export {key}=");
            write_shell_escaped(w, shell, value);
            let _ = writeln!(w, ";");
        }
    }
}

fn emit_unset(w: &mut impl Write, shell: Shell, key: &str) {
    match shell {
        Shell::Fish => {
            let _ = writeln!(w, "set -e {key};");
        }
        Shell::Bash | Shell::Zsh => {
            let _ = writeln!(w, "unset {key};");
        }
    }
}

fn emit_restore(prev: &[PrevVar], shell: Shell, out: &mut impl Write) {
    for pv in prev {
        match pv {
            PrevVar::Restore(k, v) => emit_export(out, shell, k, v),
            PrevVar::Unset(k) => emit_unset(out, shell, k),
        }
    }
}

fn emit_diff(diff: &EnvDiff, shell: Shell, out: &mut impl Write) {
    for (k, v) in &diff.set {
        emit_export(out, shell, k, v);
    }
    for k in &diff.unset {
        emit_unset(out, shell, k);
    }
}

// --- Summary ---

fn print_summary<'a>(items: impl Iterator<Item = (char, &'a str)>) {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let mut first = true;
    for (sign, k) in items {
        if k.starts_with("__DENV_") {
            continue;
        }
        if first {
            let _ = write!(err, "denv: {sign}{k}");
            first = false;
        } else {
            let _ = write!(err, " {sign}{k}");
        }
    }
    if !first {
        let _ = writeln!(err);
    }
}

// --- Export command ---

fn parse_denv_state(s: &str) -> Option<(u64, u64, &str)> {
    let (envrc_str, rest) = s.split_once(' ')?;
    let (dotenv_str, dir) = rest.split_once(' ')?;
    Some((envrc_str.parse().ok()?, dotenv_str.parse().ok()?, dir))
}

fn cmd_export(pid: &str, force: bool, shell: Shell) -> Result<(), String> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Use $PWD from the environment (zero syscalls) instead of getcwd(2)
    // which does open(".") + fcntl(F_GETPATH) + close on macOS (~1ms).
    // The shell always sets PWD before invoking denv.
    let cwd = env::var_os("PWD")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().expect("cannot get cwd"));
    let found = find_env_files(&cwd);

    let Some(found) = found else {
        // No .envrc or .env found — restore if we had active state
        if let Some(state) = load_active(pid) {
            emit_restore(&state.prev, shell, &mut out);
            emit_unset(&mut out, shell, "__DENV_DIR");
            emit_unset(&mut out, shell, "__DENV_DIRTY");
            emit_unset(&mut out, shell, "__DENV_STATE");
            clear_active(pid);
            print_summary(state.prev.iter().map(|pv| {
                let k = match pv {
                    PrevVar::Restore(k, _) | PrevVar::Unset(k) => k.as_str(),
                };
                ('-', k)
            }));
        }
        return Ok(());
    };

    // Mtimes already captured by find_env_files — zero additional stats
    let envrc_mtime = found.envrc.as_ref().map(|(_, m)| *m).unwrap_or(0);
    let dotenv_mtime = found.dotenv.as_ref().map(|(_, m)| *m).unwrap_or(0);

    // Fast path 1: env var check — zero disk reads
    if !force
        && let Ok(state_str) = env::var("__DENV_STATE")
        && let Some((st_envrc, st_dotenv, st_dir)) = parse_denv_state(&state_str)
        && st_envrc == envrc_mtime
        && st_dotenv == dotenv_mtime
        && (st_dir == found.dir.to_string_lossy().as_ref()
            || found
                .dir
                .canonicalize()
                .is_ok_and(|c| st_dir == c.to_string_lossy().as_ref()))
    {
        return Ok(());
    }

    // Fast path 2: active file check — one disk read (fallback when env var not set)
    let active = load_active(pid);
    if !force
        && let Some(ref state) = active
        && state.envrc_mtime == envrc_mtime
        && state.dotenv_mtime == dotenv_mtime
        && (state.dir == found.dir || state.dir == found.dir.canonicalize().unwrap_or_default())
    {
        return Ok(());
    }

    let dir = found
        .dir
        .canonicalize()
        .unwrap_or_else(|_| found.dir.clone());
    let envrc = found
        .envrc
        .as_ref()
        .map(|(p, _)| p.canonicalize().unwrap_or_else(|_| p.clone()));

    // Restore previous state before loading new
    if let Some(ref state) = active {
        emit_restore(&state.prev, shell, &mut out);
    }

    // .envrc requires trust; .env alone does not
    if let Some(ref envrc_path) = envrc
        && !is_allowed(envrc_path)
    {
        eprintln!(
            "denv: {} is blocked. Run `denv allow` to trust it.",
            envrc_path.display()
        );
        emit_export(&mut out, shell, "__DENV_DIR", &dir.to_string_lossy());
        emit_export(&mut out, shell, "__DENV_DIRTY", "1");
        emit_unset(&mut out, shell, "__DENV_STATE");
        if let Some(ref state) = active {
            print_summary(state.prev.iter().map(|pv| {
                let k = match pv {
                    PrevVar::Restore(k, _) | PrevVar::Unset(k) => k.as_str(),
                };
                ('-', k)
            }));
        }
        clear_active(pid);
        return Ok(());
    }

    // Parse .env entries (use found.dotenv directly — no canonicalize needed)
    let dotenv_content = match &found.dotenv {
        Some((p, _)) => match fs::read_to_string(p) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("denv: read .env: {e}");
                return Ok(());
            }
        },
        None => String::new(),
    };
    let dotenv_entries = parse_dotenv(&dotenv_content);

    // Eval: .envrc (if present) then .env entries layered on top
    let diff = if envrc.is_none() {
        // .env-only: diff directly against current env — no subprocess
        let mut set = Vec::new();
        for (k, v) in &dotenv_entries {
            if !env::var(k).is_ok_and(|cur| cur == v.as_ref()) {
                set.push((k.to_string(), v.to_string()));
            }
        }
        EnvDiff {
            set,
            unset: Vec::new(),
        }
    } else {
        match eval_env(&dir, envrc.as_deref(), &dotenv_entries, pid) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("denv: {e}");
                emit_export(&mut out, shell, "__DENV_DIR", &dir.to_string_lossy());
                emit_export(&mut out, shell, "__DENV_DIRTY", "1");
                emit_unset(&mut out, shell, "__DENV_STATE");
                return Ok(());
            }
        }
    };

    // Capture current env values for restore
    let mut prev = Vec::new();
    for (k, _) in &diff.set {
        match env::var(k) {
            Ok(v) => prev.push(PrevVar::Restore(k.clone(), v)),
            Err(_) => prev.push(PrevVar::Unset(k.clone())),
        }
    }
    for k in &diff.unset {
        if let Ok(v) = env::var(k) {
            prev.push(PrevVar::Restore(k.clone(), v));
        }
    }

    emit_diff(&diff, shell, &mut out);
    emit_export(&mut out, shell, "__DENV_DIR", &dir.to_string_lossy());
    emit_unset(&mut out, shell, "__DENV_DIRTY");
    emit_export(
        &mut out,
        shell,
        "__DENV_STATE",
        &format!("{} {} {}", envrc_mtime, dotenv_mtime, dir.display()),
    );
    print_summary(
        diff.set
            .iter()
            .map(|(k, _)| ('+', k.as_str()))
            .chain(diff.unset.iter().map(|k| ('-', k.as_str()))),
    );
    save_active(
        pid,
        &ActiveState {
            dir,
            envrc_mtime,
            dotenv_mtime,
            prev,
        },
    )?;
    Ok(())
}

// --- Hook ---

const FISH_HOOK: &str = r#"function __denv_export --on-variable PWD
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
set -gx __DENV_SHELL fish
denv export fish | source
"#;

const BASH_HOOK: &str = r#"__denv_export() { eval "$(command denv export bash)"; }
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
"#;

const ZSH_HOOK: &str = r#"__denv_export() { eval "$(command denv export zsh)"; }
denv() {
    case "$1" in
        allow|deny|reload) eval "$(command denv "$@")" ;;
        *) command denv "$@" ;;
    esac
}
export __DENV_PID=$$
export __DENV_SHELL=zsh
autoload -Uz add-zsh-hook
add-zsh-hook precmd __denv_export
eval "$(command denv export zsh)"
"#;

// --- Main ---

fn run() -> Result<(), String> {
    let cmd = env::args().nth(1);
    let Some(cmd) = cmd.as_deref() else {
        eprintln!(
            "denv {} — fast environment loader",
            env!("CARGO_PKG_VERSION")
        );
        eprintln!("usage: denv <allow|deny|export <fish|bash|zsh>|reload|hook <fish|bash|zsh>>");
        std::process::exit(1);
    };

    match cmd {
        "-V" | "--version" => {
            println!("denv {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "allow" | "deny" => {
            let cwd = env::var_os("PWD")
                .map(PathBuf::from)
                .unwrap_or_else(|| env::current_dir().expect("cannot get cwd"));
            let found = find_env_files(&cwd).ok_or("no .envrc or .env found")?;
            let (envrc, _) = found.envrc.ok_or("no .envrc found")?;
            let envrc = envrc.canonicalize().unwrap_or(envrc);
            if cmd == "allow" {
                cmd_allow(&envrc)?;
            } else {
                cmd_deny(&envrc)?;
            }
            if let (Ok(pid), Ok(shell_str)) = (env::var("__DENV_PID"), env::var("__DENV_SHELL"))
                && let Some(shell) = Shell::from_str(&shell_str)
            {
                cmd_export(&pid, true, shell)?;
            }
        }
        "export" => {
            let shell_arg = env::args().nth(2);
            let shell = shell_arg
                .as_deref()
                .and_then(Shell::from_str)
                .ok_or("usage: denv export <fish|bash|zsh>")?;
            let pid =
                env::var("__DENV_PID").map_err(|_| "__DENV_PID not set (is the hook loaded?)")?;
            cmd_export(&pid, false, shell)?;
        }
        "reload" => {
            let pid =
                env::var("__DENV_PID").map_err(|_| "__DENV_PID not set (is the hook loaded?)")?;
            let shell = env::var("__DENV_SHELL")
                .ok()
                .and_then(|s| Shell::from_str(&s))
                .ok_or("__DENV_SHELL not set (is the hook loaded?)")?;
            cmd_export(&pid, true, shell)?;
        }
        "hook" => {
            let shell_arg = env::args().nth(2);
            match shell_arg.as_deref().and_then(Shell::from_str) {
                Some(Shell::Fish) => print!("{FISH_HOOK}"),
                Some(Shell::Bash) => print!("{BASH_HOOK}"),
                Some(Shell::Zsh) => print!("{ZSH_HOOK}"),
                None => return Err("usage: denv hook <fish|bash|zsh>".to_string()),
            }
        }
        other => {
            return Err(format!("unknown command: {other}"));
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("denv: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // --- trust_key ---

    #[test]
    fn trust_key_encodes_path_bytes() {
        assert_eq!(trust_key(Path::new("/a")), "2f61");
    }

    #[test]
    fn trust_key_empty() {
        assert_eq!(trust_key(Path::new("")), "");
    }

    #[test]
    fn trust_key_slash() {
        assert_eq!(trust_key(Path::new("/")), "2f");
    }

    // --- escape_newlines / unescape_newlines ---

    #[test]
    fn escape_borrows_when_no_special() {
        let r = escape_newlines("hello world");
        assert!(matches!(r, Cow::Borrowed(_)));
        assert_eq!(r, "hello world");
    }

    #[test]
    fn escape_newline() {
        assert_eq!(escape_newlines("a\nb"), "a\\nb");
    }

    #[test]
    fn escape_backslash() {
        assert_eq!(escape_newlines("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_mixed() {
        assert_eq!(escape_newlines("\n\\"), "\\n\\\\");
    }

    #[test]
    fn escape_empty() {
        assert_eq!(escape_newlines(""), "");
    }

    #[test]
    fn unescape_borrows_when_no_backslash() {
        let r = unescape_newlines("hello");
        assert!(matches!(r, Cow::Borrowed(_)));
    }

    #[test]
    fn unescape_newline() {
        assert_eq!(unescape_newlines("a\\nb"), "a\nb");
    }

    #[test]
    fn unescape_backslash() {
        assert_eq!(unescape_newlines("a\\\\b"), "a\\b");
    }

    #[test]
    fn unescape_unknown_escape_preserved() {
        assert_eq!(unescape_newlines("a\\xb"), "a\\xb");
    }

    #[test]
    fn unescape_trailing_backslash() {
        assert_eq!(unescape_newlines("a\\"), "a\\");
    }

    #[test]
    fn escape_unescape_roundtrip_manual() {
        for s in ["", "hello", "a\nb", "a\\b", "\n\n", "\\\\", "\n\\x\n"] {
            let escaped = escape_newlines(s);
            let rt = unescape_newlines(&escaped);
            assert_eq!(rt.as_ref(), s, "roundtrip failed for {s:?}");
        }
    }

    // --- parse_dotenv ---

    #[test]
    fn dotenv_empty_input() {
        assert!(parse_dotenv("").is_empty());
    }

    #[test]
    fn dotenv_comments_and_blanks_only() {
        assert!(parse_dotenv("# comment\n\n# another\n").is_empty());
    }

    #[test]
    fn dotenv_simple_pair() {
        let r = parse_dotenv("FOO=bar");
        assert_eq!(r, vec![("FOO", Cow::Borrowed("bar"))]);
    }

    #[test]
    fn dotenv_multiple_entries() {
        let r = parse_dotenv("A=1\nB=2\nC=3");
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].0, "A");
        assert_eq!(r[1].0, "B");
        assert_eq!(r[2].0, "C");
    }

    #[test]
    fn dotenv_equals_in_value() {
        let r = parse_dotenv("KEY=a=b=c");
        assert_eq!(r[0].1.as_ref(), "a=b=c");
    }

    #[test]
    fn dotenv_empty_value() {
        let r = parse_dotenv("KEY=");
        assert_eq!(r[0].1.as_ref(), "");
    }

    #[test]
    fn dotenv_empty_key_skipped() {
        assert!(parse_dotenv("=value").is_empty());
    }

    #[test]
    fn dotenv_no_equals_skipped() {
        assert!(parse_dotenv("NOEQUALS").is_empty());
    }

    #[test]
    fn dotenv_export_prefix_stripped() {
        let r = parse_dotenv("export FOO=bar");
        assert_eq!(r[0], ("FOO", Cow::Borrowed("bar")));
    }

    #[test]
    fn dotenv_double_quoted_strips_quotes() {
        let r = parse_dotenv(r#"FOO="hello world""#);
        assert_eq!(r[0].1.as_ref(), "hello world");
    }

    #[test]
    fn dotenv_single_quoted_strips_quotes() {
        let r = parse_dotenv("FOO='hello world'");
        assert_eq!(r[0].1.as_ref(), "hello world");
    }

    #[test]
    fn dotenv_double_quoted_escape_sequences() {
        assert_eq!(parse_dotenv(r#"A="a\nb""#)[0].1.as_ref(), "a\nb");
        assert_eq!(parse_dotenv(r#"A="a\tb""#)[0].1.as_ref(), "a\tb");
        assert_eq!(parse_dotenv(r#"A="a\\b""#)[0].1.as_ref(), "a\\b");
        assert_eq!(parse_dotenv(r#"A="a\"b""#)[0].1.as_ref(), "a\"b");
        assert_eq!(parse_dotenv(r#"A="a\$b""#)[0].1.as_ref(), "a$b");
    }

    #[test]
    fn dotenv_double_quoted_unknown_escape_preserved() {
        assert_eq!(parse_dotenv(r#"A="a\xb""#)[0].1.as_ref(), "a\\xb");
    }

    #[test]
    fn dotenv_single_quoted_no_escapes() {
        let r = parse_dotenv(r"A='a\nb'");
        assert_eq!(r[0].1.as_ref(), r"a\nb");
    }

    #[test]
    fn dotenv_whitespace_around_equals() {
        let r = parse_dotenv("  FOO = bar  ");
        assert_eq!(r[0].0, "FOO");
        assert_eq!(r[0].1.as_ref(), "bar");
    }

    #[test]
    fn dotenv_unquoted_value_with_hash() {
        // Unquoted values include # — no inline comment stripping
        let r = parse_dotenv("FOO=bar#baz");
        assert_eq!(r[0].1.as_ref(), "bar#baz");
    }

    #[test]
    fn dotenv_mismatched_quotes_treated_as_unquoted() {
        // Starts with " but doesn't end with " — not double-quoted
        let r = parse_dotenv(r#"FOO="bar"#);
        assert_eq!(r[0].1.as_ref(), r#""bar"#);
    }

    #[test]
    fn dotenv_lone_quote_no_panic() {
        // Regression: ="  (single " as value) must not panic
        let r = parse_dotenv("K=\"");
        assert_eq!(r[0].1.as_ref(), "\"");
        let r = parse_dotenv("K='");
        assert_eq!(r[0].1.as_ref(), "'");
    }

    #[test]
    fn dotenv_empty_quoted_value() {
        assert_eq!(parse_dotenv(r#"A="""#)[0].1.as_ref(), "");
        assert_eq!(parse_dotenv("A=''")[0].1.as_ref(), "");
    }

    // --- diff_sorted_env ---

    #[test]
    fn diff_both_empty() {
        let d = diff_sorted_env(&[], &[]);
        assert!(d.set.is_empty());
        assert!(d.unset.is_empty());
    }

    #[test]
    fn diff_all_added() {
        let d = diff_sorted_env(&[], &[("A", "1"), ("B", "2")]);
        assert_eq!(d.set.len(), 2);
        assert!(d.unset.is_empty());
    }

    #[test]
    fn diff_all_removed() {
        let d = diff_sorted_env(&[("A", "1"), ("B", "2")], &[]);
        assert!(d.set.is_empty());
        assert_eq!(d.unset, ["A", "B"]);
    }

    #[test]
    fn diff_value_changed() {
        let d = diff_sorted_env(&[("A", "1")], &[("A", "2")]);
        assert_eq!(d.set, [("A".into(), "2".into())]);
        assert!(d.unset.is_empty());
    }

    #[test]
    fn diff_no_changes() {
        let d = diff_sorted_env(&[("A", "1"), ("B", "2")], &[("A", "1"), ("B", "2")]);
        assert!(d.set.is_empty());
        assert!(d.unset.is_empty());
    }

    #[test]
    fn diff_interleaved() {
        let before = [("A", "1"), ("C", "3"), ("E", "5")];
        let after = [("B", "2"), ("C", "changed"), ("D", "4")];
        let d = diff_sorted_env(&before, &after);
        assert_eq!(
            d.set,
            [
                ("B".into(), "2".into()),
                ("C".into(), "changed".into()),
                ("D".into(), "4".into()),
            ]
        );
        assert_eq!(d.unset, ["A", "E"]);
    }

    // --- parse_env_null ---

    #[test]
    fn env_null_empty() {
        assert!(parse_env_null(b"").is_empty());
    }

    #[test]
    fn env_null_basic_sorted() {
        let r = parse_env_null(b"FOO=bar\0BAZ=qux\0");
        assert_eq!(r, [("BAZ", "qux"), ("FOO", "bar")]);
    }

    #[test]
    fn env_null_filters_builtins() {
        let input =
            b"_=/usr/bin/env\0SHLVL=1\0PWD=/tmp\0OLDPWD=/\0BASH_EXECUTION_STRING=x\0KEEP=y\0";
        let r = parse_env_null(input);
        assert_eq!(r, [("KEEP", "y")]);
    }

    #[test]
    fn env_null_no_equals_skipped() {
        let r = parse_env_null(b"NOEQUALS\0A=1\0");
        assert_eq!(r, [("A", "1")]);
    }

    #[test]
    fn env_null_value_with_equals() {
        let r = parse_env_null(b"K=a=b=c\0");
        assert_eq!(r, [("K", "a=b=c")]);
    }

    #[test]
    fn env_null_consecutive_nulls() {
        let r = parse_env_null(b"\0\0A=1\0\0");
        assert_eq!(r, [("A", "1")]);
    }

    // --- parse_denv_state ---

    #[test]
    fn state_valid() {
        assert_eq!(
            parse_denv_state("123 456 /some/dir"),
            Some((123, 456, "/some/dir"))
        );
    }

    #[test]
    fn state_dir_with_spaces() {
        assert_eq!(
            parse_denv_state("1 2 /path with spaces"),
            Some((1, 2, "/path with spaces"))
        );
    }

    #[test]
    fn state_empty() {
        assert!(parse_denv_state("").is_none());
    }

    #[test]
    fn state_one_field() {
        assert!(parse_denv_state("123").is_none());
    }

    #[test]
    fn state_two_fields_no_dir() {
        assert!(parse_denv_state("123 456").is_none());
    }

    #[test]
    fn state_non_numeric_mtime() {
        assert!(parse_denv_state("abc 456 /dir").is_none());
        assert!(parse_denv_state("123 abc /dir").is_none());
    }

    // --- push_bash_escaped ---

    #[test]
    fn bash_escape_simple() {
        let mut s = String::new();
        push_bash_escaped(&mut s, "hello");
        assert_eq!(s, "'hello'");
    }

    #[test]
    fn bash_escape_with_single_quote() {
        let mut s = String::new();
        push_bash_escaped(&mut s, "it's");
        assert_eq!(s, "'it'\\''s'");
    }

    #[test]
    fn bash_escape_empty() {
        let mut s = String::new();
        push_bash_escaped(&mut s, "");
        assert_eq!(s, "''");
    }

    #[test]
    fn bash_escape_only_quotes() {
        let mut s = String::new();
        push_bash_escaped(&mut s, "''");
        assert_eq!(s, "''\\'''\\'''");
    }

    // --- proptest ---

    mod prop {
        use super::*;
        use proptest::prelude::*;
        use std::collections::BTreeMap;

        proptest! {
            #[test]
            fn escape_unescape_roundtrip(s in "\\PC*") {
                let escaped = escape_newlines(&s);
                let roundtripped = unescape_newlines(&escaped);
                prop_assert_eq!(roundtripped.as_ref(), s.as_str());
            }

            #[test]
            fn parse_dotenv_never_panics(s in "\\PC{0,500}") {
                let _ = parse_dotenv(&s);
            }

            #[test]
            fn parse_env_null_never_panics(data in proptest::collection::vec(any::<u8>(), 0..500)) {
                let _ = parse_env_null(&data);
            }

            #[test]
            fn parse_denv_state_never_panics(s in "\\PC{0,200}") {
                let _ = parse_denv_state(&s);
            }

            #[test]
            fn diff_applied_to_before_yields_after(
                before_raw in proptest::collection::vec(("[A-Z]{1,3}", "[a-z0-9]{0,10}"), 0..15),
                after_raw in proptest::collection::vec(("[A-Z]{1,3}", "[a-z0-9]{0,10}"), 0..15),
            ) {
                // Deduplicate and sort via BTreeMap
                let before_map: BTreeMap<&str, &str> =
                    before_raw.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let after_map: BTreeMap<&str, &str> =
                    after_raw.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

                let before: Vec<(&str, &str)> = before_map.iter().map(|(&k, &v)| (k, v)).collect();
                let after: Vec<(&str, &str)> = after_map.iter().map(|(&k, &v)| (k, v)).collect();

                let diff = diff_sorted_env(&before, &after);

                // Apply diff to before
                let mut result: BTreeMap<&str, &str> = before_map;
                for k in &diff.unset {
                    result.remove(k.as_str());
                }
                for (k, v) in &diff.set {
                    result.insert(k.as_str(), v.as_str());
                }

                let result_vec: Vec<(&str, &str)> = result.into_iter().collect();
                prop_assert_eq!(result_vec, after);
            }
        }
    }
}
