use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lightsync_domain::{AppConfig, BridgeConfiguration};
use tokio::io::AsyncWriteExt;

const CONFIG_DIRECTORY: &str = "omarchy-lightsync";
const CONFIG_FILE: &str = "config.toml";
const PENDING_PAIRING_FILE: &str = "pending-pairing.toml";

pub fn config_path() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        if directory.is_empty() {
            bail!("XDG_CONFIG_HOME is empty");
        }
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            bail!("XDG_CONFIG_HOME must be absolute");
        }
        return Ok(directory.join(CONFIG_DIRECTORY).join(CONFIG_FILE));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join(CONFIG_DIRECTORY)
        .join(CONFIG_FILE))
}

pub async fn load(path: &Path) -> Result<AppConfig> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => {
            let config: AppConfig = toml::from_str(&contents).context("config.toml is invalid")?;
            config.validate().context("config.toml failed validation")?;
            Ok(config)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(error) => Err(error).context("failed to read config.toml"),
    }
}

pub async fn save(path: &Path, config: &AppConfig) -> Result<()> {
    config
        .validate()
        .context("refusing to save invalid config")?;
    let parent = path.parent().context("config path has no parent")?;
    reject_symlink(parent).await?;
    tokio::fs::create_dir_all(parent)
        .await
        .context("failed to create config directory")?;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        .await
        .context("failed to secure config directory")?;
    reject_symlink(path).await?;

    let encoded = toml::to_string_pretty(config).context("failed to encode config")?;
    let temporary = parent.join(format!(".{CONFIG_FILE}.{}.tmp", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(&temporary)
        .await
        .context("failed to create temporary config")?;
    let result = async {
        file.write_all(encoded.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
        Ok::<_, io::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result.context("failed to atomically save config")
}

pub async fn load_pending_pairing(path: &Path) -> Result<Option<BridgeConfiguration>> {
    let path = path
        .parent()
        .context("config path has no parent")?
        .join(PENDING_PAIRING_FILE);
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => {
            let bridge: BridgeConfiguration =
                toml::from_str(&contents).context("pending pairing state is invalid")?;
            bridge
                .validate()
                .context("pending pairing state failed validation")?;
            Ok(Some(bridge))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to read pending pairing state"),
    }
}

pub async fn save_pending_pairing(
    config_path: &Path,
    bridge: Option<&BridgeConfiguration>,
) -> Result<()> {
    let parent = config_path.parent().context("config path has no parent")?;
    let path = parent.join(PENDING_PAIRING_FILE);
    if bridge.is_none() {
        return match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("failed to remove pending pairing state"),
        };
    }
    reject_symlink(parent).await?;
    tokio::fs::create_dir_all(parent).await?;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    reject_symlink(&path).await?;
    let encoded = toml::to_string_pretty(bridge.context("pending bridge is missing")?)?;
    let temporary = parent.join(format!(
        ".{PENDING_PAIRING_FILE}.{}.tmp",
        std::process::id()
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temporary).await?;
    let result = async {
        file.write_all(encoded.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, &path).await?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result.context("failed to save pending pairing state")
}

async fn reject_symlink(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlink path: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to inspect config path"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use lightsync_domain::{BridgeConfiguration, BridgeId};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temporary_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightsync-daemon-config-{}-{}/lightsync/config.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[tokio::test]
    async fn config_is_private_atomic_and_contains_no_credentials() {
        let path = temporary_path();
        let config = AppConfig {
            bridge: Some(BridgeConfiguration {
                id: BridgeId::new("001788fffe123456").expect("id"),
                host: "192.0.2.1".into(),
                name: Some("Desk".into()),
            }),
            ..AppConfig::default()
        };
        save(&path, &config).await.expect("save");
        assert_eq!(load(&path).await.expect("load"), config);
        let text = tokio::fs::read_to_string(&path).await.expect("read");
        assert!(!text.contains("application_key"));
        assert!(!text.contains("client_key"));
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().expect("parent"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let root = path.parent().and_then(Path::parent).expect("root");
        let _ = std::fs::remove_dir_all(root);
    }
}
