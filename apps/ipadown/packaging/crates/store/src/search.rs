use crate::error::{Result, StoreError};
use crate::http::client_follow;
use crate::models::Software;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DevicePlatform {
    #[default]
    Iphone,
    Ipad,
    Appletv,
}

impl DevicePlatform {
    pub fn search_entity(self) -> &'static str {
        match self {
            Self::Iphone => "software",
            Self::Ipad => "iPadSoftware",
            Self::Appletv => "software,tvSoftware",
        }
    }

    pub fn ipatool_value(self) -> &'static str {
        match self {
            Self::Iphone => "iphone",
            Self::Ipad => "ipad",
            Self::Appletv => "appletv",
        }
    }

    pub fn file_suffix(self) -> &'static str {
        match self {
            Self::Iphone => "",
            Self::Ipad => "_ipad",
            Self::Appletv => "_appletv",
        }
    }
}

#[derive(serde::Deserialize)]
struct SearchResponse {
    results: Vec<Software>,
}

/// 公开 iTunes Search API（无需登录）
pub async fn search(
    term: &str,
    country: &str,
    limit: u32,
    platform: DevicePlatform,
) -> Result<Vec<Software>> {
    let client = client_follow()?;
    let resp: SearchResponse = client
        .get("https://itunes.apple.com/search")
        .query(&[
            ("entity", platform.search_entity()),
            ("limit", &limit.to_string()),
            ("media", "software"),
            ("term", term),
            ("country", country),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.results)
}

/// 按 bundle id 精确查找（用于下载前解析 trackId 与价格）
pub async fn lookup(bundle_id: &str, country: &str, platform: DevicePlatform) -> Result<Software> {
    let client = client_follow()?;
    let resp: SearchResponse = client
        .get("https://itunes.apple.com/lookup")
        .query(&[
            ("bundleId", bundle_id),
            ("country", country),
            ("entity", platform.search_entity()),
            ("limit", "1"),
            ("media", "software"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    resp.results
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::UnexpectedResponse(format!("未找到应用 {bundle_id}")))
}

#[cfg(test)]
mod tests {
    use super::{DevicePlatform, SearchResponse};

    #[test]
    fn platforms_map_to_store_entities_and_ipatool_values() {
        assert_eq!(DevicePlatform::Iphone.search_entity(), "software");
        assert_eq!(DevicePlatform::Ipad.search_entity(), "iPadSoftware");
        assert_eq!(
            DevicePlatform::Appletv.search_entity(),
            "software,tvSoftware"
        );
        assert_eq!(DevicePlatform::Iphone.ipatool_value(), "iphone");
        assert_eq!(DevicePlatform::Ipad.ipatool_value(), "ipad");
        assert_eq!(DevicePlatform::Appletv.ipatool_value(), "appletv");
    }

    #[test]
    fn platform_json_values_match_the_api_contract() {
        for (platform, expected) in [
            (DevicePlatform::Iphone, "\"iphone\""),
            (DevicePlatform::Ipad, "\"ipad\""),
            (DevicePlatform::Appletv, "\"appletv\""),
        ] {
            assert_eq!(serde_json::to_string(&platform).unwrap(), expected);
        }
    }

    /// 中国 CDN 会把 fileSizeBytes 序列化成字符串，真实抓包回归。
    #[test]
    fn parses_cn_cdn_search_response_with_string_file_size() {
        let raw = include_bytes!("../testdata/cn-cdn-search.json");
        let resp: SearchResponse = serde_json::from_slice(raw).unwrap();
        assert!(resp.results.len() >= 2);
        assert_eq!(resp.results[0].file_size_bytes, Some(998_750_208));
        assert_eq!(resp.results[0].bundle_id, "com.tencent.xin");
    }
}
