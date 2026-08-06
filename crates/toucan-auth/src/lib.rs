//! Authentication boundary and development offline identities.

use md5::{Digest, Md5};
use thiserror::Error;
use uuid::Uuid;

/// A validated identity accepted by Toucan's login coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerIdentity {
    /// Minecraft username.
    pub username: String,
    /// Authoritative UUID selected by the server.
    pub uuid: Uuid,
}

/// Identity validation failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AuthError {
    /// The username was outside vanilla's accepted syntax.
    #[error("username must contain 3-16 ASCII letters, digits, or underscores")]
    InvalidUsername,
}

/// Validates a username and derives Java's deterministic offline UUID.
pub fn authenticate_offline(username: &str) -> Result<PlayerIdentity, AuthError> {
    if !is_valid_username(username) {
        return Err(AuthError::InvalidUsername);
    }

    let mut digest = Md5::digest(format!("OfflinePlayer:{username}").as_bytes());
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest);

    Ok(PlayerIdentity {
        username: username.to_owned(),
        uuid: Uuid::from_bytes(bytes),
    })
}

fn is_valid_username(username: &str) -> bool {
    (3..=16).contains(&username.len())
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::{AuthError, authenticate_offline};

    #[test]
    fn matches_java_offline_uuid_derivation() {
        let identity = authenticate_offline("Notch");
        assert_eq!(
            identity.as_ref().map(|value| value.uuid.to_string()),
            Ok("b50ad385-829d-3141-a216-7e7d7539ba7f".to_owned())
        );
    }

    #[test]
    fn rejects_invalid_usernames() {
        for username in ["ab", "way_too_long_for_mc", "bad-name", "ééé"] {
            assert_eq!(
                authenticate_offline(username),
                Err(AuthError::InvalidUsername)
            );
        }
    }
}
