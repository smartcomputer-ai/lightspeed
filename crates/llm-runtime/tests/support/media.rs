//! Image fixtures for the request-time normalization live tests.

/// The incident's image size: over the 2000 px per-side cap, so adapters
/// send a downscaled copy (1758 × 2000).
pub const OVERSIZED: (u32, u32) = (2166, 2464);

/// The announcement suffix a downscaled [`OVERSIZED`] image carries.
pub const OVERSIZED_SHOWN_AT: &str = "shown at 1758×2000 of 2166×2464";

/// A PNG of one dominant color. A faint gradient keeps the pixels from
/// compressing to nothing, so the fixture has realistic encoded size.
pub fn png_image(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    use image::ImageEncoder as _;
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([
            rgb[0].saturating_sub((x % 32) as u8),
            rgb[1].saturating_sub((y % 32) as u8),
            rgb[2],
        ])
    });
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&image, width, height, image::ExtendedColorType::Rgb8)
        .expect("encode png");
    bytes
}

pub fn oversized_png(rgb: [u8; 3]) -> Vec<u8> {
    png_image(OVERSIZED.0, OVERSIZED.1, rgb)
}

/// The text of every input part of a lowered provider request that carries
/// `needle`, searched recursively so one helper serves every dialect.
pub fn texts_containing(value: &serde_json::Value, needle: &str) -> Vec<String> {
    match value {
        serde_json::Value::String(text) if text.contains(needle) => vec![text.clone()],
        serde_json::Value::Array(items) => items
            .iter()
            .flat_map(|item| texts_containing(item, needle))
            .collect(),
        serde_json::Value::Object(map) => map
            .values()
            .flat_map(|item| texts_containing(item, needle))
            .collect(),
        _ => Vec::new(),
    }
}
