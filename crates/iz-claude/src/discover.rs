//! Finding the `claude` binary. A Finder-launched app gets a minimal PATH,
//! so beyond PATH we ask a login shell and then try the usual install spots.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve the binary: `settings.claude_bin`, then `IZ_CLAUDE_BIN`, then the
/// cached result of discovery (PATH, login shell, common paths). Explicit
/// settings are checked fresh on every call; only discovery is cached.
pub fn resolve(
    settings_bin: Option<&str>,
    cache: &Mutex<Option<PathBuf>>,
) -> Result<PathBuf, String> {
    if let Some(bin) = settings_bin.map(str::trim).filter(|s| !s.is_empty()) {
        return explicit(bin, "settings.claude_bin");
    }
    if let Ok(bin) = std::env::var(iz_types::env::CLAUDE_BIN) {
        if !bin.trim().is_empty() {
            return explicit(bin.trim(), iz_types::env::CLAUDE_BIN);
        }
    }
    let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = cached.as_ref().filter(|p| p.is_file()) {
        return Ok(p.clone());
    }
    let found = discover()?;
    *cached = Some(found.clone());
    Ok(found)
}

fn explicit(bin: &str, source: &str) -> Result<PathBuf, String> {
    let path = expand_tilde(bin);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{source} is set to {bin:?}, but there is no file at {}",
            path.display()
        ))
    }
}

fn discover() -> Result<PathBuf, String> {
    if let Some(p) = search_path(std::env::var_os("PATH")) {
        return Ok(p);
    }
    for shell in login_shells() {
        if let Some(p) = ask_login_shell(&shell) {
            return Ok(p);
        }
    }
    let common = common_paths();
    if let Some(p) = common.iter().find(|p| p.is_file()) {
        return Ok(p.clone());
    }
    let listed: Vec<String> = common.iter().map(|p| p.display().to_string()).collect();
    Err(format!(
        "could not find the `claude` CLI. Looked on PATH, in a login shell (zsh -lc 'command -v claude'), and at {}. Install Claude Code, or set {} or settings.claude_bin to its path.",
        listed.join(", "),
        iz_types::env::CLAUDE_BIN
    ))
}

/// The equivalent of `which claude` over the given PATH value.
fn search_path(path: Option<OsString>) -> Option<PathBuf> {
    std::env::split_paths(&path?)
        .map(|dir| dir.join("claude"))
        .find(|p| is_executable(p))
}

fn login_shells() -> Vec<PathBuf> {
    let mut shells = vec![PathBuf::from("/bin/zsh")];
    if let Some(sh) = std::env::var_os("SHELL").map(PathBuf::from) {
        if !shells.contains(&sh) && sh.is_absolute() {
            shells.push(sh);
        }
    }
    shells.retain(|s| s.is_file());
    shells
}

/// `<shell> -lc 'command -v claude'`, bounded by a timeout. Login rc files
/// may print banners, so the last line that names an existing file wins.
fn ask_login_shell(shell: &Path) -> Option<PathBuf> {
    let mut child = Command::new(shell)
        .args(["-lc", "command -v claude"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < LOGIN_SHELL_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    parse_command_v(&String::from_utf8_lossy(&out.stdout))
}

fn parse_command_v(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

fn common_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = home_dir() {
        v.push(home.join(".claude/local/claude"));
        v.push(home.join(".local/bin/claude"));
        v.push(home.join(".npm-global/bin/claude"));
        v.push(home.join(".bun/bin/claude"));
    }
    v.push(PathBuf::from("/opt/homebrew/bin/claude"));
    v.push(PathBuf::from("/usr/local/bin/claude"));
    v
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn expand_tilde(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_setting_must_exist() {
        let cache = Mutex::new(None);
        let err = resolve(Some("/definitely/not/here/claude"), &cache).unwrap_err();
        assert!(err.contains("settings.claude_bin"), "{err}");
        let me = std::env::current_exe().unwrap();
        assert_eq!(resolve(Some(me.to_str().unwrap()), &cache).unwrap(), me);
        assert!(
            cache.lock().unwrap().is_none(),
            "explicit settings are not cached"
        );
    }

    #[test]
    fn command_v_output_tolerates_banners() {
        let me = std::env::current_exe().unwrap();
        let out = format!("Welcome!\n{}\n\n", me.display());
        assert_eq!(parse_command_v(&out), Some(me));
        assert_eq!(parse_command_v("claude: aliased to foo\n"), None);
        assert_eq!(parse_command_v("/no/such/file\n"), None);
    }

    #[test]
    fn path_search_finds_executables_only() {
        let dir = std::env::temp_dir().join(format!("iz-claude-which-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                search_path(Some(dir.clone().into_os_string())),
                None,
                "not executable yet"
            );
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([PathBuf::from("/nonexistent"), dir.clone()]).unwrap();
        assert_eq!(search_path(Some(path)), Some(bin));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tilde_expands() {
        if let Some(home) = home_dir() {
            assert_eq!(expand_tilde("~/x/claude"), home.join("x/claude"));
        }
        assert_eq!(expand_tilde("/abs/claude"), PathBuf::from("/abs/claude"));
    }
}
