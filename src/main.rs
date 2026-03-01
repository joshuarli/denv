use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// --- Find env files ---

struct EnvFiles {
    dir: PathBuf,
    envrc: Option<PathBuf>,
    dotenv: Option<PathBuf>,
}

fn find_env_files(start: &Path) -> Option<EnvFiles> {
    let mut dir = start;
    loop {
        let envrc = dir.join(".envrc");
        let dotenv = dir.join(".env");
        let has_envrc = envrc.is_file();
        let has_dotenv = dotenv.is_file();
        if has_envrc || has_dotenv {
            return Some(EnvFiles {
                dir: dir.to_path_buf(),
                envrc: if has_envrc { Some(envrc) } else { None },
                dotenv: if has_dotenv { Some(dotenv) } else { None },
            });
        }
        dir = dir.parent()?;
    }
}

// --- Trust ---

fn trust_key(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn data_dir() -> PathBuf {
    if let Ok(d) = env::var("DENV_DATA_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(d) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(d).join("denv");
    }
    let home = env::var("HOME").expect("HOME not set");
    PathBuf::from(home).join(".local/share/denv")
}

fn allow_dir() -> PathBuf {
    data_dir().join("allow")
}

fn mtime_of(path: &Path) -> io::Result<u64> {
    Ok(path.metadata()?.mtime() as u64)
}

fn is_allowed(envrc: &Path) -> bool {
    let key = trust_key(envrc);
    let trust_file = allow_dir().join(&key);
    let stored = match fs::read_to_string(&trust_file) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let current = match mtime_of(envrc) {
        Ok(m) => m,
        Err(_) => return false,
    };
    stored.trim() == current.to_string()
}

fn cmd_allow(envrc: &Path) {
    let key = trust_key(envrc);
    let dir = allow_dir();
    fs::create_dir_all(&dir).expect("failed to create allow dir");
    let mtime = mtime_of(envrc).expect("failed to read .envrc mtime");
    fs::write(dir.join(&key), mtime.to_string()).expect("failed to write trust file");
    eprintln!("denv: allowed {}", envrc.display());
}

fn cmd_deny(envrc: &Path) {
    let key = trust_key(envrc);
    let trust_file = allow_dir().join(&key);
    match fs::remove_file(&trust_file) {
        Ok(_) => eprintln!("denv: denied {}", envrc.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("denv: not currently allowed");
        }
        Err(e) => panic!("failed to remove trust file: {e}"),
    }
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

fn escape_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape_newlines(s: &str) -> String {
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
    out
}

fn active_path(pid: &str) -> PathBuf {
    data_dir().join(format!("active_{pid}"))
}

fn load_active(pid: &str) -> Option<ActiveState> {
    let content = fs::read_to_string(active_path(pid)).ok()?;
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
            prev.push(PrevVar::Restore(key.to_string(), val));
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

fn save_active(pid: &str, state: &ActiveState) {
    let dir = data_dir();
    fs::create_dir_all(&dir).expect("failed to create data dir");
    let mut buf = String::new();
    buf.push_str(&state.dir.to_string_lossy());
    buf.push('\n');
    buf.push_str(&state.envrc_mtime.to_string());
    buf.push(' ');
    buf.push_str(&state.dotenv_mtime.to_string());
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
    fs::write(active_path(pid), buf).expect("failed to write active file");
}

fn clear_active(pid: &str) {
    let _ = fs::remove_file(active_path(pid));
}

// --- .env parser ---

fn parse_dotenv(path: &Path) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read .env: {e}"))?;
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
        // Strip matching outer quotes
        let val = if (val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\''))
        {
            &val[1..val.len() - 1]
        } else {
            val
        };
        if !key.is_empty() {
            entries.push((key.to_string(), val.to_string()));
        }
    }
    Ok(entries)
}

// --- Bash eval ---

const FILTERED_VARS: &[&str] = &["_", "SHLVL", "PWD", "OLDPWD", "BASH_EXECUTION_STRING"];

fn parse_env_null(data: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in data.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(entry);
        if let Some(eq) = s.find('=') {
            let key = &s[..eq];
            if !FILTERED_VARS.contains(&key) {
                map.insert(key.to_string(), s[eq + 1..].to_string());
            }
        }
    }
    map
}

fn bash_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

struct EnvDiff {
    set: Vec<(String, String)>,
    unset: Vec<String>,
}

fn eval_env(
    envrc: Option<&Path>,
    dotenv_entries: &[(String, String)],
    pid: &str,
) -> Result<EnvDiff, String> {
    let before_path = format!("/tmp/denv_before_{pid}");
    let after_path = format!("/tmp/denv_after_{pid}");

    let mut script = format!("env -0 > '{}'\n", before_path);
    if let Some(envrc) = envrc {
        script.push_str(&format!(". '{}'\n", envrc.display()));
    }
    for (k, v) in dotenv_entries {
        script.push_str(&format!("export {}={}\n", k, bash_escape(v)));
    }
    script.push_str(&format!("env -0 > '{}'", after_path));

    let output = Command::new("bash")
        .arg("-e")
        .arg("-c")
        .arg(&script)
        .output()
        .map_err(|e| format!("failed to run bash: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _ = fs::remove_file(&before_path);
        let _ = fs::remove_file(&after_path);
        // Show stderr first, then stdout (some scripts redirect stderr to stdout)
        let detail = if !stderr.is_empty() {
            stderr.into_owned()
        } else if !stdout.is_empty() {
            stdout.into_owned()
        } else {
            format!("exit code {}", output.status)
        };
        return Err(format!(".envrc evaluation failed:\n{detail}"));
    }

    let before_data = fs::read(&before_path).map_err(|e| format!("read before env: {e}"))?;
    let after_data = fs::read(&after_path).map_err(|e| format!("read after env: {e}"))?;
    let _ = fs::remove_file(&before_path);
    let _ = fs::remove_file(&after_path);

    let before = parse_env_null(&before_data);
    let after = parse_env_null(&after_data);

    let mut set = Vec::new();
    let mut unset = Vec::new();

    for (k, v) in &after {
        match before.get(k) {
            Some(old) if old == v => {}
            _ => set.push((k.clone(), v.clone())),
        }
    }
    for k in before.keys() {
        if !after.contains_key(k) {
            unset.push(k.clone());
        }
    }

    set.sort_by(|a, b| a.0.cmp(&b.0));
    unset.sort();

    Ok(EnvDiff { set, unset })
}

// --- Fish output ---

fn fish_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

fn emit_fish_restore(prev: &[PrevVar], stdout: &mut impl Write) {
    for pv in prev {
        match pv {
            PrevVar::Restore(k, v) => {
                writeln!(stdout, "set -gx {} {};", k, fish_escape(v)).unwrap();
            }
            PrevVar::Unset(k) => {
                writeln!(stdout, "set -e {};", k).unwrap();
            }
        }
    }
}

fn emit_fish_diff(diff: &EnvDiff, stdout: &mut impl Write) {
    for (k, v) in &diff.set {
        writeln!(stdout, "set -gx {} {};", k, fish_escape(v)).unwrap();
    }
    for k in &diff.unset {
        writeln!(stdout, "set -e {};", k).unwrap();
    }
}

// --- Export command ---

fn cmd_export_fish(pid: &str, force: bool) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let cwd = env::current_dir().expect("cannot get cwd");
    let found = find_env_files(&cwd);
    let active = load_active(pid);

    let Some(found) = found else {
        // No .envrc or .env found
        if let Some(state) = active {
            emit_fish_restore(&state.prev, &mut out);
            writeln!(out, "set -e __DENV_DIR;").unwrap();
            writeln!(out, "set -e __DENV_DIRTY;").unwrap();
            clear_active(pid);
        }
        return;
    };

    let dir = found
        .dir
        .canonicalize()
        .unwrap_or_else(|_| found.dir.clone());
    let envrc = found
        .envrc
        .as_ref()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
    let dotenv = found
        .dotenv
        .as_ref()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));

    let envrc_mtime = envrc.as_ref().and_then(|p| mtime_of(p).ok()).unwrap_or(0);
    let dotenv_mtime = dotenv.as_ref().and_then(|p| mtime_of(p).ok()).unwrap_or(0);

    // Fast path: same dir, same mtimes
    if !force {
        if let Some(ref state) = active {
            if state.dir == dir
                && state.envrc_mtime == envrc_mtime
                && state.dotenv_mtime == dotenv_mtime
            {
                return;
            }
        }
    }

    // Restore previous state before loading new
    if let Some(ref state) = active {
        emit_fish_restore(&state.prev, &mut out);
    }

    // .envrc requires trust; .env alone does not
    if let Some(ref envrc_path) = envrc {
        if !is_allowed(envrc_path) {
            eprintln!(
                "denv: {} is blocked. Run `denv allow` to trust it.",
                envrc_path.display()
            );
            writeln!(
                out,
                "set -gx __DENV_DIR {};",
                fish_escape(&dir.to_string_lossy())
            )
            .unwrap();
            writeln!(out, "set -gx __DENV_DIRTY 1;").unwrap();
            clear_active(pid);
            return;
        }
    }

    // Parse .env entries
    let dotenv_entries = match &dotenv {
        Some(p) => match parse_dotenv(p) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("denv: {e}");
                return;
            }
        },
        None => Vec::new(),
    };

    // Eval: .envrc (if present) then .env entries layered on top
    let diff = match eval_env(envrc.as_deref(), &dotenv_entries, pid) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("denv: {e}");
            return;
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

    emit_fish_diff(&diff, &mut out);
    writeln!(
        out,
        "set -gx __DENV_DIR {};",
        fish_escape(&dir.to_string_lossy())
    )
    .unwrap();
    writeln!(out, "set -e __DENV_DIRTY;").unwrap();
    save_active(
        pid,
        &ActiveState {
            dir,
            envrc_mtime,
            dotenv_mtime,
            prev,
        },
    );
}

// --- Hook ---

const FISH_HOOK: &str = r#"function __denv_export --on-variable PWD
    set -gx __DENV_PID %self
    denv export fish | source
end
set -gx __DENV_PID %self
denv export fish | source
"#;

// --- Main ---

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("usage: denv <allow|deny|export fish|reload|hook fish>");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "allow" => {
            let cwd = env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
            let found = find_env_files(&cwd).ok_or("no .envrc or .env found")?;
            let envrc = found
                .envrc
                .ok_or("no .envrc found (only .env files need no approval)")?;
            let envrc = envrc.canonicalize().unwrap_or(envrc);
            cmd_allow(&envrc);
            if let Ok(pid) = env::var("__DENV_PID") {
                cmd_export_fish(&pid, true);
            }
        }
        "deny" => {
            let cwd = env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
            let found = find_env_files(&cwd).ok_or("no .envrc or .env found")?;
            let envrc = found.envrc.ok_or("no .envrc found")?;
            let envrc = envrc.canonicalize().unwrap_or(envrc);
            cmd_deny(&envrc);
        }
        "export" => {
            if args.get(2).map(|s| s.as_str()) != Some("fish") {
                return Err("usage: denv export fish".to_string());
            }
            let pid =
                env::var("__DENV_PID").map_err(|_| "__DENV_PID not set (is the hook loaded?)")?;
            cmd_export_fish(&pid, false);
        }
        "reload" => {
            let pid =
                env::var("__DENV_PID").map_err(|_| "__DENV_PID not set (is the hook loaded?)")?;
            cmd_export_fish(&pid, true);
        }
        "hook" => {
            if args.get(2).map(|s| s.as_str()) != Some("fish") {
                return Err("usage: denv hook fish".to_string());
            }
            print!("{FISH_HOOK}");
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
