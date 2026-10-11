//! Images in TUI previews, drawn with half-block characters.
//!
//! Each cell shows two vertically stacked pixels: `▀` in the top pixel's
//! color over the bottom pixel's color. This works in any true-color
//! terminal without querying it for a graphics protocol, and the result is
//! ordinary styled text, so previews scroll and wrap images like any line.

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView as _, Rgba};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

/// The largest image file a preview decodes.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Image files previews decode, by extension.
pub(crate) fn is_previewable_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
}

/// Decode an image file for previewing; `None` when it is too large or not
/// a supported image.
pub(crate) fn load_preview_image(path: &Path) -> Option<DynamicImage> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_IMAGE_BYTES {
        return None;
    }
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// `image` as half-block lines at most `max_columns` wide and `max_rows`
/// tall, keeping its aspect ratio and never enlarging it.
pub(crate) fn halfblock_lines(
    image: &DynamicImage,
    max_columns: u16,
    max_rows: u16,
) -> Vec<Line<'static>> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || max_columns == 0 || max_rows == 0 {
        return Vec::new();
    }
    // A cell is one pixel wide and two pixels tall.
    let max_width = u32::from(max_columns).min(width);
    let max_height = (u32::from(max_rows) * 2).min(height);
    let scale = f64::from(max_width) / f64::from(width);
    let scale = scale.min(f64::from(max_height) / f64::from(height));
    let target_width = scaled(width, scale);
    let target_height = scaled(height, scale);
    let pixels = image
        .resize_exact(target_width, target_height, FilterType::Triangle)
        .to_rgba8();

    (0..target_height.div_ceil(2))
        .map(|row| {
            Line::from(
                (0..target_width)
                    .map(|column| {
                        let top = pixels.get_pixel(column, row * 2);
                        let bottom = (row * 2 + 1 < target_height)
                            .then(|| pixels.get_pixel(column, row * 2 + 1));
                        let mut style = Style::default();
                        if let Some(color) = cell_color(*top) {
                            style = style.fg(color);
                        }
                        if let Some(color) = bottom.and_then(|pixel| cell_color(*pixel)) {
                            style = style.bg(color);
                        }
                        Span::styled("▀", style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the scale is at most 1, so the result fits the original dimension"
)]
fn scaled(dimension: u32, scale: f64) -> u32 {
    ((f64::from(dimension) * scale).round() as u32).max(1)
}

/// The terminal color for a pixel; mostly transparent pixels keep the
/// terminal's own background.
fn cell_color(pixel: Rgba<u8>) -> Option<Color> {
    let [red, green, blue, alpha] = pixel.0;
    (alpha >= 128).then_some(Color::Rgb(red, green, blue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn halfblocks_pair_two_pixel_rows_per_cell() {
        let mut pixels = RgbaImage::new(2, 2);
        pixels.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        pixels.put_pixel(1, 0, Rgba([255, 0, 0, 255]));
        pixels.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        pixels.put_pixel(1, 1, Rgba([0, 0, 255, 255]));
        let lines = halfblock_lines(&DynamicImage::ImageRgba8(pixels), 10, 10);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "▀");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Rgb(0, 0, 255)));
    }

    #[test]
    fn images_shrink_to_fit_and_keep_their_aspect_ratio() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(400, 200));
        let lines = halfblock_lines(&image, 40, 50);

        // 400x200 fits 40 columns as 40x20 pixels, which is 10 rows.
        assert_eq!(lines.len(), 10);
        assert!(lines.iter().all(|line| line.spans.len() == 40));
        // Height limits apply too.
        assert_eq!(halfblock_lines(&image, 400, 5).len(), 5);
    }

    #[test]
    fn transparent_pixels_keep_the_terminal_background() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
        let lines = halfblock_lines(&image, 10, 10);
        assert_eq!(lines[0].spans[0].style.fg, None);
        assert_eq!(lines[0].spans[0].style.bg, None);
    }
}
