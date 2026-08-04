//! Bounded, read-only macOS Accessibility observations for start-of-turn grounding.

use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::hash::Hash;
use std::time::{Duration, Instant};

pub const BRIDGE_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_total_bytes: usize,
    pub max_string_bytes: usize,
    pub deadline: Duration,
    pub ax_message_timeout: Duration,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_nodes: 32,
            max_total_bytes: 12 * 1024,
            max_string_bytes: 256,
            deadline: Duration::from_millis(300),
            ax_message_timeout: Duration::from_millis(40),
        }
    }
}

impl SnapshotLimits {
    fn hardened(mut self) -> Self {
        self.max_depth = self.max_depth.min(16);
        self.max_nodes = self.max_nodes.min(256);
        self.max_total_bytes = self.max_total_bytes.min(64 * 1024);
        self.max_string_bytes = self.max_string_bytes.min(1_024);
        self.deadline = self.deadline.min(Duration::from_secs(1));
        self.ax_message_timeout = self.ax_message_timeout.min(Duration::from_millis(250));
        self
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct ElementFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug)]
pub struct NodeObservation<K> {
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub value: Option<String>,
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub frame: Option<ElementFrame>,
    pub actionable: bool,
    pub secure: bool,
    pub children: Vec<K>,
    pub children_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotOmission {
    AccessibilityDenied,
    NoFocusedApplication,
    Deadline,
    AxError { operation: &'static str, code: i32 },
    FrontmostLookup { operation: &'static str, code: i64 },
    InvalidAxValue,
    Degenerate,
    Serialization,
    UnsupportedPlatform,
}

impl fmt::Display for SnapshotOmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessibilityDenied => formatter.write_str("Accessibility access is denied"),
            Self::NoFocusedApplication => formatter.write_str("no focused application AX element"),
            Self::Deadline => formatter.write_str("Accessibility snapshot deadline exceeded"),
            Self::AxError { operation, code } => {
                write!(
                    formatter,
                    "Accessibility API error {code} during {operation}"
                )
            }
            Self::FrontmostLookup { operation, code } => {
                write!(
                    formatter,
                    "frontmost-app lookup error {code} during {operation}"
                )
            }
            Self::InvalidAxValue => formatter.write_str("Accessibility returned an invalid value"),
            Self::Degenerate => formatter.write_str("Accessibility tree was degenerate"),
            Self::Serialization => formatter.write_str("Accessibility JSON serialization failed"),
            Self::UnsupportedPlatform => {
                formatter.write_str("Accessibility is only available on macOS")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SnapshotJson {
    pub json: String,
    pub node_count: usize,
    pub byte_count: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub enum SnapshotAttempt {
    Captured {
        snapshot: SnapshotJson,
        elapsed: Duration,
    },
    Omitted {
        reason: SnapshotOmission,
        elapsed: Duration,
    },
}

#[derive(Serialize)]
struct SerializedSnapshot<'a> {
    schema_version: u8,
    source: &'static str,
    scope: &'static str,
    trust: &'static str,
    node_count: usize,
    truncated: bool,
    nodes: &'a [SerializedNode],
}

#[derive(Clone, Serialize)]
struct SerializedNode {
    id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<usize>,
    depth: usize,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subrole: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame: Option<ElementFrame>,
}

pub fn snapshot_from_observer<K, F>(
    root: K,
    limits: SnapshotLimits,
    observer: F,
) -> Result<SnapshotJson, SnapshotOmission>
where
    K: Clone + Eq + Hash,
    F: FnMut(&K) -> Result<NodeObservation<K>, SnapshotOmission>,
{
    snapshot_from_observer_until(root, limits.hardened(), None, observer)
}

fn snapshot_from_observer_until<K, F>(
    root: K,
    limits: SnapshotLimits,
    deadline: Option<Instant>,
    mut observer: F,
) -> Result<SnapshotJson, SnapshotOmission>
where
    K: Clone + Eq + Hash,
    F: FnMut(&K) -> Result<NodeObservation<K>, SnapshotOmission>,
{
    if limits.max_nodes == 0 || limits.max_total_bytes == 0 {
        return Err(SnapshotOmission::Degenerate);
    }

    let mut queue = VecDeque::from([(root, 0usize, None)]);
    let mut visited = HashSet::new();
    let mut nodes = Vec::new();
    let mut truncated = false;

    while let Some((key, depth, parent_id)) = queue.pop_front() {
        check_deadline(deadline)?;
        if !visited.insert(key.clone()) {
            truncated = true;
            continue;
        }
        if depth > limits.max_depth || nodes.len() >= limits.max_nodes {
            truncated = true;
            break;
        }

        let observed = observer(&key)?;
        let (role, role_cut) = truncate_utf8(observed.role, limits.max_string_bytes);
        if role.is_empty() {
            if depth == 0 {
                return Err(SnapshotOmission::Degenerate);
            }
            truncated = true;
            continue;
        }
        let (subrole, subrole_cut) = truncate_optional(observed.subrole, limits.max_string_bytes);
        let secure = observed.secure || is_secure_role(&role, subrole.as_deref());
        let (title, title_cut) = truncate_optional(observed.title, limits.max_string_bytes);
        let (description, description_cut) =
            truncate_optional(observed.description, limits.max_string_bytes);
        let (value, value_cut) = if secure {
            (None, observed.value.is_some())
        } else {
            truncate_optional(observed.value, limits.max_string_bytes)
        };
        truncated |= role_cut
            || subrole_cut
            || title_cut
            || description_cut
            || value_cut
            || observed.children_truncated;

        let id = nodes.len();
        nodes.push(SerializedNode {
            id,
            parent_id,
            depth,
            role,
            subrole,
            title,
            description,
            value,
            enabled: observed.enabled,
            focused: observed.focused,
            frame: observed.actionable.then_some(observed.frame).flatten(),
        });

        let candidate = serialize_nodes(&nodes, truncated)?;
        if candidate.len() > limits.max_total_bytes {
            nodes.pop();
            truncated = true;
            break;
        }

        if depth == limits.max_depth {
            truncated |= !observed.children.is_empty();
        } else {
            let remaining = limits
                .max_nodes
                .saturating_sub(nodes.len().saturating_add(queue.len()));
            truncated |= observed.children.len() > remaining;
            queue.extend(
                observed
                    .children
                    .into_iter()
                    .take(remaining)
                    .map(|child| (child, depth + 1, Some(id))),
            );
        }
    }

    if !queue.is_empty() {
        truncated = true;
    }
    if is_degenerate(&nodes) {
        return Err(SnapshotOmission::Degenerate);
    }

    let json = serialize_nodes(&nodes, truncated)?;
    if json.len() > limits.max_total_bytes {
        return Err(SnapshotOmission::Degenerate);
    }
    Ok(SnapshotJson {
        node_count: nodes.len(),
        byte_count: json.len(),
        json,
        truncated,
    })
}

fn serialize_nodes(nodes: &[SerializedNode], truncated: bool) -> Result<String, SnapshotOmission> {
    serde_json::to_string(&SerializedSnapshot {
        schema_version: 1,
        source: "macos_accessibility",
        scope: "frontmost_application_start_of_turn",
        trust: "untrusted_observation_data",
        node_count: nodes.len(),
        truncated,
        nodes,
    })
    .map_err(|_| SnapshotOmission::Serialization)
}

fn is_degenerate(nodes: &[SerializedNode]) -> bool {
    match nodes {
        [] => true,
        [root] => {
            root.title.is_none()
                && root.description.is_none()
                && root.subrole.is_none()
                && root.value.is_none()
                && root.frame.is_none()
        }
        _ => false,
    }
}

fn is_secure_role(role: &str, subrole: Option<&str>) -> bool {
    [Some(role), subrole].into_iter().flatten().any(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("secure") || value.contains("password")
    })
}

fn truncate_optional(value: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    match value {
        Some(value) => {
            let (value, truncated) = truncate_utf8(value, max_bytes);
            ((!value.is_empty()).then_some(value), truncated)
        }
        None => (None, false),
    }
}

fn truncate_utf8(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn check_deadline(deadline: Option<Instant>) -> Result<(), SnapshotOmission> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(SnapshotOmission::Deadline)
    } else {
        Ok(())
    }
}

pub fn snapshot_frontmost_application() -> SnapshotAttempt {
    snapshot_frontmost_application_with_limits(SnapshotLimits::default())
}

pub fn snapshot_frontmost_application_with_limits(limits: SnapshotLimits) -> SnapshotAttempt {
    let started = Instant::now();
    let result = platform::snapshot_frontmost_application(limits.hardened(), started);
    let elapsed = started.elapsed();
    match result {
        Ok(snapshot) => SnapshotAttempt::Captured { snapshot, elapsed },
        Err(reason) => SnapshotAttempt::Omitted { reason, elapsed },
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
    use objc2_core_foundation::{
        CFArray, CFBoolean, CFNumber, CFRange, CFRetained, CFString, CFStringBuiltInEncodings,
        CFType, CGRect,
    };
    use std::ptr::{NonNull, null};

    #[repr(C)]
    struct ProcessSerialNumber {
        high: u32,
        low: u32,
    }

    unsafe extern "C-unwind" {
        fn GetFrontProcess(process: *mut ProcessSerialNumber) -> i16;
        fn GetProcessPID(process: *const ProcessSerialNumber, pid: *mut libc::pid_t) -> u32;
        fn AXUIElementCreateApplication(pid: libc::pid_t) -> Option<NonNull<AXUIElement>>;
    }

    struct Attributes {
        role: CFRetained<CFString>,
        subrole: CFRetained<CFString>,
        title: CFRetained<CFString>,
        description: CFRetained<CFString>,
        value: CFRetained<CFString>,
        enabled: CFRetained<CFString>,
        focused: CFRetained<CFString>,
        frame: CFRetained<CFString>,
        children: CFRetained<CFString>,
        protected_content: CFRetained<CFString>,
    }

    impl Attributes {
        fn new() -> Self {
            Self {
                role: CFString::from_static_str("AXRole"),
                subrole: CFString::from_static_str("AXSubrole"),
                title: CFString::from_static_str("AXTitle"),
                description: CFString::from_static_str("AXDescription"),
                value: CFString::from_static_str("AXValue"),
                enabled: CFString::from_static_str("AXEnabled"),
                focused: CFString::from_static_str("AXFocused"),
                frame: CFString::from_static_str("AXFrame"),
                children: CFString::from_static_str("AXChildren"),
                protected_content: CFString::from_static_str("AXProtectedContent"),
            }
        }
    }

    struct Inspector {
        attributes: Attributes,
        limits: SnapshotLimits,
        deadline: Instant,
    }

    impl Inspector {
        fn observe(
            &mut self,
            element: &CFRetained<AXUIElement>,
        ) -> Result<NodeObservation<CFRetained<AXUIElement>>, SnapshotOmission> {
            let role = self
                .string_attribute(element, &self.attributes.role)?
                .ok_or(SnapshotOmission::InvalidAxValue)?;
            let subrole = self.string_attribute(element, &self.attributes.subrole)?;
            let protected = if non_sensitive_value_role(&role) {
                self.bool_attribute(element, &self.attributes.protected_content)?
                    .unwrap_or(false)
            } else {
                false
            };
            let secure = protected || is_secure_role(&role, subrole.as_deref());
            let title = self.string_attribute(element, &self.attributes.title)?;
            let description = self.string_attribute(element, &self.attributes.description)?;
            let enabled = self.bool_attribute(element, &self.attributes.enabled)?;
            let focused = self.bool_attribute(element, &self.attributes.focused)?;
            let actionable = actionable_role(&role);
            let frame = if actionable {
                self.frame_attribute(element, &self.attributes.frame)?
            } else {
                None
            };
            let value = if !secure && non_sensitive_value_role(&role) {
                self.value_attribute(element, &self.attributes.value)?
            } else {
                None
            };
            let (children, children_truncated) = self.children(element)?;
            Ok(NodeObservation {
                role,
                subrole,
                title,
                description,
                value,
                enabled,
                focused,
                frame,
                actionable,
                secure,
                children,
                children_truncated,
            })
        }

        fn copied_attribute(
            &self,
            element: &AXUIElement,
            attribute: &CFString,
        ) -> Result<Option<CFRetained<CFType>>, SnapshotOmission> {
            check_deadline(Some(self.deadline))?;
            let mut raw: *const CFType = null();
            let error = unsafe { element.copy_attribute_value(attribute, NonNull::from(&mut raw)) };
            check_deadline(Some(self.deadline))?;
            match error {
                AXError::Success => NonNull::new(raw.cast_mut())
                    .map(|raw| unsafe { CFRetained::from_raw(raw) })
                    .ok_or(SnapshotOmission::InvalidAxValue)
                    .map(Some),
                AXError::AttributeUnsupported | AXError::NoValue | AXError::CannotComplete => {
                    Ok(None)
                }
                other => Err(SnapshotOmission::AxError {
                    operation: "AXUIElementCopyAttributeValue",
                    code: other.0,
                }),
            }
        }

        fn string_attribute(
            &self,
            element: &AXUIElement,
            attribute: &CFString,
        ) -> Result<Option<String>, SnapshotOmission> {
            let Some(value) = self.copied_attribute(element, attribute)? else {
                return Ok(None);
            };
            let Ok(value) = value.downcast::<CFString>() else {
                return Ok(None);
            };
            Ok(Some(bounded_cf_string(
                &value,
                self.limits.max_string_bytes,
            )))
        }

        fn bool_attribute(
            &self,
            element: &AXUIElement,
            attribute: &CFString,
        ) -> Result<Option<bool>, SnapshotOmission> {
            let Some(value) = self.copied_attribute(element, attribute)? else {
                return Ok(None);
            };
            Ok(value
                .downcast::<CFBoolean>()
                .ok()
                .map(|value| value.value()))
        }

        fn value_attribute(
            &self,
            element: &AXUIElement,
            attribute: &CFString,
        ) -> Result<Option<String>, SnapshotOmission> {
            let Some(value) = self.copied_attribute(element, attribute)? else {
                return Ok(None);
            };
            let value = match value.downcast::<CFString>() {
                Ok(value) => bounded_cf_string(&value, self.limits.max_string_bytes),
                Err(value) => match value.downcast::<CFBoolean>() {
                    Ok(value) => value.value().to_string(),
                    Err(value) => match value.downcast::<CFNumber>() {
                        Ok(value) => value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        Err(_) => String::new(),
                    },
                },
            };
            Ok((!value.is_empty()).then_some(value))
        }

        fn frame_attribute(
            &self,
            element: &AXUIElement,
            attribute: &CFString,
        ) -> Result<Option<ElementFrame>, SnapshotOmission> {
            let Some(value) = self.copied_attribute(element, attribute)? else {
                return Ok(None);
            };
            let Ok(value) = value.downcast::<AXValue>() else {
                return Ok(None);
            };
            if unsafe { value.r#type() } != AXValueType::CGRect {
                return Ok(None);
            }
            let mut rect = CGRect::default();
            if !unsafe { value.value(AXValueType::CGRect, NonNull::from(&mut rect).cast()) } {
                return Ok(None);
            }
            let values = [
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            ];
            if !values.iter().all(|value| value.is_finite()) {
                return Ok(None);
            }
            Ok(Some(ElementFrame {
                x: normalize_zero(rect.origin.x),
                y: normalize_zero(rect.origin.y),
                width: normalize_zero(rect.size.width),
                height: normalize_zero(rect.size.height),
            }))
        }

        fn children(
            &self,
            element: &AXUIElement,
        ) -> Result<(Vec<CFRetained<AXUIElement>>, bool), SnapshotOmission> {
            check_deadline(Some(self.deadline))?;
            let mut count = 0;
            let count_error = unsafe {
                element.attribute_value_count(&self.attributes.children, NonNull::from(&mut count))
            };
            check_deadline(Some(self.deadline))?;
            match count_error {
                AXError::Success => {}
                AXError::AttributeUnsupported | AXError::NoValue => return Ok((Vec::new(), false)),
                AXError::CannotComplete => return Ok((Vec::new(), true)),
                other => {
                    return Err(SnapshotOmission::AxError {
                        operation: "AXUIElementGetAttributeValueCount",
                        code: other.0,
                    });
                }
            }
            if count <= 0 {
                return Ok((Vec::new(), false));
            }
            let copied_count = (count as usize).min(self.limits.max_nodes);
            let mut raw: *const CFArray = null();
            let error = unsafe {
                element.copy_attribute_values(
                    &self.attributes.children,
                    0,
                    copied_count as isize,
                    NonNull::from(&mut raw),
                )
            };
            check_deadline(Some(self.deadline))?;
            if error == AXError::CannotComplete {
                return Ok((Vec::new(), true));
            }
            if error != AXError::Success {
                return Err(SnapshotOmission::AxError {
                    operation: "AXUIElementCopyAttributeValues",
                    code: error.0,
                });
            }
            let array: CFRetained<CFArray> = NonNull::new(raw.cast_mut())
                .map(|raw| unsafe { CFRetained::from_raw(raw) })
                .ok_or(SnapshotOmission::InvalidAxValue)?;
            let typed = unsafe { array.cast_unchecked::<CFType>() };
            let children = typed
                .iter()
                .filter_map(|value| value.downcast::<AXUIElement>().ok())
                .collect();
            Ok((children, count as usize > copied_count))
        }
    }

    pub(super) fn snapshot_frontmost_application(
        limits: SnapshotLimits,
        started: Instant,
    ) -> Result<SnapshotJson, SnapshotOmission> {
        if !unsafe { objc2_application_services::AXIsProcessTrusted() } {
            return Err(SnapshotOmission::AccessibilityDenied);
        }
        let deadline = started + limits.deadline;
        check_deadline(Some(deadline))?;
        let timeout_seconds = limits.ax_message_timeout.as_secs_f32().max(0.001);
        let attributes = Attributes::new();
        let focused = frontmost_application_from_process_manager(deadline)?;
        let timeout_error = unsafe { focused.set_messaging_timeout(timeout_seconds) };
        if timeout_error != AXError::Success {
            return Err(SnapshotOmission::AxError {
                operation: "AXUIElementSetMessagingTimeout(frontmost)",
                code: timeout_error.0,
            });
        }
        let mut inspector = Inspector {
            attributes,
            limits,
            deadline,
        };
        snapshot_from_observer_until(focused, limits, Some(deadline), |element| {
            inspector.observe(element)
        })
    }

    fn frontmost_application_from_process_manager(
        deadline: Instant,
    ) -> Result<CFRetained<AXUIElement>, SnapshotOmission> {
        check_deadline(Some(deadline))?;
        let mut process_serial_number = ProcessSerialNumber { high: 0, low: 0 };
        let front_status = unsafe { GetFrontProcess(&mut process_serial_number) };
        if front_status != 0 {
            return Err(SnapshotOmission::FrontmostLookup {
                operation: "GetFrontProcess",
                code: front_status as i64,
            });
        }
        let mut pid: libc::pid_t = 0;
        let pid_status = unsafe { GetProcessPID(&process_serial_number, &mut pid) };
        check_deadline(Some(deadline))?;
        if pid_status != 0 || pid <= 0 {
            return Err(SnapshotOmission::FrontmostLookup {
                operation: "GetProcessPID",
                code: pid_status as i64,
            });
        }
        let application = unsafe { AXUIElementCreateApplication(pid) }
            .ok_or(SnapshotOmission::NoFocusedApplication)?;
        Ok(unsafe { CFRetained::from_raw(application) })
    }

    fn bounded_cf_string(value: &CFString, max_bytes: usize) -> String {
        if max_bytes == 0 {
            return String::new();
        }
        let mut bytes = vec![0u8; max_bytes];
        let mut used = 0;
        let length = value.length();
        unsafe {
            value.bytes(
                CFRange::new(0, length),
                CFStringBuiltInEncodings::EncodingUTF8.0,
                0,
                false,
                bytes.as_mut_ptr(),
                max_bytes as isize,
                &mut used,
            );
        }
        bytes.truncate(used.max(0) as usize);
        String::from_utf8(bytes).unwrap_or_default()
    }

    fn actionable_role(role: &str) -> bool {
        matches!(
            role,
            "AXButton"
                | "AXCheckBox"
                | "AXComboBox"
                | "AXDisclosureTriangle"
                | "AXLink"
                | "AXMenuButton"
                | "AXMenuItem"
                | "AXPopUpButton"
                | "AXRadioButton"
                | "AXSearchField"
                | "AXSlider"
                | "AXSwitch"
                | "AXTab"
                | "AXTextArea"
                | "AXTextField"
        )
    }

    fn non_sensitive_value_role(role: &str) -> bool {
        matches!(
            role,
            "AXCheckBox"
                | "AXDisclosureTriangle"
                | "AXIncrementor"
                | "AXProgressIndicator"
                | "AXRadioButton"
                | "AXSlider"
                | "AXSwitch"
                | "AXTab"
        )
    }

    fn normalize_zero(value: f64) -> f64 {
        if value == 0.0 { 0.0 } else { value }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn snapshot_frontmost_application(
        _limits: SnapshotLimits,
        _started: Instant,
    ) -> Result<SnapshotJson, SnapshotOmission> {
        Err(SnapshotOmission::UnsupportedPlatform)
    }
}
