use crate::error::{Result, StoreError};
use crate::http::client_follow;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::Duration;

const AGZY_VERSION_URL: &str = "https://app.agzy.cn/searchVersion";
const APP_STORE_PAGE_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    String(String),
    Number(u64),
}

impl StringOrNumber {
    fn into_string(self) -> String {
        match self {
            Self::String(value) => value,
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgzyVersion {
    version: String,
    version_id: StringOrNumber,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    create_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgzyResponse {
    code: u16,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    bundle_id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    data: Vec<AgzyVersion>,
}

pub struct HistoricalVersion {
    pub version: String,
    pub version_id: String,
    pub size: Option<String>,
    pub release_date: Option<String>,
    pub release_notes: Option<String>,
}

pub struct VersionCatalog {
    pub name: String,
    pub bundle_id: String,
    pub current_version: String,
    pub updated_at: String,
    pub versions: Vec<HistoricalVersion>,
}

impl TryFrom<AgzyResponse> for VersionCatalog {
    type Error = StoreError;

    fn try_from(response: AgzyResponse) -> Result<Self> {
        if response.code != 200 {
            return Err(StoreError::UnexpectedResponse(format!(
                "爱果历史版本接口返回 {}: {}",
                response.code, response.msg
            )));
        }
        if response.bundle_id.is_empty() || response.data.is_empty() {
            return Err(StoreError::UnexpectedResponse(
                "爱果历史版本接口未返回版本记录".into(),
            ));
        }
        let versions = response
            .data
            .into_iter()
            .map(|item| {
                let version_id = item.version_id.into_string();
                if version_id.is_empty()
                    || version_id.len() > 32
                    || !version_id.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(StoreError::UnexpectedResponse(
                        "爱果历史版本接口返回了非法版本 ID".into(),
                    ));
                }
                Ok(HistoricalVersion {
                    version: item.version,
                    version_id,
                    size: item.size.filter(|value| !value.trim().is_empty()),
                    release_date: item.create_time.filter(|value| !value.trim().is_empty()),
                    release_notes: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            name: response.name,
            bundle_id: response.bundle_id,
            current_version: response.version,
            updated_at: response.time,
            versions,
        })
    }
}

fn validate_input(app_id: i64, country: &str) -> Result<String> {
    if app_id <= 0 {
        return Err(StoreError::UnexpectedResponse("非法 App Store ID".into()));
    }
    let country = country.trim().to_ascii_lowercase();
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(StoreError::UnexpectedResponse("非法国家/地区代码".into()));
    }
    Ok(country)
}

async fn agzy_versions(app_id: i64, country: &str) -> Result<VersionCatalog> {
    let response: AgzyResponse = client_follow()?
        .post(AGZY_VERSION_URL)
        .timeout(Duration::from_secs(20))
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("appid", app_id.to_string()),
            ("country", country.to_owned()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    response.try_into()
}

fn serialized_server_data(body: &str) -> Result<&str> {
    let double_quoted = body.find(r#"id="serialized-server-data""#);
    let single_quoted = body.find("id='serialized-server-data'");
    let marker = match (double_quoted, single_quoted) {
        (Some(left), Some(right)) => left.min(right),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => {
            return Err(StoreError::UnexpectedResponse(
                "App Store 页面未包含版本数据".into(),
            ));
        }
    };
    if body[..marker].rfind("<script").is_none() {
        return Err(StoreError::UnexpectedResponse(
            "App Store 版本数据格式无效".into(),
        ));
    }
    let content_start = marker
        + body[marker..]
            .find('>')
            .ok_or_else(|| StoreError::UnexpectedResponse("App Store 版本数据格式无效".into()))?
        + 1;
    let content_end = content_start
        + body[content_start..]
            .find("</script>")
            .ok_or_else(|| StoreError::UnexpectedResponse("App Store 版本数据未完整返回".into()))?;
    Ok(body[content_start..content_end].trim())
}

fn is_version_history_page(object: &Map<String, Value>) -> bool {
    object
        .get("pageMetrics")
        .and_then(|value| value.pointer("/pageFields/pageType"))
        .and_then(Value::as_str)
        == Some("VersionHistory")
        || object
            .get("pageRenderMetrics")
            .and_then(|value| value.get("pageType"))
            .and_then(Value::as_str)
            == Some("VersionHistory")
}

fn collect_note_items(value: &Value, notes: &mut HashMap<String, String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_note_items(item, notes);
            }
        }
        Value::Object(object) => {
            if object.get("$kind").and_then(Value::as_str) == Some("TitledParagraph") {
                let version = object
                    .get("primarySubtitle")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default();
                if !version.is_empty() && !text.is_empty() {
                    notes
                        .entry(version.to_owned())
                        .or_insert_with(|| text.to_owned());
                }
            }
            for child in object.values() {
                collect_note_items(child, notes);
            }
        }
        _ => {}
    }
}

fn collect_version_history(value: &Value, notes: &mut HashMap<String, String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_version_history(item, notes);
            }
        }
        Value::Object(object) => {
            if object.get("page").and_then(Value::as_str) == Some("versionHistory") {
                if let Some(page_data) = object.get("pageData") {
                    collect_note_items(page_data, notes);
                }
                return;
            }
            if is_version_history_page(object) {
                if let Some(shelves) = object.get("shelves") {
                    collect_note_items(shelves, notes);
                }
                return;
            }
            for child in object.values() {
                collect_version_history(child, notes);
            }
        }
        _ => {}
    }
}

fn parse_app_store_release_notes(body: &str) -> Result<HashMap<String, String>> {
    let data: Value = serde_json::from_str(serialized_server_data(body)?)?;
    let mut notes = HashMap::new();
    collect_version_history(&data, &mut notes);
    if notes.is_empty() {
        return Err(StoreError::UnexpectedResponse(
            "App Store 页面未返回版本更新日志".into(),
        ));
    }
    Ok(notes)
}

async fn app_store_release_notes(app_id: i64, country: &str) -> Result<HashMap<String, String>> {
    let url = format!("https://apps.apple.com/{country}/app/id{app_id}");
    let response = client_follow()?
        .get(url)
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > APP_STORE_PAGE_LIMIT as u64)
    {
        return Err(StoreError::UnexpectedResponse(
            "App Store 版本页面过大".into(),
        ));
    }
    let body = response.bytes().await?;
    if body.len() > APP_STORE_PAGE_LIMIT {
        return Err(StoreError::UnexpectedResponse(
            "App Store 版本页面过大".into(),
        ));
    }
    let body = std::str::from_utf8(&body)
        .map_err(|_| StoreError::UnexpectedResponse("App Store 版本页面不是 UTF-8".into()))?;
    parse_app_store_release_notes(body)
}

fn attach_release_notes(catalog: &mut VersionCatalog, mut notes: HashMap<String, String>) {
    for version in &mut catalog.versions {
        version.release_notes = notes.remove(version.version.trim());
    }
}

/// 爱果提供下载所需的历史版本 ID，App Store 提供官方更新日志。
/// 仅发送公开的 App Store ID 与国家代码，不发送登录信息。
pub async fn versions(app_id: i64, country: &str) -> Result<VersionCatalog> {
    let country = validate_input(app_id, country)?;
    let (catalog, release_notes) = tokio::join!(
        agzy_versions(app_id, &country),
        app_store_release_notes(app_id, &country)
    );
    let mut catalog = catalog?;
    match release_notes {
        Ok(notes) => {
            let attached = notes.len();
            attach_release_notes(&mut catalog, notes);
            crate::log_info!(
                "history",
                "catalog fetched app_id={app_id} country={} versions={} notes={attached}",
                country.to_ascii_uppercase(),
                catalog.versions.len()
            );
        }
        Err(error) => {
            crate::log_warn!(
                "history",
                "release notes unavailable app_id={app_id} country={} error={error}",
                country.to_ascii_uppercase()
            );
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_and_string_version_ids() {
        let response: AgzyResponse = serde_json::from_str(
            r#"{
                "code": 200,
                "name": "微信",
                "bundleId": "com.tencent.xin",
                "version": "8.0.78",
                "time": "2026-09-25",
                "data": [
                    {"version":"8.0.78","versionId":890654806,"size":"887.4 MB","createTime":"2026-09-08"},
                    {"version":"8.0.76","versionId":"887344807","size":"","createTime":""}
                ]
            }"#,
        )
        .unwrap();
        let catalog = VersionCatalog::try_from(response).unwrap();

        assert_eq!(catalog.bundle_id, "com.tencent.xin");
        assert_eq!(catalog.versions[0].version_id, "890654806");
        assert_eq!(
            catalog.versions[0].release_date.as_deref(),
            Some("2026-09-08")
        );
        assert_eq!(catalog.versions[1].version_id, "887344807");
        assert_eq!(catalog.versions[1].size, None);
        assert_eq!(catalog.versions[1].release_notes, None);
    }

    #[test]
    fn rejects_non_numeric_version_ids_from_remote_source() {
        let response: AgzyResponse = serde_json::from_str(
            r#"{
                "code": 200,
                "bundleId": "com.example.app",
                "data": [{"version":"1.0","versionId":"')-alert(1)-('"}]
            }"#,
        )
        .unwrap();

        assert!(VersionCatalog::try_from(response).is_err());
    }

    #[test]
    fn parses_only_app_store_version_history_notes() {
        let body = r#"<html><script type="application/json" id="serialized-server-data">{
            "data": [{"data": {
                "shelfMapping": {"mostRecentVersion": {
                    "items": [{"$kind":"TitledParagraph","primarySubtitle":"版本 8.0.78","text":"当前版本摘要"}],
                    "seeAllAction": {"page":"versionHistory","pageData": {
                        "shelves": [{"items": [
                            {"$kind":"TitledParagraph","primarySubtitle":"8.0.78","secondarySubtitle":"date","text":"  修复问题  "},
                            {"$kind":"TitledParagraph","primarySubtitle":"8.0.76","secondarySubtitle":"date","text":"新增功能"}
                        ]}],
                        "pageMetrics":{"pageFields":{"pageType":"VersionHistory"}}
                    }}
                }}
            }}]
        }</script></html>"#;

        let notes = parse_app_store_release_notes(body).unwrap();

        assert_eq!(notes.len(), 2);
        assert_eq!(notes.get("8.0.78").map(String::as_str), Some("修复问题"));
        assert_eq!(notes.get("8.0.76").map(String::as_str), Some("新增功能"));
        assert!(!notes.contains_key("版本 8.0.78"));
    }

    #[test]
    fn parses_single_quoted_serialized_data_marker() {
        let body = r#"<script data-extra="yes" id='serialized-server-data'>{
            "pageRenderMetrics":{"pageType":"VersionHistory"},
            "shelves":[{"items":[
                {"$kind":"TitledParagraph","primarySubtitle":"1.2.3","text":"Changes"}
            ]}]
        }</script>"#;

        let notes = parse_app_store_release_notes(body).unwrap();

        assert_eq!(notes.get("1.2.3").map(String::as_str), Some("Changes"));
    }

    #[test]
    fn attaches_notes_only_to_matching_versions() {
        let response: AgzyResponse = serde_json::from_str(
            r#"{
                "code": 200,
                "bundleId": "com.example.app",
                "data": [
                    {"version":"2.0","versionId":"20"},
                    {"version":"1.0","versionId":"10"}
                ]
            }"#,
        )
        .unwrap();
        let mut catalog = VersionCatalog::try_from(response).unwrap();
        attach_release_notes(
            &mut catalog,
            HashMap::from([
                ("2.0".to_owned(), "Latest changes".to_owned()),
                ("3.0".to_owned(), "Unmatched".to_owned()),
            ]),
        );

        assert_eq!(
            catalog.versions[0].release_notes.as_deref(),
            Some("Latest changes")
        );
        assert_eq!(catalog.versions[1].release_notes, None);
    }
}
