use std::fmt;
use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use serde_json::{Map, Value};

pub const WINDOW_CROP_ENV: &str = "HOLOIROH_WINDOW_CROP";
pub const MAX_CROPPED_RESPONSE_BYTES: usize = 1024 * 1024;
pub const CROPPED_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

const MODEL_COORDINATE_MAX: i64 = 1000;
const MIN_WINDOW_POINTS: f64 = 32.0;
const MIN_CROP_PIXELS: u32 = 32;
const MAX_SCALE_SKEW: f64 = 0.02;
const MAX_ASPECT_ERROR: f64 = 0.02;
const MIN_IMAGE_SCALE: f64 = 0.1;
const MAX_IMAGE_SCALE: f64 = 8.0;
const MIN_VISIBLE_FRACTION: f64 = 0.5;
const JPEG_QUALITY: u8 = 85;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }

    fn right(self) -> f64 {
        self.x + self.width
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn area(self) -> f64 {
        self.width * self.height
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > left && bottom > top).then(|| Self::new(left, top, right - left, bottom - top))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplaySnapshot {
    pub id: u32,
    pub bounds: ScreenRect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopSnapshot {
    pub owner_pid: i32,
    pub window_bounds: ScreenRect,
    pub displays: Vec<DisplaySnapshot>,
}

pub trait WindowSnapshotSource: Send + Sync {
    fn snapshot(&self) -> Result<DesktopSnapshot, CropSkipReason>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWindowSnapshotSource;

impl WindowSnapshotSource for SystemWindowSnapshotSource {
    fn snapshot(&self) -> Result<DesktopSnapshot, CropSkipReason> {
        system_snapshot()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropSkipReason {
    Disabled,
    NoImage,
    MultipleImages,
    UnsupportedImage,
    ImageDecode,
    FrontmostProcess,
    WindowList,
    WindowNotFound,
    WindowInvalid,
    DisplayList,
    DisplayMismatch,
    DisplayAmbiguous,
    WindowSpansDisplays,
    CropTooSmall,
    CropIsFullImage,
    ImageEncode,
    UnsupportedPlatform,
}

impl CropSkipReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NoImage => "no_image",
            Self::MultipleImages => "multiple_images",
            Self::UnsupportedImage => "unsupported_image",
            Self::ImageDecode => "image_decode",
            Self::FrontmostProcess => "frontmost_process",
            Self::WindowList => "window_list",
            Self::WindowNotFound => "window_not_found",
            Self::WindowInvalid => "window_invalid",
            Self::DisplayList => "display_list",
            Self::DisplayMismatch => "display_mismatch",
            Self::DisplayAmbiguous => "display_ambiguous",
            Self::WindowSpansDisplays => "window_spans_displays",
            Self::CropTooSmall => "crop_too_small",
            Self::CropIsFullImage => "crop_is_full_image",
            Self::ImageEncode => "image_encode",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropTransform {
    pub full_width: u32,
    pub full_height: u32,
    pub crop: PixelRect,
}

impl CropTransform {
    pub fn matrix(self) -> [[f64; 3]; 3] {
        [
            [
                self.crop.width as f64 / self.full_width as f64,
                0.0,
                self.crop.x as f64 * MODEL_COORDINATE_MAX as f64 / self.full_width as f64,
            ],
            [
                0.0,
                self.crop.height as f64 / self.full_height as f64,
                self.crop.y as f64 * MODEL_COORDINATE_MAX as f64 / self.full_height as f64,
            ],
            [0.0, 0.0, 1.0],
        ]
    }

    pub fn rebase(self, x: i64, y: i64) -> Result<(i64, i64), RebaseError> {
        if !(0..=MODEL_COORDINATE_MAX).contains(&x) || !(0..=MODEL_COORDINATE_MAX).contains(&y) {
            return Err(RebaseError::CoordinateRange);
        }
        let matrix = self.matrix();
        let rebased_x = (matrix[0][0] * x as f64 + matrix[0][2]).round() as i64;
        let rebased_y = (matrix[1][1] * y as f64 + matrix[1][2]).round() as i64;
        Ok((
            rebased_x.clamp(0, MODEL_COORDINATE_MAX),
            rebased_y.clamp(0, MODEL_COORDINATE_MAX),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropMetadata {
    pub full_width: u32,
    pub full_height: u32,
    pub crop_width: u32,
    pub crop_height: u32,
    pub original_jpeg_bytes: usize,
    pub cropped_jpeg_bytes: usize,
    pub resolver_latency: Duration,
    pub decode_latency: Duration,
    pub encode_latency: Duration,
}

#[derive(Debug)]
pub struct CropRequestOutcome {
    pub transform: Option<CropTransform>,
    pub metadata: Option<CropMetadata>,
    pub skip_reason: Option<CropSkipReason>,
}

impl CropRequestOutcome {
    fn skipped(reason: CropSkipReason) -> Self {
        Self {
            transform: None,
            metadata: None,
            skip_reason: Some(reason),
        }
    }
}

pub fn crop_enabled_from_env() -> bool {
    match std::env::var(WINDOW_CROP_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

pub fn crop_chat_request(
    body: &mut Value,
    source: &dyn WindowSnapshotSource,
) -> CropRequestOutcome {
    let locations = image_url_locations(body);
    if locations.is_empty() {
        return CropRequestOutcome::skipped(CropSkipReason::NoImage);
    }
    if locations.len() != 1 {
        return CropRequestOutcome::skipped(CropSkipReason::MultipleImages);
    }
    let (message_index, content_index) = locations[0];
    let Some(url) = image_url_at(body, message_index, content_index) else {
        return CropRequestOutcome::skipped(CropSkipReason::NoImage);
    };
    let Some(encoded) = jpeg_payload(url) else {
        return CropRequestOutcome::skipped(CropSkipReason::UnsupportedImage);
    };
    let Ok(jpeg_bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return CropRequestOutcome::skipped(CropSkipReason::ImageDecode);
    };

    let decode_started = Instant::now();
    let Ok(image) = image::load_from_memory(&jpeg_bytes) else {
        return CropRequestOutcome::skipped(CropSkipReason::ImageDecode);
    };
    let decode_latency = decode_started.elapsed();
    let (full_width, full_height) = (image.width(), image.height());

    let resolver_started = Instant::now();
    let snapshot = match source.snapshot() {
        Ok(snapshot) => snapshot,
        Err(reason) => return CropRequestOutcome::skipped(reason),
    };
    let crop = match resolve_crop(&snapshot, full_width, full_height) {
        Ok(crop) => crop,
        Err(reason) => return CropRequestOutcome::skipped(reason),
    };
    let resolver_latency = resolver_started.elapsed();

    let encode_started = Instant::now();
    let cropped = image.crop_imm(crop.x, crop.y, crop.width, crop.height);
    let mut cropped_jpeg = Vec::new();
    if JpegEncoder::new_with_quality(&mut cropped_jpeg, JPEG_QUALITY)
        .encode_image(&cropped)
        .is_err()
    {
        return CropRequestOutcome::skipped(CropSkipReason::ImageEncode);
    }
    let encode_latency = encode_started.elapsed();
    let cropped_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&cropped_jpeg)
    );
    if !set_image_url(body, message_index, content_index, cropped_url) {
        return CropRequestOutcome::skipped(CropSkipReason::NoImage);
    }

    CropRequestOutcome {
        transform: Some(CropTransform {
            full_width,
            full_height,
            crop,
        }),
        metadata: Some(CropMetadata {
            full_width,
            full_height,
            crop_width: crop.width,
            crop_height: crop.height,
            original_jpeg_bytes: jpeg_bytes.len(),
            cropped_jpeg_bytes: cropped_jpeg.len(),
            resolver_latency,
            decode_latency,
            encode_latency,
        }),
        skip_reason: None,
    }
}

pub fn resolve_crop(
    snapshot: &DesktopSnapshot,
    image_width: u32,
    image_height: u32,
) -> Result<PixelRect, CropSkipReason> {
    let window = snapshot.window_bounds;
    if image_width < MIN_CROP_PIXELS
        || image_height < MIN_CROP_PIXELS
        || !window.valid()
        || window.width < MIN_WINDOW_POINTS
        || window.height < MIN_WINDOW_POINTS
    {
        return Err(CropSkipReason::WindowInvalid);
    }
    if snapshot.displays.is_empty() {
        return Err(CropSkipReason::DisplayList);
    }

    let intersecting: Vec<_> = snapshot
        .displays
        .iter()
        .filter_map(|display| {
            window
                .intersection(display.bounds)
                .map(|visible| (*display, visible))
        })
        .collect();
    if intersecting.len() > 1 {
        return Err(CropSkipReason::WindowSpansDisplays);
    }
    if snapshot.displays.len() != 1 {
        return Err(CropSkipReason::DisplayAmbiguous);
    }
    let Some((target_display, visible_window)) = intersecting.first().copied() else {
        return Err(CropSkipReason::DisplayMismatch);
    };
    if visible_window.area() / window.area() < MIN_VISIBLE_FRACTION {
        return Err(CropSkipReason::WindowInvalid);
    }

    let image_aspect = image_width as f64 / image_height as f64;
    let matching: Vec<_> = snapshot
        .displays
        .iter()
        .filter(|display| {
            if !display.bounds.valid() {
                return false;
            }
            let display_aspect = display.bounds.width / display.bounds.height;
            relative_error(image_aspect, display_aspect) <= MAX_ASPECT_ERROR
        })
        .collect();
    if matching.is_empty() {
        return Err(CropSkipReason::DisplayMismatch);
    }
    if matching.len() != 1 {
        return Err(CropSkipReason::DisplayAmbiguous);
    }
    let display = matching[0];
    if display.id != target_display.id {
        return Err(CropSkipReason::DisplayMismatch);
    }

    let scale_x = image_width as f64 / display.bounds.width;
    let scale_y = image_height as f64 / display.bounds.height;
    if !(MIN_IMAGE_SCALE..=MAX_IMAGE_SCALE).contains(&scale_x)
        || !(MIN_IMAGE_SCALE..=MAX_IMAGE_SCALE).contains(&scale_y)
        || relative_error(scale_x, scale_y) > MAX_SCALE_SKEW
    {
        return Err(CropSkipReason::DisplayMismatch);
    }

    let left = ((visible_window.x - display.bounds.x) * scale_x)
        .floor()
        .clamp(0.0, image_width as f64) as u32;
    let top = ((visible_window.y - display.bounds.y) * scale_y)
        .floor()
        .clamp(0.0, image_height as f64) as u32;
    let right = ((visible_window.right() - display.bounds.x) * scale_x)
        .ceil()
        .clamp(0.0, image_width as f64) as u32;
    let bottom = ((visible_window.bottom() - display.bounds.y) * scale_y)
        .ceil()
        .clamp(0.0, image_height as f64) as u32;
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    if width < MIN_CROP_PIXELS || height < MIN_CROP_PIXELS {
        return Err(CropSkipReason::CropTooSmall);
    }
    if left == 0 && top == 0 && width == image_width && height == image_height {
        return Err(CropSkipReason::CropIsFullImage);
    }
    Ok(PixelRect {
        x: left,
        y: top,
        width,
        height,
    })
}

fn relative_error(a: f64, b: f64) -> f64 {
    (a - b).abs() / a.abs().max(b.abs()).max(f64::EPSILON)
}

fn image_url_locations(body: &Value) -> Vec<(usize, usize)> {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut locations = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (content_index, part) in content.iter().enumerate() {
            if part.get("type").and_then(Value::as_str) == Some("image_url")
                && part
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.starts_with("data:image/"))
            {
                locations.push((message_index, content_index));
            }
        }
    }
    locations
}

fn image_url_at(body: &Value, message_index: usize, content_index: usize) -> Option<&str> {
    body.pointer(&format!(
        "/messages/{message_index}/content/{content_index}/image_url/url"
    ))
    .and_then(Value::as_str)
}

fn set_image_url(
    body: &mut Value,
    message_index: usize,
    content_index: usize,
    url: String,
) -> bool {
    let Some(value) = body.pointer_mut(&format!(
        "/messages/{message_index}/content/{content_index}/image_url/url"
    )) else {
        return false;
    };
    *value = Value::String(url);
    true
}

fn jpeg_payload(url: &str) -> Option<&str> {
    let (header, payload) = url.split_once(',')?;
    let header = header.to_ascii_lowercase();
    matches!(
        header.as_str(),
        "data:image/jpeg;base64" | "data:image/jpg;base64"
    )
    .then_some(payload)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseError {
    ResponseJson,
    ResponseShape,
    StructuredOutputJson,
    StructuredOutputShape,
    UnsupportedTool,
    UnsupportedMediaType,
    CoordinateRange,
    SseShape,
    Utf8,
}

impl fmt::Display for RebaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ResponseJson => "response_json",
            Self::ResponseShape => "response_shape",
            Self::StructuredOutputJson => "structured_output_json",
            Self::StructuredOutputShape => "structured_output_shape",
            Self::UnsupportedTool => "unsupported_tool",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::CoordinateRange => "coordinate_range",
            Self::SseShape => "sse_shape",
            Self::Utf8 => "utf8",
        })
    }
}

impl std::error::Error for RebaseError {}

#[derive(Debug)]
pub struct RebasedResponse {
    pub bytes: Vec<u8>,
    pub coordinate_count: usize,
}

pub fn rebase_response(
    body: &[u8],
    content_type: Option<&str>,
    transform: CropTransform,
) -> Result<RebasedResponse, RebaseError> {
    let content_type = content_type.ok_or(RebaseError::UnsupportedMediaType)?;
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("text/event-stream") {
        rebase_sse_response(body, transform)
    } else if content_type.contains("application/json") {
        rebase_json_response(body, transform)
    } else {
        Err(RebaseError::UnsupportedMediaType)
    }
}

fn rebase_json_response(
    body: &[u8],
    transform: CropTransform,
) -> Result<RebasedResponse, RebaseError> {
    let mut response: Value =
        serde_json::from_slice(body).map_err(|_| RebaseError::ResponseJson)?;
    let choices = response
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .ok_or(RebaseError::ResponseShape)?;
    if choices.is_empty() {
        return Err(RebaseError::ResponseShape);
    }
    let mut coordinate_count = 0;
    for choice in choices {
        let content = choice
            .pointer_mut("/message/content")
            .and_then(|value| value.as_str())
            .ok_or(RebaseError::ResponseShape)?
            .to_owned();
        let rebased = rebase_structured_output(&content, transform)?;
        coordinate_count += rebased.coordinate_count;
        if rebased.coordinate_count > 0 {
            *choice
                .pointer_mut("/message/content")
                .ok_or(RebaseError::ResponseShape)? = Value::String(rebased.content);
        }
    }
    if coordinate_count == 0 {
        return Ok(RebasedResponse {
            bytes: body.to_vec(),
            coordinate_count,
        });
    }
    let bytes = serde_json::to_vec(&response).map_err(|_| RebaseError::ResponseJson)?;
    Ok(RebasedResponse {
        bytes,
        coordinate_count,
    })
}

fn rebase_sse_response(
    body: &[u8],
    transform: CropTransform,
) -> Result<RebasedResponse, RebaseError> {
    let text = std::str::from_utf8(body).map_err(|_| RebaseError::Utf8)?;
    let normalized = text.replace("\r\n", "\n");
    let mut blocks: Vec<String> = normalized
        .split("\n\n")
        .filter(|block| !block.is_empty())
        .map(str::to_owned)
        .collect();
    if blocks.is_empty() {
        return Err(RebaseError::SseShape);
    }

    let mut fragments = Vec::new();
    let mut fragment_locations = Vec::new();
    let mut saw_done = false;
    for (block_index, block) in blocks.iter().enumerate() {
        let data_lines: Vec<_> = block
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with("data:"))
            .collect();
        if data_lines.is_empty() {
            continue;
        }
        if data_lines.len() != 1 {
            return Err(RebaseError::SseShape);
        }
        let (line_index, line) = data_lines[0];
        let data = line[5..].trim_start();
        if data == "[DONE]" {
            if saw_done {
                return Err(RebaseError::SseShape);
            }
            saw_done = true;
            continue;
        }
        if saw_done {
            return Err(RebaseError::SseShape);
        }
        let event: Value = serde_json::from_str(data).map_err(|_| RebaseError::SseShape)?;
        if let Some(content) = event
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            fragments.push(content.to_owned());
            fragment_locations.push((block_index, line_index, event));
        }
    }
    if fragments.is_empty() || !saw_done {
        return Err(RebaseError::SseShape);
    }
    let rebased = rebase_structured_output(&fragments.concat(), transform)?;
    if rebased.coordinate_count == 0 {
        return Ok(RebasedResponse {
            bytes: body.to_vec(),
            coordinate_count: 0,
        });
    }

    for (fragment_index, (block_index, line_index, mut event)) in
        fragment_locations.into_iter().enumerate()
    {
        let replacement = if fragment_index == 0 {
            rebased.content.clone()
        } else {
            String::new()
        };
        *event
            .pointer_mut("/choices/0/delta/content")
            .ok_or(RebaseError::SseShape)? = Value::String(replacement);
        let mut lines: Vec<String> = blocks[block_index].lines().map(str::to_owned).collect();
        lines[line_index] = format!(
            "data: {}",
            serde_json::to_string(&event).map_err(|_| RebaseError::SseShape)?
        );
        blocks[block_index] = lines.join("\n");
    }
    let mut rebuilt = blocks.join("\n\n");
    rebuilt.push_str("\n\n");
    Ok(RebasedResponse {
        bytes: rebuilt.into_bytes(),
        coordinate_count: rebased.coordinate_count,
    })
}

struct RebasedStructuredOutput {
    content: String,
    coordinate_count: usize,
}

fn rebase_structured_output(
    content: &str,
    transform: CropTransform,
) -> Result<RebasedStructuredOutput, RebaseError> {
    let mut output: Value =
        serde_json::from_str(content).map_err(|_| RebaseError::StructuredOutputJson)?;
    let root = output
        .as_object_mut()
        .ok_or(RebaseError::StructuredOutputShape)?;
    exact_keys(root, &["thought", "tool_calls"], &["note"])?;
    require_string(root, "thought")?;
    if let Some(note) = root.get("note")
        && !note.is_null()
        && !note.is_string()
    {
        return Err(RebaseError::StructuredOutputShape);
    }
    let tools = root
        .get_mut("tool_calls")
        .and_then(Value::as_array_mut)
        .ok_or(RebaseError::StructuredOutputShape)?;
    let mut coordinate_count = 0;
    for tool in tools {
        coordinate_count += rebase_tool(tool, transform)?;
    }
    let content = if coordinate_count == 0 {
        content.to_owned()
    } else {
        serde_json::to_string(&output).map_err(|_| RebaseError::StructuredOutputJson)?
    };
    Ok(RebasedStructuredOutput {
        content,
        coordinate_count,
    })
}

fn rebase_tool(tool: &mut Value, transform: CropTransform) -> Result<usize, RebaseError> {
    let object = tool
        .as_object_mut()
        .ok_or(RebaseError::StructuredOutputShape)?;
    let name = object
        .get("tool_name")
        .and_then(Value::as_str)
        .ok_or(RebaseError::StructuredOutputShape)?
        .to_owned();
    match name.as_str() {
        "click_desktop" => {
            exact_keys(object, &["tool_name", "element", "x", "y"], &["button"])?;
            require_string(object, "element")?;
            optional_enum(object, "button", &["left", "right", "middle"])?;
            rebase_xy(object, transform)
        }
        "double_click_desktop" | "drag_to_desktop" | "move_to_desktop" => {
            exact_keys(object, &["tool_name", "element", "x", "y"], &[])?;
            require_string(object, "element")?;
            rebase_xy(object, transform)
        }
        "scroll_desktop" => {
            exact_keys(
                object,
                &["tool_name", "element", "x", "y", "direction"],
                &["scroll_size"],
            )?;
            require_string(object, "element")?;
            require_enum(object, "direction", &["up", "down", "left", "right"])?;
            optional_integer(object, "scroll_size")?;
            rebase_xy(object, transform)
        }
        "write_desktop" => {
            exact_keys(
                object,
                &["tool_name", "content"],
                &["press_enter", "overwrite"],
            )?;
            require_string(object, "content")?;
            optional_bool(object, "press_enter")?;
            optional_bool(object, "overwrite")?;
            Ok(0)
        }
        "key_down_desktop" | "key_up_desktop" => {
            exact_keys(object, &["tool_name", "key"], &[])?;
            require_string(object, "key")?;
            Ok(0)
        }
        "hotkey_desktop" => {
            exact_keys(object, &["tool_name", "keys"], &["repeat_count"])?;
            require_string_array(object, "keys", 1, 5)?;
            optional_integer(object, "repeat_count")?;
            Ok(0)
        }
        "hold_and_tap_key_desktop" => {
            exact_keys(object, &["tool_name", "hold_keys", "tap_keys"], &[])?;
            require_string_array(object, "hold_keys", 1, 3)?;
            require_string_array(object, "tap_keys", 1, 5)?;
            Ok(0)
        }
        "answer" => {
            exact_keys(object, &["tool_name", "content"], &[])?;
            require_string(object, "content")?;
            Ok(0)
        }
        "note" => {
            exact_keys(object, &["tool_name", "note"], &[])?;
            require_string(object, "note")?;
            Ok(0)
        }
        "load_skill" => {
            exact_keys(object, &["tool_name", "name"], &[])?;
            require_string(object, "name")?;
            Ok(0)
        }
        "update_plan" => {
            exact_keys(object, &["tool_name", "goals"], &[])?;
            let goals = object
                .get("goals")
                .and_then(Value::as_array)
                .ok_or(RebaseError::StructuredOutputShape)?;
            for goal in goals {
                let goal = goal.as_object().ok_or(RebaseError::StructuredOutputShape)?;
                exact_keys(goal, &["title"], &["status"])?;
                require_string(goal, "title")?;
                optional_enum(goal, "status", &["todo", "running", "done", "failed"])?;
            }
            Ok(0)
        }
        _ => Err(RebaseError::UnsupportedTool),
    }
}

fn rebase_xy(
    object: &mut Map<String, Value>,
    transform: CropTransform,
) -> Result<usize, RebaseError> {
    let x = integer(object, "x")?;
    let y = integer(object, "y")?;
    let (x, y) = transform.rebase(x, y)?;
    object.insert("x".to_owned(), Value::from(x));
    object.insert("y".to_owned(), Value::from(y));
    Ok(2)
}

fn exact_keys(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
) -> Result<(), RebaseError> {
    if required.iter().any(|key| !object.contains_key(*key))
        || object
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(RebaseError::StructuredOutputShape);
    }
    Ok(())
}

fn require_string(object: &Map<String, Value>, key: &str) -> Result<(), RebaseError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(|_| ())
        .ok_or(RebaseError::StructuredOutputShape)
}

fn integer(object: &Map<String, Value>, key: &str) -> Result<i64, RebaseError> {
    object
        .get(key)
        .and_then(Value::as_i64)
        .ok_or(RebaseError::StructuredOutputShape)
}

fn optional_integer(object: &Map<String, Value>, key: &str) -> Result<(), RebaseError> {
    if object
        .get(key)
        .is_some_and(|value| value.as_i64().is_none())
    {
        return Err(RebaseError::StructuredOutputShape);
    }
    Ok(())
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<(), RebaseError> {
    if object.get(key).is_some_and(|value| !value.is_boolean()) {
        return Err(RebaseError::StructuredOutputShape);
    }
    Ok(())
}

fn require_enum(
    object: &Map<String, Value>,
    key: &str,
    values: &[&str],
) -> Result<(), RebaseError> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(RebaseError::StructuredOutputShape)?;
    values
        .contains(&value)
        .then_some(())
        .ok_or(RebaseError::StructuredOutputShape)
}

fn optional_enum(
    object: &Map<String, Value>,
    key: &str,
    values: &[&str],
) -> Result<(), RebaseError> {
    if object.contains_key(key) {
        require_enum(object, key, values)?;
    }
    Ok(())
}

fn require_string_array(
    object: &Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<(), RebaseError> {
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or(RebaseError::StructuredOutputShape)?;
    if !(min..=max).contains(&values.len()) || values.iter().any(|value| !value.is_string()) {
        return Err(RebaseError::StructuredOutputShape);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn system_snapshot() -> Result<DesktopSnapshot, CropSkipReason> {
    use std::ffi::c_void;

    use objc2_core_foundation::{
        CFBoolean, CFDictionary, CFNumber, CFNumberType, CFString, CGRect,
    };
    use objc2_core_graphics::{
        CGDisplayBounds, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
        kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer, kCGWindowOwnerPID,
    };

    unsafe extern "C-unwind" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
        fn CGRectMakeWithDictionaryRepresentation(
            dictionary: Option<&CFDictionary>,
            rect: *mut CGRect,
        ) -> bool;
    }

    fn dictionary_value<'a, T>(dictionary: &'a CFDictionary, key: &CFString) -> Option<&'a T> {
        let key_pointer: *const CFString = key;
        let value = unsafe { dictionary.value(key_pointer.cast::<c_void>()) };
        (!value.is_null()).then(|| unsafe { &*value.cast::<T>() })
    }

    fn number_i64(dictionary: &CFDictionary, key: &CFString) -> Option<i64> {
        let number = dictionary_value::<CFNumber>(dictionary, key)?;
        let mut value = 0_i64;
        unsafe {
            number.value(
                CFNumberType::SInt64Type,
                (&mut value as *mut i64).cast::<c_void>(),
            )
        }
        .then_some(value)
    }

    fn boolean(dictionary: &CFDictionary, key: &CFString) -> Option<bool> {
        dictionary_value::<CFBoolean>(dictionary, key).map(CFBoolean::value)
    }

    fn bounds(dictionary: &CFDictionary) -> Option<ScreenRect> {
        let value = dictionary_value::<CFDictionary>(dictionary, unsafe { kCGWindowBounds })?;
        let mut rect = CGRect::ZERO;
        if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(value), &mut rect) } {
            return None;
        }
        Some(ScreenRect::new(
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        ))
    }

    let owner_pid = frontmost_pid()?;
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let windows =
        CGWindowListCopyWindowInfo(options, kCGNullWindowID).ok_or(CropSkipReason::WindowList)?;
    let mut window_bounds = None;
    for index in 0..windows.count() {
        let pointer = unsafe { windows.value_at_index(index) };
        if pointer.is_null() {
            continue;
        }
        let dictionary = unsafe { &*pointer.cast::<CFDictionary>() };
        if number_i64(dictionary, unsafe { kCGWindowOwnerPID }) != Some(owner_pid as i64)
            || number_i64(dictionary, unsafe { kCGWindowLayer }) != Some(0)
            || boolean(dictionary, unsafe { kCGWindowIsOnscreen }) != Some(true)
        {
            continue;
        }
        let Some(candidate) = bounds(dictionary) else {
            continue;
        };
        if candidate.valid()
            && candidate.width >= MIN_WINDOW_POINTS
            && candidate.height >= MIN_WINDOW_POINTS
        {
            window_bounds = Some(candidate);
            break;
        }
    }
    let window_bounds = window_bounds.ok_or(CropSkipReason::WindowNotFound)?;

    let mut ids = [0_u32; 32];
    let mut count = 0_u32;
    let error = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if error != 0 || count == 0 || count as usize > ids.len() {
        return Err(CropSkipReason::DisplayList);
    }
    let displays = ids[..count as usize]
        .iter()
        .map(|id| {
            let rect = CGDisplayBounds(*id);
            DisplaySnapshot {
                id: *id,
                bounds: ScreenRect::new(
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ),
            }
        })
        .collect();
    Ok(DesktopSnapshot {
        owner_pid,
        window_bounds,
        displays,
    })
}

#[cfg(target_os = "macos")]
fn frontmost_pid() -> Result<i32, CropSkipReason> {
    let front = Command::new("/usr/bin/lsappinfo")
        .arg("front")
        .output()
        .map_err(|_| CropSkipReason::FrontmostProcess)?;
    if !front.status.success() {
        return Err(CropSkipReason::FrontmostProcess);
    }
    let asn = String::from_utf8(front.stdout).map_err(|_| CropSkipReason::FrontmostProcess)?;
    let asn = asn.trim();
    if asn.is_empty() {
        return Err(CropSkipReason::FrontmostProcess);
    }
    let info = Command::new("/usr/bin/lsappinfo")
        .args(["info", "-only", "pid", asn])
        .output()
        .map_err(|_| CropSkipReason::FrontmostProcess)?;
    if !info.status.success() {
        return Err(CropSkipReason::FrontmostProcess);
    }
    let text = String::from_utf8(info.stdout).map_err(|_| CropSkipReason::FrontmostProcess)?;
    text.split_once('=')
        .and_then(|(_, value)| value.trim().trim_matches('"').parse().ok())
        .filter(|pid| *pid > 0)
        .ok_or(CropSkipReason::FrontmostProcess)
}

#[cfg(not(target_os = "macos"))]
fn system_snapshot() -> Result<DesktopSnapshot, CropSkipReason> {
    Err(CropSkipReason::UnsupportedPlatform)
}
