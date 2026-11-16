//! The REAL signed checksum list of Clearcote release 151.0.7922.108, checked against the PINNED key.

use citro_install::{Error, Platform, pinned_key_fingerprint, verify_checksums};

const SUMS: &[u8] = include_bytes!("fixtures/SHA256SUMS.txt");
const SIG: &[u8] = include_bytes!("fixtures/SHA256SUMS.txt.asc");
const RELEASE_KEY: &[u8] = include_bytes!("fixtures/clearcote-signing-key.asc");

const LINUX_ARCHIVE: &str = "clearcote-151.0.7922.108-linux-x64.tar.xz";
const LINUX_SHA256: &str = "6c06f95783410d09799828b93f7bb26ac8174354b87f580827c8bcf1f2226b7e";

#[test]
fn print_fingerprint() {
    println!("FINGERPRINT {}", pinned_key_fingerprint().unwrap());
    println!("{}", String::from_utf8_lossy(SUMS));
    let l = verify_checksums(SUMS, SIG).unwrap();
    println!("{:?}", l.entries());
    let _ = (RELEASE_KEY, LINUX_ARCHIVE, LINUX_SHA256, Platform::LinuxX64);
    let _: Option<Error> = None;
}
