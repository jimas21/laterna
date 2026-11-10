//! Settings › Earbuds: installs Tragus, the earbuds companion, on this
//! headset. Laterna only fetches the pinned installer, checks its hash and
//! runs it; the installer verifies the signed packages it downloads, and
//! Tragus updates itself afterwards.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The Tragus release this build installs.
pub const TAG: &str = "2.0.12-frame.10";
/// SHA-256 of `magicpods-frame-install.py` in [`TAG`]'s release. Raise both
/// together.
pub const SCRIPT_SHA256: &str = "afb33fa6224c3a27893efa253d1a61dd3bdd1d443c4ecd403db67eec0fd6b257";

fn script_url() -> String {
    format!("https://github.com/jimas21/tragus/releases/download/{TAG}/magicpods-frame-install.py")
}

/// The installer's record of what is installed: `{"core": tag, "menu": tag}`.
fn versions_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/magicpods-app/versions.json"))
}

/// The installed Tragus version, or `None` when it is not installed (a file
/// read, no network).
pub fn installed() -> Option<String> {
    let text = std::fs::read_to_string(versions_file()?).ok()?;
    Some(parse_version(&text).unwrap_or_default())
}

/// The version in `versions.json` (the core's, else the menu's).
pub fn parse_version(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    ["core", "menu"]
        .iter()
        .find_map(|k| v.get(k)?.as_str())
        .map(str::to_string)
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// Working, with the installer's latest message.
    Running(String),
    Updated,
    UpToDate,
    Failed(String),
}

/// The install in progress or last finished, shared with its worker thread.
#[derive(Default)]
pub struct Tragus {
    phase: Arc<Mutex<Phase>>,
}

impl Tragus {
    pub fn phase(&self) -> Phase {
        self.phase.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Starts installing on a worker thread, unless one is running.
    pub fn start(&self, ctx: &egui::Context) {
        {
            let Ok(mut p) = self.phase.lock() else { return };
            if matches!(*p, Phase::Running(_)) {
                return;
            }
            *p = Phase::Running(String::new());
        }
        let phase = self.phase.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("tragus-install".into())
            .spawn(move || {
                let set = |next: Phase| {
                    if let Ok(mut p) = phase.lock() {
                        *p = next;
                    }
                    ctx.request_repaint();
                };
                let end = run(&|msg| set(Phase::Running(msg)));
                set(end);
            });
        if let Err(e) = spawned {
            if let Ok(mut p) = self.phase.lock() {
                *p = Phase::Failed(e.to_string());
            }
        }
    }
}

/// The installer's status file: `{state, message, time}`.
#[derive(Debug, PartialEq, Eq)]
pub struct Status {
    pub state: String,
    pub message: String,
}

pub fn parse_status(text: &str) -> Option<Status> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Status {
        state: v.get("state")?.as_str()?.to_string(),
        message: v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// Refuses `bytes` unless their SHA-256 is `expected` (lowercase hex).
pub fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut got = String::with_capacity(64);
    for b in digest.as_ref() {
        got.push_str(&format!("{b:02x}"));
    }
    if got == expected {
        Ok(())
    } else {
        Err(format!(
            "The Tragus installer did not match its expected checksum, so it was not run. \
             Expected {expected}, got {got}."
        ))
    }
}

fn download() -> Result<Vec<u8>, String> {
    let mut resp = fp_updater::default_agent()
        .get(&script_url())
        .call()
        .map_err(|e| format!("Could not download the Tragus installer: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!(
            "Could not download the Tragus installer (HTTP {status})."
        ));
    }
    resp.body_mut()
        .read_to_vec()
        .map_err(|e| format!("Could not download the Tragus installer: {e}"))
}

/// Downloads, checks and runs the installer; the result for the screen.
fn run(progress: &dyn Fn(String)) -> Phase {
    match run_inner(progress) {
        Ok(p) => p,
        Err(e) => Phase::Failed(e),
    }
}

fn run_inner(progress: &dyn Fn(String)) -> Result<Phase, String> {
    let bytes = download()?;
    verify(&bytes, SCRIPT_SHA256)?;
    let dir = fp_core::dirs::cache_dir().join("tragus");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The folder holds a script that is about to run: owner only.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let script = dir.join("install.py");
    let status_file = dir.join("status.json");
    let _ = std::fs::remove_file(&status_file);
    std::fs::write(&script, &bytes).map_err(|e| format!("Cannot write the installer: {e}"))?;
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .args(["--if-newer", "--status-file"])
        .arg(&status_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot run python3: {e}"))?;
    let read = || {
        std::fs::read_to_string(&status_file)
            .ok()
            .and_then(|t| parse_status(&t))
    };
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) => {
                if let Some(s) = read() {
                    progress(s.message);
                }
                std::thread::sleep(Duration::from_millis(400));
            }
            Err(e) => return Err(format!("Lost the installer: {e}")),
        }
    };
    let last = read();
    let _ = std::fs::remove_file(&script);
    let message = last.as_ref().map(|s| s.message.clone()).unwrap_or_default();
    if code != Some(0) || last.as_ref().is_some_and(|s| s.state == "failed") {
        return Err(if message.is_empty() {
            format!("The installer stopped (exit code {}).", code.unwrap_or(-1))
        } else {
            message
        });
    }
    Ok(match last.map(|s| s.state).as_deref() {
        Some("updated") => Phase::Updated,
        _ => Phase::UpToDate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_hash_is_refused() {
        assert!(verify(b"hello", SCRIPT_SHA256).is_err());
        let hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify(b"hello", hello).is_ok());
    }

    #[test]
    fn the_status_file_parses() {
        let s = parse_status(r#"{"state": "updated", "message": "Updated to x", "time": 5}"#);
        assert_eq!(
            s,
            Some(Status {
                state: "updated".into(),
                message: "Updated to x".into()
            })
        );
        assert_eq!(parse_status("not json"), None);
        let v = parse_version(r#"{"core": "2.0.12-frame.10", "menu": "2.0.12-frame.10"}"#);
        assert_eq!(v.as_deref(), Some("2.0.12-frame.10"));
        assert_eq!(parse_status(r#"{"message": "no state"}"#), None);
    }
}
