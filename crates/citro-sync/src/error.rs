use citro_hub::client::Error as HubError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this device is not logged in to a team hub: run `citro hub login --url <hub> --token <token>`")]
    NotLoggedIn,
    /// What the hub said. [`Error::locked_by`] tells whether it was "somebody else has this profile open".
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error(transparent)]
    Core(#[from] citro_core::error::Error),
    #[error("io error: {0}")]
    Io(std::io::Error),
    #[error("json error: {0}")]
    Json(serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    /// A hub profile belongs to the whole team: deleting it needs an explicit yes.
    #[error("{0} belongs to the team hub: deleting it removes it for the whole team, not only here")]
    TeamProfile(String),
    #[error("profile data archive: {0}")]
    Archive(String),
}

/// Who has a profile open right now (the reason an open was refused).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedBy {
    pub user: String,
    pub device: String,
    /// Unix seconds.
    pub since: i64,
}

impl Error {
    pub fn locked_by(&self) -> Option<LockedBy> {
        match self {
            Error::Hub(HubError::Locked { holder, device, since, .. }) => {
                Some(LockedBy { user: holder.clone(), device: device.clone(), since: *since })
            }
            _ => None,
        }
    }
}

// The message already contains the inner error, so the inner error must not also be this error's `source()`:
// `citro` prints errors with `{:#}`, which walks the chain and would print everything twice.
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `{:#}` (anyhow) prints: the message, then every `source()` after it, joined with ": ".
    fn chain(e: &dyn std::error::Error) -> String {
        let mut out = e.to_string();
        let mut cur = e.source();
        while let Some(c) = cur {
            out.push_str(": ");
            out.push_str(&c.to_string());
            cur = c.source();
        }
        out
    }

    #[test]
    fn walking_the_chain_never_repeats_a_message() {
        let cases: Vec<Error> = vec![
            HubError::Forbidden("this needs a team owner or admin".into()).into(),
            citro_core::error::Error::from(std::io::Error::other("disk full")).into(),
            std::io::Error::other("disk full").into(),
            serde_json::from_str::<u8>("x").unwrap_err().into(),
        ];
        for e in cases {
            assert_eq!(chain(&e), e.to_string(), "the chain repeats itself");
        }
        assert_eq!(
            chain(&Error::from(HubError::Forbidden("this needs a team owner or admin".into()))),
            "forbidden: this needs a team owner or admin"
        );
    }
}
