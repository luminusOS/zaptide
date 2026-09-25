use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LIMIT: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(not(target_os = "macos"))]
const MARKER: &str = "zaptide-portable-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Portable,
    WindowsInstaller,
    #[cfg(target_os = "macos")]
    MacBundle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub executable: PathBuf,
    pub kind: Kind,
}

impl Installation {
    fn root(&self) -> Result<&Path> {
        #[cfg(target_os = "macos")]
        if self.kind == Kind::MacBundle {
            return super::macos::bundle_root(&self.executable);
        }
        Ok(&self.executable)
    }
}

pub fn detect() -> Result<Installation> {
    detect_at(&std::env::current_exe()?.canonicalize()?)
}

pub fn detect_at(executable: &Path) -> Result<Installation> {
    let path = executable.to_string_lossy().replace('\\', "/");
    let lower = path.to_lowercase();
    if std::env::var_os("FLATPAK_ID").is_some() || path.starts_with("/app/") {
        bail!("Update this installation through your software center or flatpak update.");
    }
    if std::env::var_os("SNAP").is_some() || path.starts_with("/snap/") {
        bail!("Update this installation with snap refresh.");
    }
    if lower.contains("/.cargo/") {
        bail!("Update this installation with cargo install.");
    }
    if path.starts_with("/nix/") || lower.contains("/cellar/") || lower.contains("/caskroom/") {
        bail!("Update this installation with Nix or Homebrew.");
    }
    #[cfg(target_os = "linux")]
    {
        for (program, arguments, instruction) in [
            ("dpkg-query", vec!["-S"], "apt"),
            ("rpm", vec!["-qf"], "dnf"),
            ("pacman", vec!["-Qo"], "pacman"),
        ] {
            if Command::new(program)
                .args(arguments)
                .arg(executable)
                .output()
                .is_ok_and(|output| output.status.success())
            {
                bail!("Update this installation through {instruction} or your software center.");
            }
        }
        if path.starts_with("/usr/") || path.starts_with("/bin/") || path.starts_with("/sbin/") {
            bail!(
                "This installation is in a system directory. Use your package manager or the download page."
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    let directory = executable
        .parent()
        .context("The application has no installation directory")?;
    #[cfg(windows)]
    {
        let installed = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("Programs/ZapTide/zaptide.exe"));
        if fs::read_to_string(directory.join("zaptide-installer.txt"))
            .is_ok_and(|value| value.trim() == "zaptide-installer-v1")
            || (installed
                .and_then(|path| path.canonicalize().ok())
                .as_deref()
                == Some(executable)
                && directory.join("unins000.exe").is_file())
        {
            return Ok(Installation {
                executable: executable.to_owned(),
                kind: Kind::WindowsInstaller,
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        super::macos::detect(executable)?;
        Ok(Installation {
            executable: executable.to_owned(),
            kind: Kind::MacBundle,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        ensure!(
            fs::read_to_string(directory.join("zaptide-portable.txt"))
                .is_ok_and(|value| value.trim() == MARKER),
            "This installation does not identify itself as a portable download. Use the download page to install an update-enabled build."
        );
        Ok(Installation {
            executable: executable.to_owned(),
            kind: Kind::Portable,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prepared {
    pub installation: Installation,
    pub directory: PathBuf,
    pub payload: PathBuf,
    pub sha256: String,
    pub version: String,
}

#[derive(Serialize, Deserialize)]
struct Handoff {
    prepared: Prepared,
    parent: u32,
    arguments: Vec<String>,
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(super::hex(&hash.finalize()))
}

pub fn staging(installation: &Installation) -> Result<PathBuf> {
    let parent = installation
        .root()?
        .parent()
        .context("Missing installation directory")?;
    let directory = parent.join(format!(".zaptide-update-{:016x}", rand::random::<u64>()));
    fs::create_dir(&directory).context("Cannot write to the installation directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

pub fn extract(archive: &Path, entry: &str, destination: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut command = Command::new("tar");
    command
        .arg("-xOf")
        .arg(archive)
        .arg(entry)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command
        .spawn()
        .context("Cannot run tar to unpack the update")?;
    let result = (|| -> Result<()> {
        let mut stdout = child
            .stdout
            .take()
            .context("Missing archive stream")?
            .take(LIMIT + 1);
        let mut file = file;
        let count = std::io::copy(&mut stdout, &mut file)?;
        ensure!(
            count > 0 && count <= LIMIT,
            "The update executable has an invalid size"
        );
        ensure!(
            child.wait()?.success(),
            "Cannot unpack the update executable"
        );
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

pub fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

pub fn verify_version(executable: &Path, expected: &str) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command
        .spawn()
        .context("The downloaded app cannot run on this computer")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "The downloaded app failed its startup check"
            );
            let mut version = String::new();
            child
                .stdout
                .take()
                .context("Missing version output")?
                .take(4096)
                .read_to_string(&mut version)?;
            ensure!(
                version.trim() == format!("zaptide {expected}"),
                "The downloaded app has the wrong version"
            );
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("The downloaded app did not answer its startup check");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn handoff(prepared: &Prepared, arguments: Vec<String>) -> Result<()> {
    ensure!(
        hash(&prepared.payload)? == prepared.sha256,
        "The staged update changed. Download it again."
    );
    let helper = prepared.directory.join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    fs::copy(std::env::current_exe()?, &helper)?;
    let job = prepared.directory.join("handoff.json");
    let mut file = File::create(&job)?;
    serde_json::to_writer(
        &mut file,
        &Handoff {
            prepared: prepared.clone(),
            parent: std::process::id(),
            arguments,
        },
    )?;
    file.flush()?;
    file.sync_all()?;
    let mut command = Command::new(helper);
    command
        .arg("--apply-update")
        .arg(&job)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command.spawn().context("Cannot start the update helper")?;
    let ready = prepared.directory.join("ready");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if ready.exists() {
            return Ok(());
        }
        ensure!(
            child.try_wait()?.is_none(),
            "The update helper exited before it was ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!("The update helper did not start. Try again.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_package_managed_paths_are_not_portable() {
        for path in [
            "/usr/bin/zaptide",
            "/nix/store/package/bin/zaptide",
            "/home/test/.cargo/bin/zaptide",
            "/unknown/zaptide",
        ] {
            assert!(detect_at(Path::new(path)).is_err());
        }
    }
}
