//! MP4 rendering: draws each RSVP frame to an RGBA image and encodes it with
//! libx264 via ffmpeg-next.

use std::path::Path;
use std::sync::LazyLock;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use anyhow::{Context as _, Result};
use core::{Frame, Theme, frames};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{context::Context as Scaler, flag::Flags};
use ffmpeg::util::rational::Rational;
use ffmpeg::{Dictionary, Packet, codec, format, frame};
use ffmpeg_next as ffmpeg;
use fontdb::Source;
use image::{Rgba, RgbaImage};
use rayon::prelude::*;

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 24;
const FONT_PX: f32 = 64.0;

/// RSVP frames drawn per rayon batch: 2x the core count so rayon's
/// work-stealing has slack for uneven glyph costs, clamped so tiny boxes
/// still fill a batch and big ones stay under ~115 MB of RGBA buffers
/// (each frame is WIDTH*HEIGHT*4).
fn draw_batch_size(cores: usize) -> usize {
    (cores * 2).clamp(4, 32)
}

/// Resolved once at first render and cached: the core count cannot change
/// at runtime, so `available_parallelism` is queried exactly once
/// (`LazyLock` handles concurrent first-calls internally).
static DRAW_BATCH: LazyLock<usize> = LazyLock::new(|| {
    draw_batch_size(
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(4),
    )
});

/// Resolve the theme font via the shared fontdb resolver ([`core::resolve_font`])
/// and load the matched face into ab_glyph.
fn load_font(theme: &Theme) -> Result<FontArc> {
    let resolved = core::resolve_font(theme)?;
    if resolved.fell_back && theme.font != core::DEFAULT_FONT {
        eprintln!(
            "warning: theme font {:?} not installed, using {:?}",
            theme.font, resolved.family
        );
    }
    let data = match &resolved.source {
        Source::File(path) => std::fs::read(path)?,
        Source::Binary(data) | Source::SharedFile(_, data) => data.as_ref().as_ref().to_vec(),
    };
    let font = ab_glyph::FontVec::try_from_vec_and_index(data, resolved.index)?;
    Ok(FontArc::from(font))
}

/// Number of video frames an RSVP frame occupies at the given wpm.
fn video_frames_for(ticks: u32, wpm: u32) -> u32 {
    let ms = ticks as f64 * 60_000.0 / wpm as f64;
    ((ms * FPS as f64 / 1000.0).round() as u32).max(1)
}

/// Draw one RSVP frame (background + word + pivot highlight + arrows).
fn draw_frame(img: &mut RgbaImage, frame: &Frame<'_>, font: &FontArc, theme: &Theme) {
    let bg = Rgba([
        theme.background[0],
        theme.background[1],
        theme.background[2],
        255,
    ]);
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
            if (0..WIDTH as i64).contains(&px) && (0..HEIGHT as i64).contains(&py) && coverage > 0.0
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

/// Parse extra ffmpeg options from a user string into (key, value) pairs.
/// Accepts ffmpeg-CLI style `-key value` pairs only, whitespace separated:
/// `-preset slow -crf 18`. Duplicate keys: last wins. `-c:v`/`-codec:v`/
/// `-vcodec` additionally selects the encoder (e.g. a hardware encoder such
/// as `h264_nvenc`); everything else is passed as an encoder/muxer option.
pub fn parse_ffmpeg_args(input: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut it = input.split_whitespace();
    while let Some(tok) = it.next() {
        anyhow::ensure!(
            tok.len() > 1 && tok.starts_with('-'),
            "unexpected token {tok:?}: expected '-key value' pairs"
        );
        let k = &tok[1..];
        let v = it
            .next()
            .with_context(|| format!("missing value for '-{k}'"))?;
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// User-arg keys that select the encoder instead of being codec options.
const CODEC_KEYS: [&str; 3] = ["c:v", "codec:v", "vcodec"];

fn is_codec_key(k: &str) -> bool {
    CODEC_KEYS.contains(&k)
}

/// Encoder name requested via `-c:v`/`-codec:v`/`-vcodec` (last wins), if any.
fn codec_override(user_args: &[(String, String)]) -> Option<&str> {
    user_args
        .iter()
        .rev()
        .find(|(k, _)| is_codec_key(k))
        .map(|(_, v)| v.as_str())
}

/// Render `book_text` to an mp4 at `out`.
pub fn render_video(
    book_text: &str,
    out: &Path,
    wpm: u32,
    theme: &Theme,
    ffmpeg_args: &str,
) -> Result<()> {
    anyhow::ensure!(wpm > 0, "wpm must be positive");
    let user_args = parse_ffmpeg_args(ffmpeg_args)?;
    let font = load_font(theme)?;

    ffmpeg::init().map_err(|e| anyhow::anyhow!("ffmpeg init failed: {e}"))?;

    let mut octx =
        format::output(out).with_context(|| format!("failed to open output {}", out.display()))?;

    // Default stays the portable software encoder; ffmpeg_args may select a
    // device-specific one (h264_nvenc, h264_amf, ...). Codec keys are consumed
    // here and never passed on as encoder/muxer options.
    let codec_name = codec_override(&user_args).unwrap_or("libx264");
    let codec = codec::encoder::find_by_name(codec_name)
        .with_context(|| format!("encoder '{codec_name}' not available in this FFmpeg build"))?;
    let mut stream = octx.add_stream(codec)?;

    let mut ctx = codec::context::Context::new_with_codec(codec);
    ctx.set_time_base(Rational(1, FPS as i32));
    ctx.set_frame_rate(Some(Rational(FPS as i32, 1)));

    let mut opts = Dictionary::new();
    if codec_name == "libx264" {
        // Flat-background text: `veryfast` + the stillimage tune is ~3-4x
        // faster than `medium` with no visible loss, and x264's default
        // threads=auto already spreads over all cores. There is no portable
        // hardware encoder; device-specific ones are opt-in via `-c:v`.
        opts.set("preset", "veryfast");
        opts.set("tune", "stillimage");
        opts.set("crf", "20");
    }
    for (k, v) in &user_args {
        if !is_codec_key(k) {
            opts.set(k, v);
        }
    }
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
        .map_err(|e| anyhow::anyhow!("failed to open {codec_name} encoder: {e}"))?;
    stream.set_parameters(&encoder);

    let mut mux_opts = Dictionary::new();
    // moov atom up front so the mp4 is playable while still being written.
    mux_opts.set("movflags", "+faststart");
    for (k, v) in &user_args {
        if !is_codec_key(k) {
            mux_opts.set(k, v);
        }
    }
    octx.write_header_with(mux_opts)
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
    let mut rgba = frame::Video::new(Pixel::RGBA, WIDTH, HEIGHT);
    let mut yuv = frame::Video::new(Pixel::YUV420P, WIDTH, HEIGHT);
    let mut packet = Packet::empty();
    let mut pts: i64 = 0;

    // Drawing (glyph rasterization + background fill) is the serial
    // bottleneck: x264's own threads starve while the main thread rasterizes
    // the next frame. Draw batches of RSVP frames in parallel with rayon;
    // copy/scale/encode stay on this thread (swscale contexts and AVFrames
    // are not Send).
    let mut it = frames(book_text);
    loop {
        let mut batch: Vec<Frame<'_>> = Vec::with_capacity(*DRAW_BATCH);
        batch.extend(it.by_ref().take(*DRAW_BATCH));
        if batch.is_empty() {
            break;
        }
        let imgs: Vec<RgbaImage> = batch
            .par_iter()
            .map(|frame| {
                let mut img = RgbaImage::new(WIDTH, HEIGHT);
                draw_frame(&mut img, frame, &font, theme);
                img
            })
            .collect();
        for (img, frame) in imgs.into_iter().zip(batch) {
            {
                // Copy RGBA image into the ffmpeg frame (stride may differ).
                let stride = rgba.stride(0);
                let data = rgba.data_mut(0);
                for y in 0..HEIGHT {
                    let row =
                        &img.as_raw()[y as usize * WIDTH as usize * 4..][..WIDTH as usize * 4];
                    data[y as usize * stride..][..row.len()].copy_from_slice(row);
                }
            }
            // Scale once per RSVP frame; the repeated video frames are identical.
            scaler
                .run(&rgba, &mut yuv)
                .map_err(|e| anyhow::anyhow!("sws scale failed: {e}"))?;
            let repeats = video_frames_for(frame.ticks, wpm);
            for _ in 0..repeats {
                yuv.set_pts(Some(pts));
                unsafe { (*yuv.as_mut_ptr()).duration = 1 };
                encoder
                    .send_frame(&yuv)
                    .map_err(|e| anyhow::anyhow!("encoder send failed: {e}"))?;
                drain_packets(
                    &mut encoder,
                    &mut octx,
                    &mut packet,
                    enc_time_base,
                    ost_time_base,
                )?;
                pts += 1;
            }
        }
    }

    encoder
        .send_eof()
        .map_err(|e| anyhow::anyhow!("encoder eof failed: {e}"))?;
    drain_packets(
        &mut encoder,
        &mut octx,
        &mut packet,
        enc_time_base,
        ost_time_base,
    )?;

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
        packet
            .write(octx)
            .map_err(|e| anyhow::anyhow!("packet write failed: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{codec_override, draw_batch_size, parse_ffmpeg_args};

    #[test]
    fn empty_input_yields_no_pairs() {
        assert_eq!(
            parse_ffmpeg_args("").unwrap(),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn parses_key_value_pairs() {
        assert_eq!(
            parse_ffmpeg_args("-preset slow -crf 18").unwrap(),
            vec![
                ("preset".to_string(), "slow".to_string()),
                ("crf".to_string(), "18".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_leading_plus_in_value() {
        assert_eq!(
            parse_ffmpeg_args("-movflags +faststart").unwrap(),
            vec![("movflags".to_string(), "+faststart".to_string())]
        );
    }

    #[test]
    fn keeps_colon_value_verbatim() {
        assert_eq!(
            parse_ffmpeg_args("-x264-params keyint=250:scenecut=0").unwrap(),
            vec![(
                "x264-params".to_string(),
                "keyint=250:scenecut=0".to_string()
            )]
        );
    }

    #[test]
    fn dangling_key_errors() {
        let e = parse_ffmpeg_args("-crf").unwrap_err().to_string();
        assert!(e.contains("missing value for '-crf'"), "{e}");
    }

    #[test]
    fn codec_override_extracted() {
        let args = parse_ffmpeg_args("-c:v h264_nvenc -preset p5").unwrap();
        assert_eq!(codec_override(&args), Some("h264_nvenc"));
        assert_eq!(codec_override(&parse_ffmpeg_args("-crf 18").unwrap()), None);
        let last = parse_ffmpeg_args("-c:v a -vcodec b").unwrap();
        assert_eq!(codec_override(&last), Some("b"));
    }

    #[test]
    fn draw_batch_scales_with_cores() {
        assert_eq!(draw_batch_size(1), 4);
        assert_eq!(draw_batch_size(8), 16);
        assert_eq!(draw_batch_size(64), 32);
    }

    #[test]
    fn bare_word_errors() {
        let e = parse_ffmpeg_args("foo").unwrap_err().to_string();
        assert!(e.contains("unexpected token"), "{e}");
    }
}
