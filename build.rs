use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=languages");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let language_dir = manifest.join("languages");
    let mut paths = fs::read_dir(&language_dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", language_dir.display()))
        .map(|entry| entry.expect("language directory entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "languages must contain at least one JSON file"
    );

    let mut generated = String::from("pub static EMBEDDED_LANGUAGES: &[EmbeddedLanguage] = &[\n");
    let mut reference_keys: Option<BTreeSet<String>> = None;
    let mut locales = BTreeSet::new();
    let mut has_english = false;

    for path in paths {
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        let value: serde_json::Value = serde_json::from_str(&contents)
            .unwrap_or_else(|error| panic!("invalid JSON in {}: {error}", path.display()));
        let locale = value
            .get("locale")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("{} has no locale", path.display()));
        let display_name = value
            .get("display_name")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("{} has no display_name", path.display()));
        let strings = value
            .get("strings")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{} has no strings object", path.display()));
        assert!(
            locales.insert(locale.to_ascii_lowercase()),
            "duplicate locale {locale}"
        );
        has_english |= locale.eq_ignore_ascii_case("en");
        let keys = strings.keys().cloned().collect::<BTreeSet<_>>();
        assert!(
            !keys.is_empty(),
            "{} has no translated strings",
            path.display()
        );
        if let Some(reference) = &reference_keys {
            assert_eq!(
                reference,
                &keys,
                "{} does not have exactly the same keys as the first language file",
                path.display()
            );
        } else {
            reference_keys = Some(keys);
        }
        for (key, translated) in strings {
            assert!(
                translated.as_str().is_some_and(|value| !value.is_empty()),
                "{} contains an empty or non-string value for {key}",
                path.display()
            );
        }

        generated.push_str(&format!(
            "    EmbeddedLanguage {{ locale: {locale:?}, display_name: {display_name:?}, json: {contents:?} }},\n"
        ));
    }
    assert!(
        has_english,
        "languages/en.json is required as the stable fallback"
    );
    generated.push_str("];\n");

    let output = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("languages.rs");
    fs::write(&output, generated)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", output.display()));
}
