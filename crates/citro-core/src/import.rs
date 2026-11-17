//! Reading profiles from other tools, forgivingly.
//!
//! [`parse_file`] takes whatever a person is likely to have: a CSV (comma, semicolon or tab separated, with or
//! without a byte-order mark), an Excel `.xlsx` (AdsPower's export and import template), a JSON list (AdsPower's API
//! shape), or plain text with one proxy per line. Columns are recognised by many names ("Proxy Host", `proxy_host`,
//! "IP", "server" ...), proxies in every common notation, OS and country in loose spellings.
//!
//! Nothing that is *almost* right is rejected: a value that cannot be understood becomes a warning on that row and
//! is left out, the rest of the row is imported, and a row without a name gets one. Only a file with no usable row at
//! all is an error. [`SAMPLE_CSV`] shows every column.

use crate::profile::{Profile, ProxyConfig};
use serde_json::Value;

/// One profile to import, with what was not understood about its row.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub profile: Profile,
    /// Group by name ("" = ungrouped); created when missing.
    pub group: String,
    pub warnings: Vec<String>,
    /// The file could not hold its cookies (AdsPower writes "Cookie too large ..." instead): they are fetched from
    /// AdsPower's API by the profile's AdsPower id ([`crate::adspower::fill_cookies`]).
    pub cookies_elsewhere: bool,
}

/// What a file held.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub profiles: Vec<Incoming>,
    /// About the file as a whole (ignored columns, the format guessed).
    pub notes: Vec<String>,
    /// Groups to create even when no profile is in them (AdsPower's).
    pub groups: Vec<String>,
}

/// A CSV with every column the importer understands.
pub const SAMPLE_CSV: &str = "name,group,remark,proxy,platform,username,password,start_urls,os,country,user_agent,cookie
Shop FR - Camille,Vinted FR,main seller,socks5://user:pass@203.0.113.21:1080,vinted.fr,camille@example.com,secret1,https://www.vinted.fr/,macos,fr,,
Shop DE - Jonas,eBay DE,,203.0.113.44:8080:jonas:pw,ebay.de,jonas@example.com,,https://www.ebay.de/ https://mail.example.com/,windows,de,,
Research 1,Research,no proxy,,google.com,,,,linux,us,,\"[{\"\"name\"\":\"\"sid\"\",\"\"value\"\":\"\"abc\"\",\"\"domain\"\":\"\".google.com\"\"}]\"
";

// -------------------------------------------------------------------------------------------------------------
// columns

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Group,
    Remark,
    Proxy,
    ProxyType,
    ProxyHost,
    ProxyPort,
    ProxyUser,
    ProxyPass,
    Platform,
    Username,
    Password,
    Tabs,
    Os,
    Country,
    UserAgent,
    Cookie,
    TwoFa,
    Ip,
    /// AdsPower's profile id (`id` in its export): where cookies are fetched from, and what an import matches on.
    AdsId,
    /// AdsPower's serial (`acc_id`).
    AdsSerial,
    Tags,
    /// AdsPower's saved-proxy id: not in the file, so the proxy cannot come along.
    SavedProxy,
    Skip,
}

fn norm(h: &str) -> String {
    h.trim().trim_start_matches('\u{feff}').to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// The column a header names, by many spellings.
fn column(header: &str) -> Option<Col> {
    let h = norm(header);
    let is = |names: &[&str]| names.contains(&h.as_str());
    Some(if is(&["name", "profilename", "profile", "accountname", "title", "browsername", "envname"]) {
        Col::Name
    } else if is(&["group", "groupname", "folder", "category", "groupid"]) {
        Col::Group
    } else if is(&["remark", "remarks", "note", "notes", "description", "comment", "comments", "memo"]) {
        Col::Remark
    } else if is(&["proxy", "proxystring", "proxies", "proxyaddress", "userproxyconfig"]) {
        Col::Proxy
    } else if is(&["proxytype", "proxyprotocol", "protocol", "scheme", "type"]) {
        Col::ProxyType
    } else if is(&["proxyhost", "host", "proxyip", "ip", "server", "proxyserver", "hostname", "address"]) {
        Col::ProxyHost
    } else if is(&["proxyport", "port"]) {
        Col::ProxyPort
    } else if is(&["proxyuser", "proxyusername", "proxylogin", "proxyaccount"]) {
        Col::ProxyUser
    } else if is(&["proxypassword", "proxypass", "proxypwd"]) {
        Col::ProxyPass
    } else if is(&["platform", "domain", "domainname", "site", "website", "platformurl", "url"]) {
        Col::Platform
    } else if is(&["username", "user", "account", "email", "login", "accountusername", "mail"]) {
        Col::Username
    } else if is(&["password", "pass", "pwd", "accountpassword"]) {
        Col::Password
    } else if is(&["tabs", "tab", "starturls", "starturl", "urls", "openurls", "startpages", "startpage"]) {
        Col::Tabs
    } else if is(&["os", "system", "operatingsystem", "personaos", "platformos"]) {
        Col::Os
    } else if is(&["country", "ipcountry", "region", "countrycode", "location"]) {
        Col::Country
    } else if is(&["ua", "useragent"]) {
        Col::UserAgent
    } else if is(&["cookie", "cookies"]) {
        Col::Cookie
    } else if is(&["2fa", "fakey", "totp", "2fakey", "twofactor", "otp"]) {
        Col::TwoFa
    } else if is(&["exitip", "lastip"]) {
        Col::Ip
    } else if is(&["tags", "tag", "labels", "label"]) {
        Col::Tags
    } else if is(&["proxyid", "savedproxyid"]) {
        Col::SavedProxy
    } else if is(&[
        // AdsPower's `proxyurl` is the proxy's "change IP" link, not the proxy
        "proxyurl",
        "ipchecker",
        "id",
        "serial",
        "serialnumber",
        "no",
        "number",
        "acc",
        "accid",
        "userid",
        "profileid",
        "createdtime",
        "lastopentime",
    ]) {
        Col::Skip
    } else {
        return None;
    })
}

// -------------------------------------------------------------------------------------------------------------
// values

/// A proxy in any common notation: `scheme://user:pass@host:port`, `user:pass@host:port`, `host:port@user:pass`,
/// `host:port:user:pass`, `host:port`, `socks5 host port user pass`, with `scheme` defaulting to `kind` or http.
pub fn proxy(text: &str, kind: &str) -> Result<ProxyConfig, String> {
    let t = text.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if t.is_empty() || matches!(t.to_lowercase().as_str(), "none" | "direct" | "noproxy" | "no proxy" | "-") {
        return Err(String::new());
    }
    let kind = proxy_kind(kind);
    let (scheme, rest) = match t.split_once("://") {
        Some((s, r)) => (proxy_kind(s).or(kind), r.to_owned()),
        None => {
            // "socks5 1.2.3.4 1080 user pass" / "1.2.3.4 1080"
            let words: Vec<&str> = t.split_whitespace().collect();
            match words.as_slice() {
                [s, rest @ ..] if proxy_kind(s).is_some() && !rest.is_empty() => (proxy_kind(s), rest.join(":")),
                many if many.len() > 1 => (kind, many.join(":")),
                _ => (kind, t.to_owned()),
            }
        }
    };
    let rest = rest.trim_end_matches('/');
    let scheme = scheme.unwrap_or("http");
    let (host, port, user, pass) = if let Some((left, right)) = rest.rsplit_once('@') {
        match (split_hostport(right), split_hostport(left)) {
            (Some((h, p)), _) => {
                let (u, pw) = left.split_once(':').unwrap_or((left, ""));
                (h, p, u.to_owned(), pw.to_owned())
            }
            (None, Some((h, p))) => {
                let (u, pw) = right.split_once(':').unwrap_or((right, ""));
                (h, p, u.to_owned(), pw.to_owned())
            }
            _ => return Err(format!("cannot read the proxy `{t}`")),
        }
    } else {
        let parts: Vec<&str> = rest.splitn(4, ':').collect();
        match parts.as_slice() {
            [h, p] => (h.to_string(), p.to_string(), String::new(), String::new()),
            [h, p, u] => (h.to_string(), p.to_string(), u.to_string(), String::new()),
            [h, p, u, pw] => (h.to_string(), p.to_string(), u.to_string(), pw.to_string()),
            _ => return Err(format!("cannot read the proxy `{t}` (no port)")),
        }
    };
    let host = host.trim().trim_start_matches('[').trim_end_matches(']').to_owned();
    let port = port.trim();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(format!("the proxy `{t}` has no usable host"));
    }
    match port.parse::<u16>() {
        Ok(p) if p > 0 => {}
        _ => return Err(format!("the proxy `{t}` has no usable port")),
    }
    Ok(ProxyConfig {
        proxy_soft: Some("other".into()),
        proxy_type: Some(scheme.to_owned()),
        proxy_host: Some(host),
        proxy_port: Some(port.to_owned()),
        proxy_user: Some(user.trim().to_owned()).filter(|u| !u.is_empty()),
        proxy_password: Some(pass).filter(|p| !p.is_empty()),
        ..Default::default()
    })
}

fn split_hostport(s: &str) -> Option<(String, String)> {
    let (h, p) = s.trim().rsplit_once(':')?;
    p.trim().parse::<u16>().ok().filter(|p| *p > 0)?;
    (!h.trim().is_empty()).then(|| (h.trim().to_owned(), p.trim().to_owned()))
}

fn proxy_kind(s: &str) -> Option<&'static str> {
    match norm(s).as_str() {
        "socks5" | "socks5h" | "socks" => Some("socks5"),
        "socks4" | "socks4a" => Some("socks4"),
        "http" => Some("http"),
        "https" | "ssl" => Some("https"),
        _ => None,
    }
}

/// `Windows 10`, `win`, `Mac OS X`, `osx`, `ubuntu` ... -> `windows` / `macos` / `linux`; anything else is "".
pub fn os(s: &str) -> Option<&'static str> {
    let n = norm(s);
    if n.is_empty() {
        return Some("");
    }
    if n.starts_with("win") {
        Some("windows")
    } else if n.starts_with("mac") || n.starts_with("osx") || n == "darwin" || n == "apple" {
        Some("macos")
    } else if n.contains("linux") || n == "ubuntu" || n == "debian" || n == "fedora" {
        Some("linux")
    } else {
        None
    }
}

/// A two-letter country code, from a code or a common English name.
pub fn country(s: &str) -> Option<String> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return Some(String::new());
    }
    if t.len() == 2 && t.bytes().all(|b| b.is_ascii_lowercase()) {
        return Some(if t == "uk" { "gb".into() } else { t });
    }
    let names: &[(&str, &str)] = &[
        ("united states", "us"),
        ("usa", "us"),
        ("america", "us"),
        ("united kingdom", "gb"),
        ("england", "gb"),
        ("great britain", "gb"),
        ("germany", "de"),
        ("deutschland", "de"),
        ("france", "fr"),
        ("netherlands", "nl"),
        ("holland", "nl"),
        ("spain", "es"),
        ("italy", "it"),
        ("poland", "pl"),
        ("czechia", "cz"),
        ("czech republic", "cz"),
        ("belgium", "be"),
        ("austria", "at"),
        ("switzerland", "ch"),
        ("sweden", "se"),
        ("norway", "no"),
        ("denmark", "dk"),
        ("finland", "fi"),
        ("portugal", "pt"),
        ("ireland", "ie"),
        ("canada", "ca"),
        ("australia", "au"),
        ("brazil", "br"),
        ("mexico", "mx"),
        ("japan", "jp"),
        ("india", "in"),
        ("turkey", "tr"),
        ("ukraine", "ua"),
        ("romania", "ro"),
        ("greece", "gr"),
        ("hungary", "hu"),
        ("singapore", "sg"),
        ("hong kong", "hk"),
        ("south korea", "kr"),
        ("korea", "kr"),
        ("china", "cn"),
        ("lithuania", "lt"),
        ("latvia", "lv"),
        ("estonia", "ee"),
        ("slovakia", "sk"),
        ("bulgaria", "bg"),
    ];
    names.iter().find(|(n, _)| *n == t).map(|(_, c)| (*c).to_owned())
}

/// A name on one line: line breaks and runs of spaces become one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Start pages separated by new lines, spaces, `;`, `|`, or commas *between* addresses (a comma inside a URL's query,
/// as in an OAuth link, stays).
fn urls(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in s.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|')) {
        let mut cur = String::new();
        for part in piece.split(',') {
            let starts_url = {
                let t = part.trim_start().to_ascii_lowercase();
                t.starts_with("http://")
                    || t.starts_with("https://")
                    || t.starts_with("www.")
                    || t.starts_with("about:")
            };
            if cur.is_empty() {
                cur.push_str(part);
            } else if starts_url {
                out.push(std::mem::take(&mut cur));
                cur.push_str(part);
            } else {
                cur.push(',');
                cur.push_str(part);
            }
        }
        out.push(cur);
    }
    out.into_iter()
        .map(|u| u.trim().trim_matches(|c| c == '"' || c == '\'').trim_end_matches(',').to_owned())
        .filter(|u| !u.is_empty() && (u.contains('.') || u.contains("://") || u.starts_with("about:")))
        .collect()
}

/// AdsPower's text in place of cookies that did not fit an Excel cell.
fn cookies_left_out(text: &str) -> bool {
    let t = text.to_lowercase();
    !t.trim_start().starts_with('[') && (t.contains("too large") || t.contains("too long") || t.contains("exceeds"))
}

// -------------------------------------------------------------------------------------------------------------
// tables

/// Split delimited text into rows of cells: RFC 4180 quoting (`""` inside quotes), the delimiter guessed from the
/// header line (`,` `;` or tab).
fn table(text: &str) -> Vec<Vec<String>> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("");
    let delim =
        [('\t', first.matches('\t').count()), (';', first.matches(';').count()), (',', first.matches(',').count())]
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .filter(|(_, n)| *n > 0)
            .map_or(',', |(d, _)| d);
    let mut rows = Vec::new();
    let (mut row, mut cell, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' if quoted => quoted = false,
            '"' if cell.trim().is_empty() => {
                cell.clear();
                quoted = true;
            }
            c if c == delim && !quoted => row.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut cell));
                if row.iter().any(|c| !c.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            c => cell.push(c),
        }
    }
    row.push(cell);
    if row.iter().any(|c| !c.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

/// Rows with a header line -> profiles.
fn from_rows(rows: Vec<Vec<String>>, out: &mut Parsed) {
    let Some((header, body)) = rows.split_first() else { return };
    // AdsPower's export (`acc_id`, `id`, `proxytype`, ...): `id` is its profile id, `acc_id` its serial
    let adspower = header.iter().any(|h| norm(h) == "accid") || header.iter().any(|h| norm(h) == "ipchecker");
    let cols: Vec<Option<Col>> = header
        .iter()
        .map(|h| match norm(h).as_str() {
            "id" | "userid" | "profileid" if adspower => Some(Col::AdsId),
            "accid" | "serialnumber" if adspower => Some(Col::AdsSerial),
            _ => column(h),
        })
        .collect();
    if adspower {
        out.notes.push("This is an AdsPower export: profiles are matched to AdsPower by their id.".into());
    }
    if cols.iter().all(Option::is_none) {
        // no header at all: maybe one proxy per line
        for (i, r) in rows.iter().enumerate() {
            from_proxy_line(&r.join(":"), i + 1, out);
        }
        return;
    }
    let ignored: Vec<&str> =
        header.iter().zip(&cols).filter(|(h, c)| c.is_none() && !h.trim().is_empty()).map(|(h, _)| h.trim()).collect();
    if !ignored.is_empty() {
        out.notes.push(format!("Columns not used: {}", ignored.join(", ")));
    }
    for (i, r) in body.iter().enumerate() {
        let get = |want: Col| -> String {
            cols.iter()
                .zip(r)
                .filter(|(c, _)| **c == Some(want))
                .map(|(_, v)| v.trim())
                .find(|v| !v.is_empty())
                .unwrap_or("")
                .to_owned()
        };
        let mut warnings = Vec::new();
        let mut p = Profile { name: get(Col::Name), remark: get(Col::Remark), ..Default::default() };
        // the proxy: one column, or type / host / port / user / password columns
        let kind = get(Col::ProxyType);
        let one = get(Col::Proxy);
        let text = if !one.is_empty() {
            one
        } else if !get(Col::ProxyHost).is_empty() {
            format!("{}:{}:{}:{}", get(Col::ProxyHost), get(Col::ProxyPort), get(Col::ProxyUser), get(Col::ProxyPass))
                .trim_end_matches(':')
                .to_owned()
        } else {
            String::new()
        };
        if !text.is_empty() {
            match proxy(&text, &kind) {
                Ok(c) => p.user_proxy_config = c,
                Err(e) if e.is_empty() => {}
                Err(e) => warnings.push(format!("{e}; imported without a proxy")),
            }
        }
        p.domain_name = get(Col::Platform);
        p.username = get(Col::Username);
        p.password = get(Col::Password);
        p.fakey = get(Col::TwoFa);
        p.ip = get(Col::Ip);
        p.tabs = urls(&get(Col::Tabs));
        match os(&get(Col::Os)) {
            Some(o) => p.persona_os = o.to_owned(),
            None => warnings.push(format!("OS `{}` not understood; using this computer's", get(Col::Os))),
        }
        match country(&get(Col::Country)) {
            Some(c) => p.country = c,
            None => warnings.push(format!("country `{}` not understood; it will follow the proxy", get(Col::Country))),
        }
        let ua = get(Col::UserAgent);
        if !ua.is_empty() {
            p.fingerprint_config.0.insert("ua".into(), Value::String(ua));
        }
        let ads_id = get(Col::AdsId);
        if !ads_id.is_empty() {
            p.source = format!("adspower:{ads_id}");
        }
        let mut cookies_elsewhere = false;
        let cookie = get(Col::Cookie);
        if cookies_left_out(&cookie) {
            cookies_elsewhere = true;
            if ads_id.is_empty() {
                warnings.push(
                    "the file left the cookies out (too large for Excel) and has no AdsPower id to fetch them".into(),
                );
            }
        } else if !cookie.is_empty() {
            match crate::cookies::parse(&cookie, &crate::cookies::default_domain(&p.domain_name, &p.tabs)) {
                Ok(_) => p.cookie = cookie,
                Err(e) => warnings.push(format!("cookies left out: {e}")),
            }
        }
        let tags = get(Col::Tags);
        let serial = get(Col::AdsSerial);
        let mut extra: Vec<String> = Vec::new();
        if !tags.is_empty() {
            extra.push(format!("Tags: {tags}"));
        }
        if !serial.is_empty() {
            extra.push(format!("AdsPower #{serial}"));
        }
        if !extra.is_empty() {
            let tail = extra.join(" \u{b7} ");
            p.remark = if p.remark.trim().is_empty() { tail } else { format!("{} ({tail})", p.remark.trim()) };
        }
        let saved = get(Col::SavedProxy);
        if !saved.is_empty() && saved != "0" && p.user_proxy_config.is_direct() {
            warnings.push(format!(
                "uses AdsPower's saved proxy #{saved}, which the file does not contain: set the proxy by hand"
            ));
        }
        p.name = one_line(&p.name);
        if p.name.is_empty() {
            p.name = if p.username.is_empty() { format!("Imported {}", i + 1) } else { p.username.clone() };
        }
        out.profiles.push(Incoming { profile: p, group: get(Col::Group), warnings, cookies_elsewhere });
    }
}

fn from_proxy_line(line: &str, n: usize, out: &mut Parsed) {
    match proxy(line, "") {
        Ok(c) => {
            let host = c.proxy_host.clone().unwrap_or_default();
            out.profiles.push(Incoming {
                profile: Profile { name: format!("Proxy {n} ({host})"), user_proxy_config: c, ..Default::default() },
                group: String::new(),
                warnings: Vec::new(),
                cookies_elsewhere: false,
            });
        }
        Err(e) if e.is_empty() => {}
        Err(e) => out.notes.push(format!("line {n}: {e}")),
    }
}

/// AdsPower's API shape (`{data:{list:[...]}}` or a list of profile objects) -> profiles.
pub fn from_json(v: &Value, groups: &[(String, String)], out: &mut Parsed) {
    let list = v.pointer("/data/list").or_else(|| v.get("list")).unwrap_or(v);
    let Some(items) = list.as_array() else {
        out.notes.push("the JSON holds no list of profiles".into());
        return;
    };
    for (i, item) in items.iter().enumerate() {
        // AdsPower writes numbers as text ("381"): the fields re-assigned on import are dropped before reading
        let mut clean = item.clone();
        if let Some(o) = clean.as_object_mut() {
            for k in [
                "serial_number",
                "profile_no",
                "created_time",
                "last_open_time",
                "last_close_time",
                "user_id",
                "profile_id",
            ] {
                o.remove(k);
            }
        }
        let Ok(mut p) = serde_json::from_value::<Profile>(clean) else {
            out.notes.push(format!("item {} is not a profile; left out", i + 1));
            continue;
        };
        let mut warnings = Vec::new();
        let source_id = item.get("user_id").or_else(|| item.get("profile_id")).and_then(Value::as_str).unwrap_or("");
        if !source_id.is_empty() {
            p.source = format!("adspower:{source_id}");
        }
        let serial = item
            .get("serial_number")
            .or_else(|| item.get("profile_no"))
            .and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())));
        (p.profile_id, p.serial_number, p.created_time, p.last_open_time, p.last_close_time) =
            (String::new(), 0, 0, 0, 0);
        if p.country.is_empty()
            && let Some(c) = item.get("ip_country").and_then(Value::as_str)
        {
            p.country = country(c).unwrap_or_default();
        }
        if p.user_proxy_config.proxy_soft.as_deref() == Some("no_proxy")
            || p.user_proxy_config.proxy_host.as_deref().unwrap_or("").is_empty()
        {
            p.user_proxy_config = ProxyConfig::default();
        }
        if !p.cookie.trim().is_empty() && crate::cookies::parse(&p.cookie, &p.domain_name).is_err() {
            warnings.push("cookies left out: not in a format Citroenition reads".into());
            p.cookie.clear();
        }
        if let Some(s) = serial
            && !s.is_empty()
        {
            let tag = format!("AdsPower #{s}");
            p.remark = if p.remark.trim().is_empty() { tag } else { format!("{} ({tag})", p.remark.trim()) };
        }
        let gid = item
            .get("group_id")
            .and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())))
            .unwrap_or_default();
        let group = item
            .get("group_name")
            .and_then(Value::as_str)
            .filter(|g| !g.is_empty())
            .map(str::to_owned)
            .or_else(|| groups.iter().find(|(id, _)| *id == gid && gid != "0").map(|(_, n)| n.clone()))
            .unwrap_or_default();
        p.group_id.clear();
        p.name = one_line(&p.name);
        if p.name.is_empty() {
            p.name = format!("Imported {}", i + 1);
        }
        out.profiles.push(Incoming { profile: p, group, warnings, cookies_elsewhere: false });
    }
}

// -------------------------------------------------------------------------------------------------------------
// xlsx

/// The first worksheet of an `.xlsx` as rows of cell texts.
fn xlsx_rows(bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a readable .xlsx: {e}"))?;
    let mut read = |name: &str| -> Option<String> {
        let mut f = z.by_name(name).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let shared: Vec<String> = read("xl/sharedStrings.xml")
        .map(|x| {
            let mut out = Vec::new();
            for si in x.split("<si>").skip(1) {
                let si = si.split("</si>").next().unwrap_or("");
                // a string may be split into rich-text runs: join every <t>
                let text: String = si
                    .split("<t")
                    .skip(1)
                    .filter_map(|t| t.split_once('>').map(|(_, rest)| rest.split("</t>").next().unwrap_or("")))
                    .collect();
                out.push(xml_unescape(&text));
            }
            out
        })
        .unwrap_or_default();
    let sheet = read("xl/worksheets/sheet1.xml").ok_or("the .xlsx has no first sheet")?;
    let mut rows = Vec::new();
    for r in sheet.split("<row").skip(1) {
        let r = r.split("</row>").next().unwrap_or("");
        let mut cells: Vec<String> = Vec::new();
        for c in r.split("<c ").skip(1) {
            let (attrs, body) = c.split_once('>').unwrap_or((c, ""));
            let col = attrs
                .split("r=\"")
                .nth(1)
                .map(|a| {
                    a.chars().take_while(|ch| ch.is_ascii_uppercase()).fold(0usize, |n, ch| n * 26 + (ch as usize - 64))
                })
                .unwrap_or(cells.len() + 1);
            let kind = attrs.split("t=\"").nth(1).and_then(|a| a.split('"').next()).unwrap_or("");
            let value = if kind == "inlineStr" {
                body.split("<t")
                    .nth(1)
                    .and_then(|t| t.split_once('>'))
                    .map(|(_, v)| v.split("</t>").next().unwrap_or(""))
                    .unwrap_or("")
                    .to_owned()
            } else {
                body.split("<v>").nth(1).and_then(|v| v.split("</v>").next()).unwrap_or("").to_owned()
            };
            let text = match kind {
                "s" => value.parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                _ => xml_unescape(&value),
            };
            while cells.len() + 1 < col {
                cells.push(String::new());
            }
            cells.push(text);
        }
        if cells.iter().any(|c| !c.trim().is_empty()) {
            rows.push(cells);
        }
    }
    Ok(rows)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&amp;", "&")
}

// -------------------------------------------------------------------------------------------------------------

/// Read a file the user dropped or named.
pub fn parse_file(bytes: &[u8], name: &str) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    if bytes.starts_with(b"PK") || name.to_lowercase().ends_with(".xlsx") {
        out.notes.push("Read as an Excel workbook (first sheet).".into());
        from_rows(xlsx_rows(bytes)?, &mut out);
    } else {
        let text = String::from_utf8_lossy(bytes);
        let t = text.trim_start_matches('\u{feff}').trim();
        if t.starts_with('[') || t.starts_with('{') {
            let v: Value = serde_json::from_str(t).map_err(|e| format!("the JSON cannot be read: {e}"))?;
            from_json(&v, &[], &mut out);
        } else {
            from_rows(table(t), &mut out);
        }
    }
    if out.profiles.is_empty() {
        return Err(match out.notes.last() {
            Some(n) => format!("no profile found in the file ({n})"),
            None => "no profile found in the file".into(),
        });
    }
    Ok(out)
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub created: usize,
    /// Imported before from the same source (`Profile::source`) and given what they were missing.
    pub updated: usize,
    /// Imported before, nothing new to add.
    pub skipped: usize,
    pub groups_created: usize,
}

/// Give `old` (imported before from the same place) whatever it lacks and `new` has. Nothing set is overwritten,
/// except cookies when `new` brings some and `old` has none.
fn fill_in(old: &mut Profile, new: &Profile, new_group: Option<String>) -> bool {
    let before = old.clone();
    let take = |a: &mut String, b: &String| {
        if a.trim().is_empty() && !b.trim().is_empty() {
            *a = b.clone();
        }
    };
    take(&mut old.cookie, &new.cookie);
    take(&mut old.domain_name, &new.domain_name);
    take(&mut old.username, &new.username);
    take(&mut old.password, &new.password);
    take(&mut old.fakey, &new.fakey);
    take(&mut old.country, &new.country);
    take(&mut old.persona_os, &new.persona_os);
    take(&mut old.remark, &new.remark);
    if old.tabs.is_empty() {
        old.tabs = new.tabs.clone();
    }
    if old.bookmarks.is_empty() {
        old.bookmarks = new.bookmarks.clone();
    }
    if old.user_proxy_config.is_direct() && !new.user_proxy_config.is_direct() {
        old.user_proxy_config = new.user_proxy_config.clone();
    }
    for (k, v) in &new.fingerprint_config.0 {
        old.fingerprint_config.0.entry(k.clone()).or_insert_with(|| v.clone());
    }
    if let Some(g) = new_group
        && (old.group_id.is_empty() || old.group_id == "0")
    {
        old.group_id = g;
    }
    *old != before
}

/// Add `parsed` to the store: groups by name (created when missing), every profile a new one with its own id (its
/// fingerprint seed), except the ones whose source is already in the store.
pub fn apply(home: &crate::store::Home, parsed: &Parsed) -> crate::Result<Report> {
    crate::store::locked(home, |s| {
        let mut r = Report::default();
        let before = s.groups().len();
        for g in &parsed.groups {
            if !g.trim().is_empty() {
                s.group_id_for(g)?;
            }
        }
        for inc in &parsed.profiles {
            let src = inc.profile.source.clone();
            if let Some(existing) = s.profiles().iter().find(|p| !src.is_empty() && p.source == src).cloned() {
                let group = if inc.group.trim().is_empty() { None } else { Some(s.group_id_for(&inc.group)?) };
                let mut merged = existing.clone();
                if fill_in(&mut merged, &inc.profile, group) {
                    s.update(&existing.profile_id, |p| *p = merged)?;
                    r.updated += 1;
                } else {
                    r.skipped += 1;
                }
                continue;
            }
            let mut p = inc.profile.clone();
            p.group_id = if inc.group.trim().is_empty() { "0".into() } else { s.group_id_for(&inc.group)? };
            s.create(p)?;
            r.created += 1;
        }
        r.groups_created = s.groups().len() - before;
        Ok(r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_in_every_notation() {
        let hop = |t: &str, k: &str| proxy(t, k).unwrap().hop().unwrap();
        for t in [
            "socks5://u:p@1.2.3.4:1080",
            "u:p@1.2.3.4:1080",
            "1.2.3.4:1080@u:p",
            "1.2.3.4:1080:u:p",
            "socks5 1.2.3.4 1080 u p",
            " \"socks5://u:p@1.2.3.4:1080/\" ",
        ] {
            let k = if t.contains("socks5") { "" } else { "socks5" };
            let h = hop(t, k);
            assert_eq!(
                (h.scheme.as_str(), h.host.as_str(), h.port.as_str(), h.account.as_str(), h.password.as_str()),
                ("socks5", "1.2.3.4", "1080", "u", "p"),
                "{t}"
            );
        }
        assert_eq!(hop("1.2.3.4:8080", "").scheme, "http");
        assert_eq!(hop("1.2.3.4:1080:user:pa:ss", "").password, "pa:ss", "a password may hold ':'");
        assert!(proxy("none", "").unwrap_err().is_empty(), "`none` = no proxy, no warning");
        assert!(proxy("1.2.3.4", "").is_err());
    }

    #[test]
    fn the_sample_csv_imports_cleanly() {
        let p = parse_file(SAMPLE_CSV.as_bytes(), "sample.csv").unwrap();
        assert_eq!(p.profiles.len(), 3);
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let a = &p.profiles[0];
        assert_eq!(
            (a.profile.name.as_str(), a.group.as_str(), a.profile.persona_os.as_str(), a.profile.country.as_str()),
            ("Shop FR - Camille", "Vinted FR", "macos", "fr")
        );
        assert_eq!(a.profile.user_proxy_config.hop().unwrap().account, "user");
        assert!(a.warnings.is_empty());
        assert_eq!(p.profiles[1].profile.tabs.len(), 2);
        assert_eq!(p.profiles[1].profile.user_proxy_config.hop().unwrap().port, "8080");
        assert!(p.profiles[2].profile.user_proxy_config.is_direct());
        assert!(p.profiles[2].profile.cookie.contains("sid"));
    }

    #[test]
    fn almost_right_rows_are_imported_with_warnings() {
        let csv = "\u{feff}Profile Name;Proxy Host;Proxy Port;Proxy User;Proxy Password;Proxy Type;Operating System;Country;Favourite colour\n\
                   ;5.6.7.8;3128;bob;pw;SOCKS5;Windows 11;Germany;blue\n\
                   Two;9.9.9.9;notaport;;;http;TempleOS;Atlantis;red\n";
        let p = parse_file(csv.as_bytes(), "x.csv").unwrap();
        assert_eq!(p.profiles.len(), 2);
        assert!(p.notes.iter().any(|n| n.contains("Favourite colour")));
        let a = &p.profiles[0].profile;
        assert_eq!((a.name.as_str(), a.persona_os.as_str(), a.country.as_str()), ("Imported 1", "windows", "de"));
        assert_eq!(a.user_proxy_config.hop().unwrap().scheme, "socks5");
        let b = &p.profiles[1];
        assert!(b.profile.user_proxy_config.is_direct());
        assert_eq!(b.warnings.len(), 3, "{:?}", b.warnings); // proxy, OS, country
    }

    #[test]
    fn plain_proxy_lists_become_profiles() {
        let p = parse_file(b"1.2.3.4:1080:u:p\nsocks5://5.6.7.8:9050\n\n", "proxies.txt").unwrap();
        assert_eq!(p.profiles.len(), 2);
        assert_eq!(p.profiles[1].profile.user_proxy_config.hop().unwrap().scheme, "socks5");
    }

    #[test]
    fn adspower_api_json() {
        let v: Value = serde_json::from_str(r#"{"code":0,"data":{"list":[
            {"user_id":"k1","serial_number":"381","name":"Flow","group_id":"9","group_name":"","remark":"main","domain_name":"google.com",
             "ip_country":"cz","user_proxy_config":{"proxy_soft":"other","proxy_type":"socks5","proxy_host":"1.2.3.4","proxy_port":"1080","proxy_user":"u","proxy_password":"p"},
             "cookie":"[{\"name\":\"a\",\"value\":\"1\",\"domain\":\".google.com\",\"session\":true,\"expires\":1824066717}]"},
            {"user_id":"k2","serial_number":"382","name":"","group_id":"0","user_proxy_config":{"proxy_soft":"no_proxy"}}]}}"#).unwrap();
        let mut out = Parsed::default();
        from_json(&v, &[("9".into(), "TikTok Ads".into())], &mut out);
        let a = &out.profiles[0];
        assert_eq!(
            (a.group.as_str(), a.profile.country.as_str(), a.profile.source.as_str()),
            ("TikTok Ads", "cz", "adspower:k1")
        );
        assert_eq!(a.profile.remark, "main (AdsPower #381)");
        assert_eq!(a.profile.user_proxy_config.hop().unwrap().password, "p");
        assert!(a.profile.profile_id.is_empty() && a.profile.serial_number == 0);
        let b = &out.profiles[1];
        assert_eq!((b.profile.name.as_str(), b.group.as_str()), ("Imported 2", ""));
        assert!(b.profile.user_proxy_config.is_direct());
    }

    #[test]
    fn applying_creates_groups_and_skips_what_was_imported_before() {
        let root = std::env::temp_dir().join(format!("citro-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = crate::store::Home { root: root.clone() };
        let mut parsed = parse_file(SAMPLE_CSV.as_bytes(), "s.csv").unwrap();
        parsed.profiles[0].profile.source = "adspower:k1".into();
        parsed.groups.push("Empty group".into());
        let r = apply(&home, &parsed).unwrap();
        assert_eq!(r, Report { created: 3, skipped: 0, groups_created: 4 });
        let r = apply(&home, &parsed).unwrap();
        assert_eq!((r.created, r.skipped, r.groups_created), (2, 1, 0), "the AdsPower one is not imported twice");
        let s = crate::store::Store::open(&home).unwrap();
        let ids: std::collections::HashSet<_> = s.profiles().iter().map(|p| p.profile_id.clone()).collect();
        assert_eq!(ids.len(), 5, "every profile gets its own id (its fingerprint seed)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quoted_cells_with_commas_and_newlines() {
        let rows = table("name,remark\n\"A, B\",\"line 1\nline \"\"2\"\"\"\n");
        assert_eq!(
            rows,
            vec![vec!["name".to_owned(), "remark".into()], vec!["A, B".into(), "line 1\nline \"2\"".into()]]
        );
    }
}
