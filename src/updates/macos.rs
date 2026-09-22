use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use super::install::{self, Installation, Prepared};

const IDENTIFIER: &str = "dev.luminusos.ZapTide";

pub(super) fn bundle_root(executable: &Path) -> Result<&Path> {
    let root = executable
        .ancestors()
        .nth(3)
        .context("Missing app bundle")?;
    ensure!(
        root.extension().is_some_and(|extension| extension == "app")
            && root.join("Contents/MacOS/zaptide") == executable,
        "Move the app to Applications, then open it to update."
    );
    Ok(root)
}

fn plist(bundle: &Path, key: &str) -> Result<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")])
        .arg(bundle.join("Contents/Info.plist"))
        .output()?;
    ensure!(output.status.success(), "The app bundle is missing {key}");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn identity(bundle: &Path) -> Result<()> {
    ensure!(
        plist(bundle, "CFBundleIdentifier")? == IDENTIFIER
            && plist(bundle, "CFBundleExecutable")? == "zaptide"
            && plist(bundle, "CFBundlePackageType")? == "APPL",
        "The download is not a ZapTide app bundle"
    );
    Ok(())
}

pub(super) fn detect(executable: &Path) -> Result<()> {
    ensure!(
        !executable.starts_with("/Volumes")
            && !executable
                .components()
                .any(|part| part.as_os_str() == "AppTranslocation"),
        "Move the app to Applications, then open it to update."
    );
    let bundle = bundle_root(executable)?;
    let prefixes = [
        Some(PathBuf::from("/opt/homebrew")),
        Some(PathBuf::from("/usr/local")),
        std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from),
    ];
    for prefix in prefixes.into_iter().flatten() {
        ensure!(
            !cask_owns(&prefix.join("Caskroom/zaptide"), bundle),
            "Update this installation with Homebrew."
        );
    }
    identity(bundle)
}

fn cask_owns(cask: &Path, bundle: &Path) -> bool {
    let Ok(bundle) = bundle.canonicalize() else {
        return false;
    };
    fs::read_dir(cask).is_ok_and(|versions| {
        versions.flatten().any(|version| {
            version
                .path()
                .join("ZapTide.app")
                .canonicalize()
                .is_ok_and(|installed| installed == bundle)
        })
    })
}

fn team(bundle: &Path) -> Result<Option<String>> {
    let verify = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()?;
    ensure!(
        verify.status.success(),
        "The app signature could not be verified"
    );
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(bundle)
        .output()?;
    ensure!(
        output.status.success(),
        "Cannot read the app signing identity"
    );
    Ok(String::from_utf8(output.stderr)?
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|value| *value != "not set")
        .map(str::to_owned))
}

fn validate(bundle: &Path, installation: &Installation, version: &str) -> Result<()> {
    identity(bundle)?;
    ensure!(
        plist(bundle, "CFBundleShortVersionString")? == version,
        "The app bundle has the wrong version"
    );
    let incoming = team(bundle)?;
    if let Some(current) = team(bundle_root(&installation.executable)?)? {
        ensure!(
            incoming.as_deref() == Some(current.as_str()),
            "The update was signed by a different publisher"
        );
        let assessment = Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(bundle)
            .output()?;
        ensure!(
            assessment.status.success(),
            "macOS could not approve this update for launch"
        );
    }
    install::verify_version(&bundle.join("Contents/MacOS/zaptide"), version)
}

struct Mounted(PathBuf);

fn mountpoint(archive: &Path) -> Result<PathBuf> {
    let mount = archive
        .parent()
        .context("Missing update directory")?
        .join(format!("mounted-{:016x}", rand::random::<u64>()));
    fs::create_dir(&mount)?;
    Ok(mount)
}

impl Mounted {
    fn open(archive: &Path) -> Result<Self> {
        // A failed detach can leave the previous attempt mounted. Never
        // traverse that volume or reuse its directory on a later attempt.
        let mount = mountpoint(archive)?;
        let output = Command::new("/usr/bin/hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(archive)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_dir(&mount);
            anyhow::bail!("macOS could not open the downloaded disk image");
        }
        Ok(Self(mount))
    }

    fn bundle(&self) -> Result<PathBuf> {
        image_bundle(&self.0)
    }
}

fn image_bundle(root: &Path) -> Result<PathBuf> {
    let bundle = root.join("ZapTide.app");
    match fs::symlink_metadata(&bundle) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "The disk image has an invalid app bundle"
            );
            Ok(bundle)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("The disk image has no ZapTide app bundle")
        }
        Err(error) => Err(error.into()),
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let detached = Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&self.0)
            .output()
            .is_ok_and(|output| output.status.success());
        if detached {
            let _ = fs::remove_dir(&self.0);
        }
    }
}

pub(super) fn validate_download(
    archive: &Path,
    installation: &Installation,
    version: &str,
) -> Result<()> {
    let mounted = Mounted::open(archive)?;
    validate(&mounted.bundle()?, installation, version)
}

pub(super) fn replace(prepared: &Prepared) -> Result<()> {
    let target = bundle_root(&prepared.installation.executable)?;
    let backup = prepared.directory.join("previous");
    let candidate = prepared.directory.join("ZapTide.app");
    ensure!(
        !backup.exists() && !candidate.exists(),
        "This update was already applied"
    );
    let mounted = Mounted::open(&prepared.payload)?;
    let source = mounted.bundle()?;
    validate(&source, &prepared.installation, &prepared.version)?;
    let copy = Command::new("/usr/bin/ditto")
        .arg(&source)
        .arg(&candidate)
        .output()?;
    ensure!(
        copy.status.success(),
        "Could not copy the downloaded app bundle"
    );
    validate(&candidate, &prepared.installation, &prepared.version)?;
    drop(mounted);
    fs::rename(target, &backup).context("Cannot back up the current app bundle")?;
    if let Err(error) = fs::rename(&candidate, target) {
        fs::rename(&backup, target).context("Could not restore the previous app bundle")?;
        return Err(error).context("Could not replace the app bundle");
    }
    Ok(())
}

pub(super) fn restore(prepared: &Prepared) -> Result<()> {
    let backup = prepared.directory.join("previous");
    if backup.is_dir() {
        let target = bundle_root(&prepared.installation.executable)?;
        if target.exists() {
            fs::rename(target, prepared.directory.join("failed.app"))
                .context("Could not move the failed update aside")?;
        }
        fs::rename(backup, target).context("Could not restore the previous app bundle")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_accept_only_the_zaptide_bundle() {
        let root =
            std::env::temp_dir().join(format!("zaptide-image-test-{}", rand::random::<u64>()));
        fs::create_dir(&root).unwrap();
        assert!(image_bundle(&root).is_err());
        let app = root.join("ZapTide.app");
        fs::create_dir(&app).unwrap();
        assert_eq!(image_bundle(&root).unwrap(), app);
        fs::remove_dir(&app).unwrap();
        std::os::unix::fs::symlink(root.join("Other.app"), &app).unwrap();
        assert!(
            image_bundle(&root).is_err(),
            "never follow an app-bundle symlink"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn homebrew_ownership_uses_the_zaptide_bundle() {
        let root =
            std::env::temp_dir().join(format!("zaptide-cask-test-{}", rand::random::<u64>()));
        let installed = root.join("Applications/ZapTide.app");
        let version = root.join("Caskroom/zaptide/0.8.0");
        fs::create_dir_all(&installed).unwrap();
        fs::create_dir_all(&version).unwrap();
        std::os::unix::fs::symlink(&installed, version.join("ZapTide.app")).unwrap();
        assert!(cask_owns(&root.join("Caskroom/zaptide"), &installed));
        assert!(!cask_owns(&root.join("Caskroom/unrelated"), &installed));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn another_mount_attempt_leaves_the_previous_volume_alone() {
        let directory =
            std::env::temp_dir().join(format!("zaptide-mount-test-{}", rand::random::<u64>()));
        fs::create_dir(&directory).unwrap();
        let archive = directory.join("update.dmg");
        let first = mountpoint(&archive).unwrap();
        fs::write(first.join("still-mounted"), b"existing volume").unwrap();
        let second = mountpoint(&archive).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            fs::read(first.join("still-mounted")).unwrap(),
            b"existing volume"
        );
        assert!(second.is_dir());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn homebrew_app_symlink_does_not_claim_other_copies() {
        let directory =
            std::env::temp_dir().join(format!("zaptide-cask-test-{}", rand::random::<u64>()));
        let installed = directory.join("Applications/ZapTide.app");
        let cask = directory.join("Caskroom/zaptide");
        let copy = directory.join("dev/ZapTide.app");
        for path in [&installed, &copy, &cask.join("0.7.1")] {
            fs::create_dir_all(path).unwrap();
        }
        std::os::unix::fs::symlink(&installed, cask.join("0.7.1/ZapTide.app")).unwrap();
        assert!(cask_owns(&cask, &installed));
        assert!(!cask_owns(&cask, &copy));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rollback_restores_resources_and_executable_together() {
        let directory =
            std::env::temp_dir().join(format!("zaptide-mac-test-{}", rand::random::<u64>()));
        let app = directory.join("ZapTide.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/zaptide"), b"new executable").unwrap();
        fs::write(app.join("Contents/Info.plist"), b"new metadata").unwrap();
        let installation = Installation {
            executable: app.join("Contents/MacOS/zaptide"),
            kind: install::Kind::MacBundle,
        };
        let stage = install::staging(&installation).unwrap();
        assert_eq!(stage.parent(), Some(directory.as_path()));
        let backup = stage.join("previous");
        fs::create_dir_all(backup.join("Contents/MacOS")).unwrap();
        fs::write(backup.join("Contents/MacOS/zaptide"), b"old executable").unwrap();
        fs::write(backup.join("Contents/Info.plist"), b"old metadata").unwrap();
        let prepared = Prepared {
            installation,
            directory: stage.clone(),
            payload: stage.join("update.dmg"),
            sha256: String::new(),
            version: "0.7.2".into(),
        };
        restore(&prepared).unwrap();
        assert_eq!(
            fs::read(app.join("Contents/MacOS/zaptide")).unwrap(),
            b"old executable"
        );
        assert_eq!(
            fs::read(app.join("Contents/Info.plist")).unwrap(),
            b"old metadata"
        );
        assert_eq!(
            fs::read(stage.join("failed.app/Contents/Info.plist")).unwrap(),
            b"new metadata"
        );
        assert!(!backup.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn only_expected_bundle_layout_is_accepted() {
        assert_eq!(
            bundle_root(Path::new(
                "/Applications/ZapTide.app/Contents/MacOS/zaptide"
            ))
            .unwrap(),
            Path::new("/Applications/ZapTide.app")
        );
        for path in [
            "/Applications/ZapTide.app/zaptide",
            "/tmp/Contents/MacOS/zaptide",
            "/usr/local/bin/zaptide",
        ] {
            assert!(bundle_root(Path::new(path)).is_err());
        }
        assert!(
            detect(Path::new(
                "/Volumes/ZapTide/ZapTide.app/Contents/MacOS/zaptide"
            ))
            .is_err()
        );
        assert!(
            detect(Path::new(
                "/private/var/folders/test/AppTranslocation/test/ZapTide.app/Contents/MacOS/zaptide"
            ))
            .is_err()
        );
    }
}
