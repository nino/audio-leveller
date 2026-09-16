//! Audio Leveller.

mod access;
mod draw;
mod layout;
#[cfg(target_os = "macos")]
mod shell;

fn main() {
    // `--shot <file>` draws the window into a PNG and exits, which is how the
    // interface is looked at on a machine that cannot take a screenshot — a CI
    // runner, or a shell without the screen-recording permission. It draws the
    // same code the window does.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--shot") => {
            let path = args.next().unwrap_or_else(|| "audio-leveller.png".into());
            shell::shot(std::path::Path::new(&path)).expect("writing the shot");
            println!("wrote {path}");
            return;
        }
        // The same, but of a window that has actually done something: a file
        // processed, a stage opened, a parameter moved. Every state the drawing
        // has a branch for, in one image.
        Some("--shot-busy") => {
            let path = args
                .next()
                .unwrap_or_else(|| "audio-leveller-busy.png".into());
            shell::shot_busy(std::path::Path::new(&path)).expect("writing the shot");
            println!("wrote {path}");
            return;
        }
        _ => {}
    }

    shell::run();
}
