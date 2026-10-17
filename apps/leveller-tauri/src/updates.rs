//! Updates from GitHub releases.
//!
//! The app checks once at start-up and again whenever "Check for Updates…" is
//! chosen. A found update is offered in a strip at the top of the window and
//! installed only when the person says so; the app then restarts into it.
//!
//! The feed and the key that signs each update are set under
//! `plugins.updater` in `tauri.conf.json`. The release workflow writes the
//! feed, `latest-audio-leveller.json`, next to the signed `.app.tar.gz` on
//! each release.
//!
//! This is not part of `leveller-ui`'s model: it has nothing to do with
//! levelling, and the AppKit app it would otherwise have to serve has no
//! updater.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_updater::{Update, UpdaterExt};

/// How often a download repaints its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// What the update strip shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UpdateView {
    /// No strip.
    None,
    Available {
        version: String,
        notes: Option<String>,
    },
    /// Downloading, then installing. `fraction` is missing while the size is
    /// not known.
    Installing {
        version: String,
        fraction: Option<f64>,
    },
    Failed {
        message: String,
    },
}

pub struct Updates {
    view: Mutex<UpdateView>,
    /// The update the strip offers, kept to install it from.
    pending: Mutex<Option<Update>>,
}

impl Updates {
    pub fn new() -> Self {
        Self {
            view: Mutex::new(UpdateView::None),
            pending: Mutex::new(None),
        }
    }
}

fn show(app: &AppHandle, view: UpdateView) {
    *app.state::<Updates>().view.lock().expect("the update lock") = view.clone();
    let _ = app.emit("update", view);
}

/// Look for an update. A check the person asked for always answers; the one at
/// start-up stays quiet unless there is something to offer, since being
/// offline is not worth a dialog.
pub async fn check(app: AppHandle, asked: bool) {
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    match found {
        Ok(Some(update)) => {
            let view = UpdateView::Available {
                version: update.version.clone(),
                notes: update.body.clone(),
            };
            *app.state::<Updates>()
                .pending
                .lock()
                .expect("the update lock") = Some(update);
            show(&app, view);
        }
        Ok(None) => {
            if asked {
                let version = app.package_info().version.to_string();
                app.dialog()
                    .message(format!("Audio Leveller {version} is the newest version."))
                    .title("You're up to date")
                    .show(|_| {});
            }
        }
        Err(error) => {
            eprintln!("update check failed: {error}");
            if asked {
                app.dialog()
                    .message(format!("Couldn't check for updates: {error}"))
                    .title("Check for Updates")
                    .kind(MessageDialogKind::Warning)
                    .show(|_| {});
            }
        }
    }
}

#[tauri::command]
pub fn update_view(app: AppHandle) -> UpdateView {
    app.state::<Updates>()
        .view
        .lock()
        .expect("the update lock")
        .clone()
}

/// Download and install the offered update, then restart into it.
#[tauri::command]
pub async fn install_update(app: AppHandle) {
    let Some(update) = app
        .state::<Updates>()
        .pending
        .lock()
        .expect("the update lock")
        .take()
    else {
        return;
    };
    let version = update.version.clone();
    show(
        &app,
        UpdateView::Installing {
            version: version.clone(),
            fraction: None,
        },
    );

    let mut received = 0u64;
    let mut last: Option<Instant> = None;
    let installed = update
        .download_and_install(
            |chunk, total| {
                received += chunk as u64;
                if last.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
                    return;
                }
                last = Some(Instant::now());
                show(
                    &app,
                    UpdateView::Installing {
                        version: version.clone(),
                        fraction: total
                            .filter(|&t| t > 0)
                            .map(|t| (received as f64 / t as f64).min(1.0)),
                    },
                );
            },
            || {},
        )
        .await;

    match installed {
        Ok(()) => app.restart(),
        Err(error) => show(
            &app,
            UpdateView::Failed {
                message: error.to_string(),
            },
        ),
    }
}

/// "Later", or closing a failure.
#[tauri::command]
pub fn dismiss_update(app: AppHandle) {
    app.state::<Updates>()
        .pending
        .lock()
        .expect("the update lock")
        .take();
    show(&app, UpdateView::None);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_strip_serialises_to_what_the_frontend_reads() {
        assert_eq!(
            serde_json::to_value(UpdateView::Installing {
                version: "0.3.0".into(),
                fraction: Some(0.5),
            })
            .unwrap(),
            json!({ "kind": "installing", "version": "0.3.0", "fraction": 0.5 })
        );
        assert_eq!(
            serde_json::to_value(UpdateView::None).unwrap(),
            json!({ "kind": "none" })
        );
    }
}
