//! Chrome's own language defaults, and the country -> locale / time zone table.
//!
//! Genuine Chrome derives everything from ONE value, the OS locale. It resolves that to a UI locale it
//! ships (`de-AT` -> `de`, `en-CA` -> `en-GB`, `es-MX` -> `es-419`); `navigator.languages` and the
//! `Accept-Language` header are then that UI locale's built-in list, and `Intl` reports the UI locale
//! itself (`de`, not `de-DE`). A persona that follows the same rule looks like a fresh Chrome profile on
//! that OS. The lists below are the values Chromium's `components_locale_settings_<ui>.xtb` ships and the
//! Clearcote SDK measured on Chrome 154.

/// Locales Desktop Chrome ships a UI for.
const UI_LOCALES: &[&str] = &[
    "af", "am", "ar", "bg", "bn", "ca", "cs", "da", "de", "el", "en-GB", "en-US", "es", "es-419", "et", "fa", "fi",
    "fil", "fr", "gu", "he", "hi", "hr", "hu", "id", "it", "ja", "kn", "ko", "lt", "lv", "ml", "mr", "ms", "nb", "nl",
    "pl", "pt-BR", "pt-PT", "ro", "ru", "sk", "sl", "sr", "sv", "sw", "ta", "te", "th", "tr", "uk", "ur", "vi",
    "zh-CN", "zh-TW",
];

/// `IDS_ACCEPT_LANGUAGES` per UI locale. A UI locale missing here (af, ms, ur, en-US) uses `en-US,en`.
const ACCEPT_LANGUAGES: &[(&str, &str)] = &[
    ("am", "am,en-GB,en"),
    ("ar", "ar,en-US,en"),
    ("bg", "bg-BG,bg"),
    ("bn", "bn-IN,bn,en-US,en"),
    ("ca", "ca-ES,ca"),
    ("cs", "cs-CZ,cs"),
    ("da", "da-DK,da,en-US,en"),
    ("de", "de-DE,de,en-US,en"),
    ("el", "el-GR,el"),
    ("en-GB", "en-GB,en-US,en"),
    ("es", "es-ES,es"),
    ("es-419", "es-419,es"),
    ("et", "et-EE,et,en-US,en"),
    ("fa", "fa,en-US,en"),
    ("fi", "fi-FI,fi,en-US,en"),
    ("fil", "fil,fil-PH,tl,en-US,en"),
    ("fr", "fr-FR,fr,en-US,en"),
    ("gu", "gu-IN,gu,hi-IN,hi,en-US,en"),
    ("he", "he-IL,he,en-US,en"),
    ("hi", "hi-IN,hi,en-US,en"),
    ("hr", "hr-HR,hr,en-US,en"),
    ("hu", "hu-HU,hu,en-US,en"),
    ("id", "id-ID,id,en-US,en"),
    ("it", "it-IT,it,en-US,en"),
    ("ja", "ja,en-US,en"),
    ("kn", "kn-IN,kn,en-US,en"),
    ("ko", "ko-KR,ko,en-US,en"),
    ("lt", "lt,en-US,en,ru,pl"),
    ("lv", "lv-LV,lv,en-US,en"),
    ("ml", "ml-IN,ml,en-US,en"),
    ("mr", "mr-IN,mr,hi-IN,hi,en-US,en"),
    ("nb", "nb-NO,nb,no,nn,en-US,en"),
    ("nl", "nl-NL,nl,en-US,en"),
    ("pl", "pl-PL,pl,en-US,en"),
    ("pt-BR", "pt-BR,pt,en-US,en"),
    ("pt-PT", "pt-PT,pt,en-US,en"),
    ("ro", "ro-RO,ro,en-US,en"),
    ("ru", "ru-RU,ru,en-US,en"),
    ("sk", "sk-SK,sk,cs,en-US,en"),
    ("sl", "sl-SI,sl,en-GB,en"),
    ("sr", "sr-RS,sr,en-US,en"),
    ("sv", "sv-SE,sv,en-US,en"),
    ("sw", "sw,en-GB,en"),
    ("ta", "ta-IN,ta,en-US,en"),
    ("te", "te-IN,te,hi-IN,hi,en-US,en"),
    ("th", "th-TH,th"),
    ("tr", "tr-TR,tr,en-US,en"),
    ("uk", "uk-UA,uk,en-US,en"),
    ("vi", "vi-VN,vi,fr-FR,fr,en-US,en"),
    ("zh-CN", "zh-CN,zh"),
    ("zh-TW", "zh-TW,zh,en-US,en"),
];

/// English regions Chrome resolves to `en-US`; every other region gets `en-GB`.
const EN_US_REGIONS: &[&str] = &["", "US", "PH", "AS", "GU", "MH", "MP", "PR", "UM", "VI"];
/// Spanish regions that stay `es`; the rest of the Spanish-speaking world gets `es-419`.
const ES_ES_REGIONS: &[&str] = &["", "ES", "EA", "IC", "GQ"];

/// `(language, script, region)` of a BCP-47-ish tag, normalised (`zh_hant_tw` -> `zh`, `Hant`, `TW`).
fn split_tag(tag: &str) -> (String, String, String) {
    let mut parts = tag.trim().split(['-', '_']).filter(|p| !p.is_empty());
    let Some(first) = parts.next() else { return Default::default() };
    let lang = match first.to_ascii_lowercase().as_str() {
        "iw" => "he".to_owned(),
        "in" => "id".to_owned(),
        "tl" => "fil".to_owned(),
        "no" | "nn" => "nb".to_owned(),
        other => other.to_owned(),
    };
    let (mut script, mut region) = (String::new(), String::new());
    for part in parts {
        let alpha = part.chars().all(|c| c.is_ascii_alphabetic());
        if part.len() == 4 && alpha {
            let (h, t) = part.split_at(1);
            script = format!("{}{}", h.to_ascii_uppercase(), t.to_ascii_lowercase());
        } else if (part.len() == 2 && alpha) || (part.len() == 3 && part.chars().all(|c| c.is_ascii_digit())) {
            region = part.to_ascii_uppercase();
        }
    }
    (lang, script, region)
}

/// The UI locale Chrome picks for an OS locale `tag` (`de-AT` -> `de`), or `None` when Chrome ships no UI
/// in that language.
pub fn chrome_ui_locale(tag: &str) -> Option<String> {
    let (lang, script, region) = split_tag(tag);
    if lang.is_empty() {
        return None;
    }
    let region = region.as_str();
    Some(match lang.as_str() {
        "en" if EN_US_REGIONS.contains(&region) => "en-US".to_owned(),
        "en" => "en-GB".to_owned(),
        "es" if ES_ES_REGIONS.contains(&region) => "es".to_owned(),
        "es" => "es-419".to_owned(),
        "pt" if region.is_empty() || region == "BR" => "pt-BR".to_owned(),
        "pt" => "pt-PT".to_owned(),
        "zh" => {
            let traditional = script == "Hant" || (script != "Hans" && ["TW", "HK", "MO"].contains(&region));
            if traditional { "zh-TW" } else { "zh-CN" }.to_owned()
        }
        l if UI_LOCALES.contains(&l) => l.to_owned(),
        _ => return None,
    })
}

/// Chrome's default `navigator.languages` for a UI locale, as a comma list (`de` -> `de-DE,de,en-US,en`).
pub fn chrome_accept_languages(ui_locale: &str) -> &'static str {
    ACCEPT_LANGUAGES.iter().find(|(k, _)| *k == ui_locale).map_or("en-US,en", |(_, v)| v)
}

/// Map a cleaned language value to `(--accept-lang, --lang)` for the engine.
///
/// * ONE tag (`de-AT`) is read as the OS locale: the result is exactly what a fresh Chrome profile on that
///   OS shows -- `("de-DE,de,en-US,en", Some("de"))`.
/// * A LIST (`de-AT,de,en`) is the caller's own list and is kept verbatim; only the UI locale is resolved
///   from its first tag.
/// * A tag Chrome has no UI for (`is-IS`) is passed through unchanged.
pub fn resolve_languages(clean: &str) -> (String, Option<String>) {
    let tags: Vec<&str> = clean.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    let Some(first) = tags.first() else { return (clean.to_owned(), None) };
    let ui = chrome_ui_locale(first);
    match (&ui, tags.len()) {
        (Some(ui), 1) => (chrome_accept_languages(ui).to_owned(), Some(ui.clone())),
        _ => (tags.join(","), ui.or_else(|| Some((*first).to_owned()))),
    }
}

/// `(country code, IANA time zone, OS locale)`.
const COUNTRIES: &[(&str, &str, &str)] = &[
    ("fr", "Europe/Paris", "fr-FR"),
    ("de", "Europe/Berlin", "de-DE"),
    ("gb", "Europe/London", "en-GB"),
    ("uk", "Europe/London", "en-GB"),
    ("us", "America/New_York", "en-US"),
    ("ca", "America/Toronto", "en-CA"),
    ("es", "Europe/Madrid", "es-ES"),
    ("it", "Europe/Rome", "it-IT"),
    ("nl", "Europe/Amsterdam", "nl-NL"),
    ("be", "Europe/Brussels", "fr-BE"),
    ("ch", "Europe/Zurich", "de-CH"),
    ("at", "Europe/Vienna", "de-AT"),
    ("pt", "Europe/Lisbon", "pt-PT"),
    ("ie", "Europe/Dublin", "en-IE"),
    ("pl", "Europe/Warsaw", "pl-PL"),
    ("se", "Europe/Stockholm", "sv-SE"),
    ("no", "Europe/Oslo", "nb-NO"),
    ("dk", "Europe/Copenhagen", "da-DK"),
    ("fi", "Europe/Helsinki", "fi-FI"),
    ("ro", "Europe/Bucharest", "ro-RO"),
    ("gr", "Europe/Athens", "el-GR"),
    ("cz", "Europe/Prague", "cs-CZ"),
    ("hu", "Europe/Budapest", "hu-HU"),
    ("tr", "Europe/Istanbul", "tr-TR"),
    ("ru", "Europe/Moscow", "ru-RU"),
    ("ua", "Europe/Kyiv", "uk-UA"),
    ("br", "America/Sao_Paulo", "pt-BR"),
    ("mx", "America/Mexico_City", "es-MX"),
    ("ar", "America/Argentina/Buenos_Aires", "es-AR"),
    ("au", "Australia/Sydney", "en-AU"),
    ("nz", "Pacific/Auckland", "en-NZ"),
    ("jp", "Asia/Tokyo", "ja-JP"),
    ("kr", "Asia/Seoul", "ko-KR"),
    ("cn", "Asia/Shanghai", "zh-CN"),
    ("hk", "Asia/Hong_Kong", "zh-HK"),
    ("sg", "Asia/Singapore", "en-SG"),
    ("in", "Asia/Kolkata", "en-IN"),
    ("id", "Asia/Jakarta", "id-ID"),
    ("th", "Asia/Bangkok", "th-TH"),
    ("vn", "Asia/Ho_Chi_Minh", "vi-VN"),
    ("ph", "Asia/Manila", "en-PH"),
    ("my", "Asia/Kuala_Lumpur", "ms-MY"),
    ("ae", "Asia/Dubai", "ar-AE"),
    ("sa", "Asia/Riyadh", "ar-SA"),
    ("il", "Asia/Jerusalem", "he-IL"),
    ("za", "Africa/Johannesburg", "en-ZA"),
    ("eg", "Africa/Cairo", "ar-EG"),
    ("ma", "Africa/Casablanca", "fr-MA"),
    ("ng", "Africa/Lagos", "en-NG"),
];

/// `(IANA time zone, OS locale)` of an ISO country code (any case).
pub fn country(code: &str) -> Option<(&'static str, &'static str)> {
    let code = code.trim().to_ascii_lowercase();
    COUNTRIES.iter().find(|c| c.0 == code).map(|c| (c.1, c.2))
}

/// The country a locale tag names (`fr-FR` -> `FR`'s row), when the tag carries a region we know.
pub fn country_of_tag(tag: &str) -> Option<(&'static str, &'static str)> {
    let (_, _, region) = split_tag(tag);
    (!region.is_empty()).then(|| country(&region)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tag_is_an_os_locale_and_becomes_chromes_default_list() {
        assert_eq!(resolve_languages("fr-FR"), ("fr-FR,fr,en-US,en".to_owned(), Some("fr".to_owned())));
        assert_eq!(resolve_languages("de-AT"), ("de-DE,de,en-US,en".to_owned(), Some("de".to_owned())));
        assert_eq!(resolve_languages("en-US"), ("en-US,en".to_owned(), Some("en-US".to_owned())));
        assert_eq!(resolve_languages("en-CA").1.as_deref(), Some("en-GB"));
        assert_eq!(resolve_languages("es-MX").1.as_deref(), Some("es-419"));
        assert_eq!(resolve_languages("pt_br").1.as_deref(), Some("pt-BR"));
        assert_eq!(resolve_languages("zh-Hant-TW").1.as_deref(), Some("zh-TW"));
        assert_eq!(resolve_languages("iw-IL").1.as_deref(), Some("he"));
    }

    #[test]
    fn a_list_is_kept_and_an_unknown_tag_passes_through() {
        assert_eq!(resolve_languages("fr-FR, fr"), ("fr-FR,fr".to_owned(), Some("fr".to_owned())));
        assert_eq!(resolve_languages("is-IS"), ("is-IS".to_owned(), Some("is-IS".to_owned())));
        assert_eq!(resolve_languages(""), (String::new(), None));
    }

    #[test]
    fn every_ui_locale_has_a_known_list_or_the_default() {
        for l in UI_LOCALES {
            assert!(chrome_accept_languages(l).contains(','), "{l}");
        }
        assert_eq!(chrome_accept_languages("af"), "en-US,en");
    }

    #[test]
    fn countries_resolve_in_any_case() {
        assert_eq!(country("FR"), Some(("Europe/Paris", "fr-FR")));
        assert_eq!(country(" de "), Some(("Europe/Berlin", "de-DE")));
        assert_eq!(country("zz"), None);
        assert_eq!(country_of_tag("fr-FR"), Some(("Europe/Paris", "fr-FR")));
        assert_eq!(country_of_tag("fr"), None);
    }
}
