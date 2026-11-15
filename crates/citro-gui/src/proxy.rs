//! Parsing of pasted proxy strings, as providers hand them out:
//!
//! * `host:port`
//! * `host:port:user:pass`          (the password may itself contain `:`)
//! * `user:pass@host:port`
//! * `host:port@user:pass`          (the "reversed" form some providers use)
//! * `scheme://user:pass@host:port` (`http`, `https`, `socks4`, `socks5`, `socks5h`; `%XX` is decoded)
//! * any of the above with `[ipv6]` hosts.

use citro_core::profile::ProxyConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ProxyKind {
    /// No proxy: the browser connects directly.
    #[default]
    Direct,
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyKind {
    pub const ALL: [ProxyKind; 5] =
        [ProxyKind::Direct, ProxyKind::Http, ProxyKind::Https, ProxyKind::Socks4, ProxyKind::Socks5];

    pub fn label(self) -> &'static str {
        match self {
            ProxyKind::Direct => "No proxy (direct)",
            ProxyKind::Http => "HTTP",
            ProxyKind::Https => "HTTPS",
            ProxyKind::Socks4 => "SOCKS4",
            ProxyKind::Socks5 => "SOCKS5",
        }
    }

    /// The `proxy_type` string stored in the profile.
    pub fn scheme(self) -> &'static str {
        match self {
            ProxyKind::Direct => "",
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
            ProxyKind::Socks4 => "socks4",
            ProxyKind::Socks5 => "socks5",
        }
    }

    pub fn from_scheme(s: &str) -> Option<ProxyKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Some(ProxyKind::Http),
            "https" => Some(ProxyKind::Https),
            "socks4" | "socks4a" => Some(ProxyKind::Socks4),
            "socks5" | "socks5h" | "socks" => Some(ProxyKind::Socks5),
            _ => None,
        }
    }

    /// What a stored profile means: `no_proxy` / missing host = direct.
    pub fn from_config(c: &ProxyConfig) -> ProxyKind {
        match c.hop() {
            None => ProxyKind::Direct,
            Some(h) => ProxyKind::from_scheme(&h.scheme).unwrap_or(ProxyKind::Http),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedProxy {
    /// `None` when the string carried no scheme (the form keeps its current type then).
    pub kind: Option<ProxyKind>,
    pub host: String,
    pub port: u16,
    pub user: String,
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

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
        && !h.starts_with('.')
}

fn parse_port(p: &str) -> Option<u16> {
    p.trim().parse::<u16>().ok().filter(|&p| p != 0)
}

/// `host:port` -> (host, port); the host may be `[v6]`.
fn host_port(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    let (host, port) = s.rsplit_once(':')?;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let port = parse_port(port)?;
    valid_host(host).then(|| (host.to_owned(), port))
}

/// `host:port:user:pass...` with an optional bracketed IPv6 host.
fn colon_form(s: &str) -> Option<ParsedProxy> {
    let (host, rest) = if let Some(stripped) = s.strip_prefix('[') {
        let (h, r) = stripped.split_once(']')?;
        (h.to_owned(), r.strip_prefix(':')?)
    } else {
        let (h, r) = s.split_once(':')?;
        (h.to_owned(), r)
    };
    let mut it = rest.splitn(3, ':');
    let port = parse_port(it.next()?)?;
    let user = it.next().unwrap_or("").to_owned();
    let pass = it.next().unwrap_or("").to_owned();
    valid_host(&host).then_some(ParsedProxy { kind: None, host, port, user, password: pass })
}

const FORMAT_HELP: &str = "Unrecognised format. Use host:port, host:port:user:pass, user:pass@host:port or socks5://user:pass@host:port";

pub fn parse(input: &str) -> Result<ParsedProxy, String> {
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if s.is_empty() {
        return Err("Paste a proxy first".into());
    }
    if s.contains(char::is_whitespace) {
        return Err("A proxy string has no spaces".into());
    }

    let (kind, rest) = match s.split_once("://") {
        Some((scheme, rest)) => {
            let kind = ProxyKind::from_scheme(scheme)
                .ok_or_else(|| format!("Unsupported scheme '{scheme}' (use http, https, socks4 or socks5)"))?;
            (Some(kind), rest.trim_end_matches('/'))
        }
        None => (None, s.trim_end_matches('/')),
    };

    if let Some((left, right)) = rest.rsplit_once('@') {
        let creds = |c: &str| {
            let (user, pass) = c.split_once(':').unwrap_or((c, ""));
            (percent_decode(user), percent_decode(pass))
        };
        // standard form: user:pass@host:port
        if let Some((host, port)) = host_port(right) {
            let (user, password) = creds(left);
            return Ok(ParsedProxy { kind, host, port, user, password });
        }
        // reversed form: host:port@user:pass
        if let Some((host, port)) = host_port(left) {
            let (user, password) = creds(right);
            return Ok(ParsedProxy { kind, host, port, user, password });
        }
        return Err(FORMAT_HELP.into());
    }

    // host:port[:user[:pass]]
    colon_form(rest).map(|p| ParsedProxy { kind, ..p }).ok_or_else(|| FORMAT_HELP.to_owned())
}

/// Apply a parsed proxy to a [`ProxyConfig`] (`ProxyConfig::from_url` produces the same shape for URLs).
pub fn to_config(p: &ParsedProxy, fallback: ProxyKind) -> ProxyConfig {
    let kind = p.kind.unwrap_or(if fallback == ProxyKind::Direct { ProxyKind::Http } else { fallback });
    ProxyConfig {
        proxy_soft: Some("other".into()),
        proxy_type: Some(kind.scheme().into()),
        proxy_host: Some(p.host.clone()),
        proxy_port: Some(p.port.to_string()),
        proxy_user: Some(p.user.clone()).filter(|u| !u.is_empty()),
        proxy_password: Some(p.password.clone()).filter(|u| !u.is_empty()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedProxy {
        parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn t(kind: Option<ProxyKind>, host: &str, port: u16, user: &str, pass: &str) -> ParsedProxy {
        ParsedProxy { kind, host: host.into(), port, user: user.into(), password: pass.into() }
    }

    #[test]
    fn bare_host_port() {
        assert_eq!(p("203.0.113.9:1080"), t(None, "203.0.113.9", 1080, "", ""));
        assert_eq!(p("  proxy.example.com:8080  "), t(None, "proxy.example.com", 8080, "", ""));
    }

    #[test]
    fn host_port_user_pass() {
        assert_eq!(p("203.0.113.9:1080:bob:s3cret"), t(None, "203.0.113.9", 1080, "bob", "s3cret"));
        // the password may contain colons
        assert_eq!(p("h.example:3128:bob:pa:ss:word"), t(None, "h.example", 3128, "bob", "pa:ss:word"));
        // user only
        assert_eq!(p("h.example:3128:bob"), t(None, "h.example", 3128, "bob", ""));
    }

    #[test]
    fn user_pass_at_host_port() {
        assert_eq!(p("bob:s3cret@203.0.113.9:1080"), t(None, "203.0.113.9", 1080, "bob", "s3cret"));
        assert_eq!(p("bob@203.0.113.9:1080"), t(None, "203.0.113.9", 1080, "bob", ""));
    }

    #[test]
    fn reversed_host_port_at_user_pass() {
        assert_eq!(p("203.0.113.9:1080@bob:s3cret"), t(None, "203.0.113.9", 1080, "bob", "s3cret"));
    }

    #[test]
    fn full_urls() {
        assert_eq!(
            p("socks5://bob:s3cret@203.0.113.9:1080"),
            t(Some(ProxyKind::Socks5), "203.0.113.9", 1080, "bob", "s3cret")
        );
        assert_eq!(p("http://203.0.113.9:8080/"), t(Some(ProxyKind::Http), "203.0.113.9", 8080, "", ""));
        assert_eq!(p("HTTPS://a.b:443"), t(Some(ProxyKind::Https), "a.b", 443, "", ""));
        assert_eq!(p("socks5h://u:p@h.example:9050"), t(Some(ProxyKind::Socks5), "h.example", 9050, "u", "p"));
        assert_eq!(p("socks4://h.example:1080"), t(Some(ProxyKind::Socks4), "h.example", 1080, "", ""));
        // percent-escapes in credentials
        assert_eq!(p("http://us%40er:p%3Ass@h.example:80"), t(Some(ProxyKind::Http), "h.example", 80, "us@er", "p:ss"));
        // the scheme can prefix the colon form too
        assert_eq!(p("socks5://h.example:1080:bob:pw"), t(Some(ProxyKind::Socks5), "h.example", 1080, "bob", "pw"));
    }

    #[test]
    fn ipv6_hosts() {
        assert_eq!(p("[2001:db8::1]:1080"), t(None, "2001:db8::1", 1080, "", ""));
        assert_eq!(p("[2001:db8::1]:1080:bob:pw"), t(None, "2001:db8::1", 1080, "bob", "pw"));
        assert_eq!(p("socks5://bob:pw@[2001:db8::1]:1080"), t(Some(ProxyKind::Socks5), "2001:db8::1", 1080, "bob", "pw"));
    }

    #[test]
    fn quotes_and_trailing_slash_are_ignored() {
        assert_eq!(p("\"203.0.113.9:1080\""), t(None, "203.0.113.9", 1080, "", ""));
        assert_eq!(p("203.0.113.9:1080/"), t(None, "203.0.113.9", 1080, "", ""));
    }

    #[test]
    fn rejects_garbage_with_a_reason() {
        for bad in ["", "   ", "hostonly", "h.example:0", "h.example:70000", "h.example:abc", ":1080", "a b:1", "2001:db8::1:1080"] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
        let e = parse("ftp://h.example:21").unwrap_err();
        assert!(e.contains("Unsupported scheme"), "{e}");
        assert!(parse("a b:1").unwrap_err().contains("no spaces"));
    }

    #[test]
    fn agrees_with_core_from_url_on_urls() {
        let url = "socks5://bob:s3cret@203.0.113.9:1080";
        let core = ProxyConfig::from_url(url).unwrap();
        let mine = to_config(&parse(url).unwrap(), ProxyKind::Direct);
        assert_eq!(mine.proxy_type, core.proxy_type);
        assert_eq!(mine.proxy_host, core.proxy_host);
        assert_eq!(mine.proxy_port, core.proxy_port);
        assert_eq!(mine.proxy_user, core.proxy_user);
        assert_eq!(mine.proxy_password, core.proxy_password);
    }

    #[test]
    fn kinds_round_trip_through_config() {
        for k in [ProxyKind::Http, ProxyKind::Https, ProxyKind::Socks4, ProxyKind::Socks5] {
            let c = to_config(&t(Some(k), "h.example", 1, "", ""), ProxyKind::Direct);
            assert_eq!(ProxyKind::from_config(&c), k);
        }
        assert_eq!(ProxyKind::from_config(&ProxyConfig::default()), ProxyKind::Direct);
        // a bare paste keeps the type the form already has, falling back to http
        assert_eq!(to_config(&t(None, "h", 1, "", ""), ProxyKind::Socks5).proxy_type.as_deref(), Some("socks5"));
        assert_eq!(to_config(&t(None, "h", 1, "", ""), ProxyKind::Direct).proxy_type.as_deref(), Some("http"));
    }
}
