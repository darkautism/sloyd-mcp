use crate::cookies::NormalizedCookie;
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    Client, StatusCode, Url,
    header::{COOKIE, LOCATION},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

const AUTH_DOMAIN: &str = "auth.sloyd.ai";
const AUTHORIZE_URL: &str = "https://auth.sloyd.ai/authorize";
const TOKEN_URL: &str = "https://auth.sloyd.ai/oauth/token";
const CLIENT_ID: &str = "72kndYPfVYaNnfkmjqgXwUFXI1lhZFtO";
const REDIRECT_URI: &str = "https://app.sloyd.ai";
const AUDIENCE: &str = "sloyd-api";
const SCOPE: &str = "openid profile email";

#[derive(Debug)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct Auth0Session {
    cookie_path: PathBuf,
    http: Client,
    token: Option<CachedToken>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

impl Auth0Session {
    pub fn new(cookie_path: impl Into<PathBuf>) -> Result<Self> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .context("build Auth0 HTTP client")?;
        Ok(Self {
            cookie_path: cookie_path.into(),
            http,
            token: None,
        })
    }

    pub fn cookie_path(&self) -> &Path {
        &self.cookie_path
    }

    pub async fn probe(&mut self) -> Result<bool> {
        match self.access_token().await {
            Ok(_) => Ok(true),
            Err(error) => {
                let message = error.to_string();
                if message.contains("login_required") || message.contains("no Auth0 session cookie")
                {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub async fn access_token(&mut self) -> Result<String> {
        if let Some(cached) = &self.token
            && cached.expires_at > Instant::now() + Duration::from_secs(60)
        {
            return Ok(cached.value.clone());
        }

        let token = self.silent_login().await?;
        self.token = Some(CachedToken {
            value: token.0.clone(),
            expires_at: Instant::now() + Duration::from_secs(token.1),
        });
        Ok(token.0)
    }

    pub fn invalidate(&mut self) {
        self.token = None;
    }

    async fn silent_login(&self) -> Result<(String, u64)> {
        let cookies = load_cookie_file(&self.cookie_path).await?;
        let cookie_header = auth_cookie_header(&cookies)?;

        let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = Uuid::new_v4().simple().to_string();
        let nonce = Uuid::new_v4().simple().to_string();

        let response = self
            .http
            .get(AUTHORIZE_URL)
            .query(&[
                ("client_id", CLIENT_ID),
                ("redirect_uri", REDIRECT_URI),
                ("response_type", "code"),
                ("scope", SCOPE),
                ("audience", AUDIENCE),
                ("prompt", "none"),
                ("state", &state),
                ("nonce", &nonce),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
            ])
            .header(COOKIE, cookie_header)
            .send()
            .await
            .context("Auth0 silent authorize request failed")?;

        if response.status() != StatusCode::FOUND
            && response.status() != StatusCode::SEE_OTHER
            && response.status() != StatusCode::TEMPORARY_REDIRECT
        {
            bail!("Auth0 silent authorize returned HTTP {}", response.status());
        }

        let location = response
            .headers()
            .get(LOCATION)
            .context("Auth0 silent authorize returned no redirect")?
            .to_str()
            .context("Auth0 redirect header is not UTF-8")?;
        let redirect = Url::parse(location).context("parse Auth0 redirect")?;

        if let Some(error) = redirect
            .query_pairs()
            .find(|(key, _)| key == "error")
            .map(|(_, value)| value.into_owned())
        {
            bail!("Auth0 silent authorize failed: {error}");
        }

        let returned_state = redirect
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .context("Auth0 redirect contained no state")?;
        if returned_state != state {
            bail!("Auth0 state mismatch");
        }

        let code = redirect
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .context("Auth0 redirect contained no authorization code")?;

        let response = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("redirect_uri", REDIRECT_URI),
            ])
            .send()
            .await
            .context("Auth0 token exchange failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!(
                "Auth0 token exchange failed HTTP {}: {}",
                status,
                body.chars().take(300).collect::<String>()
            );
        }

        let token: TokenResponse = response
            .json()
            .await
            .context("parse Auth0 token response")?;
        Ok((token.access_token, token.expires_in.unwrap_or(3600)))
    }
}

async fn load_cookie_file(path: &Path) -> Result<Vec<NormalizedCookie>> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read cookie file {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse cookie file {}", path.display()))
}

fn domain_matches(cookie_domain: &str, host: &str) -> bool {
    let domain = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn auth_cookie_header(cookies: &[NormalizedCookie]) -> Result<String> {
    let mut auth0 = None;
    let mut auth0_compat = None;
    let mut optional = Vec::new();

    for cookie in cookies {
        if !domain_matches(&cookie.domain, AUTH_DOMAIN) {
            continue;
        }
        match cookie.name.as_str() {
            "auth0" => auth0 = Some(cookie),
            "auth0_compat" => auth0_compat = Some(cookie),
            "did" | "did_compat" | "__cf_bm" => optional.push(cookie),
            _ => {}
        }
    }

    if auth0.is_none() && auth0_compat.is_none() {
        bail!("no Auth0 session cookie (auth0/auth0_compat) for auth.sloyd.ai");
    }

    let mut selected = Vec::new();
    if let Some(cookie) = auth0 {
        selected.push(cookie);
    }
    if let Some(cookie) = auth0_compat {
        selected.push(cookie);
    }
    selected.extend(optional);

    Ok(selected
        .into_iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, domain: &str, value: &str) -> NormalizedCookie {
        NormalizedCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: Some("/".into()),
            secure: Some(true),
            http_only: Some(true),
            same_site: None,
            expires: None,
        }
    }

    #[test]
    fn auth0_cookie_is_sufficient() {
        let header = auth_cookie_header(&[cookie("auth0", "auth.sloyd.ai", "secret")]).unwrap();
        assert_eq!(header, "auth0=secret");
    }

    #[test]
    fn auth0_compat_is_accepted_without_auth0() {
        let header =
            auth_cookie_header(&[cookie("auth0_compat", "auth.sloyd.ai", "compat")]).unwrap();
        assert_eq!(header, "auth0_compat=compat");
    }

    #[test]
    fn analytics_cookie_is_not_authentication() {
        let error =
            auth_cookie_header(&[cookie("intercom-session", ".sloyd.ai", "not-auth")]).unwrap_err();
        assert!(error.to_string().contains("no Auth0 session cookie"));
    }

    #[test]
    fn auth_domain_matching_accepts_dot_prefix() {
        assert!(domain_matches(".auth.sloyd.ai", "auth.sloyd.ai"));
        assert!(domain_matches(".sloyd.ai", "auth.sloyd.ai"));
        assert!(!domain_matches("app.sloyd.ai", "auth.sloyd.ai"));
    }
}
