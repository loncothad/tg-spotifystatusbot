use tg_spotifystatusbot_render::{encode_jpeg, render_card, CardKind, RenderOptions};

use crate::error::{AppError, Result};

pub async fn render_jpeg(kind: CardKind) -> Result<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    rayon::spawn(move || {
        let result = (|| {
            let image = render_card(&kind)?;
            encode_jpeg(&image, RenderOptions::default().jpeg_quality)
        })();
        let _ = tx.send(result);
    });
    rx.await
        .map_err(|_| AppError::render("Render worker dropped"))?
        .map_err(AppError::render)
}
