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

/// 按 App Store ID（trackId）查找（ipatool 降级路径：list-versions 需要 bundle_id）
pub async fn lookup_by_id(app_id: i64, country: &str) -> Result<Software> {
    let client = client_follow()?;
    let resp: SearchResponse = client
        .get("https://itunes.apple.com/lookup")
        .query(&[
            ("id", app_id.to_string()),
            ("country", country.to_string()),
            ("entity", "software".to_string()),
            ("limit", "1".to_string()),
            ("media", "software".to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    resp.results
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::UnexpectedResponse(format!("未找到应用 id{app_id}")))
}

/// 按多个 App Store ID 批量查找（lookup 端点支持逗号合并，一次请求补全字段）
async fn lookup_by_ids(app_ids: &[i64], country: &str) -> Result<Vec<Software>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = app_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let client = client_follow()?;
    let resp: SearchResponse = client
        .get("https://itunes.apple.com/lookup")
        .query(&[
            ("id", ids),
            ("country", country.to_string()),
            ("entity", "software".to_string()),
            ("limit", format!("{}", app_ids.len())),
            ("media", "software".to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.results)
}

/// 爱果搜索（iTunes search 被限流时的降级源）。
/// 返回 appId 无 bundleId，需再用 lookup 批量补全（见 search_with_fallback）。
async fn agzy_search(term: &str, country: &str, limit: usize) -> Result<Vec<i64>> {
    #[derive(serde::Deserialize)]
    struct AgzySearchResponse {
        code: u16,
        #[serde(default)]
        msg: String,
        #[serde(default, deserialize_with = "crate::history::flexible_versions")]
        data: Vec<AgzySearchApp>,
    }
    #[derive(serde::Deserialize)]
    struct AgzySearchApp {
        #[serde(rename = "appId")]
        app_id: i64,
    }
    let response: AgzySearchResponse = client_follow()?
        .post("https://app.agzy.cn/searchApp")
        .timeout(std::time::Duration::from_secs(20))
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("country", country.to_ascii_uppercase()),
            ("name", term.to_owned()),
            ("limit", limit.to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if response.code != 200 {
        return Err(StoreError::Upstream(format!(
            "爱果搜索返回 {}：{}",
            response.code,
            response.msg.trim()
        )));
    }
    Ok(response.data.into_iter().map(|item| item.app_id).collect())
}

/// 搜索：iTunes search 优先；被限流（403）或网络失败时降级
/// 爱果 searchApp 拿 appId 列表 → lookup 批量补全 bundleId 等字段。
pub async fn search_with_fallback(
    term: &str,
    country: &str,
    limit: usize,
    platform: DevicePlatform,
) -> Result<Vec<Software>> {
    let limit_u32 = u32::try_from(limit).unwrap_or(15);
    match search(term, country, limit_u32, platform).await {
        Ok(results) => Ok(results),
        Err(primary) => {
            let text = primary.to_string();
            // 仅在疑似限流/网络故障时降级；参数错误（如词太长）不必
            let retryable = text.contains("403")
                || text.contains("404")
                || text.contains("网络请求失败")
                || matches!(primary, StoreError::Http(_));
            if !retryable {
                return Err(primary);
            }
            crate::log_warn!(
                "search",
                "itunes search unavailable, falling back to agzy term={term} error={primary}"
            );
            let ids = agzy_search(term, country, limit)
                .await
                .map_err(|fallback| {
                    crate::log_warn!(
                        "search",
                        "agzy search fallback failed term={term} error={fallback}"
                    );
                    StoreError::Upstream("苹果搜索接口被限流，备用搜索也不可用，请稍后再试".into())
                })?;
            let results = lookup_by_ids(&ids, country).await.map_err(|fallback| {
                crate::log_warn!(
                    "search",
                    "lookup completion failed term={term} error={fallback}"
                );
                StoreError::Upstream("苹果搜索接口被限流，应用信息补全失败，请稍后再试".into())
            })?;
            crate::log_info!(
                "search",
                "search via agzy fallback term={term} country={country} results={}",
                results.len()
            );
            Ok(results
                .into_iter()
                .take(limit)
                .filter(|app| !app.bundle_id.is_empty())
                .collect())
        }
    }
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
