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

struct State {
    view: UpdateView,
    /// The update the strip offers. Kept after a failed install, so the strip
    /// can offer it again.
    pending: Option<Update>,
}

/// One lock over both, so deciding to install and marking the install as
/// started happen together: two clicks, or a check landing mid-install, can't
/// start a second download over the first.
pub struct Updates(Mutex<State>);

impl Updates {
    pub fn new() -> Self {
        Self(Mutex::new(State {
            view: UpdateView::None,
            pending: None,
        }))
    }
}

fn state(app: &AppHandle) -> std::sync::MutexGuard<'_, State> {
    app.state::<Updates>()
        .inner()
        .0
        .lock()
        .expect("the update lock")
}

fn emit(app: &AppHandle, view: UpdateView) {
    let _ = app.emit("update", view);
}

fn note(app: &AppHandle, title: &str, message: String, kind: MessageDialogKind) {
    app.dialog()
        .message(message)
        .title(title)
        .kind(kind)
        .show(|_| {});
}

pub fn is_installing(app: &AppHandle) -> bool {
    matches!(state(app).view, UpdateView::Installing { .. })
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
            let installing = {
                let mut state = state(&app);
                // An install may have started while the check was out.
                let installing = matches!(state.view, UpdateView::Installing { .. });
                if !installing {
                    state.view = view.clone();
                    state.pending = Some(update);
                }
                installing
            };
            if !installing {
                emit(&app, view);
            } else if asked {
                note(
                    &app,
                    "Check for Updates",
                    "An update is already being installed.".into(),
                    MessageDialogKind::Info,
                );
            }
        }
        Ok(None) => {
            if asked {
                let version = app.package_info().version.to_string();
                note(
                    &app,
                    "You're up to date",
                    format!("Audio Leveller {version} is the newest version."),
                    MessageDialogKind::Info,
                );
            }
        }
        Err(error) => {
            eprintln!("update check failed: {error}");
            if asked {
                note(
                    &app,
                    "Check for Updates",
                    format!("Couldn't check for updates: {error}"),
                    MessageDialogKind::Warning,
                );
            }
        }
    }
}

#[tauri::command]
pub fn update_view(app: AppHandle) -> UpdateView {
    state(&app).view.clone()
}

/// Download and install the offered update, then restart into it.
///
/// Not while a file is being levelled, since the restart would kill the run
/// and leave a half-written output next to the input. Files are refused while
/// an install is going (see `open` in `main.rs`) for the same reason.
#[tauri::command]
pub async fn install_update(app: AppHandle) {
    if crate::is_working(&app) {
        note(
            &app,
            "A file is being levelled",
            "Install the update once the current file is finished.".into(),
            MessageDialogKind::Info,
        );
        return;
    }
    let (update, view) = {
        let mut state = state(&app);
        if matches!(state.view, UpdateView::Installing { .. }) {
            return;
        }
        let Some(update) = state.pending.clone() else {
            return;
        };
        state.view = UpdateView::Installing {
            version: update.version.clone(),
            fraction: None,
        };
        (update, state.view.clone())
    };
    emit(&app, view);

    let version = update.version.clone();
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
                let view = UpdateView::Installing {
                    version: version.clone(),
                    fraction: total
                        .filter(|&t| t > 0)
                        .map(|t| (received as f64 / t as f64).min(1.0)),
                };
                state(&app).view = view.clone();
                emit(&app, view);
            },
            || {},
        )
        .await;

    match installed {
        Ok(()) => app.restart(),
        Err(error) => {
            let view = UpdateView::Failed {
                message: error.to_string(),
            };
            state(&app).view = view.clone();
            emit(&app, view);
        }
    }
}

/// "Later", or closing a failure. Not during an install, which can't be
/// called back.
#[tauri::command]
pub fn dismiss_update(app: AppHandle) {
    {
        let mut state = state(&app);
        if matches!(state.view, UpdateView::Installing { .. }) {
            return;
        }
        state.view = UpdateView::None;
        state.pending = None;
    }
    emit(&app, UpdateView::None);
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
