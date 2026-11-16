use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bad config encoding: {0}")]
    Codec(String),
    #[error("profile not found: {0}")]
    ProfileNotFound(String),
    #[error(
        "no Clearcote kernel found (searched: {0}); install one with `citro kernel install`, or pass --kernel / set CITRO_KERNEL"
    )]
    KernelNotFound(String),
    #[error("the kernel exited before opening its DevTools port (status {status:?}): {stderr}")]
    KernelExited { status: Option<i32>, stderr: String },
    #[error("timed out waiting for the kernel's DevTools endpoint")]
    LaunchTimeout,
    #[error("{0}")]
    Invalid(String),
    #[error(
        "profile {0} belongs to a team hub and is not launched without the hub's lock: open it with `citro open` or in the app, which take the lock first"
    )]
    HubBound(String),
}

pub type Result<T> = std::result::Result<T, Error>;
