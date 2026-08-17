use std::io::Cursor;

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use better_default::Default;
use compact_str::{format_compact, CompactString};
use derive_more::{Display, Error};
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

use palette::{FromColor, OklabHue, Oklch, Srgb};

pub const CARD_WIDTH: u32 = 1200;
pub const CARD_HEIGHT: u32 = 528;

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/GoNotoCurrent-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/fonts/GoNotoCurrent-Bold.ttf");
const FONT_EMOJI: &[u8] = include_bytes!("../assets/fonts/NotoEmoji.ttf");

struct FontStack<'a> {
    regular: FontRef<'a>,
    bold: FontRef<'a>,
    emoji: FontRef<'a>,
}

impl FontStack<'static> {
    fn load() -> Result<Self> {
        Ok(Self {
            regular: FontRef::try_from_slice(FONT_REGULAR).map_err(|_| RenderError::Font)?,
            bold: FontRef::try_from_slice(FONT_BOLD).map_err(|_| RenderError::Font)?,
            emoji: FontRef::try_from_slice(FONT_EMOJI).map_err(|_| RenderError::Font)?,
        })
    }
}

fn is_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{200c}'
            | '\u{200d}'
            | '\u{20e3}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0100}'..='\u{e01ef}'
            | '\u{1f3fb}'..='\u{1f3ff}'
    )
}

impl FontStack<'_> {
    fn has(font: &FontRef<'_>, ch: char) -> bool {
        font.glyph_id(ch).0 != 0
    }

    fn pick(&self, ch: char, bold: bool) -> &FontRef<'_> {
        if bold && Self::has(&self.bold, ch) {
            &self.bold
        } else if Self::has(&self.regular, ch) {
            &self.regular
        } else {
            &self.emoji
        }
    }

    fn first_ink_left(&self, text: &str, size: f32, bold: bool) -> f32 {
        let Some(ch) = text.chars().find(|ch| !is_format_char(*ch)) else {
            return 0.0;
        };
        let scale = PxScale::from(size);
        let font = self.pick(ch, bold);
        let id = font.glyph_id(ch);
        let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
        if let Some(outlined) = font.outline_glyph(glyph) {
            return outlined.px_bounds().min.x;
        }
        font.as_scaled(scale).h_side_bearing(id)
    }

    fn measure(&self, text: &str, size: f32, bold: bool) -> i32 {
        let scale = PxScale::from(size);
        let mut width = 0.0f32;
        for ch in text.chars().filter(|ch| !is_format_char(*ch)) {
            let font = self.pick(ch, bold);
            width += font.as_scaled(scale).h_advance(font.glyph_id(ch));
        }
        width.ceil() as i32
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        img: &mut RgbaImage,
        color: Rgba<u8>,
        x: i32,
        y: i32,
        size: f32,
        bold: bool,
        text: &str,
    ) {
        let scale = PxScale::from(size);
        let ascent = self.regular.as_scaled(scale).ascent();
        let mut caret = x as f32 - self.first_ink_left(text, size, bold);
        for ch in text.chars().filter(|ch| !is_format_char(*ch)) {
            let font = self.pick(ch, bold);
            let scaled = font.as_scaled(scale);
            let id = font.glyph_id(ch);
            let glyph = id.with_scale_and_position(scale, point(caret, y as f32 + ascent));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|px, py, coverage| {
                    if coverage > 0.0 {
                        blend(
                            img,
                            bounds.min.x as i32 + px as i32,
                            bounds.min.y as i32 + py as i32,
                            color,
                            coverage,
                        );
                    }
                });
            }
            caret += scaled.h_advance(id);
        }
    }
}

fn color_oklch(l: f32, chroma: f32, hue: f32) -> Rgba<u8> {
    let srgb = Srgb::<f32>::from_color(Oklch::new(l, chroma, OklabHue::from_degrees(hue)));
    let srgb = srgb.into_format::<u8>();
    Rgba([srgb.red, srgb.green, srgb.blue, 255])
}

fn oklch_from_rgba(color: Rgba<u8>) -> Oklch {
    Oklch::from_color(Srgb::new(color[0], color[1], color[2]).into_format::<f32>())
}

fn rgba_from_oklch(color: Oklch) -> Rgba<u8> {
    let srgb = Srgb::<f32>::from_color(color).into_format::<u8>();
    Rgba([srgb.red, srgb.green, srgb.blue, 255])
}

struct Theme {
    bg: Rgba<u8>,
    white: Rgba<u8>,
    muted: Rgba<u8>,
    faint: Rgba<u8>,
    track: Rgba<u8>,
    art_fallback: Rgba<u8>,
    default_accent: Rgba<u8>,
}

impl Theme {
    fn new() -> Self {
        Self {
            bg: color_oklch(0.13, 0.008, 264.0),
            white: color_oklch(0.985, 0.0, 0.0),
            muted: color_oklch(0.76, 0.012, 264.0),
            faint: color_oklch(0.62, 0.014, 264.0),
            track: color_oklch(0.41, 0.01, 264.0),
            art_fallback: color_oklch(0.23, 0.01, 264.0),
            default_accent: color_oklch(0.78, 0.18, 145.0),
        }
    }
}

fn theme() -> &'static Theme {
    static THEME: std::sync::LazyLock<Theme> = std::sync::LazyLock::new(Theme::new);
    &THEME
}

#[derive(Debug, Display, Error)]
pub enum RenderError {
    #[display("Invalid font data")]
    Font,
    #[display("Failed to encode image: {_0}")]
    Encode(#[error(not(source))] String),
}

pub type Result<T> = std::result::Result<T, RenderError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardKind {
    Playing {
        username: CompactString,
        title: CompactString,
        artist: CompactString,
        album: CompactString,
        progress_ms: u32,
        duration_ms: u32,
        is_playing: bool,
        album_art: Option<Vec<u8>>,
        avatar: Option<Vec<u8>>,
        track_url: Option<CompactString>,
    },
    Idle,
    NotLinked,
    Error {
        message: CompactString,
    },
}

impl CardKind {
    fn album_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Playing { album_art, .. } => album_art.as_deref(),
            _ => None,
        }
    }

    pub fn track_url(&self) -> Option<&str> {
        match self {
            Self::Playing { track_url, .. } => track_url.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
#[default(width: CARD_WIDTH, height: CARD_HEIGHT, jpeg_quality: 90)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub jpeg_quality: u8,
}

#[derive(Clone, Copy, Debug, Default)]
#[default(pad_x: 40, pad_top: 40, pad_bottom: 40, radius: 18, gap: 36, header: 56, header_gap: 32, art: 360)]
struct Layout {
    pad_x: i32,
    pad_top: i32,
    pad_bottom: i32,
    radius: u32,
    gap: i32,
    header: i32,
    header_gap: i32,
    art: u32,
}

impl Layout {
    fn art(&self, _height: u32) -> u32 {
        self.art
    }

    fn art_origin(&self, _height: u32) -> (i32, i32) {
        (self.pad_x, self.pad_top + self.header + self.header_gap)
    }

    fn text_x(&self, height: u32) -> i32 {
        self.pad_x + self.art(height) as i32 + self.gap
    }

    fn text_right(&self, width: u32) -> i32 {
        width as i32 - self.pad_x
    }

    fn text_width(&self, width: u32, height: u32) -> i32 {
        self.text_right(width) - self.text_x(height)
    }

    fn content_bottom(&self, height: u32) -> i32 {
        height as i32 - self.pad_bottom
    }
}

struct Palette {
    accent: Rgba<u8>,
}

pub fn render_card(kind: &CardKind) -> Result<RgbaImage> {
    render_card_with(kind, &RenderOptions::default())
}

pub fn render_card_with(kind: &CardKind, options: &RenderOptions) -> Result<RgbaImage> {
    let fonts = FontStack::load()?;
    let layout = Layout::default();
    let art_size = layout.art(options.height);
    let art = decode_art(kind.album_bytes(), art_size);
    let palette = palette_from(art.as_ref());

    let mut img = RgbaImage::from_pixel(options.width, options.height, theme().bg);
    wash_background(&mut img, palette.accent);
    blit_cover(&mut img, art.as_ref(), &layout, art_size);

    let x = layout.text_x(options.height);
    let max_w = layout.text_width(options.width, options.height);
    let (_art_x, art_top) = layout.art_origin(options.height);
    let art_bottom = art_top + art_size as i32;
    let controls_y = layout.content_bottom(options.height).min(art_bottom);
    let controls_top = controls_y - CONTROL_H;

    match kind {
        CardKind::Playing {
            username,
            avatar,
            title,
            artist,
            album,
            progress_ms,
            duration_ms,
            is_playing,
            ..
        } => {
            draw_status_watermark(
                &mut img,
                *is_playing,
                palette.accent,
                options.width,
                options.height,
            );
            draw_listener_line(
                &mut img,
                &fonts,
                username,
                avatar.as_deref(),
                layout.pad_x,
                layout.pad_top,
                options.width as i32 - layout.pad_x * 2,
            );
            let budget = (controls_top - art_top).max(0);
            let fit = fit_playing_stack(&fonts, title, artist, album, max_w, budget);
            let mut y = art_top + ((budget - fit.height()).max(0) / 2);
            y = draw_text_block(
                &mut img,
                &fonts,
                true,
                title,
                theme().white,
                x,
                y,
                fit.title_size,
                max_w,
                fit.title_lines,
            );
            y += META_GAP;
            y = draw_text_block(
                &mut img,
                &fonts,
                false,
                artist,
                theme().muted,
                x,
                y,
                fit.artist_size,
                max_w,
                fit.artist_lines,
            );
            y += META_GAP;
            draw_text_block(
                &mut img,
                &fonts,
                false,
                album,
                theme().faint,
                x,
                y,
                fit.album_size,
                max_w,
                fit.album_lines,
            );
            draw_progress(
                &mut img,
                &fonts,
                *progress_ms,
                *duration_ms,
                palette.accent,
                x,
                controls_y,
                max_w,
            );
        }
        CardKind::Idle => {
            draw_status_copy(
                &mut img,
                &fonts,
                palette.accent,
                "SPOTIFY",
                "Nothing playing",
                "Start a track in Spotify, then try again.",
                x,
                art_top,
                max_w,
            );
        }
        CardKind::NotLinked => {
            draw_status_copy(
                &mut img,
                &fonts,
                palette.accent,
                "SPOTIFY",
                "Spotify isn't linked",
                "Open the bot and send /link to connect.",
                x,
                art_top,
                max_w,
            );
        }
        CardKind::Error { message } => {
            draw_status_copy(
                &mut img,
                &fonts,
                palette.accent,
                "STATUS",
                "Can't load playback",
                message,
                x,
                art_top,
                max_w,
            );
        }
    }

    Ok(img)
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|err| RenderError::Encode(err.to_string()))?;
    Ok(out.into_inner())
}

pub fn encode_jpeg(image: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = DynamicImage::ImageRgba8(image.clone()).to_rgb8();
    let mut out = Cursor::new(Vec::new());
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode_image(&rgb)
        .map_err(|err| RenderError::Encode(err.to_string()))?;
    Ok(out.into_inner())
}

pub fn example_playing_card() -> CardKind {
    example_card_ja()
}

pub fn example_card_ja() -> CardKind {
    example_card("山田", "荒城の月", "瀧廉太郎", "日本の歌曲", 264.0)
}

pub fn example_card_ru() -> CardKind {
    example_card(
        "Анна",
        "Подмосковные вечера",
        "Василий Соловьёв-Седой",
        "Песни военных лет",
        28.0,
    )
}

pub fn example_card_en() -> CardKind {
    example_card(
        "alex",
        "Greensleeves 🎵",
        "Traditional ♪",
        "English Airs ✨",
        145.0,
    )
}

pub fn example_card_lorem() -> CardKind {
    example_card(
        "lorem",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua",
        "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat",
        "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur",
        200.0,
    )
}

fn example_card(username: &str, title: &str, artist: &str, album: &str, art_hue: f32) -> CardKind {
    CardKind::Playing {
        username: CompactString::from(username),
        title: CompactString::from(title),
        artist: CompactString::from(artist),
        album: CompactString::from(album),
        progress_ms: 83_000,
        duration_ms: 243_000,
        is_playing: true,
        album_art: Some(synthetic_album_art_hue(art_hue)),
        avatar: Some(synthetic_avatar()),
        track_url: Some(CompactString::from(
            "https://open.spotify.com/track/example",
        )),
    }
}

pub fn synthetic_album_art() -> Vec<u8> {
    synthetic_album_art_hue(264.0)
}

fn synthetic_album_art_hue(hue: f32) -> Vec<u8> {
    let mut art = RgbaImage::new(256, 256);
    for y in 0..256 {
        for x in 0..256 {
            let fx = x as f32 / 255.0;
            let fy = y as f32 / 255.0;
            let t = (fx * 0.55 + fy * 0.45).clamp(0.0, 1.0);
            art.put_pixel(
                x,
                y,
                color_oklch(0.28 + 0.42 * t, 0.14 + 0.06 * (1.0 - fy), hue + 28.0 * fx),
            );
        }
    }
    encode_png(&art).expect("synthetic album art encodes")
}

pub fn synthetic_avatar() -> Vec<u8> {
    let mut art = RgbaImage::new(64, 64);
    for y in 0..64 {
        for x in 0..64 {
            let fx = x as f32 / 63.0;
            let fy = y as f32 / 63.0;
            art.put_pixel(x, y, color_oklch(0.62 + 0.18 * fx, 0.12, 250.0 + 40.0 * fy));
        }
    }
    encode_png(&art).expect("synthetic avatar encodes")
}

fn decode_art(bytes: Option<&[u8]>, size: u32) -> Option<RgbaImage> {
    let bytes = bytes?;
    let decoded = image::load_from_memory(bytes).ok()?;
    Some(
        decoded
            .resize_to_fill(size, size, FilterType::Lanczos3)
            .to_rgba8(),
    )
}

fn palette_from(art: Option<&RgbaImage>) -> Palette {
    Palette {
        accent: art.map(accent_from).unwrap_or(theme().default_accent),
    }
}

fn accent_from(art: &RgbaImage) -> Rgba<u8> {
    let mut best = theme().default_accent;
    let mut best_score = 0.0f32;
    let step = (art.width().max(8) / 8).max(1);
    for y in (0..art.height()).step_by(step as usize) {
        for x in (0..art.width()).step_by(step as usize) {
            let [r, g, b, _] = art.get_pixel(x, y).0;
            let sample = oklch_from_rgba(Rgba([r, g, b, 255]));
            let score = sample.chroma * (1.0 - (sample.l - 0.62).abs() * 1.15);
            if score > best_score {
                best_score = score;
                best = rgba_from_oklch(Oklch::new(
                    0.72,
                    (sample.chroma * 0.65 + 0.07).min(0.2),
                    sample.hue,
                ));
            }
        }
    }
    if best_score < 0.04 {
        theme().default_accent
    } else {
        best
    }
}

fn wash_background(img: &mut RgbaImage, accent: Rgba<u8>) {
    let width = img.width();
    let height = img.height();
    let bg = oklch_from_rgba(theme().bg);
    let accent = oklch_from_rgba(accent);
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;
            let falloff = (1.0 - fx).powf(1.35) * (1.0 - (fy - 0.5).abs()) * 0.28;
            let mixed = Oklch::new(
                bg.l + (accent.l - bg.l) * falloff,
                bg.chroma + (accent.chroma - bg.chroma) * falloff,
                accent.hue,
            );
            img.put_pixel(x, y, rgba_from_oklch(mixed));
        }
    }
}

fn blit_cover(img: &mut RgbaImage, art: Option<&RgbaImage>, layout: &Layout, art_size: u32) {
    let (x, y) = layout.art_origin(img.height());
    let src = art
        .cloned()
        .unwrap_or_else(|| RgbaImage::from_pixel(art_size, art_size, theme().art_fallback));
    blit_rounded(img, &src, x, y, layout.radius);
}

fn blit_rounded(dst: &mut RgbaImage, src: &RgbaImage, ox: i32, oy: i32, radius: u32) {
    let w = src.width() as f32;
    let h = src.height() as f32;
    let r = radius as f32;
    for py in 0..src.height() {
        for px in 0..src.width() {
            let alpha = rounded_alpha(px as f32 + 0.5, py as f32 + 0.5, w, h, r);
            if alpha <= 0.0 {
                continue;
            }
            let pixel = *src.get_pixel(px, py);
            blend(
                dst,
                ox + px as i32,
                oy + py as i32,
                Rgba([pixel[0], pixel[1], pixel[2], 255]),
                alpha * (f32::from(pixel[3]) / 255.0),
            );
        }
    }
}

fn rounded_alpha(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let r = radius.min(w.min(h) / 2.0);
    let qx = (x - w * 0.5).abs() - (w * 0.5 - r);
    let qy = (y - h * 0.5).abs() - (h * 0.5 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let dist = qx.min(0.0).max(qy.min(0.0)) + outside - r;
    (0.75 - dist).clamp(0.0, 1.0)
}

const CONTROL_H: i32 = 76;
const BAR_H: u32 = 12;
const META_GAP: i32 = 10;
const ELLIPSIS: &str = "…";

fn line_height(size: u32) -> i32 {
    (size as f32 * 1.16).round() as i32
}

struct FittedText {
    title_size: u32,
    artist_size: u32,
    album_size: u32,
    title_lines: usize,
    artist_lines: usize,
    album_lines: usize,
}

impl FittedText {
    fn height(&self) -> i32 {
        self.title_lines as i32 * line_height(self.title_size)
            + META_GAP
            + self.artist_lines as i32 * line_height(self.artist_size)
            + META_GAP
            + self.album_lines as i32 * line_height(self.album_size)
    }
}

fn needed_lines(
    fonts: &FontStack<'_>,
    size: u32,
    bold: bool,
    text: &str,
    max_width: i32,
    cap: usize,
) -> usize {
    wrap_lines(fonts, size as f32, bold, text, max_width, cap)
        .len()
        .max(1)
}

fn fit_playing_stack(
    fonts: &FontStack<'_>,
    title: &str,
    artist: &str,
    album: &str,
    max_width: i32,
    budget: i32,
) -> FittedText {
    const ALBUM_SIZE: u32 = 42;
    let mut best: Option<FittedText> = None;
    let mut best_key = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut title_size = 84u32;
    while title_size >= 64 {
        let mut artist_size = 50u32;
        while artist_size >= 40 {
            let title_need = needed_lines(fonts, title_size, true, title, max_width, 3);
            let artist_need = needed_lines(fonts, artist_size, false, artist, max_width, 2);
            let album_need = needed_lines(fonts, ALBUM_SIZE, false, album, max_width, 2);
            for title_lines in 1..=title_need {
                for artist_lines in 1..=artist_need {
                    for album_lines in 1..=album_need {
                        let fit = FittedText {
                            title_size,
                            artist_size,
                            album_size: ALBUM_SIZE,
                            title_lines,
                            artist_lines,
                            album_lines,
                        };
                        if fit.height() > budget {
                            continue;
                        }
                        let key = (
                            title_lines as u32,
                            title_size,
                            artist_lines as u32,
                            artist_size,
                            album_lines as u32,
                        );
                        if best.is_none() || key > best_key {
                            best_key = key;
                            best = Some(fit);
                        }
                    }
                }
            }
            artist_size -= 2;
        }
        title_size -= 2;
    }
    best.unwrap_or(FittedText {
        title_size: 64,
        artist_size: 40,
        album_size: ALBUM_SIZE,
        title_lines: 1,
        artist_lines: 1,
        album_lines: 1,
    })
}

fn draw_status_watermark(
    img: &mut RgbaImage,
    is_playing: bool,
    accent: Rgba<u8>,
    width: u32,
    height: u32,
) {
    let size = height as f32 * 0.94;
    let cx = width as f32 - size * 0.38;
    let cy = height as f32 * 0.58;
    if is_playing {
        draw_play_mark(img, cx, cy, size, accent, 0.055);
    } else {
        draw_pause_mark(img, cx, cy, size, accent, 0.055);
    }
}

fn draw_pause_mark(img: &mut RgbaImage, cx: f32, cy: f32, size: f32, color: Rgba<u8>, alpha: f32) {
    let bar_w = size * 0.18;
    let bar_h = size * 0.78;
    let gap = size * 0.16;
    let y = (cy - bar_h / 2.0).round() as i32;
    let h = bar_h.round() as u32;
    let w = bar_w.round() as u32;
    fill_rect_opacity(
        img,
        (cx - gap / 2.0 - bar_w).round() as i32,
        y,
        w,
        h,
        color,
        alpha,
    );
    fill_rect_opacity(img, (cx + gap / 2.0).round() as i32, y, w, h, color, alpha);
}

fn draw_play_mark(img: &mut RgbaImage, cx: f32, cy: f32, size: f32, color: Rgba<u8>, alpha: f32) {
    let left = cx - size * 0.30;
    let right = cx + size * 0.40;
    let top = cy - size * 0.40;
    let bottom = cy + size * 0.40;
    let min_x = left.floor() as i32;
    let max_x = right.ceil() as i32;
    let min_y = top.floor() as i32;
    let max_y = bottom.ceil() as i32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = ((py - top) / (bottom - top)).clamp(0.0, 1.0);
            let edge = left + (right - left) * (1.0 - (t * 2.0 - 1.0).abs());
            if px >= left && px <= edge {
                blend(img, x, y, color, alpha);
            }
        }
    }
}

fn fill_rect_opacity(
    img: &mut RgbaImage,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    color: Rgba<u8>,
    alpha: f32,
) {
    for dy in 0..height {
        for dx in 0..width {
            blend(img, x + dx as i32, y + dy as i32, color, alpha);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_progress(
    img: &mut RgbaImage,
    fonts: &FontStack<'_>,
    progress_ms: u32,
    duration_ms: u32,
    accent: Rgba<u8>,
    x: i32,
    y: i32,
    width: i32,
) {
    let row_top = y - CONTROL_H;
    let start = format_ms(progress_ms);
    let end = format_ms(duration_ms);
    let bar_x = x;
    let bar_w = width.max(48) as u32;
    let bar_y = row_top + 10;
    fill_rect_opacity(img, bar_x, bar_y, bar_w, BAR_H, theme().track, 1.0);

    let ratio = if duration_ms == 0 {
        0.0
    } else {
        (progress_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
    };
    let filled = ((bar_w as f32) * ratio).round() as u32;
    if filled > 0 {
        fill_rect_opacity(img, bar_x, bar_y, filled, BAR_H, accent, 1.0);
    }

    let head_w = 14u32;
    let head_h = 28u32;
    let head_x = bar_x
        + filled
            .saturating_sub(head_w / 2)
            .min(bar_w.saturating_sub(head_w)) as i32;
    let head_y = bar_y - ((head_h as i32 - BAR_H as i32) / 2);
    fill_rect_opacity(img, head_x, head_y, head_w, head_h, theme().white, 0.95);

    let time_size = 34.0;
    let end_w = fonts.measure(&end, time_size, false);
    let text_y = bar_y + BAR_H as i32 + 14;
    fonts.draw(img, theme().faint, x, text_y, time_size, false, &start);
    fonts.draw(
        img,
        theme().faint,
        x + width - end_w,
        text_y,
        time_size,
        false,
        &end,
    );
}

const AVATAR_SIZE: u32 = 56;

#[allow(clippy::too_many_arguments)]
fn draw_listener_line(
    img: &mut RgbaImage,
    fonts: &FontStack<'_>,
    username: &str,
    avatar: Option<&[u8]>,
    x: i32,
    y: i32,
    max_width: i32,
) {
    let size = 52.0;
    let suffix = " is now playing";
    let suffix_w = fonts.measure(suffix, size, false);
    let decoded = avatar.and_then(|bytes| decode_art(Some(bytes), AVATAR_SIZE));
    let (text_x, name_budget) = if let Some(src) = decoded.as_ref() {
        blit_rounded(img, src, x, y, AVATAR_SIZE / 2);
        (
            x + AVATAR_SIZE as i32 + 14,
            (max_width - AVATAR_SIZE as i32 - 14 - suffix_w).max(48),
        )
    } else {
        (x, (max_width - suffix_w).max(48))
    };
    let text_y = y + (AVATAR_SIZE as i32 - size as i32) / 2;
    let name = ellipsize(fonts, size, true, username, name_budget);
    fonts.draw(img, theme().white, text_x, text_y, size, true, &name);
    let name_w = fonts.measure(&name, size, true);
    fonts.draw(
        img,
        theme().muted,
        text_x + name_w,
        text_y,
        size,
        false,
        suffix,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_status_copy(
    img: &mut RgbaImage,
    fonts: &FontStack<'_>,
    accent: Rgba<u8>,
    label: &str,
    title: &str,
    body: &str,
    x: i32,
    y: i32,
    max_w: i32,
) {
    let mut cursor = y + 24;
    cursor = draw_text_block(img, fonts, true, label, accent, x, cursor, 28, max_w, 1);
    cursor += 18;
    cursor = draw_text_block(
        img,
        fonts,
        true,
        title,
        theme().white,
        x,
        cursor,
        52,
        max_w,
        3,
    );
    cursor += 14;
    draw_text_block(
        img,
        fonts,
        false,
        body,
        theme().muted,
        x,
        cursor,
        28,
        max_w,
        3,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_text_block(
    img: &mut RgbaImage,
    fonts: &FontStack<'_>,
    bold: bool,
    text: &str,
    color: Rgba<u8>,
    x: i32,
    y: i32,
    size: u32,
    max_width: i32,
    max_lines: usize,
) -> i32 {
    let height = line_height(size);
    let lines = wrap_lines(fonts, size as f32, bold, text, max_width, max_lines);
    for (index, line) in lines.iter().enumerate() {
        fonts.draw(
            img,
            color,
            x,
            y + index as i32 * height,
            size as f32,
            bold,
            line,
        );
    }
    y + lines.len() as i32 * height
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{30ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        if is_cjk(ch) {
            if !buf.is_empty() {
                tokens.push(std::mem::take(&mut buf));
            }
            tokens.push(ch.to_string());
        } else if ch.is_whitespace() {
            if !buf.is_empty() {
                tokens.push(std::mem::take(&mut buf));
            }
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    tokens
}

fn join_token(current: &str, token: &str) -> String {
    if current.is_empty() {
        return token.to_owned();
    }
    let prev = current.chars().last();
    let next = token.chars().next();
    if matches!((prev, next), (Some(a), Some(b)) if is_cjk(a) || is_cjk(b)) {
        format!("{current}{token}")
    } else {
        format!("{current} {token}")
    }
}

fn wrap_lines(
    fonts: &FontStack<'_>,
    size: f32,
    bold: bool,
    text: &str,
    max_width: i32,
    max_lines: usize,
) -> Vec<String> {
    let max_lines = max_lines.max(1);
    let words = tokenize(text);
    if words.is_empty() {
        return vec![String::new()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        let candidate = join_token(&current, word);
        if fonts.measure(&candidate, size, bold) <= max_width {
            current = candidate;
            index += 1;
            continue;
        }
        if !current.is_empty() {
            if lines.len() + 1 == max_lines {
                let mut rest = current;
                for extra in &words[index..] {
                    rest = join_token(&rest, extra);
                }
                lines.push(ellipsize(fonts, size, bold, &rest, max_width));
                return lines;
            }
            lines.push(std::mem::take(&mut current));
            continue;
        }
        if lines.len() + 1 == max_lines {
            let mut rest = String::new();
            for extra in &words[index..] {
                rest = join_token(&rest, extra);
            }
            lines.push(ellipsize(fonts, size, bold, &rest, max_width));
            return lines;
        }
        lines.push(ellipsize(fonts, size, bold, word, max_width));
        index += 1;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn ellipsize(fonts: &FontStack<'_>, size: f32, bold: bool, text: &str, max_width: i32) -> String {
    if fonts.measure(text, size, bold) <= max_width {
        return text.to_owned();
    }
    let mut candidate = text.trim_end().to_owned();
    while !candidate.is_empty() {
        candidate.pop();
        while candidate.ends_with(|ch: char| ch.is_whitespace() || is_format_char(ch)) {
            candidate.pop();
        }
        let with_ellipsis = format!("{candidate}{ELLIPSIS}");
        if fonts.measure(&with_ellipsis, size, bold) <= max_width {
            return with_ellipsis;
        }
    }
    ELLIPSIS.into()
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>, alpha: f32) {
    if x < 0 || y < 0 {
        return;
    }
    let (x, y) = (x as u32, y as u32);
    if x >= img.width() || y >= img.height() || alpha <= 0.0 {
        return;
    }
    let dst = img.get_pixel_mut(x, y);
    let a = alpha.clamp(0.0, 1.0);
    for i in 0..3 {
        dst.0[i] = ((1.0 - a) * f32::from(dst.0[i]) + a * f32::from(color.0[i])).round() as u8;
    }
}

pub fn format_ms(ms: u32) -> CompactString {
    let total = ms / 1000;
    let minutes = total / 60;
    let seconds = total % 60;
    format_compact!("{minutes}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0x89, b'P', b'N', b'G'])
    }

    #[test]
    fn renders_playing_and_idle_cards() {
        let playing = render_card(&example_playing_card()).unwrap();
        assert_eq!(playing.dimensions(), (CARD_WIDTH, CARD_HEIGHT));
        let png = encode_png(&playing).unwrap();
        assert!(png_header(&png));
        assert!(png.len() > 2_000);

        let idle = render_card(&CardKind::Idle).unwrap();
        assert_eq!(idle.dimensions(), (CARD_WIDTH, CARD_HEIGHT));
        assert!(png_header(&encode_png(&idle).unwrap()));

        let error = render_card(&CardKind::Error {
            message: CompactString::from("Spotify timed out"),
        })
        .unwrap();
        assert!(png_header(&encode_png(&error).unwrap()));
        assert!(encode_jpeg(&playing, 90).unwrap().len() > 1_000);

        let long = render_card(&CardKind::Playing {
            username: CompactString::from("swiftie"),
            title: CompactString::from(
                "The Last Great American Dynasty Of The Late Twentieth Century",
            ),
            artist: CompactString::from("Taylor Swift, Bon Iver, The National, and Friends"),
            album: CompactString::from("Folklore"),
            progress_ms: 83_000,
            duration_ms: 243_000,
            is_playing: true,
            album_art: Some(synthetic_album_art()),
            avatar: Some(synthetic_avatar()),
            track_url: None,
        })
        .unwrap();
        assert_eq!(long.dimensions(), (CARD_WIDTH, CARD_HEIGHT));
        let lorem = render_card(&example_card_lorem()).unwrap();
        assert_eq!(lorem.dimensions(), (CARD_WIDTH, CARD_HEIGHT));
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_ms(83_000), "1:23");
        assert_eq!(format_ms(243_000), "4:03");
        assert_eq!(format_ms(0), "0:00");
    }

    #[test]
    fn wraps_long_text_and_ellipsizes_only_when_needed() {
        let fonts = FontStack::load().unwrap();
        for ch in "Alex Анна 山田荒城の月Подмосковные ♪✨♥★".chars() {
            if ch.is_whitespace() {
                continue;
            }
            assert!(
                FontStack::has(fonts.pick(ch, false), ch),
                "Missing glyph for {ch} (U+{:04X})",
                ch as u32
            );
        }
        let short = wrap_lines(&fonts, 58.0, true, "Midnight City", 520, 3);
        assert_eq!(short, vec!["Midnight City".to_owned()]);

        let wrapped = wrap_lines(
            &fonts,
            58.0,
            true,
            "The Last Great American Dynasty",
            360,
            3,
        );
        assert!(wrapped.len() >= 2);
        assert!(wrapped.iter().all(|line| !line.ends_with(ELLIPSIS)));

        let overflow = wrap_lines(
            &fonts,
            58.0,
            true,
            "A Very Long Track Title That Cannot Possibly Fit On Three Wrapped Lines Without Overflow",
            280,
            2,
        );
        assert_eq!(overflow.len(), 2);
        let last = overflow.last().unwrap();
        assert!(last.ends_with(ELLIPSIS), "{last}");
        assert!(fonts.measure(last, 58.0, true) <= 280);

        let huge_word = wrap_lines(
            &fonts,
            64.0,
            true,
            "SupercalifragilisticexpialidociousWonderful",
            160,
            1,
        );
        assert_eq!(huge_word.len(), 1);
        assert!(huge_word[0].ends_with(ELLIPSIS));
        assert!(fonts.measure(&huge_word[0], 64.0, true) <= 160);

        let cjk = wrap_lines(&fonts, 58.0, true, "荒城の月は春の夜に", 120, 4);
        assert!(cjk.len() >= 2);
        assert!(cjk.iter().any(|line| line.contains('月')));
    }

    #[test]
    fn playing_stack_stays_above_seekbar() {
        let fonts = FontStack::load().unwrap();
        let layout = Layout::default();
        let max_w = layout.text_width(CARD_WIDTH, CARD_HEIGHT);
        let art_top = layout.art_origin(CARD_HEIGHT).1;
        let controls_top = layout
            .content_bottom(CARD_HEIGHT)
            .min(art_top + layout.art(CARD_HEIGHT) as i32)
            - CONTROL_H;
        let budget = controls_top - art_top;
        let fit = fit_playing_stack(
            &fonts,
            "The Last Great American Dynasty Of The Late Twentieth Century And Then Some Extra Words",
            "Taylor Swift, Bon Iver, The National, Phoebe Bridgers, and Many More Collaborators",
            "Folklore: The Long Pond Studio Sessions Deluxe Edition With Extra Notes",
            max_w,
            budget,
        );
        assert!(fit.height() <= budget, "{} > {budget}", fit.height());
        assert!(fit.title_lines >= 1 && fit.artist_lines >= 1 && fit.album_lines >= 1);
        for (text, size, bold, lines) in [
            ("The Last Great American Dynasty Of The Late Twentieth Century And Then Some Extra Words", fit.title_size, true, fit.title_lines),
            ("Taylor Swift, Bon Iver, The National, Phoebe Bridgers, and Many More Collaborators", fit.artist_size, false, fit.artist_lines),
            ("Folklore: The Long Pond Studio Sessions Deluxe Edition With Extra Notes", fit.album_size, false, fit.album_lines),
        ] {
            let wrapped = wrap_lines(&fonts, size as f32, bold, text, max_w, lines);
            assert!(wrapped.iter().all(|line| fonts.measure(line, size as f32, bold) <= max_w));
        }
    }

    #[test]
    fn accent_follows_cover_color() {
        let mut red = RgbaImage::new(32, 32);
        for pixel in red.pixels_mut() {
            *pixel = Rgba([200, 24, 40, 255]);
        }
        let accent = accent_from(&red);
        assert!(accent[0] > accent[1] && accent[0] > accent[2]);
    }
}
