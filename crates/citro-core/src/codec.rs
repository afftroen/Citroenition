//! AdsPower's obfuscated base64.
//!
//! Every config blob the AdsPower desktop app hands to the SunBrowser kernel
//! (`--extended-parameters`, and the `StaticConfig` / `DynamicConfig` files it
//! points at) is plain RFC 4648 base64 whose alphabet has been swapped for a
//! shuffled one (`encodeBase64` in the app's main bundle). `=` padding is kept
//! as-is.

use crate::{Error, Result};

/// The shuffled alphabet. Position `i` replaces the standard alphabet's `i`-th symbol.
pub const SHUFFLED: &[u8; 64] = b"hTy1bfRJz4nLPcBCO7WtmNIaGvVeul5Zo8kq32UxrYw_-0gsjp96SDFXQiEMKdHA";

const fn reverse_table() -> [u8; 256] {
    let mut t = [0xFFu8; 256];
    let mut i = 0;
    while i < 64 {
        t[SHUFFLED[i] as usize] = i as u8;
        i += 1;
    }
    t
}

const REVERSE: [u8; 256] = reverse_table();

/// Encode bytes with the shuffled alphabet (padded).
pub fn encode(input: impl AsRef<[u8]>) -> String {
    let data = input.as_ref();
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, ix) in idx.iter().enumerate() {
            if i <= chunk.len() {
                out.push(SHUFFLED[*ix as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode a shuffled-alphabet string. Whitespace and `=` padding are ignored.
pub fn decode(input: &str) -> Result<Vec<u8>> {
    let body: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    if body.len() % 4 == 1 {
        return Err(Error::Codec("truncated input".into()));
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4 + 3);
    for chunk in body.chunks(4) {
        let mut n: u32 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            let v = REVERSE[c as usize];
            if v == 0xFF {
                return Err(Error::Codec(format!("invalid character {:?}", c as char)));
            }
            n |= u32::from(v) << (18 - 6 * i as u32);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Serialise `value` as JSON and encode it.
pub fn encode_json<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(encode(serde_json::to_vec(value)?))
}

/// Decode a blob and parse it as JSON.
pub fn decode_json(input: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&decode(input)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_prefix_seen_in_real_adspower_config_files() {
        // Real StaticConfig files start with `{"C...` and begin with "5941".
        assert_eq!(encode("{\"C"), "5941");
    }

    #[test]
    fn empty_object() {
        assert_eq!(encode("{}"), "5XS=");
        assert_eq!(decode("5XS=").unwrap(), b"{}");
    }

    #[test]
    fn roundtrip_all_small_lengths() {
        for len in 0..200usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
            assert_eq!(decode(&encode(&data)).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("!!!!").is_err());
        assert!(decode("A").is_err());
    }

    #[test]
    fn json_roundtrip() {
        let v = serde_json::json!({"Platform": "Win32", "n": 6});
        assert_eq!(decode_json(&encode_json(&v).unwrap()).unwrap(), v);
    }
}
