use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub ok: bool,
}

fn spawn(mut cmd: Command, stdin_data: &str, timeout: Duration) -> Result<Outcome, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_data.as_bytes());
    }
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    Ok(Outcome { stdout, stderr, ok: status.success() })
}

/* The manifest every cell runs under, each official name at the release its lock pins. */
pub const MANIFEST: &str = r#"{
  "imports": {
    "json": "0.1.0",
    "re": "0.1.0",
    "math": "0.1.0",
    "struct": "0.1.0",
    "test": "0.1.0"
  }
}
"#;

// A fresh scratch dir, one per cell so runs never share files.
fn fresh() -> Result<std::path::PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("skill-cell-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).map_err(|e| format!("tempdir failed: {e}"))?;
    std::fs::write(dir.join("edge.json"), MANIFEST).map_err(|e| format!("write edge.json failed: {e}"))?;
    Ok(dir)
}

/* The lock `edge lock` writes for the manifest, resolved once and copied into every cell. */
fn lock(edge: &str) -> Result<&'static str, String> {
    static LOCK: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| {
        let dir = fresh()?;
        let out = Command::new(edge).arg("lock").current_dir(&dir).stdin(Stdio::null()).output().map_err(|e| format!("spawn failed: {e}"))?;
        let text = std::fs::read_to_string(dir.join("edge.lock"));
        let _ = std::fs::remove_dir_all(&dir);
        match (out.status.success(), text) {
            (true, Ok(text)) => Ok(text),
            _ => Err(format!("edge lock failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    })
    .as_deref()
    .map_err(Clone::clone)
}

// The dir of one cell, the manifest beside the lock that resolves it.
fn scratch(edge: &str) -> Result<std::path::PathBuf, String> {
    let dir = fresh()?;
    std::fs::write(dir.join("edge.lock"), lock(edge)?).map_err(|e| format!("write edge.lock failed: {e}"))?;
    Ok(dir)
}

pub fn run_script(edge: &str, src: &str, timeout: Duration) -> Result<Outcome, String> {
    let dir = scratch(edge)?;
    let mut cmd = Command::new(edge);
    cmd.arg("run").arg("--manifest").arg(dir.join("edge.json"));
    let outcome = spawn(cmd, src, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

pub fn run_actor(edge: &str, yml: &str, timeout: Duration) -> Result<Outcome, String> {
    let dir = scratch(edge)?;
    let path = dir.join("actor.yml");
    std::fs::write(&path, yml).map_err(|e| format!("write actor.yml failed: {e}"))?;
    let mut cmd = Command::new(edge);
    cmd.arg("actor").arg(&path);
    let outcome = spawn(cmd, "", timeout);
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

pub fn check(expect: &str, outcome: &Outcome, verdict_is_error: bool) -> Result<(), String> {
    if verdict_is_error {
        if outcome.ok {
            return Err("expected a failing run, exit code was 0".to_string());
        }
        if !outcome.stderr.contains(expect.trim()) {
            return Err(format!(
                "stderr mismatch\n  expected substring: {}\n  got: {}",
                expect.trim(),
                outcome.stderr.trim()
            ));
        }
        return Ok(());
    }
    if !outcome.ok {
        return Err(format!(
            "run failed\n  stderr: {}",
            outcome.stderr.trim()
        ));
    }
    if expect.trim_end() != outcome.stdout.trim_end() {
        return Err(format!(
            "stdout mismatch\n  expected: {}\n  got: {}",
            expect.trim_end(),
            outcome.stdout.trim_end()
        ));
    }
    Ok(())
}
