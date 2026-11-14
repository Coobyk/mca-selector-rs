//! Localization. Upstream loads `lang/<locale>.lang` ResourceBundle-style
//! key=value files; we embed them (include_str!) so single-binary installs
//! ship every locale, and `t(key)` falls back to English then the key.

use std::collections::HashMap;
use std::sync::RwLock;

/// (code, native display name, embedded source) — Settings picker order;
/// element 0 is the English fallback.
pub(crate) const LANGS: &[(&str, &str, &str)] = &[
    ("en", "English", include_str!("../lang/en.lang")),
    ("de", "Deutsch", include_str!("../lang/de.lang")),
    ("fr", "Français", include_str!("../lang/fr.lang")),
    ("es", "Español", include_str!("../lang/es.lang")),
    ("zh_cn", "简体中文", include_str!("../lang/zh_cn.lang")),
    ("ru", "Русский", include_str!("../lang/ru.lang")),
    ("ja", "日本語", include_str!("../lang/ja.lang")),
    ("pt_br", "Português (Brasil)", include_str!("../lang/pt_br.lang")),
];

fn parse(src: &'static str) -> HashMap<&'static str, &'static str> {
    src.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim(), v.trim()))
        })
        .collect()
}

static ACTIVE: RwLock<Option<HashMap<&'static str, &'static str>>> = RwLock::new(None);

/// Select the active language (GlobalConfig.language). Called at startup and
/// again when the Settings language changes — menus are rebuilt next frame.
pub(crate) fn init(code: &str) {
    let src = LANGS
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, _, s)| *s)
        .unwrap_or(LANGS[0].2);
    *ACTIVE.write().unwrap() = Some(parse(src));
}

pub(crate) fn display_name(code: &str) -> &'static str {
    LANGS.iter().find(|(c, _, _)| *c == code).map(|(_, n, _)| *n).unwrap_or("English")
}

/// Next language code after `code` in LANGS order (Settings cycles through).
pub(crate) fn next_code(code: &str) -> &'static str {
    let i = LANGS.iter().position(|(c, _, _)| *c == code).unwrap_or(0);
    LANGS[(i + 1) % LANGS.len()].0
}

pub(crate) fn t(key: &'static str) -> &'static str {
    let m = ACTIVE.read().unwrap();
    let m = m.as_ref();
    if let Some(v) = m.and_then(|m| m.get(key)) {
        return v;
    }
    // english fallback — index into the same RwLock slot is fine (en is
    // only consulted when ACTIVE isn't english)
    static EN: RwLock<Option<HashMap<&'static str, &'static str>>> = RwLock::new(None);
    {
        let en = EN.read().unwrap();
        if let Some(v) = en.as_ref().and_then(|e| e.get(key)) {
            return v;
        }
    }
    let mut en = EN.write().unwrap();
    let e = en.get_or_insert_with(|| parse(LANGS[0].2));
    e.get(key).copied().unwrap_or(key)
}
