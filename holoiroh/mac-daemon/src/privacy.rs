//! Redacts sensitive OCR text and faces before cloud egress.
//!
//! Apple Vision processes each image on-device for optical character recognition (OCR) and face
//! detection. Deterministic detectors cover common PII and credential formats. Foundation's
//! `NSDataDetector` supplements address, phone, and link detection. NaturalLanguage's `NLTagger`
//! is a secondary person, organization, and place detector; witnessed fixtures on this macOS show
//! that it can return no entity for valid names, so deterministic high-confidence forms remain the
//! primary path. Imageproc paints opaque padded boxes over the complete OCR region and every face.
//!
//! [`redact_sensitive_content`] returns separate sensitive-text and face counts.
//! [`ocr_and_redact`] keeps the original tuple API for existing callers. Both functions fail closed
//! when image encoding, a Vision request, or an Apple text detector fails.

use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;
use objc2::AnyThread;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSObject, NSRange, NSString};
use objc2_vision::{
    VNFaceObservation, VNImageBasedRequest, VNImageOption, VNImageRequestHandler,
    VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
};
use regex::Regex;
use std::sync::OnceLock;

#[link(name = "NaturalLanguage", kind = "framework")]
unsafe extern "C" {}

objc2::extern_class!(
    #[unsafe(super(VNImageBasedRequest, VNRequest, NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct VNDetectFaceRectanglesRequest;
);

impl VNDetectFaceRectanglesRequest {
    objc2::extern_methods!(
        #[unsafe(method(init))]
        #[unsafe(method_family = init)]
        unsafe fn init(this: Allocated<Self>) -> Retained<Self>;

        #[unsafe(method(results))]
        #[unsafe(method_family = none)]
        unsafe fn results(&self) -> Option<Retained<NSArray<VNFaceObservation>>>;
    );
}

objc2::extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct NLTagger;
);

impl NLTagger {
    objc2::extern_methods!(
        #[unsafe(method(initWithTagSchemes:))]
        #[unsafe(method_family = init)]
        unsafe fn init_with_tag_schemes(
            this: Allocated<Self>,
            schemes: &NSArray<NSString>,
        ) -> Retained<Self>;

        #[unsafe(method(setString:))]
        #[unsafe(method_family = none)]
        unsafe fn set_string(&self, text: Option<&NSString>);

        #[unsafe(method(tagAtIndex:unit:scheme:tokenRange:))]
        #[unsafe(method_family = none)]
        unsafe fn tag_at_index(
            &self,
            index: usize,
            unit: usize,
            scheme: &NSString,
            token_range: *mut NSRange,
        ) -> Option<Retained<NSString>>;
    );
}

objc2::extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct NSTextCheckingResult;
);

impl NSTextCheckingResult {
    objc2::extern_methods!(
        #[unsafe(method(resultType))]
        #[unsafe(method_family = none)]
        unsafe fn result_type(&self) -> usize;
    );
}

objc2::extern_class!(
    #[unsafe(super(NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    struct NSDataDetector;
);

impl NSDataDetector {
    objc2::extern_methods!(
        #[unsafe(method(dataDetectorWithTypes:error:))]
        #[unsafe(method_family = none)]
        unsafe fn detector_with_types(
            checking_types: usize,
            error: *mut *mut AnyObject,
        ) -> Option<Retained<Self>>;

        #[unsafe(method(matchesInString:options:range:))]
        #[unsafe(method_family = none)]
        unsafe fn matches(
            &self,
            text: &NSString,
            options: usize,
            range: NSRange,
        ) -> Retained<NSArray<NSTextCheckingResult>>;
    );
}

const PII_PADDING_PX: u32 = 2;
const FACE_PADDING_PX: u32 = 8;
const BLOCK_COLOR: Rgba<u8> = Rgba([20, 20, 20, 255]);
const LABEL_COLOR: Rgba<u8> = Rgba([255, 255, 255, 255]);
const NS_TEXT_CHECKING_ADDRESS: usize = 1 << 4;
const NS_TEXT_CHECKING_LINK: usize = 1 << 5;
const NS_TEXT_CHECKING_PHONE: usize = 1 << 11;
const NL_TOKEN_UNIT_WORD: usize = 0;

/// A recognized text block in top-left-origin pixel coordinates.
#[derive(Debug, Clone)]
pub struct TextRegion {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A detected face in top-left-origin pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The output from one on-device detection and redaction pass.
#[derive(Debug)]
pub struct RedactionResult {
    pub image: DynamicImage,
    pub pii_count: usize,
    pub face_count: usize,
}

/// The sensitive-data category assigned to a recognized text block.
///
/// The original four variants remain unchanged for API compatibility. `DetectorFailure` is the
/// fail-closed category and never contains detector or matched-text details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiiCategory {
    Email,
    Phone,
    Ssn,
    CreditCard,
    Person,
    Address,
    Organization,
    Url,
    Credential,
    DetectorFailure,
}

impl PiiCategory {
    fn label_prefix(self) -> &'static str {
        match self {
            PiiCategory::Email => "EMAIL",
            PiiCategory::Phone => "PHONE",
            PiiCategory::Ssn => "SSN",
            PiiCategory::CreditCard => "CARD",
            PiiCategory::Person => "PERSON",
            PiiCategory::Address => "ADDRESS",
            PiiCategory::Organization => "ORG",
            PiiCategory::Url => "URL",
            PiiCategory::Credential => "CREDENTIAL",
            PiiCategory::DetectorFailure => "PRIVATE",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PixelRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl PixelRegion {
    fn padded(self, padding: u32, image_width: u32, image_height: u32) -> Option<Rect> {
        let left = self.x.saturating_sub(padding).min(image_width);
        let top = self.y.saturating_sub(padding).min(image_height);
        let right = self
            .x
            .saturating_add(self.width)
            .saturating_add(padding)
            .min(image_width);
        let bottom = self
            .y
            .saturating_add(self.height)
            .saturating_add(padding)
            .min(image_height);
        let width = right.saturating_sub(left);
        let height = bottom.saturating_sub(top);
        (width > 0 && height > 0).then(|| Rect::at(left as i32, top as i32).of_size(width, height))
    }
}

struct VisionRegions {
    text: Vec<TextRegion>,
    faces: Vec<FaceRegion>,
}

fn static_regex(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(&RE, r"(?i)\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b")
}

fn phone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(&RE, r"\b(\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]\d{3}[-.\s]\d{4}\b")
}

fn ssn_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(&RE, r"\b\d{3}-\d{2}-\d{4}\b")
}

fn credit_card_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(&RE, r"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{4}\b")
}

fn credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r"(?x)
        \b(?:AKIA|ASIA|AIDA|AROA|AIPA|ANPA|ANVA)[A-Z0-9]{16}\b
        |\bgh[pousr]_[A-Za-z0-9]{36,255}\b
        |\bgithub_pat_[A-Za-z0-9_]{40,255}\b
        |(?i:\bxox[baprs][\p{P}\s]+[A-Za-z0-9][A-Za-z0-9\p{P}\s]{9,255}\b)
        |\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b
        |-----BEGIN\x20(?:RSA\x20|EC\x20|OPENSSH\x20|DSA\x20|ENCRYPTED\x20)?PRIVATE\x20KEY-----
        |\bsk_live_[A-Za-z0-9]{16,255}\b
        |\bAIza[0-9A-Za-z_-]{35}\b
        ",
    )
}

fn assigned_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r#"(?i)\b(?:api[_-]?(?:key|secret)|access[_-]?token|auth[_-]?token|client[_-]?secret|secret[_-]?key|password|passwd)\s*[:=]\s*["']?[A-Za-z0-9_./+~=-]{12,}|\bslack\s+token\s*[:=]"#,
    )
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(&RE, r"(?i)\bbearer\s+[A-Za-z0-9_./+~=-]{20,512}\b")
}

fn address_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r"(?i)\b(?:P\.?O\.?\s+Box\s+\d{1,8}|\d{1,6}\s+(?:[a-z0-9.'-]+\s+){0,6}(?:street|st|road|rd|avenue|ave|boulevard|blvd|lane|ln|drive|dr|court|ct|way|parkway|pkwy|loop|terrace|place|pl))\b",
    )
}

fn organization_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r"\b(?:[A-Z][A-Za-z0-9&.'-]*\s+){0,5}(?:Inc(?:orporated)?|LLC|L\.L\.C\.|Ltd\.?|Limited|Corp(?:oration)?|Company|Co\.|LLP|PLC|GmbH|Foundation|University|Association)\b",
    )
}

fn explicit_person_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r"\b(?:(?:Mr|Mrs|Ms|Miss|Dr|Prof)\.?\s+|(?:name|customer|patient|employee|contact)\s*[:=]\s*)[A-Z][A-Za-z'-]{1,}(?:\s+[A-Z][A-Za-z'-]{1,}){1,3}\b",
    )
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    static_regex(
        &RE,
        r"(?i)\b(?:https?://|www\.)[^\s<>]{4,}|\b[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z]{2,}){1,3}(?:/[^\s<>]*)?",
    )
}

fn looks_like_title_case_name(text: &str) -> bool {
    const UI_WORDS: &[&str] = &[
        "about",
        "account",
        "apply",
        "back",
        "cancel",
        "changes",
        "close",
        "continue",
        "copy",
        "details",
        "done",
        "edit",
        "enable",
        "general",
        "help",
        "home",
        "learn",
        "more",
        "next",
        "notifications",
        "open",
        "policy",
        "preferences",
        "privacy",
        "retry",
        "save",
        "search",
        "security",
        "settings",
        "sign",
        "submit",
        "terms",
        "update",
        "welcome",
    ];

    let words: Vec<&str> = text
        .trim()
        .trim_matches(|character: char| !character.is_alphanumeric())
        .split_whitespace()
        .collect();
    if !(2..=4).contains(&words.len()) {
        return false;
    }

    words.iter().all(|word| {
        let word = word.trim_matches(|character: char| !character.is_alphabetic());
        let mut characters = word.chars();
        let Some(first) = characters.next() else {
            return false;
        };
        first.is_uppercase()
            && characters
                .all(|character| character.is_lowercase() || character == '-' || character == '\'')
            && !UI_WORDS.contains(&word.to_ascii_lowercase().as_str())
    })
}

fn detect_with_data_detector(text: &str) -> anyhow::Result<Option<PiiCategory>> {
    unsafe {
        let detector = NSDataDetector::detector_with_types(
            NS_TEXT_CHECKING_ADDRESS | NS_TEXT_CHECKING_LINK | NS_TEXT_CHECKING_PHONE,
            std::ptr::null_mut(),
        )
        .ok_or_else(|| anyhow::anyhow!("NSDataDetector initialization failed"))?;
        let ns_text = NSString::from_str(text);
        let matches = detector.matches(&ns_text, 0, NSRange::new(0, ns_text.length()));
        for result in matches.iter() {
            let result_type = result.result_type();
            if result_type & NS_TEXT_CHECKING_ADDRESS != 0 {
                return Ok(Some(PiiCategory::Address));
            }
            if result_type & NS_TEXT_CHECKING_PHONE != 0 {
                return Ok(Some(PiiCategory::Phone));
            }
            if result_type & NS_TEXT_CHECKING_LINK != 0 {
                return Ok(Some(PiiCategory::Url));
            }
        }
    }
    Ok(None)
}

fn detect_with_natural_language(text: &str) -> anyhow::Result<Option<PiiCategory>> {
    unsafe {
        let scheme = NSString::from_str("NameType");
        let schemes = NSArray::from_slice(&[scheme.as_ref()]);
        let tagger = NLTagger::init_with_tag_schemes(NLTagger::alloc(), &schemes);
        let ns_text = NSString::from_str(text);
        tagger.set_string(Some(&ns_text));

        let length = ns_text.length();
        let mut index = 0usize;
        while index < length {
            let mut token_range = NSRange::new(0, 0);
            let tag = tagger.tag_at_index(index, NL_TOKEN_UNIT_WORD, &scheme, &mut token_range);
            if token_range.length == 0
                || token_range.location > length
                || token_range.location.saturating_add(token_range.length) > length
            {
                return Err(anyhow::anyhow!("NLTagger returned an invalid token range"));
            }
            if let Some(tag) = tag {
                match tag.to_string().as_str() {
                    "PersonalName" => return Ok(Some(PiiCategory::Person)),
                    "OrganizationName" => return Ok(Some(PiiCategory::Organization)),
                    "PlaceName" => return Ok(Some(PiiCategory::Address)),
                    _ => {}
                }
            }
            index = token_range
                .location
                .saturating_add(token_range.length)
                .max(index + 1);
        }
    }
    Ok(None)
}

/// Detects sensitive text without network access or live credential validation.
///
/// Deterministic formats run first. Apple's on-device detectors supplement them. Detector errors
/// are returned without including the input or a matched substring.
pub fn detect_sensitive_text(text: &str) -> anyhow::Result<Option<PiiCategory>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    if credential_re().is_match(text)
        || assigned_secret_re().is_match(text)
        || bearer_re().is_match(text)
    {
        return Ok(Some(PiiCategory::Credential));
    }
    if email_re().is_match(text) {
        return Ok(Some(PiiCategory::Email));
    }
    if ssn_re().is_match(text) {
        return Ok(Some(PiiCategory::Ssn));
    }
    if credit_card_re().is_match(text) {
        return Ok(Some(PiiCategory::CreditCard));
    }
    if phone_re().is_match(text) {
        return Ok(Some(PiiCategory::Phone));
    }
    if address_re().is_match(text) {
        return Ok(Some(PiiCategory::Address));
    }
    if organization_re().is_match(text) {
        return Ok(Some(PiiCategory::Organization));
    }
    if explicit_person_re().is_match(text) || looks_like_title_case_name(text) {
        return Ok(Some(PiiCategory::Person));
    }
    if url_re().is_match(text) {
        return Ok(Some(PiiCategory::Url));
    }
    if let Some(category) = detect_with_data_detector(text)? {
        return Ok(Some(category));
    }
    detect_with_natural_language(text)
}

/// Returns the first sensitive-data category that matches `text`.
///
/// This preserves the original infallible API. An Apple detector hard failure maps to
/// `DetectorFailure`, which causes the complete OCR region to be redacted rather than passed
/// through.
pub fn detect_pii(text: &str) -> Option<PiiCategory> {
    detect_sensitive_text(text).unwrap_or(Some(PiiCategory::DetectorFailure))
}

fn vision_rect_to_pixels(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    image_width: u32,
    image_height: u32,
) -> Option<PixelRegion> {
    if image_width == 0
        || image_height == 0
        || ![x, y, width, height].iter().all(|value| value.is_finite())
        || width <= 0.0
        || height <= 0.0
    {
        return None;
    }

    let left_normalized = x.clamp(0.0, 1.0);
    let right_normalized = (x + width).clamp(0.0, 1.0);
    let bottom_normalized = y.clamp(0.0, 1.0);
    let top_normalized = (y + height).clamp(0.0, 1.0);
    if right_normalized <= left_normalized || top_normalized <= bottom_normalized {
        return None;
    }

    let left = (left_normalized * image_width as f64).floor() as u32;
    let right = (right_normalized * image_width as f64).ceil() as u32;
    let top = ((1.0 - top_normalized) * image_height as f64).floor() as u32;
    let bottom = ((1.0 - bottom_normalized) * image_height as f64).ceil() as u32;
    let pixel_width = right.saturating_sub(left);
    let pixel_height = bottom.saturating_sub(top);
    (pixel_width > 0 && pixel_height > 0).then_some(PixelRegion {
        x: left,
        y: top,
        width: pixel_width,
        height: pixel_height,
    })
}

fn detect_vision_regions(image: &DynamicImage) -> anyhow::Result<VisionRegions> {
    let image_width = image.width();
    let image_height = image.height();
    let mut png_bytes = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        image
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|error| anyhow::anyhow!("failed to encode image for Vision: {error}"))?;
    }

    unsafe {
        let ns_data = NSData::with_bytes(&png_bytes);
        let options: Retained<NSDictionary<VNImageOption, AnyObject>> = NSDictionary::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &ns_data,
            &options,
        );
        let text_request = VNRecognizeTextRequest::init(VNRecognizeTextRequest::alloc());
        let face_request =
            VNDetectFaceRectanglesRequest::init(VNDetectFaceRectanglesRequest::alloc());
        let text_request_base: &VNRequest = &text_request;
        let face_request_base: &VNRequest = &face_request;
        let requests: Retained<NSArray<VNRequest>> =
            NSArray::from_slice(&[text_request_base, face_request_base]);

        handler
            .performRequests_error(&requests)
            .map_err(|error| anyhow::anyhow!("Vision request batch failed: {error:?}"))?;

        let text_observations = text_request
            .results()
            .ok_or_else(|| anyhow::anyhow!("VNRecognizeTextRequest returned no results array"))?;
        let face_observations = face_request.results().ok_or_else(|| {
            anyhow::anyhow!("VNDetectFaceRectanglesRequest returned no results array")
        })?;

        let mut text = Vec::new();
        for observation in text_observations.iter() {
            let Some(observation) = observation.downcast_ref::<VNRecognizedTextObservation>()
            else {
                continue;
            };
            let candidates = observation.topCandidates(1);
            let Some(top_candidate) = candidates.iter().next() else {
                continue;
            };
            let bounding_box = observation.boundingBox();
            let Some(region) = vision_rect_to_pixels(
                bounding_box.origin.x,
                bounding_box.origin.y,
                bounding_box.size.width,
                bounding_box.size.height,
                image_width,
                image_height,
            ) else {
                continue;
            };
            text.push(TextRegion {
                text: top_candidate.string().to_string(),
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            });
        }

        let mut faces = Vec::new();
        for observation in face_observations.iter() {
            let Some(observation) = observation.downcast_ref::<VNFaceObservation>() else {
                continue;
            };
            let bounding_box = observation.boundingBox();
            let Some(region) = vision_rect_to_pixels(
                bounding_box.origin.x,
                bounding_box.origin.y,
                bounding_box.size.width,
                bounding_box.size.height,
                image_width,
                image_height,
            ) else {
                continue;
            };
            faces.push(FaceRegion {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            });
        }

        Ok(VisionRegions { text, faces })
    }
}

/// Runs the shared on-device Vision batch and returns recognized text regions.
pub fn ocr_text_regions(image: &DynamicImage) -> anyhow::Result<Vec<TextRegion>> {
    Ok(detect_vision_regions(image)?.text)
}

/// Loads a system font for redaction labels. Missing fonts disable labels, not opaque boxes.
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

fn text_as_pixel_region(region: &TextRegion) -> PixelRegion {
    PixelRegion {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

fn face_as_pixel_region(region: &FaceRegion) -> PixelRegion {
    PixelRegion {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

fn redact_detected_regions(
    image: &DynamicImage,
    text_regions: &[TextRegion],
    face_regions: &[FaceRegion],
) -> RedactionResult {
    let mut canvas: RgbaImage = image.to_rgba8();
    let (image_width, image_height) = canvas.dimensions();
    let font = load_label_font();
    let mut pii_count = 0usize;
    let mut per_category_counter = std::collections::HashMap::new();

    for region in text_regions {
        let Some(category) = detect_pii(&region.text) else {
            continue;
        };
        let Some(rect) =
            text_as_pixel_region(region).padded(PII_PADDING_PX, image_width, image_height)
        else {
            continue;
        };
        draw_filled_rect_mut(&mut canvas, rect, BLOCK_COLOR);
        pii_count += 1;

        let counter = per_category_counter
            .entry(category.label_prefix())
            .or_insert(0u32);
        *counter += 1;
        if let Some(font) = &font {
            let label = format!("«{}_{}»", category.label_prefix(), counter);
            let scale = ab_glyph::PxScale::from((rect.height() as f32 * 0.7).max(8.0));
            draw_text_mut(
                &mut canvas,
                LABEL_COLOR,
                rect.left() + 2,
                rect.top(),
                scale,
                font,
                &label,
            );
        }
    }

    let mut face_count = 0usize;
    for region in face_regions {
        let Some(rect) =
            face_as_pixel_region(region).padded(FACE_PADDING_PX, image_width, image_height)
        else {
            continue;
        };
        draw_filled_rect_mut(&mut canvas, rect, BLOCK_COLOR);
        face_count += 1;
    }

    RedactionResult {
        image: DynamicImage::ImageRgba8(canvas),
        pii_count,
        face_count,
    }
}

/// Redacts matching text regions. The returned count includes only boxes that were painted.
pub fn redact_image(image: &DynamicImage, regions: &[TextRegion]) -> (DynamicImage, usize) {
    let result = redact_detected_regions(image, regions, &[]);
    (result.image, result.pii_count)
}

/// Runs one Vision batch and redacts all detected sensitive text and faces.
///
/// The function returns an error when image encoding or either Vision request fails. A text
/// detector error redacts that complete OCR region with the fail-closed `PRIVATE` category.
pub fn redact_sensitive_content(image: &DynamicImage) -> anyhow::Result<RedactionResult> {
    let regions = detect_vision_regions(image)?;
    Ok(redact_detected_regions(
        image,
        &regions.text,
        &regions.faces,
    ))
}

/// Preserves the original `(image, pii_count)` API while also redacting faces.
///
/// Use [`redact_sensitive_content`] when the caller needs the face count.
pub fn ocr_and_redact(image: &DynamicImage) -> anyhow::Result<(DynamicImage, usize)> {
    let result = redact_sensitive_content(image)?;
    Ok((result.image, result.pii_count))
}
