//! IPA 文件元数据读取

use crate::error::Result;
use std::io::Read;
use std::path::Path;

#[derive(Default)]
pub struct IpaMetadata {
    pub bundle_id: Option<String>,
    pub bundle_version: Option<String>,
    pub display_name: Option<String>,
}

pub fn read_metadata(ipa: &Path) -> Result<IpaMetadata> {
    let file = std::fs::File::open(ipa)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if name.contains(".app/Info.plist") && !name.contains("/Watch/") {
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_err() {
                continue;
            }
            if let Ok(plist::Value::Dictionary(d)) = plist::from_bytes::<plist::Value>(&buf) {
                let get = |k: &str| d.get(k).and_then(|v| v.as_string()).map(String::from);
                return Ok(IpaMetadata {
                    bundle_id: get("CFBundleIdentifier"),
                    bundle_version: get("CFBundleShortVersionString")
                        .or_else(|| get("CFBundleVersion")),
                    display_name: get("CFBundleDisplayName").or_else(|| get("CFBundleName")),
                });
            }
        }
    }
    Ok(IpaMetadata::default())
}
