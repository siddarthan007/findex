//! Signed, consent-gated self-updates for the CLI and TUI.
//!
//! The public key and manifest endpoint are compiled into release binaries.
//! Local builds without `FINDEX_UPDATER_PUBLIC_KEY` remain network-silent.

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_REPOSITORY: &str = "siddarthan007/findex";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateArtifact {
    pub url: String,
    pub signature: String,
    pub binary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    pub target: String,
    pub artifact: UpdateArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateCheck {
    pub enabled: bool,
    pub current_version: String,
    pub checked_at_unix: u64,
    pub available: Option<AvailableUpdate>,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateManifest {
    version: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    pub_date: String,
    platforms: HashMap<String, UpdateArtifact>,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("updates are disabled in this build because no signing public key was compiled in")]
    Disabled,
    #[error("unsupported update target: {0}")]
    UnsupportedTarget(String),
    #[error("update transport must use HTTPS: {0}")]
    InsecureUrl(String),
    #[error("invalid release version '{value}': {source}")]
    Version {
        value: String,
        #[source]
        source: semver::Error,
    },
    #[error("update request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid update manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("update archive exceeds the {0} MiB safety limit")]
    ArchiveTooLarge(u64),
    #[error("invalid Minisign public key: {0}")]
    PublicKey(String),
    #[error("invalid Minisign signature: {0}")]
    Signature(String),
    #[error("release signature verification failed: {0}")]
    Verification(String),
    #[error("unsafe archive binary path: {0}")]
    UnsafeBinaryPath(String),
    #[error("archive does not contain expected binary '{0}'")]
    MissingBinary(String),
    #[error("refusing to replace a development binary; install a packaged release or set FINDEX_ALLOW_DEV_UPDATE=1")]
    DevelopmentBinary,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

pub fn updater_public_key() -> Option<&'static str> {
    option_env!("FINDEX_UPDATER_PUBLIC_KEY").filter(|value| !value.trim().is_empty())
}

pub fn updater_repository() -> &'static str {
    option_env!("FINDEX_UPDATER_REPOSITORY")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_REPOSITORY)
}

pub fn updater_manifest_url() -> String {
    option_env!("FINDEX_UPDATER_MANIFEST_URL")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{}/releases/latest/download/latest-cli.json",
                updater_repository()
            )
        })
}

pub fn updater_enabled() -> bool {
    updater_public_key().is_some()
}

pub fn target_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn check_for_update(force: bool) -> Result<UpdateCheck, UpdateError> {
    let now = unix_now();
    if !updater_enabled() {
        return Ok(UpdateCheck {
            enabled: false,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            checked_at_unix: now,
            available: None,
            from_cache: false,
        });
    }

    if !force {
        if let Some(mut cached) = read_cached_check() {
            if now.saturating_sub(cached.checked_at_unix) < CHECK_INTERVAL.as_secs() {
                cached.from_cache = true;
                return Ok(cached);
            }
        }
    }

    let manifest_url = updater_manifest_url();
    require_https(&manifest_url)?;
    let manifest: UpdateManifest = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(format!("findex/{} updater", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(&manifest_url)
        .send()?
        .error_for_status()?
        .json()?;
    let check = evaluate_manifest(manifest, now)?;
    let _ = write_cached_check(&check);
    Ok(check)
}

fn evaluate_manifest(manifest: UpdateManifest, now: u64) -> Result<UpdateCheck, UpdateError> {
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    let offered = parse_version(&manifest.version)?;
    let target = target_key();
    let available = if offered > current {
        let artifact = manifest
            .platforms
            .get(&target)
            .cloned()
            .ok_or_else(|| UpdateError::UnsupportedTarget(target.clone()))?;
        require_https(&artifact.url)?;
        Some(AvailableUpdate {
            version: offered.to_string(),
            notes: manifest.notes,
            pub_date: manifest.pub_date,
            target,
            artifact,
        })
    } else {
        None
    };
    Ok(UpdateCheck {
        enabled: true,
        current_version: current.to_string(),
        checked_at_unix: now,
        available,
        from_cache: false,
    })
}

pub fn install_update(update: &AvailableUpdate) -> Result<(), UpdateError> {
    let public_key = updater_public_key().ok_or(UpdateError::Disabled)?;
    require_https(&update.artifact.url)?;
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    let offered = parse_version(&update.version)?;
    if offered <= current {
        return Ok(());
    }
    if update.target != target_key() {
        return Err(UpdateError::UnsupportedTarget(update.target.clone()));
    }
    validate_binary_name(&update.artifact.binary)?;
    refuse_development_binary()?;

    let public_key = if public_key.contains('\n') {
        PublicKey::decode(public_key)
    } else {
        PublicKey::from_base64(public_key.trim())
    }
    .map_err(|error| UpdateError::PublicKey(error.to_string()))?;
    let signature = Signature::decode(&update.artifact.signature)
        .map_err(|error| UpdateError::Signature(error.to_string()))?;

    let updates_dir = findex_home().join("updates");
    std::fs::create_dir_all(&updates_dir)?;
    let stage = tempfile::Builder::new()
        .prefix("findex-update-")
        .tempdir_in(&updates_dir)?;
    let archive_path = stage.path().join("release.zip");
    download_verified(&update.artifact.url, &archive_path, &public_key, &signature)?;

    let staged_binary = stage.path().join(&update.artifact.binary);
    extract_binary(&archive_path, &update.artifact.binary, &staged_binary)?;
    self_replace::self_replace(&staged_binary)?;
    Ok(())
}

fn download_verified(
    url: &str,
    destination: &Path,
    public_key: &PublicKey,
    signature: &Signature,
) -> Result<(), UpdateError> {
    let mut response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(format!("findex/{} updater", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(UpdateError::ArchiveTooLarge(
            MAX_ARCHIVE_BYTES / 1024 / 1024,
        ));
    }

    let mut verifier = public_key
        .verify_stream(signature)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    let mut destination = File::create(destination)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_ARCHIVE_BYTES {
            return Err(UpdateError::ArchiveTooLarge(
                MAX_ARCHIVE_BYTES / 1024 / 1024,
            ));
        }
        verifier.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    destination.sync_all()?;
    verifier
        .finalize()
        .map_err(|error| UpdateError::Verification(error.to_string()))
}

fn extract_binary(archive: &Path, binary: &str, destination: &Path) -> Result<(), UpdateError> {
    let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
    let mut entry = archive
        .by_name(binary)
        .map_err(|_| UpdateError::MissingBinary(binary.to_string()))?;
    let mut output = File::create(destination)?;
    std::io::copy(&mut entry, &mut output)?;
    output.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn validate_binary_name(binary: &str) -> Result<(), UpdateError> {
    let path = Path::new(binary);
    if path.file_name().and_then(|name| name.to_str()) != Some(binary)
        || binary == "."
        || binary == ".."
    {
        return Err(UpdateError::UnsafeBinaryPath(binary.to_string()));
    }
    Ok(())
}

fn refuse_development_binary() -> Result<(), UpdateError> {
    if std::env::var("FINDEX_ALLOW_DEV_UPDATE").as_deref() == Ok("1") {
        return Ok(());
    }
    let current = std::env::current_exe()?;
    if current.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("target")
    }) {
        return Err(UpdateError::DevelopmentBinary);
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<Version, UpdateError> {
    Version::parse(value.trim().trim_start_matches('v')).map_err(|source| UpdateError::Version {
        value: value.to_string(),
        source,
    })
}

fn require_https(url: &str) -> Result<(), UpdateError> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(UpdateError::InsecureUrl(url.to_string()))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn findex_home() -> PathBuf {
    std::env::var_os("FINDEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
        .join(".findex")
}

fn cache_path() -> PathBuf {
    findex_home().join("update-state.json")
}

fn read_cached_check() -> Option<UpdateCheck> {
    serde_json::from_slice(&std::fs::read(cache_path()).ok()?).ok()
}

fn write_cached_check(check: &UpdateCheck) -> Result<(), UpdateError> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec(check)?)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, url: &str) -> UpdateManifest {
        UpdateManifest {
            version: version.to_string(),
            notes: "test".to_string(),
            pub_date: "2026-07-15T00:00:00Z".to_string(),
            platforms: HashMap::from([(
                target_key(),
                UpdateArtifact {
                    url: url.to_string(),
                    signature: "signature".to_string(),
                    binary: if cfg!(windows) {
                        "findex.exe".to_string()
                    } else {
                        "findex".to_string()
                    },
                },
            )]),
        }
    }

    #[test]
    fn newer_manifest_is_offered() {
        let check = evaluate_manifest(manifest("99.0.0", "https://example.invalid/findex.zip"), 42)
            .unwrap();
        assert_eq!(check.available.unwrap().version, "99.0.0");
    }

    #[test]
    fn current_manifest_is_not_offered() {
        let check = evaluate_manifest(
            manifest(
                env!("CARGO_PKG_VERSION"),
                "https://example.invalid/findex.zip",
            ),
            42,
        )
        .unwrap();
        assert!(check.available.is_none());
    }

    #[test]
    fn insecure_download_is_rejected() {
        let error = evaluate_manifest(manifest("99.0.0", "http://example.invalid/findex.zip"), 42)
            .unwrap_err();
        assert!(matches!(error, UpdateError::InsecureUrl(_)));
    }

    #[test]
    fn archive_paths_cannot_escape_staging() {
        assert!(validate_binary_name("findex.exe").is_ok());
        assert!(validate_binary_name("../findex.exe").is_err());
        assert!(validate_binary_name("bin/findex").is_err());
    }
}
