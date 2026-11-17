//! Make the macOS kernel's DCHECKs non-fatal.
//!
//! Clearcote's macOS build is compiled with DCHECKs (Chromium's developer self-checks) on and fatal, and not
//! configurable at run time (`--disable-features=DcheckIsFatal` does nothing in it). A check that trips on a busy page
//! (TikTok Ads Manager, Gmail) kills the tab: "Aw, Snap! Error code: 6". Stable Chrome has no DCHECKs at all.
//!
//! Every DCHECK failure is built by one of three functions, `logging::CheckError::DCheck`, `::DCheckOp` and
//! `::DPCheck`, and each passes the severity to the log message as `mov w3, #3` (FATAL). This module changes that one
//! instruction in each to `mov w3, #2` (ERROR): the failure is then written to the log, Chromium's own non-fatal path
//! runs (a crash key and a dump-without-crashing, which goes nowhere here) and the page lives on. `CHECK`s and
//! `NOTREACHED`s use other functions and stay fatal.
//!
//! The functions are found by name in the framework's symbol table, so the patch follows the kernel version. The app
//! bundle is patched as a copy, re-signed ad hoc and swapped in by renaming, so browsers already running from the old
//! files are not disturbed. A marker file remembers a patched framework; a kernel with another layout is left alone.

use crate::kernel::Kernel;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MARKER: &str = ".citro-dchecks-nonfatal";
const MARKER_VERSION: &str = "v1";
const FUNCTIONS: [&str; 3] = [
    "__ZN7logging10CheckError6DCheckEPKcRKN4base8LocationE",
    "__ZN7logging10CheckError8DCheckOpEPcRKN4base8LocationE",
    "__ZN7logging10CheckError7DPCheckEPKcRKN4base8LocationE",
];
/// `mov w3, #3` and `mov w3, #2` (MOVZ W3), little endian.
const FATAL: u32 = 0x5280_0063;
const ERROR: u32 = 0x5280_0043;
/// How far into each function the severity is looked for.
const WINDOW: u64 = 40 * 4;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not a macOS app bundle (the Linux and Windows kernels are left as they are).
    NotApplicable,
    AlreadyDone,
    Patched(usize),
}

fn err(m: impl Into<String>) -> std::io::Error {
    std::io::Error::other(m.into())
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

/// File offsets of the severity instruction in each DCHECK function, and whether each is already patched.
pub fn find_sites(framework: &Path) -> std::io::Result<Vec<(u64, bool)>> {
    let mut f = File::open(framework)?;
    let mut head = vec![0u8; 64 * 1024];
    let n = f.read(&mut head)?;
    head.truncate(n);
    if head.len() < 32 || u32le(&head, 0) != 0xfeed_facf {
        return Err(err("not a thin 64-bit Mach-O file"));
    }
    let ncmds = u32le(&head, 16) as usize;
    let (mut text, mut symtab) = (None, None);
    let mut at = 32usize;
    for _ in 0..ncmds {
        if at + 8 > head.len() {
            return Err(err("load commands are cut off"));
        }
        let (cmd, size) = (u32le(&head, at), u32le(&head, at + 4) as usize);
        match cmd {
            0x19 if &head[at + 8..at + 14] == b"__TEXT" => {
                text = Some((u64le(&head, at + 24), u64le(&head, at + 40))); // vmaddr, fileoff
            }
            0x2 => {
                let r = |o| u32le(&head, at + o) as u64;
                symtab = Some((r(8), r(12), r(16), r(20))); // symoff, nsyms, stroff, strsize
            }
            _ => {}
        }
        at += size.max(8);
    }
    let (vmaddr, fileoff) = text.ok_or_else(|| err("no __TEXT segment"))?;
    let (symoff, nsyms, stroff, strsize) = symtab.ok_or_else(|| err("no symbol table"))?;
    let mut strings = vec![0u8; strsize as usize];
    f.seek(SeekFrom::Start(stroff))?;
    f.read_exact(&mut strings)?;
    // the string-table offset of each name ("\0name\0")
    let mut wanted: Vec<(usize, u32)> = Vec::new();
    for (i, name) in FUNCTIONS.iter().enumerate() {
        let needle = [&[0u8][..], name.as_bytes(), &[0u8][..]].concat();
        if let Some(p) = memchr::memmem::find(&strings, &needle) {
            wanted.push((i, (p + 1) as u32));
        }
    }
    drop(strings);
    let mut syms = vec![0u8; (nsyms * 16) as usize];
    f.seek(SeekFrom::Start(symoff))?;
    f.read_exact(&mut syms)?;
    let mut sites = Vec::new();
    for (_, strx) in &wanted {
        let Some(value) = syms
            .as_chunks::<16>()
            .0
            .iter()
            .find(|e| u32le(&e[..], 0) == *strx && e[4] & 0x0e == 0x0e) // N_SECT: defined in a section
            .map(|e| u64le(&e[..], 8))
        else {
            continue;
        };
        let start = value.checked_sub(vmaddr).ok_or_else(|| err("a function lies outside __TEXT"))? + fileoff;
        let mut code = vec![0u8; WINDOW as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut code)?;
        let words: Vec<u32> = code.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        if let Some(k) = words.iter().position(|w| *w == FATAL) {
            sites.push((start + 4 * k as u64, false));
        } else if let Some(k) = words.iter().position(|w| *w == ERROR) {
            sites.push((start + 4 * k as u64, true));
        }
    }
    // DCheck and DCheckOp carry nearly every DCHECK; without them this is not the layout this patch knows
    if sites.len() < 2 {
        return Err(err("the DCHECK functions were not found"));
    }
    Ok(sites)
}

fn framework_of(app: &Path) -> Option<PathBuf> {
    let name = app.file_stem()?.to_str()?;
    let f =
        app.join("Contents/Frameworks").join(format!("{name} Framework.framework")).join(format!("{name} Framework"));
    f.is_file().then_some(f)
}

fn stamp(path: &Path) -> String {
    let m = fs::metadata(path).ok();
    let mtime = m.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    format!("{MARKER_VERSION} {} {}", m.map_or(0, |m| m.len()), mtime.map_or(0, |d| d.as_secs()))
}

fn codesign(args: &[&str], app: &Path) -> std::io::Result<()> {
    let out = std::process::Command::new("/usr/bin/codesign").args(args).arg(app).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(err(format!("codesign {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// Patch the kernel if it needs it. Cheap when it is done already (one small file is read).
pub fn ensure(kernel: &Kernel) -> std::io::Result<Outcome> {
    if !cfg!(target_os = "macos") {
        return Ok(Outcome::NotApplicable);
    }
    let Some(app) =
        kernel.binary.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
    else {
        return Ok(Outcome::NotApplicable);
    };
    let Some(framework) = framework_of(&app) else { return Ok(Outcome::NotApplicable) };
    let parent = app.parent().ok_or_else(|| err("the app has no folder"))?.to_path_buf();
    let marker = parent.join(MARKER);
    if fs::read_to_string(&marker).is_ok_and(|m| m.trim() == stamp(&framework)) {
        return Ok(Outcome::AlreadyDone);
    }
    // one patcher at a time (the app and the command line may both start a browser)
    let lock = OpenOptions::new().create(true).write(true).truncate(false).open(parent.join(".citro-patch.lock"))?;
    // SAFETY: flock on a file descriptor we own; released when `lock` is dropped.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if fs::read_to_string(&marker).is_ok_and(|m| m.trim() == stamp(&framework)) {
        return Ok(Outcome::AlreadyDone);
    }
    let sites = find_sites(&framework)?;
    if sites.iter().all(|(_, done)| *done) {
        fs::write(&marker, stamp(&framework))?;
        return Ok(Outcome::AlreadyDone);
    }
    // the copy keeps its name (codesign only treats a folder ending in .app as an app) in a folder of its own
    let name = app.file_name().ok_or_else(|| err("the app has no name"))?.to_owned();
    let (work_dir, old_dir) = (parent.join(".citro-patching"), parent.join(".citro-replaced"));
    let _ = fs::remove_dir_all(&work_dir);
    let _ = fs::remove_dir_all(&old_dir);
    fs::create_dir_all(&work_dir)?;
    fs::create_dir_all(&old_dir)?;
    let (work, old) = (work_dir.join(&name), old_dir.join(&name));
    // a clone on APFS: instant and takes no space until the patched file is written
    let cp = std::process::Command::new("/bin/cp").arg("-cR").arg(&app).arg(&work).status()?;
    if !cp.success() {
        let _ = fs::remove_dir_all(&work_dir);
        return Err(err("could not copy the browser to patch it"));
    }
    let result = (|| -> std::io::Result<usize> {
        // the framework path is a symlink (`Chromium Framework` -> `Versions/Current/...`): patch the real file
        let real = fs::canonicalize(&framework)?;
        let rel = real
            .strip_prefix(fs::canonicalize(&app)?)
            .map_err(|_| err("the framework is outside the app"))?
            .to_path_buf();
        let fw = work.join(&rel);
        if !fw.is_file() {
            return Err(err(format!("the copied framework is missing ({})", rel.display())));
        }
        // the clone shares blocks with the original: write a whole new file instead of patching in place
        let tmp = fw.with_file_name(".citro-patching");
        fs::copy(&fw, &tmp)?;
        {
            let mut f = OpenOptions::new().write(true).open(&tmp)?;
            for (off, done) in &sites {
                if !done {
                    f.seek(SeekFrom::Start(*off))?;
                    f.write_all(&ERROR.to_le_bytes())?;
                }
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, &fw)?;
        // Clearcote signs each program on its own (ad hoc), not the bundle: re-sign the one file that changed
        codesign(&["--force", "--sign", "-"], &fw)?;
        codesign(&["--verify"], &fw)?;
        Ok(sites.iter().filter(|(_, d)| !d).count())
    })();
    let n = match result {
        Ok(n) => n,
        Err(e) => {
            let _ = fs::remove_dir_all(&work_dir);
            return Err(e);
        }
    };
    // swap: running browsers keep the files they have open
    fs::rename(&app, &old)?;
    if let Err(e) = fs::rename(&work, &app) {
        let _ = fs::rename(&old, &app);
        return Err(e);
    }
    let _ = fs::remove_dir_all(&old_dir);
    let _ = fs::remove_dir_all(&work_dir);
    fs::write(&marker, stamp(&framework))?;
    Ok(Outcome::Patched(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal thin Mach-O: header, __TEXT, a symbol table with the three DCHECK functions.
    fn fake_framework(dir: &Path, severity: u32) -> PathBuf {
        let mut b = vec![0u8; 0x4000];
        let w32 = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let w64 = |b: &mut Vec<u8>, at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        w32(&mut b, 0, 0xfeed_facf);
        w32(&mut b, 16, 2); // ncmds
        // LC_SEGMENT_64 __TEXT at 32 (size 72): vmaddr 0x1000, fileoff 0
        w32(&mut b, 32, 0x19);
        w32(&mut b, 36, 72);
        b[40..46].copy_from_slice(b"__TEXT");
        w64(&mut b, 56, 0x1000);
        w64(&mut b, 72, 0);
        // LC_SYMTAB at 104
        let (symoff, stroff) = (0x2000u32, 0x3000u32);
        let mut strings = vec![0u8];
        let mut strx = Vec::new();
        for f in FUNCTIONS {
            strx.push(strings.len() as u32);
            strings.extend_from_slice(f.as_bytes());
            strings.push(0);
        }
        w32(&mut b, 104, 0x2);
        w32(&mut b, 108, 24);
        w32(&mut b, 112, symoff);
        w32(&mut b, 116, FUNCTIONS.len() as u32);
        w32(&mut b, 120, stroff);
        w32(&mut b, 124, strings.len() as u32);
        b[stroff as usize..stroff as usize + strings.len()].copy_from_slice(&strings);
        for (i, x) in strx.iter().enumerate() {
            let e = symoff as usize + 16 * i;
            let func = 0x400 + 0x100 * i as u64; // file offset; vm address = + 0x1000
            w32(&mut b, e, *x);
            b[e + 4] = 0x0e; // N_SECT
            w64(&mut b, e + 8, 0x1000 + func);
            w32(&mut b, func as usize, 0xd503_201f); // nop
            w32(&mut b, func as usize + 8, severity);
        }
        let path = dir.join("Chromium Framework");
        fs::write(&path, b).unwrap();
        path
    }

    #[test]
    fn the_severity_instruction_is_found_by_symbol() {
        let dir = std::env::temp_dir().join(format!("citro-patch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = fake_framework(&dir, FATAL);
        let sites = find_sites(&f).unwrap();
        assert_eq!(sites, vec![(0x408, false), (0x508, false), (0x608, false)]);
        let done = fake_framework(&dir, ERROR);
        assert!(find_sites(&done).unwrap().iter().all(|(_, d)| *d));
        let none = fake_framework(&dir, 0xd503_201f);
        assert!(find_sites(&none).is_err(), "an unknown layout is refused, not guessed at");
        fs::write(&f, b"not mach-o").unwrap();
        assert!(find_sites(&f).is_err());
    }
}
