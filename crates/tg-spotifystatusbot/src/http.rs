use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::SendMessageParams;
use frankenstein::AsyncTelegramApi;
use serde::Deserialize;

use crate::card_url::verify_card;
use crate::config::Config;
use crate::db::Store;
use crate::render::render_jpeg;
use crate::spotify::SpotifyClient;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub store: Store,
    pub spotify: SpotifyClient,
    pub bot: Bot,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/oauth/callback", get(oauth_callback))
        .route("/card/{user_id}", get(card))
        .with_state(Arc::new(state))
}

pub async fn serve(state: AppState) -> crate::error::Result<()> {
    let addr = state.config.http_bind;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "HTTP server listening");
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
struct OauthQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(query): Query<OauthQuery>,
) -> Response {
    if let Some(error) = query.error {
        tracing::warn!(error, "Spotify OAuth rejected");
        return html_page(
            StatusCode::BAD_REQUEST,
            "Link cancelled",
            "Spotify did not authorize the bot. You can close this tab and send /link again.",
        );
    }

    let (Some(code), Some(oauth_state)) = (query.code, query.state) else {
        return html_page(
            StatusCode::BAD_REQUEST,
            "Missing OAuth parameters",
            "Send /link in Telegram and try the button again.",
        );
    };

    match state
        .spotify
        .finish_link(
            &state.store,
            &code,
            &oauth_state,
            state.config.http.oauth_state_ttl_secs,
        )
        .await
    {
        Ok(user_id) => {
            let params = SendMessageParams::builder()
                .chat_id(user_id as i64)
                .text("Spotify is linked. Use /now or mention the bot inline to share what you're playing.")
                .build();
            if let Err(err) = state.bot.send_message(&params).await {
                tracing::warn!(user_id, error = %err, "Linked Spotify but failed to notify user");
            }
            html_page(
                StatusCode::OK,
                "Spotify linked",
                "You can close this tab and return to Telegram.",
            )
        }
        Err(err) => {
            tracing::warn!(error = %err, "OAuth callback failed");
            html_page(
                StatusCode::BAD_REQUEST,
                "Could not link Spotify",
                err.user_message(),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct CardQuery {
    t: i64,
    sig: String,
}

async fn card(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<CardQuery>,
) -> Response {
    let user_id = user_id.trim_end_matches(".jpg");
    let Ok(user_id) = user_id.parse::<u64>() else {
        return (StatusCode::BAD_REQUEST, "Invalid user").into_response();
    };

    if let Err(err) = verify_card(
        user_id,
        query.t,
        &query.sig,
        &state.config.card_signing_secret,
        state.config.http.card_url_ttl_secs,
    ) {
        tracing::debug!(user_id, error = %err, "Rejected card URL");
        return (StatusCode::FORBIDDEN, err.user_message()).into_response();
    }

    match render_fresh_jpeg(&state, user_id).await {
        Ok(bytes) => {
            let mut response = bytes.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, max-age=0"),
            );
            response
        }
        Err(err) => {
            tracing::warn!(user_id, error = %err, "Failed to render card");
            (StatusCode::INTERNAL_SERVER_ERROR, err.user_message()).into_response()
        }
    }
}

async fn render_fresh_jpeg(state: &AppState, user_id: u64) -> crate::error::Result<Vec<u8>> {
    let kind = state.spotify.card_for_user(&state.store, user_id).await?;
    render_jpeg(kind).await
}

fn html_page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font-family:sans-serif;background:#121212;color:#fff;display:flex;min-height:100vh;\
         align-items:center;justify-content:center}} main{{max-width:32rem}} h1{{color:#1ed760}}</style>\
         </head><body><main><h1>{title}</h1><p>{body}</p></main></body></html>"
    );
    (status, Html(html)).into_response()
}
