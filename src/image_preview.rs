//! Inline image preview: decode/downscale plus Kitty PNG placement.
//!
//! ratatui-image's Kitty backend transmits uncompressed RGBA and resizes on the UI thread.
//! Over SSH that is megabytes per pane-resize and pegs CPU. We transmit a small PNG instead,
//! cap decoded pixels, encode off the input thread, and write the Kitty payload once to
//! stdout (never as a ratatui cell — that copy is what stalled herdr).

use crate::config::ImageProtocol;
use image::DynamicImage;
use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, ResizeEncodeRender};
use std::fmt::Write as _;
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// Hard file-size cap for image preview (independent of `preview_max_kib`, which is a text-read
/// bound ~1 MiB by default — too small for photos). Documented next to `image_protocol`.
pub const IMAGE_MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Decode-time pixel box. Combined with `max_alloc` so a decompression bomb fails closed.
const DECODE_MAX_DIM: u32 = 8192;
const DECODE_MAX_ALLOC: u64 = 64 * 1024 * 1024;
/// Long-edge cap after decode. 4K/12 MP still gets reduced; a 1536×1024 screenshot must stay
/// native — 800 px + Kitty upscale is what made previews look smeared.
pub const MAX_SOURCE_EDGE: u32 = 1600;
/// Long-edge / pixel cap for the PNG we actually transmit (Kitty scales it onto the placeholder
/// grid). Match [`MAX_SOURCE_EDGE`] so a HiDPI pane downscales (sharp) instead of upscaling.
const MAX_TRANSMIT_EDGE: u32 = 1600;
const MAX_TRANSMIT_PIXELS: u64 = 1600 * 1000;
/// Encoded PNG budget (before base64). Game screenshots / photos compress badly as PNG
/// (~2 MiB at 1536×1024). A 256 KiB cap still crushed them to ~500 px. Direct stdout can
/// carry a couple of megabytes once; only shrink pixels past this.
const MAX_TRANSMIT_PNG_BYTES: usize = 2560 * 1024;
/// Nominal cell aspect (width/height) used to Fit the placeholder grid. Typical terminal glyphs
/// are about twice as tall as they are wide; a probe is not required for a preview.
const CELL_ASPECT: f64 = 0.5;

const UNIT_WIDTH: CellDiffOption = CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap());
const KITTY_ID: u32 = 1;
const B64_CHUNK_CHARS: usize = 4096;

/// What the Presenter paints. Never resized on the UI thread — encode happens on the image worker.
pub enum Paint {
    Ratatui(StatefulProtocol),
    KittyPng(KittyPng),
}

impl Paint {
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Ratatui(protocol) => ResizeEncodeRender::render(protocol, area, buf),
            Self::KittyPng(png) => png.render(area, buf),
        }
    }

    /// Encode a Kitty PNG placement for tests and for the off-thread worker.
    pub fn from_kitty_png(
        img: &DynamicImage,
        cols: u16,
        rows: u16,
        is_tmux: bool,
        font_w: u16,
        font_h: u16,
    ) -> Option<Self> {
        KittyPng::encode(img, cols, rows, is_tmux, font_w, font_h).map(Self::KittyPng)
    }

    pub fn is_kitty(&self) -> bool {
        matches!(self, Self::KittyPng(_))
    }
}

/// Caption + optional decoded bitmap for ImageView. `image` is `None` when protocol is off,
/// the file is refused, or decode fails — the caption/notices still render as text.
pub struct ImagePreview {
    pub caption: String,
    pub image: Option<Arc<DynamicImage>>,
    pub notices: Vec<String>,
}

/// Resolve, bound, and optionally decode an image under `root`.
pub fn open_for_preview(
    root: &Path,
    path: &Path,
    timeout: Duration,
    decode_pixels: bool,
) -> ImagePreview {
    let denied = |caption: &str, notice: &str| ImagePreview {
        caption: caption.to_string(),
        image: None,
        notices: vec![notice.to_string()],
    };
    let Some(canonical) = crate::render::resolve_regular_file_in_root(root, path) else {
        return denied(
            "[image unavailable]",
            "image is not a regular file under the tree root",
        );
    };
    let len = std::fs::metadata(&canonical).map(|m| m.len()).unwrap_or(0);
    if len > IMAGE_MAX_BYTES {
        return denied(
            "[image exceeds size limit]",
            &format!(
                "file too large to display as image preview (max {})",
                crate::render::human_bytes(IMAGE_MAX_BYTES)
            ),
        );
    }
    if !decode_pixels {
        let dim = image::ImageReader::open(&canonical)
            .ok()
            .and_then(|reader| reader.into_dimensions().ok());
        let caption = match dim {
            Some((w, h)) => format!(
                "{} \u{d7} {} px \u{2022} {}",
                w,
                h,
                crate::render::human_bytes(len)
            ),
            None => crate::render::human_bytes(len),
        };
        return ImagePreview {
            caption,
            image: None,
            notices: Vec::new(),
        };
    }
    match decode_bounded(&canonical, timeout) {
        Ok(img) => {
            let native_w = img.width();
            let native_h = img.height();
            let img = downscale_for_preview(img);
            ImagePreview {
                caption: format!(
                    "{} \u{d7} {} px \u{2022} {}",
                    native_w,
                    native_h,
                    crate::render::human_bytes(len)
                ),
                image: Some(Arc::new(img)),
                notices: Vec::new(),
            }
        }
        Err(msg) => denied("[image error]", &msg),
    }
}

/// Build a graphics picker after the alternate screen is up. `Off` returns `None` so ImageView
/// never decodes pixels. Auto uses the terminal probe; if the probe is silent (typical inside
/// a herdr pane over SSH) env falls back to Kitty for Kitty/Ghostty/**herdr**, never WezTerm.
pub fn init_picker(configured: ImageProtocol) -> Option<Picker> {
    if matches!(configured, ImageProtocol::Off) {
        return None;
    }
    // Never call `Picker::from_query_stdio` after ratatui has taken the terminal: its helper
    // thread enable/disable-raw-modes and can leave stdin cooked, so `q`/`a` never arrive on
    // a pty (cli_smoke, e2e_annotations) or a herdr pane. The probe is also silent over SSH.
    // Protocol comes from config + env; font size is the crate default (10×20).
    let mut picker = Picker::halfblocks();
    match configured {
        ImageProtocol::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        ImageProtocol::Sixel => picker.set_protocol_type(ProtocolType::Sixel),
        ImageProtocol::Halfblocks => {}
        ImageProtocol::Auto if prefer_kitty_from_env(|k| std::env::var(k).ok()) => {
            picker.set_protocol_type(ProtocolType::Kitty);
        }
        ImageProtocol::Auto => {}
        _ => {}
    }
    Some(picker)
}

pub(crate) fn prefer_kitty_from_env(get: impl Fn(&str) -> Option<String>) -> bool {
    let nonempty = |key: &str| get(key).is_some_and(|v| !v.is_empty());
    // Graphics-capability queries often never come back through a herdr pane / SSH, so Auto
    // would otherwise stick on unicode halfblocks (a mosaic). Herdr itself is not a terminal,
    // but its panes run in the user's GUI terminal; prefer Kitty there. Force `halfblocks` if
    // you really are on a dumb tty.
    if nonempty("KITTY_WINDOW_ID") || nonempty("GHOSTTY_RESOURCES_DIR") || nonempty("HERDR_ENV") {
        return true;
    }
    let has =
        |key: &str, needle: &str| get(key).is_some_and(|v| v.to_ascii_lowercase().contains(needle));
    has("TERM", "kitty")
        || has("TERM", "ghostty")
        || has("TERM_PROGRAM", "kitty")
        || has("TERM_PROGRAM", "ghostty")
}

fn decode_bounded(path: &Path, timeout: Duration) -> Result<DynamicImage, String> {
    let path = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(decode_with_limits(&path));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(img)) => Ok(img),
        Ok(Err(msg)) => Err(msg),
        Err(_) => Err("image decode timed out".into()),
    }
}

fn decode_with_limits(path: &Path) -> Result<DynamicImage, String> {
    // One decode at a time. A timed-out job still finishes in the background; without this
    // slot, browsing photos would stack full-size JPEG decodes and pin every core.
    static DECODE_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _slot = DECODE_SLOT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut reader =
        image::ImageReader::open(path).map_err(|e| format!("could not read image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(DECODE_MAX_DIM);
    limits.max_image_height = Some(DECODE_MAX_DIM);
    limits.max_alloc = Some(DECODE_MAX_ALLOC);
    reader.limits(limits);
    reader
        .with_guessed_format()
        .map_err(|e| format!("could not read image: {e}"))?
        .decode()
        .map_err(|e| format!("could not decode image: {e}"))
}

/// Downscale a decoded image so later clones/encodes stay bounded.
pub fn downscale_for_preview(img: DynamicImage) -> DynamicImage {
    let img = if img.width() <= MAX_SOURCE_EDGE && img.height() <= MAX_SOURCE_EDGE {
        img
    } else {
        img.resize(
            MAX_SOURCE_EDGE,
            MAX_SOURCE_EDGE,
            image::imageops::FilterType::Triangle,
        )
    };
    if img.color().has_alpha() || matches!(img, DynamicImage::ImageRgb8(_)) {
        img
    } else {
        DynamicImage::ImageRgb8(img.to_rgb8())
    }
}

pub(crate) struct ImageEncodeJob {
    pub seq: u64,
    pub image: Arc<DynamicImage>,
    pub cols: u16,
    pub rows: u16,
    pub kind: ImageEncodeKind,
}

pub(crate) enum ImageEncodeKind {
    KittyPng {
        is_tmux: bool,
        font_w: u16,
        font_h: u16,
    },
    Ratatui(Picker),
}

pub(crate) fn encode_paint(job: ImageEncodeJob) -> Option<Paint> {
    if job.cols == 0 || job.rows == 0 {
        return None;
    }
    match job.kind {
        ImageEncodeKind::KittyPng {
            is_tmux,
            font_w,
            font_h,
        } => Paint::from_kitty_png(&job.image, job.cols, job.rows, is_tmux, font_w, font_h),
        ImageEncodeKind::Ratatui(picker) => {
            let img = Arc::try_unwrap(job.image).unwrap_or_else(|shared| (*shared).clone());
            let mut protocol = picker.new_resize_protocol(img);
            protocol.resize_encode(
                &Resize::Fit(None),
                Size {
                    width: job.cols,
                    height: job.rows,
                },
            );
            Some(Paint::Ratatui(protocol))
        }
    }
}

pub(crate) fn terminal_is_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux")
}

/// Kitty graphics placement: PNG payload + unicode placeholders (so ratatui owns the cells).
pub struct KittyPng {
    cols: u16,
    rows: u16,
    transmit: String,
    id_color: String,
    id_extra: u16,
    transmitted: bool,
}

impl KittyPng {
    fn encode(
        img: &DynamicImage,
        max_cols: u16,
        max_rows: u16,
        is_tmux: bool,
        font_w: u16,
        font_h: u16,
    ) -> Option<Self> {
        let cell_aspect = f64::from(font_w.max(1)) / f64::from(font_h.max(1));
        let (cols, rows) =
            placeholder_size(img.width(), img.height(), max_cols, max_rows, cell_aspect);
        // Ignore the guessed cell-pixel box (default 10×20). Underestimating that on a HiDPI
        // pane sent too few pixels and Kitty upscaled them into mush. Cap the bitmap instead;
        // Kitty downscales onto the placeholder grid.
        let (mut px_w, mut px_h) = fit_transmit_px(img.width(), img.height());
        let mut png = Vec::new();
        for attempt in 0..3 {
            let fitted = (img.width() != px_w || img.height() != px_h)
                .then(|| img.resize_exact(px_w, px_h, image::imageops::FilterType::Triangle));
            png = encode_png(fitted.as_ref().unwrap_or(img))?;
            if png.len() <= MAX_TRANSMIT_PNG_BYTES || attempt == 2 {
                break;
            }
            // Stay close to the budget: a 0.35 floor turned a 2 MiB screenshot into ~500 px.
            let shrink = (MAX_TRANSMIT_PNG_BYTES as f64 / png.len() as f64)
                .sqrt()
                .clamp(0.55, 0.9);
            px_w = (f64::from(px_w) * shrink).round().max(32.0) as u32;
            px_h = (f64::from(px_h) * shrink).round().max(32.0) as u32;
        }
        let transmit = kitty_transmit_png(KITTY_ID, &png, cols, rows, is_tmux);
        let [id_extra, id_r, id_g, id_b] = KITTY_ID.to_be_bytes();
        Some(Self {
            cols,
            rows,
            transmit,
            id_color: format!("\x1b[38;2;{id_r};{id_g};{id_b}m"),
            id_extra: u16::from(id_extra),
            transmitted: false,
        })
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if !self.transmitted {
            self.transmitted = true;
            // Write the APC payload directly. Putting it in a ratatui cell forced the backend
            // to clone and diff a 0.5–1 MiB symbol on the UI thread, which is what froze herdr.
            emit_kitty_payload(&self.transmit);
            self.transmit = String::new();
        }
        render_placeholders(
            placement_rect(area, self.cols, self.rows),
            self.cols,
            self.rows,
            buf,
            &self.id_color,
            self.id_extra,
            None,
        );
    }
}

fn fit_transmit_px(src_w: u32, src_h: u32) -> (u32, u32) {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);
    let scale = (f64::from(MAX_TRANSMIT_EDGE) / f64::from(src_w))
        .min(f64::from(MAX_TRANSMIT_EDGE) / f64::from(src_h))
        .min(1.0);
    let mut w = (f64::from(src_w) * scale).round().max(1.0) as u32;
    let mut h = (f64::from(src_h) * scale).round().max(1.0) as u32;
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_TRANSMIT_PIXELS {
        let shrink = (MAX_TRANSMIT_PIXELS as f64 / pixels as f64).sqrt();
        w = (f64::from(w) * shrink).round().max(1.0) as u32;
        h = (f64::from(h) * shrink).round().max(1.0) as u32;
    }
    (w, h)
}

fn placeholder_size(
    px_w: u32,
    px_h: u32,
    max_cols: u16,
    max_rows: u16,
    cell_aspect: f64,
) -> (u16, u16) {
    let img_aspect = f64::from(px_w.max(1)) / f64::from(px_h.max(1));
    let cell_aspect = if cell_aspect.is_finite() && cell_aspect > 0.0 {
        cell_aspect
    } else {
        CELL_ASPECT
    };
    let cols_over_rows = img_aspect / cell_aspect;
    let max_c = f64::from(max_cols.max(1));
    let max_r = f64::from(max_rows.max(1));
    let mut rows = max_r;
    let mut cols = rows * cols_over_rows;
    if cols > max_c {
        cols = max_c;
        rows = cols / cols_over_rows;
    }
    let cols = cols.round().clamp(1.0, max_c) as u16;
    let rows = rows.round().clamp(1.0, max_r) as u16;
    (cols, rows.min(DIACRITICS.len() as u16))
}

/// Fit `cols`×`rows` inside `area` and center the leftover space.
fn placement_rect(area: Rect, cols: u16, rows: u16) -> Rect {
    let width = cols.min(area.width);
    let height = rows.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn encode_png(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    // Fast + Sub: Adaptive tries every filter per row (CPU), and RGBA is 33% more pixels
    // for photos that have no alpha. Kitty `f=100` accepts either.
    let encoder = PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::Sub);
    if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        encoder
            .write_image(
                rgba.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
    } else {
        let rgb = img.to_rgb8();
        encoder
            .write_image(
                rgb.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
    }
    Some(out)
}

fn emit_kitty_payload(seq: &str) {
    use std::io::{IsTerminal, Write};
    if seq.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

fn kitty_transmit_png(id: u32, png: &[u8], cols: u16, rows: u16, is_tmux: bool) -> String {
    let b64 = b64_encode(png);
    let (start, escape, end) = tmux_wrap(is_tmux);
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(B64_CHUNK_CHARS)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();
    let mut data = String::with_capacity(b64.len() + chunks.len() * 48);
    for (i, chunk) in chunks.iter().enumerate() {
        data.push_str(start);
        let _ = write!(data, "{escape}_Gq=2,");
        if i == 0 {
            let _ = write!(data, "i={id},a=T,U=1,f=100,t=d,c={cols},r={rows},");
        }
        let more = u8::from(i + 1 < chunks.len());
        let _ = write!(data, "m={more};{chunk}{escape}\\");
        data.push_str(end);
    }
    data
}

fn tmux_wrap(is_tmux: bool) -> (&'static str, &'static str, &'static str) {
    if is_tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    }
}

fn render_placeholders(
    area: Rect,
    size_cols: u16,
    size_rows: u16,
    buf: &mut Buffer,
    id_color: &str,
    id_extra: u16,
    mut seq: Option<&str>,
) {
    let full_width = area.width.min(size_cols);
    if full_width == 0 {
        return;
    }
    let width_usize = usize::from(full_width);
    let row_diacritics: String =
        std::iter::repeat_n('\u{10EEEE}', width_usize.saturating_sub(1)).collect();
    let right = area.width.saturating_sub(1);
    let down = area.height.saturating_sub(1);
    let restore_cursor = format!("\x1b[u\x1b[{right}C\x1b[{down}B");
    let height = area.height.min(size_rows).min(DIACRITICS.len() as u16);
    let mut symbol = String::new();
    for y in 0..height {
        symbol.clear();
        if let Some(seq) = seq.take() {
            symbol.push_str(seq);
        }
        let _ = write!(
            symbol,
            "\x1b[s{id_color}\u{10EEEE}{}{}{}",
            diacritic(y),
            diacritic(0),
            diacritic(id_extra)
        );
        symbol.push_str(&row_diacritics);
        for x in 1..full_width {
            if let Some(cell) = buf.cell_mut((area.left() + x, area.top() + y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
        symbol.push_str(&restore_cursor);
        if let Some(cell) = buf.cell_mut((area.left(), area.top() + y)) {
            cell.set_symbol(&symbol).set_diff_option(UNIT_WIDTH);
        }
    }
}

fn b64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n =
            (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8) | u32::from(input[i + 2]);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    match input.len() - i {
        1 => {
            let n = u32::from(input[i]) << 16;
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8);
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push(T[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn diacritic(i: u16) -> char {
    char::from_u32(
        DIACRITICS
            .get(usize::from(i))
            .copied()
            .unwrap_or(DIACRITICS[0]),
    )
    .unwrap_or('\u{305}')
}

/// Kitty unicode-placeholder row/column diacritics (graphics-protocol spec table).
const DIACRITICS: [u32; 297] = [
    0x305, 0x30D, 0x30E, 0x310, 0x312, 0x33D, 0x33E, 0x33F, 0x346, 0x34A, 0x34B, 0x34C, 0x350,
    0x351, 0x352, 0x357, 0x35B, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369, 0x36A, 0x36B,
    0x36C, 0x36D, 0x36E, 0x36F, 0x483, 0x484, 0x485, 0x486, 0x487, 0x592, 0x593, 0x594, 0x595,
    0x597, 0x598, 0x599, 0x59C, 0x59D, 0x59E, 0x59F, 0x5A0, 0x5A1, 0x5A8, 0x5A9, 0x5AB, 0x5AC,
    0x5AF, 0x5C4, 0x610, 0x611, 0x612, 0x613, 0x614, 0x615, 0x616, 0x617, 0x657, 0x658, 0x659,
    0x65A, 0x65B, 0x65D, 0x65E, 0x6D6, 0x6D7, 0x6D8, 0x6D9, 0x6DA, 0x6DB, 0x6DC, 0x6DF, 0x6E0,
    0x6E1, 0x6E2, 0x6E4, 0x6E7, 0x6E8, 0x6EB, 0x6EC, 0x730, 0x732, 0x733, 0x735, 0x736, 0x73A,
    0x73D, 0x73F, 0x740, 0x741, 0x743, 0x745, 0x747, 0x749, 0x74A, 0x7EB, 0x7EC, 0x7ED, 0x7EE,
    0x7EF, 0x7F0, 0x7F1, 0x7F3, 0x816, 0x817, 0x818, 0x819, 0x81B, 0x81C, 0x81D, 0x81E, 0x81F,
    0x820, 0x821, 0x822, 0x823, 0x825, 0x826, 0x827, 0x829, 0x82A, 0x82B, 0x82C, 0x82D, 0x951,
    0x953, 0x954, 0xF82, 0xF83, 0xF86, 0xF87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17,
    0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F,
    0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1,
    0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3,
    0x1DD4, 0x1DD5, 0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5,
    0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0,
    0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC,
    0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8,
    0x2DF9, 0x2DFA, 0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB,
    0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE,
    0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38,
    0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242,
    0x1D243, 0x1D244,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_leaves_small_images_alone() {
        let img = DynamicImage::new_rgb8(32, 24);
        let out = downscale_for_preview(img);
        assert_eq!((out.width(), out.height()), (32, 24));
    }

    #[test]
    fn downscale_caps_long_edge() {
        let img = DynamicImage::new_rgb8(4000, 2000);
        let out = downscale_for_preview(img);
        assert_eq!(out.width().max(out.height()), MAX_SOURCE_EDGE);
        assert_eq!(out.width(), MAX_SOURCE_EDGE);
        assert_eq!(out.height(), MAX_SOURCE_EDGE / 2);
    }

    #[test]
    fn screenshot_sized_images_stay_native() {
        let img = downscale_for_preview(DynamicImage::new_rgb8(1536, 1024));
        assert_eq!((img.width(), img.height()), (1536, 1024));
        assert_eq!(fit_transmit_px(1229, 1037), (1229, 1037));
        assert_eq!(fit_transmit_px(1536, 1024), (1536, 1024));
    }

    #[test]
    fn game_screenshot_png_is_not_crushed_to_a_thumbnail() {
        // Photographic PNGs are ~2 MiB at this size; the old 256 KiB cap shrank them to ~500 px.
        let img = DynamicImage::ImageRgb8({
            let mut buf = image::RgbImage::new(1536, 1024);
            for (x, y, p) in buf.enumerate_pixels_mut() {
                let n = x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663);
                *p = image::Rgb([(n >> 16) as u8, (n >> 8) as u8, n as u8]);
            }
            buf
        });
        let job = ImageEncodeJob {
            seq: 1,
            image: Arc::new(img),
            cols: 100,
            rows: 30,
            kind: ImageEncodeKind::KittyPng {
                is_tmux: false,
                font_w: 10,
                font_h: 20,
            },
        };
        let Some(Paint::KittyPng(png)) = encode_paint(job) else {
            panic!("expected kitty png paint");
        };
        assert!(
            png.cols >= 80 && png.rows >= 20,
            "placeholder {}x{} too small",
            png.cols,
            png.rows
        );
        // Native 1536×1024 PNG of noise is large but must still be sent, not downscaled away.
        assert!(
            png.transmit.len() > 200 * 1024,
            "expected a large native payload, got {}",
            png.transmit.len()
        );
        assert!(
            png.transmit.len() < 4 * 1024 * 1024,
            "transmit {} bytes",
            png.transmit.len()
        );
    }

    #[test]
    fn placement_rect_centers_a_smaller_image() {
        let area = Rect {
            x: 10,
            y: 2,
            width: 80,
            height: 20,
        };
        let dest = placement_rect(area, 40, 10);
        assert_eq!(dest.x, 30);
        assert_eq!(dest.y, 7);
        assert_eq!(dest.width, 40);
        assert_eq!(dest.height, 10);
    }

    #[test]
    fn placeholder_size_fits_a_square_icon_inside_a_wide_pane() {
        let (cols, rows) = placeholder_size(256, 256, 80, 24, 0.5);
        assert!(cols <= 80);
        assert!(rows <= 24);
        assert_eq!(cols, (rows as f64 * 2.0).round() as u16);
        assert!(
            cols < 80,
            "square icon must not stretch to the full pane width"
        );
    }

    #[test]
    fn kitty_transmit_is_png_not_rgba() {
        let img = DynamicImage::new_rgb8(64, 32);
        let job = ImageEncodeJob {
            seq: 1,
            image: Arc::new(img),
            cols: 80,
            rows: 24,
            kind: ImageEncodeKind::KittyPng {
                is_tmux: false,
                font_w: 10,
                font_h: 20,
            },
        };
        let Some(Paint::KittyPng(png)) = encode_paint(job) else {
            panic!("expected kitty png paint");
        };
        assert!(
            png.transmit.contains("f=100"),
            "Kitty payload must be PNG, got {}",
            png.transmit
        );
        assert!(
            !png.transmit.contains("f=32"),
            "must not send uncompressed RGBA"
        );
        assert!(png.transmit.contains("U=1"));
        assert!(png.transmit.contains("c="));
        assert!(png.transmit.contains("r="));
        // Uncompressed RGBA for 64x32 would be 8 KiB; PNG+base64 of a solid bitmap is far smaller,
        // and must stay well under a fullscreen RGBA dump (megabytes).
        assert!(
            png.transmit.len() < 8 * 1024,
            "transmit {} bytes",
            png.transmit.len()
        );
    }

    #[test]
    fn kitty_transmit_of_a_noisy_preview_stays_under_the_pty_budget() {
        // Incompressible pixels so PNG cannot collapse to nothing; this is the class of
        // payload that used to dump hundreds of KiB through the pane and stall herdr.
        let mut buf = image::RgbImage::new(800, 500);
        for (x, y, p) in buf.enumerate_pixels_mut() {
            let n = x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663);
            *p = image::Rgb([(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        }
        let job = ImageEncodeJob {
            seq: 1,
            image: Arc::new(DynamicImage::ImageRgb8(buf)),
            cols: 120,
            rows: 40,
            kind: ImageEncodeKind::KittyPng {
                is_tmux: false,
                font_w: 10,
                font_h: 20,
            },
        };
        let Some(Paint::KittyPng(png)) = encode_paint(job) else {
            panic!("expected kitty png paint");
        };
        // Noise at 800×500 is ~1 MiB PNG; must stay under the 2.5 MiB budget + base64.
        assert!(
            png.transmit.len() < 3 * 1024 * 1024,
            "transmit {} bytes",
            png.transmit.len()
        );
    }

    #[test]
    fn b64_roundtrip_padding() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn auto_env_hints_prefer_kitty_in_herdr_and_ghostty() {
        assert!(prefer_kitty_from_env(
            |k| (k == "KITTY_WINDOW_ID").then(|| "1".into())
        ));
        assert!(prefer_kitty_from_env(
            |k| (k == "TERM").then(|| "xterm-ghostty".into())
        ));
        assert!(prefer_kitty_from_env(
            |k| (k == "HERDR_ENV").then(|| "1".into())
        ));
        assert!(!prefer_kitty_from_env(|k| {
            (k == "WEZTERM_EXECUTABLE").then(|| "/wez".into())
        }));
        assert!(!prefer_kitty_from_env(|k| {
            (k == "TERM").then(|| "xterm-256color".into())
        }));
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-img-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_tiny_png(path: &Path) {
        DynamicImage::new_rgb8(2, 2)
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    #[test]
    fn open_refuses_symlink_escaping_root() {
        let root = tmp("escape");
        let outside = root
            .parent()
            .unwrap()
            .join(format!("hfv-outside-{}.png", std::process::id()));
        write_tiny_png(&outside);
        let inside = root.join("photo.png");
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&inside);
            std::os::unix::fs::symlink(&outside, &inside).unwrap();
            let out = open_for_preview(&root, &inside, Duration::from_secs(2), true);
            assert!(out.image.is_none());
            assert!(out.notices.iter().any(|n| n.contains("tree root")));
            let _ = std::fs::remove_file(&outside);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_refuses_oversize_file_without_decoding() {
        let root = tmp("oversize");
        let path = root.join("huge.png");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(IMAGE_MAX_BYTES + 1).unwrap();
        let out = open_for_preview(&root, &path, Duration::from_secs(2), true);
        assert!(out.image.is_none());
        assert!(out.caption.contains("size limit"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn off_skips_pixel_decode_but_keeps_dimensions() {
        let root = tmp("off");
        let path = root.join("pic.png");
        write_tiny_png(&path);
        let out = open_for_preview(&root, &path, Duration::from_secs(2), false);
        assert!(out.image.is_none());
        assert!(out.caption.contains("2"));
        assert!(out.notices.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_fifo() {
        let root = tmp("fifo");
        let path = root.join("photo.png");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo");
        if status.success() {
            let out = open_for_preview(&root, &path, Duration::from_millis(500), true);
            assert!(out.image.is_none());
            assert!(out.notices.iter().any(|n| n.contains("tree root")));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
