use std::error::Error;
use std::fmt::{Display, Formatter};

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct ApiKeyHasher {
    secret: Vec<u8>,
}

impl ApiKeyHasher {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    pub fn hash_api_key(&self, api_key: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC-SHA256 accepts secret keys of any size");
        mac.update(api_key.as_bytes());

        hex::encode(mac.finalize().into_bytes())
    }
}

pub async fn load_api_key_hasher(
    secrets_client: &SecretsManagerClient,
    secret_arn: &str,
) -> Result<ApiKeyHasher, ApiKeySecretError> {
    let output = secrets_client
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await
        .map_err(|source| ApiKeySecretError::FetchFailed {
            source: Box::new(source),
        })?;

    let secret = output
        .secret_string()
        .ok_or(ApiKeySecretError::MissingSecretString)?;

    if secret.is_empty() {
        return Err(ApiKeySecretError::EmptySecretString);
    }

    Ok(ApiKeyHasher::new(secret.as_bytes().to_vec()))
}

#[derive(Debug)]
pub enum ApiKeySecretError {
    FetchFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    MissingSecretString,
    EmptySecretString,
}

impl Display for ApiKeySecretError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchFailed { source } => {
                write!(
                    formatter,
                    "failed to fetch proxy api key hash secret: {source}"
                )
            }
            Self::MissingSecretString => write!(
                formatter,
                "proxy api key hash secret must contain a string value"
            ),
            Self::EmptySecretString => {
                write!(formatter, "proxy api key hash secret must not be empty")
            }
        }
    }
}

impl Error for ApiKeySecretError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FetchFailed { source } => Some(source.as_ref()),
            Self::MissingSecretString | Self::EmptySecretString => None,
        }
    }
}
