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
        cmd.args(args)
            .current_dir(cwd)
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
    assert!(r.stdout.is_empty());
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
    assert!(
        r.stdout.contains("unset FOO;"),
        "stdout: {}",
        r.stdout
    );
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
