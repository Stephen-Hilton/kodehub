//! Subprocess execution with timeout, output capture/streaming and best-effort sandboxing.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
    pub fn combined(&self) -> String {
        let mut s = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&self.stderr);
        }
        s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputLine {
    pub stream: String,
    pub line: String,
}

/// Environment variables passed to child processes. Everything else (Claude tokens, API keys,
/// database passwords, …) is cleared — generated code must never see credentials.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USER",
    "LOGNAME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "UV_CACHE_DIR",
    "XDG_CACHE_HOME",
];

/// Paths generated code may never read (credentials and the token env file's directory).
pub fn denied_read_paths() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut v: Vec<String> = [
        ".ssh",
        ".claude",
        ".claude.json",
        ".aws",
        ".config/anthropic",
        ".config/gh",
        ".netrc",
        ".docker",
    ]
    .iter()
    .map(|p| format!("{home}/{p}"))
    .collect();
    // the per-launch UI secret is for the UI only
    v.push(
        kodehub_core::config::kodehub_home()
            .join("ui-token")
            .to_string_lossy()
            .to_string(),
    );
    if let Some(dir) = std::env::var_os("KODEHUB_ENV_FILE").and_then(|f| {
        std::path::PathBuf::from(f)
            .parent()
            .map(|d| d.to_path_buf())
    }) {
        v.push(dir.to_string_lossy().to_string());
    }
    v
}

/// macOS sandbox profile: no network, no reads of credential locations.
pub fn sandbox_profile() -> String {
    let denies: String = denied_read_paths()
        .iter()
        .map(|p| format!("(subpath {p:?})"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("(version 1)(allow default)(deny network*)(deny file-read* {denies})")
}

/// Build a command with a cleared environment (allowlist only), wrapped in `sandbox-exec`
/// (no network, no credential reads) when requested and available.
pub fn command(program: &Path, args: &[&str], sandbox: bool) -> Command {
    let sb = Path::new("/usr/bin/sandbox-exec");
    let mut c = if sandbox && sb.exists() {
        let mut c = Command::new(sb);
        c.arg("-p").arg(sandbox_profile()).arg(program);
        c
    } else {
        Command::new(program)
    };
    c.args(args);
    c.env_clear();
    for k in ENV_ALLOWLIST {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    c
}

/// Run to completion (or timeout, which kills the whole process group).
pub async fn run(
    mut cmd: Command,
    stdin: Option<String>,
    timeout: Duration,
    stream: Option<UnboundedSender<OutputLine>>,
) -> std::io::Result<ExecOutput> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let pid = child.id();
    if let Some(input) = stdin {
        if let Some(mut si) = child.stdin.take() {
            si.write_all(input.as_bytes()).await?;
            drop(si);
        }
    }
    let out = child.stdout.take().expect("stdout piped");
    let err = child.stderr.take().expect("stderr piped");
    let s1 = stream.clone();
    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        let mut buf = String::new();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(s) = &s1 {
                let _ = s.send(OutputLine {
                    stream: "stdout".into(),
                    line: l.clone(),
                });
            }
            buf.push_str(&l);
            buf.push('\n');
        }
        buf
    });
    let s2 = stream.clone();
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(err).lines();
        let mut buf = String::new();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(s) = &s2 {
                let _ = s.send(OutputLine {
                    stream: "stderr".into(),
                    line: l.clone(),
                });
            }
            buf.push_str(&l);
            buf.push('\n');
        }
        buf
    });
    let (exit_code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => {
            kill_group(pid);
            let _ = child.kill().await;
            (None, true)
        }
    };
    let stdout = out_task.await.unwrap_or_default();
    let mut stderr = err_task.await.unwrap_or_default();
    if timed_out {
        stderr.push_str(&format!(
            "kodehub: killed after {}s timeout\n",
            timeout.as_secs_f32()
        ));
    }
    Ok(ExecOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(p) = pid {
        // SAFETY: killpg with a pgid we created via process_group(0); failure is harmless.
        unsafe {
            libc::killpg(p as i32, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_kills_process_group() {
        let c = command(Path::new("/bin/sh"), &["-c", "sleep 30 & sleep 30"], false);
        let o = run(c, None, Duration::from_millis(300), None)
            .await
            .unwrap();
        assert!(o.timed_out);
        assert!(o.duration_ms < 5000);
    }

    #[tokio::test]
    async fn captures_and_streams() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let c = command(Path::new("/bin/sh"), &["-c", "cat; echo err >&2"], false);
        let o = run(c, Some("hello".into()), Duration::from_secs(5), Some(tx))
            .await
            .unwrap();
        assert!(o.success());
        assert_eq!(o.stdout, "hello\n");
        assert_eq!(o.stderr, "err\n");
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn secrets_never_reach_child_processes() {
        // SAFETY: test-only env mutation before spawning.
        unsafe {
            std::env::set_var("DEV1_TOKEN", "secret-token-value");
            std::env::set_var("KODEHUB_ANTHROPIC_API_KEY", "secret-key-value");
        }
        let c = command(Path::new("/usr/bin/env"), &[], false);
        let o = run(c, None, Duration::from_secs(5), None).await.unwrap();
        assert!(!o.stdout.contains("secret-"), "{}", o.stdout);
        assert!(o.stdout.contains("PATH="));
    }

    #[tokio::test]
    async fn sandbox_denies_credential_reads_on_macos() {
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            return;
        }
        let home = std::env::var("HOME").unwrap();
        let c = command(Path::new("/bin/ls"), &[&format!("{home}/.ssh")], true);
        let o = run(c, None, Duration::from_secs(5), None).await.unwrap();
        assert!(!o.success(), "~/.ssh must not be readable: {o:?}");
    }

    #[tokio::test]
    async fn sandbox_denies_network_on_macos() {
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            return;
        }
        let c = command(
            Path::new("/usr/bin/python3"),
            &[
                "-c",
                "import socket; socket.create_connection(('1.1.1.1', 80), timeout=2)",
            ],
            true,
        );
        let o = run(c, None, Duration::from_secs(10), None).await.unwrap();
        assert!(!o.success(), "network should be denied: {o:?}");
    }
}
