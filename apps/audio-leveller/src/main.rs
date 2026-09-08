//! Audio Leveller.

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
    if args.next().as_deref() == Some("--shot") {
        let path = args.next().unwrap_or_else(|| "audio-leveller.png".into());
        shell::shot(std::path::Path::new(&path)).expect("writing the shot");
        println!("wrote {path}");
        return;
    }

    shell::run();
}
