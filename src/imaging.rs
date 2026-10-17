//! Image acquisition rules and preparation for OCR.

use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, ImageFormat};
use std::io::Cursor;

/// Only Discord's CDN is ever fetched (no SSRF, no leaking our IP to attackers).
pub fn is_allowed_host(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "https"
                && matches!(u.host_str(), Some("cdn.discordapp.com") | Some("media.discordapp.net"))
        }
        Err(_) => false,
    }
}

pub fn looks_like_image(content_type: Option<&str>, filename: &str) -> bool {
    if let Some(ct) = content_type {
        return ct.starts_with("image/");
    }
    let f = filename.to_lowercase();
    ["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp"].iter().any(|e| f.ends_with(&format!(".{e}")))
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrepError {
    Undecodable,
    CannotFit,
}

pub struct Prepared {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).ok()?;
    Some(out)
}

/// Make the image acceptable to OCR.space: a supported format and under `max_bytes`.
/// Already-supported formats under the limit pass through unchanged.
pub fn prepare(bytes: &[u8], max_bytes: usize) -> Result<Prepared, PrepError> {
    let fmt = image::guess_format(bytes).map_err(|_| PrepError::Undecodable)?;
    let native = match fmt {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::Bmp => Some("image/bmp"),
        ImageFormat::Tiff => Some("image/tiff"),
        _ => None,
    };
    if let Some(mime) = native {
        if bytes.len() <= max_bytes {
            return Ok(Prepared { bytes: bytes.to_vec(), mime });
        }
    }
    let mut img = image::load_from_memory(bytes).map_err(|_| PrepError::Undecodable)?;
    // Cap the longest edge so huge screenshots shrink quickly.
    if img.width().max(img.height()) > 3000 {
        img = img.resize(3000, 3000, FilterType::Triangle);
    }
    for _ in 0..6 {
        for q in [85u8, 70, 55] {
            if let Some(b) = encode_jpeg(&img, q) {
                if b.len() <= max_bytes {
                    return Ok(Prepared { bytes: b, mime: "image/jpeg" });
                }
            }
        }
        let (w, h) = (img.width() * 3 / 4, img.height() * 3 / 4);
        if w < 64 || h < 64 {
            break;
        }
        img = img.resize(w, h, FilterType::Triangle);
    }
    Err(PrepError::CannotFit)
}

pub fn png_bytes(img: &DynamicImage) -> Vec<u8> {
    let mut c = Cursor::new(Vec::new());
    img.write_to(&mut c, ImageFormat::Png).unwrap();
    c.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn noise(w: u32, h: u32) -> DynamicImage {
        let mut x: u32 = 12345;
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |_, _| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            Rgb([(x >> 24) as u8, (x >> 16) as u8, (x >> 8) as u8])
        }))
    }

    #[test]
    fn host_allowlist() {
        assert!(is_allowed_host("https://cdn.discordapp.com/attachments/1/2/a.png"));
        assert!(is_allowed_host("https://media.discordapp.net/attachments/1/2/a.png?x=1"));
        assert!(!is_allowed_host("http://cdn.discordapp.com/a.png"));
        assert!(!is_allowed_host("https://evil.com/a.png"));
        assert!(!is_allowed_host("https://cdn.discordapp.com.evil.com/a.png"));
        assert!(!is_allowed_host("https://evil.com/cdn.discordapp.com"));
        assert!(!is_allowed_host("not a url"));
    }

    #[test]
    fn image_detection() {
        assert!(looks_like_image(Some("image/png"), "x"));
        assert!(!looks_like_image(Some("video/mp4"), "x.png"));
        assert!(looks_like_image(None, "SCAM.JPG"));
        assert!(!looks_like_image(None, "doc.pdf"));
    }

    #[test]
    fn small_png_passes_through() {
        let png = png_bytes(&noise(20, 20));
        let p = prepare(&png, 1_000_000).unwrap();
        assert_eq!(p.mime, "image/png");
        assert_eq!(p.bytes, png);
    }

    #[test]
    fn oversized_png_is_recompressed_under_limit() {
        let png = png_bytes(&noise(600, 600));
        assert!(png.len() > 100_000);
        let p = prepare(&png, 100_000).unwrap();
        assert_eq!(p.mime, "image/jpeg");
        assert!(p.bytes.len() <= 100_000);
        assert!(image::load_from_memory(&p.bytes).is_ok());
    }

    #[test]
    fn webp_is_converted() {
        let img = noise(40, 40);
        let mut c = Cursor::new(Vec::new());
        if img.write_to(&mut c, ImageFormat::WebP).is_ok() {
            let p = prepare(c.get_ref(), 1_000_000).unwrap();
            assert_eq!(p.mime, "image/jpeg");
        }
    }

    #[test]
    fn garbage_is_undecodable() {
        assert!(matches!(prepare(b"definitely not an image", 1000), Err(PrepError::Undecodable)));
    }

    #[test]
    fn impossible_limit_is_cannot_fit() {
        let png = png_bytes(&noise(300, 300));
        assert!(matches!(prepare(&png, 50), Err(PrepError::CannotFit)));
    }
}
