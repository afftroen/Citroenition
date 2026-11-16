use citro_hub::client::Error as HubError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this device is not logged in to a team hub: run `citro hub login --url <hub> --token <token>`")]
    NotLoggedIn,
    /// What the hub said. [`Error::locked_by`] tells whether it was "somebody else has this profile open".
    #[error("{0}")]
    Hub(#[from] HubError),
    #[error("{0}")]
    Core(#[from] citro_core::error::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
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
