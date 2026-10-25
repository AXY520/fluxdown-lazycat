use serde::{Deserialize, Serialize};

/// 苹果中国 CDN（ksyuncdn）会把 fileSizeBytes 序列化成字符串（"998750208"），
/// 数字与数字字符串都接受。
fn flexible_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("fileSizeBytes 不是合法的非负整数")),
        Some(Value::String(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("fileSizeBytes 不是合法的非负整数")),
        Some(_) => Err(serde::de::Error::custom("fileSizeBytes 不是合法的非负整数")),
    }
}

/// 搜索/lookup 返回的 App 信息（字段名对齐 iTunes Search API）
#[derive(Serialize, Deserialize)]
pub struct Software {
    #[serde(rename(deserialize = "trackId", serialize = "id"))]
    pub id: i64,
    #[serde(rename(deserialize = "bundleId", serialize = "bundle_id"))]
    pub bundle_id: String,
    #[serde(rename(deserialize = "trackName", serialize = "name"))]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(rename(deserialize = "artistName", serialize = "artist_name"), default)]
    pub artist_name: Option<String>,
    #[serde(
        rename(deserialize = "artworkUrl512", serialize = "artwork_url"),
        default
    )]
    pub artwork_url: Option<String>,
    #[serde(
        rename(deserialize = "minimumOsVersion", serialize = "minimum_os_version"),
        default
    )]
    pub minimum_os_version: Option<String>,
    #[serde(
        rename(deserialize = "formattedPrice", serialize = "formatted_price"),
        default
    )]
    pub formatted_price: Option<String>,
    #[serde(
        rename(deserialize = "fileSizeBytes", serialize = "file_size_bytes"),
        deserialize_with = "flexible_u64",
        default
    )]
    /// 当前版本安装包大小（字节），用作 latest 下载的进度条总量
    pub file_size_bytes: Option<u64>,
}
