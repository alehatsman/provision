//! Facts about the local machine. Spec §5. Computed once, read-only.
//!
//! There are no user-declared facts. Anything else is a variable.

use minijinja::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct Facts(BTreeMap<String, Value>);

impl Facts {
    pub(crate) fn detect() -> Facts {
        let mut f: BTreeMap<String, Value> = BTreeMap::new();
        let mut set = |k: &str, v: Value| {
            f.insert(k.to_string(), v);
        };

        // Spec §5: a build says what it is. `android` is its own value and
        // not `linux`, because the difference is the whole point — Termux's
        // apt runs as the app user and there is no sudo, so a plan's
        // `when: os == "linux"` steps are exactly the ones that must not run
        // there. Reported as `unknown` before, which skipped every gate
        // silently and read as a broken plan rather than an unnamed platform.
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "android") {
            "android"
        } else {
            "unknown"
        };
        set("os", Value::from(os));
        set("arch", Value::from(std::env::consts::ARCH));
        set("hostname", Value::from(hostname()));
        set("username", Value::from(username()));
        set(
            "home",
            Value::from(
                home::home_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
        );

        let (distro, distro_version) = os_release();
        set("distro", Value::from(distro));
        set("distro_version", Value::from(distro_version));
        set("is_wsl", Value::from(is_wsl()));
        set("is_termux", Value::from(is_termux()));

        for bin in ["apt", "pacman", "brew", "winget", "yay"] {
            set(&format!("{bin}_available"), Value::from(on_path(bin)));
        }
        set("systemd_available", Value::from(systemd_available()));
        set(
            "launchd_available",
            Value::from(os == "darwin" && on_path("launchctl")),
        );

        Facts(f)
    }

    /// Only the tests read a single fact by name; the CLI iterates.
    #[cfg(test)]
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }
}

fn hostname() -> String {
    // Short name, as the kernel reports it: everything before the first dot.
    let full = gethostname::gethostname().to_string_lossy().into_owned();
    full.split('.').next().unwrap_or(&full).to_string()
}

fn username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

fn on_path(bin: &str) -> bool {
    which::which(bin).is_ok()
}

/// `/etc/os-release` `ID` and `VERSION_ID`. Empty off Linux.
fn os_release() -> (String, String) {
    let Ok(text) = std::fs::read_to_string("/etc/os-release") else {
        return (String::new(), String::new());
    };
    let mut id = String::new();
    let mut version = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "ID" => id = v,
            "VERSION_ID" => version = v,
            _ => {}
        }
    }
    (id, version)
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .is_ok_and(|v| v.to_ascii_lowercase().contains("microsoft"))
}

/// Termux, which is an Android app and not a distribution: no `/etc`, no
/// sudo, and an `apt` that runs as the app user.
///
/// Asked of the environment rather than of a file, because that is where
/// Termux says so: the app exports `TERMUX_VERSION`, and `PREFIX` points
/// inside its private directory. Either answers; a login shell that dropped
/// one of them still has the other.
fn is_termux() -> bool {
    termux_from(
        std::env::var("TERMUX_VERSION").ok().as_deref(),
        std::env::var("PREFIX").ok().as_deref(),
    )
}

fn termux_from(version: Option<&str>, prefix: Option<&str>) -> bool {
    version.is_some_and(|v| !v.is_empty()) || prefix.is_some_and(|p| p.contains("com.termux"))
}

/// systemd is the init system, not merely installed: pid 1 must be systemd.
/// Under WSL without `systemd=true` the binary exists but nothing runs it,
/// which is exactly the case `platforms/windows/index.yml` gates on.
fn systemd_available() -> bool {
    std::path::Path::new("/run/systemd/system").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fact_in_the_spec_is_present() {
        let f = Facts::detect();
        for k in [
            "os",
            "arch",
            "hostname",
            "username",
            "home",
            "distro",
            "distro_version",
            "is_wsl",
            "is_termux",
            "apt_available",
            "pacman_available",
            "brew_available",
            "winget_available",
            "yay_available",
            "systemd_available",
            "launchd_available",
        ] {
            assert!(f.get(k).is_some(), "missing fact `{k}`");
        }
    }

    // Termux is asked of the environment, so the detection is a pure
    // function over it: a test that set the variables would be changing the
    // process every other test runs in.
    #[test]
    fn termux_is_read_from_the_environment_termux_sets() {
        assert!(termux_from(Some("0.118.0"), None));
        assert!(termux_from(None, Some("/data/data/com.termux/files/usr")));
        assert!(!termux_from(None, Some("/usr/local")));
        assert!(!termux_from(None, None));
        // An exported-but-empty `TERMUX_VERSION` is not an answer.
        assert!(!termux_from(Some(""), None));
    }

    #[test]
    fn hostname_is_short() {
        let f = Facts::detect();
        let h = f.get("hostname").unwrap().to_string();
        assert!(!h.contains('.'), "hostname `{h}` is not short");
    }
}
