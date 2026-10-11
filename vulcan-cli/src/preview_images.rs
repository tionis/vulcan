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

/// How much a small image may be enlarged. Terminals on high-density
/// screens use large cells, so an image at its pixel size can look tiny.
const MAX_UPSCALE: f64 = 2.0;

/// The size in cells to show `image` at: as wide as `max_columns` allows,
/// at most `max_rows` tall, keeping its aspect ratio for cells of
/// `font_size` pixels, and enlarged at most [`MAX_UPSCALE`] times.
pub(crate) fn image_cells(
    image: &DynamicImage,
    max_columns: u16,
    max_rows: u16,
    font_size: (u16, u16),
) -> (u16, u16) {
    let (width, height) = image.dimensions();
    let (font_width, font_height) = (f64::from(font_size.0.max(1)), f64::from(font_size.1.max(1)));
    if width == 0 || height == 0 || max_columns == 0 || max_rows == 0 {
        return (1, 1);
    }
    // Columns and rows at the largest allowed size.
    let natural_columns = f64::from(width) * MAX_UPSCALE / font_width;
    let natural_rows = f64::from(height) * MAX_UPSCALE / font_height;
    let scale = (f64::from(max_columns) / natural_columns)
        .min(f64::from(max_rows) / natural_rows)
        .min(1.0);
    (
        cells(natural_columns * scale).min(max_columns),
        cells(natural_rows * scale).min(max_rows),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers clamp the value to a u16 cell count"
)]
fn cells(value: f64) -> u16 {
    (value.round() as u32).clamp(1, u32::from(u16::MAX)) as u16
}

/// `image` as half-block lines exactly `columns` wide and `rows` tall.
pub(crate) fn halfblock_lines(image: &DynamicImage, columns: u16, rows: u16) -> Vec<Line<'static>> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || columns == 0 || rows == 0 {
        return Vec::new();
    }
    let target_width = u32::from(columns);
    let target_height = u32::from(rows) * 2;
    let pixels = image
        .resize_exact(target_width, target_height, FilterType::Triangle)
        .to_rgba8();

    (0..u32::from(rows))
        .map(|row| {
            Line::from(
                (0..target_width)
                    .map(|column| {
                        let top = pixels.get_pixel(column, row * 2);
                        let bottom = pixels.get_pixel(column, row * 2 + 1);
                        let mut style = Style::default();
                        if let Some(color) = cell_color(*top) {
                            style = style.fg(color);
                        }
                        if let Some(color) = cell_color(*bottom) {
                            style = style.bg(color);
                        }
                        Span::styled("▀", style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
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
        let lines = halfblock_lines(&DynamicImage::ImageRgba8(pixels), 2, 1);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "▀");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(lines[0].spans[0].style.bg, Some(Color::Rgb(0, 0, 255)));
    }

    #[test]
    fn images_fill_the_width_and_keep_their_aspect_ratio() {
        let font = (10, 20);
        // A large image shrinks to the width: 2000x1000 at 80 columns is
        // 800x400 pixels, 20 rows of 20 pixels.
        let large = DynamicImage::ImageRgba8(RgbaImage::new(2000, 1000));
        assert_eq!(image_cells(&large, 80, 40, font), (80, 20));
        // Height limits apply too, keeping the proportions.
        assert_eq!(image_cells(&large, 80, 10, font), (40, 10));
        // A small image is enlarged at most twice: 100x50 becomes 200x100
        // pixels, 20 columns by 5 rows.
        let small = DynamicImage::ImageRgba8(RgbaImage::new(100, 50));
        assert_eq!(image_cells(&small, 80, 40, font), (20, 5));
    }

    #[test]
    fn transparent_pixels_keep_the_terminal_background() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(1, 2));
        let lines = halfblock_lines(&image, 1, 1);
        assert_eq!(lines[0].spans[0].style.fg, None);
        assert_eq!(lines[0].spans[0].style.bg, None);
    }
}
