//! CI witness for `privacy.rs`'s on-device OCR + PII redaction pipeline. Real, deterministic
//! code path (Apple Vision framework via `objc2-vision`) on a synthetic in-memory image --
//! `VNImageRequestHandler` processes provided image data, not a live capture, so this needs no
//! Screen Recording/TCC grant and runs headlessly on CI's `macos-latest` runner, same posture as
//! `permissions_probe`'s plain synchronous TCC-state queries (which also need no grant).
//!
//!   cargo run --example privacy_redaction_probe -p holoiroh-daemon

use holoiroh_daemon::privacy::{detect_pii, load_label_font, ocr_and_redact, PiiCategory};
use image::{Rgba, RgbaImage};
use imageproc::drawing::draw_text_mut;

fn main() {
    // --- detect_pii: pure regex logic, no OCR/Vision involved ---
    assert_eq!(detect_pii("contact me at jane.doe@example.com"), Some(PiiCategory::Email));
    assert_eq!(detect_pii("call 555-123-4567"), Some(PiiCategory::Phone));
    assert_eq!(detect_pii("ssn 123-45-6789"), Some(PiiCategory::Ssn));
    assert_eq!(detect_pii("card 4111 1111 1111 1111"), Some(PiiCategory::CreditCard));
    assert_eq!(detect_pii("just some ordinary sentence"), None);
    println!("detect_pii: OK -- 4 categories match, plain text does not");

    // --- ocr_and_redact: real Vision OCR + imageproc redaction on a synthetic image ---
    let mut canvas = RgbaImage::from_pixel(800, 200, Rgba([255, 255, 255, 255]));
    let font = load_label_font().expect(
        "no usable system font found for rendering the synthetic test image -- \
         this probe cannot construct its own input without one",
    );
    let scale = ab_glyph::PxScale::from(48.0);
    draw_text_mut(
        &mut canvas,
        Rgba([0, 0, 0, 255]),
        20,
        60,
        scale,
        &font,
        "Email me at jane.doe@example.com",
    );
    let image = image::DynamicImage::ImageRgba8(canvas);

    let (redacted, count) = ocr_and_redact(&image).expect("ocr_and_redact must not error on a valid image");
    assert!(
        count > 0,
        "Vision OCR + regex must find and redact the rendered email address, found 0 regions to redact"
    );
    println!("ocr_and_redact: OK -- found and redacted {count} PII region(s)");

    // The redacted image must differ from the original (something was actually painted over) --
    // a real assertion the pipeline mutated pixels, not just returned the input unchanged.
    let orig_rgba = image.to_rgba8();
    let redacted_rgba = redacted.to_rgba8();
    assert_eq!(orig_rgba.dimensions(), redacted_rgba.dimensions(), "redaction must not resize the image");
    let differs = orig_rgba.pixels().zip(redacted_rgba.pixels()).any(|(a, b)| a != b);
    assert!(differs, "redacted image is pixel-identical to the original -- redaction did not actually paint anything");
    println!("pixel diff: OK -- redacted image differs from the original");

    // A clean image (no PII) must pass through with 0 redactions and be unmodified.
    let clean_canvas = RgbaImage::from_pixel(400, 100, Rgba([255, 255, 255, 255]));
    let clean_image = image::DynamicImage::ImageRgba8(clean_canvas);
    let (clean_redacted, clean_count) =
        ocr_and_redact(&clean_image).expect("ocr_and_redact must not error on a blank image");
    assert_eq!(clean_count, 0, "a blank image must have 0 redactions, got {clean_count}");
    assert_eq!(
        clean_image.to_rgba8().as_raw(),
        clean_redacted.to_rgba8().as_raw(),
        "a PII-free image must pass through byte-identical"
    );
    println!("clean image: OK -- 0 redactions, pixel-identical passthrough");

    println!(
        "privacy_redaction_probe: OK -- detect_pii regexes match correctly, real on-device Vision OCR finds and redacts injected PII, and a clean image passes through unmodified."
    );
}
