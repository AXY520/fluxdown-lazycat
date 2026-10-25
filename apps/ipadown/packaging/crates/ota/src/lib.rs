//! ipadown-ota: 浏览器 OTA 安装链接与 manifest 生成。
//!
//! 使用 Apple `itms-services` 机制：Safari 打开安装链接后，
//! 系统从受信 HTTPS 地址拉取 manifest 与 IPA。

/// 生成 Apple 要求的 XML manifest。
pub fn manifest_plist(ipa_url: &str, bundle_id: &str, bundle_version: &str, title: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>items</key>
	<array>
		<dict>
			<key>assets</key>
			<array>
				<dict>
					<key>kind</key>
					<string>software-package</string>
					<key>url</key>
					<string>{}</string>
				</dict>
			</array>
			<key>metadata</key>
			<dict>
				<key>bundle-identifier</key>
				<string>{}</string>
				<key>bundle-version</key>
				<string>{}</string>
				<key>kind</key>
				<string>software</string>
				<key>title</key>
				<string>{}</string>
			</dict>
		</dict>
	</array>
</dict>
</plist>
"#,
        xml_escape(ipa_url),
        xml_escape(bundle_id),
        xml_escape(bundle_version),
        xml_escape(title),
    )
}

pub fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_escapes_dynamic_values() {
        let manifest = manifest_plist(
            "https://example.test/a.ipa?x=1&y=2",
            "com.example.app",
            "1.0",
            "A < B",
        );
        assert!(manifest.contains("x=1&amp;y=2"));
        assert!(manifest.contains("A &lt; B"));
    }

    #[test]
    fn url_encode_is_safe_for_itms_manifest_urls() {
        assert_eq!(
            url_encode("https://example.test/a b"),
            "https%3A%2F%2Fexample.test%2Fa%20b"
        );
    }
}
