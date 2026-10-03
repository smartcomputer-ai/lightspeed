//! Model-facing image normalization.
//!
//! Adapters send every image as a bounded copy rather than the stored bytes:
//! no side above [`MAX_IMAGE_SIDE`] pixels and no more than
//! [`MAX_IMAGE_BYTES`]. The cap is fixed and independent of the request, so
//! an image lowers to the same bytes on its first request and its last;
//! a cap that tightened as a session accumulated images would rewrite history
//! the provider has already seen, invalidating its prompt cache and preserved
//! reasoning. Compliant images pass through byte-identical.
//!
//! The copy exists only in the provider request. Stored blobs, context
//! entries, and `media:` handles keep referring to the original, because a
//! session may change models and a copy computed for one request shape must
//! not become durable state.
//!
//! A whole request is also held to one media budget, the same for every
//! provider and model: [`RequestMedia`] omits the oldest media when the
//! request would carry too many items or too many bytes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use harness::{BlobRef, ContextEntry, ContextEntryId, ContextEntryKind, storage::BlobStore};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, RgbImage};

use crate::error::{LlmAdapterError, LlmAdapterResult};

/// Longest side, in pixels, of any image sent to a model. Anthropic rejects
/// requests with more than 20 images when any side exceeds 2000 px, and
/// counts images from earlier turns; every provider downscales larger images
/// itself, so the cap gives up little.
pub const MAX_IMAGE_SIDE: u32 = 2000;
/// Largest image payload sent to a model, before base64 encoding: 5 MiB once
/// encoded, half of Anthropic's 10 MB per-image limit.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;
/// Fixed JPEG quality for re-encoded images. Changing it, the resampling
/// filter, or the codec crate version changes the bytes of every normalized
/// image once, like any other history edit.
const JPEG_QUALITY: u8 = 85;
/// Upper bound on normalized copies held in memory per worker. Normalization
/// is deterministic, so a miss only costs a decode.
const CACHE_CAPACITY_BYTES: usize = 64 * 1024 * 1024;

/// Most media items one request carries. Anthropic accepts 100 images per
/// request on 200k-context models and more elsewhere; OpenAI accepts more.
pub const MAX_REQUEST_MEDIA_ITEMS: usize = 100;
/// Most encoded media bytes one request carries, leaving room for text below
/// Anthropic's 32 MB request body limit; OpenAI's limits are higher.
pub const MAX_REQUEST_MEDIA_BYTES: usize = 24 * 1024 * 1024;
/// Omitted media is counted in whole chunks, so the cut point moves once per
/// chunk of new media rather than every turn: each move rewrites history the
/// provider has seen, invalidating its prompt cache and preserved reasoning.
const OMISSION_CHUNK: usize = 10;
/// Rounding to a chunk never omits media below this many of the newest items
/// that fit, so a single large tool batch keeps its newest images.
const MIN_KEPT_MEDIA: usize = 4;

/// The media a request carries, prepared before any entry is lowered so the
/// request can be held to the media budget as a whole. Which media is omitted
/// is a pure function of the entries, so retries send identical requests.
#[derive(Default)]
pub struct RequestMedia {
    prepared: HashMap<ContextEntryId, PreparedMedia>,
    omitted: HashSet<ContextEntryId>,
}

enum PreparedMedia {
    Image(ModelImage),
    Pdf(String),
}

impl PreparedMedia {
    fn encoded_len(&self) -> usize {
        match self {
            Self::Image(image) => image.base64.len(),
            Self::Pdf(base64) => base64.len(),
        }
    }
}

impl RequestMedia {
    pub async fn prepare(
        blobs: &dyn BlobStore,
        entries: &[ContextEntry],
    ) -> LlmAdapterResult<Self> {
        let mut media = Vec::new();
        for entry in entries {
            if !matches!(entry.kind, ContextEntryKind::Message { .. }) {
                continue;
            }
            let content = &entry.content;
            let prepared = if let Some(mime) =
                crate::blob_io::image_media_type(content.media_type.as_deref())
            {
                PreparedMedia::Image(model_image(blobs, &content.content_ref, mime).await?)
            } else if crate::blob_io::document_entry(
                content.media_type.as_deref(),
                entry.preview.as_deref(),
            )
            .is_some_and(|document| document.is_pdf)
            {
                PreparedMedia::Pdf(crate::blob_io::read_base64(blobs, &content.content_ref).await?)
            } else {
                continue;
            };
            media.push((entry.entry_id, prepared));
        }
        let sizes = media
            .iter()
            .map(|(_, prepared)| prepared.encoded_len())
            .collect::<Vec<_>>();
        let omit = omission_count(&sizes);
        let mut request = Self::default();
        for (index, (entry_id, prepared)) in media.into_iter().enumerate() {
            if index < omit {
                request.omitted.insert(entry_id);
            } else {
                request.prepared.insert(entry_id, prepared);
            }
        }
        Ok(request)
    }

    /// The entry's media is left out of this request to keep it in budget.
    pub fn is_omitted(&self, entry: &ContextEntry) -> bool {
        self.omitted.contains(&entry.entry_id)
    }

    /// The image to send for `entry`, prepared or computed now.
    pub async fn image(
        &mut self,
        blobs: &dyn BlobStore,
        entry: &ContextEntry,
        mime: &str,
    ) -> LlmAdapterResult<ModelImage> {
        match self.prepared.remove(&entry.entry_id) {
            Some(PreparedMedia::Image(image)) => Ok(image),
            _ => model_image(blobs, &entry.content.content_ref, mime).await,
        }
    }

    /// The base64 PDF to send for `entry`, prepared or read now.
    pub async fn pdf_base64(
        &mut self,
        blobs: &dyn BlobStore,
        entry: &ContextEntry,
    ) -> LlmAdapterResult<String> {
        match self.prepared.remove(&entry.entry_id) {
            Some(PreparedMedia::Pdf(base64)) => Ok(base64),
            _ => crate::blob_io::read_base64(blobs, &entry.content.content_ref).await,
        }
    }
}

/// The text sent in place of media omitted to keep a request in budget:
/// `[image · media:3f9a2c1d4e7b · omitted from this request to stay within
/// provider limits]`. The handle stays, so the model can still name it.
pub fn omission_placeholder(entry: &ContextEntry) -> String {
    let announcement = crate::blob_io::media_announcement(entry);
    match announcement.rsplit_once(" · ") {
        Some((head, _media_type)) => {
            format!("{head} · omitted from this request to stay within provider limits]")
        }
        None => announcement,
    }
}

/// How many of the oldest media items to omit, given each item's encoded
/// size in context order: the fewest that bring the rest within the budget,
/// rounded up to a whole chunk but never below the newest items that fit
/// (at least [`MIN_KEPT_MEDIA`] of them).
fn omission_count(sizes: &[usize]) -> usize {
    let total = sizes.len();
    let mut omit = total.saturating_sub(MAX_REQUEST_MEDIA_ITEMS);
    let mut bytes = sizes[omit..].iter().sum::<usize>();
    while bytes > MAX_REQUEST_MEDIA_BYTES && omit < total {
        bytes -= sizes[omit];
        omit += 1;
    }
    if omit == 0 {
        return 0;
    }
    omit.div_ceil(OMISSION_CHUNK)
        .saturating_mul(OMISSION_CHUNK)
        .min(total.saturating_sub(MIN_KEPT_MEDIA))
        .max(omit)
}

/// One image as the model receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelImage {
    /// Media type of the bytes sent, which differs from the stored type when
    /// the image was re-encoded.
    pub media_type: String,
    pub base64: String,
    /// Present when the image was downscaled.
    pub resize: Option<ImageResize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageResize {
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
}

impl ModelImage {
    /// The media announcement written before the image block, extended with
    /// the dimensions the model sees when the image was downscaled, so
    /// coordinate-based work can scale back to the source.
    pub fn announcement(&self, entry: &harness::ContextEntry) -> String {
        let announcement = crate::blob_io::media_announcement(entry);
        match (&self.resize, announcement.strip_suffix(']')) {
            (Some(resize), Some(head)) => format!(
                "{head} · shown at {}×{} of {}×{}]",
                resize.width, resize.height, resize.original_width, resize.original_height
            ),
            _ => announcement,
        }
    }
}

/// Read an image blob and return the copy to send to the model.
pub async fn model_image(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
    media_type: &str,
) -> LlmAdapterResult<ModelImage> {
    if let Some(cached) = cache().lock().expect("image cache lock").get(blob_ref) {
        return Ok(cached);
    }
    let bytes = blobs.read_bytes(blob_ref).await?;
    let (bytes, normalized) = tokio::task::spawn_blocking(move || {
        let normalized = normalize_image(&bytes);
        (bytes, normalized)
    })
    .await
    .map_err(|error| LlmAdapterError::InvalidProviderRequest {
        message: format!("image normalization for {blob_ref} did not complete: {error}"),
    })?;
    let normalized = match normalized {
        Ok(Some(normalized)) => normalized,
        Ok(None) => return Ok(passthrough(&bytes, media_type)),
        Err(error) => {
            // Admission accepted these bytes; the provider stays the judge of
            // an image the runtime cannot decode.
            tracing::warn!(%blob_ref, %error, "image normalization failed; sending original bytes");
            return Ok(passthrough(&bytes, media_type));
        }
    };
    let image = ModelImage {
        media_type: normalized.media_type.to_owned(),
        base64: base64::engine::general_purpose::STANDARD.encode(&normalized.bytes),
        resize: normalized.resize,
    };
    cache()
        .lock()
        .expect("image cache lock")
        .insert(blob_ref.clone(), image.clone());
    Ok(image)
}

fn passthrough(bytes: &[u8], media_type: &str) -> ModelImage {
    ModelImage {
        media_type: media_type.to_owned(),
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        resize: None,
    }
}

#[derive(Debug)]
struct NormalizedImage {
    media_type: &'static str,
    bytes: Vec<u8>,
    resize: Option<ImageResize>,
}

/// The bounded copy of `bytes`, or `None` when the image already complies
/// and is sent unchanged. Dimensions come from the header, so a compliant
/// image is never decoded. An image whose header cannot be read is also left
/// to the provider.
///
/// Output is a pure function of the input: fixed filter, fixed encoder
/// settings, no timestamps or metadata.
fn normalize_image(bytes: &[u8]) -> image::ImageResult<Option<NormalizedImage>> {
    let Some((width, height)) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
    else {
        return Ok(None);
    };
    if width <= MAX_IMAGE_SIDE && height <= MAX_IMAGE_SIDE && bytes.len() <= MAX_IMAGE_BYTES {
        return Ok(None);
    }

    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let lossless_source = reader.format() != Some(ImageFormat::Jpeg);
    // Animated GIFs decode to their first frame, which is what providers read.
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let (original_width, original_height) = image.dimensions();
    if original_width > MAX_IMAGE_SIDE || original_height > MAX_IMAGE_SIDE {
        image = image.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, FilterType::Lanczos3);
    }

    let (media_type, bytes) = loop {
        if lossless_source {
            let png = encode_png(&image)?;
            if png.len() <= MAX_IMAGE_BYTES {
                break ("image/png", png);
            }
        }
        let jpeg = encode_jpeg(&image)?;
        if jpeg.len() <= MAX_IMAGE_BYTES || (image.width() <= 1 && image.height() <= 1) {
            break ("image/jpeg", jpeg);
        }
        // Pathological content can exceed the budget even at the pixel cap;
        // halving always converges.
        image = image.resize(
            (image.width() / 2).max(1),
            (image.height() / 2).max(1),
            FilterType::Lanczos3,
        );
    };
    let (width, height) = image.dimensions();
    let resize = (width != original_width || height != original_height).then_some(ImageResize {
        width,
        height,
        original_width,
        original_height,
    });
    Ok(Some(NormalizedImage {
        media_type,
        bytes,
        resize,
    }))
}

fn encode_png(image: &DynamicImage) -> image::ImageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Default, PngFilter::Adaptive);
    match image {
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageRgb8(_)
        | DynamicImage::ImageRgba8(_) => image.write_with_encoder(encoder)?,
        _ if image.color().has_alpha() => {
            DynamicImage::ImageRgba8(image.to_rgba8()).write_with_encoder(encoder)?
        }
        _ => DynamicImage::ImageRgb8(image.to_rgb8()).write_with_encoder(encoder)?,
    }
    Ok(bytes)
}

fn encode_jpeg(image: &DynamicImage) -> image::ImageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY);
    DynamicImage::ImageRgb8(flatten_onto_white(image)).write_with_encoder(encoder)?;
    Ok(bytes)
}

/// JPEG has no alpha channel; transparent pixels become white rather than
/// whatever color the transparent pixels happen to store.
fn flatten_onto_white(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |channel: u8| {
            let alpha = u32::from(a);
            ((u32::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        };
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}

/// Worker-local cache of normalized copies, keyed by source blob. The
/// normalization spec is fixed for the life of the process, so the blob
/// alone identifies the output.
#[derive(Default)]
struct ImageCache {
    entries: HashMap<BlobRef, ModelImage>,
    order: VecDeque<BlobRef>,
    bytes: usize,
}

impl ImageCache {
    fn get(&mut self, blob_ref: &BlobRef) -> Option<ModelImage> {
        let image = self.entries.get(blob_ref)?.clone();
        if let Some(position) = self.order.iter().position(|key| key == blob_ref) {
            let key = self.order.remove(position).expect("cached key position");
            self.order.push_back(key);
        }
        Some(image)
    }

    fn insert(&mut self, blob_ref: BlobRef, image: ModelImage) {
        let size = image.base64.len();
        if size > CACHE_CAPACITY_BYTES || self.entries.contains_key(&blob_ref) {
            return;
        }
        while self.bytes + size > CACHE_CAPACITY_BYTES {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(image) = self.entries.remove(&evicted) {
                self.bytes -= image.base64.len();
            }
        }
        self.bytes += size;
        self.order.push_back(blob_ref.clone());
        self.entries.insert(blob_ref, image);
    }
}

fn cache() -> &'static Mutex<ImageCache> {
    static CACHE: OnceLock<Mutex<ImageCache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgba, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        });
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, width, height, image::ExtendedColorType::Rgba8)
            .expect("encode png");
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 90)
            .write_image(&image, width, height, image::ExtendedColorType::Rgb8)
            .expect("encode jpeg");
        bytes
    }

    /// Deterministic noise, which neither PNG nor JPEG compresses well.
    fn noise_png(width: u32, height: u32) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        let image = RgbaImage::from_fn(width, height, |_, _| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            };
            Rgba([next(), next(), next(), next()])
        });
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, width, height, image::ExtendedColorType::Rgba8)
            .expect("encode png");
        bytes
    }

    fn image_entry(content_ref: BlobRef) -> harness::ContextEntry {
        harness::ContextEntry {
            entry_id: harness::ContextEntryId::new(1),
            key: None,
            kind: harness::ContextEntryKind::Message {
                role: harness::ContextMessageRole::User,
            },
            source: harness::ContextEntrySource::RunInput {
                run_id: harness::RunId::new(1),
                input_index: 0,
            },
            content: harness::ContentRef {
                content_ref,
                media_type: Some("image/png".to_owned()),
                provider_kind: None,
            },
            preview: Some("[image]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .expect("guess format")
            .into_dimensions()
            .expect("dimensions")
    }

    #[test]
    fn compliant_images_pass_through_unchanged() {
        assert!(
            normalize_image(&png(2000, 1200))
                .expect("normalize")
                .is_none()
        );
        assert!(
            normalize_image(&jpeg(640, 480))
                .expect("normalize")
                .is_none()
        );
    }

    #[test]
    fn undecodable_bytes_pass_through_unchanged() {
        assert!(
            normalize_image(b"not an image")
                .expect("normalize")
                .is_none()
        );
    }

    #[test]
    fn oversized_png_is_downscaled_within_the_cap() {
        // The incident's image: 2166 × 2464 px.
        let normalized = normalize_image(&png(2166, 2464))
            .expect("normalize")
            .expect("oversized image is normalized");
        assert_eq!(normalized.media_type, "image/png");
        let (width, height) = dimensions(&normalized.bytes);
        assert_eq!(height, MAX_IMAGE_SIDE);
        assert_eq!(width, 1758);
        assert!(normalized.bytes.len() <= MAX_IMAGE_BYTES);
        assert_eq!(
            normalized.resize,
            Some(ImageResize {
                width,
                height,
                original_width: 2166,
                original_height: 2464,
            })
        );
    }

    #[test]
    fn oversized_jpeg_stays_jpeg() {
        let normalized = normalize_image(&jpeg(4000, 1000))
            .expect("normalize")
            .expect("oversized image is normalized");
        assert_eq!(normalized.media_type, "image/jpeg");
        assert_eq!(dimensions(&normalized.bytes), (2000, 500));
    }

    #[test]
    fn image_over_the_byte_budget_is_reencoded_as_jpeg() {
        let source = noise_png(1600, 1600);
        assert!(
            source.len() > MAX_IMAGE_BYTES,
            "fixture must exceed the budget"
        );
        let normalized = normalize_image(&source)
            .expect("normalize")
            .expect("over-budget image is normalized");
        assert_eq!(normalized.media_type, "image/jpeg");
        assert!(normalized.bytes.len() <= MAX_IMAGE_BYTES);
        assert_eq!(normalized.resize, None);
    }

    #[test]
    fn normalization_is_deterministic() {
        let source = png(3000, 2000);
        let first = normalize_image(&source)
            .expect("normalize")
            .expect("normalized");
        let second = normalize_image(&source)
            .expect("normalize")
            .expect("normalized");
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn transparency_flattens_onto_white() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])));
        assert_eq!(
            flatten_onto_white(&image).get_pixel(0, 0).0,
            [255, 255, 255]
        );
    }

    #[test]
    fn announcement_names_the_shown_dimensions() {
        let image = ModelImage {
            media_type: "image/png".to_owned(),
            base64: String::new(),
            resize: Some(ImageResize {
                width: 2000,
                height: 1400,
                original_width: 4000,
                original_height: 2800,
            }),
        };
        let blob_ref = BlobRef::from_bytes(b"image");
        let entry = image_entry(blob_ref.clone());
        assert_eq!(
            image.announcement(&entry),
            format!(
                "[image · {} · image/png · shown at 2000×1400 of 4000×2800]",
                harness::media::media_handle(&blob_ref)
            )
        );
    }

    #[test]
    fn requests_within_budget_omit_nothing() {
        assert_eq!(omission_count(&[]), 0);
        assert_eq!(omission_count(&[1024; MAX_REQUEST_MEDIA_ITEMS]), 0);
    }

    #[test]
    fn omission_drops_the_oldest_media_in_whole_chunks() {
        // One item over the count limit omits a whole chunk of the oldest.
        assert_eq!(omission_count(&[1024; MAX_REQUEST_MEDIA_ITEMS + 1]), 10);
        // The cut holds until the next chunk is needed.
        assert_eq!(omission_count(&[1024; MAX_REQUEST_MEDIA_ITEMS + 10]), 10);
        assert_eq!(omission_count(&[1024; MAX_REQUEST_MEDIA_ITEMS + 11]), 20);
    }

    #[test]
    fn an_oversized_batch_keeps_its_newest_items() {
        // Eight 5 MiB images exceed the byte budget: the oldest go first, and
        // rounding to a chunk never drops the newest items that fit.
        let sizes = [5 * 1024 * 1024; 8];
        let omit = omission_count(&sizes);
        assert_eq!(omit, 4);
        assert!(sizes[omit..].iter().sum::<usize>() <= MAX_REQUEST_MEDIA_BYTES);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_media_omits_the_oldest_and_keeps_the_rest_prepared() {
        let blobs = harness::storage::InMemoryBlobStore::new();
        let png = {
            let mut bytes = Vec::new();
            PngEncoder::new(&mut bytes)
                .write_image(&[0, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
                .expect("encode png");
            bytes
        };
        let mut entries = Vec::new();
        for index in 0..MAX_REQUEST_MEDIA_ITEMS + 1 {
            let mut bytes = png.clone();
            bytes.extend((index as u32).to_le_bytes());
            let blob_ref = blobs.put_bytes(bytes).await.expect("store");
            let mut entry = image_entry(blob_ref);
            entry.entry_id = ContextEntryId::new(index as u64 + 1);
            entries.push(entry);
        }

        let mut media = RequestMedia::prepare(&blobs, &entries)
            .await
            .expect("prepare");

        let omitted = entries
            .iter()
            .filter(|entry| media.is_omitted(entry))
            .map(|entry| entry.entry_id.as_u64())
            .collect::<Vec<_>>();
        assert_eq!(omitted, (1..=10).collect::<Vec<_>>());
        let newest = entries.last().expect("newest");
        let image = media
            .image(&blobs, newest, "image/png")
            .await
            .expect("prepared image");
        assert_eq!(image.media_type, "image/png");
        assert!(
            omission_placeholder(&entries[0])
                .ends_with(" · omitted from this request to stay within provider limits]")
        );
    }

    #[test]
    fn cache_evicts_least_recently_used_entries() {
        let entry = |size: usize| ModelImage {
            media_type: "image/png".to_owned(),
            base64: "a".repeat(size),
            resize: None,
        };
        let mut cache = ImageCache::default();
        let third = CACHE_CAPACITY_BYTES / 3;
        cache.insert(BlobRef::from_bytes(b"a"), entry(third));
        cache.insert(BlobRef::from_bytes(b"b"), entry(third));
        cache.insert(BlobRef::from_bytes(b"c"), entry(third));
        assert!(cache.get(&BlobRef::from_bytes(b"a")).is_some());
        cache.insert(BlobRef::from_bytes(b"d"), entry(third));
        assert!(cache.get(&BlobRef::from_bytes(b"a")).is_some());
        assert!(cache.get(&BlobRef::from_bytes(b"b")).is_none());
        assert!(cache.bytes <= CACHE_CAPACITY_BYTES);
    }
}
