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

    fn denv(&self, args: &[&str]) -> DenvCmd {
        self.denv_in(&self.proj, args)
    }

    fn denv_in(&self, cwd: &Path, args: &[&str]) -> DenvCmd {
        let output = Command::new(denv_bin())
            .args(args)
            .current_dir(cwd)
            .env("DENV_DATA_DIR", &self.data)
            .env("__DENV_PID", &self.pid)
            .output()
            .expect("failed to run denv");
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
        let _ = fs::remove_file(format!("/tmp/denv_before_{}", self.pid));
        let _ = fs::remove_file(format!("/tmp/denv_after_{}", self.pid));
    }
}

struct DenvCmd {
    stdout: String,
    stderr: String,
    success: bool,
}

// --- Tests ---

#[test]
fn allow_activates_immediately() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv(&["allow"]);
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

    let r = t.denv(&["allow"]);
    assert!(r.stdout.contains("set -gx FOO"));

    t.denv(&["deny"]);
    let r = t.denv(&["reload"]);
    assert!(r.stdout.contains("set -e FOO;"));
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn leave_directory_restores_vars() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.denv(&["allow"]); // activates and saves active

    let r = t.denv_in(Path::new("/tmp"), &["export", "fish"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -e FOO;"));
}

#[test]
fn fast_path_no_output_on_same_mtime() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv(&["allow"]);
    assert!(r.stdout.contains("set -gx FOO"));

    // Second export with same mtime -> no output
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.is_empty());
    assert!(r.stderr.is_empty());
}

#[test]
fn edit_envrc_invalidates_trust() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.denv(&["allow"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export FOO=changed");

    // mtime changed -> trust revoked: vars unloaded + dirty flag set
    let r = t.denv(&["reload"]);
    assert!(r.stdout.contains("set -e FOO;"));
    assert!(r.stdout.contains("set -gx __DENV_DIRTY 1;"));
    assert!(r.stderr.contains("blocked"));
}

#[test]
fn reload_forces_reevaluation() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");
    t.denv(&["allow"]);

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

    let r = t.denv(&["allow"]);
    assert!(r.stdout.contains("set -gx AAA '111';"));
    assert!(r.stdout.contains("set -gx BBB '222';"));
    assert!(r.stdout.contains("set -gx CCC '333';"));
}

#[test]
fn value_with_spaces() {
    let t = TestEnv::new();
    t.write_envrc("export MSG='hello world'");

    let r = t.denv(&["allow"]);
    assert!(r.stdout.contains("set -gx MSG 'hello world';"));
}

#[test]
fn value_with_single_quotes() {
    let t = TestEnv::new();
    t.write_envrc(r#"export MSG="it's fine""#);

    let r = t.denv(&["allow"]);
    assert!(r.stdout.contains(r"set -gx MSG 'it\'s fine';"));
}

#[test]
fn unset_var_in_envrc() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar\nexport BAZ=qux");
    t.denv(&["allow"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    t.write_envrc("export BAZ=qux");

    // Re-allow activates immediately: restores old (FOO+BAZ unset), loads new (BAZ set)
    let r = t.denv(&["allow"]);
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

    t.denv_in(&dir_a, &["allow"]);
    t.denv_in(&dir_b, &["allow"]);

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

    t.denv(&["allow"]);

    // allow activated from proj root; export from child finds same envrc, hits fast path
    // Use reload to force re-eval from the child dir
    let r = t.denv_in(&child, &["reload"]);
    assert!(r.stdout.contains("set -gx FOO 'parent';"));
}

#[test]
fn envrc_with_path_manipulation() {
    let t = TestEnv::new();
    t.write_envrc("export PATH=\"/custom/bin:$PATH\"");

    let r = t.denv(&["allow"]);
    assert!(r.success);
    assert!(r.stdout.contains("set -gx PATH '/custom/bin:"));
}

#[test]
fn envrc_error_in_script() {
    let t = TestEnv::new();
    t.write_envrc("false"); // bash -e will fail
    t.denv(&["allow"]);

    let r = t.denv(&["export", "fish"]);
    assert!(r.success);
    assert!(r.stderr.contains("evaluation failed"));
    assert!(r.stdout.is_empty());
}

#[test]
fn denv_dir_set_on_activate() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    let r = t.denv(&["allow"]);
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
    t.denv(&["allow"]);

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
    assert!(r.stdout.contains("set -gx __DENV_DIRTY 1;"));
}

#[test]
fn denv_dirty_cleared_after_allow() {
    let t = TestEnv::new();
    t.write_envrc("export FOO=bar");

    // First export → blocked, dirty
    let r = t.denv(&["export", "fish"]);
    assert!(r.stdout.contains("set -gx __DENV_DIRTY 1;"));

    // Allow → activates, clears dirty
    let r = t.denv(&["allow"]);
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

    let r = t.denv(&["allow"]);
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
