//! Showing a kernel command line without its secrets (`citro open --dry-run`, logs, bug reports).

use crate::kernel_env::PERSONA_ENV;
use crate::launch::Prepared;

pub const HIDDEN: &str = "<hidden>";

/// Switches whose value is a credential.
const SECRET_SWITCHES: &[&str] = &["proxy-auth", "socks5-credentials"];

fn is_secret(name: &str) -> bool {
    SECRET_SWITCHES.contains(&name)
}

/// `--proxy-auth=user:pass` -> `--proxy-auth=<hidden>`; every other argument is returned unchanged.
pub fn redact_arg(arg: &str) -> String {
    match arg.strip_prefix("--").and_then(|r| r.split_once('=')) {
        Some((name, _)) if is_secret(name) => format!("--{name}={HIDDEN}"),
        _ => arg.to_owned(),
    }
}

/// Decode RFC 4648 base64 (the inverse of [`crate::kernel_env::base64`]).
pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(s.len() / 4 * 3), 0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The switches inside a `CLEARCOTE_PERSONA_ARGS` payload (`name NUL value NUL ...`, base64), as
/// `--name=value` with secrets hidden.
pub fn persona_switches(payload: &str) -> Option<Vec<String>> {
    let raw = unbase64(payload)?;
    let mut parts = raw.split(|b| *b == 0).map(|p| String::from_utf8_lossy(p).into_owned());
    let mut out = Vec::new();
    while let (Some(name), Some(value)) = (parts.next(), parts.next()) {
        out.push(if is_secret(&name) { format!("--{name}={HIDDEN}") } else { format!("--{name}={value}") });
    }
    Some(out)
}

/// What [`Prepared`] would run, safe to print: the argument list, and the environment where the persona
/// payload is shown decoded (as `CLEARCOTE_PERSONA_ARGS` -> list of switches).
pub fn command_line(prep: &Prepared) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let args = prep.args.iter().map(|a| redact_arg(a)).collect();
    let env = prep
        .env
        .iter()
        .map(|(k, v)| match (k.as_str() == PERSONA_ENV).then(|| persona_switches(v)).flatten() {
            Some(list) => (k.clone(), list),
            None => (k.clone(), vec![v.clone()]),
        })
        .collect();
    (args, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_env::{base64, persona_to_env};
    use std::path::PathBuf;

    #[test]
    fn base64_roundtrips_every_length() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(unbase64(&base64(&data)).as_deref(), Some(&data[..]), "len {n}");
        }
        assert_eq!(unbase64("***"), None);
    }

    #[test]
    fn credentials_are_hidden_and_nothing_else_is() {
        assert_eq!(redact_arg("--proxy-auth=u:secret"), "--proxy-auth=<hidden>");
        assert_eq!(redact_arg("--socks5-credentials=u:secret"), "--socks5-credentials=<hidden>");
        assert_eq!(redact_arg("--proxy-server=socks5://h:1"), "--proxy-server=socks5://h:1");
        assert_eq!(redact_arg("--proxy-authenticate=x"), "--proxy-authenticate=x");
        assert_eq!(redact_arg("https://example.com/?a=b"), "https://example.com/?a=b");
    }

    #[test]
    fn the_persona_payload_is_shown_decoded_with_secrets_hidden() {
        let args: Vec<String> =
            ["--fingerprint=abc", "--socks5-credentials=u:secret", "--lang=fr"].map(str::to_owned).to_vec();
        let (kept, payload) = persona_to_env(&args).unwrap();
        let prep = Prepared {
            args: kept,
            env: vec![(PERSONA_ENV.to_owned(), payload.clone()), ("LANGUAGE".into(), "fr".into())],
            data_dir: PathBuf::from("/d"),
            relay: None,
            start_urls: Vec::new(),
            later_tabs: Vec::new(),
        };
        let (shown_args, shown_env) = command_line(&prep);
        assert_eq!(shown_args, ["--lang=fr", "--persona-from-env"]);
        assert_eq!(
            shown_env[0],
            (PERSONA_ENV.to_owned(), vec!["--fingerprint=abc".to_owned(), "--socks5-credentials=<hidden>".into()])
        );
        assert_eq!(shown_env[1], ("LANGUAGE".to_owned(), vec!["fr".to_owned()]));
        assert!(!format!("{shown_args:?}{shown_env:?}").contains("secret"));
    }
}
