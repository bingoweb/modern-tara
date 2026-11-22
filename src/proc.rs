//! Child processes: arguments as a vector (never a shell string), no console
//! window, stdout and stderr drained on their own threads (no deadlock), and a
//! timeout that kills the whole process tree.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long to keep reading after the child exited: a grandchild that
/// inherited the pipe (an `MSBuild` node, a compiler server) may never close it.
const DRAIN_GRACE: Duration = Duration::from_secs(3);

pub struct Cmd {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env_set: Vec<(OsString, OsString)>,
    env_remove: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
}

#[derive(Debug, Default)]
pub struct Output {
    /// `None` when killed or the exit code is unknown.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub ms: u64,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
    /// Last non-empty lines of stderr (else stdout), for a one-line message.
    pub fn tail(&self, lines: usize) -> String {
        let text = if self.stderr.iter().any(|b| !b.is_ascii_whitespace()) { self.stderr_text() } else { self.stdout_text() };
        let kept: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let start = kept.len().saturating_sub(lines);
        kept[start..].join(" | ")
    }
}

impl Cmd {
    pub fn new(program: impl Into<PathBuf>) -> Cmd {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            // Never let rustup install a toolchain a project pins.
            env_set: vec![("RUSTUP_AUTO_INSTALL".into(), "0".into())],
            env_remove: Vec::new(),
            stdin: None,
            timeout: Duration::from_secs(120),
        }
    }
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Cmd {
        self.args.push(a.as_ref().to_os_string());
        self
    }
    pub fn args<I, S>(mut self, items: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(items.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }
    pub fn cwd(mut self, dir: &Path) -> Cmd {
        self.cwd = Some(dir.to_path_buf());
        self
    }
    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Cmd {
        self.env_set.push((key.into(), value.as_ref().to_os_string()));
        self
    }
    pub fn env_remove(mut self, key: &str) -> Cmd {
        self.env_remove.push(key.into());
        self
    }
    pub fn stdin(mut self, data: Vec<u8>) -> Cmd {
        self.stdin = Some(data);
        self
    }
    pub fn timeout(mut self, t: Duration) -> Cmd {
        self.timeout = t;
        self
    }
    /// Total length of the command line, to stay under the Windows limit.
    pub fn line_len(&self) -> usize {
        self.program.as_os_str().len() + self.args.iter().map(|a| a.len() + 3).sum::<usize>()
    }

    pub fn run(self) -> Result<Output, String> {
        let started = Instant::now();
        let mut command = Command::new(&self.program);
        command.args(&self.args).stdin(if self.stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        for key in &self.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &self.env_set {
            command.env(key, value);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        // Its own process group, so a timeout can kill the whole tree.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|e| format!("cannot start {}: {e}", self.program.display()))?;

        let (tx, rx) = mpsc::channel::<(u8, Option<Vec<u8>>)>();
        if let Some(mut out) = child.stdout.take() {
            let tx = tx.clone();
            thread::spawn(move || pump(&mut out, 1, &tx));
        }
        if let Some(mut err) = child.stderr.take() {
            let tx = tx.clone();
            thread::spawn(move || pump(&mut err, 2, &tx));
        }
        drop(tx);
        if let (Some(mut pipe), Some(data)) = (child.stdin.take(), self.stdin) {
            thread::spawn(move || {
                let _ = pipe.write_all(&data);
            });
        }

        let mut output = Output::default();
        let mut open = 2u8;
        let deadline = started + self.timeout;
        let status = loop {
            while let Ok(msg) = rx.try_recv() {
                take(&mut output, &mut open, msg);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(_) => break None,
            }
            if Instant::now() >= deadline {
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                output.timed_out = true;
                break None;
            }
            match rx.recv_timeout(Duration::from_millis(15)) {
                Ok(msg) => take(&mut output, &mut open, msg),
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
            }
        };
        let drain_until = Instant::now() + DRAIN_GRACE;
        while open > 0 {
            let left = drain_until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(msg) => take(&mut output, &mut open, msg),
                Err(_) => break,
            }
        }
        output.code = status.and_then(|s| s.code());
        output.ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(output)
    }
}

fn pump(reader: &mut impl Read, stream: u8, tx: &mpsc::Sender<(u8, Option<Vec<u8>>)>) {
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send((stream, Some(buf[..n].to_vec()))).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = tx.send((stream, None));
}

fn take(output: &mut Output, open: &mut u8, (stream, data): (u8, Option<Vec<u8>>)) {
    match data {
        Some(bytes) if stream == 1 => output.stdout.extend_from_slice(&bytes),
        Some(bytes) => output.stderr.extend_from_slice(&bytes),
        None => *open = open.saturating_sub(1),
    }
}

/// The child and everything it started; `Child::kill` alone leaves
/// grandchildren (rustc, `MSBuild` nodes) running.
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let _ = Cmd::new(system.join("System32").join("taskkill.exe")).args(["/PID", &pid.to_string(), "/T", "/F"]).timeout(Duration::from_secs(10)).run();
    }
    #[cfg(unix)]
    {
        // The child leads its own process group (`process_group(0)` at spawn).
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
    }
}

/// An executable on PATH (`.exe` only: batch shims would need a shell), else
/// the first fallback that exists.
pub fn which(name: &str, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    let has_exe = Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    let file = if cfg!(windows) && !has_exe { format!("{name}.exe") } else { name.to_string() };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.as_os_str().is_empty() || !dir.is_absolute() {
                continue;
            }
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    fallbacks.iter().find(|p| p.is_file()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn timeout_kills_and_reports() {
        let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let ping = system.join("System32").join("ping.exe");
        let out = Cmd::new(ping).args(["-n", "30", "127.0.0.1"]).timeout(Duration::from_millis(500)).run().unwrap();
        assert!(out.timed_out);
        assert!(out.ms < 10_000);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_process_group() {
        let dir = std::env::temp_dir().join(format!("mt-pgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let out = Cmd::new("sh").args(["-c", script.as_str()]).timeout(Duration::from_millis(500)).run().unwrap();
        assert!(out.timed_out);
        // A surviving grandchild would hold the pipes open for DRAIN_GRACE.
        assert!(out.ms < 2_500, "{} ms", out.ms);
        let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
        let alive = || Command::new("kill").args(["-0", pid.as_str()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "grandchild {pid} survived the timeout");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        assert!(Cmd::new("definitely-not-a-program-modern-tara").run().is_err());
    }
}
