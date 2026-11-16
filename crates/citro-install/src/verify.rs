//! Trust.
//!
//! A kernel is trusted when two independent facts hold, and both are hard failures:
//!
//! 1. the detached OpenPGP signature `SHA256SUMS.txt.asc` verifies over `SHA256SUMS.txt` against the release-signing
//!    key that is **compiled into this binary** (`assets/clearcote-signing-key.asc`). A key published next to the
//!    release is never consulted: whoever can replace a release asset can replace that key too;
//! 2. the SHA-256 of the downloaded archive (computed while streaming it to disk) equals the line for that very file
//!    name in the signed list.

use crate::error::{Error, Result};
use crate::platform::Platform;
use crate::version::compare_versions;
use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::types::KeyDetails;
use std::sync::OnceLock;

/// The pinned release-signing public key (armored). It is part of the source tree on purpose: changing it is a code
/// change that gets reviewed, not something a download can do.
pub const PINNED_KEY_ARMORED: &[u8] = include_bytes!("../assets/clearcote-signing-key.asc");

/// A signed checksum list is a handful of lines; anything beyond this is not one.
pub const MAX_SUMS_BYTES: usize = 1 << 20;
/// A detached signature is a few hundred bytes.
pub const MAX_SIGNATURE_BYTES: usize = 1 << 16;

/// Signatures made with a weaker digest than SHA-224 (MD5, SHA-1, RIPEMD-160, unknown ids) are refused.
fn hash_is_acceptable(alg: HashAlgorithm) -> bool {
    matches!(
        alg,
        HashAlgorithm::Sha224
            | HashAlgorithm::Sha256
            | HashAlgorithm::Sha384
            | HashAlgorithm::Sha512
            | HashAlgorithm::Sha3_256
            | HashAlgorithm::Sha3_512
    )
}

/// Parse an armored public key and check that its self-signatures hold together.
pub(crate) fn parse_key(armored: &[u8]) -> Result<SignedPublicKey> {
    let (key, _headers) = SignedPublicKey::from_armor_single(armored)
        .map_err(|e| Error::Signature(format!("unreadable public key: {e}")))?;
    key.verify_bindings().map_err(|e| Error::Signature(format!("the public key is not self-consistent: {e}")))?;
    Ok(key)
}

/// The pinned key, parsed once.
pub(crate) fn pinned_key() -> Result<&'static SignedPublicKey> {
    static KEY: OnceLock<Result<SignedPublicKey>> = OnceLock::new();
    match KEY.get_or_init(|| parse_key(PINNED_KEY_ARMORED)) {
        Ok(key) => Ok(key),
        Err(e) => Err(Error::Signature(format!("the pinned release-signing key is broken: {e}"))),
    }
}

/// The OpenPGP fingerprint (lowercase hex) of the pinned release-signing key.
pub fn pinned_key_fingerprint() -> Result<String> {
    Ok(pinned_key()?.fingerprint().to_string())
}

/// One `<sha256>  <name>` line of a checksum list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumEntry {
    /// The file name as written in the list (`./` and the `*` binary-mode marker removed).
    pub name: String,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

/// A checksum list that has been through [`verify_checksums`]: its OpenPGP signature checked out.
///
/// There is deliberately no public way to build one from unverified bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumList {
    entries: Vec<ChecksumEntry>,
    signed_by: String,
}

/// The archive picked out of a verified list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedArchive {
    /// `clearcote-<version>-<suffix>`
    pub name: String,
    /// The kernel version in the name.
    pub version: String,
    /// The signed SHA-256 (lowercase hex).
    pub sha256: String,
}

impl ChecksumList {
    /// Every line, in file order.
    pub fn entries(&self) -> &[ChecksumEntry] {
        &self.entries
    }

    /// Fingerprint (lowercase hex) of the key whose signature vouched for this list.
    pub fn signed_by(&self) -> &str {
        &self.signed_by
    }

    /// The signed SHA-256 of the file called exactly `name`.
    pub fn sha256_of(&self, name: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.sha256.as_str())
    }

    /// The archive for `platform`: the one of version `want` when given, else the highest version listed.
    pub fn find_archive(&self, platform: Platform, want: Option<&str>) -> Result<ListedArchive> {
        let mut best: Option<ListedArchive> = None;
        for e in &self.entries {
            let Some(version) = platform.version_of_asset(&e.name) else { continue };
            if want.is_some_and(|w| w != version) {
                continue;
            }
            if best.as_ref().is_none_or(|b| compare_versions(&version, &b.version).is_gt()) {
                best = Some(ListedArchive { name: e.name.clone(), version, sha256: e.sha256.clone() });
            }
        }
        best.ok_or_else(|| Error::NoArchive {
            suffix: match want {
                Some(v) => platform.asset_name(v),
                None => platform.asset_suffix().to_owned(),
            },
        })
    }

    /// Parse `sha256sum` output: `<64 hex digits>  <name>` (or ` *<name>` for binary mode), one per line. Blank lines
    /// and `#` comments are skipped; anything else that does not parse is an error.
    fn parse(text: &[u8], signed_by: String) -> Result<ChecksumList> {
        let bad = |msg: String| Error::BadSums(msg);
        let text = std::str::from_utf8(text).map_err(|_| bad("not valid UTF-8".into()))?;
        let mut entries: Vec<ChecksumEntry> = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = n + 1;
            let digest = line.get(..64).ok_or_else(|| bad(format!("line {at}: too short for `<sha256>  <name>`")))?;
            if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad(format!("line {at}: the first 64 characters are not a hex SHA-256")));
            }
            let rest = &line[64..];
            if !rest.starts_with(' ') {
                return Err(bad(format!("line {at}: the digest must be followed by a space and the file name")));
            }
            let name = rest.trim_start_matches(' ');
            let name = name.strip_prefix('*').unwrap_or(name);
            let name = name.strip_prefix("./").unwrap_or(name);
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(bad(format!("line {at}: missing or unprintable file name")));
            }
            let sha256 = digest.to_ascii_lowercase();
            match entries.iter().find(|e| e.name == name) {
                Some(prev) if prev.sha256 != sha256 => {
                    return Err(bad(format!("`{name}` is listed twice with different digests")));
                }
                Some(_) => {}
                None => entries.push(ChecksumEntry { name: name.to_owned(), sha256 }),
            }
        }
        if entries.is_empty() {
            return Err(bad("it lists no files".into()));
        }
        Ok(ChecksumList { entries, signed_by })
    }
}

/// Check the detached OpenPGP signature `sig` (armored or binary) over `sums` against the PINNED release-signing key
/// and, only if it holds, parse `sums` into a [`ChecksumList`].
///
/// The signature must be made by the pinned key itself with SHA-224 or stronger. A list with any bit flipped, a
/// signature by any other key, or garbage in place of either file is an error.
pub fn verify_checksums(sums: &[u8], sig: &[u8]) -> Result<ChecksumList> {
    verify_with_key(pinned_key()?, sums, sig)
}

/// [`verify_checksums`] against an explicit key (the unit tests sign with a throw-away key).
pub(crate) fn verify_with_key(key: &SignedPublicKey, sums: &[u8], sig: &[u8]) -> Result<ChecksumList> {
    if sums.len() > MAX_SUMS_BYTES {
        return Err(Error::BadSums(format!("larger than {MAX_SUMS_BYTES} bytes")));
    }
    if sig.len() > MAX_SIGNATURE_BYTES {
        return Err(Error::Signature(format!("the signature file is larger than {MAX_SIGNATURE_BYTES} bytes")));
    }
    let mut failures: Vec<String> = Vec::new();
    for parsed in parse_signatures(sig)? {
        let signature = match parsed {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("unreadable signature: {e}"));
                continue;
            }
        };
        match signature.signature.hash_alg() {
            Some(alg) if hash_is_acceptable(alg) => {}
            other => {
                failures.push(format!("the signature uses a digest that is too weak or unknown ({other:?})"));
                continue;
            }
        }
        match signature.verify(key, sums) {
            Ok(()) => return ChecksumList::parse(sums, key.fingerprint().to_string()),
            Err(e) => failures.push(e.to_string()),
        }
    }
    let why =
        if failures.is_empty() { "the signature file holds no signature".to_owned() } else { failures.join("; ") };
    Err(Error::Signature(format!("SHA256SUMS.txt does not verify against the pinned release-signing key ({why})")))
}

/// The signatures in a `.asc` (ASCII-armored) or `.sig` (binary) file; one entry per signature packet.
fn parse_signatures(sig: &[u8]) -> Result<Vec<std::result::Result<DetachedSignature, String>>> {
    let unreadable = |e: pgp::errors::Error| Error::Signature(format!("unreadable signature file: {e}"));
    let armored = sig.trim_ascii_start().starts_with(b"-----BEGIN PGP");
    let parsed = if armored {
        DetachedSignature::from_armor_many_buf(sig).map_err(unreadable)?.0
    } else {
        DetachedSignature::from_bytes_many(sig).map_err(unreadable)?
    };
    Ok(parsed.map(|r| r.map_err(|e| e.to_string())).collect())
}

/// Lowercase hex.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a byte slice as lowercase hex.
#[cfg(test)]
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(&Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "6c06f95783410d09799828b93f7bb26ac8174354b87f580827c8bcf1f2226b7e";

    fn parse(text: &str) -> Result<ChecksumList> {
        ChecksumList::parse(text.as_bytes(), String::new())
    }

    #[test]
    fn the_pinned_key_is_the_documented_one() {
        // The asset file is the root of trust. If it ever changes, this test fails until a human updates the
        // fingerprint below on purpose (clearcote-release@users.noreply.github.com).
        assert_eq!(pinned_key_fingerprint().unwrap(), "ca96f185f96a693aedb3ac1fcb00d851b7a86b0f");
    }

    #[test]
    fn parses_sha256sum_output() {
        let l = parse(&format!(
            "{DIGEST}  clearcote-1.2.3-linux-x64.tar.xz\r\n\n# comment\n{} *bin.exe\n{} ./chrome\n",
            "A".repeat(64),
            "b".repeat(64)
        ))
        .unwrap();
        assert_eq!(l.entries().len(), 3);
        assert_eq!(l.sha256_of("clearcote-1.2.3-linux-x64.tar.xz"), Some(DIGEST));
        assert_eq!(l.sha256_of("bin.exe"), Some("a".repeat(64).as_str()));
        assert_eq!(l.sha256_of("chrome"), Some("b".repeat(64).as_str()));
        assert_eq!(l.sha256_of("missing"), None);
    }

    #[test]
    fn rejects_malformed_lists() {
        for bad in [
            "",
            "# only a comment\n",
            "tooshort  file\n",
            &format!("{}  file\n", "g".repeat(64)),
            &format!("{DIGEST}file\n"),
            &format!("{DIGEST}  \n"),
            &format!("{DIGEST}  a\u{7}b\n"),
            &format!("{DIGEST}  f\n{}  f\n", "0".repeat(64)),
        ] {
            assert!(matches!(parse(bad), Err(Error::BadSums(_))), "{bad:?}");
        }
        assert!(ChecksumList::parse(&[0xff, 0xfe], String::new()).is_err());
        // an identical duplicate is harmless
        assert_eq!(parse(&format!("{DIGEST}  f\n{DIGEST}  f\n")).unwrap().entries().len(), 1);
    }

    #[test]
    fn picks_the_archive_for_the_platform_and_version() {
        let l = parse(&format!(
            "{d}  clearcote-150.9.1-linux-x64.tar.xz\n{d}  clearcote-151.0.7922.108-linux-x64.tar.xz\n\
             {d}  clearcote-99.0.1-linux-x64.tar.xz\n{d}  clearcote-151.0.7922.108-windows-x64.zip\n\
             {d}  clearcote-151.0.7922.108-linux-x64.tar.xz.sha256\n{d}  chrome\n",
            d = DIGEST
        ))
        .unwrap();
        let newest = l.find_archive(Platform::LinuxX64, None).unwrap();
        assert_eq!(newest.version, "151.0.7922.108");
        assert_eq!(newest.name, "clearcote-151.0.7922.108-linux-x64.tar.xz");
        assert_eq!(l.find_archive(Platform::LinuxX64, Some("99.0.1")).unwrap().version, "99.0.1");
        assert_eq!(
            l.find_archive(Platform::WindowsX64, None).unwrap().name,
            "clearcote-151.0.7922.108-windows-x64.zip"
        );
        let e = l.find_archive(Platform::LinuxX64, Some("1.0.0")).unwrap_err();
        assert!(matches!(&e, Error::NoArchive { suffix } if suffix == "clearcote-1.0.0-linux-x64.tar.xz"), "{e}");
        let only_chrome = parse(&format!("{DIGEST}  chrome\n")).unwrap();
        assert!(matches!(only_chrome.find_archive(Platform::LinuxX64, None), Err(Error::NoArchive { .. })));
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
