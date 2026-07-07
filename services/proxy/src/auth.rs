use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use axum::http::HeaderMap;

use crate::api_key::ApiKeyHasher;
use crate::engineer_auth::{AuthenticatedEngineer, EngineerAuth, EngineerAuthError};

const API_KEY_HEADER: &str = "x-api-key";
const API_KEY_PREFIX: &str = "iag_";

#[derive(Clone)]
pub struct RequestAuthenticator {
    api_key_hasher: Arc<ApiKeyHasher>,
    engineer_auth: Arc<EngineerAuth>,
}

impl RequestAuthenticator {
    pub fn new(api_key_hasher: Arc<ApiKeyHasher>, engineer_auth: Arc<EngineerAuth>) -> Self {
        Self {
            api_key_hasher,
            engineer_auth,
        }
    }

    pub async fn authenticate_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<AuthenticatedEngineer, AuthError> {
        let api_key = read_api_key(headers)?;
        let api_key_hash = self.api_key_hasher.hash_api_key(api_key);
        let engineer = self
            .engineer_auth
            .find_engineer_by_api_key_hash(&api_key_hash)
            .await
            .map_err(AuthError::LookupFailed)?
            .ok_or(AuthError::InvalidCredentials)?;

        if !engineer.enabled {
            return Err(AuthError::DisabledEngineer);
        }

        Ok(engineer)
    }
}

pub(crate) fn read_api_key(headers: &HeaderMap) -> Result<&str, AuthError> {
    let api_key = headers
        .get(API_KEY_HEADER)
        .ok_or(AuthError::MissingApiKey)?
        .to_str()
        .map_err(|_| AuthError::InvalidApiKeyFormat)?;

    validate_api_key(api_key)?;

    Ok(api_key)
}

fn validate_api_key(api_key: &str) -> Result<(), AuthError> {
    if api_key.is_empty() || !api_key.starts_with(API_KEY_PREFIX) {
        return Err(AuthError::InvalidApiKeyFormat);
    }

    if api_key.chars().any(char::is_whitespace) {
        return Err(AuthError::InvalidApiKeyFormat);
    }

    Ok(())
}

#[derive(Debug)]
pub enum AuthError {
    MissingApiKey,
    InvalidApiKeyFormat,
    InvalidCredentials,
    DisabledEngineer,
    LookupFailed(EngineerAuthError),
}

impl Display for AuthError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingApiKey => write!(formatter, "missing api key"),
            Self::InvalidApiKeyFormat => write!(formatter, "invalid api key format"),
            Self::InvalidCredentials => write!(formatter, "invalid api key"),
            Self::DisabledEngineer => write!(formatter, "engineer is disabled"),
            Self::LookupFailed(error) => write!(formatter, "engineer auth lookup failed: {error}"),
        }
    }
}

impl Error for AuthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::LookupFailed(error) => Some(error),
            Self::MissingApiKey
            | Self::InvalidApiKeyFormat
            | Self::InvalidCredentials
            | Self::DisabledEngineer => None,
        }
    }
}
