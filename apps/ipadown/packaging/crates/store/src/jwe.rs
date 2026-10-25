//! JWE (PBES2-HS256+A128KW / A256GCM) 解密
//!
//! ipatool 的钥匙串文件后端（byteness/keyring）将账号以 JWE compact
//! 序列化存储在 ~/.ipatool/account，口令即 keychain-passphrase。
//! 解密后得到账号 JSON，用于构建原生商店会话。

use crate::error::{Result, StoreError};
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

fn jwe_err(msg: &str) -> StoreError {
    StoreError::UnexpectedResponse(format!("JWE 解密失败: {msg}"))
}

pub fn decrypt_pbes2(compact: &str, passphrase: &[u8]) -> Result<Vec<u8>> {
    let parts: Vec<&str> = compact.trim().split('.').collect();
    if parts.len() != 5 {
        return Err(jwe_err("分段数量不为 5"));
    }
    let header_b64 = parts[0];
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|e| jwe_err(&e.to_string()))?;
    let header: serde_json::Value =
        serde_json::from_slice(&header_bytes).map_err(|e| jwe_err(&e.to_string()))?;

    let alg = header["alg"].as_str().unwrap_or("");
    if alg != "PBES2-HS256+A128KW" {
        return Err(jwe_err(&format!("不支持的 alg: {alg}")));
    }
    let p2c = header["p2c"].as_u64().ok_or_else(|| jwe_err("缺少 p2c"))? as u32;
    let p2s = URL_SAFE_NO_PAD
        .decode(header["p2s"].as_str().ok_or_else(|| jwe_err("缺少 p2s"))?)
        .map_err(|e| jwe_err(&e.to_string()))?;

    // PBES2 salt = alg || 0x00 || p2s（RFC 7518 §4.8）
    let mut salt = Vec::with_capacity(alg.len() + 1 + p2s.len());
    salt.extend_from_slice(alg.as_bytes());
    salt.push(0);
    salt.extend_from_slice(&p2s);

    let mut kek = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase, &salt, p2c, &mut kek);

    let wrapped = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| jwe_err(&e.to_string()))?;
    let kek_obj = aes_kw::Kek::<aes::Aes128>::new((&kek).into());
    let cek = kek_obj
        .unwrap_vec(&wrapped)
        .map_err(|e| jwe_err(&format!("AES-KW 解包失败: {e}")))?;

    let iv = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|e| jwe_err(&e.to_string()))?;
    let mut data = URL_SAFE_NO_PAD
        .decode(parts[3])
        .map_err(|e| jwe_err(&e.to_string()))?;
    let tag = URL_SAFE_NO_PAD
        .decode(parts[4])
        .map_err(|e| jwe_err(&e.to_string()))?;
    data.extend_from_slice(&tag);

    let cipher = Aes256Gcm::new_from_slice(&cek).map_err(|e| jwe_err(&e.to_string()))?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &data,
                aad: header_b64.as_bytes(),
            },
        )
        .map_err(|_| jwe_err("GCM 解密失败（口令错误？）"))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    // 与 byteness/keyring 文件后端（PBES2-HS256+A128KW / A256GCM）相同格式的测试向量
    #[test]
    fn decrypt_ipatool_keychain_entry() {
        let jwe = "eyJhbGciOiJQQkVTMi1IUzI1NitBMTI4S1ciLCJlbmMiOiJBMjU2R0NNIiwicDJjIjoxMDAwMCwicDJzIjoiT005V19KUXFmVmUzQVhzWDNHOWhRdyJ9.pdO-_OPh5MHxi-T4hbb_5u5nuZTVouSoLk8oU6Y8HTpsK8Vk2QxMiA.bhyHqHOsIWP5jxPJ.ejPx4PsNXCXRuEajQKGYYm-bE_symJ3uspRghi-gIuecItdJ_D1zTr3keV5or2s-DHWlnHKGeM27PSmcRuvy7qS1ywq46fGsWBNolnODy1X8oMAQRd0ImYucqWWvbc4W2NF_lrY2Yw.8NwP7C8Hzd908OTvtDqV-w";
        let plain = super::decrypt_pbes2(jwe, b"test-passphrase-123").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(v["email"], "demo@icloud.com");
        assert_eq!(v["storeFront"], "143465-19,29");
    }
}
