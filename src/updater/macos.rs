//! macOS: the release's zip of `ClipboardX.app` (`ditto -c -k
//! --keepParent`, see the CI) replaces the running app, then the new one
//! starts once this process has exited.
//!
//! The zip is unpacked next to the app (a rename needs the same volume),
//! checked (this app's bundle id and executable, a valid signature), and
//! swapped in by two renames, the old app put back if the second fails.
//! Downloaded by the app, not a browser, it carries no quarantine: it
//! starts without Gatekeeper's prompt, as the app did after the user's
//! first approval.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BUNDLE_ID: &str = "com.clipboardx.app";
/// What an update leaves next to the app while it runs.
const STAGING: &str = ".ClipboardX-update-";
const OLD: &str = ".ClipboardX-old-";

/// The `.app` this process runs from.
fn running_app() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let app = executable.parent()?.parent()?.parent()?;
    app.extension()
        .is_some_and(|e| e == "app")
        .then(|| app.to_owned())
}

/// Whether the running app can be replaced: a bundle, not run from the
/// read-only copy Gatekeeper makes of a quarantined app (App
/// Translocation), in a directory the user can write (not a disk image).
pub fn can_install() -> bool {
    let Some(app) = running_app() else {
        return false;
    };
    if app.to_string_lossy().contains("/AppTranslocation/") {
        return false;
    }
    let Some(parent) = app.parent() else {
        return false;
    };
    staging(parent).is_ok_and(|probe| fs::remove_dir(probe).is_ok())
}

/// Replaces the running app with the one in `zip` and arranges for it to
/// start once this process has exited: the caller quits next. `false`:
/// replaced, but it will not start by itself.
pub fn install(zip: &Path) -> Result<bool, String> {
    let app = running_app().ok_or("the app is not running from a bundle")?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let executable = executable
        .strip_prefix(&app)
        .map_err(|e| e.to_string())?
        .to_owned();
    replace(&app, zip, &executable, BUNDLE_ID)?;
    Ok(relaunch(&app).inspect_err(|e| log::error!("{e}")).is_ok())
}

/// Removes what an interrupted update left beside the app.
pub fn remove_leftovers() {
    let Some(parent) = running_app().and_then(|app| app.parent().map(Path::to_owned)) else {
        return;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with(STAGING) || name.starts_with(OLD))
            && let Err(e) = fs::remove_dir_all(entry.path())
        {
            log::warn!("cannot remove {}: {e}", entry.path().display());
        }
    }
}

/// Replaces `app` with the bundle in `zip`, which must be `bundle_id` with
/// `executable` (relative to the bundle).
fn replace(app: &Path, zip: &Path, executable: &Path, bundle_id: &str) -> Result<(), String> {
    let parent = app.parent().ok_or("the app has no directory")?;
    let scratch = staging(parent).map_err(|e| format!("cannot unpack next to the app: {e}"))?;
    let result = unpack(zip, &scratch).and_then(|new| {
        check(&new, executable, bundle_id)?;
        swap(app, &new)
    });
    let _ = fs::remove_dir_all(&scratch);
    result
}

/// A fresh directory in `parent`.
fn staging(parent: &Path) -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    for attempt in 0..100 {
        let path = parent.join(format!("{STAGING}{pid}-{attempt}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free staging directory"))
}

/// Unpacks `zip` into `into` with `ditto`, which keeps what a bundle needs
/// (modes, extended attributes); returns the one `.app` it held.
fn unpack(zip: &Path, into: &Path) -> Result<PathBuf, String> {
    run(Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(into))
    .map_err(|e| format!("cannot unpack the update: {e}"))?;
    let apps: Vec<PathBuf> = fs::read_dir(into)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "app"))
        .collect();
    match apps.as_slice() {
        [app] => Ok(app.clone()),
        _ => Err("the update does not hold one app".to_owned()),
    }
}

/// Whether `new` is this app, runnable and validly signed.
fn check(new: &Path, executable: &Path, bundle_id: &str) -> Result<(), String> {
    let plist = new.join("Contents/Info.plist");
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .map_err(|e| e.to_string())?;
    let id = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || id.trim() != bundle_id {
        return Err(format!("the update is not {bundle_id}"));
    }
    if !new.join(executable).is_file() {
        return Err(format!("the update has no {}", executable.display()));
    }
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(new))
    .map_err(|_| "the update's code signature is not valid".to_owned())
}

/// Puts `new` where `app` is, by two renames; `app` comes back if the
/// second fails.
fn swap(app: &Path, new: &Path) -> Result<(), String> {
    let old = app.with_file_name(format!("{OLD}{}.app", std::process::id()));
    fs::rename(app, &old).map_err(|e| format!("cannot move the app aside: {e}"))?;
    if let Err(e) = fs::rename(new, app) {
        if let Err(back) = fs::rename(&old, app) {
            log::error!("cannot put the app back from {}: {back}", old.display());
        }
        return Err(format!("cannot put the update in place: {e}"));
    }
    // The running process keeps its files open: removing them is safe.
    if let Err(e) = fs::remove_dir_all(&old) {
        log::warn!("cannot remove the old app at {}: {e}", old.display());
    }
    Ok(())
}

/// Starts `app` once this process has exited, if it does within a minute
/// (else a quit long after would bring the app back).
fn relaunch(app: &Path) -> Result<(), String> {
    Command::new("/bin/sh")
        .args([
            "-c",
            r#"i=0
while kill -0 "$1" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 300 ] && exit 0; sleep 0.2
done
exec /usr/bin/open "$2""#,
            "sh",
        ])
        .arg(std::process::id().to_string())
        .arg(app)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot restart the app: {e}"))
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "com.clipboardx.test";
    const EXECUTABLE: &str = "Contents/MacOS/ClipboardX";

    /// An ad hoc signed bundle `<dir>/<name>.app` of `id` whose
    /// executable prints `version`.
    fn make_app(dir: &Path, name: &str, id: &str, version: &str) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleExecutable</key><string>ClipboardX</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        let executable = app.join(EXECUTABLE);
        fs::write(&executable, format!("#!/bin/sh\necho {version}\n")).unwrap();
        run(Command::new("/bin/chmod").arg("755").arg(&executable)).unwrap();
        run(Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app))
        .unwrap();
        app
    }

    fn zip(app: &Path, to: &Path) {
        run(Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(app)
            .arg(to))
        .unwrap();
    }

    fn version_of(app: &Path) -> String {
        fs::read_to_string(app.join(EXECUTABLE)).unwrap()
    }

    /// Only the installed app is left: no staging or old copy.
    fn only_entry(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn replaces_the_app_with_the_zipped_one() {
        let source = tempfile::tempdir().unwrap();
        let new = make_app(source.path(), "ClipboardX", ID, "new");
        let archive = source.path().join("update.zip");
        zip(&new, &archive);

        let installed = tempfile::tempdir().unwrap();
        // The user renamed it: the update keeps the name.
        let app = make_app(installed.path(), "My ClipboardX", ID, "old");
        replace(&app, &archive, Path::new(EXECUTABLE), ID).unwrap();

        assert!(version_of(&app).contains("new"));
        assert_eq!(only_entry(installed.path()), vec!["My ClipboardX.app"]);
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app))
        .unwrap();
    }

    #[test]
    fn refuses_another_app_and_leaves_this_one() {
        let source = tempfile::tempdir().unwrap();
        let other = make_app(source.path(), "ClipboardX", "com.example.other", "new");
        let archive = source.path().join("update.zip");
        zip(&other, &archive);

        let installed = tempfile::tempdir().unwrap();
        let app = make_app(installed.path(), "ClipboardX", ID, "old");
        let error = replace(&app, &archive, Path::new(EXECUTABLE), ID).unwrap_err();

        assert!(error.contains(ID), "{error}");
        assert!(version_of(&app).contains("old"));
        assert_eq!(only_entry(installed.path()), vec!["ClipboardX.app"]);
    }

    #[test]
    fn refuses_a_broken_signature() {
        let source = tempfile::tempdir().unwrap();
        let new = make_app(source.path(), "ClipboardX", ID, "new");
        // Changed after signing.
        fs::write(new.join(EXECUTABLE), "#!/bin/sh\necho tampered\n").unwrap();
        let archive = source.path().join("update.zip");
        zip(&new, &archive);

        let installed = tempfile::tempdir().unwrap();
        let app = make_app(installed.path(), "ClipboardX", ID, "old");
        let error = replace(&app, &archive, Path::new(EXECUTABLE), ID).unwrap_err();

        assert!(error.contains("signature"), "{error}");
        assert!(version_of(&app).contains("old"));
    }

    #[test]
    fn refuses_what_is_not_a_zip() {
        let installed = tempfile::tempdir().unwrap();
        let app = make_app(installed.path(), "ClipboardX", ID, "old");
        let junk = installed.path().join("junk.zip");
        fs::write(&junk, "not a zip").unwrap();
        assert!(replace(&app, &junk, Path::new(EXECUTABLE), ID).is_err());
        assert!(version_of(&app).contains("old"));
    }
}
