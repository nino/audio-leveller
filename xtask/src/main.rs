//! Build tasks: assemble the app bundles, sign them, make a disk image.
//!
//! Hand-rolled rather than `cargo-bundle`, which has not shipped since 2023 and
//! knows nothing about notarisation, entitlements, the hardened runtime or
//! universal binaries — all four of which this project's release workflow
//! already does by hand. What it wanted was the assembly step, and that is a
//! hundred lines.
//!
//! `cargo xtask bundle` puts the apps in `target/bundle`.
//!
//! The updater is Sparkle. `cargo xtask fetch-sparkle` downloads the framework,
//! and a bundle made with `SPARKLE_PUBLIC_KEY` in the environment carries it,
//! the key and the feed. The release workflow makes one update archive and one
//! appcast per app, and attaches them to the GitHub release; each app's feed
//! is that file on whichever release is the latest.

use std::fs;
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
    /// The name its update archive and appcast go by on the release page.
    /// No spaces, since it is part of a URL.
    slug: &'static str,
}

const APPS: &[App] = &[
    App {
        package: "audio-leveller-app",
        binary: "audio-leveller-app",
        display_name: "Audio Leveller",
        identifier: "com.ninoan.audioleveller",
        icon: Some("icon.icns"),
        opens_wav: true,
        slug: "audio-leveller",
    },
    App {
        package: "listen-app",
        binary: "listen-app",
        display_name: "Listen",
        identifier: "com.ninoan.listen",
        icon: None,
        // It reads whole sessions out of a folder rather than one file at a
        // time, so a WAV dropped on its icon would have nowhere to go.
        opens_wav: false,
        slug: "listen",
    },
];

/// Where the releases live: the appcasts and archives are their assets.
const REPOSITORY: &str = "https://github.com/nino/audio-leveller";

/// The oldest macOS the bundles say they run on, and the appcasts with them.
const MINIMUM_SYSTEM: &str = "13.0";

/// The Sparkle release the apps carry, pinned by the archive's hash like the
/// model weights are.
const SPARKLE_VERSION: &str = "2.10.0";
const SPARKLE_SHA256: &str = "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c";

const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  bundle [--debug]        assemble the .app bundles into target/bundle
  sign <identity>         codesign the bundles with the hardened runtime
  dmg                     build a disk image from the bundles
  verify                  check the signatures and the notarisation
  fetch-sparkle           download and verify the Sparkle framework and tools
  sparkle-public-key      print the public key for the private key on stdin
  archive                 zip each bundle as a Sparkle update
  appcast <zip> <sig>     write the appcast for an update archive
  verify-update           check the appcasts' signatures against the bundles' keys
  fetch-model [<id>]      download and verify the DeepFilterNet3 weights
  fetch-fixtures [<id>]   download and verify the evaluation recordings";

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
        Some("fetch-sparkle") => fetch_sparkle(),
        Some("sparkle-public-key") => {
            let mut private = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut private)?;
            println!("{}", public_key(&private)?);
            Ok(())
        }
        Some("archive") => {
            for app in APPS {
                println!("{}", archive(app)?.display());
            }
            Ok(())
        }
        Some("appcast") => {
            let archive = PathBuf::from(args.next().context("appcast needs an update archive")?);
            let signature = args
                .next()
                .context("appcast needs the archive's EdDSA signature")?;
            println!("{}", appcast(app_for(&archive)?, &signature)?.display());
            Ok(())
        }
        Some("verify-update") => {
            for app in APPS {
                verify_update(app)?;
            }
            Ok(())
        }
        Some("fetch-model") => {
            let wanted: Vec<String> = args.collect();
            fetch_models(&wanted)
        }
        Some("fetch-fixtures") => fetch_fixtures(args.next().as_deref()),
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

    // The updater goes in only when there is a key to check updates against,
    // so a bundle made for trying something out stays a plain one.
    let public_key = std::env::var("SPARKLE_PUBLIC_KEY")
        .ok()
        .filter(|key| !key.is_empty());
    if public_key.is_some() {
        let framework = sparkle_root().join("Sparkle.framework");
        if !framework.exists() {
            bail!(
                "SPARKLE_PUBLIC_KEY is set but there is no Sparkle — run `cargo xtask fetch-sparkle`"
            );
        }
        let frameworks = contents.join("Frameworks");
        std::fs::create_dir_all(&frameworks)?;
        // ditto, because the framework is versioned through symlinks that a
        // plain copy would turn into duplicates and the signature would reject.
        run(
            "ditto",
            &[
                &framework.to_string_lossy(),
                &frameworks.join("Sparkle.framework").to_string_lossy(),
            ],
        )?;
    }

    std::fs::write(
        contents.join("Info.plist"),
        info_plist(app, &version()?, public_key.as_deref())?,
    )?;
    // Classic, and still read by some of the system: the four-character type
    // and creator codes.
    std::fs::write(contents.join("PkgInfo"), "APPL????")?;

    Ok(bundle)
}

fn info_plist(app: &App, version: &str, public_key: Option<&str>) -> Result<String> {
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

    // The feed is the appcast on the latest release, which GitHub serves at a
    // URL that never changes.
    let updates = match public_key {
        Some(key) => format!(
            "\n\t<key>SUFeedURL</key>\n\t<string>{}</string>\n\t<key>SUPublicEDKey</key>\n\t<string>{key}</string>",
            feed_url(app)
        ),
        None => String::new(),
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
	<string>{MINIMUM_SYSTEM}</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
	<key>NSHumanReadableCopyright</key>
	<string>MIT licensed. Everything runs on your machine; nothing is uploaded.</string>{updates}{document_types}
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
    // produces valid-looking but unnotarisable bundles. The only nested code is
    // Sparkle, signed here inside out, as its documentation lays out for an app
    // not built by Xcode.
    let framework = bundle.join("Contents/Frameworks/Sparkle.framework");
    if framework.exists() {
        let version = framework.join("Versions/B");
        for (part, preserve_entitlements) in [
            ("XPCServices/Installer.xpc", false),
            // The downloader's sandbox entitlements are what let it reach the
            // network on a sandboxed app's behalf; they have to survive.
            ("XPCServices/Downloader.xpc", true),
            ("Autoupdate", false),
            ("Updater.app", false),
        ] {
            let path = version.join(part);
            if path.exists() {
                codesign(&path, identity, preserve_entitlements.then_some(PRESERVE))?;
            }
        }
        codesign(&framework, identity, None)?;
    }

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

const PRESERVE: &str = "--preserve-metadata=entitlements";

/// Sign one piece of nested code with the hardened runtime and a timestamp.
fn codesign(path: &Path, identity: &str, extra: Option<&str>) -> Result<()> {
    let mut command = Command::new("codesign");
    command.args([
        "--force",
        "--options",
        "runtime",
        "--timestamp",
        "--sign",
        identity,
    ]);
    command.args(extra);
    let status = command.arg(path).status().context("running codesign")?;
    if !status.success() {
        bail!("codesign failed on {}", path.display());
    }
    Ok(())
}

/// The disk image, named for the version and the architecture it was built on,
/// since that is what goes up on the release page.
fn dmg() -> Result<PathBuf> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    let output = root().join(format!("target/Audio Leveller-{}-{arch}.dmg", version()?));
    if output.exists() {
        std::fs::remove_file(&output)?;
    }

    // The bundles plus a link to /Applications, so installing is a drag inside
    // the window that opens. ditto rather than a Rust copy: it keeps the
    // extended attributes a stapled ticket lives in.
    let staging = root().join("target/dmg");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    for app in APPS {
        let bundle = bundle_path(app);
        if !bundle.exists() {
            bail!(
                "{} is not there — run `cargo xtask bundle` first",
                bundle.display()
            );
        }
        let target = staging.join(bundle.file_name().expect("a bundle has a name"));
        run(
            "ditto",
            &[&bundle.to_string_lossy(), &target.to_string_lossy()],
        )?;
    }
    std::os::unix::fs::symlink("/Applications", staging.join("Applications"))?;

    let status = Command::new("hdiutil")
        .args(["create", "-volname", "Audio Leveller", "-srcfolder"])
        .arg(&staging)
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

/// Run a program and fail loudly if it does.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed");
    }
    Ok(())
}

// ---------------------------------------------------------------- sparkle --

fn sparkle_root() -> PathBuf {
    root().join("target/sparkle")
}

/// Download the Sparkle release, check it against the pinned hash and unpack
/// it into `target/sparkle`: the framework the bundles carry, and `bin/` with
/// the tools that sign an update.
fn fetch_sparkle() -> Result<()> {
    let target = sparkle_root();
    let stamp = target.join("VERSION");
    if fs::read_to_string(&stamp).is_ok_and(|v| v == SPARKLE_VERSION)
        && target.join("Sparkle.framework").exists()
    {
        println!("Sparkle {SPARKLE_VERSION}: already in {}", target.display());
        return Ok(());
    }

    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target)?;
    let archive = target.join(format!("Sparkle-{SPARKLE_VERSION}.tar.xz"));
    let url = format!(
        "https://github.com/sparkle-project/Sparkle/releases/download/{SPARKLE_VERSION}/Sparkle-{SPARKLE_VERSION}.tar.xz"
    );
    println!("Sparkle {SPARKLE_VERSION}: fetching {url}");
    run(
        "curl",
        &[
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--output",
            &archive.to_string_lossy(),
            &url,
        ],
    )?;

    let digest = leveller_model::registry::sha256(&fs::read(&archive)?);
    if digest != SPARKLE_SHA256 {
        let _ = fs::remove_dir_all(&target);
        bail!(
            "Sparkle archive checksum mismatch\n  expected {SPARKLE_SHA256}\n  got      {digest}"
        );
    }

    run(
        "tar",
        &[
            "xf",
            &archive.to_string_lossy(),
            "-C",
            &target.to_string_lossy(),
        ],
    )?;
    fs::remove_file(&archive)?;
    fs::write(&stamp, SPARKLE_VERSION)?;
    println!(
        "Sparkle {SPARKLE_VERSION}: unpacked into {}",
        target.display()
    );
    Ok(())
}

/// The public half of a Sparkle private key, as `SUPublicEDKey` wants it.
///
/// The key is what `generate_keys -x` exports: base64 of the 32-byte seed, or,
/// from keys made before Sparkle 2, of 96 bytes that end in the public key.
/// Deriving it here means the release needs one secret rather than a secret and
/// a copy of its public half that could fall out of step with it.
fn public_key(private: &str) -> Result<String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let bytes = STANDARD
        .decode(private.trim())
        .context("the private key is not base64")?;
    let public: [u8; 32] = match bytes.len() {
        32 => {
            let seed: [u8; 32] = bytes.try_into().expect("checked the length");
            ed25519_dalek::SigningKey::from_bytes(&seed)
                .verifying_key()
                .to_bytes()
        }
        96 => bytes[64..].try_into().expect("checked the length"),
        n => bail!(
            "the private key is {n} bytes; Sparkle's are 32, or 96 in the old format — \
             export it with `generate_keys -x`"
        ),
    };
    Ok(STANDARD.encode(public))
}

/// The appcast on the latest release. GitHub redirects this to the asset of
/// that name on whichever release is newest, so the URL baked into a bundle
/// keeps working for every release after it.
fn feed_url(app: &App) -> String {
    format!(
        "{REPOSITORY}/releases/latest/download/{}",
        appcast_name(app)
    )
}

fn appcast_name(app: &App) -> String {
    format!("appcast-{}.xml", app.slug)
}

fn updates_root() -> PathBuf {
    root().join("target/updates")
}

fn archive_path(app: &App) -> Result<PathBuf> {
    Ok(updates_root().join(format!(
        "{}-{}.zip",
        app.display_name.replace(' ', "-"),
        version()?
    )))
}

/// Which app an update archive, as `archive` named it, belongs to.
fn app_for(archive: &Path) -> Result<&'static App> {
    let wanted = archive.file_name();
    for app in APPS {
        if archive_path(app)?.file_name() == wanted {
            return Ok(app);
        }
    }
    bail!("{} is not an archive `cargo xtask archive` made", archive.display())
}

/// Zip a bundle the way Sparkle wants an update: ditto, with the bundle as the
/// top level and its extended attributes kept, so the stapled ticket goes too.
fn archive(app: &App) -> Result<PathBuf> {
    let bundle = bundle_path(app);
    if !bundle.exists() {
        bail!(
            "{} is not there — run `cargo xtask bundle` first",
            bundle.display()
        );
    }
    fs::create_dir_all(updates_root())?;
    let output = archive_path(app)?;
    if output.exists() {
        fs::remove_file(&output)?;
    }
    run(
        "ditto",
        &[
            "-c",
            "-k",
            "--sequesterRsrc",
            "--keepParent",
            &bundle.to_string_lossy(),
            &output.to_string_lossy(),
        ],
    )?;
    Ok(output)
}

/// Write an app's appcast: one item, the update archive on this version's
/// release, signed. Sparkle only ever wants the newest item, and an older
/// release keeps its own appcast as an asset of its own.
fn appcast(app: &App, signature: &str) -> Result<PathBuf> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let signature = signature.trim();
    // A mangled signature would publish a feed every copy of the app rejects,
    // so it is checked for shape here and against the key by `verify-update`.
    let decoded = STANDARD
        .decode(signature)
        .context("the signature is not base64")?;
    if decoded.len() != 64 {
        bail!("an EdDSA signature is 64 bytes, not {}", decoded.len());
    }

    let archive = archive_path(app)?;
    let length = fs::metadata(&archive)
        .with_context(|| {
            format!(
                "{} is not there — run `cargo xtask archive`",
                archive.display()
            )
        })?
        .len();
    let version = version()?;
    let file = archive
        .file_name()
        .expect("an archive has a name")
        .to_string_lossy();

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>{name}</title>
    <link>{REPOSITORY}</link>
    <item>
      <title>{name} {version}</title>
      <sparkle:version>{version}</sparkle:version>
      <sparkle:shortVersionString>{version}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>{MINIMUM_SYSTEM}</sparkle:minimumSystemVersion>
      <sparkle:fullReleaseNotesLink>{REPOSITORY}/releases/tag/v{version}</sparkle:fullReleaseNotesLink>
      <enclosure url="{REPOSITORY}/releases/download/v{version}/{file}" length="{length}" type="application/octet-stream" sparkle:edSignature="{signature}"/>
    </item>
  </channel>
</rss>
"#,
        name = app.display_name,
    );

    let output = updates_root().join(appcast_name(app));
    fs::write(&output, xml)?;
    Ok(output)
}

/// Check what a copy of the app would check: the appcast's signature over the
/// archive, against the public key in the bundle's own Info.plist. The archive
/// is signed by Sparkle's tool and the key derived here, so this is where a
/// disagreement between the two would show — before anyone downloads it.
fn verify_update(app: &App) -> Result<()> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signature, VerifyingKey};

    let plist = fs::read_to_string(bundle_path(app).join("Contents/Info.plist"))?;
    let key = plist_string(&plist, "SUPublicEDKey").context("the bundle has no SUPublicEDKey")?;
    let feed = plist_string(&plist, "SUFeedURL").context("the bundle has no SUFeedURL")?;
    if feed != feed_url(app) {
        bail!("the bundle's feed is {feed}, not {}", feed_url(app));
    }

    let xml = fs::read_to_string(updates_root().join(appcast_name(app)))?;
    let signature =
        attribute(&xml, "sparkle:edSignature").context("the appcast has no signature")?;
    let length: u64 = attribute(&xml, "length")
        .context("the appcast has no length")?
        .parse()?;
    let url = attribute(&xml, "url").context("the appcast has no enclosure URL")?;

    let archive = archive_path(app)?;
    let name = archive
        .file_name()
        .expect("an archive has a name")
        .to_string_lossy();
    if !url.ends_with(&format!("/{name}")) {
        bail!("the appcast points at {url}, not {name}");
    }
    let bytes = fs::read(&archive)?;
    if bytes.len() as u64 != length {
        bail!(
            "the appcast says {length} bytes; the archive is {}",
            bytes.len()
        );
    }

    let key: [u8; 32] = STANDARD
        .decode(key)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("SUPublicEDKey is not 32 bytes"))?;
    let signature: [u8; 64] = STANDARD
        .decode(signature)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("the signature is not 64 bytes"))?;
    VerifyingKey::from_bytes(&key)?
        .verify_strict(&bytes, &Signature::from_bytes(&signature))
        .context("the signature does not match the key in the bundle")?;

    println!("{name}: signature matches {}'s key", app.display_name);
    Ok(())
}

/// The string after `<key>name</key>` in a plist this file wrote.
fn plist_string<'a>(plist: &'a str, name: &str) -> Option<&'a str> {
    let after = plist.split_once(&format!("<key>{name}</key>"))?.1;
    let value = after.split_once("<string>")?.1;
    Some(value.split_once("</string>")?.0)
}

/// The value of `name="..."` in an appcast this file wrote.
fn attribute<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let after = xml.split_once(&format!(" {name}=\""))?.1;
    Some(after.split_once('"')?.0)
}

// ------------------------------------------------------------ fetch-model --

/// Download the weights the model backend needs, and verify them.
///
/// The archive is checked against its own hash and then every extracted file
/// against its own, because the two catch different things: a corrupted
/// download, and a correct archive with one graph swapped inside it.
///
/// Nothing is bundled with the app. The weights carry their own licences, they
/// are large, and a local-first tool should let the user decide what to
/// download — which is why this is a separate command rather than something the
/// app does on first launch.
fn fetch_models(wanted: &[String]) -> Result<()> {
    use leveller_model::registry::{MODELS, Role, model_path, sha256, verify_model};

    let specs: Vec<_> = MODELS
        .iter()
        .filter(|spec| wanted.is_empty() || wanted.iter().any(|w| w == spec.id))
        .collect();
    if specs.is_empty() {
        bail!(
            "no such model (known: {})",
            MODELS
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for spec in specs {
        let target = model_path(spec);
        if verify_model(spec).is_ok() {
            println!("{}: already present in {}", spec.id, target.display());
            continue;
        }

        println!("{}: fetching {}", spec.id, spec.archive.url);
        println!("  licence: {} — see {}", spec.licence, spec.source);

        let scratch = std::env::temp_dir().join(format!("audio-leveller-model-{}", spec.id));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch)?;
        let archive = scratch.join("model.tar.gz");

        // curl rather than an HTTP crate: it is on every macOS and Linux box,
        // and a build task does not need to carry an async runtime to fetch one
        // file.
        run(
            "curl",
            &[
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
                &archive.to_string_lossy(),
                spec.archive.url,
            ],
        )?;

        let digest = sha256(&fs::read(&archive)?);
        if digest != spec.archive.sha256 {
            bail!(
                "{}: archive checksum mismatch\n  expected {}\n  got      {}",
                spec.id,
                spec.archive.sha256,
                digest
            );
        }

        run(
            "tar",
            &[
                "xzf",
                &archive.to_string_lossy(),
                "-C",
                &scratch.to_string_lossy(),
                &format!("--strip-components={}", spec.archive.strip),
            ],
        )?;

        // Every file verified before anything is installed, so a bad archive
        // never leaves a half-installed model behind.
        for (role, file) in &spec.files {
            let extracted = scratch.join(file.name);
            if !extracted.is_file() {
                bail!("{}: archive has no {}", spec.id, file.name);
            }
            let digest = sha256(&fs::read(&extracted)?);
            if digest != file.sha256 {
                bail!(
                    "{}: {:?} ({}) checksum mismatch\n  expected {}\n  got      {}",
                    spec.id,
                    role,
                    file.name,
                    file.sha256,
                    digest
                );
            }
        }

        fs::create_dir_all(&target)?;
        for (_, file) in &spec.files {
            fs::rename(scratch.join(file.name), target.join(file.name))?;
        }
        let _ = fs::remove_dir_all(&scratch);

        verify_model(spec).map_err(anyhow::Error::msg)?;
        println!("{}: installed into {}", spec.id, target.display());
        let _ = Role::ALL;
    }

    Ok(())
}

// --------------------------------------------------------- fetch-fixtures --

/// A set of real recordings, as `eval/references/manifest.json` describes them.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureSet {
    id: String,
    description: String,
    base_url: String,
    format: String,
    files: Vec<FixtureFile>,
}

#[derive(serde::Deserialize)]
struct FixtureFile {
    name: String,
    role: String,
    sha256: String,
}

#[derive(serde::Deserialize)]
struct Manifest {
    sets: Vec<FixtureSet>,
}

/// Where a file goes depends on the part it plays, and the harness treats the
/// three differently.
///
/// A reference must never land in `eval/fixtures/`, or the harness would enrol
/// somebody else's master as a recording to be mastered — and mastering a
/// master produces numbers that mean nothing.
fn destination(role: &str) -> Result<&'static str> {
    match role {
        // Picked up automatically as `fixture:<name>`.
        "input" => Ok("eval/fixtures"),
        // Never processed; something to compare against.
        "reference" | "archive" => Ok("eval/references"),
        other => bail!("unknown role \"{other}\""),
    }
}

/// Download the real recordings the evaluation harness can be run against.
///
/// The audio is not in the repository — it is hundreds of megabytes and it is
/// someone's actual voice — so what lives there instead is a manifest of URLs
/// and SHA-256 checksums. That is the whole point: a fixture whose bytes can
/// drift silently is worthless as a regression baseline, and "it sounded better
/// on my machine" is not a result anyone can check.
fn fetch_fixtures(wanted: Option<&str>) -> Result<()> {
    use leveller_model::registry::sha256_file;

    let manifest_path = root().join("eval/references/manifest.json");
    let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)
        .with_context(|| format!("reading {}", manifest_path.display()))?;

    let sets: Vec<&FixtureSet> = manifest
        .sets
        .iter()
        .filter(|set| wanted.is_none_or(|id| set.id == id))
        .collect();
    if sets.is_empty() {
        bail!(
            "no set called \"{}\" (available: {})",
            wanted.unwrap_or(""),
            manifest
                .sets
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for set in sets {
        println!("{}: {} files, {}", set.id, set.files.len(), set.format);
        println!("  {}", set.description);

        for file in &set.files {
            let dir = root().join(destination(&file.role)?);
            fs::create_dir_all(&dir)?;
            let target = dir.join(&file.name);

            if target.is_file() {
                let digest = sha256_file(&target)?;
                if digest == file.sha256 {
                    println!("  {}: present and verified", file.name);
                    continue;
                }
                // Not overwritten silently: a mismatch here is either a
                // corrupted download or a file edited in place, and both are
                // worth knowing about before the bytes are replaced.
                bail!(
                    "{}: on disk but does not match the manifest\n  expected {}\n  got      {}\n  \
                     delete it and re-run if you want the pinned version back",
                    file.name,
                    file.sha256,
                    digest
                );
            }

            let url = format!("{}{}", set.base_url, file.name);
            println!("  {}: downloading", file.name);

            // Streamed to a scratch name, verified, then renamed — so an
            // interrupted or corrupt download never appears at the real path
            // where the harness would pick it up as a fixture.
            let scratch = dir.join(format!("{}.partial", file.name));
            let fetched = run(
                "curl",
                &[
                    "--fail",
                    "--location",
                    "--progress-bar",
                    "--output",
                    &scratch.to_string_lossy(),
                    &url,
                ],
            );
            if let Err(error) = fetched {
                let _ = fs::remove_file(&scratch);
                return Err(error);
            }

            let digest = sha256_file(&scratch)?;
            if digest != file.sha256 {
                let _ = fs::remove_file(&scratch);
                bail!(
                    "{}: checksum mismatch\n  expected {}\n  got      {}",
                    file.name,
                    file.sha256,
                    digest
                );
            }
            let size = fs::metadata(&scratch)?.len();
            fs::rename(&scratch, &target)?;
            println!(
                "  {}: verified ({} MB) → {}/",
                file.name,
                size / 1_000_000,
                destination(&file.role)?
            );
        }
    }

    println!("\nRun `cargo run --release --bin leveller-eval` to include them.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8032, section 7.1, test 1: the seed and the public key it makes.
    const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn public_key_from_a_seed() {
        assert_eq!(public_key(&b64(&hex(SEED))).unwrap(), b64(&hex(PUBLIC)));
    }

    #[test]
    fn public_key_ignores_the_newline_a_secret_ends_with() {
        let key = format!("{}\n", b64(&hex(SEED)));
        assert_eq!(public_key(&key).unwrap(), b64(&hex(PUBLIC)));
    }

    #[test]
    fn public_key_from_the_old_format() {
        let mut old = vec![0u8; 64];
        old.extend(hex(PUBLIC));
        assert_eq!(public_key(&b64(&old)).unwrap(), b64(&hex(PUBLIC)));
    }

    #[test]
    fn public_key_rejects_anything_else() {
        assert!(public_key(&b64(&[0u8; 48])).is_err());
        assert!(public_key("not base64!").is_err());
    }

    #[test]
    fn the_plist_carries_the_updater_only_with_a_key() {
        let plain = info_plist(&APPS[0], "1.0", None).unwrap();
        assert!(!plain.contains("SUFeedURL"));

        let keyed = info_plist(&APPS[0], "1.0", Some("KEY")).unwrap();
        assert_eq!(plist_string(&keyed, "SUPublicEDKey"), Some("KEY"));
        assert_eq!(
            plist_string(&keyed, "SUFeedURL"),
            Some(
                "https://github.com/nino/audio-leveller/releases/latest/download/appcast-audio-leveller.xml"
            )
        );
        // Still a single dictionary, so the keys landed inside it.
        assert_eq!(
            keyed.matches("<dict>").count(),
            keyed.matches("</dict>").count()
        );
    }

    #[test]
    fn attribute_reads_the_enclosure() {
        let xml = r#"<enclosure url="https://x/a.zip" length="12" sparkle:edSignature="abc="/>"#;
        assert_eq!(attribute(xml, "url"), Some("https://x/a.zip"));
        assert_eq!(attribute(xml, "length"), Some("12"));
        assert_eq!(attribute(xml, "sparkle:edSignature"), Some("abc="));
    }
}
