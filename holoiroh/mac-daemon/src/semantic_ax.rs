use holoiroh_wire::ActionId;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::accessibility_tree::{SnapshotAttempt, snapshot_frontmost_application};
use crate::action_executor::{
    ActionProposal, CommitAction, DesktopAction, FreshTargetState, NavigationAction,
    ObservationRef, PrimitiveAdapter, TargetRef, canonical_proposal_digest,
};
use crate::remote_input;

pub const MAX_AX_ELEMENTS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticAxElement {
    pub bundle_id: String,
    pub window_id: String,
    pub element_id: String,
    pub role: String,
    pub title: String,
    pub value: Option<String>,
    pub focused: bool,
    pub enabled: bool,
    pub sensitive: bool,
    pub credential: bool,
    pub bounds: Option<(i32, i32, i32, i32)>,
}

pub trait SemanticAxSource {
    type Error;

    fn observe(&mut self) -> Result<Vec<SemanticAxElement>, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxAdapterError<E> {
    Source(E),
    Unresolved,
    Ambiguous,
    Disabled,
    Credential,
    Sensitive,
    MissingBounds,
    NotFocused,
    DraftTooLarge,
    Unsupported,
}

pub struct AxPrimitiveAdapter<S> {
    source: S,
    resolved: Option<SemanticAxElement>,
    handles: std::collections::HashMap<String, SemanticAxElement>,
    observation_sequence: u64,
}

impl<S> AxPrimitiveAdapter<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            resolved: None,
            handles: std::collections::HashMap::new(),
            observation_sequence: 0,
        }
    }

    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }
}

impl<S: SemanticAxSource> AxPrimitiveAdapter<S> {
    pub fn observation_json(&mut self) -> Result<String, AxAdapterError<S::Error>> {
        let elements: Vec<_> = self
            .source
            .observe()
            .map_err(AxAdapterError::Source)?
            .into_iter()
            .take(MAX_AX_ELEMENTS)
            .collect();
        self.observation_sequence = self.observation_sequence.wrapping_add(1);
        self.handles.clear();
        let public: Vec<_> = elements
            .into_iter()
            .enumerate()
            .map(|(index, element)| {
                let handle = digest_parts(
                    b"holoiroh/ax-target-handle/1\0",
                    &[
                        &self.observation_sequence.to_string(),
                        &index.to_string(),
                        &element.bundle_id,
                        &element.window_id,
                        &element.element_id,
                    ],
                );
                let public = PublicElement::new(&handle, &element);
                self.handles.insert(handle, element);
                public
            })
            .collect();
        serde_json::to_string(&public).map_err(|_| AxAdapterError::Unsupported)
    }

    pub fn resolve_proposal(
        &mut self,
        goal_id: String,
        intent_digest: String,
        run_id: String,
        task_id: String,
        action_id: ActionId,
        element_id: &str,
        action: DesktopAction,
    ) -> Result<ActionProposal, AxAdapterError<S::Error>> {
        let expected = self.handles.remove(element_id).ok_or(AxAdapterError::Unresolved)?;
        let element = self.resolve_fresh_identity(&expected)?;
        if element.credential {
            return Err(AxAdapterError::Credential);
        }
        if element.sensitive && !matches!(action, DesktopAction::Observe) {
            return Err(AxAdapterError::Sensitive);
        }
        if matches!(action, DesktopAction::DraftText { .. }) && !element.focused {
            return Err(AxAdapterError::NotFocused);
        }
        let state = fresh_state(&element);
        let mut proposal = ActionProposal {
            goal_id,
            intent_digest,
            run_id,
            task_id,
            action_id,
            observation: ObservationRef {
                observation_id: digest_parts(
                    b"holoiroh/ax-observation/1\0",
                    &[&state.before_state_digest],
                ),
                before_state_digest: state.before_state_digest.clone(),
            },
            target: TargetRef {
                bundle_id: state.bundle_id,
                window_id: state.window_id,
                element_id: state.element_id,
                expected_role: state.role,
                expected_title_digest: state.title_digest,
                expected_value_digest: state.value_digest,
                sensitive: element.sensitive,
                credential: element.credential,
                resolved: true,
            },
            action,
            proposal_digest: String::new(),
        };
        proposal.proposal_digest = canonical_proposal_digest(&proposal);
        self.resolved = Some(element);
        Ok(proposal)
    }

    fn resolve_fresh_identity(
        &mut self,
        expected: &SemanticAxElement,
    ) -> Result<SemanticAxElement, AxAdapterError<S::Error>> {
        let mut matches = self
            .source
            .observe()
            .map_err(AxAdapterError::Source)?
            .into_iter()
            .take(MAX_AX_ELEMENTS)
            .filter(|element| {
                element.bundle_id == expected.bundle_id
                    && element.window_id == expected.window_id
                    && element.element_id == expected.element_id
            });
        let element = matches.next().ok_or(AxAdapterError::Unresolved)?;
        if matches.next().is_some() {
            return Err(AxAdapterError::Ambiguous);
        }
        if !element.enabled {
            return Err(AxAdapterError::Disabled);
        }
        Ok(element)
    }

    fn resolve_fresh(
        &mut self,
        element_id: &str,
    ) -> Result<SemanticAxElement, AxAdapterError<S::Error>> {
        let mut matches = self
            .source
            .observe()
            .map_err(AxAdapterError::Source)?
            .into_iter()
            .take(MAX_AX_ELEMENTS)
            .filter(|element| element.element_id == element_id);
        let element = matches.next().ok_or(AxAdapterError::Unresolved)?;
        if matches.next().is_some() {
            return Err(AxAdapterError::Ambiguous);
        }
        if !element.enabled {
            return Err(AxAdapterError::Disabled);
        }
        Ok(element)
    }
}

impl<S: SemanticAxSource> PrimitiveAdapter for AxPrimitiveAdapter<S> {
    type Error = AxAdapterError<S::Error>;

    fn observe(&mut self, target: &TargetRef) -> Result<FreshTargetState, Self::Error> {
        let element = self.resolve_fresh(&target.element_id)?;
        let state = fresh_state(&element);
        self.resolved = Some(element);
        Ok(state)
    }

    fn execute_observe(&mut self, _target: &TargetRef) -> Result<(), Self::Error> {
        self.take_resolved().map(|_| ())
    }

    fn execute_navigation(
        &mut self,
        _target: &TargetRef,
        action: &NavigationAction,
        fresh_bounds: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), Self::Error> {
        let element = self.take_resolved()?;
        if element.credential || element.sensitive {
            return Err(if element.credential {
                AxAdapterError::Credential
            } else {
                AxAdapterError::Sensitive
            });
        }
        match action {
            NavigationAction::SemanticActivate => click_center(fresh_bounds)?,
            NavigationAction::CoordinateActivate { x, y } => {
                let bounds = fresh_bounds.ok_or(AxAdapterError::MissingBounds)?;
                if !contains(bounds, *x, *y) {
                    return Err(AxAdapterError::MissingBounds);
                }
                remote_input::click_absolute(*x as f64, *y as f64, false, 1);
            }
            NavigationAction::Scroll {
                horizontal,
                vertical,
            } => {
                let bounds = fresh_bounds.ok_or(AxAdapterError::MissingBounds)?;
                let (x, y) = center(bounds).ok_or(AxAdapterError::MissingBounds)?;
                remote_input::scroll_absolute(x, y, *horizontal as f64, *vertical as f64);
            }
        }
        Ok(())
    }

    fn execute_focus(&mut self, _target: &TargetRef) -> Result<(), Self::Error> {
        let element = self.take_resolved()?;
        if element.credential || element.sensitive {
            return Err(if element.credential {
                AxAdapterError::Credential
            } else {
                AxAdapterError::Sensitive
            });
        }
        click_center(element.bounds)
    }

    fn execute_draft(&mut self, _target: &TargetRef, text: &str) -> Result<(), Self::Error> {
        let element = self.take_resolved()?;
        if element.credential {
            return Err(AxAdapterError::Credential);
        }
        if element.sensitive {
            return Err(AxAdapterError::Sensitive);
        }
        if !element.focused {
            return Err(AxAdapterError::NotFocused);
        }
        if text.len() > crate::action_executor::MAX_DRAFT_BYTES {
            return Err(AxAdapterError::DraftTooLarge);
        }
        remote_input::text(text);
        Ok(())
    }

    fn execute_commit(
        &mut self,
        _target: &TargetRef,
        _action: CommitAction,
    ) -> Result<(), Self::Error> {
        let element = self.take_resolved()?;
        if element.credential {
            return Err(AxAdapterError::Credential);
        }
        if element.sensitive {
            return Err(AxAdapterError::Sensitive);
        }
        click_center(element.bounds)
    }
}

impl<S: SemanticAxSource> AxPrimitiveAdapter<S> {
    fn take_resolved(&mut self) -> Result<SemanticAxElement, AxAdapterError<S::Error>> {
        self.resolved.take().ok_or(AxAdapterError::Unresolved)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemAxSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemAxError {
    Snapshot(String),
    InvalidSnapshot,
}

impl SemanticAxSource for SystemAxSource {
    type Error = SystemAxError;

    fn observe(&mut self) -> Result<Vec<SemanticAxElement>, Self::Error> {
        let bundle_before = frontmost_bundle_id_sync().ok_or_else(|| {
            SystemAxError::Snapshot("frontmost application identity unavailable".to_string())
        })?;
        let snapshot = match snapshot_frontmost_application() {
            SnapshotAttempt::Captured { snapshot, .. } => snapshot,
            SnapshotAttempt::Omitted { reason, .. } => {
                return Err(SystemAxError::Snapshot(reason.to_string()));
            }
        };
        let parsed: Snapshot =
            serde_json::from_str(&snapshot.json).map_err(|_| SystemAxError::InvalidSnapshot)?;
        let bundle_after = frontmost_bundle_id_sync().ok_or_else(|| {
            SystemAxError::Snapshot("frontmost application identity unavailable".to_string())
        })?;
        let bundle_id = coherent_bundle_identity(Some(bundle_before), Some(bundle_after))?;
        let categories = crate::sensitive_categories::SensitiveCategories::load_default()
            .unwrap_or_else(|_| {
                crate::sensitive_categories::SensitiveCategories::default_categories()
            });
        let sensitive_application = categories.classify(&bundle_id).is_some();
        let nodes: std::collections::HashMap<_, _> = parsed
            .nodes
            .iter()
            .cloned()
            .map(|node| (node.id, node))
            .collect();
        Ok(parsed
            .nodes
            .into_iter()
            .take(MAX_AX_ELEMENTS)
            .map(|node| {
                let window_id = window_identity(&node, &nodes, &bundle_id);
                SemanticAxElement::from_snapshot(
                    node,
                    bundle_id.clone(),
                    window_id,
                    sensitive_application,
                )
            })
            .collect())
    }
}

fn coherent_bundle_identity(
    before: Option<String>,
    after: Option<String>,
) -> Result<String, SystemAxError> {
    let before = before.ok_or_else(|| {
        SystemAxError::Snapshot("frontmost application identity unavailable".to_string())
    })?;
    let after = after.ok_or_else(|| {
        SystemAxError::Snapshot("frontmost application identity unavailable".to_string())
    })?;
    if before != after {
        return Err(SystemAxError::Snapshot(
            "frontmost application changed during accessibility snapshot".to_string(),
        ));
    }
    Ok(after)
}

#[doc(hidden)]
pub fn coherent_bundle_identity_for_probing(
    before: Option<String>,
    after: Option<String>,
) -> Result<String, SystemAxError> {
    coherent_bundle_identity(before, after)
}

#[derive(Deserialize)]
struct Snapshot {
    nodes: Vec<SnapshotNode>,
}

#[derive(Deserialize, Clone)]
struct SnapshotNode {
    id: usize,
    #[serde(default)]
    parent_id: Option<usize>,
    #[serde(default)]
    depth: usize,
    role: String,
    #[serde(default)]
    subrole: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    frame: Option<SnapshotFrame>,
}

#[derive(Deserialize, Clone)]
struct SnapshotFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl SemanticAxElement {
    fn from_snapshot(
        node: SnapshotNode,
        bundle_id: String,
        window_id: String,
        sensitive_application: bool,
    ) -> Self {
        let credential = [&node.role, node.subrole.as_deref().unwrap_or("")]
            .iter()
            .any(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("secure") || value.contains("password")
            });
        Self {
            bundle_id,
            window_id,
            element_id: format!("ax-node-{}", node.id),
            role: node.role,
            title: node.title.unwrap_or_default(),
            value: node.value,
            focused: node.focused.unwrap_or(false),
            enabled: node.enabled.unwrap_or(false),
            sensitive: sensitive_application || credential,
            credential,
            bounds: node.frame.and_then(|frame| {
                let values = [frame.x, frame.y, frame.width, frame.height];
                values.iter().all(|value| value.is_finite()).then(|| {
                    (
                        frame.x.round() as i32,
                        frame.y.round() as i32,
                        frame.width.round() as i32,
                        frame.height.round() as i32,
                    )
                })
            }),
        }
    }
}

fn frontmost_bundle_id_sync() -> Option<String> {
    let front = std::process::Command::new("lsappinfo")
        .arg("front")
        .output()
        .ok()?;
    if !front.status.success() {
        return None;
    }
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let info = std::process::Command::new("lsappinfo")
        .args(["info", "-only", "bundleid", &asn])
        .output()
        .ok()?;
    if !info.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&info.stdout);
    let bundle_id = line.split('=').nth(1)?.trim().trim_matches('"');
    (!bundle_id.is_empty() && bundle_id != "NULL").then(|| bundle_id.to_string())
}

fn window_identity(
    node: &SnapshotNode,
    nodes: &std::collections::HashMap<usize, SnapshotNode>,
    bundle_id: &str,
) -> String {
    let mut current = node;
    let mut shallowest = node;
    loop {
        if current.role.to_ascii_lowercase().contains("window") {
            shallowest = current;
            break;
        }
        if current.depth < shallowest.depth {
            shallowest = current;
        }
        let Some(parent_id) = current.parent_id else {
            break;
        };
        let Some(parent) = nodes.get(&parent_id) else {
            break;
        };
        current = parent;
    }
    let frame = shallowest
        .frame
        .as_ref()
        .map(|frame| format!("{},{},{},{}", frame.x, frame.y, frame.width, frame.height))
        .unwrap_or_default();
    digest_parts(
        b"holoiroh/ax-window/1\0",
        &[
            bundle_id,
            &shallowest.id.to_string(),
            shallowest.title.as_deref().unwrap_or(""),
            &frame,
        ],
    )
}

#[derive(serde::Serialize)]
struct PublicElement {
    element_id: String,
    role: String,
    title: String,
    focused: bool,
    enabled: bool,
    bounds: Option<(i32, i32, i32, i32)>,
}

impl PublicElement {
    fn new(handle: &str, element: &SemanticAxElement) -> Self {
        Self {
            element_id: handle.to_owned(),
            role: element.role.clone(),
            title: element.title.clone(),
            focused: element.focused,
            enabled: element.enabled,
            bounds: element.bounds,
        }
    }
}

fn fresh_state(element: &SemanticAxElement) -> FreshTargetState {
    let title_digest = digest_text(&element.title);
    let value_digest = element.value.as_deref().map(digest_text);
    let bounds = element
        .bounds
        .map(|(x, y, width, height)| format!("{x},{y},{width},{height}"))
        .unwrap_or_default();
    let before_state_digest = digest_parts(
        b"holoiroh/ax-target-state/1\0",
        &[
            &element.bundle_id,
            &element.window_id,
            &element.element_id,
            &element.role,
            &title_digest,
            value_digest.as_deref().unwrap_or(""),
            if element.focused {
                "focused"
            } else {
                "unfocused"
            },
            if element.enabled {
                "enabled"
            } else {
                "disabled"
            },
            if element.sensitive {
                "sensitive"
            } else {
                "non_sensitive"
            },
            if element.credential {
                "credential"
            } else {
                "non_credential"
            },
            "resolved",
            &bounds,
        ],
    );
    FreshTargetState {
        bundle_id: element.bundle_id.clone(),
        window_id: element.window_id.clone(),
        element_id: element.element_id.clone(),
        role: element.role.clone(),
        title_digest,
        value_digest,
        before_state_digest,
        bounds: element.bounds,
    }
}

fn digest_text(value: &str) -> String {
    digest_parts(b"holoiroh/ax-text/1\0", &[value])
}

fn digest_parts(domain: &[u8], parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}

fn contains(bounds: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
    let (left, top, width, height) = bounds;
    width > 0
        && height > 0
        && x >= left
        && y >= top
        && x < left.saturating_add(width)
        && y < top.saturating_add(height)
}

fn center(bounds: (i32, i32, i32, i32)) -> Option<(f64, f64)> {
    let (x, y, width, height) = bounds;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some((
        x as f64 + width as f64 / 2.0,
        y as f64 + height as f64 / 2.0,
    ))
}

fn click_center<E>(bounds: Option<(i32, i32, i32, i32)>) -> Result<(), AxAdapterError<E>> {
    let (x, y, width, height) = bounds.ok_or(AxAdapterError::MissingBounds)?;
    if width <= 0 || height <= 0 {
        return Err(AxAdapterError::MissingBounds);
    }
    remote_input::click_absolute(
        x as f64 + width as f64 / 2.0,
        y as f64 + height as f64 / 2.0,
        false,
        1,
    );
    Ok(())
}
