//! The countries the profile form offers. They are exactly the ones `citro-core` knows a time zone and a language
//! for (`citro_core::locale::country`), so picking one always drives a coherent language / time zone.

use citro_core::locale;

/// `(ISO 3166-1 alpha-2, English name)`, ordered by name.
pub const COUNTRIES: &[(&str, &str)] = &[
    ("ar", "Argentina"),
    ("au", "Australia"),
    ("at", "Austria"),
    ("be", "Belgium"),
    ("br", "Brazil"),
    ("ca", "Canada"),
    ("cn", "China"),
    ("cz", "Czechia"),
    ("dk", "Denmark"),
    ("eg", "Egypt"),
    ("fi", "Finland"),
    ("fr", "France"),
    ("de", "Germany"),
    ("gr", "Greece"),
    ("hk", "Hong Kong"),
    ("hu", "Hungary"),
    ("in", "India"),
    ("id", "Indonesia"),
    ("ie", "Ireland"),
    ("il", "Israel"),
    ("it", "Italy"),
    ("jp", "Japan"),
    ("my", "Malaysia"),
    ("mx", "Mexico"),
    ("ma", "Morocco"),
    ("nl", "Netherlands"),
    ("nz", "New Zealand"),
    ("ng", "Nigeria"),
    ("no", "Norway"),
    ("ph", "Philippines"),
    ("pl", "Poland"),
    ("pt", "Portugal"),
    ("ro", "Romania"),
    ("ru", "Russia"),
    ("sa", "Saudi Arabia"),
    ("sg", "Singapore"),
    ("za", "South Africa"),
    ("kr", "South Korea"),
    ("es", "Spain"),
    ("se", "Sweden"),
    ("ch", "Switzerland"),
    ("th", "Thailand"),
    ("tr", "Türkiye"),
    ("ua", "Ukraine"),
    ("ae", "United Arab Emirates"),
    ("gb", "United Kingdom"),
    ("us", "United States"),
    ("vn", "Vietnam"),
];

/// `"de"` / `"DE"` -> `"Germany"`.
pub fn name(code: &str) -> Option<&'static str> {
    let code = code.trim().to_ascii_lowercase();
    let code = if code == "uk" { "gb" } else { code.as_str() };
    COUNTRIES.iter().find(|c| c.0 == code).map(|c| c.1)
}

/// `Germany (DE)`, or the bare code when it is not in the list, or an empty string for "not set".
pub fn label(code: &str) -> String {
    match (name(code), code.trim()) {
        (_, "") => String::new(),
        (Some(n), c) => format!("{n} ({})", c.to_ascii_uppercase()),
        (None, c) => c.to_ascii_uppercase(),
    }
}

/// `(time zone, language)` a country implies, for the hint under the country box.
pub fn implied(code: &str) -> Option<(&'static str, &'static str)> {
    locale::country(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_country_has_a_timezone_and_language() {
        for (code, name) in COUNTRIES {
            assert!(locale::country(code).is_some(), "{code} ({name}) is unknown to citro-core");
        }
    }

    #[test]
    fn the_list_is_sorted_and_unique() {
        let names: Vec<&str> = COUNTRIES.iter().map(|c| c.1).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let mut codes: Vec<&str> = COUNTRIES.iter().map(|c| c.0).collect();
        codes.dedup();
        assert_eq!(codes.len(), COUNTRIES.len());
    }

    #[test]
    fn labels() {
        assert_eq!(label("de"), "Germany (DE)");
        assert_eq!(label("UK"), "United Kingdom (UK)");
        assert_eq!(label("zz"), "ZZ");
        assert_eq!(label(""), "");
        assert_eq!(implied("fr"), Some(("Europe/Paris", "fr-FR")));
    }
}
