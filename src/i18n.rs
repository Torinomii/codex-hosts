use std::collections::BTreeMap;

use serde::Deserialize;

pub struct EmbeddedLanguage {
    pub locale: &'static str,
    pub display_name: &'static str,
    pub json: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/languages.rs"));

#[derive(Debug, Deserialize)]
struct LanguageFile {
    locale: String,
    display_name: String,
    strings: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    locale: String,
    strings: BTreeMap<String, String>,
}

impl Catalog {
    pub fn for_locale(requested: Option<&str>) -> Self {
        let requested = requested
            .map(str::to_owned)
            .or_else(sys_locale::get_locale)
            .unwrap_or_else(|| "en".to_owned());
        let selected = find_language(&requested)
            .or_else(|| find_language("en"))
            .expect("build script guarantees an English catalog");
        let parsed: LanguageFile =
            serde_json::from_str(selected.json).expect("build script validates language JSON");
        debug_assert_eq!(parsed.locale, selected.locale);
        debug_assert_eq!(parsed.display_name, selected.display_name);
        Self {
            locale: parsed.locale,
            strings: parsed.strings,
        }
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    pub fn text(&self, key: &str) -> &str {
        self.strings
            .get(key)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("validated catalog is missing key {key}"))
    }

    pub fn format(&self, key: &str, values: &[(&str, &str)]) -> String {
        let mut output = self.text(key).to_owned();
        for (name, value) in values {
            output = output.replace(&format!("{{{name}}}"), value);
        }
        output
    }

    pub fn available() -> &'static [EmbeddedLanguage] {
        EMBEDDED_LANGUAGES
    }
}

fn find_language(requested: &str) -> Option<&'static EmbeddedLanguage> {
    let normalized = requested.replace('_', "-").to_ascii_lowercase();
    EMBEDDED_LANGUAGES
        .iter()
        .find(|language| language.locale.eq_ignore_ascii_case(&normalized))
        .or_else(|| {
            let primary = normalized.split('-').next().unwrap_or(&normalized);
            EMBEDDED_LANGUAGES.iter().find(|language| {
                language
                    .locale
                    .split('-')
                    .next()
                    .is_some_and(|value| value.eq_ignore_ascii_case(primary))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_supported_system_language_family() {
        assert_eq!(Catalog::for_locale(Some("zh-Hans-SG")).locale(), "zh-CN");
        assert_eq!(Catalog::for_locale(Some("ja-JP")).locale(), "ja");
        assert_eq!(Catalog::for_locale(Some("fr-FR")).locale(), "en");
    }

    #[test]
    fn every_embedded_catalog_is_loadable() {
        for language in Catalog::available() {
            let catalog = Catalog::for_locale(Some(language.locale));
            assert!(!catalog.text("app_title").is_empty());
        }
    }

    #[test]
    fn authentication_labels_keep_hardware_files_and_agent_routes_distinct() {
        let expected = [
            (
                "en",
                "Key file / hardware key",
                "SSH Agent / Pageant",
                "Find saved hardware-key file",
            ),
            (
                "zh-CN",
                "密钥文件 / 硬件密钥",
                "SSH Agent / Pageant",
                "查找已保存的硬件密钥文件",
            ),
            (
                "zh-TW",
                "金鑰檔案 / 硬體金鑰",
                "SSH Agent / Pageant",
                "尋找已儲存的硬體金鑰檔案",
            ),
            (
                "ja",
                "鍵ファイル / ハードウェアキー",
                "SSH Agent / Pageant",
                "保存済みハードウェアキー用ファイルを検索",
            ),
        ];
        for (locale, direct, agent, find_hardware_file) in expected {
            let catalog = Catalog::for_locale(Some(locale));
            assert_eq!(catalog.text("private_key_auth"), direct);
            assert_eq!(catalog.text("ssh_agent_auth"), agent);
            assert_eq!(catalog.text("find_fido_key"), find_hardware_file);
            assert!(catalog.text("fido_setup_hint").contains("SSH"));
        }
    }
}
