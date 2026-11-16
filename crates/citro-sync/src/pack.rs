//! A profile's data directory as one archive (`.tar.gz`), and back.
//!
//! **In:** everything that makes the browser *the same browser* on another device: cookies, local storage,
//! IndexedDB, saved logins, extensions, preferences, history, session.
//! **Out:** caches (they rebuild), crash dumps, the component updater's downloads (large and per machine) and the
//! lock files Chromium leaves behind (`Singleton*`, `DevToolsActivePort`). Symlinks, sockets and pipes are never
//! archived.
//!
//! Unpacking trusts nothing: absolute paths, `..`, links and device nodes are refused, and the unpacked size is
//! capped, so a damaged or hostile archive cannot write outside the target directory.

use crate::error::{Error, Result};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

/// Names that are not archived. Matched against the first three levels of a path (`Default/Cache`,
/// `Default/Service Worker/CacheStorage`), so an extension's own `Cache` folder deep inside `Extensions/` is kept.
const SKIP: &[&str] = &[
    // caches
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GraphiteDawnCache",
    "GrShaderCache",
    "ShaderCache",
    "Media Cache",
    "CacheStorage",
    "ScriptCache",
    // diagnostics
    "Crashpad",
    "BrowserMetrics",
    "BrowserMetrics-spare.pma",
    // downloaded by the component updater, per machine, large
    "Safe Browsing",
    "component_crx_cache",
    "extensions_crx_cache",
    "optimization_guide_model_store",
    "OptGuideOnDeviceModel",
    "OnDeviceHeadSuggestModel",
    "Subresource Filter",
    "OriginTrials",
    "MEIPreload",
    "CertificateRevocation",
    "WasmTtsEngine",
    "ZxcvbnData",
    "FirstPartySetsPreloaded",
    "hyphen-data",
    "segmentation_platform",
    "screen_ai",
    "TrustTokenKeyCommitments",
    "PKIMetadata",
    "AutofillStates",
    "crl-set",
    "ssl_error_assistant",
    "SafetyTips",
    "Webstore Downloads",
];

/// Process lock files Chromium leaves in the data directory (only the top two levels).
fn is_lock_name(name: &str) -> bool {
    name.starts_with("Singleton") || name == "DevToolsActivePort" || name == "lockfile"
}

/// Is this path (relative to the data directory) left out of the archive?
pub fn skipped(rel: &Path) -> bool {
    let names: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    names.iter().take(3).any(|n| SKIP.contains(n)) || names.iter().take(2).any(|n| is_lock_name(n))
}

/// What went into (or came out of) an archive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Packed {
    pub files: u64,
    /// Bytes of file content, uncompressed.
    pub bytes: u64,
    /// Size of the archive file (packing only).
    pub archive: u64,
}

/// Archive `data_dir` into `dest` (created fresh, readable by the owner only). A directory that does not exist
/// yields an empty archive: the browser never ran.
pub fn pack(data_dir: &Path, dest: &Path) -> Result<Packed> {
    let file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dest)?;
    let gz = GzEncoder::new(BufWriter::new(file), Compression::new(3));
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    let mut stats = Packed::default();
    if data_dir.is_dir() {
        walk(&mut tar, data_dir, Path::new(""), &mut stats)?;
    }
    let mut out = tar.into_inner()?.finish()?;
    out.flush()?;
    out.get_ref().sync_all()?;
    stats.archive = fs::metadata(dest)?.len();
    Ok(stats)
}

fn walk<W: Write>(tar: &mut tar::Builder<W>, root: &Path, rel: &Path, stats: &mut Packed) -> Result<()> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(root.join(rel))?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let rel = rel.join(e.file_name());
        if skipped(&rel) {
            continue;
        }
        let meta = match fs::symlink_metadata(e.path()) {
            Ok(m) => m,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue, // vanished while we looked
            Err(err) => return Err(err.into()),
        };
        let kind = meta.file_type();
        if kind.is_dir() {
            tar.append_dir(&rel, e.path())?;
            walk(tar, root, &rel, stats)?;
        } else if kind.is_file() {
            match tar.append_path_with_name(e.path(), &rel) {
                Ok(()) => {
                    stats.files += 1;
                    stats.bytes += meta.len();
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        // symlinks (Chromium's Singleton*), sockets and pipes stay out
    }
    Ok(())
}

/// The most an archive may unpack to. The hub caps what it stores; this caps what a decompressor may inflate.
pub const MAX_UNPACKED: u64 = 16 << 30;

/// Unpack `archive` into `dest`, which must not exist yet. On any failure `dest` is removed again.
pub fn unpack(archive: &Path, dest: &Path) -> Result<Packed> {
    fs::create_dir(dest)?;
    let result = unpack_into(archive, dest);
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    } else {
        fs::set_permissions(dest, fs::Permissions::from_mode(0o700))?;
    }
    result
}

fn unpack_into(archive: &Path, dest: &Path) -> Result<Packed> {
    let mut ar = tar::Archive::new(GzDecoder::new(BufReader::new(File::open(archive)?)));
    ar.set_preserve_permissions(true);
    ar.set_preserve_mtime(true);
    ar.set_unpack_xattrs(false);
    ar.set_overwrite(false);
    let mut stats = Packed::default();
    for entry in ar.entries().map_err(|e| Error::Archive(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Archive(e.to_string()))?;
        let path = entry.path().map_err(|e| Error::Archive(e.to_string()))?.into_owned();
        if path.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(Error::Archive(format!("refusing the path `{}`", path.display())));
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => {}
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                stats.files += 1;
                stats.bytes = stats.bytes.saturating_add(entry.size());
                if stats.bytes > MAX_UNPACKED {
                    return Err(Error::Archive("the archive unpacks to more than the allowed size".into()));
                }
            }
            other => {
                return Err(Error::Archive(format!("refusing the {other:?} entry `{}`", path.display())));
            }
        }
        if !entry.unpack_in(dest).map_err(|e| Error::Archive(format!("`{}`: {e}", path.display())))? {
            return Err(Error::Archive(format!("`{}` points outside the target directory", path.display())));
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::uniq;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let dir = std::env::temp_dir().join(format!("citro-sync-pack-{tag}-{}", uniq()));
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        fn p(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn put(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn caches_and_lock_files_are_left_out_and_identity_is_kept() {
        for (rel, out) in [
            ("Default/Network/Cookies", false),
            ("Default/Local Storage/leveldb/000003.log", false),
            ("Default/IndexedDB/https_x_0.indexeddb.leveldb/CURRENT", false),
            ("Default/Extensions/abcdefghijklmnopabcdefghijklmnop/1.0/Cache/keep.js", false),
            ("Default/Preferences", false),
            ("Local State", false),
            ("Default/Cache/Cache_Data/f_000001", true),
            ("Default/Code Cache/js/x", true),
            ("Default/GPUCache/data_0", true),
            ("Default/Service Worker/CacheStorage/a/b", true),
            ("Default/Service Worker/Database/000003.log", false),
            ("GrShaderCache/index", true),
            ("Crashpad/pending/x.dmp", true),
            ("component_crx_cache/x.crx", true),
            ("SingletonLock", true),
            ("SingletonCookie", true),
            ("DevToolsActivePort", true),
            ("Default/lockfile", true),
        ] {
            assert_eq!(skipped(Path::new(rel)), out, "{rel}");
        }
    }

    #[test]
    fn a_profile_survives_the_round_trip_and_the_cache_does_not() {
        let t = Tmp::new("rt");
        let data = t.p("data");
        put(&data, "Default/Network/Cookies", b"cookie-bytes\0\x01\x02");
        put(&data, "Default/Preferences", br#"{"a":1}"#);
        put(&data, "Default/Cache/Cache_Data/f_1", &[7u8; 4096]);
        put(&data, "Local State", b"{}");
        symlink("/proc/1/ns/net", data.join("SingletonSocket")).unwrap();
        symlink("nowhere", data.join("Default/dangling")).unwrap();

        let archive = t.p("p.tar.gz");
        let st = pack(&data, &archive).unwrap();
        assert_eq!(st.files, 3, "cookies, preferences and local state; no cache, no symlinks");
        assert!(st.archive > 0);
        assert_eq!(fs::metadata(&archive).unwrap().permissions().mode() & 0o777, 0o600);

        let out = t.p("out");
        let un = unpack(&archive, &out).unwrap();
        assert_eq!((un.files, un.bytes), (st.files, st.bytes));
        assert_eq!(fs::read(out.join("Default/Network/Cookies")).unwrap(), b"cookie-bytes\0\x01\x02");
        assert_eq!(fs::read(out.join("Default/Preferences")).unwrap(), br#"{"a":1}"#);
        assert!(!out.join("Default/Cache").exists());
        assert!(!out.join("SingletonSocket").exists());
        assert!(fs::symlink_metadata(out.join("Default/dangling")).is_err());
        assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn identical_data_gives_an_identical_listing_and_a_missing_directory_an_empty_archive() {
        let t = Tmp::new("empty");
        let archive = t.p("e.tar.gz");
        let st = pack(&t.p("never-ran"), &archive).unwrap();
        assert_eq!((st.files, st.bytes), (0, 0));
        let out = t.p("out");
        let un = unpack(&archive, &out).unwrap();
        assert_eq!(un.files, 0);
        assert!(out.is_dir());
    }

    /// A tar whose only entry has the given (hostile) name, written byte by byte because the builder refuses to.
    fn hostile(dest: &Path, name: &str, kind: u8) {
        let mut h = tar::Header::new_gnu();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_size(1);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::new(kind));
        h.set_cksum();
        let gz = GzEncoder::new(File::create(dest).unwrap(), Compression::fast());
        let mut b = tar::Builder::new(gz);
        b.get_mut().write_all(h.as_bytes()).unwrap();
        b.get_mut().write_all(&[b'x'; 512]).unwrap();
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn a_hostile_archive_writes_nothing_outside_and_leaves_no_target_behind() {
        let t = Tmp::new("evil");
        for (name, kind) in [("../escape", b'0'), ("/etc/escape", b'0'), ("ok/../../escape", b'0'), ("link", b'2')] {
            let archive = t.p("evil.tar.gz");
            hostile(&archive, name, kind);
            let out = t.p("out");
            let err = unpack(&archive, &out).unwrap_err();
            assert!(matches!(err, Error::Archive(_)), "{name}: {err}");
            assert!(!out.exists(), "{name}: the target is removed after a failure");
            assert!(!t.0.join("escape").exists() && !t.0.parent().unwrap().join("escape").exists());
            let _ = fs::remove_file(&archive);
        }
    }

    #[test]
    fn a_target_that_exists_is_refused_so_nothing_is_merged_by_accident() {
        let t = Tmp::new("exists");
        let archive = t.p("a.tar.gz");
        pack(&t.p("none"), &archive).unwrap();
        fs::create_dir(t.p("out")).unwrap();
        fs::write(t.p("out/mine"), b"keep").unwrap();
        assert!(unpack(&archive, &t.p("out")).is_err());
        assert_eq!(fs::read(t.p("out/mine")).unwrap(), b"keep", "an existing directory is left alone");
    }
}
