//! MP4 rendering: draws each RSVP frame to an RGBA image and encodes it with
//! libx264 via ffmpeg-next.

use std::path::Path;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use anyhow::{Context as _, Result, bail};
use core::{Frame, Theme, frames};
use ffmpeg_next as ffmpeg;
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg::util::rational::Rational;
use ffmpeg::{Dictionary, Packet, codec, format, frame};
use image::{Rgba, RgbaImage};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 24;
const FONT_PX: f32 = 64.0;

const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
    "/usr/share/fonts/google-noto-vf/NotoSansMono[wght].ttf",
];

/// Resolve the theme font. The generic `monospace` alias uses the fixed
/// candidate paths (deterministic output); any other family name is looked
/// up in the system fonts via fontdb, which also covers `~/.fonts`.
fn load_font(theme: &Theme) -> Result<FontArc> {
    if theme.font != core::DEFAULT_FONT {
        return load_font_by_family(&theme.font);
    }
    let mut tried = Vec::new();
    for path in FONT_CANDIDATES {
        let loaded: Result<FontArc> = (|| {
            let data = std::fs::read(path)?;
            Ok(FontArc::try_from_vec(data)?)
        })();
        match loaded {
            Ok(font) => return Ok(font),
            Err(e) => tried.push(format!("{path}: {e}")),
        }
    }
    bail!("no usable monospace font found. Tried: {}", tried.join(", "))
}

fn load_font_by_family(family: &str) -> Result<FontArc> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let id = db
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            ..Default::default()
        })
        .with_context(|| format!("theme font {family:?} is not installed"))?;
    let face = db.face(id).context("fontdb query returned an unknown face")?;
    let data = match &face.source {
        fontdb::Source::File(path) => std::fs::read(path)?,
        fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
            data.as_ref().as_ref().to_vec()
        }
    };
    let face_index = face.index;
    let font = ab_glyph::FontVec::try_from_vec_and_index(data, face_index)?;
    Ok(FontArc::from(font))
}

/// Number of video frames an RSVP frame occupies at the given wpm.
fn video_frames_for(ticks: u32, wpm: u32) -> u32 {
    let ms = ticks as f64 * 60_000.0 / wpm as f64;
    ((ms * FPS as f64 / 1000.0).round() as u32).max(1)
}

/// Draw one RSVP frame (background + word + pivot highlight + arrows).
fn draw_frame(img: &mut RgbaImage, frame: &Frame<'_>, font: &FontArc, theme: &Theme) {
    let bg = Rgba([theme.background[0], theme.background[1], theme.background[2], 255]);
    for px in img.pixels_mut() {
        *px = bg;
    }
    let Some(word) = frame.word else {
        return;
    };

    let scale = PxScale::from(FONT_PX);
    let sf = font.as_scaled(scale);
    let cw = sf.h_advance(font.glyph_id(' '));
    let ascent = sf.ascent();
    let descent = sf.descent();

    // Word x-origin so the pivot glyph straddles the horizontal center.
    let origin_x = WIDTH as f32 / 2.0 - (frame.pivot as f32 + 0.5) * cw;
    // Fixed baseline so nothing shifts between frames. ab_glyph's descent is
    // NEGATIVE (below baseline); the ascent..descent box is centered on the
    // midline, so descenders always fit inside the band.
    let baseline_y = HEIGHT as f32 / 2.0 + (ascent + descent) / 2.0;

    let text = Rgba([theme.text[0], theme.text[1], theme.text[2], 255]);
    let pivot_color = Rgba([theme.pivot[0], theme.pivot[1], theme.pivot[2], 255]);

    for (i, ch) in word.chars().enumerate() {
        let color = if i == frame.pivot { pivot_color } else { text };
        let x = origin_x + i as f32 * cw;
        let Some(glyph) = font.outline_glyph(
            font.glyph_id(ch)
                .with_scale_and_position(scale, ab_glyph::point(x, baseline_y)),
        ) else {
            continue;
        };
        // draw() emits coords relative to the glyph's pixel-box top-left.
        let rect = glyph.px_bounds();
        let (left, top) = (rect.min.x, rect.min.y);
        glyph.draw(|gx, gy, coverage| {
            let px = (left + gx as f32) as i64;
            let py = (top + gy as f32) as i64;
            if (0..WIDTH as i64).contains(&px)
                && (0..HEIGHT as i64).contains(&py)
                && coverage > 0.0
            {
                blend(img.get_pixel_mut(px as u32, py as u32), color, coverage);
            }
        });
    }
    // Arrows: pivot glyph center is always at WIDTH/2 by construction.
    let arrows = Rgba([theme.arrows[0], theme.arrows[1], theme.arrows[2], 255]);
    let cx = WIDTH / 2;
    let tri_h = 18;
    let tri_w = 14;
    let gap = 10;
    // Fixed positions relative to the font-metric text band (same visual
    // relationship as the GUI/CLI markers), identical for every frame.
    // Above the text, pointing down (apex at bottom).
    let apex_top = (baseline_y - ascent - gap as f32).max(0.) as u32;
    for dy in 0..tri_h {
        let y = apex_top - dy;
        let half = ((tri_w as u32) * dy / tri_h).max(1) as i64;
        for dx in -half..=half {
            let x = cx as i64 + dx;
            if (0..WIDTH as i64).contains(&x) {
                *img.get_pixel_mut(x as u32, y) = arrows;
            }
        }
    }
    // Below the text, pointing up (apex at top).
    let apex_bot = (baseline_y - descent + gap as f32).min((HEIGHT - tri_h - 1) as f32) as u32;
    for dy in 0..tri_h {
        let y = apex_bot + dy;
        let half = ((tri_w as u32) * dy / tri_h).max(1) as i64;
        for dx in -half..=half {
            let x = cx as i64 + dx;
            if (0..WIDTH as i64).contains(&x) && y < HEIGHT {
                *img.get_pixel_mut(x as u32, y) = arrows;
            }
        }
    }
}

fn blend(dst: &mut Rgba<u8>, color: Rgba<u8>, coverage: f32) {
    let a = coverage.min(1.0);
    for c in 0..3 {
        dst[c] = (dst[c] as f32 * (1.0 - a) + color[c] as f32 * a) as u8;
    }
}

/// Render `book_text` to an mp4 at `out`.
pub fn render_video(book_text: &str, out: &Path, wpm: u32, theme: &Theme) -> Result<()> {
    anyhow::ensure!(wpm > 0, "wpm must be positive");
    let font = load_font(theme)?;

    ffmpeg::init().map_err(|e| anyhow::anyhow!("ffmpeg init failed: {e}"))?;

    let mut octx = format::output(out)
        .with_context(|| format!("failed to open output {}", out.display()))?;

    let codec = codec::encoder::find_by_name("libx264")
        .context("libx264 encoder not available in this FFmpeg build")?;
    let mut stream = octx.add_stream(codec)?;

    let mut ctx = codec::context::Context::new_with_codec(codec);
    ctx.set_time_base(Rational(1, FPS as i32));
    ctx.set_frame_rate(Some(Rational(FPS as i32, 1)));

    let mut opts = Dictionary::new();
    opts.set("preset", "medium");
    opts.set("crf", "20");
    let mut encoder = ctx
        .encoder()
        .video()
        .and_then(|mut e| {
            e.set_width(WIDTH);
            e.set_height(HEIGHT);
            e.set_format(Pixel::YUV420P);
            e.set_gop(FPS * 2);
            e.open_with(opts)
        })
        .map_err(|e| anyhow::anyhow!("failed to open libx264 encoder: {e}"))?;
    stream.set_parameters(&encoder);

    octx.write_header()
        .map_err(|e| anyhow::anyhow!("failed to write mp4 header: {e}"))?;

    // The mp4 muxer rewrites the stream timebase (typically 1/90000); packets
    // come out of the encoder in the encoder's 1/FPS timebase and must be
    // rescaled before writing.
    let enc_time_base = encoder.time_base();
    let ost_time_base = octx.stream(0).context("no stream 0")?.time_base();

    let mut scaler = Scaler::get(
        Pixel::RGBA,
        WIDTH,
        HEIGHT,
        Pixel::YUV420P,
        WIDTH,
        HEIGHT,
        Flags::BILINEAR,
    )
    .map_err(|e| anyhow::anyhow!("sws init failed: {e}"))?;

    let mut img = RgbaImage::new(WIDTH, HEIGHT);
    let mut rgba = frame::Video::new(Pixel::RGBA, WIDTH, HEIGHT);
    let mut yuv = frame::Video::new(Pixel::YUV420P, WIDTH, HEIGHT);
    let mut packet = Packet::empty();
    let mut pts: i64 = 0;

    for frame in frames(book_text) {
        draw_frame(&mut img, &frame, &font, theme);
        {
            // Copy RGBA image into the ffmpeg frame (stride may differ).
            let stride = rgba.stride(0);
            let data = rgba.data_mut(0);
            for y in 0..HEIGHT {
                let row = &img.as_raw()[y as usize * WIDTH as usize * 4..][..WIDTH as usize * 4];
                data[y as usize * stride..][..row.len()].copy_from_slice(row);
            }
        }
        let repeats = video_frames_for(frame.ticks, wpm);
        for _ in 0..repeats {
            scaler
                .run(&rgba, &mut yuv)
                .map_err(|e| anyhow::anyhow!("sws scale failed: {e}"))?;
            yuv.set_pts(Some(pts));
            unsafe { (*yuv.as_mut_ptr()).duration = 1 };
            encoder
                .send_frame(&yuv)
                .map_err(|e| anyhow::anyhow!("encoder send failed: {e}"))?;
            drain_packets(&mut encoder, &mut octx, &mut packet, enc_time_base, ost_time_base)?;
            pts += 1;
        }
    }

    encoder
        .send_eof()
        .map_err(|e| anyhow::anyhow!("encoder eof failed: {e}"))?;
    drain_packets(&mut encoder, &mut octx, &mut packet, enc_time_base, ost_time_base)?;

    octx.write_trailer()
        .map_err(|e| anyhow::anyhow!("failed to write mp4 trailer: {e}"))?;
    Ok(())
}

fn drain_packets(
    encoder: &mut ffmpeg::codec::encoder::Video,
    octx: &mut format::context::Output,
    packet: &mut Packet,
    enc_time_base: Rational,
    ost_time_base: Rational,
) -> Result<()> {
    while encoder.receive_packet(packet).is_ok() {
        packet.set_stream(0);
        packet.rescale_ts(enc_time_base, ost_time_base);
        packet.write(octx).map_err(|e| anyhow::anyhow!("packet write failed: {e}"))?;
    }
    Ok(())
}
