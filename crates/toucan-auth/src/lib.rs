use md5::{Digest, Md5};
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

const MAX_PROFILE_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerIdentity {
    pub username: String,
    pub uuid: Uuid,
    pub properties: Vec<ProfileProperty>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AuthError {
    #[error("username must contain 3-16 ASCII letters, digits, or underscores")]
    InvalidUsername,
}

#[derive(Debug, Error)]
pub enum ProfileLookupError {
    #[error("Mojang profile request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Mojang profile response was malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Mojang profile response exceeded {MAX_PROFILE_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
}

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
        properties: Vec::new(),
    })
}

pub async fn lookup_profile_properties(
    username: &str,
) -> Result<Option<Vec<ProfileProperty>>, ProfileLookupError> {
    #[derive(Deserialize)]
    struct ProfileSummary {
        id: String,
    }
    #[derive(Deserialize)]
    struct ProfileResponse {
        properties: Vec<ProfileProperty>,
    }

    if !is_valid_username(username) {
        return Ok(None);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    let response = client
        .get(format!(
            "https://api.mojang.com/users/profiles/minecraft/{username}"
        ))
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NO_CONTENT
        || response.status() == reqwest::StatusCode::NOT_FOUND
    {
        return Ok(None);
    }
    let summary: ProfileSummary = bounded_json(response).await?;
    let response = client
        .get(format!(
            "https://sessionserver.mojang.com/session/minecraft/profile/{}?unsigned=false",
            summary.id
        ))
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NO_CONTENT
        || response.status() == reqwest::StatusCode::NOT_FOUND
    {
        return Ok(None);
    }
    let profile: ProfileResponse = bounded_json(response).await?;
    Ok(Some(profile.properties))
}

async fn bounded_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, ProfileLookupError> {
    let response = response.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROFILE_RESPONSE_BYTES as u64)
    {
        return Err(ProfileLookupError::ResponseTooLarge);
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_PROFILE_RESPONSE_BYTES {
        return Err(ProfileLookupError::ResponseTooLarge);
    }
    Ok(serde_json::from_slice(&bytes)?)
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
