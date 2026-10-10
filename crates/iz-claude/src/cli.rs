//! Running `claude -p` as a child process: arguments, a scrubbed
//! environment, stdin/stdout plumbing, cancellation and a timeout.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Enough for the image Reads (ideally one turn) and the structured answer,
/// with room for a schema-validation retry; not enough to wander.
pub const MAX_TURNS: u32 = 8;
const POLL: Duration = Duration::from_millis(100);

/// Command-line arguments after the binary. The prompt goes on stdin.
pub fn args(model: &str, system_prompt: &str, schema: &str) -> Vec<String> {
    let max_turns = MAX_TURNS.to_string();
    [
        "-p",
        "--model",
        model,
        "--output-format",
        "json",
        "--json-schema",
        schema,
        "--system-prompt",
        system_prompt,
        // Only Read exists in the child, it is pre-approved, and anything
        // else is denied without a prompt (the user's own settings may
        // default to bypassPermissions; this overrides that).
        "--tools",
        "Read",
        "--allowedTools",
        "Read",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--max-turns",
        &max_turns,
        // No session files per frame, no MCP servers spun up, no skills.
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// True for variables a parent Claude Code session sets that would confuse
/// a nested `claude`.
pub fn is_parent_session_var(key: &str) -> bool {
    key == "CLAUDECODE" || key.starts_with("CLAUDE_CODE_")
}

/// Set on the child after scrubbing. The user's own settings may enable the
/// server-side advisor tool (`advisorModel`); in a measured call it was over
/// half the cost of a frame, so the app's one-shot describe calls turn it off.
pub const CHILD_ENV: [(&str, &str); 1] = [("CLAUDE_CODE_DISABLE_ADVISOR_TOOL", "1")];

/// A parent-session variable that is not one the app sets on purpose: if
/// one of these reaches the child, scrubbing failed.
pub fn is_leaked_session_var(key: &str) -> bool {
    is_parent_session_var(key) && !CHILD_ENV.iter().any(|(k, _)| *k == key)
}

/// The child command: working directory, scrubbed environment, and the
/// binary's own directory first on PATH (an npm-installed `claude` is a
/// `#!/usr/bin/env node` script and node usually lives beside it).
pub fn command(bin: &Path, args: &[String], cwd: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args).current_dir(cwd);
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(is_parent_session_var) {
            cmd.env_remove(&key);
        }
    }
    cmd.envs(CHILD_ENV);
    if let Some(dir) = bin.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut parts = vec![dir.to_path_buf()];
        if let Some(old) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&old).filter(|p| p != dir));
        }
        if let Ok(joined) = std::env::join_paths(parts) {
            cmd.env("PATH", joined);
        }
    }
    cmd
}

/// Names of the variables `command` removes from the child's environment.
pub fn stripped_vars(cmd: &Command) -> Vec<OsString> {
    cmd.get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_os_string())
        .collect()
}

#[derive(Debug)]
pub struct Finished {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum RunError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Cancelled(Duration),
    TimedOut(Duration),
}

/// Spawn, feed `stdin_text`, and poll every 100 ms until the child exits,
/// `cancelled()` turns true (the child is killed), or `timeout` passes.
/// Pipes are drained on their own threads so a large reply cannot deadlock.
pub fn run(
    mut cmd: Command,
    stdin_text: &str,
    cancelled: impl Fn() -> bool,
    timeout: Duration,
) -> Result<Finished, RunError> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;

    let stdin = child.stdin.take();
    let text = stdin_text.to_owned();
    let writer = std::thread::spawn(move || {
        if let Some(mut pipe) = stdin {
            // A child that exits early closes the pipe; that is not our error.
            let _ = pipe.write_all(text.as_bytes());
        } // Dropping the pipe sends EOF.
    });
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let status = loop {
        if cancelled() {
            stop(&mut child);
            return Err(RunError::Cancelled(started.elapsed()));
        }
        if started.elapsed() > timeout {
            stop(&mut child);
            return Err(RunError::TimedOut(started.elapsed()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                stop(&mut child);
                return Err(RunError::Wait(e));
            }
        }
    };
    let _ = writer.join();
    Ok(Finished {
        status,
        stdout: join(out),
        stderr: join(err),
        elapsed: started.elapsed(),
    })
}

/// Kill and reap. Reader threads are left to finish on their own: a
/// grandchild could still hold a pipe open and we must not block on it.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut p| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            buf
        })
    })
}

fn join(h: Option<JoinHandle<Vec<u8>>>) -> String {
    h.and_then(|h| h.join().ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_the_contract() {
        let a = args("claude-opus-5-5", "SYS", "{\"type\":\"object\"}");
        let joined = a.join(" ");
        assert_eq!(a[0], "-p");
        for pair in [
            "--model claude-opus-5-5",
            "--output-format json",
            "--json-schema {\"type\":\"object\"}",
            "--system-prompt SYS",
            "--tools Read",
            "--allowedTools Read",
            "--permission-mode dontAsk",
            "--max-turns 8",
        ] {
            assert!(joined.contains(pair), "missing {pair:?} in {joined}");
        }
    }

    #[test]
    fn parent_session_vars_are_recognised() {
        assert!(is_parent_session_var("CLAUDECODE"));
        assert!(is_parent_session_var("CLAUDE_CODE_ENTRYPOINT"));
        assert!(is_parent_session_var("CLAUDE_CODE_SESSION_ID"));
        assert!(!is_parent_session_var("CLAUDE_CONFIG_DIR"));
        assert!(!is_parent_session_var("ANTHROPIC_API_KEY"));
        assert!(!is_parent_session_var("PATH"));
        assert!(is_leaked_session_var("CLAUDE_CODE_SESSION_ID"));
        assert!(!is_leaked_session_var("CLAUDE_CODE_DISABLE_ADVISOR_TOOL"));
    }

    #[test]
    fn command_strips_inherited_session_vars_and_sets_cwd() {
        let cmd = command(
            Path::new("/opt/x/bin/claude"),
            &["-p".into()],
            Path::new("/tmp"),
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp")));
        let stripped = stripped_vars(&cmd);
        for (key, _) in std::env::vars_os() {
            let k = key.to_string_lossy();
            assert_eq!(stripped.contains(&key), is_leaked_session_var(&k), "{k}");
        }
        let advisor = cmd
            .get_envs()
            .find(|(k, _)| *k == "CLAUDE_CODE_DISABLE_ADVISOR_TOOL");
        assert_eq!(
            advisor.and_then(|(_, v)| v),
            Some(std::ffi::OsStr::new("1"))
        );
        let path = cmd
            .get_envs()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v)
            .unwrap();
        assert!(path.to_string_lossy().starts_with("/opt/x/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn run_feeds_stdin_and_collects_output() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "tr a-z A-Z; echo oops >&2; exit 3"]);
        let f = run(cmd, "hello", || false, Duration::from_secs(10)).unwrap();
        assert_eq!(f.stdout, "HELLO");
        assert_eq!(f.stderr.trim(), "oops");
        assert_eq!(f.status.code(), Some(3));
    }

    #[cfg(unix)]
    #[test]
    fn run_honours_cancel_and_timeout() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            f2.store(true, Ordering::Relaxed);
        });
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("30");
        let started = Instant::now();
        let r = run(
            cmd,
            "",
            || flag.load(Ordering::Relaxed),
            Duration::from_secs(60),
        );
        assert!(matches!(r, Err(RunError::Cancelled(_))), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5));

        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("30");
        let r = run(cmd, "", || false, Duration::from_millis(300));
        assert!(matches!(r, Err(RunError::TimedOut(_))), "{r:?}");
    }
}
