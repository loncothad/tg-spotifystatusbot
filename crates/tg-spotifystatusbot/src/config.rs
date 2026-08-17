use std::net::SocketAddr;
use std::path::PathBuf;

use better_default::Default;
use compact_str::{format_compact, CompactString};
use smallvec::SmallVec;
use url::Url;

use crate::error::{AppError, Result};

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const DEFAULT_REDB: &str = "./data/bot.redb";

#[derive(Clone, Debug)]
pub struct Config {
    pub telegram_bot_token: CompactString,
    pub spotify_client_id: CompactString,
    pub spotify_client_secret: CompactString,
    pub public_base_url: CompactString,
    pub oauth_redirect_uri: CompactString,
    pub redb_path: PathBuf,
    pub http_bind: SocketAddr,
    pub card_signing_secret: CompactString,
    pub admin_ids: SmallVec<[u64; 8]>,
    pub http: HttpSettings,
}

#[derive(Clone, Debug, Default)]
#[default(request_timeout_secs: 20, oauth_state_ttl_secs: 900, card_url_ttl_secs: 300)]
pub struct HttpSettings {
    pub request_timeout_secs: u64,
    pub oauth_state_ttl_secs: i64,
    pub card_url_ttl_secs: i64,
}

impl Config {
    pub fn is_admin(&self, telegram_user_id: u64) -> bool {
        self.admin_ids.contains(&telegram_user_id)
    }

    pub fn from_env() -> Result<Self> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars(vars: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let mut map = std::collections::HashMap::<CompactString, CompactString>::new();
        for (key, value) in vars {
            map.insert(CompactString::from(key), CompactString::from(value));
        }

        let required = |name: &str| -> Result<CompactString> {
            map.get(name)
                .map(|value| CompactString::from(value.trim()))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::config(format_compact!("Missing required env var {name}")))
        };

        let telegram_bot_token = required("TELEGRAM_BOT_TOKEN")?;
        let spotify_client_id = required("SPOTIFY_CLIENT_ID")?;
        let spotify_client_secret = required("SPOTIFY_CLIENT_SECRET")?;
        let public_base_url = normalize_base_url(&required("PUBLIC_BASE_URL")?)?;

        let oauth_redirect_uri = map
            .get("OAUTH_REDIRECT_URI")
            .map(|value| CompactString::from(value.trim()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format_compact!("{public_base_url}/oauth/callback"));

        let redb_path = map
            .get("REDB_PATH")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_REDB));

        let http_bind = map
            .get("HTTP_BIND")
            .map(|value| CompactString::from(value.trim()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| CompactString::from(DEFAULT_BIND))
            .parse::<SocketAddr>()
            .map_err(|err| AppError::config(format_compact!("Invalid HTTP_BIND: {err}")))?;

        let card_signing_secret = map
            .get("CARD_SIGNING_SECRET")
            .map(|value| CompactString::from(value.trim()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format_compact!("card:{telegram_bot_token}"));

        let admin_ids = map
            .get("ADMIN_USER_IDS")
            .map(|value| parse_admin_ids(value))
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            telegram_bot_token,
            spotify_client_id,
            spotify_client_secret,
            public_base_url,
            oauth_redirect_uri,
            redb_path,
            http_bind,
            card_signing_secret,
            admin_ids,
            http: HttpSettings::default(),
        })
    }
}

fn parse_admin_ids(raw: &str) -> Result<SmallVec<[u64; 8]>> {
    let mut ids = SmallVec::new();
    for part in raw.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let id = part.parse::<u64>().map_err(|_| {
            AppError::config(format_compact!("Invalid ADMIN_USER_IDS entry: {part}"))
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn normalize_base_url(raw: &str) -> Result<CompactString> {
    let parsed = Url::parse(raw)
        .map_err(|err| AppError::config(format_compact!("Invalid PUBLIC_BASE_URL: {err}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(AppError::config(
            "PUBLIC_BASE_URL must be an http(s) URL reachable by Telegram and Spotify",
        ));
    }
    let mut normalized = CompactString::from(parsed.as_str());
    if normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::SmallVec;

    fn vars(pairs: &[(&str, &str)]) -> SmallVec<[(String, String); 8]> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn requires_core_env() {
        let err = Config::from_vars(vars(&[])).unwrap_err();
        assert!(err.to_string().contains("TELEGRAM_BOT_TOKEN"));
    }

    #[test]
    fn loads_defaults() {
        let cfg = Config::from_vars(vars(&[
            ("TELEGRAM_BOT_TOKEN", "token"),
            ("SPOTIFY_CLIENT_ID", "id"),
            ("SPOTIFY_CLIENT_SECRET", "secret"),
            ("PUBLIC_BASE_URL", "https://bot.example.com/"),
        ]))
        .unwrap();

        assert_eq!(
            cfg.oauth_redirect_uri,
            "https://bot.example.com/oauth/callback"
        );
        assert_eq!(cfg.redb_path, PathBuf::from(DEFAULT_REDB));
        assert_eq!(cfg.http_bind, DEFAULT_BIND.parse().unwrap());
        assert_eq!(cfg.http.card_url_ttl_secs, 300);
        assert_eq!(cfg.public_base_url, "https://bot.example.com");
        assert!(cfg.admin_ids.is_empty());
    }

    #[test]
    fn parses_admin_ids() {
        let cfg = Config::from_vars(vars(&[
            ("TELEGRAM_BOT_TOKEN", "token"),
            ("SPOTIFY_CLIENT_ID", "id"),
            ("SPOTIFY_CLIENT_SECRET", "secret"),
            ("PUBLIC_BASE_URL", "https://bot.example.com"),
            ("ADMIN_USER_IDS", "42, 7 7,9"),
        ]))
        .unwrap();
        assert_eq!(cfg.admin_ids.as_slice(), &[42, 7, 9]);
        assert!(cfg.is_admin(7));
        assert!(!cfg.is_admin(1));
    }
}
