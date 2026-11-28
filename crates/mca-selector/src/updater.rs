//! GitHub nightly self-updater. The release publishes latest.json containing
//! the commit and per-platform asset hashes; builds compare their embedded
//! commit and transparently stage the matching replacement binary.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

const LATEST_JSON: &str = "https://github.com/Coobyk/mca-selector-rs/releases/download/nightly/latest.json";
const RELEASE_DOWNLOAD: &str = "https://github.com/Coobyk/mca-selector-rs/releases/download/nightly";
const CURRENT_SHA: &str = env!("MCA_GIT_SHA");

#[derive(Deserialize)]
struct Latest {
    sha: String,
    assets: std::collections::HashMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    file: String,
    sha256: String,
}

pub struct DownloadedUpdate {
    pub sha: String,
    bytes: Vec<u8>,
    target: PathBuf,
}

fn asset_key() -> &'static str {
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("APPIMAGE").is_some() {
            return "linux-appimage";
        }
        return "linux";
    }
    #[cfg(target_os = "macos")]
    return "macos";
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn get(url: &str) -> Result<Vec<u8>> {
    let response = ureq::get(url)
        .set("User-Agent", concat!("mca-selector-rs/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    let mut out = Vec::new();
    response.into_reader().read_to_end(&mut out)?;
    Ok(out)
}

/// Check the rolling nightly release and download its replacement binary.
/// Returning None means current/error-prone local builds should not prompt.
pub fn check_and_download() -> Result<Option<DownloadedUpdate>> {
    if cfg!(debug_assertions) || CURRENT_SHA.is_empty() || CURRENT_SHA == "unknown" {
        return Ok(None);
    }
    let latest: Latest = serde_json::from_slice(&get(LATEST_JSON)?)?;
    if latest.sha == CURRENT_SHA {
        return Ok(None);
    }
    let key = asset_key();
    let asset = latest.assets.get(key).ok_or_else(|| anyhow!("release metadata has no {key} asset"))?;
    let bytes = get(&format!("{RELEASE_DOWNLOAD}/{}", asset.file))?;
    let digest = hex(&Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        bail!("downloaded update hash mismatch");
    }
    #[cfg(target_os = "linux")]
    let target = if key == "linux-appimage" {
        PathBuf::from(std::env::var_os("APPIMAGE").unwrap())
    } else {
        std::env::current_exe()?
    };
    #[cfg(not(target_os = "linux"))]
    let target = std::env::current_exe()?;
    Ok(Some(DownloadedUpdate { sha: latest.sha, bytes, target }))
}

fn write_script(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

#[cfg(windows)]
fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[cfg(not(windows))]
fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\''"))
}

/// Replace the current executable after this process exits, then relaunch it.
/// Windows cannot overwrite a running executable, while Unix uses the same
/// script path for raw binaries, AppImages, and .app inner binaries.
pub fn install_and_restart(update: DownloadedUpdate) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("mca-selector-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let new_bin = dir.join(if cfg!(windows) { "mca-selector-new.exe" } else { "mca-selector-new" });
    std::fs::write(&new_bin, &update.bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755))?;
    }
    let target = update.target;
    let pid = std::process::id();
    #[cfg(windows)]
    {
        let script = dir.join("update.ps1");
        write_script(&script, &format!(
            "$ErrorActionPreference = 'Stop'\n\
             while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 300 }}\n\
             Copy-Item -LiteralPath {} -Destination {} -Force\n\
             Start-Process -FilePath {}\n\
             Remove-Item -LiteralPath {} -Recurse -Force\n",
            ps_quote(&new_bin), ps_quote(&target), ps_quote(&target), ps_quote(&dir)
        ))?;
        std::process::Command::new("powershell")
            .args(["-WindowStyle", "Hidden", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .spawn()?;
    }
    #[cfg(not(windows))]
    {
        let script = dir.join("update.sh");
        write_script(&script, &format!(
            "#!/bin/sh\n\
             while kill -0 {pid} 2>/dev/null; do sleep 0.3; done\n\
             mv {} {}\n\
             chmod +x {}\n\
             {} >/dev/null 2>&1 &\n\
             rm -rf {}\n",
            sh_quote(&new_bin), sh_quote(&target), sh_quote(&target), sh_quote(&target), sh_quote(&dir)
        ))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        std::process::Command::new("/bin/sh").arg(&script).spawn()?;
    }
    std::process::exit(0);
}
