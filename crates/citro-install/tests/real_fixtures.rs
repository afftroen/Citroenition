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

// ---- release v0.1.0-pre.24: the only Clearcote release that carries a macOS kernel ----------------------------------

const MAC_SUMS: &[u8] = include_bytes!("fixtures/macos/SHA256SUMS.txt");
const MAC_SIG: &[u8] = include_bytes!("fixtures/macos/SHA256SUMS.txt.asc");

const MAC_ARCHIVE: &str = "clearcote-150.0.7871.114-macos-arm64.zip";
const MAC_SHA256: &str = "04493686efd16015c383cba7b1e8625e2297c1861944e050d9a2435cd26bb75c";

#[test]
fn the_real_signed_list_of_the_release_with_a_macos_kernel_verifies_and_names_it() {
    let list = verify_checksums(MAC_SUMS, MAC_SIG).expect("the pinned key signed it");
    assert_eq!(list.signed_by(), pinned_key_fingerprint().unwrap());

    let mac = list.find_archive(Platform::MacosArm64, None).unwrap();
    assert_eq!(
        (mac.name.as_str(), mac.version.as_str(), mac.sha256.as_str()),
        (MAC_ARCHIVE, "150.0.7871.114", MAC_SHA256)
    );
    assert_eq!(list.sha256_of(MAC_ARCHIVE), Some(MAC_SHA256));

    // the list also holds raw binaries, one of them with a space in its name; none of that is an archive
    assert!(list.entries().iter().any(|e| e.name == "Chromium Framework"));
    // and the same list still serves the other platforms of that release
    for (platform, name) in [
        (Platform::LinuxX64, "clearcote-150.0.7871.114-linux-x64.tar.xz"),
        (Platform::WindowsX64, "clearcote-150.0.7871.114-windows-x64.zip"),
    ] {
        assert_eq!(list.find_archive(platform, None).unwrap().name, name);
    }
}

#[test]
fn the_macos_archive_is_not_in_the_list_of_a_release_without_one() {
    let list = verify_checksums(SUMS, SIG).unwrap();
    let e = list.find_archive(Platform::MacosArm64, None).unwrap_err();
    assert!(matches!(&e, Error::NoArchive { suffix } if suffix == "macos-arm64.zip"), "{e}");
    assert!(list.find_archive(Platform::MacosArm64, Some("150.0.7871.114")).is_err());
}

#[test]
fn a_flipped_byte_in_the_macos_list_breaks_the_signature() {
    let mut tampered = MAC_SUMS.to_vec();
    let at = tampered.iter().position(|b| *b == b'0').unwrap();
    tampered[at] = b'1';
    assert!(matches!(verify_checksums(&tampered, MAC_SIG), Err(Error::Signature(_))));
}
