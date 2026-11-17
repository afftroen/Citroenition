//! Safe unpacking of `.tar.xz` and `.zip` kernel archives.
//!
//! By the time an archive gets here its SHA-256 has matched the signed list, so this is the second line of defence:
//! it never writes outside `dest`, never creates anything but regular files, directories and contained symlinks,
//! and refuses archive bombs.
//!
//! Rules (every violation is an error, nothing is skipped silently):
//! * names must be relative and made of plain components: no `..`, no absolute paths, no NUL, valid UTF-8;
//! * files are created with `O_EXCL`, so nothing is overwritten and a pre-existing link is never followed;
//! * an entry may not live below a symlink or a file created earlier by the same archive, and no path appears twice
//!   (except directories), so a `link -> ../..` followed by `link/evil` cannot work;
//! * a symlink's target must be relative and inside the tree (leading `..` components only, never more than the
//!   link's own depth); a hard link must name a regular file extracted earlier;
//! * device nodes, fifos, sparse files and unknown entry types are refused;
//! * modes are normalised to `0755` (some exec bit set) or `0644`: no setuid/setgid/sticky, no odd permissions;
//! * at most [`MAX_ENTRIES`] entries and [`MAX_TOTAL_BYTES`] of file data, and the decompressed tar stream itself
//!   is capped too (headers, padding and long-name records included).

use crate::error::{Error, IoContext, Result};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cap on the total size of the files in an archive.
pub(crate) const MAX_TOTAL_BYTES: u64 = 4 << 30;
/// Cap on the number of entries (files, directories, links) in an archive.
pub(crate) const MAX_ENTRIES: usize = 200_000;
/// What the xz decoder may allocate (a hostile header can ask for a multi-GiB dictionary), in KiB.
const XZ_MEM_LIMIT_KIB: u32 = 2 << 20;
/// Longest accepted entry name or link target.
const MAX_NAME_BYTES: usize = 4096;
const IO_CHUNK: usize = 256 * 1024;

/// Caps on what an archive may contain.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_total_bytes: u64,
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_total_bytes: MAX_TOTAL_BYTES, max_entries: MAX_ENTRIES }
    }
}

impl Limits {
    /// Bound on the decompressed tar stream: the payload plus a header, padding and long-name/PAX records per entry.
    fn max_stream_bytes(&self) -> u64 {
        self.max_total_bytes.saturating_add((self.max_entries as u64).saturating_mul(4096))
    }
}

/// What was unpacked.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Extracted {
    /// Entries seen (including directories and links).
    pub entries: usize,
    /// File data written, in bytes.
    pub bytes: u64,
}

/// How entry names are split into components, and whether symbolic links may be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Naming {
    /// Only `/` separates components (tar on Unix, and the zip of a macOS app bundle).
    Posix,
    /// `\` separates too and `:` is refused; no links (a Windows zip, and anything unpacked on Windows).
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Symlink,
}

fn bad_name(name: &[u8], why: &str) -> Error {
    Error::unsafe_archive(format!("entry name `{}`: {why}", String::from_utf8_lossy(name).escape_debug()))
}

fn utf8<'a>(bytes: &'a [u8], what: &str) -> Result<&'a str> {
    if bytes.len() > MAX_NAME_BYTES {
        return Err(bad_name(bytes, &format!("{what} is longer than {MAX_NAME_BYTES} bytes")));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| bad_name(bytes, &format!("{what} is not valid UTF-8")))?;
    if text.contains('\0') {
        return Err(bad_name(bytes, &format!("{what} contains a NUL byte")));
    }
    Ok(text)
}

/// An archive member name as a relative path of plain components. The empty path is the archive root (`.`, `./`).
fn sanitize(name: &[u8], naming: Naming) -> Result<PathBuf> {
    let text = utf8(name, "name")?;
    let text = if naming == Naming::Windows { text.replace('\\', "/") } else { text.to_owned() };
    if text.starts_with('/') {
        return Err(bad_name(name, "absolute path"));
    }
    let mut out = PathBuf::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(bad_name(name, "contains a `..` component")),
            p if naming == Naming::Windows && p.contains(':') => {
                return Err(bad_name(name, "contains `:` (drive letter or stream name)"));
            }
            p => out.push(p),
        }
    }
    Ok(out)
}

/// Check a symlink target: relative, and resolving inside the tree. Only leading `..` components are accepted (at
/// most as many as the link has parent directories), so the target cannot be steered out by other links.
fn check_symlink_target(link: &Path, target: &[u8]) -> Result<()> {
    let text = utf8(target, "link target")?;
    let reject = |why: &str| {
        Error::unsafe_archive(format!(
            "symlink `{}` -> `{}`: {why}",
            link.display(),
            String::from_utf8_lossy(target).escape_debug()
        ))
    };
    if text.starts_with('/') {
        return Err(reject("absolute target"));
    }
    let depth = link.components().count().saturating_sub(1);
    let (mut ups, mut named) = (0usize, false);
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." if named => return Err(reject("`..` after a name")),
            ".." => {
                ups += 1;
                if ups > depth {
                    return Err(reject("points outside the install directory"));
                }
            }
            _ => named = true,
        }
    }
    if !named && ups == 0 {
        return Err(reject("empty target"));
    }
    Ok(())
}

/// Counts the decompressed bytes handed to the tar reader and fails past the cap.
struct CapReader<R> {
    inner: R,
    left: u64,
    tripped: Arc<AtomicBool>,
}

impl<R: Read> Read for CapReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.left {
            self.tripped.store(true, Ordering::Relaxed);
            return Err(io::Error::other("decompressed data exceeds the size limit"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

struct Unpacker<'a> {
    dest: &'a Path,
    limits: Limits,
    naming: Naming,
    seen: HashMap<PathBuf, Kind>,
    stats: Extracted,
    /// Set by [`CapReader`] when the stream cap is hit, to tell that apart from a corrupt archive.
    tripped: Arc<AtomicBool>,
    buf: Vec<u8>,
}

impl<'a> Unpacker<'a> {
    fn new(dest: &'a Path, limits: Limits, naming: Naming, tripped: Arc<AtomicBool>) -> Self {
        Self { dest, limits, naming, seen: HashMap::new(), stats: Extracted::default(), tripped, buf: Vec::new() }
    }

    /// A read failure while parsing/decompressing: a cap hit or a corrupt archive.
    fn read_error(&self, e: &io::Error) -> Error {
        if self.tripped.load(Ordering::Relaxed) {
            Error::ArchiveLimit(format!(
                "the decompressed archive is larger than {} bytes",
                self.limits.max_stream_bytes()
            ))
        } else {
            Error::BadArchive(e.to_string())
        }
    }

    /// Account for one more entry of `size` bytes.
    fn account(&mut self, size: u64) -> Result<()> {
        self.stats.entries += 1;
        if self.stats.entries > self.limits.max_entries {
            return Err(Error::ArchiveLimit(format!("more than {} entries", self.limits.max_entries)));
        }
        self.stats.bytes = self.stats.bytes.saturating_add(size);
        if self.stats.bytes > self.limits.max_total_bytes {
            return Err(Error::ArchiveLimit(format!("more than {} bytes of file data", self.limits.max_total_bytes)));
        }
        Ok(())
    }

    fn sanitize(&self, name: &[u8]) -> Result<PathBuf> {
        sanitize(name, self.naming)
    }

    /// Vet `rel` before creating anything at that path: no ancestor may be a file or symlink, no duplicates; the
    /// missing parent directories are created (and remembered). Returns `false` for a directory seen twice.
    fn claim(&mut self, rel: &Path, kind: Kind) -> Result<bool> {
        let shown = || rel.display().to_string();
        match self.seen.get(rel) {
            None => {}
            Some(Kind::Dir) if kind == Kind::Dir => return Ok(false),
            Some(_) => return Err(Error::unsafe_archive(format!("`{}` appears twice in the archive", shown()))),
        }
        let mut missing: Vec<&Path> = Vec::new();
        for anc in rel.ancestors().skip(1).take_while(|a| !a.as_os_str().is_empty()) {
            match self.seen.get(anc) {
                Some(Kind::Dir) => break,
                Some(_) => {
                    return Err(Error::unsafe_archive(format!(
                        "`{}` would be created below `{}`, which is a file or a symlink",
                        shown(),
                        anc.display()
                    )));
                }
                None => missing.push(anc),
            }
        }
        for anc in missing.into_iter().rev() {
            self.mkdir(anc)?;
            self.seen.insert(anc.to_path_buf(), Kind::Dir);
        }
        Ok(true)
    }

    fn mkdir(&self, rel: &Path) -> Result<()> {
        let path = self.dest.join(rel);
        match fs::create_dir(&path) {
            Ok(()) => Ok(()),
            // Only a real directory may already be there (never a symlink to one).
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) =>
            {
                Ok(())
            }
            Err(e) => Err(Error::io(format!("creating directory {}", path.display()), e)),
        }
    }

    fn make_dir(&mut self, rel: &Path) -> Result<()> {
        if self.claim(rel, Kind::Dir)? {
            self.mkdir(rel)?;
            self.seen.insert(rel.to_path_buf(), Kind::Dir);
        }
        Ok(())
    }

    /// Write exactly `size` bytes from `reader` into a new file at `rel`.
    fn make_file(&mut self, rel: &Path, exec: bool, size: u64, reader: &mut dyn Read) -> Result<()> {
        self.claim(rel, Kind::File)?;
        let path = self.dest.join(rel);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(if exec { 0o755 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = exec;
        let mut file = opts.open(&path).context(|| format!("creating {}", path.display()))?;
        self.seen.insert(rel.to_path_buf(), Kind::File);
        if self.buf.is_empty() {
            self.buf = vec![0; IO_CHUNK];
        }
        let mut left = size;
        while left > 0 {
            let want = usize::try_from(left).map_or(IO_CHUNK, |l| l.min(IO_CHUNK));
            let n = match reader.read(&mut self.buf[..want]) {
                Ok(0) => return Err(Error::BadArchive(format!("`{}` is truncated", rel.display()))),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(self.read_error(&e)),
            };
            file.write_all(&self.buf[..n]).context(|| format!("writing {}", path.display()))?;
            left -= n as u64;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn make_symlink(&mut self, rel: &Path, target: &[u8]) -> Result<()> {
        if self.naming == Naming::Windows {
            return Err(Error::unsafe_archive(format!(
                "`{}` is a symlink; not supported in this kind of archive",
                rel.display()
            )));
        }
        check_symlink_target(rel, target)?;
        self.claim(rel, Kind::Symlink)?;
        let path = self.dest.join(rel);
        let target = utf8(target, "link target")?;
        std::os::unix::fs::symlink(target, &path).context(|| format!("creating symlink {}", path.display()))?;
        self.seen.insert(rel.to_path_buf(), Kind::Symlink);
        Ok(())
    }

    #[cfg(not(unix))]
    fn make_symlink(&mut self, rel: &Path, _target: &[u8]) -> Result<()> {
        Err(Error::unsafe_archive(format!("`{}` is a symlink; not supported on this platform", rel.display())))
    }

    /// A hard link may only name a regular file that this archive extracted earlier.
    fn make_hardlink(&mut self, rel: &Path, target: &[u8]) -> Result<()> {
        let src = self.sanitize(target)?;
        if self.seen.get(&src) != Some(&Kind::File) {
            return Err(Error::unsafe_archive(format!(
                "hard link `{}` -> `{}`: the target is not a regular file extracted earlier",
                rel.display(),
                src.display()
            )));
        }
        self.claim(rel, Kind::File)?;
        let to = self.dest.join(rel);
        fs::hard_link(self.dest.join(&src), &to).context(|| format!("creating hard link {}", to.display()))?;
        self.seen.insert(rel.to_path_buf(), Kind::File);
        Ok(())
    }
}

/// Unpack a (possibly xz-compressed) tar stream into the existing, empty directory `dest`.
pub(crate) fn extract_tar<R: Read>(reader: R, dest: &Path, limits: Limits) -> Result<Extracted> {
    use tar::EntryType;
    let tripped = Arc::new(AtomicBool::new(false));
    let naming = if cfg!(windows) { Naming::Windows } else { Naming::Posix };
    let mut up = Unpacker::new(dest, limits, naming, Arc::clone(&tripped));
    let capped = CapReader { inner: reader, left: limits.max_stream_bytes(), tripped };
    let mut archive = tar::Archive::new(capped);
    for entry in archive.entries().map_err(|e| up.read_error(&e))? {
        let mut entry = entry.map_err(|e| up.read_error(&e))?;
        let kind = entry.header().entry_type();
        let size = entry.size();
        up.account(size)?;
        let name = entry.path_bytes().into_owned();
        match kind {
            // Global PAX attributes describe the archive, not a file.
            EntryType::XGlobalHeader => {}
            EntryType::Regular | EntryType::Continuous => {
                let rel = up.sanitize(&name)?;
                if rel.as_os_str().is_empty() {
                    return Err(bad_name(&name, "a file cannot be the archive root"));
                }
                let mode = entry.header().mode().map_err(|e| up.read_error(&e))?;
                up.make_file(&rel, mode & 0o111 != 0, size, &mut entry)?;
            }
            EntryType::Directory => {
                let rel = up.sanitize(&name)?;
                if !rel.as_os_str().is_empty() {
                    up.make_dir(&rel)?;
                }
            }
            EntryType::Symlink | EntryType::Link => {
                let rel = up.sanitize(&name)?;
                if rel.as_os_str().is_empty() {
                    return Err(bad_name(&name, "a link cannot be the archive root"));
                }
                let target = entry
                    .link_name_bytes()
                    .ok_or_else(|| Error::BadArchive(format!("link `{}` has no target", rel.display())))?
                    .into_owned();
                if kind == EntryType::Symlink {
                    up.make_symlink(&rel, &target)?;
                } else {
                    up.make_hardlink(&rel, &target)?;
                }
            }
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                return Err(bad_name(&name, "device nodes and fifos are not allowed"));
            }
            EntryType::GNUSparse => return Err(bad_name(&name, "sparse files are not allowed")),
            other => return Err(bad_name(&name, &format!("unsupported entry type {:?}", other.as_byte() as char))),
        }
    }
    // Read to the very end so the xz stream's own integrity checks (block CRCs, index, footer) run.
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink()).map_err(|e| up.read_error(&e))?;
    Ok(up.stats)
}

/// Unpack the `.tar.xz` file `archive` into the existing, empty directory `dest`.
pub(crate) fn extract_tar_xz(archive: &Path, dest: &Path, limits: Limits) -> Result<Extracted> {
    let file = File::open(archive).context(|| format!("opening {}", archive.display()))?;
    let xz = lzma_rust2::XzReader::new_mem_limit(BufReader::with_capacity(IO_CHUNK, file), false, XZ_MEM_LIMIT_KIB);
    extract_tar(xz, dest, limits)
}

/// Unpack the `.zip` file `archive` into the existing, empty directory `dest`.
///
/// `naming` says what made the archive: [`Naming::Windows`] (no symbolic links, `\` also separates) or
/// [`Naming::Posix`] (a macOS app bundle, whose frameworks are built from relative symlinks such as
/// `Versions/Current -> 150.0.7871.114`; links are held to the rules in the module docs).
pub(crate) fn extract_zip(archive: &Path, dest: &Path, limits: Limits, naming: Naming) -> Result<Extracted> {
    let file = File::open(archive).context(|| format!("opening {}", archive.display()))?;
    let corrupt = |e: zip::result::ZipError| Error::BadArchive(e.to_string());
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(corrupt)?;
    if zip.len() > limits.max_entries {
        return Err(Error::ArchiveLimit(format!("more than {} entries", limits.max_entries)));
    }
    let mut up = Unpacker::new(dest, limits, naming, Arc::new(AtomicBool::new(false)));
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(corrupt)?;
        let size = entry.size();
        up.account(size)?;
        let name = entry.name_raw().to_vec();
        let mode = entry.unix_mode();
        if entry.is_symlink() {
            if naming == Naming::Windows {
                return Err(bad_name(&name, "symbolic links are not allowed in zip archives"));
            }
            let rel = up.sanitize(&name)?;
            if rel.as_os_str().is_empty() {
                return Err(bad_name(&name, "a link cannot be the archive root"));
            }
            // the member's data is the link target; one byte more than the longest accepted name is enough to refuse it
            let mut target = Vec::new();
            (&mut entry)
                .take(MAX_NAME_BYTES as u64 + 1)
                .read_to_end(&mut target)
                .map_err(|e| Error::BadArchive(format!("link `{}`: {e}", rel.display())))?;
            up.make_symlink(&rel, &target)?;
            continue;
        }
        // 0o100000 regular file, 0o040000 directory, 0 = no type recorded (archives made on Windows).
        if let Some(m) = mode
            && !matches!(m & 0o170000, 0 | 0o100000 | 0o040000)
        {
            return Err(bad_name(&name, "special files are not allowed"));
        }
        let rel = up.sanitize(&name)?;
        if entry.is_dir() {
            if !rel.as_os_str().is_empty() {
                up.make_dir(&rel)?;
            }
        } else if rel.as_os_str().is_empty() {
            return Err(bad_name(&name, "a file cannot be the archive root"));
        } else {
            up.make_file(&rel, mode.is_some_and(|m| m & 0o111 != 0), size, &mut entry)?;
        }
    }
    Ok(up.stats)
}

/// The unpacked tree must hold the browser as a regular, executable file at its top level.
pub(crate) fn require_binary(dir: &Path, name: &str) -> Result<()> {
    let meta = fs::symlink_metadata(dir.join(name)).map_err(|_| Error::MissingBinary(name.to_owned()))?;
    if !meta.is_file() {
        return Err(Error::MissingBinary(name.to_owned()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(Error::BadArchive(format!("`{name}` is not executable")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{ZipEntry, zip};

    /// Unpack `entries` as a zip into a fresh directory; returns the directory (kept alive by the guard) and the result.
    fn unzip(entries: &[ZipEntry], naming: Naming) -> (tempfile::TempDir, Result<Extracted>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(&archive, zip(entries)).unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract_zip(&archive, &out, Limits::default(), naming);
        (dir, r)
    }

    const FW: &str = "Chromium.app/Contents/Frameworks/Chromium Framework.framework";

    /// The shape of Clearcote's macOS archive: an app bundle whose framework is held together by relative symlinks;
    /// only some directories have an entry of their own.
    fn bundle() -> Vec<ZipEntry> {
        vec![
            ZipEntry::dir("Chromium.app/"),
            ZipEntry::dir("Chromium.app/Contents/"),
            ZipEntry::file("Chromium.app/Contents/Info.plist", b"<plist/>"),
            ZipEntry::exec("Chromium.app/Contents/MacOS/Chromium", b"\xcf\xfa\xed\xfe launcher"),
            ZipEntry::exec(&format!("{FW}/Versions/150.0.1.2/Chromium Framework"), b"engine"),
            ZipEntry::file(&format!("{FW}/Versions/150.0.1.2/Resources/en.lproj/x.pak"), b"pak"),
            ZipEntry::symlink(&format!("{FW}/Versions/Current"), "150.0.1.2"),
            ZipEntry::symlink(&format!("{FW}/Chromium Framework"), "Versions/Current/Chromium Framework"),
            ZipEntry::symlink(&format!("{FW}/Resources"), "Versions/Current/Resources"),
        ]
    }

    #[test]
    fn a_macos_app_bundle_unpacks_with_its_links_and_exec_bits() {
        let (dir, r) = unzip(&bundle(), Naming::Posix);
        let stats = r.unwrap();
        assert_eq!(stats.entries, bundle().len());
        let out = dir.path().join("out");
        let fw = out.join(FW);

        // the links are links, with the targets the archive named, and they resolve
        assert_eq!(fs::read_link(fw.join("Versions/Current")).unwrap(), Path::new("150.0.1.2"));
        assert_eq!(
            fs::read_link(fw.join("Chromium Framework")).unwrap(),
            Path::new("Versions/Current/Chromium Framework")
        );
        assert_eq!(fs::read(fw.join("Chromium Framework")).unwrap(), b"engine");
        assert_eq!(fs::read(fw.join("Resources/en.lproj/x.pak")).unwrap(), b"pak");

        // the launcher is a regular executable file; the plist is not executable
        require_binary(&out, "Chromium.app/Contents/MacOS/Chromium").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("Chromium.app/Contents/MacOS/Chromium"), 0o755);
            assert_eq!(mode("Chromium.app/Contents/Info.plist"), 0o644);
        }
        // a link is never accepted as the browser, even when it points at one
        let e = require_binary(&out, &format!("{FW}/Chromium Framework")).unwrap_err();
        assert!(matches!(e, Error::MissingBinary(_)), "{e}");
    }

    #[test]
    fn links_in_a_windows_zip_are_still_refused() {
        let (_dir, r) = unzip(&bundle(), Naming::Windows);
        let e = r.unwrap_err();
        assert!(matches!(&e, Error::UnsafeArchive(m) if m.contains("symbolic links")), "{e}");
    }

    #[test]
    fn links_that_could_leave_the_install_are_refused() {
        let bad: [(&str, &str, &str); 6] = [
            ("absolute", "Chromium.app/a", "/etc/passwd"),
            ("one level too far up", "Chromium.app/a", "../../x"),
            ("up after a name", "Chromium.app/Contents/a", "b/../../../x"),
            ("empty", "Chromium.app/a", ""),
            ("a deep link going up past the root", "Chromium.app/Contents/Frameworks/a", "../../../../x"),
            ("up to the root and beyond", "a", ".."),
        ];
        for (why, name, target) in bad {
            let (_dir, r) = unzip(
                &[ZipEntry::file("Chromium.app/Contents/x", b"x"), ZipEntry::symlink(name, target)],
                Naming::Posix,
            );
            let e = r.expect_err(why);
            assert!(matches!(e, Error::UnsafeArchive(_)), "{why}: {e}");
        }
        // an oversized target is refused, not buffered
        let (_dir, r) = unzip(&[ZipEntry::symlink("a", &"x".repeat(100_000))], Naming::Posix);
        assert!(matches!(r.unwrap_err(), Error::UnsafeArchive(_)));
    }

    #[test]
    fn nothing_can_be_written_through_a_link_or_over_one() {
        // `a -> b`, then a file below the link: the classic way out of the tree
        let (dir, r) =
            unzip(&[ZipEntry::dir("b/"), ZipEntry::symlink("a", "b"), ZipEntry::file("a/evil", b"x")], Naming::Posix);
        assert!(matches!(r.unwrap_err(), Error::UnsafeArchive(m) if m.contains("below")));
        assert!(!dir.path().join("out/b/evil").exists());
        // the same path twice, as a link and as a directory (the zip writer refuses two identical names)
        let (_dir, r) = unzip(&[ZipEntry::symlink("a", "b"), ZipEntry::dir("a/")], Naming::Posix);
        assert!(matches!(r.unwrap_err(), Error::UnsafeArchive(m) if m.contains("twice")));
        // a link in place of the archive root
        let (_dir, r) = unzip(&[ZipEntry::symlink("./", "b")], Naming::Posix);
        assert!(matches!(r.unwrap_err(), Error::UnsafeArchive(_)));
    }

    /// Clearcote's real macOS archive (about 230 MB, so not in the repository): download
    /// `clearcote-<version>-macos-arm64.zip` from a release and run
    /// `CITRO_REAL_MACOS_ZIP=<path> cargo test -p citro-install real_clearcote_macos -- --ignored`.
    #[test]
    #[ignore = "needs CITRO_REAL_MACOS_ZIP=<path to the real archive>"]
    fn the_real_clearcote_macos_archive_unpacks() {
        let archive = PathBuf::from(std::env::var_os("CITRO_REAL_MACOS_ZIP").expect("CITRO_REAL_MACOS_ZIP"));
        let out = tempfile::tempdir().unwrap();
        let stats = extract_zip(&archive, out.path(), Limits::default(), Naming::Posix).unwrap();
        println!("{} entries, {} bytes", stats.entries, stats.bytes);
        require_binary(out.path(), "Chromium.app/Contents/MacOS/Chromium").unwrap();
        let fw = out.path().join("Chromium.app/Contents/Frameworks/Chromium Framework.framework");
        let engine = fs::metadata(fw.join("Chromium Framework")).expect("the framework link resolves");
        assert!(engine.len() > 100 << 20, "the engine is {} bytes", engine.len());
        assert!(fs::symlink_metadata(fw.join("Versions/Current")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn names_in_a_unix_zip_may_hold_a_backslash_or_colon() {
        // legal in a macOS file name; only a Windows archive reads `\` as a separator and refuses `:`
        let (dir, r) = unzip(&[ZipEntry::file("Chromium.app/we\\ird:name", b"x")], Naming::Posix);
        r.unwrap();
        assert!(dir.path().join("out/Chromium.app/we\\ird:name").is_file());
        let (_dir, r) = unzip(&[ZipEntry::file("Chromium.app/a:b", b"x")], Naming::Windows);
        assert!(matches!(r.unwrap_err(), Error::UnsafeArchive(_)));
    }
}
