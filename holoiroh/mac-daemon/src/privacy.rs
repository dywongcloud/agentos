//! This module redacts PII in images on-device, before the images leave the loopback boundary
//! to the Tinfoil cloud inference backend. `Cargo.toml` documented this module's existence and
//! dependency set (`image`/`imageproc`/`ab_glyph`/`regex`/`objc2-vision`) when the crate's own
//! dependency-justification comments were first written. No one ever created or `mod`-declared
//! the module itself. This file is that missing implementation. This file is not new scope
//! invented for the Tinfoil integration task.
//!
//! ## Pipeline
//!
//! 1. **OCR** (`ocr_text_regions`): Apple's Vision framework (`VNImageRequestHandler` +
//!    `VNRecognizeTextRequest`, via `objc2-vision`) detects every text region in the image and
//!    its bounding box. This step runs entirely on-device, with no network call, matching this
//!    codebase's no-cloud-by-default posture ([`crate::local_model`]'s module doc) for anything
//!    that can stay local.
//! 2. **Detect** (`detect_pii`): this step matches each recognized text string against a small
//!    set of regexes for common PII shapes (email, phone number, US SSN-shaped,
//!    credit-card-shaped).
//! 3. **Redact** (`redact_image`): this step paints a solid block over every matched region's
//!    bounding box, with a placeholder label (`«EMAIL_1»`, `«PHONE_2»`, ...) on top. A human
//!    glancing at the redacted image can still tell *that* something was removed, and *what
//!    kind*. The original value is not recoverable from the image itself. This process is
//!    one-way: no vault maps placeholders back to original values, because the redacted image
//!    only ever travels *outbound* to Tinfoil. Nothing downstream ever needs to reverse it.
//!
//! This module is ported as a PATTERN from `dael-amz/browser-agent-privacy-layer` ("PLVA"), per
//! `Cargo.toml`'s existing comment. That repo is unlicensed and not viable to vendor. So this
//! module reimplements its OCR+regex+placeholder-token architecture natively, rather than
//! importing it.

use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation,
    VNRequest,
};
use regex::Regex;
use std::sync::OnceLock;

/// One block of text that Vision recognized, with its bounding box in **pixel** coordinates.
/// The origin is top-left; `y` grows downward. [`ocr_text_regions`] already converts this from
/// Vision's normalized bottom-left-origin convention. So callers never need to know about that
/// flip.
#[derive(Debug, Clone)]
pub struct TextRegion {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Which PII category a [`TextRegion`] matched. This module uses the category only to pick the
/// placeholder label prefix (`EMAIL`, `PHONE`, `SSN`, `CARD`). This module never logs or
/// persists the category with the matched value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiiCategory {
    Email,
    Phone,
    Ssn,
    CreditCard,
}

impl PiiCategory {
    fn label_prefix(self) -> &'static str {
        match self {
            PiiCategory::Email => "EMAIL",
            PiiCategory::Phone => "PHONE",
            PiiCategory::Ssn => "SSN",
            PiiCategory::CreditCard => "CARD",
        }
    }
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b").expect("static regex")
    })
}

fn phone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]\d{3}[-.\s]\d{4}\b").expect("static regex")
    })
}

fn ssn_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("static regex"))
}

fn credit_card_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{4}\b").expect("static regex")
    })
}

/// Runs every PII regex against `text`. Returns the first category that matches. This function
/// checks the regexes in email/phone/ssn/credit-card order. A region rarely matches more than
/// one shape. The redaction only needs one label, not an exhaustive list.
pub fn detect_pii(text: &str) -> Option<PiiCategory> {
    if email_re().is_match(text) {
        return Some(PiiCategory::Email);
    }
    if ssn_re().is_match(text) {
        return Some(PiiCategory::Ssn);
    }
    if credit_card_re().is_match(text) {
        return Some(PiiCategory::CreditCard);
    }
    if phone_re().is_match(text) {
        return Some(PiiCategory::Phone);
    }
    None
}

/// Runs on-device OCR (Apple Vision) over `image`. Returns every detected text region with its
/// pixel-space bounding box. This is a real, synchronous Vision call. It needs no async
/// handling. Vision's `performRequests` is itself synchronous on the calling thread, matching
/// `permissions.rs`'s own plain-synchronous-TCC-query posture for other Vision-adjacent calls.
pub fn ocr_text_regions(image: &DynamicImage) -> anyhow::Result<Vec<TextRegion>> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());

    // Re-encode as PNG bytes: VNImageRequestHandler's initWithData:options: expects data in a
    // format CIImage can decode (docs: "See CIImage imageWithData for supported format"), and
    // the raw RgbaImage buffer is not itself such a format.
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        image
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| anyhow::anyhow!("failed to encode image to PNG for OCR: {e}"))?;
    }

    let regions = unsafe {
        let ns_data = NSData::with_bytes(&png_bytes);
        let options: Retained<NSDictionary<VNImageOption, AnyObject>> = NSDictionary::new();

        let handler_alloc = VNImageRequestHandler::alloc();
        let handler = VNImageRequestHandler::initWithData_options(
            handler_alloc,
            &ns_data,
            &options,
        );

        let text_request = VNRecognizeTextRequest::init(VNRecognizeTextRequest::alloc());

        let requests: Retained<NSArray<VNRequest>> =
            NSArray::from_slice(&[text_request.as_ref()]);

        // VNImageRequestHandler was already constructed with the image (initWithData:options:
        // above), so its performRequests:error: takes only the request list -- unlike
        // VNSequenceRequestHandler's performRequests:onImageData:error: (a different class, for
        // per-frame analysis), which is not what a one-shot OCR pass needs.
        handler
            .performRequests_error(&requests)
            .map_err(|err| anyhow::anyhow!("Vision performRequests failed: {err:?}"))?;

        let observations = text_request
            .results()
            .ok_or_else(|| anyhow::anyhow!("VNRecognizeTextRequest returned no results array"))?;

        let mut out = Vec::new();
        for observation in observations.iter() {
            let observation: &VNRecognizedTextObservation =
                match observation.downcast_ref::<VNRecognizedTextObservation>() {
                    Some(o) => o,
                    None => continue,
                };
            let candidates = observation.topCandidates(1);
            let Some(top) = candidates.iter().next() else {
                continue;
            };
            let text = top.string().to_string();

            // Vision's boundingBox is normalized [0,1] with a BOTTOM-LEFT origin (Core Image /
            // Core Graphics convention). Flip Y and scale to pixels so callers get ordinary
            // top-left-origin pixel coordinates matching `image`/`imageproc`'s own convention.
            let bb = observation.boundingBox();
            let px_x = (bb.origin.x * width as f64).round().max(0.0) as u32;
            let px_w = (bb.size.width * width as f64).round().max(0.0) as u32;
            let px_h = (bb.size.height * height as f64).round().max(0.0) as u32;
            let top_normalized_y = 1.0 - (bb.origin.y + bb.size.height);
            let px_y = (top_normalized_y * height as f64).round().max(0.0) as u32;

            out.push(TextRegion {
                text,
                x: px_x,
                y: px_y,
                width: px_w,
                height: px_h,
            });
        }
        out
    };

    Ok(regions)
}

/// Best-effort system font for placeholder labels. Returns `None`, rather than erroring, when
/// no font is available. A missing font degrades to "redaction box with no label text" instead
/// of blocking the redaction itself. The box is what actually protects privacy. The label is a
/// cosmetic aid for a human glancing at the image.
pub fn load_label_font() -> Option<ab_glyph::FontArc> {
    const CANDIDATE_PATHS: &[&str] = &[
        "/System/Library/Fonts/Monaco.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
    ];
    for path in CANDIDATE_PATHS {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = ab_glyph::FontArc::try_from_vec(bytes) {
                return Some(font);
            }
        }
    }
    None
}

/// Redacts every region in `regions` that [`detect_pii`] flags. This function paints a solid
/// block over the region's bounding box, padded by [`REDACT_PADDING_PX`] on each side. The
/// padding stops OCR's occasionally tight box from leaving a sliver of the original glyph
/// visible at the edge. This function also draws a `«CATEGORY_N»` placeholder label on top,
/// when a system font is available. Returns the redacted image and the count of regions
/// actually redacted. A count of 0 means the image had no detected PII. The image still passes
/// through byte-for-byte re-encoded. It is not literally unmodified.
pub fn redact_image(image: &DynamicImage, regions: &[TextRegion]) -> (DynamicImage, usize) {
    const REDACT_PADDING_PX: i32 = 2;
    const BLOCK_COLOR: Rgba<u8> = Rgba([20, 20, 20, 255]);
    const LABEL_COLOR: Rgba<u8> = Rgba([255, 255, 255, 255]);

    let mut canvas: RgbaImage = image.to_rgba8();
    let font = load_label_font();
    let mut redacted_count = 0usize;
    let mut per_category_counter = std::collections::HashMap::new();

    for region in regions {
        let Some(category) = detect_pii(&region.text) else {
            continue;
        };
        redacted_count += 1;

        let rect_x = (region.x as i32 - REDACT_PADDING_PX).max(0);
        let rect_y = (region.y as i32 - REDACT_PADDING_PX).max(0);
        let rect_w = region.width + (2 * REDACT_PADDING_PX) as u32;
        let rect_h = region.height + (2 * REDACT_PADDING_PX) as u32;
        if rect_w == 0 || rect_h == 0 {
            continue;
        }

        let rect = Rect::at(rect_x, rect_y).of_size(rect_w, rect_h);
        draw_filled_rect_mut(&mut canvas, rect, BLOCK_COLOR);

        let counter = per_category_counter.entry(category.label_prefix()).or_insert(0u32);
        *counter += 1;
        let label = format!("«{}_{}»", category.label_prefix(), counter);

        if let Some(font) = &font {
            let scale = ab_glyph::PxScale::from((rect_h as f32 * 0.7).max(8.0));
            draw_text_mut(&mut canvas, LABEL_COLOR, rect_x + 2, rect_y, scale, font, &label);
        }
    }

    (DynamicImage::ImageRgba8(canvas), redacted_count)
}

/// Convenience one-shot function: OCR, then redact. This is the entry point that
/// [`crate::tinfoil_proxy`] and [`crate::tinfoil_vision`] call before any image bytes leave the
/// loopback boundary.
pub fn ocr_and_redact(image: &DynamicImage) -> anyhow::Result<(DynamicImage, usize)> {
    let regions = ocr_text_regions(image)?;
    Ok(redact_image(image, &regions))
}
