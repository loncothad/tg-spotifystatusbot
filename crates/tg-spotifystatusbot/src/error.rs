use compact_str::CompactString;
use derive_more::{Display, Error, From};

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Display, Error, From)]
pub enum AppError {
    #[display("Configuration error: {_0}")]
    Config(#[error(not(source))] CompactString),

    #[display("Database error: {_0}")]
    Database(#[error(not(source))] CompactString),

    #[display("Spotify is not linked")]
    NotLinked,

    #[display("Spotify OAuth state is invalid or expired")]
    InvalidOauthState,

    #[display("Spotify API error: {_0}")]
    Spotify(#[error(not(source))] CompactString),

    #[display("Failed to render card: {_0}")]
    Render(#[error(not(source))] CompactString),

    #[display("Invalid card URL")]
    InvalidCardUrl,

    #[display("HTTP client error: {_0}")]
    #[from]
    Http(reqwest::Error),

    #[display("JSON error: {_0}")]
    #[from]
    Json(serde_json::Error),

    #[display("I/O error: {_0}")]
    #[from]
    Io(std::io::Error),

    #[display("Telegram API error: {_0}")]
    Telegram(#[error(not(source))] CompactString),

    #[display("{_0}")]
    Message(#[error(not(source))] CompactString),
}

impl AppError {
    pub fn database(err: impl ToString) -> Self {
        Self::Database(CompactString::from(err.to_string()))
    }

    pub fn config(err: impl ToString) -> Self {
        Self::Config(CompactString::from(err.to_string()))
    }

    pub fn spotify(err: impl ToString) -> Self {
        Self::Spotify(CompactString::from(err.to_string()))
    }

    pub fn render(err: impl ToString) -> Self {
        Self::Render(CompactString::from(err.to_string()))
    }

    pub fn telegram(err: impl ToString) -> Self {
        Self::Telegram(CompactString::from(err.to_string()))
    }

    pub fn user_message(&self) -> &'static str {
        match self {
            Self::NotLinked => "Spotify isn't linked yet. Send /link to connect your account.",
            Self::InvalidOauthState => {
                "That Spotify login link expired. Send /link and try again."
            }
            Self::Spotify(_) => {
                "I couldn't reach Spotify. Play something and try again, or /link if this keeps happening."
            }
            Self::InvalidCardUrl => "That status image link is invalid or expired.",
            Self::Telegram(_) => "Telegram rejected the request. Try again in a moment.",
            _ => "Something went wrong. Try again in a moment.",
        }
    }
}
