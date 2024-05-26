use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, instrument};
use which::which;

use crate::types::Token;
use crate::{Error, TokenProvider};

#[derive(Debug)]
pub(crate) struct GCloudAuthorizedUser {
    gcloud: PathBuf,
    project_id: Option<Arc<str>>,
    token: RwLock<Arc<Token>>,
}

impl GCloudAuthorizedUser {
    pub(crate) async fn new() -> Result<Self, Error> {
        debug!("try to print access token via `gcloud`");
        let gcloud = which("gcloud").map_err(|_| Error::Str("`gcloud` binary not found"))?;
        let project_id = run(&gcloud, &["config", "get-value", "project"]).ok();
        let token = RwLock::new(Self::fetch_token(&gcloud)?);
        Ok(Self {
            gcloud,
            project_id: project_id.map(Arc::from),
            token,
        })
    }

    #[instrument(level = tracing::Level::DEBUG, skip(gcloud))]
    fn fetch_token(gcloud: &Path) -> Result<Arc<Token>, Error> {
        Ok(Arc::new(Token::from_string(
            run(gcloud, &["auth", "print-access-token", "--quiet"])?,
            DEFAULT_TOKEN_DURATION,
        )))
    }
}

#[async_trait]
impl TokenProvider for GCloudAuthorizedUser {
    async fn token(&self, _scopes: &[&str]) -> Result<Arc<Token>, Error> {
        let token = self.token.read().await.clone();
        if !token.has_expired() {
            return Ok(token);
        }

        let mut locked = self.token.write().await;
        let token = Self::fetch_token(&self.gcloud)?;
        *locked = token.clone();
        Ok(token)
    }

    async fn project_id(&self) -> Result<Arc<str>, Error> {
        self.project_id
            .clone()
            .ok_or(Error::Str("failed to get project ID from `gcloud`"))
    }
}

fn run(gcloud: &Path, cmd: &[&str]) -> Result<String, Error> {
    let mut command = Command::new(gcloud);
    command.args(cmd);

    let mut stdout = match command.output() {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => return Err(Error::Str("running `gcloud` command failed")),
        Err(err) => return Err(Error::Io("failed to run `gcloud`", err)),
    };

    while let Some(b' ' | b'\r' | b'\n') = stdout.last() {
        stdout.pop();
    }

    String::from_utf8(stdout).map_err(|_| Error::Str("output from `gcloud` is not UTF-8"))
}

/// The default number of seconds that it takes for a Google Cloud auth token to expire.
/// This appears to be the default from practical testing, but we have not found evidence
/// that this will always be the default duration.
pub(crate) const DEFAULT_TOKEN_DURATION: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn gcloud() {
        let gcloud = GCloudAuthorizedUser::new().await.unwrap();
        println!("{:?}", gcloud.project_id);
        if let Ok(t) = gcloud.token(&[""]).await {
            let expires = Utc::now() + DEFAULT_TOKEN_DURATION;
            println!("{:?}", t);
            assert!(!t.has_expired());
            assert!(t.expires_at() < expires + Duration::from_secs(1));
            assert!(t.expires_at() > expires - Duration::from_secs(1));
        } else {
            panic!("GCloud Authorized User failed to get a token");
        }
    }

    /// `gcloud_authorized_user` is the only user type to get a token that isn't deserialized from
    /// JSON, and that doesn't include an expiry time. As such, the default token expiry time
    /// functionality is tested here.
    #[test]
    fn test_token_from_string() {
        let s = String::from("abc123");
        let token = Token::from_string(s, DEFAULT_TOKEN_DURATION);
        let expires = Utc::now() + DEFAULT_TOKEN_DURATION;

        assert_eq!(token.as_str(), "abc123");
        assert!(!token.has_expired());
        assert!(token.expires_at() < expires + Duration::from_secs(1));
        assert!(token.expires_at() > expires - Duration::from_secs(1));
    }

    #[test]
    fn test_deserialize_no_time() {
        let s = r#"{"access_token":"abc123"}"#;
        let result = serde_json::from_str::<Token>(s)
            .expect_err("Deserialization from JSON should fail when no expiry_time is included");

        assert!(result.is_data());
    }
}
