//! Signing users in with the GitHub App's own OAuth credentials (for the dashboard).
//!
//! A user-to-server token can only see installations of this App that the user can access, so
//! the repositories it lists are exactly those the user may look at. The token is used during
//! sign-in only and never stored or logged.

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sheltie_core::RepoId;

/// Most repositories read per sign-in (pages of 100).
const MAX_REPOS: usize = 2_000;

/// Why sign-in failed.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// GitHub could not be reached or answered with an error status.
    #[error("GitHub request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// GitHub refused the code (expired, reused, or for another App).
    #[error("GitHub refused the sign-in: {0}")]
    Refused(String),
}

/// The App's OAuth settings.
#[derive(Clone)]
pub struct OAuthApp {
    client_id: String,
    client_secret: SecretString,
    /// `https://github.com`, or the GitHub Enterprise Server URL.
    web_url: String,
    /// `https://api.github.com`, or the GHES API URL.
    api_url: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for OAuthApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthApp")
            .field("client_id", &self.client_id)
            .field("web_url", &self.web_url)
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
}

/// Who signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    /// GitHub login.
    pub login: String,
    /// Repositories of this App's installations the user can access.
    pub repos: Vec<RepoId>,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct Installations {
    installations: Vec<IdOnly>,
}

#[derive(Deserialize)]
struct Repositories {
    repositories: Vec<IdOnly>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: u64,
}

impl OAuthApp {
    /// Creates the client. `web_url` and `api_url` default to github.com.
    pub fn new(
        client_id: String,
        client_secret: SecretString,
        web_url: Option<&str>,
        api_url: Option<&str>,
    ) -> Result<Self, OAuthError> {
        Ok(Self {
            client_id,
            client_secret,
            web_url: web_url
                .unwrap_or("https://github.com")
                .trim_end_matches('/')
                .to_owned(),
            api_url: api_url
                .unwrap_or("https://api.github.com")
                .trim_end_matches('/')
                .to_owned(),
            http: reqwest::Client::builder()
                .user_agent("sheltie")
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
        })
    }

    /// Where to send the browser to sign in.
    pub fn authorize_url(&self, redirect_uri: &str, state: &str) -> String {
        let enc = |s: &str| -> String {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect()
        };
        format!(
            "{}/login/oauth/authorize?client_id={}&redirect_uri={}&state={}",
            self.web_url,
            enc(&self.client_id),
            enc(redirect_uri),
            enc(state)
        )
    }

    /// Exchanges the callback's `code` and reads who signed in and what they can see.
    pub async fn sign_in(&self, code: &str, redirect_uri: &str) -> Result<SignedIn, OAuthError> {
        let reply: TokenReply = self
            .http
            .post(format!("{}/login/oauth/access_token", self.web_url))
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": self.client_id,
                "client_secret": self.client_secret.expose_secret(),
                "code": code,
                "redirect_uri": redirect_uri,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let token = match (reply.access_token, reply.error) {
            (Some(t), _) => SecretString::from(t),
            (None, e) => return Err(OAuthError::Refused(e.unwrap_or_else(|| "no token".into()))),
        };
        let get = |path: String| {
            self.http
                .get(format!("{}{path}", self.api_url))
                .bearer_auth(token.expose_secret())
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
        };
        let user: User = get("/user".into())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let installations: Installations = get("/user/installations?per_page=100".into())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut repos = Vec::new();
        'installations: for inst in installations.installations {
            for page in 1..=(MAX_REPOS / 100) {
                let batch: Repositories = get(format!(
                    "/user/installations/{}/repositories?per_page=100&page={page}",
                    inst.id
                ))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
                let n = batch.repositories.len();
                repos.extend(batch.repositories.into_iter().map(|r| RepoId(r.id)));
                if repos.len() >= MAX_REPOS {
                    break 'installations;
                }
                if n < 100 {
                    break;
                }
            }
        }
        repos.sort();
        repos.dedup();
        Ok(SignedIn {
            login: user.login,
            repos,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_is_encoded() {
        let app = OAuthApp::new("Iv1.abc".into(), SecretString::from("s"), None, None).unwrap();
        assert_eq!(
            app.authorize_url("https://sheltie.example.com/auth/callback", "x y"),
            "https://github.com/login/oauth/authorize?client_id=Iv1.abc&redirect_uri=https%3A%2F%2Fsheltie.example.com%2Fauth%2Fcallback&state=x%20y"
        );
        assert!(!format!("{app:?}").contains("client_secret"));
    }
}
