use crate::error::{Result, StoreError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Mutex;

/// 串行化 config.json 的「加载→修改→保存」，避免并发请求互相覆盖。
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// 模拟 Apple Configurator 2 的 User-Agent（与 ApplePackage 保持一致）
pub const USER_AGENT: &str =
    "Configurator/2.17 (Macintosh; OS X 15.2; 24C5089c) AppleWebKit/0620.1.16.11.6";

#[derive(Serialize, Deserialize)]
pub struct AccountMeta {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    /// 账号所属国家/地区（由 storeFront 解析，如 CN/US）
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// ipatool 文件钥匙串口令（首次自动生成并持久化）
    #[serde(default = "random_passphrase")]
    pub keychain_passphrase: String,
    /// 已登录账号列表（多账号，每个账号独立的 ipatool home）
    #[serde(default)]
    pub accounts: Vec<AccountMeta>,
    /// 当前选中账号（所有商店操作走该账号）
    #[serde(default)]
    pub current_account: Option<String>,
}

fn config_dir() -> Result<PathBuf> {
    // Docker 部署时通过 IPADOWN_DATA_DIR 指定数据目录（挂卷持久化）
    if let Ok(dir) = std::env::var("IPADOWN_DATA_DIR") {
        let dir = PathBuf::from(dir);
        create_private_dir(&dir)?;
        return Ok(dir);
    }
    let proj = directories::ProjectDirs::from("dev", "ipadown", "ipadown")
        .ok_or_else(|| StoreError::Config("无法确定用户配置目录".into()))?;
    let dir = proj.config_dir().to_path_buf();
    create_private_dir(&dir)?;
    Ok(dir)
}

fn create_private_dir(path: &std::path::Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    let temp_path = path.with_extension(format!("tmp-{}", random_hex::<8>()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp_path)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&temp_path, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn ipas_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("ipas");
    create_private_dir(&dir)?;
    Ok(dir)
}

fn random_passphrase() -> String {
    random_hex::<32>()
}

fn random_hex<const N: usize>() -> String {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("无法从操作系统获取安全随机数");
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// 原子的读-改-写：在锁内重新加载最新配置，应用修改并落盘。
/// 修改返回 Err 时不会写入（用于校验失败等场景）。
pub fn update<T>(mutate: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let _guard = CONFIG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut cfg = Config::load_or_create()?;
    let outcome = mutate(&mut cfg)?;
    cfg.save()?;
    Ok(outcome)
}

impl Config {
    pub fn load_or_create() -> Result<Self> {
        let path = config_dir()?.join("config.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            let cfg: Config = serde_json::from_str(&data)?;
            // serde default 为旧版文件补全字段后只回写一次，避免每次读配置都 fsync。
            let normalized = serde_json::to_string_pretty(&cfg)?;
            if normalized != data {
                write_private(&path, normalized.as_bytes())?;
            }
            return Ok(cfg);
        }
        let cfg = Config {
            keychain_passphrase: random_passphrase(),
            accounts: Vec::new(),
            current_account: None,
        };
        let data = serde_json::to_string_pretty(&cfg)?;
        write_private(&path, data.as_bytes())?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_dir()?.join("config.json");
        write_private(&path, serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }

    pub fn upsert_account(&mut self, meta: AccountMeta) {
        match self.accounts.iter_mut().find(|a| a.email == meta.email) {
            Some(a) => *a = meta,
            None => self.accounts.push(meta),
        }
    }

    pub fn remove_account(&mut self, email: &str) {
        self.accounts.retain(|a| a.email != email);
        if self.current_account.as_deref() == Some(email) {
            self.current_account = self.accounts.first().map(|a| a.email.clone());
        }
    }

    pub fn current_meta(&self) -> Option<&AccountMeta> {
        self.accounts
            .iter()
            .find(|a| Some(&a.email) == self.current_account.as_ref())
            .or(self.accounts.first())
    }
}

/// ipatool 多账号根目录：每个账号一个独立 HOME（钥匙串隔离）
pub fn ipatool_accounts_root() -> Result<PathBuf> {
    let dir = config_dir()?.join("ipatool-home").join("accounts");
    create_private_dir(&dir)?;
    Ok(dir)
}

/// 指定账号的 ipatool HOME
pub fn ipatool_home_for(email: &str) -> Result<PathBuf> {
    let safe: String = email
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = ipatool_accounts_root()?.join(safe);
    create_private_dir(&dir)?;
    Ok(dir)
}

/// 早期单账号时代的 ipatool HOME（用于迁移）
pub fn legacy_ipatool_home() -> Result<PathBuf> {
    Ok(config_dir()?.join("ipatool-home"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn keychain_passphrase_has_at_least_128_bits_of_entropy() {
        let passphrase = super::random_passphrase();
        assert!(passphrase.len() >= 32, "passphrase is too short");
    }
}
