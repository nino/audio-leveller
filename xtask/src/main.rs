//! Build tasks: assemble the app bundles, sign them, make a disk image.
//!
//! Hand-rolled rather than `cargo-bundle`, which has not shipped since 2023 and
//! knows nothing about notarisation, entitlements, the hardened runtime or
//! universal binaries — all four of which this project's release workflow
//! already does by hand. What it wanted was the assembly step, and that is a
//! hundred lines.
//!
//! `cargo xtask bundle` puts the apps in `target/bundle`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// One app: what to build, and what to call the thing that comes out.
struct App {
    /// Cargo package.
    package: &'static str,
    /// The binary that package produces.
    binary: &'static str,
    /// The bundle's name, spaces and all. Also what the executable inside is
    /// renamed to, since that is the name the Dock, the force-quit dialog and
    /// Activity Monitor all show.
    display_name: &'static str,
    identifier: &'static str,
    /// The `.icns` in `build/`, if there is one.
    icon: Option<&'static str>,
    /// Whether the app opens `.wav` files, which is what puts it in the Open
    /// With menu and lets a file be dropped on its Dock icon.
    opens_wav: bool,
}

const APPS: &[App] = &[App {
    package: "audio-leveller-app",
    binary: "audio-leveller-app",
    display_name: "Audio Leveller",
    identifier: "com.ninoan.audioleveller",
    icon: Some("icon.icns"),
    opens_wav: true,
}];

const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  bundle [--debug]        assemble the .app bundles into target/bundle
  sign <identity>         codesign the bundles with the hardened runtime
  dmg                     build a disk image from the bundles
  verify                  check the signatures and the notarisation";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bundle") => {
            let release = args.next().as_deref() != Some("--debug");
            for app in APPS {
                let path = bundle(app, release)?;
                println!("{}", path.display());
            }
            Ok(())
        }
        Some("sign") => {
            let identity = args
                .next()
                .context("sign needs a signing identity, as codesign spells it")?;
            for app in APPS {
                sign(&bundle_path(app), &identity)?;
                println!("signed {}", bundle_path(app).display());
            }
            Ok(())
        }
        Some("dmg") => {
            let path = dmg()?;
            println!("{}", path.display());
            Ok(())
        }
        Some("verify") => {
            for app in APPS {
                verify(&bundle_path(app))?;
            }
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

fn root() -> PathBuf {
    // The manifest directory is `xtask`, so the repository is its parent.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent")
        .to_path_buf()
}

fn bundle_root() -> PathBuf {
    root().join("target/bundle")
}

fn bundle_path(app: &App) -> PathBuf {
    bundle_root().join(format!("{}.app", app.display_name))
}

/// The version from the workspace manifest, so the bundle and the crate cannot
/// disagree — the release workflow checks the git tag against one of them.
fn version() -> Result<String> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml"))?;
    manifest
        .lines()
        .skip_while(|line| !line.starts_with("[workspace.package]"))
        .find_map(|line| line.strip_prefix("version = "))
        .map(|value| value.trim().trim_matches('"').to_string())
        .context("no version in [workspace.package]")
}

fn bundle(app: &App, release: bool) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    let mut build = Command::new("cargo");
    build.current_dir(root()).args(["build", "-p", app.package]);
    if release {
        build.arg("--release");
    }
    let status = build.status().context("running cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }

    let bundle = bundle_path(app);
    // Start from nothing, so a file removed from the app does not survive in a
    // stale bundle and get signed into the next release.
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)?;
    }
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;

    let built = root().join("target").join(profile).join(app.binary);
    std::fs::copy(&built, contents.join("MacOS").join(app.display_name))
        .with_context(|| format!("copying {}", built.display()))?;

    if let Some(icon) = app.icon {
        let source = root().join("build").join(icon);
        if source.exists() {
            std::fs::copy(&source, contents.join("Resources").join("icon.icns"))?;
        }
    }

    std::fs::write(contents.join("Info.plist"), info_plist(app, &version()?)?)?;
    // Classic, and still read by some of the system: the four-character type
    // and creator codes.
    std::fs::write(contents.join("PkgInfo"), "APPL????")?;

    Ok(bundle)
}

fn info_plist(app: &App, version: &str) -> Result<String> {
    let document_types = if app.opens_wav {
        r#"
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>WAVE audio</string>
			<key>CFBundleTypeRole</key>
			<string>Editor</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>com.microsoft.waveform-audio</string>
				<string>public.wav</string>
			</array>
		</dict>
	</array>"#
    } else {
        ""
    };

    let icon = if app.icon.is_some() {
        "\n\t<key>CFBundleIconFile</key>\n\t<string>icon</string>"
    } else {
        ""
    };

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>{name}</string>
	<key>CFBundleIdentifier</key>
	<string>{identifier}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{name}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>{icon}
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.music</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
	<key>NSHumanReadableCopyright</key>
	<string>MIT licensed. Everything runs on your machine; nothing is uploaded.</string>{document_types}
</dict>
</plist>
"#,
        identifier = app.identifier,
        name = app.display_name,
    ))
}

fn sign(bundle: &Path, identity: &str) -> Result<()> {
    if !bundle.exists() {
        bail!(
            "{} is not there — run `cargo xtask bundle` first",
            bundle.display()
        );
    }
    // No `--deep`: it is deprecated and signs nested code in an order that
    // produces valid-looking but unnotarisable bundles. With a statically
    // linked binary there is nothing nested anyway.
    let entitlements = root().join("build/entitlements.plist");
    let mut command = Command::new("codesign");
    command.args([
        "--force",
        "--options",
        "runtime",
        "--timestamp",
        "--sign",
        identity,
    ]);
    if entitlements.exists() {
        command.arg("--entitlements").arg(&entitlements);
    }
    command.arg(bundle);

    let status = command.status().context("running codesign")?;
    if !status.success() {
        bail!("codesign failed");
    }
    Ok(())
}

fn dmg() -> Result<PathBuf> {
    let output = root().join("target/Audio Leveller.dmg");
    if output.exists() {
        std::fs::remove_file(&output)?;
    }
    let status = Command::new("hdiutil")
        .args(["create", "-volname", "Audio Leveller", "-srcfolder"])
        .arg(bundle_root())
        .args(["-ov", "-format", "UDZO"])
        .arg(&output)
        .status()
        .context("running hdiutil")?;
    if !status.success() {
        bail!("hdiutil failed");
    }
    Ok(output)
}

fn verify(bundle: &Path) -> Result<()> {
    for (program, args) in [
        ("codesign", vec!["--verify", "--strict", "--verbose=2"]),
        (
            "spctl",
            vec!["--assess", "--type", "execute", "--verbose=2"],
        ),
    ] {
        let status = Command::new(program)
            .args(&args)
            .arg(bundle)
            .status()
            .with_context(|| format!("running {program}"))?;
        if !status.success() {
            bail!("{program} rejected {}", bundle.display());
        }
    }
    println!("{} verifies", bundle.display());
    Ok(())
}
