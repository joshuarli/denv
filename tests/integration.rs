use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn denv_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_denv"))
}

struct TestEnv {
    proj: PathBuf,
    data: PathBuf,
    pid: String,
}

impl TestEnv {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = PathBuf::from(format!("/tmp/denv_test_{id}_{}", std::process::id()));
        let proj = base.join("proj");
        let data = base.join("data");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&data).unwrap();
        Self {
            proj,
            data,
            pid: format!("test{id}"),
        }
    }

    fn write_envrc(&self, content: &str) {
        fs::write(self.proj.join(".envrc"), content).unwrap();
    }

    fn write_dotenv(&self, content: &str) {
        fs::write(self.proj.join(".env"), content).unwrap();
    }

    fn write_envrc_at(&self, subdir: &str, content: &str) {
        let dir = self.proj.join(subdir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".envrc"), content).unwrap();
    }

    fn allow(&self) -> DenvCmd {
        self.allow_in(&self.proj)
    }

    fn allow_in(&self, cwd: &Path) -> DenvCmd {
        self.denv_in(cwd, &["allow"])
    }

    fn denv(&self, args: &[&str]) -> DenvCmd {
        self.denv_in(&self.proj, args)
    }

    fn denv_in(&self, cwd: &Path, args: &[&str]) -> DenvCmd {
        self.denv_in_env(cwd, args, &[])
    }

    fn denv_in_env(&self, cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> DenvCmd {
        let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let mut cmd = Command::new(denv_bin());
        cmd.env_clear()
            .args(args)
            .current_dir(cwd)
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", std::env::var("HOME").unwrap())
            .env("PWD", &canonical_cwd)
            .env("DENV_DATA_DIR", &self.data)
            .env("__DENV_PID", &self.pid)
            .env("__DENV_SHELL", "fish");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let output = cmd.output().expect("failed to run denv");
        DenvCmd {
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
            success: output.status.success(),
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if let Some(base) = self.proj.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

struct DenvCmd {
    stdout: String,
    stderr: String,
    success: bool,
}

impl TestEnv {
    fn denv_bash(&self, args: &[&str]) -> DenvCmd {
        self.denv_in_env(&self.proj, args, &[("__DENV_SHELL", "bash")])
    }

    fn denv_bash_in(&self, cwd: &Path, args: &[&str]) -> DenvCmd {
        self.denv_in_env(cwd, args, &[("__DENV_SHELL", "bash")])
    }

    fn allow_bash(&self) -> DenvCmd {
        self.denv_in_env(&self.proj, &["allow"], &[("__DENV_SHELL", "bash")])
    }
}

// --- Tests ---

#[test]
fn allow_then_export_activates() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stderr.contains("allowed"));
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
}

#[test]
fn export_blocked_without_allow() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(!r.stdout.contains("set -gx FOO")); // env var NOT loaded
    assert!(r.stdout.contains("__DENV_DIRTY")); // but dirty indicator set
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn deny_revokes_trust() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    t.allow();

    // deny unloads env directly (no separate reload needed)
    let r = t.denv(&["deny"]);
    assert!(r.stdout.contains("set -e FOO;"));
    assert!(r.stderr.contains("denied"));
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn leave_directory_restores_vars() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -e FOO;"));
}

#[test]
fn fast_path_no_output_on_same_mtime() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    // Second export with same mtime -> no output
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.is_empty());
    assert!(r.stderr.is_empty());
}

#[test]
fn edit_envrc_invalidates_trust() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export FOO=changed");

    // mtime changed -> trust revoked: vars unloaded + dirty flag set
    let r = t.denv(&["reload"]);
    assert!(r.stdout.contains("set -e FOO;"));
    assert!(r.stdout.contains("set -gx __DENV_DIRTY '1';"));
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn reload_forces_reevaluation() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export FOO=updated");
    t.denv(&["allow"]); // re-allow with new mtime

    let r = t.denv(&["reload"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -gx FOO 'updated';"));
}

#[test]
fn multiple_vars() {
    let t = TestEnv::new();
    t.write_envrc("export AAA=111\nexport BBB=222\nexport CCC=333");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx AAA '111';"));
    assert!(r.stdout.contains("set -gx BBB '222';"));
    assert!(r.stdout.contains("set -gx CCC '333';"));
}

#[test]
fn value_with_spaces() {
    let t = TestEnv::new();
    t.write_envrc("export MSG='hello world'");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx MSG 'hello world';"));
}

#[test]
fn value_with_single_quotes() {
    let t = TestEnv::new();
    t.write_envrc(r#"export MSG="it's fine""#);

    let r = t.allow();
    assert!(r.stdout.contains(r"set -gx MSG 'it\'s fine';"));
}

#[test]
fn unset_var_in_envrc() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar\nexport BAZ=qux");
    t.allow();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export BAZ=qux");

    // Re-allow + export: restores old (FOO+BAZ unset), loads new (BAZ set)
    let r = t.allow();
    assert!(r.stdout.contains("set -e FOO;"));
    assert!(r.stdout.contains("set -gx BAZ 'qux';"));
}

#[test]
fn switch_between_directories() {
    let t = TestEnv::new();

    t.write_envrc_at("projA", "export PROJ=A");
    t.write_envrc_at("projB", "export PROJ=B");

    let dir_a = t.proj.join("projA");
    let dir_b = t.proj.join("projB");

    t.allow_in(&dir_a);
    t.allow_in(&dir_b);

    // Export in projA to set active
    let r = t.denv_in(&dir_a, &["export", "fish"]);
    assert!(r.stdout.contains("set -gx PROJ 'A';"));

    // Switch to projB
    let r = t.denv_in(&dir_b, &["export", "fish"]);
    assert!(r.stdout.contains("set -gx PROJ 'B';"));
}

#[test]
fn hook_fish_output() {
    let t = TestEnv::new();
    let r = t.denv(&["hook", "fish"]);
    assert!(r.success);
    assert!(
        r.stdout
            .contains("function __denv_export --on-variable PWD")
    );
    assert!(r.stdout.contains("function denv --wraps denv"));
    assert!(r.stdout.contains("command denv $argv | source"));
    assert!(r.stdout.contains("denv export fish | source"));
    assert!(r.stdout.contains("%self"));
}

#[test]
fn no_envrc_allow_fails() {
    let t = TestEnv::new();
    let r = t.denv(&["allow"]);
    assert!(!r.success);
    assert!(r.stderr.contains("no .envrc"));
}

#[test]
fn no_envrc_deny_fails() {
    let t = TestEnv::new();
    let r = t.denv(&["deny"]);
    assert!(!r.success);
    assert!(r.stderr.contains("no .envrc"));
}

#[test]
fn deny_when_not_allowed() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    let r = t.denv(&["deny"]);
    assert!(r.success);
    assert!(r.stderr.contains("not currently allowed"));
}

#[test]
fn no_output_when_no_envrc_and_no_active() {
    let t = TestEnv::new();
    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.success);
    assert!(r.stdout.is_empty());
    assert!(r.stderr.is_empty());
}

#[test]
fn unknown_command() {
    let t = TestEnv::new();
    let r = t.denv(&["bogus"]);
    assert!(!r.success);
    assert!(r.stderr.contains("unknown command"));
}

#[test]
fn export_requires_fish_arg() {
    let t = TestEnv::new();
    let r = t.denv(&["export"]);
    assert!(!r.success);
    assert!(r.stderr.contains("usage"));
}

#[test]
fn hook_requires_fish_arg() {
    let t = TestEnv::new();
    let r = t.denv(&["hook"]);
    assert!(!r.success);
    assert!(r.stderr.contains("usage"));
}

#[test]
fn envrc_in_parent_directory() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=parent");
    let child = t.proj.join("sub/deep");
    fs::create_dir_all(&child).unwrap();

    t.allow();

    // Reload from child dir — same envrc found via parent walk
    let r = t.denv_in(&child, &["reload"]);
    assert!(r.stdout.contains("set -gx FOO 'parent';"));
}

#[test]
fn envrc_with_path_manipulation() {
    let t = TestEnv::new();
    t.write_envrc("export PATH=\"/custom/bin:$PATH\"");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx PATH '/custom/bin:"));
}

#[test]
fn envrc_error_in_script() {
    let t = TestEnv::new();
    t.write_envrc("false"); // bash -e will fail

    let r = t.allow();
    assert!(r.stderr.contains("evaluation failed"));
    assert!(r.stdout.contains("__DENV_DIRTY"));
    assert!(!r.stdout.contains("set -gx FOO"));
}

#[test]
fn denv_dir_set_on_activate() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow();
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout
            .contains(&format!("set -gx __DENV_DIR '{}';", proj.display()))
    );
    assert!(r.stdout.contains("set -e __DENV_DIRTY;"));
}

#[test]
fn denv_dir_cleared_on_leave() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.stdout.contains("set -e __DENV_DIR;"));
    assert!(r.stdout.contains("set -e __DENV_DIRTY;"));
}

#[test]
fn denv_dirty_when_blocked() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    // Not allowed → should set __DENV_DIR but also __DENV_DIRTY
    let r = t.denv(&["export", "fish"]);
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout
            .contains(&format!("set -gx __DENV_DIR '{}';", proj.display()))
    );
    assert!(r.stdout.contains("set -gx __DENV_DIRTY '1';"));
}

#[test]
fn denv_dirty_cleared_after_allow() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    // First export → blocked, dirty
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx __DENV_DIRTY '1';"));

    // Allow + export → activates, clears dirty
    let r = t.allow();
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
    assert!(r.stdout.contains("set -e __DENV_DIRTY;"));
}

// --- .env tests ---

#[test]
fn dotenv_only_loads_without_allow() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar");

    // .env alone requires no trust
    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
    assert!(!r.stdout.contains("set -gx __DENV_DIRTY"));
}

#[test]
fn dotenv_after_envrc() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=from_envrc\nexport ONLY_ENVRC=1");
    t.write_dotenv("FOO=from_dotenv\nONLY_DOTENV=1");

    let r = t.allow();
    // .env overrides .envrc for FOO
    assert!(r.stdout.contains("set -gx FOO 'from_dotenv';"));
    // Both sources contribute unique vars
    assert!(r.stdout.contains("set -gx ONLY_ENVRC '1';"));
    assert!(r.stdout.contains("set -gx ONLY_DOTENV '1';"));
}

#[test]
fn dotenv_with_comments_and_blanks() {
    let t = TestEnv::new();
    t.write_dotenv("# this is a comment\n\nFOO=bar\n# another comment\nBAZ=qux\n");

    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
    assert!(r.stdout.contains("set -gx BAZ 'qux';"));
}

#[test]
fn dotenv_with_export_prefix() {
    let t = TestEnv::new();
    t.write_dotenv("export FOO=bar\nexport BAZ=qux");

    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
    assert!(r.stdout.contains("set -gx BAZ 'qux';"));
}

#[test]
fn dotenv_with_quoted_values() {
    let t = TestEnv::new();
    t.write_dotenv("DOUBLE=\"hello world\"\nSINGLE='foo bar'");

    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx DOUBLE 'hello world';"));
    assert!(r.stdout.contains("set -gx SINGLE 'foo bar';"));
}

#[test]
fn dotenv_escape_newline() {
    let t = TestEnv::new();
    t.write_dotenv(r#"MSG="hello\nworld""#);

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    // Value should contain actual newline, not literal \n
    assert!(
        r.stdout.contains("set -gx MSG 'hello\nworld';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn dotenv_escape_tab() {
    let t = TestEnv::new();
    t.write_dotenv(r#"MSG="col1\tcol2""#);

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx MSG 'col1\tcol2';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn dotenv_escape_backslash() {
    let t = TestEnv::new();
    t.write_dotenv(r#"MSG="path\\to\\file""#);

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(
        r.stdout.contains(r"set -gx MSG 'path\to\file';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn dotenv_escape_quote() {
    let t = TestEnv::new();
    t.write_dotenv(r#"MSG="say \"hi\"""#);

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(
        r.stdout.contains(r#"set -gx MSG 'say "hi"';"#),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn dotenv_single_quote_no_escape() {
    let t = TestEnv::new();
    t.write_dotenv(r"MSG='hello\nworld'");

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    // Single-quoted: no escape processing, literal \n
    assert!(
        r.stdout.contains(r"set -gx MSG 'hello\nworld';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn dotenv_leave_directory_restores() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar");
    t.denv(&["export", "fish"]); // loads, saves active

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.stdout.contains("set -e FOO;"));
}

#[test]
fn dotenv_fast_path() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar");

    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx FOO"));

    // Same mtime → no output
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.is_empty());
    assert!(r.stderr.is_empty());
}

#[test]
fn dotenv_change_triggers_reload() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar");
    t.denv(&["export", "fish"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_dotenv("FOO=changed");

    // mtime changed → reload picks up new value (no allow needed)
    let r = t.denv(&["reload"]);
    assert!(r.stdout.contains("set -gx FOO 'changed';"));
}

// --- direnv compat tests ---

#[test]
fn compat_path_add_relative() {
    let t = TestEnv::new();
    t.write_envrc("PATH_add .venv/bin");

    let r = t.allow();
    let proj = t.proj.canonicalize().unwrap();
    let expected = format!("{}", proj.join(".venv/bin").display());
    assert!(r.stdout.contains(&expected), "stdout: {}", r.stdout);
}

#[test]
fn compat_path_add_absolute() {
    let t = TestEnv::new();
    t.write_envrc("PATH_add /custom/bin");

    let r = t.allow();
    assert!(r.stdout.contains("/custom/bin"), "stdout: {}", r.stdout);
}

#[test]
fn compat_has() {
    let t = TestEnv::new();
    // bash always exists; bogus_cmd_xyz never does
    t.write_envrc("has bash && export HAS_BASH=1\nhas bogus_cmd_xyz || export NO_BOGUS=1");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx HAS_BASH '1';"));
    assert!(r.stdout.contains("set -gx NO_BOGUS '1';"));
}

#[test]
fn compat_dotenv_in_envrc() {
    let t = TestEnv::new();
    t.write_envrc("dotenv");
    t.write_dotenv("FROMENV=yes");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx FROMENV 'yes';"));
}

#[test]
fn compat_source_env() {
    let t = TestEnv::new();
    fs::write(t.proj.join("extra.sh"), "export EXTRA=loaded\n").unwrap();
    t.write_envrc("source_env extra.sh");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx EXTRA 'loaded';"));
}

// --- summary tests ---

#[test]
fn summary_on_activate() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar\nexport BAZ=qux");

    let r = t.allow();
    assert!(r.stderr.contains("+BAZ"));
    assert!(r.stderr.contains("+FOO"));
    // Internal vars not shown
    assert!(!r.stderr.contains("__DENV_"));
}

#[test]
fn summary_on_leave() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.stderr.contains("-FOO"));
}

#[test]
fn summary_on_deny() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    let r = t.denv(&["deny"]);
    assert!(r.stderr.contains("-FOO"));
}

// --- __DENV_STATE env var fast path tests ---

#[test]
fn state_var_set_on_activate() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow();
    assert!(r.stdout.contains("set -gx __DENV_STATE"));
}

#[test]
fn state_var_cleared_on_leave() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.stdout.contains("set -e __DENV_STATE;"));
}

#[test]
fn state_var_cleared_on_blocked() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    // Not allowed → blocked → no __DENV_STATE
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -e __DENV_STATE;"));
}

#[test]
fn state_var_fast_path_skips_disk_read() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow();
    // Extract __DENV_STATE value from output
    let state_line = r
        .stdout
        .lines()
        .find(|l| l.contains("__DENV_STATE"))
        .expect("should emit __DENV_STATE");
    // Parse: "set -gx __DENV_STATE 'value';"
    let val = state_line
        .strip_prefix("set -gx __DENV_STATE '")
        .unwrap()
        .strip_suffix("';")
        .unwrap();

    // Delete the active file — env var fast path should still work
    let active_file = t.data.join(format!("active_{}", t.pid));
    assert!(active_file.exists());
    fs::remove_file(&active_file).unwrap();

    // With __DENV_STATE set, fast path triggers — no output, no error
    let r = t.denv_in_env(&t.proj, &["export", "fish"], &[("__DENV_STATE", val)]);
    assert!(r.stdout.is_empty(), "stdout: {}", r.stdout);
    assert!(r.stderr.is_empty(), "stderr: {}", r.stderr);
}

#[test]
fn stale_state_var_falls_through() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow();

    // Stale __DENV_STATE with wrong mtimes — should fall through to active file
    let r = t.denv_in_env(
        &t.proj,
        &["export", "fish"],
        &[("__DENV_STATE", "0 0 /wrong/dir")],
    );
    // Falls through to fast path 2 (active file matches) — no output
    assert!(r.stdout.is_empty(), "stdout: {}", r.stdout);
}

#[test]
fn state_var_fast_path_detects_mtime_change() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow();
    let state_line = r
        .stdout
        .lines()
        .find(|l| l.contains("__DENV_STATE"))
        .unwrap();
    let val = state_line
        .strip_prefix("set -gx __DENV_STATE '")
        .unwrap()
        .strip_suffix("';")
        .unwrap();

    // Edit envrc — mtime changes
    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export FOO=changed");
    t.denv(&["allow"]); // re-allow

    // Old __DENV_STATE has stale mtime — should NOT fast path
    let r = t.denv_in_env(&t.proj, &["reload"], &[("__DENV_STATE", val)]);
    assert!(
        r.stdout.contains("set -gx FOO 'changed';"),
        "stdout: {}",
        r.stdout
    );
}

// --- Bash/Zsh shell tests ---

#[test]
fn bash_export_syntax() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow_bash();
    assert!(r.success);
    assert!(
        r.stdout.contains("export FOO='bar';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn zsh_export_syntax() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv_in_env(&t.proj, &["allow"], &[("__DENV_SHELL", "zsh")]);
    assert!(r.success);
    assert!(
        r.stdout.contains("export FOO='bar';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn bash_unset_syntax() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow_bash();

    let r = t.denv_bash_in(Path::new("/tmp"), &["export", "bash"]);
    assert!(r.success);
    assert!(r.stdout.contains("unset FOO;"), "stdout: {}", r.stdout);
}

#[test]
fn bash_single_quote_escaping() {
    let t = TestEnv::new();
    t.write_envrc(r#"export MSG="it's fine""#);

    let r = t.allow_bash();
    assert!(
        r.stdout.contains("export MSG='it'\\''s fine';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn bash_state_vars() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.allow_bash();
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout
            .contains(&format!("export __DENV_DIR='{}';", proj.display())),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("unset __DENV_DIRTY;"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("export __DENV_STATE="),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn bash_blocked_state() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv_bash(&["export", "bash"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("export __DENV_DIRTY='1';"),
        "stdout: {}",
        r.stdout
    );
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn bash_leave_clears_state() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow_bash();

    let r = t.denv_bash_in(Path::new("/tmp"), &["export", "bash"]);
    assert!(
        r.stdout.contains("unset __DENV_DIR;"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("unset __DENV_DIRTY;"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("unset __DENV_STATE;"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn bash_hook_output() {
    let t = TestEnv::new();
    let r = t.denv(&["hook", "bash"]);
    assert!(r.success);
    assert!(r.stdout.contains("__denv_export()"));
    assert!(r.stdout.contains("PROMPT_COMMAND="));
    assert!(r.stdout.contains("export __DENV_PID=$$"));
    assert!(r.stdout.contains("export __DENV_SHELL=bash"));
    assert!(r.stdout.contains("denv export bash"));
}

#[test]
fn zsh_hook_output() {
    let t = TestEnv::new();
    let r = t.denv(&["hook", "zsh"]);
    assert!(r.success);
    assert!(r.stdout.contains("__denv_export()"));
    assert!(r.stdout.contains("add-zsh-hook precmd __denv_export"));
    assert!(r.stdout.contains("export __DENV_PID=$$"));
    assert!(r.stdout.contains("export __DENV_SHELL=zsh"));
    assert!(r.stdout.contains("denv export zsh"));
}

#[test]
fn fish_hook_sets_denv_shell() {
    let t = TestEnv::new();
    let r = t.denv(&["hook", "fish"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx __DENV_SHELL fish"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn bash_allow_reexports() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    // allow with __DENV_SHELL=bash should output bash syntax
    let r = t.denv_in_env(&t.proj, &["allow"], &[("__DENV_SHELL", "bash")]);
    assert!(r.success);
    assert!(
        r.stdout.contains("export FOO='bar';"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("set -gx"),
        "should not contain fish syntax: {}",
        r.stdout
    );
}

#[test]
fn reload_uses_denv_shell() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.allow_bash();

    let r = t.denv_bash(&["reload"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("export FOO='bar';"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("set -gx"),
        "should not contain fish syntax: {}",
        r.stdout
    );
}

#[test]
fn dotenv_only_no_subprocess() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar\nBAZ=qux");

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -gx FOO 'bar';"));
    assert!(r.stdout.contains("set -gx BAZ 'qux';"));
    // No bash subprocess was spawned, so BASH_EXECUTION_STRING should not appear
    assert!(
        !r.stdout.contains("BASH_EXECUTION_STRING"),
        "stdout should not leak bash internals: {}",
        r.stdout
    );
}

#[test]
fn dotenv_only_skips_unchanged() {
    let t = TestEnv::new();
    t.write_dotenv("FOO=bar");

    // Pre-set FOO=bar in the environment so the diff sees no change
    let r = t.denv_in_env(&t.proj, &["export", "fish"], &[("FOO", "bar")]);
    assert!(r.success);
    // FOO should not be re-exported since it already matches
    assert!(
        !r.stdout.contains("set -gx FOO"),
        "should skip unchanged var: {}",
        r.stdout
    );
    // __DENV_DIR and __DENV_STATE should still be set
    assert!(r.stdout.contains("__DENV_DIR"));
    assert!(r.stdout.contains("__DENV_STATE"));
}

#[test]
fn dotenv_only_with_envrc_still_uses_bash() {
    let t = TestEnv::new();
    t.write_envrc("export FROM_ENVRC=1");
    t.write_dotenv("FROM_DOTENV=1");

    let r = t.allow();
    assert!(r.success);
    // Both sources should contribute
    assert!(r.stdout.contains("FROM_ENVRC"));
    assert!(r.stdout.contains("FROM_DOTENV"));
}

// --- .envrc edge cases ---

#[test]
fn envrc_empty_is_noop() {
    let t = TestEnv::new();
    t.write_envrc("");

    let r = t.allow();
    assert!(r.success);
    // No user-visible vars set (only __DENV_* internals)
    for line in r.stdout.lines() {
        if line.contains("set -gx") && !line.contains("__DENV_") {
            panic!("unexpected export: {line}");
        }
    }
}

#[test]
fn envrc_newline_in_value() {
    let t = TestEnv::new();
    t.write_envrc(
        r#"export MSG="line1
line2""#,
    );

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx MSG 'line1\nline2';"),
        "stdout: {}",
        r.stdout
    );

    // Leave and verify restore works (tests escape/unescape roundtrip through active state)
    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.stdout.contains("set -e MSG;"));
}

#[test]
fn envrc_empty_value() {
    let t = TestEnv::new();
    t.write_envrc("export EMPTY=''");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx EMPTY '';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_value_with_equals() {
    let t = TestEnv::new();
    t.write_envrc("export DSN='postgres://u:p@host/db?opt=val'");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("postgres://u:p@host/db?opt=val"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_unicode_value() {
    let t = TestEnv::new();
    t.write_envrc("export GREETING='こんにちは'");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx GREETING 'こんにちは';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_modifies_existing_var() {
    let t = TestEnv::new();
    t.write_envrc("export EXISTING=new_value");

    let r = t.denv_in_env(&t.proj, &["allow"], &[("EXISTING", "old_value")]);
    assert!(r.success);
    assert!(r.stdout.contains("set -gx EXISTING 'new_value';"));

    // Leave — should restore old value
    let r = t.denv_in_env(
        Path::new("/tmp"),
        &["export", "fish"],
        &[("EXISTING", "old_value")],
    );
    assert!(
        r.stdout.contains("set -gx EXISTING 'old_value';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_unsets_preexisting_var() {
    let t = TestEnv::new();
    t.write_envrc("unset REMOVE_ME\nexport KEEP=yes");

    let r = t.denv_in_env(&t.proj, &["allow"], &[("REMOVE_ME", "was_here")]);
    assert!(r.success);
    assert!(r.stdout.contains("set -e REMOVE_ME;"));
    assert!(r.stdout.contains("set -gx KEEP 'yes';"));
}

#[test]
fn envrc_set_then_unset_is_noop_for_that_var() {
    let t = TestEnv::new();
    t.write_envrc("export TEMP_VAR=hello\nunset TEMP_VAR\nexport REAL=yes");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx REAL 'yes';"));
    assert!(
        !r.stdout.contains("TEMP_VAR"),
        "TEMP_VAR should not appear: {}",
        r.stdout
    );
}

#[test]
fn envrc_long_value() {
    let t = TestEnv::new();
    let long_val = "x".repeat(10_000);
    t.write_envrc(&format!("export BIG='{long_val}'"));

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains(&long_val),
        "long value should survive roundtrip"
    );
}

#[test]
fn envrc_special_chars_in_value() {
    let t = TestEnv::new();
    // Tabs, backticks, dollar signs, parens, brackets
    t.write_envrc(r#"export SPECIAL='	`$()[]{}|&;<>'"#);

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx SPECIAL"));
}

#[test]
fn envrc_multiple_path_add() {
    let t = TestEnv::new();
    t.write_envrc("PATH_add bin\nPATH_add scripts\nPATH_add tools");

    let r = t.allow();
    assert!(r.success);
    let proj = t.proj.canonicalize().unwrap();
    let stdout = &r.stdout;
    assert!(
        stdout.contains(&format!("{}/bin", proj.display())),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{}/scripts", proj.display())),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{}/tools", proj.display())),
        "stdout: {stdout}"
    );
}

#[test]
fn envrc_strict_env() {
    let t = TestEnv::new();
    // strict_env enables set -euo pipefail; undefined var reference should fail
    t.write_envrc("strict_env\necho $UNDEFINED_VAR_ABCXYZ\nexport FOO=bar");

    let r = t.allow();
    assert!(r.stderr.contains("evaluation failed"));
    assert!(!r.stdout.contains("set -gx FOO"));
}

#[test]
fn envrc_unstrict_after_strict() {
    let t = TestEnv::new();
    t.write_envrc("strict_env\nexport A=1\nunstrict_env\nexport B=2");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx A '1';"));
    assert!(r.stdout.contains("set -gx B '2';"));
}

#[test]
fn envrc_source_env_nonexistent_is_silent() {
    let t = TestEnv::new();
    t.write_envrc("source_env_if_exists nonexistent.sh\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"));
}

#[test]
fn envrc_watch_file_is_noop() {
    let t = TestEnv::new();
    t.write_envrc("watch_file Makefile\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"));
}

#[test]
fn envrc_expand_path_relative() {
    let t = TestEnv::new();
    t.write_envrc("export EXPANDED=$(expand_path sub)");

    let r = t.allow();
    assert!(r.success);
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout.contains(&format!("{}/sub", proj.display())),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_path_rm() {
    let t = TestEnv::new();
    t.write_envrc("PATH_add /custom/bin\nPATH_rm '/custom/bin'");

    let r = t.allow();
    assert!(r.success);
    // /custom/bin added then removed — should not appear in final PATH
    let path_line = r.stdout.lines().find(|l| l.starts_with("set -gx PATH "));
    if let Some(line) = path_line {
        assert!(
            !line.contains("/custom/bin"),
            "PATH_rm should have removed /custom/bin: {line}"
        );
    }
}

#[test]
fn envrc_log_status_goes_to_stderr() {
    let t = TestEnv::new();
    t.write_envrc("log_status 'loading project'\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stderr.contains("loading project"));
    assert!(r.stdout.contains("set -gx OK '1';"));
}

#[test]
fn envrc_log_error_goes_to_stderr() {
    let t = TestEnv::new();
    t.write_envrc("log_error 'something wrong'\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stderr.contains("something wrong"));
}

#[test]
fn envrc_conditional_export() {
    let t = TestEnv::new();
    t.write_envrc("if has bash; then export HAS_SHELL=1; fi\nif has totally_bogus_cmd; then export NOPE=1; fi");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx HAS_SHELL '1';"));
    assert!(!r.stdout.contains("NOPE"), "stdout: {}", r.stdout);
}

#[test]
fn envrc_dotenv_override() {
    // .env loaded after .envrc — .env wins for shared keys
    let t = TestEnv::new();
    t.write_envrc("export SHARED=from_envrc\nexport ENVRC_ONLY=1");
    t.write_dotenv("SHARED=from_dotenv\nDOTENV_ONLY=1");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx SHARED 'from_dotenv';"),
        "stdout: {}",
        r.stdout
    );
    assert!(r.stdout.contains("ENVRC_ONLY"));
    assert!(r.stdout.contains("DOTENV_ONLY"));
}

#[test]
fn envrc_syntax_error() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar\nif; then; fi");

    let r = t.allow();
    assert!(r.stderr.contains("evaluation failed"));
    assert!(r.stdout.contains("__DENV_DIRTY"));
}

#[test]
fn envrc_backslash_in_value() {
    let t = TestEnv::new();
    t.write_envrc(r#"export BS='back\slash'"#);

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains(r"set -gx BS 'back\slash';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_manpath_add() {
    let t = TestEnv::new();
    t.write_envrc("MANPATH_add man");

    let r = t.allow();
    assert!(r.success);
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout.contains(&format!("{}/man", proj.display())),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_env_vars_required_missing() {
    let t = TestEnv::new();
    t.write_envrc("env_vars_required TOTALLY_MISSING_VAR_XYZ");

    let r = t.allow();
    // env_vars_required returns nonzero, bash -e causes failure
    assert!(r.stderr.contains("evaluation failed") || r.stderr.contains("required"));
}

#[test]
fn envrc_env_vars_required_present() {
    let t = TestEnv::new();
    t.write_envrc("env_vars_required HOME\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"));
}

#[test]
fn envrc_find_up() {
    let t = TestEnv::new();
    // Create a file in the project root, then an envrc in a subdirectory that finds it
    fs::write(t.proj.join("marker.txt"), "found").unwrap();
    let sub = t.proj.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join(".envrc"), "export FOUND=$(find_up marker.txt)").unwrap();

    t.allow_in(&sub);
    let r = t.denv_in(&sub, &["reload"]);
    assert!(r.success);
    let proj = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout.contains(&format!("{}/marker.txt", proj.display())),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn envrc_source_up() {
    let t = TestEnv::new();
    t.write_envrc("export PARENT=1");
    t.allow();

    let sub = t.proj.join("child");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join(".envrc"), "source_up\nexport CHILD=1").unwrap();
    t.allow_in(&sub);

    let r = t.denv_in(&sub, &["reload"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx PARENT '1';"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("set -gx CHILD '1';"),
        "stdout: {}",
        r.stdout
    );
}

// --- Ported from direnv test suite ---

#[test]
fn direnv_space_in_directory_name() {
    let t = TestEnv::new();
    let space_dir = t.proj.join("space dir");
    fs::create_dir_all(&space_dir).unwrap();
    fs::write(
        space_dir.join(".envrc"),
        "PATH_add bin\nexport SPACE_DIR=true",
    )
    .unwrap();

    t.allow_in(&space_dir);
    let r = t.denv_in(&space_dir, &["reload"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx SPACE_DIR 'true';"),
        "stdout: {}",
        r.stdout
    );
    let canon = space_dir.canonicalize().unwrap();
    assert!(
        r.stdout.contains(&format!("{}/bin", canon.display())),
        "PATH_add should work in space dir: {}",
        r.stdout
    );
}

#[test]
fn direnv_dollar_in_directory_name() {
    let t = TestEnv::new();
    // Literal $test directory name — tests path handling with shell metacharacters
    let dollar_dir = t.proj.join("$test");
    fs::create_dir_all(&dollar_dir).unwrap();
    fs::write(dollar_dir.join(".envrc"), "export FOO=bar").unwrap();

    t.allow_in(&dollar_dir);
    let r = t.denv_in(&dollar_dir, &["reload"]);
    assert!(r.success);
    assert!(
        r.stdout.contains("set -gx FOO 'bar';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn direnv_ls_colors_complex_value() {
    let t = TestEnv::new();
    // LS_COLORS-style value with colons, semicolons, equals inside
    t.write_envrc(r"export LS_COLORS='*.ogg=38;5;45:*.wav=38;5;45:*.flac=38;5;45'");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout
            .contains("*.ogg=38;5;45:*.wav=38;5;45:*.flac=38;5;45"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn direnv_triple_backslash_value() {
    let t = TestEnv::new();
    t.write_envrc(r"export THREE_BS='\\\'");

    let r = t.allow();
    assert!(r.success);
    // Value is three literal backslashes: \\\
    assert!(
        r.stdout.contains(r"set -gx THREE_BS '\\\\\\';")
            || r.stdout.contains(r"set -gx THREE_BS '\\\';"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn direnv_pipe_in_value() {
    let t = TestEnv::new();
    t.write_envrc(r"export LESSOPEN='||/usr/bin/lesspipe.sh %s'");

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("||/usr/bin/lesspipe.sh %s"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn direnv_dotenv_fails_on_missing() {
    let t = TestEnv::new();
    t.write_envrc("dotenv .env.nonexistent\nexport AFTER=1");

    let r = t.allow();
    // dotenv on missing file returns 1, bash -e causes failure
    assert!(r.stderr.contains("evaluation failed"));
    assert!(!r.stdout.contains("AFTER"));
}

#[test]
fn direnv_dotenv_if_exists_missing() {
    let t = TestEnv::new();
    t.write_envrc("dotenv_if_exists .env.nonexistent\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"));
}

#[test]
fn direnv_dotenv_if_exists_present() {
    let t = TestEnv::new();
    t.write_dotenv("FROM_DOTENV=loaded");
    t.write_envrc("dotenv_if_exists\nexport ALSO=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("FROM_DOTENV"), "stdout: {}", r.stdout);
    assert!(r.stdout.contains("set -gx ALSO '1';"));
}

#[test]
fn direnv_path_rm_glob_pattern() {
    let t = TestEnv::new();
    // PATH_rm with glob should remove all matching entries
    t.write_envrc(
        r#"export PATH="/usr/local/bin:/home/foo/bin:/usr/bin:/home/foo/.local/bin"
PATH_rm '/home/foo/*'"#,
    );

    let r = t.allow();
    assert!(r.success);
    let path_line = r
        .stdout
        .lines()
        .find(|l| l.contains("set -gx PATH "))
        .expect("should export PATH");
    assert!(
        !path_line.contains("/home/foo/"),
        "PATH_rm glob should remove all /home/foo/* entries: {path_line}"
    );
    assert!(
        path_line.contains("/usr/local/bin") && path_line.contains("/usr/bin"),
        "non-matching entries preserved: {path_line}"
    );
}

#[test]
fn direnv_path_rm_on_custom_var() {
    let t = TestEnv::new();
    t.write_envrc(
        r#"export MYPATH="/a/one:/b/two:/a/three"
path_rm MYPATH '/a/*'"#,
    );

    let r = t.allow();
    assert!(r.success);
    assert!(
        r.stdout.contains("/b/two"),
        "non-matching entry preserved: {}",
        r.stdout
    );
    // /a/one and /a/three should be removed
    let mypath_line = r
        .stdout
        .lines()
        .find(|l| l.contains("MYPATH"))
        .expect("should export MYPATH");
    assert!(
        !mypath_line.contains("/a/"),
        "path_rm should remove /a/* entries: {mypath_line}"
    );
}

#[test]
fn direnv_source_env_missing_file_fails() {
    let t = TestEnv::new();
    // source_env on a file that doesn't exist returns non-zero,
    // which triggers bash -e — this is expected (use source_env_if_exists for optional)
    t.write_envrc("source_env nonexistent.sh\nexport AFTER=1");

    let r = t.allow();
    assert!(r.stderr.contains("evaluation failed"));
    assert!(!r.stdout.contains("AFTER"));
}

#[test]
fn direnv_source_env_if_exists_missing_dir() {
    let t = TestEnv::new();
    // source_env_if_exists on a nonexistent file should silently succeed
    let empty_dir = t.proj.join("empty_sub");
    fs::create_dir_all(&empty_dir).unwrap();
    t.write_envrc("source_env_if_exists empty_sub/.envrc\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"), "stdout: {}", r.stdout);
}

#[test]
fn direnv_symlink_dir_allow_deny() {
    let t = TestEnv::new();
    let real_dir = t.proj.join("real");
    let link_dir = t.proj.join("linked");
    fs::create_dir_all(&real_dir).unwrap();
    fs::write(real_dir.join(".envrc"), "export SYM=yes").unwrap();
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

    // Allow via symlink path — should resolve to the real .envrc
    let r = t.allow_in(&link_dir);
    assert!(r.success);
    assert!(r.stderr.contains("allowed"));
    assert!(
        r.stdout.contains("set -gx SYM 'yes';"),
        "allow via symlink should activate: {}",
        r.stdout
    );

    // Deny via symlink should also work
    let r = t.denv_in(&link_dir, &["deny"]);
    assert!(r.success);
    assert!(r.stderr.contains("denied"));

    // Re-allow via real path after denying via symlink — should work
    let r = t.allow_in(&real_dir);
    assert!(r.success);
    assert!(r.stderr.contains("allowed"));
}

#[test]
fn direnv_source_up_if_exists_no_parent() {
    let t = TestEnv::new();
    // No parent .envrc exists — source_up_if_exists should silently succeed
    t.write_envrc("source_up_if_exists\nexport OK=1");

    let r = t.allow();
    assert!(r.success);
    assert!(r.stdout.contains("set -gx OK '1';"), "stdout: {}", r.stdout);
}

#[test]
fn direnv_load_prefix() {
    let t = TestEnv::new();
    // Create a prefix-style directory layout
    let prefix = t.proj.join("local");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::create_dir_all(prefix.join("lib")).unwrap();
    fs::create_dir_all(prefix.join("include")).unwrap();
    fs::create_dir_all(prefix.join("share/man")).unwrap();
    t.write_envrc("load_prefix local");

    let r = t.allow();
    assert!(r.success);
    let canon = t.proj.canonicalize().unwrap();
    let stdout = &r.stdout;
    // Should add bin and sbin to PATH
    assert!(
        stdout.contains(&format!("{}/local/bin", canon.display())),
        "load_prefix should add bin to PATH: {stdout}"
    );
    // Should add include to CPATH
    assert!(
        stdout.contains("CPATH"),
        "load_prefix should set CPATH: {stdout}"
    );
    // Should add lib to LIBRARY_PATH
    assert!(
        stdout.contains("LIBRARY_PATH"),
        "load_prefix should set LIBRARY_PATH: {stdout}"
    );
    // Should add man to MANPATH
    assert!(
        stdout.contains("MANPATH"),
        "load_prefix should set MANPATH: {stdout}"
    );
}

#[test]
fn direnv_path_add_custom_variable() {
    let t = TestEnv::new();
    // path_add on a non-PATH variable
    t.write_envrc("path_add PYTHONPATH lib/python\npath_add PYTHONPATH vendor/python");

    let r = t.allow();
    assert!(r.success);
    let canon = t.proj.canonicalize().unwrap();
    assert!(
        r.stdout
            .contains(&format!("{}/lib/python", canon.display())),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains(&format!("{}/vendor/python", canon.display())),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn direnv_exit_code_failure() {
    let t = TestEnv::new();
    // Ported from direnv's "failure" scenario: exit 5 in .envrc
    t.write_envrc("exit 5");

    let r = t.allow();
    // Should report failure, set dirty flag, and NOT loop forever
    assert!(r.stderr.contains("evaluation failed"));
    assert!(r.stdout.contains("__DENV_DIRTY"));
    assert!(!r.stdout.contains("__DENV_STATE '"));
}

#[test]
fn export_requires_valid_shell() {
    let t = TestEnv::new();
    let r = t.denv(&["export", "powershell"]);
    assert!(!r.success);
    assert!(r.stderr.contains("usage"));
}

#[test]
fn hook_requires_valid_shell() {
    let t = TestEnv::new();
    let r = t.denv(&["hook", "powershell"]);
    assert!(!r.success);
    assert!(r.stderr.contains("usage"));
}
