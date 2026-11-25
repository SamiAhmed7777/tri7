//! Test-only support shared by the unit tests of every module and the
//! integration tests (tri7 workqueue item 45, TEST-TMPDIR-LEAK).
//!
//! History: tests used to build a directory under `std::env::temp_dir()`
//! and end with `let _ = std::fs::remove_dir_all(&dir);` — a line that runs
//! only when the test reaches its end, so every failed assertion left its
//! directory behind (a fully green `cargo test --lib` under an isolated
//! TMPDIR left 69 top-level entries / 186 files+dirs / 2.3 MB — measured in
//! evidence/ev_baseline_lib.txt, workqueue item 45). [`TestDir`] owns
//! the removal instead: the directory dies with its binding, on every exit
//! path, and [`test_dir`] names are unique per call so parallel tests never
//! share (and pre-emptively delete) one directory.
//!
//! Convention (drop order matters): bind the guard BEFORE any handle that
//! opens files inside it — `let dir = test_dir(..); let wh = open(.., &dir);`
//! — so the handle drops first and the directory is out of use when it is
//! removed (that keeps `remove_dir_all` portable beyond Unix). Helpers that
//! hand out a directory return the guard with it, as a tuple element bound
//! to a REAL name (`_dir`, not `_`).
//!
//! Cleanup failure is printed to stderr and never panics: a panic inside
//! `drop` during an unwind would abort the whole test binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process serial number making every `test_dir` name unique within one
/// test-binary run (the process id covers cross-binary uniqueness).
fn next_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Keep the tag a single path component even if a caller builds it from
/// runtime values (a `/` would silently nest directories).
fn sanitize(tag: &str) -> String {
    tag.chars()
        .map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c })
        .collect()
}

/// A directory (or single file) deleted when the binding leaves scope — the
/// RAII replacement for a trailing `remove_dir_all`. See the module docs.
#[derive(Debug)]
pub struct TestDir(PathBuf);

impl TestDir {
    /// Adopt a path the test already owns (file or directory): removed on
    /// drop. Never `std::env::temp_dir()` itself — that root is shared by
    /// the whole system.
    pub fn adopt(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// The guarded path.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // A path the test already removed itself is success, not an error.
        let gone = if self.0.is_file() {
            std::fs::remove_file(&self.0)
        } else {
            std::fs::remove_dir_all(&self.0)
        };
        if let Err(e) = gone {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("TestDir cleanup failed ({}): {e}", self.0.display());
            }
        }
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// Create a fresh, uniquely named directory under the system temp dir:
/// `<temp>/<tag>-<pid>-<seq>`, already `create_dir_all`-ed. Two calls with
/// the same tag never alias (unique `-<seq>` suffix), which also retires the
/// old remove-then-create "fresh start" idiom.
pub fn test_dir(tag: &str) -> TestDir {
    let path = std::env::temp_dir().join(format!(
        "{}-{}-{}",
        sanitize(tag),
        std::process::id(),
        next_seq()
    ));
    std::fs::create_dir_all(&path).unwrap_or_else(|e| {
        panic!("test_dir({tag}): create {:#?}: {e}", path.display())
    });
    TestDir(path)
}

/// Run the calling test's body in a FRESH COPY of this test binary.
///
/// Returns `None` in the child (run the body here) and `Some(child output)`
/// in the parent after the child ran the same test (`test` = its full path
/// name, selected with `--exact`).
///
/// Why: some tests pin PROCESS-GLOBAL state — the embedded node's in-process
/// RPC bridge, env vars, signal handlers — and a sibling test in the same
/// process could register its own stub at any moment. In the child that
/// state starts clean and no sibling can touch it.
pub fn child_process(tag: &str, test: &str) -> Option<std::process::Output> {
    const MARK: &str = "TRI7_TEST_CHILD";
    if std::env::var(MARK).as_deref() == Ok(tag) {
        return None;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", test, "--test-threads=1", "--include-ignored"])
        .env(MARK, tag)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn child test process");
    // drain both pipes WHILE the child runs: a chatty child (a real node
    // run) would otherwise block on a full pipe and never reach its exit
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = std::io::Read::read_to_end(&mut p, &mut buf);
            }
            buf
        })
    }
    let out_pipe = drain(child.stdout.take());
    let err_pipe = drain(child.stderr.take());
    // bounded: a wedged child is killed, never a hung test suite
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child process for {test} did not finish within 120s");
            }
            Err(e) => panic!("child process for {test}: {e}"),
        }
    };
    let (stdout, stderr) = (out_pipe.join().unwrap_or_default(), err_pipe.join().unwrap_or_default());
    let out = std::process::Output { status, stdout, stderr };
    assert!(
        out.status.success(),
        "child {test} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // a mistyped `test` name would run ZERO tests and still exit 0
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "child {test} did not run exactly one test:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core promise: a dropped guard removes its directory entirely.
    #[test]
    fn drop_removes_the_directory() {
        let outer = std::env::temp_dir();
        let gone: PathBuf;
        {
            let d = test_dir("testsupport_drop");
            assert!(d.is_dir(), "{} must exist while bound", d.display());
            std::fs::create_dir_all(d.join("nested")).unwrap();
            std::fs::write(d.join("nested").join("f.txt"), b"x").unwrap();
            gone = d.to_path_buf();
        }
        assert!(!gone.exists(), "{} leaked after drop", gone.display());
        assert!(gone.starts_with(&outer));
    }

    /// Two guards with the same tag never alias one directory.
    #[test]
    fn same_tag_never_aliases() {
        let a = test_dir("testsupport_alias");
        let b = test_dir("testsupport_alias");
        assert_ne!(a.path(), b.path(), "unique per call, safe under the parallel runner");
        std::fs::write(b.join("marker"), b"b").unwrap();
        assert!(b.join("marker").exists());
        assert!(!a.join("marker").exists());
    }

    /// `adopt` guards a single file as well as a directory tree.
    #[test]
    fn adopt_covers_files() {
        let d = test_dir("testsupport_adopt");
        let f = d.join("one.txt");
        std::fs::write(&f, b"1").unwrap();
        let g = TestDir::adopt(&f);
        assert!(g.is_file());
        drop(g);
        assert!(!f.exists(), "adopted file must be removed on drop");
        // NotFound on an already-removed path must not print or panic.
        let g2 = TestDir::adopt(&f);
        drop(g2);
    }
}
