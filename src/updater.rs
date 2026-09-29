//! App self-update from GitHub Releases.
//!
//! Release assets must contain the target triple in their name, e.g.
//! `talyxel-sound-v0.2.0-x86_64-pc-windows-msvc.zip`, with the `talyxel` binary inside
//! (see `.github/workflows/release.yml`).

use anyhow::{Context, Result};
use self_update::{backends::github, cargo_crate_version};
use semver::Version;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use crate::{config::Config, event::AppEvent};

const BIN_NAME: &str = "talyxel";

pub fn current_version() -> &'static str {
    cargo_crate_version!()
}

/// Returns `Some(latest)` when `latest` is a strictly newer semver than `current`.
pub fn newer_version(latest: &str, current: &str) -> Option<String> {
    let parse = |v: &str| Version::parse(v.trim().trim_start_matches('v')).ok();
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) if l > c => Some(l.to_string()),
        _ => None,
    }
}

/// Blocking: asks GitHub for the newest release.
pub fn check(cfg: &Config) -> Result<Option<String>> {
    let releases = github::ReleaseList::configure()
        .repo_owner(&cfg.update_repo_owner)
        .repo_name(&cfg.update_repo_name)
        .build()?
        .fetch()
        .context("fetching GitHub releases")?;
    Ok(releases
        .latest()
        .and_then(|r| newer_version(r.version(), current_version())))
}

/// Blocking: downloads the release asset for this platform and replaces the running binary.
pub fn install(cfg: &Config, interactive: bool) -> Result<Option<String>> {
    let status = github::Update::configure()
        .repo_owner(&cfg.update_repo_owner)
        .repo_name(&cfg.update_repo_name)
        .bin_name(BIN_NAME)
        .current_version(current_version())
        .show_output(interactive)
        .show_download_progress(interactive)
        .no_confirm(!interactive)
        .build()?
        .update()
        .context("installing update")?;
    Ok(status.is_updated().then(|| status.version().to_string()))
}

/// Background startup check; reports through the app event channel.
pub fn spawn_check(cfg: Config, tx: UnboundedSender<AppEvent>) {
    if !cfg.check_updates {
        return;
    }
    tokio::task::spawn_blocking(move || match check(&cfg) {
        Ok(Some(v)) => {
            info!("update available: v{v}");
            let _ = tx.send(AppEvent::UpdateAvailable(v));
        }
        Ok(None) => info!("Talyxel Sound is up to date"),
        Err(e) => warn!("update check failed: {e:#}"),
    });
}

/// Installs from inside the TUI (no stdout output).
pub fn spawn_install(cfg: Config, tx: UnboundedSender<AppEvent>) {
    tokio::task::spawn_blocking(move || match install(&cfg, false) {
        Ok(Some(v)) => {
            let _ = tx.send(AppEvent::UpdateInstalled(v));
        }
        Ok(None) => {
            let _ = tx.send(AppEvent::Status("Already up to date".into()));
        }
        Err(e) => {
            let _ = tx.send(AppEvent::Error(format!("Update failed: {e:#}")));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(newer_version("v0.2.0", "0.1.0").as_deref(), Some("0.2.0"));
        assert_eq!(newer_version("0.1.1", "0.1.0").as_deref(), Some("0.1.1"));
        assert_eq!(newer_version("0.1.0", "0.1.0"), None);
        assert_eq!(newer_version("0.0.9", "0.1.0"), None);
        assert_eq!(newer_version("0.2.0-beta.1", "0.2.0"), None);
        assert_eq!(newer_version("garbage", "0.1.0"), None);
    }
}
