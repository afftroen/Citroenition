//! The crate-wide error type.

use std::io;

/// `Result` alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while locating, downloading, verifying or unpacking a kernel.
///
/// Nothing in this crate panics on bad input (hostile archives, garbage signatures, odd server replies): every such
/// case surfaces as one of these variants.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// There is no prebuilt Clearcote kernel for this OS/CPU (Intel Macs, arm64 Linux and Windows, ...).
    #[error(
        "no prebuilt Clearcote kernel is published for {os}/{arch} (only linux-x64, windows-x64 and macos-arm64 are); \
         use a kernel you provide yourself: set CITRO_KERNEL (or pass --kernel) to the path of its browser \
         executable (`chrome`, or `Chromium.app/Contents/MacOS/Chromium` on macOS)"
    )]
    UnsupportedPlatform {
        /// `std::env::consts::OS` of the host.
        os: String,
        /// `std::env::consts::ARCH` of the host.
        arch: String,
    },

    /// There is no Citroenition release archive for this OS/CPU.
    #[error("Citroenition releases are built for Linux x86-64 and macOS on Apple silicon, not {os}/{arch}")]
    UnsupportedApp {
        /// `std::env::consts::OS` of the host.
        os: String,
        /// `std::env::consts::ARCH` of the host.
        arch: String,
    },

    /// A caller-supplied value (release tag, repository, ...) has an unacceptable shape.
    #[error("invalid {what} `{value}`")]
    InvalidInput {
        /// Which input was rejected.
        what: &'static str,
        /// The rejected value.
        value: String,
    },

    /// The request could not be completed (DNS, connect, TLS, timeout, broken connection, ...).
    #[error("request to {url} failed: {message}")]
    Network {
        /// The URL that was requested.
        url: String,
        /// What went wrong.
        message: String,
    },

    /// The server answered with a non-success HTTP status.
    #[error("{url} answered HTTP {status}{}", http_hint(*.status))]
    HttpStatus {
        /// The URL that was requested.
        url: String,
        /// The HTTP status code.
        status: u16,
    },

    /// The server answered, but not with something we can use.
    #[error("unexpected response from {url}: {message}")]
    BadResponse {
        /// The URL that was requested.
        url: String,
        /// What was wrong with the reply.
        message: String,
    },

    /// The release does not carry a file we need.
    #[error("release {tag} has no asset named `{name}`")]
    MissingAsset {
        /// Tag of the release that was inspected.
        tag: String,
        /// Name of the missing asset.
        name: String,
    },

    /// The signed checksum list has no archive for this platform.
    #[error("the signed SHA256SUMS.txt lists no `{suffix}` archive")]
    NoArchive {
        /// The platform suffix that was searched for (e.g. `linux-x64.tar.xz`).
        suffix: String,
    },

    /// The checksum list is malformed or contradicts itself.
    #[error("SHA256SUMS.txt is unusable: {0}")]
    BadSums(String),

    /// The OpenPGP signature on SHA256SUMS.txt did not verify against the pinned key (or the pinned key itself is
    /// broken).
    #[error("signature check failed: {0}")]
    Signature(String),

    /// A SHA-256 differs from the one in the (signed) checksum list.
    #[error("sha256 mismatch for {what}: the signed list says {expected}, got {actual}")]
    ChecksumMismatch {
        /// Which file was hashed.
        what: String,
        /// The digest from the signed list.
        expected: String,
        /// The digest that was computed.
        actual: String,
    },

    /// The archive's digest is not in the signed checksum list at all.
    #[error("the archive (sha256 {sha256}) is not listed in the signed SHA256SUMS.txt")]
    UnknownArchive {
        /// The digest of the archive that was offered.
        sha256: String,
    },

    /// The archive contains something we refuse to extract (path traversal, links out of the install dir, devices, ...).
    #[error("unsafe archive rejected: {0}")]
    UnsafeArchive(String),

    /// The archive exceeds the entry-count or size caps meant to blunt archive bombs.
    #[error("archive exceeds a safety limit: {0}")]
    ArchiveLimit(String),

    /// The archive is truncated or corrupt (bad xz / tar / zip data).
    #[error("corrupt archive: {0}")]
    BadArchive(String),

    /// The extracted tree has no (or a non-regular) main binary at the top level.
    #[error("the archive has no regular file `{0}` at its top level")]
    MissingBinary(String),

    /// A filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What we were doing.
        context: String,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
}

fn http_hint(status: u16) -> &'static str {
    match status {
        403 | 429 => " (GitHub rate limit? unauthenticated API calls are capped per IP; retry later)",
        404 => " (release or asset not found)",
        _ => "",
    }
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    pub(crate) fn unsafe_archive(msg: impl Into<String>) -> Self {
        Error::UnsafeArchive(msg.into())
    }
}

/// Attach a human-readable context to an `io::Result`.
pub(crate) trait IoContext<T> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| Error::io(what(), e))
    }
}
