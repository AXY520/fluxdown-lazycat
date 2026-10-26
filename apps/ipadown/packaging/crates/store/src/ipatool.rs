//! ipatool 二进制封装层
//!
//! 背景：2026-08 起苹果为商店登录/下载端点强制启用 SAP 签名
//! （X-Apple-ActionSignature + 设备证明），纯 REST 复现一律 HTTP 403。
//! ipatool v2.4+ 通过 Unicorn 模拟 Apple storeagent 完成签名，是目前唯一
//! 可用的开源实现，因此登录/下载改走 ipatool 子进程。
//! 历史版本目录由 `history` 模块直接查询爱果接口。
//! 参考：https://github.com/majd/ipatool/issues/522

use crate::config::{ipatool_home_for, legacy_ipatool_home, AccountMeta, Config};
use crate::error::{Result, StoreError};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const IPATOOL_BIN: &str = "ipatool";

pub struct SessionInfo {
    pub email: String,
}

pub struct Ipatool {
    home: PathBuf,
    passphrase: String,
}

impl Ipatool {
    /// 当前选中账号的 ipatool（账号与下载操作走它）
    pub fn new(cfg: &Config) -> Result<Self> {
        let email = cfg
            .current_meta()
            .map(|m| m.email.clone())
            .ok_or(StoreError::NoAccount)?;
        Self::for_account(cfg, &email)
    }

    /// 指定账号的 ipatool
    pub fn for_account(cfg: &Config, email: &str) -> Result<Self> {
        Ok(Ipatool {
            home: ipatool_home_for(email)?,
            passphrase: cfg.keychain_passphrase.clone(),
        })
    }

    fn base_args(&self) -> Vec<String> {
        vec![
            "--keychain-passphrase".into(),
            self.passphrase.clone(),
            "--non-interactive".into(),
            "--format".into(),
            "json".into(),
        ]
    }

    /// 网络类错误（连接被重置/超时等）自动重试，最多 3 次
    async fn run(&self, args: &[&str]) -> Result<serde_json::Value> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.run_once(args).await {
                Err(StoreError::AuthFailed(msg)) if is_network_error(&msg) && attempt < 3 => {
                    crate::log_warn!(
                        "ipatool",
                        "network error, retrying cmd={:?} attempt={attempt} error={msg}",
                        log_argv(args)
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(u64::from(attempt) * 2))
                        .await;
                    continue;
                }
                other => return other,
            }
        }
    }

    /// 运行 ipatool 子命令，返回最后一行 JSON（约 10 分钟超时，首次登录需下载并模拟 storeagent）
    async fn run_once(&self, args: &[&str]) -> Result<serde_json::Value> {
        let started = std::time::Instant::now();
        let mut cmd = Command::new(IPATOOL_BIN);
        cmd.args(args)
            .args(self.base_args())
            .env("HOME", &self.home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .kill_on_drop(true);
        let output =
            match tokio::time::timeout(std::time::Duration::from_secs(600), cmd.output()).await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => {
                    return Err(StoreError::Config(format!(
                        "ipatool 启动失败（镜像内是否包含 ipatool 二进制？）: {e}"
                    )))
                }
                Err(_) => return Err(StoreError::UnexpectedResponse("ipatool 执行超时".into())),
            };
        let elapsed_ms = started.elapsed().as_millis();
        let argv = log_argv(args);
        if output.status.success() {
            crate::log_info!("ipatool", "ok cmd={argv} dur={elapsed_ms}ms");
        } else {
            crate::log_warn!(
                "ipatool",
                "failed cmd={argv} exit={} dur={elapsed_ms}ms",
                output.status.code().unwrap_or(-1)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // 特例：非交互模式下"需要 2FA"只是一条普通 info 日志（无 success/error 字段，
        // 见 ipatool cmd/auth.go：打印后直接 return nil），必须全文检测
        let combined = format!("{stdout}\n{stderr}").to_lowercase();
        if combined.contains("2fa code is required") || combined.contains("--auth-code") {
            crate::log_warn!("ipatool", "2fa challenge cmd={argv}");
            return Err(StoreError::CodeRequired);
        }

        // zerolog 每次日志一行 JSON。注意：部分命令（如 list-purchases）的成功
        // 负载行没有 success 字段，因此以进程退出码区分成败（cobra 出错即 exit 1）
        let jsons: Vec<serde_json::Value> = stdout
            .lines()
            .chain(stderr.lines())
            .filter(|l| l.trim_start().starts_with('{'))
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .collect();

        if output.status.success() {
            if let Some(json) = jsons
                .iter()
                .find(|j| j.get("success").and_then(|v| v.as_bool()) == Some(true))
                .or_else(|| jsons.last())
            {
                return Ok(json.clone());
            }
            return Err(StoreError::UnexpectedResponse(format!(
                "ipatool 无 JSON 输出: {}",
                combined.chars().take(200).collect::<String>()
            )));
        }

        let msg = jsons
            .iter()
            .rev()
            .find_map(|j| {
                ["error", "messages", "msg"]
                    .iter()
                    .find_map(|k| j.get(k).and_then(|v| v.as_str()))
            })
            .map(String::from)
            .unwrap_or_else(|| {
                let tail = combined.chars().take(200).collect::<String>();
                format!("ipatool 输出无法解析: {tail}")
            });
        crate::log_error!("ipatool", "error cmd={argv} msg={msg}");
        let lower = msg.to_lowercase();
        if lower.contains("2fa") || lower.contains("auth code") || lower.contains("verification") {
            return Err(StoreError::CodeRequired);
        }
        if lower.contains("not been found in the keyring")
            || lower.contains("failed to get account")
        {
            return Err(StoreError::NoAccount);
        }
        Err(StoreError::AuthFailed(msg))
    }

    pub async fn login(
        &self,
        email: &str,
        password: &str,
        code: Option<&str>,
    ) -> Result<SessionInfo> {
        let mut args = vec!["auth", "login", "-e", email, "-p", password];
        if let Some(c) = code {
            if !c.is_empty() {
                args.push("--auth-code");
                args.push(c);
            }
        }
        let json = self.run(&args).await?;
        Ok(SessionInfo {
            email: json
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or(email)
                .to_string(),
        })
    }

    async fn session(&self) -> Result<Option<SessionInfo>> {
        match self.run(&["auth", "info"]).await {
            Ok(json) => Ok(json
                .get("email")
                .and_then(|v| v.as_str())
                .map(|e| SessionInfo {
                    email: e.to_string(),
                })),
            Err(StoreError::NoAccount) => Ok(None),
            Err(StoreError::AuthFailed(m)) if m.contains("keyring") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 主动验证会话是否仍然有效
    /// （auth info 只读本地钥匙串不代表会话有效，需发真实商店请求）
    pub async fn validate_session(&self) -> Result<SessionInfo> {
        let session = self.session().await?.ok_or(StoreError::NoAccount)?;
        self.run(&["list-purchases", "--max-results", "1"]).await?;
        Ok(session)
    }

    pub async fn logout(&self) -> Result<()> {
        match self.run(&["auth", "revoke"]).await {
            Ok(_) => Ok(()),
            Err(StoreError::NoAccount) => Ok(()),
            Err(StoreError::AuthFailed(m)) if m.contains("keyring") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 下载（--purchase 自动获取 license）
    pub async fn download(
        &self,
        bundle_id: &str,
        version_id: Option<&str>,
        output: &Path,
        platform: crate::search::DevicePlatform,
    ) -> Result<()> {
        let out = output.to_string_lossy().to_string();
        let mut args = vec![
            "download",
            "--bundle-identifier",
            bundle_id,
            "--purchase",
            "--platform",
            platform.ipatool_value(),
            "-o",
            &out,
        ];
        if let Some(v) = version_id {
            args.push("--external-version-id");
            args.push(v);
        }
        self.run(&args).await?;
        if output.exists() {
            Ok(())
        } else {
            Err(StoreError::UnexpectedResponse(format!(
                "下载完成但找不到产物: {}",
                output.display()
            )))
        }
    }

    /// 列出应用可下载的历史版本 ID（与下载接口同源，数据可靠）。
    /// 爱果目录不可用时的降级数据源：只返回版本 ID，无发布日期/体积。
    /// 返回最新在前（ipatool 顺序为从旧到新）。
    /// 账号未拥有该 App 时自动 purchase（免费 App 获取 license）后重试一次。
    pub async fn list_versions(
        &self,
        bundle_id: &str,
        platform: crate::search::DevicePlatform,
    ) -> Result<Vec<String>> {
        let args = [
            "list-versions",
            "--bundle-identifier",
            bundle_id,
            "--platform",
            platform.ipatool_value(),
        ];
        match self.run(&args).await {
            Ok(json) => extract_version_ids(&json),
            Err(StoreError::AuthFailed(msg)) if msg.contains("license is required") => {
                crate::log_info!(
                    "ipatool",
                    "license required, purchasing free app bundle={bundle_id}"
                );
                self.run(&[
                    "purchase",
                    "--bundle-identifier",
                    bundle_id,
                    "--platform",
                    platform.ipatool_value(),
                ])
                .await?;
                let json = self.run(&args).await?;
                extract_version_ids(&json)
            }
            other => other.and_then(|json| extract_version_ids(&json)),
        }
    }
}

/// 从 list-versions 成功行提取版本 ID 列表（zerolog Interface 字段，json 格式下
/// 为字符串数组），过滤非法项并倒序（ipatool 从旧到新 → 最新在前）。
fn extract_version_ids(json: &serde_json::Value) -> Result<Vec<String>> {
    let Some(ids) = json
        .get("externalVersionIdentifiers")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .collect::<Vec<_>>()
        })
    else {
        return Err(StoreError::UnexpectedResponse(
            "ipatool 未返回版本列表".into(),
        ));
    };
    let mut ids = ids;
    if ids.is_empty() {
        return Err(StoreError::UnexpectedResponse(
            "ipatool 版本列表为空".into(),
        ));
    }
    ids.reverse();
    Ok(ids)
}

#[cfg(test)]
mod list_versions_tests {
    use super::extract_version_ids;

    #[test]
    fn extracts_filters_and_reverses_version_ids() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"level":"info","bundleID":"com.tencent.xin","externalVersionIdentifiers":["3328911","3373135","893607403","","x"],"success":true}"#,
        )
        .unwrap();
        assert_eq!(
            extract_version_ids(&json).unwrap(),
            ["893607403", "3373135", "3328911"]
        );
    }

    #[test]
    fn rejects_missing_or_empty_identifier_lists() {
        assert!(extract_version_ids(&serde_json::json!({"success": true})).is_err());
        assert!(extract_version_ids(&serde_json::json!({
            "externalVersionIdentifiers": []
        }))
        .is_err());
        assert!(extract_version_ids(&serde_json::json!({
            "externalVersionIdentifiers": "not-an-array"
        }))
        .is_err());
    }
}

// ---------- 多账号辅助 ----------

/// 用于日志的参数列表：密码（-p 后面的值）打码，其余原样。
fn log_argv(args: &[&str]) -> String {
    let mut redacted = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            redacted.push("***");
            hide_next = false;
        } else if *arg == "-p" {
            redacted.push(arg);
            hide_next = true;
        } else {
            redacted.push(arg);
        }
    }
    redacted.join(" ")
}

/// 解密钥匙串 JWE，读取账号元数据（邮箱/姓名/storeFront→国家）
pub fn read_account_meta(cfg: &Config, email: &str) -> Option<AccountMeta> {
    let jwe_path = ipatool_home_for(email)
        .ok()?
        .join(".ipatool")
        .join("account");
    let compact = std::fs::read_to_string(jwe_path).ok()?;
    let plain = crate::jwe::decrypt_pbes2(&compact, cfg.keychain_passphrase.as_bytes()).ok()?;
    parse_account_meta(&plain, email)
}

fn parse_account_meta(plain: &[u8], email: &str) -> Option<AccountMeta> {
    let keyring_item: serde_json::Value = serde_json::from_slice(plain).ok()?;
    let v = if let Some(data) = keyring_item.get("Data").and_then(|value| value.as_str()) {
        if keyring_item.get("Key").and_then(|value| value.as_str()) != Some("account") {
            return None;
        }
        let decoded = STANDARD.decode(data).ok()?;
        serde_json::from_slice(&decoded).ok()?
    } else {
        keyring_item
    };
    let storefront = v.get("storeFront").and_then(|s| s.as_str()).unwrap_or("");
    let sf_id = storefront.split(['-', ',']).next().unwrap_or("");
    Some(AccountMeta {
        email: v
            .get("email")
            .and_then(|s| s.as_str())
            .unwrap_or(email)
            .to_string(),
        name: v.get("name").and_then(|s| s.as_str()).map(String::from),
        country: crate::countries::country_for_storefront(sf_id).map(String::from),
    })
}

/// 迁移早期单账号目录（ipatool-home/.ipatool）到多账号结构，并把未登记的账号目录注册进配置
pub fn migrate_legacy(cfg: &mut Config) {
    // 1. 迁移根目录遗留钥匙串
    if let Ok(legacy) = legacy_ipatool_home() {
        let jwe = legacy.join(".ipatool").join("account");
        if jwe.exists() {
            if let Ok(compact) = std::fs::read_to_string(&jwe) {
                if let Ok(plain) =
                    crate::jwe::decrypt_pbes2(&compact, cfg.keychain_passphrase.as_bytes())
                {
                    if let Some(meta) = parse_account_meta(&plain, "") {
                        let email = meta.email.clone();
                        if !email.is_empty() {
                            let mut account_ready = false;
                            if let Ok(new_home) = ipatool_home_for(&email) {
                                let destination = new_home.join(".ipatool");
                                account_ready = destination.exists()
                                    || std::fs::rename(legacy.join(".ipatool"), &destination)
                                        .is_ok();
                                // 目标账号目录已存在时，根目录副本是旧版留下的重复钥匙串。
                                if account_ready
                                    && jwe.exists()
                                    && destination.join("account").exists()
                                {
                                    let _ = std::fs::remove_dir_all(legacy.join(".ipatool"));
                                }
                            }
                            if account_ready {
                                cfg.upsert_account(meta);
                                if cfg.current_account.is_none() {
                                    cfg.current_account = Some(email);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // 2. 注册散落在 accounts/ 下但未登记的账号目录
    if let Ok(root) = crate::config::ipatool_accounts_root() {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let email = e.file_name().to_string_lossy().to_string();
                if !e.path().join(".ipatool").join("account").exists() {
                    continue;
                }
                if cfg.accounts.iter().any(|a| a.email == email) {
                    continue;
                }
                if let Some(meta) = read_account_meta(cfg, &email) {
                    cfg.upsert_account(meta);
                }
            }
        }
    }
    // 旧版把文件钥匙串的外层 Item 当成账号 JSON，已登记账号会缺少姓名和地区。
    // 每次启动重新读取一次真实账号数据，自动修复已有配置，无需用户重新登录。
    let registered_emails: Vec<String> = cfg
        .accounts
        .iter()
        .map(|account| account.email.clone())
        .collect();
    for email in registered_emails {
        if let Some(meta) = read_account_meta(cfg, &email) {
            cfg.upsert_account(meta);
        }
    }
    let _ = cfg.save();
}

/// 可重试的网络层错误（GFW 重置/抖动）
fn is_network_error(msg: &str) -> bool {
    let l = msg.to_lowercase();
    [
        "connection reset",
        "timed out",
        "i/o timeout",
        "timeout",
        "eof",
        "connection refused",
        "no route to host",
        "network is unreachable",
        "tls handshake",
        "temporary failure",
    ]
    .iter()
    .any(|p| l.contains(p))
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::STANDARD, Engine};

    #[test]
    fn parses_account_metadata_from_file_keyring_item() {
        let account = serde_json::json!({
            "name": "Test User",
            "email": "test@example.com",
            "storeFront": "143465-19,29"
        });
        let item = serde_json::json!({
            "Key": "account",
            "Data": STANDARD.encode(serde_json::to_vec(&account).unwrap()),
            "Label": "ipatool",
            "Description": "",
            "KeychainNotTrustApplication": false,
            "KeychainNotSynchronizable": false
        });

        let meta =
            super::parse_account_meta(&serde_json::to_vec(&item).unwrap(), "fallback@example.com")
                .unwrap();

        assert_eq!(meta.email, "test@example.com");
        assert_eq!(meta.name.as_deref(), Some("Test User"));
        assert_eq!(meta.country.as_deref(), Some("CN"));
    }
}
