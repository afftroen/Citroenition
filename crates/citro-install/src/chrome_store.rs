//! Downloading an extension from the Chrome Web Store.
//!
//! The store serves an extension as a CRX file from Google's update service (the same request Chrome makes when it
//! installs one): `clients2.google.com/service/update2/crx?response=redirect&prodversion=<chromium>&x=id%3D<id>%26uc`.
//! A CRX is a ZIP with a signed header in front ("Cr24", version 2 or 3). The header is skipped, the ZIP unpacked with
//! the same path-safe extractor as the kernel, and the store's `_metadata` folder removed: Chromium refuses to load an
//! unpacked extension that has a top-level name starting with `_` other than `_locales`.
//!
//! Trust: TLS to Google; the CRX signature is not checked here (the browser does not check an unpacked extension
//! either). Install only extensions you would install in Chrome.

use crate::error::{Error, IoContext, Result};
use crate::extract::{self, Limits, Naming};
use crate::http::Http;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_CRX_BYTES: u64 = 256 << 20;

/// A Chrome Web Store id: 32 letters `a`-`p`.
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn crx_url(id: &str, chromium_major: u32) -> String {
    format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={chromium_major}.0.0.0\
         &acceptformat=crx2,crx3&x=id%3D{id}%26installsource%3Dondemand%26uc"
    )
}

/// The ZIP inside a CRX (or the bytes themselves when they already are a ZIP).
pub fn crx_zip(bytes: &[u8]) -> Result<&[u8]> {
    let u32le = |at: usize| -> Result<usize> {
        let b = bytes.get(at..at + 4).ok_or_else(|| Error::BadArchive("the extension file is truncated".into()))?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok(bytes);
    }
    if !bytes.starts_with(b"Cr24") {
        return Err(Error::BadArchive("not a Chrome extension (CRX) file".into()));
    }
    let start = match u32le(4)? {
        3 => 12 + u32le(8)?,
        2 => 16 + u32le(8)? + u32le(12)?,
        v => return Err(Error::BadArchive(format!("unknown CRX version {v}"))),
    };
    bytes.get(start..).filter(|z| z.starts_with(b"PK")).ok_or_else(|| Error::BadArchive("the CRX holds no ZIP".into()))
}

/// Download extension `id` for a Chromium of major version `chromium_major` and unpack it into a new folder inside
/// `work` (created). Returns that folder; [`citro_core::extensions::install`] moves it into place.
pub fn download_extension(id: &str, chromium_major: u32, work: &Path, timeout: Duration) -> Result<PathBuf> {
    if !valid_id(id) {
        return Err(Error::InvalidInput { what: "Chrome Web Store extension id", value: id.to_owned() });
    }
    let bytes = Http::new(timeout).get_bytes(&crx_url(id, chromium_major.max(100)), MAX_CRX_BYTES)?;
    if bytes.is_empty() {
        return Err(Error::BadArchive(format!("the Chrome Web Store has no extension {id} (or it is not public)")));
    }
    unpack_crx(&bytes, id, work)
}

fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at)?;
        *at += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The length-delimited fields `field` of a protobuf message.
fn proto_bytes(msg: &[u8], field: u64) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < msg.len() {
        let Some(tag) = varint(msg, &mut at) else { break };
        match tag & 7 {
            0 => {
                if varint(msg, &mut at).is_none() {
                    break;
                }
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let Some(len) = varint(msg, &mut at) else { break };
                let end = at.saturating_add(len as usize);
                let Some(v) = msg.get(at..end) else { break };
                if tag >> 3 == field {
                    out.push(v);
                }
                at = end;
            }
            _ => break,
        }
    }
    out
}

/// The extension id a public key gives: the first 16 bytes of its SHA-256, one letter `a`-`p` per nibble.
pub fn id_of_key(key: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(key)[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect()
}

/// The store's public key of extension `id` in a CRX3 header (`CrxFileHeader.sha256_with_rsa[].public_key`).
pub fn crx3_key(bytes: &[u8], id: &str) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"Cr24") || bytes.get(4..8)? != 3u32.to_le_bytes() {
        return None;
    }
    let len = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?) as usize;
    let header = bytes.get(12..12 + len)?;
    proto_bytes(header, 2)
        .into_iter()
        .flat_map(|proof| proto_bytes(proof, 1))
        .find(|key| id_of_key(key) == id)
        .map(<[u8]>::to_vec)
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Unpack CRX bytes into `<work>/<id>-unpacked` (replacing it).
///
/// The store's public key goes into `manifest.json` as `key`: Chromium names an unpacked extension after its folder
/// otherwise, and a different id means different extension storage (a wallet would look empty after the data folder
/// moved). With the key the id is the store's, wherever the folder is.
pub fn unpack_crx(bytes: &[u8], id: &str, work: &Path) -> Result<PathBuf> {
    let zip = crx_zip(bytes)?;
    fs::create_dir_all(work).context(|| format!("cannot create {}", work.display()))?;
    let zip_path = work.join(format!("{id}.zip"));
    fs::write(&zip_path, zip).context(|| format!("cannot write {}", zip_path.display()))?;
    let out = work.join(format!("{id}-unpacked"));
    if out.exists() {
        fs::remove_dir_all(&out).context(|| format!("cannot clear {}", out.display()))?;
    }
    fs::create_dir_all(&out).context(|| format!("cannot create {}", out.display()))?;
    let limits = Limits { max_total_bytes: 512 << 20, max_entries: 50_000 };
    let r = extract::extract_zip(&zip_path, &out, limits, Naming::Posix);
    let _ = fs::remove_file(&zip_path);
    r?;
    let manifest_path = out.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(Error::MissingBinary("manifest.json".into()));
    }
    if let Some(key) = crx3_key(bytes, id) {
        let text = fs::read(&manifest_path).context(|| "cannot read manifest.json".to_owned())?;
        // a manifest may start with a byte-order mark
        let text = text.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&text);
        if let Ok(serde_json::Value::Object(mut m)) = serde_json::from_slice::<serde_json::Value>(text) {
            m.insert("key".into(), serde_json::Value::String(base64(&key)));
            let json = serde_json::to_vec_pretty(&m).map_err(|e| Error::BadArchive(e.to_string()))?;
            fs::write(&manifest_path, json).context(|| "cannot write manifest.json".to_owned())?;
        }
    }
    for e in fs::read_dir(&out).context(|| format!("cannot read {}", out.display()))?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('_') && name != "_locales" {
            let _ = fs::remove_dir_all(e.path()).or_else(|_| fs::remove_file(e.path()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{ZipEntry, zip};

    fn crx3(zip_bytes: &[u8]) -> Vec<u8> {
        let header = b"signed-header-bytes";
        let mut v = b"Cr24".to_vec();
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&(header.len() as u32).to_le_bytes());
        v.extend_from_slice(header);
        v.extend_from_slice(zip_bytes);
        v
    }

    #[test]
    fn a_crx3_unpacks_without_the_store_metadata() {
        let z = zip(&[
            ZipEntry::file("manifest.json", br#"{"name":"x","version":"1"}"#),
            ZipEntry::file("_metadata/verified_contents.json", b"{}"),
            ZipEntry::file("_locales/en/messages.json", b"{}"),
            ZipEntry::file("js/bg.js", b"//"),
        ]);
        let t = tempfile::tempdir().unwrap();
        let id = "cjpalhdlnbpafiamejdnhcphjbkeiagm";
        let out = unpack_crx(&crx3(&z), id, t.path()).unwrap();
        assert!(out.join("manifest.json").is_file() && out.join("js/bg.js").is_file());
        assert!(out.join("_locales/en/messages.json").is_file());
        assert!(!out.join("_metadata").exists(), "Chromium refuses unpacked extensions with _metadata");
        assert!(!t.path().join(format!("{id}.zip")).exists());
    }

    #[test]
    fn the_store_key_is_found_and_names_the_extension() {
        let key = b"a fake DER public key";
        let id = id_of_key(key);
        assert_eq!(id.len(), 32);
        // CrxFileHeader { sha256_with_rsa (2): AsymmetricKeyProof { public_key (1), signature (2) }, signed_header_data (10000) }
        let other = b"the publisher key";
        let proof = |k: &[u8]| {
            let mut p = vec![0x0a, k.len() as u8];
            p.extend_from_slice(k);
            p.extend_from_slice(&[0x12, 3, 1, 2, 3]);
            p
        };
        let mut header = Vec::new();
        for k in [&other[..], &key[..]] {
            let pr = proof(k);
            header.push(0x12);
            header.push(pr.len() as u8);
            header.extend_from_slice(&pr);
        }
        header.extend_from_slice(&[0x82, 0xf1, 0x04, 2, 0x0a, 0]); // field 10000, an empty crx_id
        let mut crx = b"Cr24".to_vec();
        crx.extend_from_slice(&3u32.to_le_bytes());
        crx.extend_from_slice(&(header.len() as u32).to_le_bytes());
        crx.extend_from_slice(&header);
        assert_eq!(crx3_key(&crx, &id).as_deref(), Some(&key[..]));
        assert_eq!(crx3_key(&crx, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), None);
        // and it lands in the manifest
        let z = zip(&[ZipEntry::file("manifest.json", br#"{"name":"x","version":"1"}"#)]);
        crx.extend_from_slice(&z);
        let t = tempfile::tempdir().unwrap();
        let out = unpack_crx(&crx, &id, t.path()).unwrap();
        let m: serde_json::Value = serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m["key"], base64(key));
        assert_eq!(m["name"], "x");
    }

    #[test]
    fn crx_headers() {
        let z = zip(&[ZipEntry::file("manifest.json", b"{}")]);
        assert!(crx_zip(&crx3(&z)).unwrap().starts_with(b"PK"));
        let mut v2 = b"Cr24".to_vec();
        for n in [2u32, 3, 4] {
            v2.extend_from_slice(&n.to_le_bytes());
        }
        v2.extend_from_slice(b"KEYSIGN");
        v2.extend_from_slice(&z);
        assert!(crx_zip(&v2).unwrap().starts_with(b"PK"));
        assert!(crx_zip(b"<html>not found</html>").is_err());
        assert!(crx_zip(b"Cr24\x03\x00\x00\x00").is_err());
    }
}
