//! Native chrome only. Catalog credit: Monocode PR #1073, kev17in, MIT.
//! https://github.com/hardbeat920/monocode/pull/1073
use std::{cell::Cell, collections::HashMap, sync::LazyLock};
thread_local! {static CHINESE: Cell<bool> = const {Cell::new(false)};}
static CATALOG: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("i18n/zh-CN.json")).expect("bundled locale")
});
static SYSTEM_ZH: LazyLock<bool> = LazyLock::new(|| {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = text
            .lines()
            .map(str::trim)
            .find(|s| !s.is_empty() && !s.starts_with('(') && !s.starts_with(')'))
        {
            return first
                .trim_matches(|c: char| c == '"' || c == ',' || c.is_whitespace())
                .starts_with("zh");
        }
    }
    std::env::var("LANG").unwrap_or_default().starts_with("zh")
});
pub(crate) fn configure(p: &crate::settings::Preferences) {
    let chinese = crate::paseo_host::project_ui(p)
        && match p.native_locale.as_deref() {
            Some("zh-CN") => true,
            Some("en") => false,
            _ => *SYSTEM_ZH,
        };
    CHINESE.with(|s| s.set(chinese));
}
pub(crate) fn language() -> &'static str {
    if CHINESE.with(Cell::get) {
        "zh-CN"
    } else {
        "en"
    }
}
pub(crate) fn t(key: &str) -> String {
    if CHINESE.with(Cell::get) {
        CATALOG.get(key).cloned().unwrap_or_else(|| key.into())
    } else {
        key.into()
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn chrome_translation_is_opt_in_and_falls_back() {
        let mut p = crate::settings::Preferences {
            native_locale: Some("zh-CN".into()),
            ..Default::default()
        };
        super::configure(&p);
        assert_eq!(super::t("New chat"), "New chat");
        p.extra.insert(
            crate::paseo_host::PROJECT_SELECTION.into(),
            serde_json::json!({}),
        );
        super::configure(&p);
        assert_eq!(super::t("New chat"), "新建对话");
        assert_eq!(
            super::t("Synthetic untouched prose"),
            "Synthetic untouched prose"
        );
        p.native_locale = Some("en".into());
        super::configure(&p);
        assert_eq!(super::t("New chat"), "New chat");
    }
}
