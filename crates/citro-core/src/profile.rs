//! Profile, proxy and group records. Field names follow AdsPower's Local API.

use crate::fingerprint::FingerprintConfig;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Accept `"x"`, `12`, `true` for a string field (API clients are sloppy about this).
pub fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(Value::Bool(b)) => Some(if b { "1" } else { "0" }.to_owned()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(other) => Some(other.to_string()),
    })
}

/// AdsPower's `user_proxy_config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_soft: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_type: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_host: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_user: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    #[serde(deserialize_with = "lenient_string", skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One hop of a proxy chain, in the shape the kernel's `ProxyChain` expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyHop {
    pub scheme: String,
    pub host: String,
    pub port: String,
    pub account: String,
    pub password: String,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push(h * 16 + l);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ProxyConfig {
    /// Parse `[scheme://][user[:password]@]host:port` (scheme defaults to `http`; `%XX` in credentials is
    /// decoded). `None` when there is no usable host and port.
    pub fn from_url(s: &str) -> Option<ProxyConfig> {
        let s = s.trim();
        let (scheme, rest) = match s.split_once("://") {
            Some((sc, r)) => (sc.to_ascii_lowercase(), r),
            None => ("http".to_owned(), s),
        };
        let rest = rest.trim_end_matches('/');
        let (auth, hostport) = match rest.rsplit_once('@') {
            Some((a, h)) => (Some(a), h),
            None => (None, rest),
        };
        let (host, port) = hostport.rsplit_once(':')?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() || port.parse::<u16>().is_err() {
            return None;
        }
        let (user, pass) = match auth.map(|a| a.split_once(':').map_or((a, None), |(u, p)| (u, Some(p)))) {
            Some((u, p)) => (Some(u), p),
            None => (None, None),
        };
        Some(ProxyConfig {
            proxy_soft: Some("other".into()),
            proxy_type: Some(scheme),
            proxy_host: Some(host.to_owned()),
            proxy_port: Some(port.to_owned()),
            proxy_user: user.filter(|u| !u.is_empty()).map(percent_decode),
            proxy_password: pass.filter(|p| !p.is_empty()).map(percent_decode),
            ..Default::default()
        })
    }

    /// A concrete proxy, or `None` for a direct connection.
    pub fn hop(&self) -> Option<ProxyHop> {
        if self.proxy_soft.as_deref() == Some("no_proxy") {
            return None;
        }
        let host = self.proxy_host.as_deref().map(str::trim).filter(|h| !h.is_empty())?;
        let port = self.proxy_port.as_deref().map(str::trim).filter(|p| !p.is_empty())?;
        let scheme = match self.proxy_type.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("socks5" | "socks5h") => "socks5",
            Some("socks4") => "socks4",
            Some("https") => "https",
            _ => "http",
        };
        Some(ProxyHop {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            port: port.to_owned(),
            account: self.proxy_user.clone().unwrap_or_default(),
            password: self.proxy_password.clone().unwrap_or_default(),
        })
    }

    pub fn is_direct(&self) -> bool {
        self.hop().is_none()
    }

    /// For API output: never echo the password back.
    pub fn redacted(&self) -> ProxyConfig {
        let mut c = self.clone();
        if c.proxy_password.as_deref().is_some_and(|p| !p.is_empty()) {
            c.proxy_password = Some("********".into());
        }
        c
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub profile_id: String,
    pub serial_number: u64,
    pub name: String,
    pub group_id: String,
    pub remark: String,
    /// The platform's domain, opened on start when there are no explicit tabs.
    pub domain_name: String,
    pub username: String,
    pub password: String,
    pub fakey: String,
    pub cookie: String,
    pub tabs: Vec<String>,
    /// ISO country of the proxy exit; drives language and time zone when they are IP based.
    pub country: String,
    pub ip: String,
    pub user_proxy_config: ProxyConfig,
    pub fingerprint_config: FingerprintConfig,
    pub launch_args: Vec<String>,
    pub category_id: String,
    /// The operating system the browser presents: `windows`, `macos`, `linux`, or empty = the host's own.
    pub persona_os: String,
    /// The browser's default search engine (`google`, `bing`, ... see `crate::browser_prefs`); empty = Google,
    /// `browser` = leave it to the browser.
    pub search_engine: String,
    /// Bookmarks Citroenition keeps on the browser's bookmarks bar (`crate::bookmarks`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
    /// Where the profile was imported from (`adspower:<user_id>`), so an import run twice adds nothing twice.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    pub created_time: i64,
    /// Unix seconds; `0` = never.
    pub last_open_time: i64,
    /// Unix seconds of the last time the browser was closed; `0` = never.
    pub last_close_time: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub group_id: String,
    pub group_name: String,
    pub remark: String,
    /// Start pages every profile of the group opens (before the profile's own). Extensions per group are in
    /// `crate::extensions`.
    pub tabs: Vec<String>,
    /// Bookmarks on the bar of every profile of the group (before the profile's own).
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proxy_port_may_be_a_number() {
        let p: ProxyConfig = serde_json::from_value(json!({
            "proxy_soft": "other", "proxy_type": "socks5", "proxy_host": "1.2.3.4", "proxy_port": 1080,
            "proxy_user": "u", "proxy_password": "p", "future_field": 1}))
        .unwrap();
        let h = p.hop().unwrap();
        assert_eq!(
            (h.scheme.as_str(), h.host.as_str(), h.port.as_str(), h.account.as_str()),
            ("socks5", "1.2.3.4", "1080", "u")
        );
        assert_eq!(p.extra["future_field"], 1);
    }

    #[test]
    fn proxy_urls_parse() {
        let p = ProxyConfig::from_url("socks5://us%40er:p%3Ass@proxy.example.com:1080/").unwrap();
        let h = p.hop().unwrap();
        assert_eq!((h.scheme.as_str(), h.host.as_str(), h.port.as_str()), ("socks5", "proxy.example.com", "1080"));
        assert_eq!((h.account.as_str(), h.password.as_str()), ("us@er", "p:ss"));
        let bare = ProxyConfig::from_url("1.2.3.4:3128").unwrap();
        assert_eq!((bare.proxy_type.as_deref(), bare.proxy_user.as_deref()), (Some("http"), None));
        assert_eq!(ProxyConfig::from_url("http://[::1]:80").unwrap().proxy_host.as_deref(), Some("::1"));
        assert!(ProxyConfig::from_url("http://nohost").is_none());
        assert!(ProxyConfig::from_url("http://h:99999").is_none());
    }

    #[test]
    fn no_proxy_and_missing_host_are_direct() {
        assert!(ProxyConfig::default().is_direct());
        let np: ProxyConfig =
            serde_json::from_value(json!({"proxy_soft": "no_proxy", "proxy_host": "x", "proxy_port": "1"})).unwrap();
        assert!(np.is_direct());
    }

    #[test]
    fn unknown_type_defaults_to_http_and_password_is_redacted() {
        let p: ProxyConfig =
            serde_json::from_value(json!({"proxy_host": "h", "proxy_port": "8", "proxy_password": "secret"})).unwrap();
        assert_eq!(p.hop().unwrap().scheme, "http");
        assert_eq!(p.redacted().proxy_password.as_deref(), Some("********"));
        assert_eq!(p.proxy_password.as_deref(), Some("secret"));
    }
}
