//! The blind listening-test and annotation app.

mod access;
mod draw;
mod layout;
mod model;
#[cfg(target_os = "macos")]
mod shell;

fn main() {
    // `--shot <page> <file>` draws a page into a PNG and exits, which is how
    // the interface is looked at on a machine that cannot take a screenshot. It
    // runs the same drawing code the window does.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--shot") {
        let page = args.next().unwrap_or_else(|| "home".into());
        let path = args.next().unwrap_or_else(|| format!("listen-{page}.png"));
        shell::shot(
            std::path::Path::new(&path),
            &page,
            shell::default_store(),
        )
        .expect("writing the shot");
        println!("wrote {path}");
        return;
    }

    shell::run();
}
