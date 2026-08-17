use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use compact_str::{format_compact, CompactString};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use smallvec::SmallVec;
use tg_spotifystatusbot_render::CardKind;

use crate::config::Config;
use crate::db::{now_unix, OauthState, SpotifyTokens, Store};
use crate::error::{AppError, Result};

const AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const CURRENTLY_PLAYING_URL: &str = "https://api.spotify.com/v1/me/player/currently-playing";
const ME_URL: &str = "https://api.spotify.com/v1/me";
const SCOPES: &str = "user-read-currently-playing user-read-playback-state user-read-private";

#[derive(Clone, Debug)]
pub struct SpotifyClient {
    http: reqwest::Client,
    client_id: CompactString,
    client_secret: CompactString,
    redirect_uri: CompactString,
}

struct SpotifyProfile {
    name: CompactString,
    avatar: Option<Vec<u8>>,
}

impl SpotifyProfile {
    fn fallback() -> Self {
        Self {
            name: CompactString::from("Someone"),
            avatar: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: CompactString,
    pub artist: CompactString,
    pub album: CompactString,
    pub album_art_url: Option<CompactString>,
    pub progress_ms: u32,
    pub duration_ms: u32,
    pub is_playing: bool,
    pub spotify_url: Option<CompactString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Playback {
    Playing(NowPlaying),
    Idle,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: CompactString,
    token_type: CompactString,
    scope: Option<CompactString>,
    expires_in: i64,
    refresh_token: Option<CompactString>,
}

#[derive(Debug, Deserialize)]
pub struct CurrentlyPlayingResponse {
    pub is_playing: Option<bool>,
    pub progress_ms: Option<u32>,
    pub item: Option<PlayingItem>,
}

#[derive(Debug, Deserialize)]
pub struct PlayingItem {
    pub name: Option<CompactString>,
    pub duration_ms: Option<u32>,
    pub artists: Option<SmallVec<[Named; 4]>>,
    pub album: Option<Album>,
    pub show: Option<Named>,
    pub images: Option<SmallVec<[Image; 4]>>,
    pub external_urls: Option<ExternalUrls>,
}

#[derive(Debug, Deserialize)]
pub struct Named {
    pub name: Option<CompactString>,
}

#[derive(Debug, Deserialize)]
pub struct Album {
    pub name: Option<CompactString>,
    pub images: Option<SmallVec<[Image; 4]>>,
}

#[derive(Debug, Deserialize)]
pub struct Image {
    pub url: Option<CompactString>,
    pub width: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ExternalUrls {
    pub spotify: Option<CompactString>,
}

impl SpotifyClient {
    pub fn new(config: &Config, http: reqwest::Client) -> Self {
        Self {
            http,
            client_id: config.spotify_client_id.clone(),
            client_secret: config.spotify_client_secret.clone(),
            redirect_uri: config.oauth_redirect_uri.clone(),
        }
    }

    pub async fn start_link(&self, store: &Store, telegram_user_id: u64) -> Result<CompactString> {
        let state = format_compact!("{}", uuid::Uuid::now_v7());
        let verifier = pkce_verifier();
        store
            .put_oauth_state(
                &state,
                &OauthState {
                    telegram_user_id,
                    code_verifier: verifier.clone(),
                    created_at: now_unix(),
                },
            )
            .await?;

        let challenge = pkce_challenge(&verifier);
        Ok(format_compact!(
            "{AUTH_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge_method=S256&code_challenge={}&show_dialog=true",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&self.redirect_uri),
            urlencoding::encode(SCOPES),
            urlencoding::encode(&state),
            urlencoding::encode(&challenge),
        ))
    }

    pub async fn finish_link(
        &self,
        store: &Store,
        code: &str,
        state: &str,
        ttl_secs: i64,
    ) -> Result<u64> {
        let oauth = store.take_oauth_state(state, ttl_secs).await?;
        let tokens = self.exchange_code(code, &oauth.code_verifier).await?;
        store
            .put_tokens(oauth.telegram_user_id, &tokens)
            .await?;
        Ok(oauth.telegram_user_id)
    }

    pub async fn unlink(&self, store: &Store, telegram_user_id: u64) -> Result<bool> {
        store.delete_tokens(telegram_user_id).await
    }

    pub async fn playback(&self, store: &Store, telegram_user_id: u64) -> Result<Playback> {
        let access = self.access_token(store, telegram_user_id).await?;
        match self.fetch_currently_playing(&access).await {
            Ok(playback) => Ok(playback),
            Err(AppError::Spotify(message)) if message.contains("401") => {
                let refreshed = self.refresh_stored(store, telegram_user_id).await?;
                self.fetch_currently_playing(&refreshed.access_token).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn card_for_user(&self, store: &Store, telegram_user_id: u64) -> Result<CardKind> {
        let profile = self.listener_profile(store, telegram_user_id).await;
        match self.playback(store, telegram_user_id).await {
            Ok(Playback::Playing(now)) => Ok(CardKind::Playing {
                username: profile.name,
                title: now.title,
                artist: now.artist,
                progress_ms: now.progress_ms,
                duration_ms: now.duration_ms,
                is_playing: now.is_playing,
                album: now.album,
                album_art: match now.album_art_url {
                    Some(url) => self.download_image(&url).await.ok(),
                    None => None,
                },
                avatar: profile.avatar,
                track_url: now.spotify_url,
            }),
            Ok(Playback::Idle) => Ok(CardKind::Idle),
            Err(AppError::NotLinked) => Ok(CardKind::NotLinked),
            Err(err) => {
                tracing::warn!(user_id = telegram_user_id, error = %err, "Failed to fetch playback");
                Ok(CardKind::Error {
                    message: CompactString::from(err.user_message()),
                })
            }
        }
    }

    async fn listener_profile(&self, store: &Store, telegram_user_id: u64) -> SpotifyProfile {
        let Ok(token) = self.access_token(store, telegram_user_id).await else {
            return SpotifyProfile::fallback();
        };
        self.fetch_profile(&token)
            .await
            .unwrap_or_else(|_| SpotifyProfile::fallback())
    }

    async fn fetch_profile(&self, access_token: &str) -> Result<SpotifyProfile> {
        #[derive(Deserialize)]
        struct MeResponse {
            display_name: Option<CompactString>,
            id: Option<CompactString>,
            images: Option<SmallVec<[Image; 4]>>,
        }
        let response = self
            .http
            .get(ME_URL)
            .bearer_auth(access_token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AppError::spotify(format_compact!(
                "Failed to load profile ({})",
                response.status()
            )));
        }
        let parsed: MeResponse = response.json().await?;
        let name = parsed
            .display_name
            .filter(|name| !name.is_empty())
            .or(parsed.id)
            .unwrap_or_else(|| CompactString::from("Someone"));
        let mut images = parsed.images.unwrap_or_default();
        images.sort_by_key(|image| std::cmp::Reverse(image.width.unwrap_or(0)));
        let avatar = match images.into_iter().find_map(|image| image.url) {
            Some(url) => self.download_image(&url).await.ok(),
            None => None,
        };
        Ok(SpotifyProfile { name, avatar })
    }

    async fn access_token(&self, store: &Store, telegram_user_id: u64) -> Result<CompactString> {
        let tokens = store
            .get_tokens(telegram_user_id)
            .await?
            .ok_or(AppError::NotLinked)?;
        if tokens.expires_at > now_unix() + 60 {
            return Ok(tokens.access_token);
        }
        Ok(self
            .refresh_stored(store, telegram_user_id)
            .await?
            .access_token)
    }

    async fn refresh_stored(&self, store: &Store, telegram_user_id: u64) -> Result<SpotifyTokens> {
        let current = store
            .get_tokens(telegram_user_id)
            .await?
            .ok_or(AppError::NotLinked)?;
        let refreshed = self.refresh_tokens(&current).await?;
        store.put_tokens(telegram_user_id, &refreshed).await?;
        Ok(refreshed)
    }

    async fn exchange_code(&self, code: &str, verifier: &str) -> Result<SpotifyTokens> {
        let body = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", verifier),
        ];
        self.request_tokens(&body, None).await
    }

    pub async fn refresh_tokens(&self, current: &SpotifyTokens) -> Result<SpotifyTokens> {
        let body = [
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
        ];
        let mut tokens = self
            .request_tokens(&body, Some(current.refresh_token.as_str()))
            .await?;
        if tokens.refresh_token.is_empty() {
            tokens.refresh_token = current.refresh_token.clone();
        }
        Ok(tokens)
    }

    async fn request_tokens(
        &self,
        body: &[(&str, &str)],
        fallback_refresh: Option<&str>,
    ) -> Result<SpotifyTokens> {
        let response = self
            .http
            .post(TOKEN_URL)
            .header("Authorization", self.basic_auth().as_str())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(spotify_http_error(status, &text));
        }
        let parsed: TokenResponse = serde_json::from_str(&text)?;
        Ok(tokens_from_response(parsed, fallback_refresh, now_unix()))
    }

    async fn fetch_currently_playing(&self, access_token: &str) -> Result<Playback> {
        let response = self
            .http
            .get(CURRENTLY_PLAYING_URL)
            .query(&[("additional_types", "track,episode")])
            .bearer_auth(access_token)
            .send()
            .await?;
        let status = response.status();
        if status.as_u16() == 204 {
            return Ok(Playback::Idle);
        }
        let text = response.text().await?;
        if status.as_u16() == 401 {
            return Err(AppError::spotify("401 unauthorized"));
        }
        if !status.is_success() {
            return Err(spotify_http_error(status, &text));
        }
        if text.trim().is_empty() {
            return Ok(Playback::Idle);
        }
        let parsed: CurrentlyPlayingResponse = serde_json::from_str(&text)?;
        Ok(playback_from_response(parsed))
    }

    async fn download_image(&self, url: &str) -> Result<Vec<u8>> {
        let response = self.http.get(url).send().await?;
        if !response.status().is_success() {
            return Err(AppError::spotify(format_compact!(
                "Failed to download album art ({})",
                response.status()
            )));
        }
        Ok(response.bytes().await?.to_vec())
    }

    fn basic_auth(&self) -> CompactString {
        format_compact!(
            "Basic {}",
            STANDARD.encode(format_compact!("{}:{}", self.client_id, self.client_secret))
        )
    }
}

fn spotify_http_error(status: reqwest::StatusCode, body: &str) -> AppError {
    #[derive(Deserialize)]
    struct ApiError {
        error: Option<ApiErrorBody>,
        error_description: Option<CompactString>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ApiErrorBody {
        Object {
            message: Option<CompactString>,
        },
        Code(CompactString),
    }
    let parsed = serde_json::from_str::<ApiError>(body).ok();
    let message = parsed.as_ref().and_then(|error| match &error.error {
        Some(ApiErrorBody::Object { message }) => message.clone(),
        Some(ApiErrorBody::Code(code)) => Some(code.clone()),
        None => None,
    });
    let description = parsed.and_then(|error| error.error_description);
    AppError::spotify(format_compact!(
        "{status}: {} {}",
        message.unwrap_or_default(),
        description.unwrap_or_default()
    ))
}

fn tokens_from_response(
    response: TokenResponse,
    fallback_refresh: Option<&str>,
    now: i64,
) -> SpotifyTokens {
    SpotifyTokens {
        access_token: response.access_token,
        refresh_token: response
            .refresh_token
            .or_else(|| fallback_refresh.map(CompactString::from))
            .unwrap_or_default(),
        expires_at: now + response.expires_in,
        token_type: response.token_type,
        scope: response.scope.unwrap_or_default(),
    }
}

pub fn playback_from_response(response: CurrentlyPlayingResponse) -> Playback {
    let Some(item) = response.item else {
        return Playback::Idle;
    };
    let title = item
        .name
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| CompactString::from("Unknown title"));
    let artist = item
        .artists
        .as_ref()
        .map(|artists| {
            let names: SmallVec<[&str; 4]> = artists
                .iter()
                .filter_map(|artist| artist.name.as_deref())
                .collect();
            CompactString::from(names.join(", "))
        })
        .filter(|joined| !joined.is_empty())
        .or_else(|| item.show.and_then(|show| show.name))
        .unwrap_or_else(|| CompactString::from("Unknown artist"));

    let album_name = item
        .album
        .as_ref()
        .and_then(|album| album.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| CompactString::from("Unknown album"));
    let mut images = item
        .album
        .and_then(|album| album.images)
        .or(item.images)
        .unwrap_or_else(SmallVec::new);
    images.sort_by_key(|image| std::cmp::Reverse(image.width.unwrap_or(0)));
    let album_art_url = images.into_iter().find_map(|image| image.url);

    Playback::Playing(NowPlaying {
        title,
        artist,
        album: album_name,
        album_art_url,
        progress_ms: response.progress_ms.unwrap_or(0),
        duration_ms: item.duration_ms.unwrap_or(0),
        is_playing: response.is_playing.unwrap_or(false),
        spotify_url: item.external_urls.and_then(|urls| urls.spotify),
    })
}

fn pkce_verifier() -> CompactString {
    let mut bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut bytes);
    CompactString::from(URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge(verifier: &str) -> CompactString {
    let digest = Sha256::digest(verifier.as_bytes());
    CompactString::from(URL_SAFE_NO_PAD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_currently_playing_track() {
        let json = r#"{
            "is_playing": true,
            "progress_ms": 83000,
            "item": {
                "name": "Midnight City",
                "duration_ms": 243000,
                "artists": [{"name": "M83"}],
                "album": {"name": "Hurry Up, We're Dreaming", "images": [
                    {"url": "https://i.scdn.co/large.jpg", "width": 640},
                    {"url": "https://i.scdn.co/small.jpg", "width": 64}
                ]},
                "external_urls": {"spotify": "https://open.spotify.com/track/abc"}
            }
        }"#;
        let parsed: CurrentlyPlayingResponse = serde_json::from_str(json).unwrap();
        match playback_from_response(parsed) {
            Playback::Playing(now) => {
                assert_eq!(now.title, "Midnight City");
                assert_eq!(now.artist, "M83");
                assert_eq!(now.album, "Hurry Up, We're Dreaming");
                assert_eq!(now.progress_ms, 83000);
                assert_eq!(now.duration_ms, 243000);
                assert!(now.is_playing);
                assert_eq!(
                    now.album_art_url.as_deref(),
                    Some("https://i.scdn.co/large.jpg")
                );
            }
            Playback::Idle => panic!("expected playing"),
        }
    }

    #[test]
    fn empty_item_is_idle() {
        let parsed: CurrentlyPlayingResponse =
            serde_json::from_str(r#"{"is_playing": false}"#).unwrap();
        assert_eq!(playback_from_response(parsed), Playback::Idle);
    }

    #[test]
    fn refresh_keeps_existing_refresh_token() {
        let response = TokenResponse {
            access_token: "new-access".into(),
            token_type: "Bearer".into(),
            scope: Some("user-read-currently-playing".into()),
            expires_in: 3600,
            refresh_token: None,
        };
        let tokens = tokens_from_response(response, Some("old-refresh"), 1_000);
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token, "old-refresh");
        assert_eq!(tokens.expires_at, 4_600);
    }
}
