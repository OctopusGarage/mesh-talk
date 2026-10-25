//! Versioned sticker presentation metadata carried by ordinary image media.

pub const MAX_STICKER_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerMetadata {
    pub id: String,
    pub fallback: String,
}

use std::io::Cursor;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use image::{AnimationDecoder, ImageDecoder, ImageFormat, ImageReader, Limits};

const PREFIX: &str = "mt-sticker-v1-";
const MAX_PIXELS: u64 = 16_000_000;
const MAX_ANIMATION_PIXELS: u64 = 64_000_000;
const MAX_FRAMES: usize = 256;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;

/// Recognizes the reserved namespace even when the presentation metadata is malformed.
pub fn is_sticker_name(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Parses canonical v1 presentation metadata; this is not the asset checksum.
pub fn from_name(name: &str) -> Option<StickerMetadata> {
    if !name.is_ascii() || name.len() > 233 {
        return None;
    }
    let (encoded, _) = name.strip_prefix(PREFIX)?.rsplit_once('.')?;
    expected_format(name)?;
    let decoded = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    if URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return None;
    }
    let text = std::str::from_utf8(&decoded).ok()?;
    let (id, fallback) = text.split_once('\n')?;
    if id.is_empty()
        || id.len() > 126
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_:.-".contains(&c))
        || fallback.is_empty()
        || fallback.len() > 32
        || fallback.chars().count() > 8
        || fallback.chars().any(char::is_control)
    {
        return None;
    }
    Some(StickerMetadata {
        id: id.into(),
        fallback: fallback.into(),
    })
}

fn expected_format(name: &str) -> Option<(ImageFormat, &'static str)> {
    match name.rsplit_once('.')?.1 {
        "png" => Some((ImageFormat::Png, "image/png")),
        "jpg" => Some((ImageFormat::Jpeg, "image/jpeg")),
        "webp" => Some((ImageFormat::WebP, "image/webp")),
        "gif" => Some((ImageFormat::Gif, "image/gif")),
        _ => None,
    }
}

/// Checks sticker metadata before admitting an encrypted media manifest.
pub fn valid_manifest(name: &str, mime: &str, size: u64, media: bool) -> bool {
    media
        && size > 0
        && size <= MAX_STICKER_BYTES as u64
        && from_name(name).is_some()
        && expected_format(name).is_some_and(|(_, expected)| expected == mime)
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn check_dimensions((width, height): (u32, u32)) -> Result<u64, String> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || width > 4096 || height > 4096 || pixels > MAX_PIXELS {
        return Err("Sticker dimensions exceed 4096 per side or 16 million pixels".into());
    }
    Ok(pixels)
}

/// Fully decodes raster data, including every animation frame. Frames are dropped as
/// they are checked. Animations allow 256 frames and 64 million aggregate canvas
/// pixels. Decoder allocation limits are best effort (as specified by `image`);
/// the additional canvas budget allows four RGBA working buffers within 64 MiB.
pub fn validate_image(name: &str, mime: &str, bytes: &[u8]) -> Result<(), String> {
    if !valid_manifest(name, mime, bytes.len() as u64, true) {
        return Err("Invalid sticker name, media type or byte size".into());
    }
    let format = expected_format(name).unwrap().0;
    if image::guess_format(bytes).map_err(|e| e.to_string())? != format {
        return Err("Sticker raster format does not match its media type".into());
    }
    let cursor = Cursor::new(bytes);
    match format {
        ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::with_limits(cursor, limits())
                .map_err(|e| e.to_string())?;
            let pixels = check_dimensions(decoder.dimensions())?;
            if decoder.is_apng().map_err(|e| e.to_string())? {
                check_animation_memory(pixels)?;
                validate_frames(decoder.apng().map_err(|e| e.to_string())?.into_frames())
            } else {
                validate_static(decoder)
            }
        }
        ImageFormat::Gif => {
            let mut decoder =
                image::codecs::gif::GifDecoder::new(cursor).map_err(|e| e.to_string())?;
            decoder.set_limits(limits()).map_err(|e| e.to_string())?;
            check_animation_memory(check_dimensions(decoder.dimensions())?)?;
            validate_frames(decoder.into_frames())
        }
        ImageFormat::WebP => {
            let mut decoder =
                image::codecs::webp::WebPDecoder::new(cursor).map_err(|e| e.to_string())?;
            decoder.set_limits(limits()).map_err(|e| e.to_string())?;
            let pixels = check_dimensions(decoder.dimensions())?;
            if decoder.has_animation() {
                check_animation_memory(pixels)?;
                validate_frames(decoder.into_frames())
            } else {
                validate_static(decoder)
            }
        }
        _ => {
            let mut reader = ImageReader::with_format(cursor, format);
            reader.limits(limits());
            validate_static(reader.into_decoder().map_err(|e| e.to_string())?)
        }
    }
}

fn validate_static(decoder: impl ImageDecoder) -> Result<(), String> {
    check_dimensions(decoder.dimensions())?;
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err("Sticker decoded image exceeds 64 MiB".into());
    }
    let mut buffer = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut buffer).map_err(|e| e.to_string())
}

fn check_animation_memory(pixels: u64) -> Result<(), String> {
    if pixels * 16 > MAX_DECODE_BYTES {
        return Err("Sticker animation canvas exceeds the decode memory budget".into());
    }
    Ok(())
}

fn validate_frames(frames: image::Frames<'_>) -> Result<(), String> {
    let mut count = 0;
    let mut pixels = 0;
    for frame in frames {
        let frame = frame.map_err(|e| e.to_string())?;
        count += 1;
        pixels += check_dimensions(frame.buffer().dimensions())?;
        if count > MAX_FRAMES || pixels > MAX_ANIMATION_PIXELS {
            return Err("Sticker animation exceeds 256 frames or 64 million decoded pixels".into());
        }
    }
    if count == 0 {
        return Err("Sticker animation contains no frames".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use image::{
        Frame, ImageEncoder, RgbaImage,
        codecs::{gif::GifEncoder, png::PngEncoder},
    };

    fn name(id: &str, fallback: &str, extension: &str) -> String {
        format!(
            "mt-sticker-v1-{}.{}",
            URL_SAFE_NO_PAD.encode(format!("{id}\n{fallback}")),
            extension
        )
    }

    #[test]
    fn metadata_has_a_canonical_cross_language_vector() {
        assert_eq!(
            name("builtin:wave", "👋", "png"),
            "mt-sticker-v1-YnVpbHRpbjp3YXZlCvCfkYs.png"
        );
        assert_eq!(
            from_name("mt-sticker-v1-YnVpbHRpbjp3YXZlCvCfkYs.png"),
            Some(StickerMetadata {
                id: "builtin:wave".into(),
                fallback: "👋".into()
            })
        );
        assert!(from_name(&name(&"a".repeat(126), &"😀".repeat(8), "webp")).is_some());
        assert!(is_sticker_name("mt-sticker-v1-malformed"));
        assert!(!is_sticker_name("vacation.png"));
    }

    #[test]
    fn rejects_legacy_and_noncanonical_metadata() {
        for value in [
            "vacation.png".to_owned(),
            name("", "👋", "png"),
            name("A", "👋", "png"),
            name("x", "", "png"),
            name("x", "a\nb", "png"),
            name("x", &"a".repeat(9), "png"),
            name(&"x".repeat(127), "👋", "png"),
            name("x", "👋", "PNG"),
            "mt-sticker-v1-YQph=.png".to_owned(),
            "mt-sticker-v1-YQph!.png".to_owned(),
        ] {
            assert_eq!(from_name(&value), None, "{value}");
        }
    }

    #[test]
    fn manifest_requires_matching_media_and_size() {
        let value = name("x", "👋", "png");
        assert!(valid_manifest(&value, "image/png", 1, true));
        assert!(valid_manifest(
            &value,
            "image/png",
            MAX_STICKER_BYTES as u64,
            true
        ));
        for (mime, size, media) in [
            ("image/jpeg", 1, true),
            ("image/png", 0, true),
            ("image/png", MAX_STICKER_BYTES as u64 + 1, true),
            ("image/png", 1, false),
        ] {
            assert!(!valid_manifest(&value, mime, size, media));
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(
                &vec![0; (width * height * 4) as usize],
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    fn gif(frames: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            for _ in 0..frames {
                encoder
                    .encode_frame(Frame::new(RgbaImage::new(1, 1)))
                    .unwrap();
            }
        }
        bytes
    }

    #[test]
    fn decodes_actual_images_and_rejects_disguised_or_broken_data() {
        let bytes = png(2, 2);
        assert!(validate_image(&name("x", "👋", "png"), "image/png", &bytes).is_ok());
        for data in [
            b"not an image".as_slice(),
            &bytes[..bytes.len() / 2],
            &vec![0; MAX_STICKER_BYTES + 1],
        ] {
            assert!(validate_image(&name("x", "👋", "png"), "image/png", data).is_err());
        }
        assert!(validate_image(&name("x", "👋", "gif"), "image/gif", &bytes).is_err());
        assert!(validate_image(&name("x", "👋", "png"), "image/png", &png(4097, 1)).is_err());
    }

    #[test]
    fn accepts_jpeg_webp_and_apng_rasters() {
        for (format, extension, mime) in [
            (ImageFormat::Jpeg, "jpg", "image/jpeg"),
            (ImageFormat::WebP, "webp", "image/webp"),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 2)
                .write_to(&mut bytes, format)
                .unwrap();
            assert!(validate_image(&name("x", "👋", extension), mime, bytes.get_ref()).is_ok());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACGFjVEwAAAACAAAAAPONk3AAAAAaZmNUTAAAAAAAAAABAAAAAQAAAAAAAAAAAAEACgAAWn8w0AAAAA1JREFUeJxj+M/A8B8ABQAB/4mZPR0AAAAaZmNUTAAAAAEAAAABAAAAAQAAAAAAAAAAAAEACgAAwQzaBAAAABFmZEFUAAAAAnicY2D4z/AfAAQBAf9i5+mcAAAAAElFTkSuQmCC").unwrap();
        assert!(validate_image(&name("x", "👋", "png"), "image/png", &bytes).is_ok());
    }

    // Wrap a real lossless raster in an animated WebP RIFF container.
    fn animated_webp(frames: usize) -> Vec<u8> {
        fn chunk(bytes: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
            bytes.extend_from_slice(tag);
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(data);
            if !data.len().is_multiple_of(2) {
                bytes.push(0);
            }
        }
        let mut raster = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut raster, ImageFormat::WebP)
            .unwrap();
        let mut body = b"WEBP".to_vec();
        chunk(&mut body, b"VP8X", &[0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        chunk(&mut body, b"ANIM", &[0; 6]);
        for _ in 0..frames {
            let mut frame = vec![0; 16];
            frame[12] = 1;
            frame.extend_from_slice(&raster.get_ref()[12..]);
            chunk(&mut body, b"ANMF", &frame);
        }
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn validates_every_animated_webp_frame() {
        let value = name("x", "👋", "webp");
        assert!(validate_image(&value, "image/webp", &animated_webp(2)).is_ok());
        assert!(validate_image(&value, "image/webp", &animated_webp(257)).is_err());
    }

    #[test]
    fn enforces_aggregate_pixels_and_working_canvas_budget() {
        let frames = image::Frames::new(Box::new(
            (0..65).map(|_| Ok(Frame::new(RgbaImage::new(1000, 1000)))),
        ));
        assert!(validate_frames(frames).is_err());
        assert!(check_animation_memory(4096 * 4096).is_err());
        assert!(check_dimensions((4096, 4096)).is_err());
    }

    #[test]
    fn bounds_animation_frames_without_rejecting_small_animations() {
        let value = name("x", "👋", "gif");
        assert!(validate_image(&value, "image/gif", &gif(2)).is_ok());
        assert!(validate_image(&value, "image/gif", &gif(257)).is_err());
    }
}
