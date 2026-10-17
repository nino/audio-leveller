//! Audio Leveller, as a Tauri app.
//!
//! The shell is small on purpose. `leveller-ui`'s `Model` holds the state and
//! decides what every message means; the webview in `ui/audio-leveller` draws
//! the `View` it produces and sends clicks back as `Input`s. What lives here is
//! what neither of them can do: the window's own events (a file dragged over
//! it, focus), the open panel, files handed over by Finder, and the worker
//! thread a run happens on.

// No console window behind the app on Windows, in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use leveller_pipeline::StageSpec;
use leveller_ui::{Effect, Input, Model, Msg, View};
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::DialogExt;

#[cfg(target_os = "macos")]
mod chrome;

/// How often a running chain repaints its progress. Progress arrives
/// thousands of times a second from inside the DSP, and a view is a few
/// kilobytes of JSON; 20 Hz looks continuous and costs nothing.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

struct Shell {
    model: Mutex<Model>,
}

/// Apply a message, start whatever it asks for, and return what to draw.
fn dispatch(app: &AppHandle, msg: Msg) -> View {
    let shell = app.state::<Shell>();
    let (view, effect) = {
        let mut model = shell.model.lock().expect("the model lock");
        let effect = model.update(msg);
        (model.view(), effect)
    };
    if let Some(Effect::Process { path, stages }) = effect {
        start_worker(app.clone(), path, stages);
    }
    view
}

/// [`dispatch`] for messages that do not come from the webview, which then has
/// to be told.
fn publish(app: &AppHandle, msg: Msg) {
    let view = dispatch(app, msg);
    let _ = app.emit("view", view);
}

fn is_working(app: &AppHandle) -> bool {
    app.state::<Shell>()
        .model
        .lock()
        .expect("the model lock")
        .status()
        .is_working()
}

/// A file to start a run on, unless one is already going: the drop zone is
/// greyed out while it is, and a second run would race the first to the
/// status line.
fn open(app: &AppHandle, path: PathBuf) {
    if !is_working(app) {
        publish(app, Msg::Dropped(path));
    }
}

fn is_wav(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

fn first_wav(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| is_wav(p)).cloned()
}

/// Run the chain on its own thread, publishing progress and then the result.
fn start_worker(app: AppHandle, path: PathBuf, stages: Vec<StageSpec>) {
    std::thread::spawn(move || {
        let registry = leveller_model::registry();
        let mut last: Option<Instant> = None;
        let result = leveller_io::process_file(&path, &stages, &registry, |p| {
            if last.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
                return;
            }
            last = Some(Instant::now());
            publish(
                &app,
                Msg::Progress {
                    stage: p.stage.to_string(),
                    index: p.index,
                    total: p.total,
                    overall: p.overall,
                },
            );
        });
        publish(
            &app,
            Msg::Finished(result.map(Box::new).map_err(|e| e.to_string())),
        );
    });
}

#[tauri::command]
fn view(app: AppHandle) -> View {
    app.state::<Shell>()
        .model
        .lock()
        .expect("the model lock")
        .view()
}

#[tauri::command]
fn send(app: AppHandle, input: Input) -> View {
    dispatch(&app, input.into())
}

/// The open panel. Async, so it runs off the main thread, which the blocking
/// panel needs to stay free.
#[tauri::command]
async fn choose_file(app: AppHandle) -> View {
    let chosen = app
        .dialog()
        .file()
        .set_title("Choose a .wav file to level")
        .add_filter("WAV audio", &["wav", "WAV"])
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok());
    if let Some(path) = chosen {
        open(&app, path);
    }
    view(app)
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Shell {
            model: Mutex::new(Model::new()),
        })
        .invoke_handler(tauri::generate_handler![view, send, choose_file])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                chrome::hide_system_buttons(&window);
            }
            // A file named on the command line, which is also how Windows and
            // Linux hand over a file opened from the file manager. macOS sends
            // those as `RunEvent::Opened` instead.
            let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
            if let Some(path) = first_wav(&paths) {
                open(app.handle(), path);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            let app = window.app_handle();
            match event {
                WindowEvent::DragDrop(DragDropEvent::Enter { paths, .. }) => {
                    let over = first_wav(paths).is_some() && !is_working(app);
                    publish(app, Msg::DragOver(over));
                }
                WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
                    match first_wav(paths) {
                        Some(path) => open(app, path),
                        None => publish(app, Msg::DragOver(false)),
                    }
                }
                WindowEvent::DragDrop(DragDropEvent::Leave) => publish(app, Msg::DragOver(false)),
                WindowEvent::Focused(active) => publish(app, Msg::WindowActive(*active)),
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("building the app");

    app.run(on_run_event);
}

/// Finder's Open With, and a file dropped on the Dock icon.
#[cfg(target_os = "macos")]
fn on_run_event(app: &AppHandle, event: RunEvent) {
    if let RunEvent::Opened { urls } = event {
        let wav = urls
            .iter()
            .filter_map(|url| url.to_file_path().ok())
            .find(|path| is_wav(path));
        if let Some(path) = wav {
            open(app, path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn on_run_event(_app: &AppHandle, _event: RunEvent) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wav_files_start_a_run() {
        let paths = [
            PathBuf::from("/tmp/notes.txt"),
            PathBuf::from("/tmp/talk.WAV"),
            PathBuf::from("/tmp/other.wav"),
        ];
        assert_eq!(first_wav(&paths), Some(PathBuf::from("/tmp/talk.WAV")));
        assert_eq!(first_wav(&paths[..1]), None);
        assert_eq!(first_wav(&[]), None);
    }
}
