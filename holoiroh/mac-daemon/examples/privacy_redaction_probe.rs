//! Executable witness for local sensitive-text, OCR, face, and opaque redaction behavior.
//!
//! The probe renders synthetic fixtures, performs no network calls, reports only fixture IDs and
//! aggregate counts, and measures this machine rather than quoting external latency. Pass a local
//! face image path to re-witness Vision face-box drawing.
//!
//! `cargo run -p holoiroh-daemon --example privacy_redaction_probe -- /tmp/face.jpg`

use holoiroh_daemon::privacy::{
    PiiCategory, detect_sensitive_text, load_label_font, ocr_text_regions, redact_sensitive_content,
};
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use imageproc::drawing::draw_text_mut;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const BLOCK_COLOR: Rgba<u8> = Rgba([20, 20, 20, 255]);
const TEXT_RUNS: usize = 200;
const IMAGE_RUNS: usize = 5;

const SENSITIVE_FIXTURES: &[(&str, &str, PiiCategory)] = &[
    ("email", "contact jane.doe@example.com", PiiCategory::Email),
    ("phone", "call 555-123-4567", PiiCategory::Phone),
    ("ssn", "ssn 123-45-6789", PiiCategory::Ssn),
    ("card", "card 4111 1111 1111 1111", PiiCategory::CreditCard),
    ("person-direct", "Ada Lovelace", PiiCategory::Person),
    (
        "person-natural-language",
        "Ada Lovelace wrote the first algorithm.",
        PiiCategory::Person,
    ),
    (
        "address",
        "1 Infinite Loop, Cupertino, CA 95014",
        PiiCategory::Address,
    ),
    (
        "organization",
        "Microsoft Corporation headquarters",
        PiiCategory::Organization,
    ),
    (
        "url",
        "https://example.com/private/account",
        PiiCategory::Url,
    ),
    (
        "credential-aws",
        "AKIAIOSFODNN7EXAMPLE",
        PiiCategory::Credential,
    ),
    (
        "credential-github",
        "ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        PiiCategory::Credential,
    ),
    (
        "credential-slack",
        concat!("Slack token: xoxb-", "123456789012-abcdefghijklmnopqrstuvwx"),
        PiiCategory::Credential,
    ),
    (
        "credential-jwt",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlMTIzNDU2",
        PiiCategory::Credential,
    ),
    (
        "credential-pem",
        "-----BEGIN PRIVATE KEY-----",
        PiiCategory::Credential,
    ),
    (
        "credential-stripe",
        concat!("sk_", "live_51AbCdEfGhIjKlMnOpQrStUv"),
        PiiCategory::Credential,
    ),
    (
        "credential-google",
        "AIzaAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        PiiCategory::Credential,
    ),
    (
        "credential-bearer",
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789",
        PiiCategory::Credential,
    ),
    (
        "credential-assignment",
        "api_secret = 0123456789abcdef0123456789abcdef",
        PiiCategory::Credential,
    ),
];

const BENIGN_FIXTURES: &[(&str, &str)] = &[
    ("ui-open-settings", "Open Settings"),
    ("ui-save-changes", "Save Changes"),
    ("ui-account-details", "Account Details"),
    ("ui-privacy-policy", "Privacy Policy"),
    ("ui-search", "Search"),
    (
        "random-prose",
        "The quick brown fox jumps over the lazy dog.",
    ),
    (
        "ordinary-status",
        "The application finished loading and is ready to use.",
    ),
    (
        "random-identifier",
        "build artifact a8f31c0d7e4b92f6 completed normally",
    ),
];

fn render_lines(lines: &[&str]) -> DynamicImage {
    let font = load_label_font().expect("a system font is required to render OCR fixtures");
    let line_height = 68u32;
    let mut canvas = RgbaImage::from_pixel(
        2200,
        line_height * lines.len() as u32 + 32,
        Rgba([255, 255, 255, 255]),
    );
    for (index, text) in lines.iter().enumerate() {
        draw_text_mut(
            &mut canvas,
            Rgba([0, 0, 0, 255]),
            20,
            16 + index as i32 * line_height as i32,
            ab_glyph::PxScale::from(36.0),
            &font,
            text,
        );
    }
    DynamicImage::ImageRgba8(canvas)
}

fn average_duration(total: Duration, runs: usize) -> Duration {
    Duration::from_secs_f64(total.as_secs_f64() / runs as f64)
}

fn witness_pure_text() {
    let mut category_counts = BTreeMap::new();
    for (id, text, expected) in SENSITIVE_FIXTURES {
        let actual = detect_sensitive_text(text).unwrap_or_else(|error| {
            panic!("{id}: detector failed without sensitive output: {error}")
        });
        assert_eq!(actual, Some(*expected), "{id}: wrong sensitive category");
        *category_counts
            .entry(format!("{expected:?}"))
            .or_insert(0usize) += 1;
    }

    let mut false_positives = Vec::new();
    for (id, text) in BENIGN_FIXTURES {
        if detect_sensitive_text(text)
            .unwrap_or_else(|error| panic!("{id}: detector failed: {error}"))
            .is_some()
        {
            false_positives.push(*id);
        }
    }
    assert!(
        false_positives.is_empty(),
        "benign fixtures redacted: {false_positives:?}"
    );

    println!(
        "pure text: sensitive={} benign={} false_positives={} categories={category_counts:?}",
        SENSITIVE_FIXTURES.len(),
        BENIGN_FIXTURES.len(),
        false_positives.len(),
    );
}

fn measure_text_latency() {
    for (_, text, _) in SENSITIVE_FIXTURES {
        let _ = detect_sensitive_text(text).expect("warm-up text detection must succeed");
    }
    for (id, text, _) in SENSITIVE_FIXTURES {
        let started = Instant::now();
        for _ in 0..TEXT_RUNS {
            assert!(
                detect_sensitive_text(text)
                    .expect("timed text detection must succeed")
                    .is_some()
            );
        }
        println!(
            "text latency: fixture={id} runs={TEXT_RUNS} mean_us={:.1}",
            average_duration(started.elapsed(), TEXT_RUNS).as_secs_f64() * 1_000_000.0
        );
    }
}

fn witness_rendered_fixtures() {
    for (id, text, _) in SENSITIVE_FIXTURES {
        let image = render_lines(&[text]);
        let result = redact_sensitive_content(&image)
            .unwrap_or_else(|error| panic!("{id}: rendered image detection failed: {error}"));
        if result.pii_count == 0 {
            let diagnostics: Vec<(usize, bool, bool, usize, Option<PiiCategory>)> =
                ocr_text_regions(&image)
                    .expect("diagnostic OCR must succeed")
                    .iter()
                    .map(|region| {
                        let lowercase = region.text.to_ascii_lowercase();
                        (
                            region.text.chars().count(),
                            lowercase.contains("slack"),
                            lowercase.contains("xox"),
                            region
                                .text
                                .chars()
                                .filter(|character| character.is_ascii_punctuation())
                                .count(),
                            detect_sensitive_text(&region.text)
                                .expect("diagnostic detection must succeed"),
                        )
                    })
                    .collect();
            panic!(
                "{id}: rendered sensitive fixture was not redacted; OCR diagnostics={diagnostics:?}"
            );
        }
        assert_ne!(
            image.to_rgba8().as_raw(),
            result.image.to_rgba8().as_raw(),
            "{id}: redaction count changed no pixels"
        );
        println!(
            "rendered fixture: id={id} redacted_regions={} faces={}",
            result.pii_count, result.face_count
        );
    }

    let benign_lines: Vec<&str> = BENIGN_FIXTURES.iter().map(|(_, text)| *text).collect();
    let benign_image = render_lines(&benign_lines);
    let benign_result =
        redact_sensitive_content(&benign_image).expect("rendered benign image must process");
    assert_eq!(
        benign_result.pii_count, 0,
        "rendered benign UI/prose image must have zero sensitive regions"
    );
    assert_eq!(benign_result.face_count, 0);
    assert_eq!(
        benign_image.to_rgba8().as_raw(),
        benign_result.image.to_rgba8().as_raw(),
        "rendered benign image must pass through byte-identically"
    );
    println!(
        "rendered benign image: fixtures={} redacted_regions=0 faces=0",
        BENIGN_FIXTURES.len()
    );
}

fn measure_complete_image_latency() {
    let mut lines: Vec<&str> = SENSITIVE_FIXTURES
        .iter()
        .map(|(_, text, _)| *text)
        .collect();
    lines.extend(BENIGN_FIXTURES.iter().map(|(_, text)| *text));
    let image = render_lines(&lines);
    let warm = redact_sensitive_content(&image).expect("composite warm-up must succeed");
    assert!(warm.pii_count >= SENSITIVE_FIXTURES.len());

    let mut counts = Vec::with_capacity(IMAGE_RUNS);
    let started = Instant::now();
    for _ in 0..IMAGE_RUNS {
        let result = redact_sensitive_content(&image).expect("timed composite pass must succeed");
        counts.push((result.pii_count, result.face_count));
    }
    let elapsed = started.elapsed();
    assert!(
        counts.iter().all(|count| *count == counts[0]),
        "repeated image counts must be deterministic: {counts:?}"
    );
    println!(
        "complete image latency: runs={IMAGE_RUNS} mean_ms={:.3} dimensions={}x{} sensitive_regions={} faces={}",
        average_duration(elapsed, IMAGE_RUNS).as_secs_f64() * 1_000.0,
        image.width(),
        image.height(),
        counts[0].0,
        counts[0].1,
    );
}

fn assert_face_box_was_painted(path: &Path) {
    assert!(
        path.is_file(),
        "face witness path must name a file: {path:?}"
    );
    let original = image::open(path).expect("face witness path must contain a supported image");
    let result =
        redact_sensitive_content(&original).expect("valid face image must redact successfully");
    assert!(
        result.face_count > 0,
        "Vision must detect at least one face in the supplied local image"
    );
    assert_eq!(original.dimensions(), result.image.dimensions());

    let original = original.to_rgba8();
    let redacted = result.image.to_rgba8();
    let (width, height) = redacted.dimensions();
    let mut changed_to_block = vec![false; width as usize * height as usize];
    for (x, y, pixel) in redacted.enumerate_pixels() {
        let index = y as usize * width as usize + x as usize;
        changed_to_block[index] = *pixel == BLOCK_COLOR && original.get_pixel(x, y) != pixel;
    }

    let mut visited = vec![false; changed_to_block.len()];
    let mut dense_rectangles = 0usize;
    for start in 0..changed_to_block.len() {
        if !changed_to_block[start] || visited[start] {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut pixels = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        while let Some(index) = stack.pop() {
            let x = (index % width as usize) as u32;
            let y = (index / width as usize) as u32;
            pixels += 1;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            for (next_x, next_y) in [
                (x.wrapping_sub(1), y),
                (x.saturating_add(1), y),
                (x, y.wrapping_sub(1)),
                (x, y.saturating_add(1)),
            ] {
                if next_x >= width || next_y >= height {
                    continue;
                }
                let next = next_y as usize * width as usize + next_x as usize;
                if changed_to_block[next] && !visited[next] {
                    visited[next] = true;
                    stack.push(next);
                }
            }
        }
        let bounding_area = (max_x - min_x + 1) as usize * (max_y - min_y + 1) as usize;
        if pixels > 64 && pixels * 4 >= bounding_area * 3 {
            dense_rectangles += 1;
        }
    }

    assert!(
        dense_rectangles > 0,
        "face detection reported boxes but no dense filled rectangle was painted"
    );
    assert!(
        redacted
            .pixels()
            .filter(|pixel| **pixel == BLOCK_COLOR)
            .all(|pixel| pixel[3] == 255),
        "every face block pixel must be opaque"
    );
    println!(
        "face image: faces={} opaque_dense_rectangles={dense_rectangles}",
        result.face_count
    );
}

fn main() {
    witness_pure_text();
    measure_text_latency();
    witness_rendered_fixtures();
    measure_complete_image_latency();

    if let Some(path) = std::env::args_os().nth(1) {
        assert_face_box_was_painted(Path::new(&path));
    } else {
        println!("face image: not supplied; existing optional local-image witness not run");
    }

    println!("privacy_redaction_probe: OK");
}
