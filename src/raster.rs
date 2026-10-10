use image::{ImageBuffer, Rgb};
use resvg::{tiny_skia, usvg};
use thiserror::Error;

pub const DEFAULT_MAX_EDGE: u32 = 2048;
const MAX_EDGE: u32 = 4096;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("raster edge must be between 1 and {MAX_EDGE} pixels")]
    Edge,
    #[error("SVG has invalid dimensions")]
    Dimensions,
    #[error("raster dimensions exceed the configured limit")]
    TooLarge,
    #[error("SVG rasterization failed")]
    Render,
    #[error("JPEG encoding failed: {0}")]
    Jpeg(#[from] image::ImageError),
}

pub fn render(svg: &str, format: Format) -> Result<Vec<u8>, Error> {
    render_with_max_edge(svg, format, DEFAULT_MAX_EDGE)
}

pub fn render_with_max_edge(svg: &str, format: Format, max_edge: u32) -> Result<Vec<u8>, Error> {
    if max_edge == 0 || max_edge > MAX_EDGE {
        return Err(Error::Edge);
    }
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &options).map_err(|_| Error::Render)?;
    let size = tree.size();
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;
    if width == 0 || height == 0 {
        return Err(Error::Dimensions);
    }
    let scale = (max_edge as f32 / width.max(height) as f32).min(1.0);
    let width = (width as f32 * scale).ceil() as u32;
    let height = (height as f32 * scale).ceil() as u32;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::TooLarge);
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or(Error::TooLarge)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    match format {
        Format::Png => pixmap.encode_png().map_err(|_| Error::Render),
        Format::Jpeg => {
            let rgba = pixmap.data();
            let pixels: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|pixel| {
                    let alpha = u16::from(pixel[3]);
                    [
                        (u16::from(pixel[0]) + 255 - alpha).min(255) as u8,
                        (u16::from(pixel[1]) + 255 - alpha).min(255) as u8,
                        (u16::from(pixel[2]) + 255 - alpha).min(255) as u8,
                    ]
                })
                .collect();
            let image =
                ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, pixels).ok_or(Error::Render)?;
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out, 90,
            ))?;
            Ok(out.into_inner())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_max_edge() {
        assert!(matches!(
            render_with_max_edge("<svg/>", Format::Png, 0),
            Err(Error::Edge)
        ));
        assert!(matches!(
            render_with_max_edge("<svg/>", Format::Png, MAX_EDGE + 1),
            Err(Error::Edge)
        ));
    }
}
