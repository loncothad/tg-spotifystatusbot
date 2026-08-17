use std::path::PathBuf;
use std::time::Duration;

use compact_str::{format_compact, CompactString};
use frankenstein::client_reqwest::Bot;
use frankenstein::inline_mode::{InlineQueryResult, InlineQueryResultPhoto};
use frankenstein::input_file::{FileUpload, InputFile};
use frankenstein::methods::{
    AnswerInlineQueryParams, GetUpdatesParams, SendMessageParams, SendPhotoParams,
    SetMyCommandsParams,
};
use frankenstein::types::{
    AllowedUpdate, BotCommand, InlineKeyboardButton, InlineKeyboardMarkup, LinkPreviewOptions,
    Message, ReplyMarkup,
};
use frankenstein::updates::UpdateContent;
use frankenstein::AsyncTelegramApi;
use smallvec::SmallVec;
use crate::render::render_jpeg;
use uuid::Uuid;

use crate::card_url::card_path;
use crate::error::{AppError, Result};
use crate::http::AppState;

const HELP_TEXT: &str = "\
I share a live image of what you're playing on Spotify.

/link — connect your Spotify account
/unlink — forget stored tokens
/now — send a fresh now-playing card here
/privacy — what this bot stores
/help — this message

Admins: /allow, /deny, /allowlist

Inline: type @your_bot in any chat. The query text is ignored; you always get your current track.";

const PRIVACY_TEXT: &str = "\
This bot stores your Telegram user id and Spotify OAuth tokens (access token, refresh token, expiry) in a local redb file so it can read your currently playing track.

It does not store listening history, generated images, or Spotify API responses. Every /now and inline query fetches Spotify and renders a new card.

Unlink with /unlink to delete your tokens. Inline image URLs are HMAC-signed and short-lived; Telegram fetching them still renders live, with no result cache.";

const START_TEXT: &str = "\
Share a live Spotify now-playing card in any chat.

Use /link to connect Spotify, then /now or mention this bot inline. Cards are rendered fresh every time — nothing is cached.";

pub async fn run(state: AppState) -> Result<()> {
    register_commands(&state.bot).await?;
    let mut offset: i64 = 0;
    tracing::info!("Telegram long polling started");

    loop {
        let params = GetUpdatesParams::builder()
            .offset(offset)
            .timeout(30)
            .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::InlineQuery])
            .build();

        match state.bot.get_updates(&params).await {
            Ok(response) => {
                for update in response.result {
                    offset = i64::from(update.update_id) + 1;
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_update(&state, update.content).await {
                            tracing::warn!(error = %err, "Failed to handle update");
                        }
                    });
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "getUpdates failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn register_commands(bot: &Bot) -> Result<()> {
    let commands: SmallVec<[BotCommand; 8]> = [
        ("start", "Start and see how to link Spotify"),
        ("link", "Connect your Spotify account"),
        ("unlink", "Remove stored Spotify tokens"),
        ("now", "Send a fresh now-playing card"),
        ("help", "How the bot works"),
        ("privacy", "What data is stored"),
        ("allow", "Admin: allow a Telegram user id"),
        ("deny", "Admin: remove a user from the allowlist"),
        ("allowlist", "Admin: show allowed user ids"),
    ]
    .into_iter()
    .map(|(command, description)| {
        BotCommand::builder()
            .command(command)
            .description(description)
            .build()
    })
    .collect();

    bot.set_my_commands(
        &SetMyCommandsParams::builder()
            .commands(commands.to_vec())
            .build(),
    )
    .await
    .map_err(AppError::telegram)?;
    Ok(())
}

async fn handle_update(state: &AppState, content: UpdateContent) -> Result<()> {
    match content {
        UpdateContent::Message(message) => handle_message(state, *message).await,
        UpdateContent::InlineQuery(query) => handle_inline_query(state, query).await,
        _ => Ok(()),
    }
}

async fn handle_message(state: &AppState, message: Message) -> Result<()> {
    let Some(text) = message.text.as_deref() else {
        return Ok(());
    };
    let Some(from) = message.from.as_ref() else {
        return Ok(());
    };
    if from.is_bot {
        return Ok(());
    }

    let (command, rest) = split_command(text);
    match command {
        "/start" => send_start(state, message.chat.id, from.id).await,
        "/help" => send_text(state, message.chat.id, HELP_TEXT).await,
        "/privacy" => send_text(state, message.chat.id, PRIVACY_TEXT).await,
        "/allow" | "/deny" | "/allowlist" => {
            handle_admin_command(state, &message, from.id, command, rest).await
        }
        "/link" => {
            if !ensure_allowed(state, message.chat.id, from.id).await? {
                return Ok(());
            }
            send_link(state, message.chat.id, from.id).await
        }
        "/unlink" => {
            if !ensure_allowed(state, message.chat.id, from.id).await? {
                return Ok(());
            }
            let removed = state.spotify.unlink(&state.store, from.id).await?;
            let text = if removed {
                "Your Spotify tokens were deleted."
            } else {
                "Nothing was stored for your account."
            };
            send_text(state, message.chat.id, text).await
        }
        "/now" => {
            if !ensure_allowed(state, message.chat.id, from.id).await? {
                return Ok(());
            }
            send_now(state, message.chat.id, from.id).await
        }
        _ => Ok(()),
    }
}

async fn can_use(state: &AppState, user_id: u64) -> Result<bool> {
    Ok(state.config.is_admin(user_id) || state.store.is_allowlisted(user_id).await?)
}

async fn ensure_allowed(state: &AppState, chat_id: i64, user_id: u64) -> Result<bool> {
    if can_use(state, user_id).await? {
        return Ok(true);
    }
    send_text(
        state,
        chat_id,
        "This bot is private. Ask an admin to allow your Telegram user id with /allow.",
    )
    .await?;
    Ok(false)
}

fn target_user_id(message: &Message, rest: &str) -> Option<u64> {
    let trimmed = rest.trim();
    if let Ok(id) = trimmed.parse::<u64>() {
        return Some(id);
    }
    message
        .reply_to_message
        .as_ref()
        .and_then(|replied| replied.from.as_ref())
        .map(|user| user.id)
}

async fn handle_admin_command(
    state: &AppState,
    message: &Message,
    from_id: u64,
    command: &str,
    rest: &str,
) -> Result<()> {
    if !state.config.is_admin(from_id) {
        return send_text(state, message.chat.id, "That command is for admins only.").await;
    }
    match command {
        "/allow" => match target_user_id(message, rest) {
            Some(user_id) => {
                let added = state.store.allow_user(user_id).await?;
                let text = if added {
                    format_compact!("{user_id} is now allowlisted.")
                } else {
                    format_compact!("{user_id} was already allowlisted.")
                };
                send_text(state, message.chat.id, &text).await
            }
            None => {
                send_text(
                    state,
                    message.chat.id,
                    "Usage: /allow <telegram_user_id> or reply to a user with /allow",
                )
                .await
            }
        },
        "/deny" => match target_user_id(message, rest) {
            Some(user_id) => {
                let removed = state.store.deny_user(user_id).await?;
                let text = if removed {
                    format_compact!("{user_id} was removed from the allowlist.")
                } else {
                    format_compact!("{user_id} was not on the allowlist.")
                };
                send_text(state, message.chat.id, &text).await
            }
            None => {
                send_text(
                    state,
                    message.chat.id,
                    "Usage: /deny <telegram_user_id> or reply to a user with /deny",
                )
                .await
            }
        },
        _ => {
            let ids = state.store.list_allowlist().await?;
            let text = if ids.is_empty() {
                CompactString::from(
                    "Allowlist is empty. Admins from ADMIN_USER_IDS can still use the bot.",
                )
            } else {
                format_compact!(
                    "Allowlist ({}):\n{}",
                    ids.len(),
                    ids.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            send_text(state, message.chat.id, &text).await
        }
    }
}

async fn send_start(state: &AppState, chat_id: i64, user_id: u64) -> Result<()> {
    let text = if can_use(state, user_id).await? {
        START_TEXT.to_owned()
    } else {
        format!("{START_TEXT}\n\nThis bot is private. Your Telegram user id is {user_id}. Ask an admin to /allow you.")
    };
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text)
        .link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
        .build();
    state
        .bot
        .send_message(&params)
        .await
        .map_err(AppError::telegram)?;
    Ok(())
}

async fn send_link(state: &AppState, chat_id: i64, user_id: u64) -> Result<()> {
    let url = state.spotify.start_link(&state.store, user_id).await?;
    let keyboard = InlineKeyboardMarkup::builder()
        .inline_keyboard(vec![vec![InlineKeyboardButton::builder()
            .text("Link Spotify")
            .url(url)
            .build()]])
        .build();
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text("Open Spotify and approve access. I only request your currently playing track.")
        .reply_markup(ReplyMarkup::InlineKeyboardMarkup(keyboard))
        .build();
    state
        .bot
        .send_message(&params)
        .await
        .map_err(AppError::telegram)?;
    Ok(())
}

async fn send_now(state: &AppState, chat_id: i64, user_id: u64) -> Result<()> {
    let kind = state.spotify.card_for_user(&state.store, user_id).await?;
    let tg_spotifystatusbot_render::CardKind::Playing { .. } = &kind else {
        return send_text(state, chat_id, &caption_for(&kind)).await;
    };
    let jpeg = render_jpeg(kind.clone()).await?;

    let path = write_temp_jpeg(&jpeg)?;
    let builder = SendPhotoParams::builder()
        .chat_id(chat_id)
        .photo(FileUpload::InputFile(InputFile { path: path.clone() }))
        .caption(caption_for(&kind));
    let params = if let Some(markup) = track_keyboard(&kind) {
        builder
            .reply_markup(ReplyMarkup::InlineKeyboardMarkup(markup))
            .build()
    } else {
        builder.build()
    };
    let result = state.bot.send_photo(&params).await;
    let _ = std::fs::remove_file(&path);
    result.map_err(AppError::telegram)?;
    Ok(())
}

async fn send_text(state: &AppState, chat_id: i64, text: &str) -> Result<()> {
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text)
        .link_preview_options(LinkPreviewOptions::builder().is_disabled(true).build())
        .build();
    state
        .bot
        .send_message(&params)
        .await
        .map_err(AppError::telegram)?;
    Ok(())
}

async fn handle_inline_query(
    state: &AppState,
    query: frankenstein::inline_mode::InlineQuery,
) -> Result<()> {
    let user_id = query.from.id;
    if !can_use(state, user_id).await? {
        let params = AnswerInlineQueryParams::builder()
            .inline_query_id(query.id)
            .results(Vec::<InlineQueryResult>::new())
            .cache_time(0)
            .is_personal(true)
            .build();
        state
            .bot
            .answer_inline_query(&params)
            .await
            .map_err(AppError::telegram)?;
        return Ok(());
    }
    let url = card_path(
        &state.config.public_base_url,
        user_id,
        &state.config.card_signing_secret,
    );
    let kind = state.spotify.card_for_user(&state.store, user_id).await?;
    let tg_spotifystatusbot_render::CardKind::Playing { title, artist, .. } = &kind else {
        let params = AnswerInlineQueryParams::builder()
            .inline_query_id(query.id)
            .results(Vec::<InlineQueryResult>::new())
            .cache_time(0)
            .is_personal(true)
            .build();
        state
            .bot
            .answer_inline_query(&params)
            .await
            .map_err(AppError::telegram)?;
        return Ok(());
    };
    let title = format_compact!("{title} — {artist}");

    let photo = InlineQueryResultPhoto::builder()
        .id(format_compact!("{}", Uuid::now_v7()))
        .photo_url(url.as_str())
        .thumbnail_url(url.as_str())
        .photo_width(tg_spotifystatusbot_render::CARD_WIDTH)
        .photo_height(tg_spotifystatusbot_render::CARD_HEIGHT)
        .title(title)
        .caption(caption_for(&kind));
    let photo = if let Some(markup) = track_keyboard(&kind) {
        photo.reply_markup(markup).build()
    } else {
        photo.build()
    };

    let params = AnswerInlineQueryParams::builder()
        .inline_query_id(query.id)
        .results(vec![InlineQueryResult::from(photo)])
        .cache_time(0)
        .is_personal(true)
        .build();
    state
        .bot
        .answer_inline_query(&params)
        .await
        .map_err(AppError::telegram)?;
    Ok(())
}

fn track_keyboard(kind: &tg_spotifystatusbot_render::CardKind) -> Option<InlineKeyboardMarkup> {
    let url = kind.track_url()?;
    Some(
        InlineKeyboardMarkup::builder()
            .inline_keyboard(vec![vec![InlineKeyboardButton::builder()
                .text("Open in Spotify")
                .url(url)
                .build()]])
            .build(),
    )
}

fn caption_for(kind: &tg_spotifystatusbot_render::CardKind) -> CompactString {
    match kind {
        tg_spotifystatusbot_render::CardKind::Playing { title, artist, .. } => {
            format_compact!("{title} — {artist}")
        }
        tg_spotifystatusbot_render::CardKind::Idle => {
            CompactString::from("Nothing is playing on Spotify right now.")
        }
        tg_spotifystatusbot_render::CardKind::NotLinked => {
            CompactString::from("Spotify isn't linked. Open the bot and send /link.")
        }
        tg_spotifystatusbot_render::CardKind::Error { message } => message.clone(),
    }
}

fn split_command(text: &str) -> (&str, &str) {
    let mut parts = text.splitn(2, char::is_whitespace);
    let raw = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    let command = raw.split('@').next().unwrap_or(raw);
    (command, rest)
}

fn write_temp_jpeg(bytes: &[u8]) -> Result<PathBuf> {
    let path = std::env::temp_dir()
        .join(format_compact!("tg-spotifystatusbot-{}.jpg", Uuid::now_v7()).as_str());
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bot_username_from_commands() {
        assert_eq!(split_command("/now@mybot extra"), ("/now", "extra"));
        assert_eq!(split_command("/help"), ("/help", ""));
    }
}
