use std::time::Duration;

use frankenstein::client_reqwest::Bot;
use tracing_subscriber::EnvFilter;

use tg_spotifystatusbot::config::Config;
use tg_spotifystatusbot::db::Store;
use tg_spotifystatusbot::http::{serve, AppState};
use tg_spotifystatusbot::spotify::SpotifyClient;
use tg_spotifystatusbot::telegram;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    if let Err(err) = run().await {
        tracing::error!(error = %err, "Fatal error");
        std::process::exit(1);
    }
}

async fn run() -> tg_spotifystatusbot::error::Result<()> {
    let config = Config::from_env()?;
    let store = Store::open(&config.redb_path)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.http.request_timeout_secs))
        .user_agent("tg-spotifystatusbot/0.1")
        .build()?;
    let spotify = SpotifyClient::new(&config, http);
    let bot = Bot::new(&config.telegram_bot_token);
    let state = AppState {
        config,
        store,
        spotify,
        bot,
    };

    let http_state = state.clone();
    let http_task = tokio::spawn(async move {
        if let Err(err) = serve(http_state).await {
            tracing::error!(error = %err, "HTTP server stopped");
        }
    });

    let bot_result = telegram::run(state).await;
    http_task.abort();
    bot_result
}
