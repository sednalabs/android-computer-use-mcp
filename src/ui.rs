//! UI selector parsing and Android accessibility-tree normalization helpers.
//!
//! ## Rationale
//! Provides pure, testable utilities to parse Android UI XML hierarchy files
//! and transform them into normalized structures that LLMs can reason about.
//!
//! ## Security Boundaries
//! * XML parsing is handled by the `roxmltree` crate to prevent memory-unsafe parsing.
//! * No external network access or file-system writes performed by this module.
//!
//! ## References
//! * [Android Accessibility Tree](https://developer.android.com/guide/topics/ui/accessibility/services)

use std::path::Path;

use roxmltree::Document;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::McpError;

const UI_OUTPUT_TEXT_CHAR_LIMIT: usize = 240;
const UI_VISIBLE_LABEL_CHAR_LIMIT: usize = 96;
const UI_VISIBLE_NODE_LIMIT: usize = 120;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct UiNodesForOutput {
    pub(crate) nodes: Vec<NormalizedUiNode>,
    pub(crate) total_count: usize,
    pub(crate) returned_count: usize,
    pub(crate) compacted_text_fields: usize,
    pub(crate) text_char_limit: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct VisibleUiForOutput {
    pub(crate) nodes: Vec<VisibleUiNode>,
    pub(crate) total_labeled_count: usize,
    pub(crate) returned_count: usize,
    pub(crate) label_char_limit: usize,
    pub(crate) viewport: Option<UiBounds>,
    pub(crate) clipped_node_count: usize,
    pub(crate) scrollable_node_count: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct VisibleUiNode {
    pub(crate) label: String,
    pub(crate) bounds: UiBounds,
    pub(crate) resource_id: Option<String>,
    pub(crate) class_name: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) interactive: bool,
    pub(crate) scrollable: bool,
    pub(crate) clipped: bool,
    pub(crate) clip_edges: Vec<String>,
    pub(crate) visible_fraction_percent: u8,
}

/// Defines the criteria for selecting a UI element in the Android accessibility tree.
#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, Default, PartialEq, Eq)]
pub struct UiSelector {
    /// Text content of the element.
    #[serde(default)]
    pub text: Option<String>,
    /// Whether the text match must be exact.
    #[serde(default)]
    pub text_exact: Option<bool>,
    /// Logical label of the element.
    #[serde(default)]
    pub label: Option<String>,
    /// Whether the label match must be exact.
    #[serde(default)]
    pub label_exact: Option<bool>,
    /// Content description attribute.
    #[serde(default)]
    pub content_desc: Option<String>,
    /// Resource ID of the element.
    #[serde(default)]
    pub resource_id: Option<String>,
    /// Filter by clickability.
    #[serde(default)]
    pub clickable: Option<bool>,
    /// Filter by focusability.
    #[serde(default)]
    pub focusable: Option<bool>,
    /// Filter by enabled state.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Filter by selection state.
    #[serde(default)]
    pub selected: Option<bool>,
    /// Filter by checked state.
    #[serde(default)]
    pub checked: Option<bool>,
    /// Filter by focused state.
    #[serde(default)]
    pub focused: Option<bool>,
    /// Filter by scrollability.
    #[serde(default)]
    pub scrollable: Option<bool>,
    /// Filter by long-clickability.
    #[serde(default)]
    pub long_clickable: Option<bool>,
}

/// Allows either a fully defined selector or a text-based shorthand.
#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone)]
#[serde(untagged)]
pub enum UiSelectorInput {
    /// A structured selector definition.
    Selector(UiSelector),
    /// Text shorthand for common selector patterns.
    Text(String),
}

impl From<UiSelectorInput> for UiSelector {
    fn from(value: UiSelectorInput) -> Self {
        match value {
            UiSelectorInput::Selector(selector) => selector,
            UiSelectorInput::Text(text) => selector_from_text_shorthand(&text),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub(crate) struct UiBounds {
    pub(crate) left: u32,
    pub(crate) top: u32,
    pub(crate) right: u32,
    pub(crate) bottom: u32,
}

impl UiBounds {
    pub(crate) fn center(self) -> (u32, u32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }

    fn area(self) -> u64 {
        self.right.saturating_sub(self.left) as u64 * self.bottom.saturating_sub(self.top) as u64
    }

    fn intersection_area(self, other: UiBounds) -> u64 {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if right <= left || bottom <= top {
            return 0;
        }
        (right - left) as u64 * (bottom - top) as u64
    }

    fn contains(self, other: UiBounds) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct NormalizedUiNode {
    pub(crate) class_name: Option<String>,
    pub(crate) package_name: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) semantic_label: Option<String>,
    pub(crate) content_desc: Option<String>,
    pub(crate) resource_id: Option<String>,
    pub(crate) clickable: bool,
    pub(crate) focusable: bool,
    pub(crate) enabled: bool,
    pub(crate) selected: bool,
    pub(crate) checked: bool,
    pub(crate) focused: bool,
    pub(crate) scrollable: bool,
    pub(crate) long_clickable: bool,
    pub(crate) bounds: Option<UiBounds>,
    pub(crate) center: Option<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub(crate) struct UiNodeMatch {
    pub(crate) bounds: UiBounds,
}

#[derive(Debug, Clone)]
pub(crate) struct MatchCandidates<T> {
    pub(crate) matches: Vec<T>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SelectorCandidateSummary {
    pub(crate) index: usize,
    pub(crate) text: Option<String>,
    pub(crate) semantic_label: Option<String>,
    pub(crate) content_desc: Option<String>,
    pub(crate) resource_id: Option<String>,
    pub(crate) clickable: bool,
    pub(crate) focused: bool,
    pub(crate) scrollable: bool,
    pub(crate) long_clickable: bool,
    pub(crate) bounds: Option<UiBounds>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedNodeSelection {
    pub(crate) node: NormalizedUiNode,
    pub(crate) match_count: usize,
    pub(crate) selected_match_index: usize,
    pub(crate) candidates: Vec<SelectorCandidateSummary>,
}

#[derive(Debug, Clone)]
pub(crate) enum SelectionFailure {
    NoMatches,
    Ambiguous {
        match_count: usize,
        candidates: Vec<SelectorCandidateSummary>,
    },
    MatchIndexOutOfRange {
        requested_match_index: usize,
        match_count: usize,
        candidates: Vec<SelectorCandidateSummary>,
    },
}

pub(crate) fn normalize_selector_input(input: UiSelectorInput) -> UiSelector {
    input.into()
}

pub(crate) fn normalize_optional_selector_input(
    input: Option<UiSelectorInput>,
) -> Option<UiSelector> {
    input.map(UiSelector::from)
}

pub(crate) fn ensure_selector_not_empty(selector: &UiSelector) -> Result<(), McpError> {
    let has_text = |value: Option<&str>| {
        value
            .map(str::trim)
            .map(|value| !value.is_empty())
            .unwrap_or(false)
    };
    let meaningful = has_text(selector.text.as_deref())
        || has_text(selector.label.as_deref())
        || has_text(selector.content_desc.as_deref())
        || has_text(selector.resource_id.as_deref())
        || selector.clickable.is_some()
        || selector.focusable.is_some()
        || selector.enabled.is_some()
        || selector.selected.is_some()
        || selector.checked.is_some()
        || selector.focused.is_some()
        || selector.scrollable.is_some()
        || selector.long_clickable.is_some();
    if meaningful {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            "selector must include at least one field".to_string(),
            None,
        ))
    }
}

pub(crate) fn parse_ui_nodes_from_path(path: &Path) -> Result<Vec<NormalizedUiNode>, McpError> {
    let xml = std::fs::read_to_string(path)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    parse_ui_nodes_from_xml(&xml)
}

pub(crate) fn parse_ui_nodes_from_xml(xml: &str) -> Result<Vec<NormalizedUiNode>, McpError> {
    let document =
        Document::parse(xml).map_err(|err| McpError::internal_error(err.to_string(), None))?;
    Ok(document
        .descendants()
        .filter(|node| node.has_tag_name("node"))
        .map(normalized_ui_node_from_xml)
        .collect())
}

pub(crate) fn ui_nodes_for_tool_output(nodes: &[NormalizedUiNode]) -> UiNodesForOutput {
    let mut compacted_text_fields = 0;
    let compacted_nodes = nodes
        .iter()
        .cloned()
        .map(|mut node| {
            let original_text = node.text.clone();
            let compacted_text = compact_output_text(original_text.clone());
            if compacted_text != original_text {
                compacted_text_fields += 1;
            }
            node.text = compacted_text;

            let original_label = node.semantic_label.clone();
            let compacted_label = compact_output_text(original_label.clone());
            if compacted_label != original_label {
                compacted_text_fields += 1;
            }
            node.semantic_label = compacted_label;

            let original_desc = node.content_desc.clone();
            let compacted_desc = compact_output_text(original_desc.clone());
            if compacted_desc != original_desc {
                compacted_text_fields += 1;
            }
            node.content_desc = compacted_desc;
            node
        })
        .collect::<Vec<_>>();

    UiNodesForOutput {
        total_count: nodes.len(),
        returned_count: compacted_nodes.len(),
        compacted_text_fields,
        text_char_limit: UI_OUTPUT_TEXT_CHAR_LIMIT,
        nodes: compacted_nodes,
    }
}

pub(crate) fn visible_ui_for_tool_output(nodes: &[NormalizedUiNode]) -> VisibleUiForOutput {
    let mut total_labeled_count = 0;
    let mut clipped_node_count = 0;
    let scrollable_node_count = nodes.iter().filter(|node| node.scrollable).count();
    let mut visible = Vec::new();
    let viewport = infer_viewport_bounds(nodes);

    for node in nodes {
        let Some(bounds) = node.bounds else {
            continue;
        };
        let label = if node.scrollable {
            direct_visible_label_for_node(node)
                .or_else(|| scrollable_visible_label_for_node(node, bounds, viewport))
                .or_else(|| visible_label_for_node(node))
        } else {
            visible_label_for_node(node)
        };
        let Some(label) = label else {
            continue;
        };
        if is_nested_duplicate_visible_label(node, bounds, &label, nodes) {
            continue;
        }
        total_labeled_count += 1;
        if visible.len() >= UI_VISIBLE_NODE_LIMIT {
            continue;
        }
        let visibility = viewport
            .map(|viewport| visibility_within_viewport(bounds, viewport))
            .unwrap_or_default();
        if visibility.clipped {
            clipped_node_count += 1;
        }
        visible.push(VisibleUiNode {
            label: compact_visible_label_with_state(&label, node, &visibility),
            bounds,
            resource_id: node.resource_id.clone(),
            class_name: node.class_name.clone(),
            enabled: node.enabled,
            interactive: node.clickable || node.focusable || node.long_clickable || node.scrollable,
            scrollable: node.scrollable,
            clipped: visibility.clipped,
            clip_edges: visibility.clip_edges,
            visible_fraction_percent: visibility.visible_fraction_percent,
        });
    }

    VisibleUiForOutput {
        returned_count: visible.len(),
        nodes: visible,
        total_labeled_count,
        label_char_limit: UI_VISIBLE_LABEL_CHAR_LIMIT,
        viewport,
        clipped_node_count,
        scrollable_node_count,
    }
}

fn is_nested_duplicate_visible_label(
    node: &NormalizedUiNode,
    bounds: UiBounds,
    label: &str,
    nodes: &[NormalizedUiNode],
) -> bool {
    if node.clickable || node.focusable || node.long_clickable || node.scrollable {
        return false;
    }

    let label_key = visible_label_key(label);
    if label_key.is_empty() {
        return false;
    }

    nodes.iter().any(|candidate| {
        let Some(candidate_bounds) = candidate.bounds else {
            return false;
        };
        if candidate.scrollable
            || !(candidate.clickable || candidate.focusable || candidate.long_clickable)
        {
            return false;
        }
        if candidate_bounds == bounds {
            return visible_label_for_node(candidate)
                .is_some_and(|candidate_label| visible_label_key(&candidate_label) == label_key);
        }
        candidate_bounds.contains(bounds)
            && candidate_bounds.area() > bounds.area()
            && visible_label_for_node(candidate)
                .is_some_and(|candidate_label| visible_label_key(&candidate_label) == label_key)
    })
}

fn visible_label_key(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug)]
struct ViewportVisibility {
    clipped: bool,
    clip_edges: Vec<String>,
    visible_fraction_percent: u8,
}

impl Default for ViewportVisibility {
    fn default() -> Self {
        Self {
            clipped: false,
            clip_edges: Vec::new(),
            visible_fraction_percent: 100,
        }
    }
}

fn infer_viewport_bounds(nodes: &[NormalizedUiNode]) -> Option<UiBounds> {
    nodes
        .iter()
        .filter_map(|node| node.bounds)
        .filter(|bounds| bounds.left == 0 && bounds.top == 0)
        .max_by_key(|bounds| bounds.area())
        .or_else(|| {
            nodes
                .iter()
                .filter_map(|node| node.bounds)
                .max_by_key(|bounds| bounds.area())
        })
}

fn visibility_within_viewport(bounds: UiBounds, viewport: UiBounds) -> ViewportVisibility {
    let mut clip_edges = Vec::new();
    if bounds.left < viewport.left {
        clip_edges.push("left".to_string());
    }
    if bounds.top < viewport.top {
        clip_edges.push("top".to_string());
    }
    if bounds.right > viewport.right {
        clip_edges.push("right".to_string());
    }
    if bounds.bottom > viewport.bottom {
        clip_edges.push("bottom".to_string());
    }

    let node_area = bounds.area();
    let visible_area = bounds.intersection_area(viewport);
    let visible_fraction_percent = if node_area == 0 {
        0
    } else {
        ((visible_area.saturating_mul(100) + (node_area / 2)) / node_area).min(100) as u8
    };

    ViewportVisibility {
        clipped: !clip_edges.is_empty() || visible_fraction_percent < 100,
        clip_edges,
        visible_fraction_percent,
    }
}

pub(crate) fn matching_nodes(
    nodes: &[NormalizedUiNode],
    selector: &UiSelector,
) -> MatchCandidates<NormalizedUiNode> {
    MatchCandidates {
        matches: nodes
            .iter()
            .filter(|node| selector_matches(node, selector))
            .cloned()
            .collect::<Vec<_>>(),
    }
}

pub(crate) fn find_interactive_ui_node(
    path: &Path,
    selector: &UiSelector,
) -> Result<MatchCandidates<NormalizedUiNode>, McpError> {
    let xml = std::fs::read_to_string(path)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let document =
        Document::parse(&xml).map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let mut matches = Vec::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name("node"))
    {
        let normalized = normalized_ui_node_from_xml(node);
        if !interactive_selector_matches(node, &normalized, selector) {
            continue;
        }
        let interactive_target = node
            .ancestors()
            .find_map(|candidate| {
                interactive_bounds_from_node(candidate).map(|bounds| (candidate, bounds))
            });
        let (resolved, interactive_bounds) = match interactive_target {
            Some((interactive_node, bounds)) => {
                (normalized_ui_node_from_xml(interactive_node), Some(bounds))
            }
            None => {
                let bounds = normalized.bounds;
                (normalized, bounds)
            }
        };
        if matches
            .iter()
            .any(|existing: &NormalizedUiNode| existing.bounds == interactive_bounds)
        {
            continue;
        }
        matches.push(NormalizedUiNode {
            center: interactive_bounds.map(UiBounds::center),
            bounds: interactive_bounds,
            ..resolved
        });
    }
    Ok(MatchCandidates { matches })
}

pub(crate) fn text_verification_target_selector(node: &NormalizedUiNode) -> UiSelector {
    UiSelector {
        resource_id: node.resource_id.clone(),
        clickable: node.clickable.then_some(true),
        focusable: node.focusable.then_some(true),
        long_clickable: node.long_clickable.then_some(true),
        ..UiSelector::default()
    }
}

pub(crate) fn focus_verification_target_selector(node: &NormalizedUiNode) -> UiSelector {
    UiSelector {
        text: node.text.clone(),
        content_desc: node.content_desc.clone(),
        resource_id: node.resource_id.clone(),
        focusable: node.focusable.then_some(true),
        focused: Some(true),
        ..UiSelector::default()
    }
}

pub(crate) fn selector_matches(node: &NormalizedUiNode, selector: &UiSelector) -> bool {
    if let Some(text) = selector.text.as_deref() {
        let wanted = text.trim();
        if wanted.is_empty()
            || !(matches_text(
                node.text.as_deref(),
                wanted,
                selector.text_exact.unwrap_or(false),
            ) || matches_text(
                node.content_desc.as_deref(),
                wanted,
                selector.text_exact.unwrap_or(false),
            ))
        {
            return false;
        }
    }
    if let Some(label) = selector.label.as_deref() {
        let wanted = label.trim();
        if wanted.is_empty()
            || !matches_text(
                node.semantic_label.as_deref(),
                wanted,
                selector.label_exact.unwrap_or(false),
            )
        {
            return false;
        }
    }
    if let Some(content_desc) = selector.content_desc.as_deref() {
        let wanted = content_desc.trim();
        if wanted.is_empty() || !matches_text(node.content_desc.as_deref(), wanted, false) {
            return false;
        }
    }
    if let Some(resource_id) = selector.resource_id.as_deref() {
        let wanted = resource_id.trim();
        if wanted.is_empty() || node.resource_id.as_deref() != Some(wanted) {
            return false;
        }
    }
    if let Some(clickable) = selector.clickable
        && node.clickable != clickable
    {
        return false;
    }
    if let Some(focusable) = selector.focusable
        && node.focusable != focusable
    {
        return false;
    }
    if let Some(enabled) = selector.enabled
        && node.enabled != enabled
    {
        return false;
    }
    if let Some(selected) = selector.selected
        && node.selected != selected
    {
        return false;
    }
    if let Some(checked) = selector.checked
        && node.checked != checked
    {
        return false;
    }
    if let Some(focused) = selector.focused
        && node.focused != focused
    {
        return false;
    }
    if let Some(scrollable) = selector.scrollable
        && node.scrollable != scrollable
    {
        return false;
    }
    if let Some(long_clickable) = selector.long_clickable
        && node.long_clickable != long_clickable
    {
        return false;
    }
    true
}

pub(crate) fn selector_candidate_summary(
    matches: &[NormalizedUiNode],
) -> Vec<SelectorCandidateSummary> {
    matches
        .iter()
        .enumerate()
        .take(5)
        .map(|(index, node)| SelectorCandidateSummary {
            index,
            text: node.text.clone(),
            semantic_label: node.semantic_label.clone(),
            content_desc: node.content_desc.clone(),
            resource_id: node.resource_id.clone(),
            clickable: node.clickable,
            focused: node.focused,
            scrollable: node.scrollable,
            long_clickable: node.long_clickable,
            bounds: node.bounds,
        })
        .collect()
}

pub(crate) fn resolve_node_selection(
    matches: Vec<NormalizedUiNode>,
    match_index: Option<usize>,
) -> Result<ResolvedNodeSelection, SelectionFailure> {
    let match_count = matches.len();
    let candidates = selector_candidate_summary(&matches);
    if matches.is_empty() {
        return Err(SelectionFailure::NoMatches);
    }
    let selected_match_index = match match_index {
        Some(index) => {
            if index >= match_count {
                return Err(SelectionFailure::MatchIndexOutOfRange {
                    requested_match_index: index,
                    match_count,
                    candidates,
                });
            }
            index
        }
        None => {
            if match_count == 1 {
                0
            } else if let Some(index) = auto_resolve_match_index(&matches) {
                index
            } else {
                return Err(SelectionFailure::Ambiguous {
                    match_count,
                    candidates,
                });
            }
        }
    };
    Ok(ResolvedNodeSelection {
        node: matches[selected_match_index].clone(),
        match_count,
        selected_match_index,
        candidates,
    })
}

fn auto_resolve_match_index(matches: &[NormalizedUiNode]) -> Option<usize> {
    if matches.len() < 2 {
        return None;
    }

    let mut best: Option<(usize, (u8, u8, u8, u8, u8))> = None;
    let mut best_count = 0usize;

    for (index, node) in matches.iter().enumerate() {
        let score = (
            u8::from(node.clickable),
            u8::from(node.focusable),
            u8::from(node.long_clickable),
            u8::from(node.resource_id.is_some()),
            u8::from(node.center.is_some()),
        );

        if score == (0, 0, 0, 0, 0) {
            continue;
        }

        match best {
            None => {
                best = Some((index, score));
                best_count = 1;
            }
            Some((_, best_score)) if score > best_score => {
                best = Some((index, score));
                best_count = 1;
            }
            Some((_, best_score)) if score == best_score => {
                best_count += 1;
            }
            _ => {}
        }
    }

    match (best, best_count) {
        (Some((index, _)), 1) => Some(index),
        _ => None,
    }
}

pub(crate) fn selection_failure_json(error: &SelectionFailure) -> serde_json::Value {
    match error {
        SelectionFailure::NoMatches => json!({
            "reason": "no_matches",
            "match_count": 0,
            "candidates": [],
        }),
        SelectionFailure::Ambiguous {
            match_count,
            candidates,
        } => json!({
            "reason": "ambiguous_match",
            "match_count": match_count,
            "candidates": candidates,
        }),
        SelectionFailure::MatchIndexOutOfRange {
            requested_match_index,
            match_count,
            candidates,
        } => json!({
            "reason": "match_index_out_of_range",
            "requested_match_index": requested_match_index,
            "match_count": match_count,
            "candidates": candidates,
        }),
    }
}

pub(crate) fn actionable_center(
    node: &Option<NormalizedUiNode>,
    selector: &UiSelector,
    action: &str,
) -> Result<(u32, u32), McpError> {
    match node {
        Some(node) => node.center.ok_or_else(|| {
            McpError::internal_error(
                format!(
                    "matched selector {:?} for {action}, but no actionable bounds were available",
                    selector
                ),
                None,
            )
        }),
        None => Err(McpError::internal_error(
            format!("no UI element matched selector {:?} for {action}", selector),
            None,
        )),
    }
}

pub(crate) fn find_ui_node_by_label(path: &Path, label: &str) -> Result<UiNodeMatch, McpError> {
    let xml = std::fs::read_to_string(path)
        .map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let document =
        Document::parse(&xml).map_err(|err| McpError::internal_error(err.to_string(), None))?;
    let wanted = label.trim().to_ascii_lowercase();
    document
        .descendants()
        .filter(|node| node.has_tag_name("node"))
        .filter_map(|node| {
            let text = node.attribute("text");
            let content_desc = node.attribute("content-desc");
            let matches = text
                .map(|value| {
                    value.trim().eq_ignore_ascii_case(&wanted)
                        || value.to_ascii_lowercase().contains(&wanted)
                })
                .unwrap_or(false)
                || content_desc
                    .map(|value| {
                        value.trim().eq_ignore_ascii_case(&wanted)
                            || value.to_ascii_lowercase().contains(&wanted)
                    })
                    .unwrap_or(false);
            if !matches {
                return None;
            }
            let bounds = node
                .ancestors()
                .find_map(interactive_bounds_from_node)
                .or_else(|| parse_bounds(node.attribute("bounds")?))?;
            Some(UiNodeMatch { bounds })
        })
        .next()
        .ok_or_else(|| {
            McpError::internal_error(format!("no ui node matched label '{label}'"), None)
        })
}

pub(crate) fn matches_text(value: Option<&str>, wanted: &str, exact: bool) -> bool {
    value
        .map(|candidate| {
            let candidate = candidate.to_ascii_lowercase();
            let wanted = wanted.to_ascii_lowercase();
            if exact {
                candidate == wanted
            } else {
                candidate.contains(&wanted)
            }
        })
        .unwrap_or(false)
}

pub(crate) fn parse_bounds(raw: &str) -> Option<UiBounds> {
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_prefix('[')?;
    let (first, second) = trimmed.split_once("][")?;
    let second = second.strip_suffix(']')?;
    let (left, top) = first.split_once(',')?;
    let (right, bottom) = second.split_once(',')?;
    Some(UiBounds {
        left: left.parse().ok()?,
        top: top.parse().ok()?,
        right: right.parse().ok()?,
        bottom: bottom.parse().ok()?,
    })
}

fn selector_from_text_shorthand(raw: &str) -> UiSelector {
    parse_semantic_selector_phrase(raw).unwrap_or_else(|| {
        let trimmed = raw.trim();
        UiSelector {
            text: (!trimmed.is_empty()).then(|| trimmed.to_string()),
            ..UiSelector::default()
        }
    })
}

fn parse_semantic_selector_phrase(raw: &str) -> Option<UiSelector> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(label) = extract_labeled_phrase(
        trimmed,
        &[
            "button labeled ",
            "button labelled ",
            "button named ",
            "button with text ",
            "button ",
        ],
    ) {
        return Some(UiSelector {
            label: Some(label),
            label_exact: Some(true),
            clickable: Some(true),
            ..UiSelector::default()
        });
    }

    if let Some(label) = extract_labeled_phrase(
        trimmed,
        &[
            "text field labeled ",
            "text field labelled ",
            "text field named ",
            "text field with text ",
            "input field labeled ",
            "input field labelled ",
            "input field named ",
            "text input labeled ",
            "text input labelled ",
            "text input named ",
            "field labeled ",
            "field labelled ",
            "field named ",
            "input labeled ",
            "input labelled ",
            "input named ",
        ],
    ) {
        return Some(UiSelector {
            label: Some(label),
            label_exact: Some(true),
            focusable: Some(true),
            ..UiSelector::default()
        });
    }

    match trimmed.to_ascii_lowercase().as_str() {
        "text field" | "input field" | "text input" | "field" | "input" => Some(UiSelector {
            focusable: Some(true),
            ..UiSelector::default()
        }),
        "scroll view" | "scrollable view" | "list view" | "list" | "scrollable" => {
            Some(UiSelector {
                scrollable: Some(true),
                ..UiSelector::default()
            })
        }
        _ => None,
    }
}

fn extract_labeled_phrase(raw: &str, prefixes: &[&str]) -> Option<String> {
    let lowered = raw.to_ascii_lowercase();
    for prefix in prefixes {
        if !lowered.starts_with(prefix) {
            continue;
        }
        let label = raw[prefix.len()..]
            .trim()
            .trim_matches(|ch| matches!(ch, '"' | '\''))
            .trim();
        if !label.is_empty() {
            return Some(label.to_string());
        }
    }
    None
}

fn interactive_selector_matches(
    node: roxmltree::Node<'_, '_>,
    normalized: &NormalizedUiNode,
    selector: &UiSelector,
) -> bool {
    if selector_matches(normalized, selector) {
        return true;
    }
    let Some(label) = selector.label.as_deref() else {
        return false;
    };
    let wanted = label.trim();
    if wanted.is_empty() {
        return false;
    }

    let mut selector_without_label = selector.clone();
    selector_without_label.label = None;
    selector_without_label.label_exact = None;
    if !selector_matches(normalized, &selector_without_label) {
        return false;
    }

    let exact = selector.label_exact.unwrap_or(false);
    node.descendants()
        .skip(1)
        .filter(|descendant| descendant.has_tag_name("node"))
        .any(|descendant| {
            matches_text(non_empty_attr(descendant, "text").as_deref(), wanted, exact)
                || matches_text(
                    non_empty_attr(descendant, "content-desc").as_deref(),
                    wanted,
                    exact,
                )
        })
}

fn normalized_ui_node_from_xml(node: roxmltree::Node<'_, '_>) -> NormalizedUiNode {
    let bounds = node.attribute("bounds").and_then(parse_bounds);
    NormalizedUiNode {
        class_name: non_empty_attr(node, "class"),
        package_name: non_empty_attr(node, "package"),
        text: non_empty_attr(node, "text"),
        semantic_label: semantic_label_from_xml(node),
        content_desc: non_empty_attr(node, "content-desc"),
        resource_id: non_empty_attr(node, "resource-id"),
        clickable: bool_attr(node, "clickable"),
        focusable: bool_attr(node, "focusable"),
        enabled: bool_attr_default_true(node, "enabled"),
        selected: bool_attr(node, "selected"),
        checked: bool_attr(node, "checked"),
        focused: bool_attr(node, "focused"),
        scrollable: bool_attr(node, "scrollable"),
        long_clickable: bool_attr(node, "long-clickable"),
        center: bounds.map(UiBounds::center),
        bounds,
    }
}

fn visible_label_for_node(node: &NormalizedUiNode) -> Option<String> {
    [
        node.semantic_label.as_deref(),
        node.text.as_deref(),
        node.content_desc.as_deref(),
        node.resource_id
            .as_deref()
            .and_then(|resource_id| resource_id.rsplit('/').next()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .map(ToOwned::to_owned)
}

fn direct_visible_label_for_node(node: &NormalizedUiNode) -> Option<String> {
    [
        node.text.as_deref(),
        node.content_desc.as_deref(),
        node.resource_id
            .as_deref()
            .and_then(|resource_id| resource_id.rsplit('/').next()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .map(ToOwned::to_owned)
}

fn scrollable_visible_label_for_node(
    node: &NormalizedUiNode,
    bounds: UiBounds,
    viewport: Option<UiBounds>,
) -> Option<String> {
    if !node.scrollable || viewport.is_some_and(|viewport| bounds == viewport) {
        return None;
    }

    let class_name = node.class_name.as_deref().unwrap_or_default();
    let class_name = class_name.to_ascii_lowercase();
    let label = if class_name.contains("recyclerview") || class_name.contains("listview") {
        "Scrollable list"
    } else if class_name.contains("viewpager") {
        "Scrollable pages"
    } else if class_name.contains("scrollview") {
        "Scrollable view"
    } else {
        "Scrollable region"
    };
    Some(label.to_string())
}

fn semantic_label_from_xml(node: roxmltree::Node<'_, '_>) -> Option<String> {
    if let Some(text) = non_empty_attr(node, "text") {
        return Some(text);
    }
    if let Some(content_desc) = non_empty_attr(node, "content-desc") {
        return Some(content_desc);
    }
    let is_interactive =
        node.attribute("clickable") == Some("true") || node.attribute("focusable") == Some("true");
    if !is_interactive {
        return None;
    }
    node.descendants()
        .skip(1)
        .filter(|descendant| descendant.has_tag_name("node"))
        .find_map(|descendant| {
            non_empty_attr(descendant, "text")
                .or_else(|| non_empty_attr(descendant, "content-desc"))
        })
}

fn interactive_bounds_from_node(node: roxmltree::Node<'_, '_>) -> Option<UiBounds> {
    let clickable = node.attribute("clickable") == Some("true");
    let focusable = node.attribute("focusable") == Some("true");
    if clickable || focusable {
        parse_bounds(node.attribute("bounds")?)
    } else {
        None
    }
}

fn non_empty_attr(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.attribute(name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn compact_output_text(value: Option<String>) -> Option<String> {
    value.map(|value| {
        let original_chars = value.chars().count();
        if original_chars <= UI_OUTPUT_TEXT_CHAR_LIMIT {
            return value;
        }
        let prefix: String = value.chars().take(UI_OUTPUT_TEXT_CHAR_LIMIT).collect();
        format!("{prefix}... [truncated; original_chars={original_chars}]")
    })
}

fn compact_visible_label(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let original_chars = normalized.chars().count();
    if original_chars <= UI_VISIBLE_LABEL_CHAR_LIMIT {
        return normalized;
    }
    let prefix = normalized
        .chars()
        .take(UI_VISIBLE_LABEL_CHAR_LIMIT)
        .collect::<String>();
    format!(
        "{}... [truncated; original_chars={original_chars}]",
        prefix.trim_end()
    )
}

fn compact_visible_label_with_state(
    value: &str,
    node: &NormalizedUiNode,
    visibility: &ViewportVisibility,
) -> String {
    let suffix = visible_label_state_suffix(value, node, visibility);
    let Some(suffix) = suffix else {
        return compact_visible_label(value);
    };

    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let original_chars = normalized.chars().count();
    if original_chars + suffix.chars().count() <= UI_VISIBLE_LABEL_CHAR_LIMIT {
        return format!("{normalized}{suffix}");
    }

    let marker = format!("... [truncated; original_chars={original_chars}]{suffix}");
    let marker_chars = marker.chars().count();
    if marker_chars >= UI_VISIBLE_LABEL_CHAR_LIMIT {
        return compact_visible_label(&format!("{normalized}{suffix}"));
    }

    let prefix_chars = UI_VISIBLE_LABEL_CHAR_LIMIT - marker_chars;
    let prefix = normalized.chars().take(prefix_chars).collect::<String>();
    format!("{}{}", prefix.trim_end(), marker)
}

fn visible_label_state_suffix(
    label: &str,
    node: &NormalizedUiNode,
    visibility: &ViewportVisibility,
) -> Option<String> {
    let mut tags = Vec::new();

    if !node.enabled {
        tags.push("disabled".to_string());
    }

    let lower_label = label.to_ascii_lowercase();
    if node.scrollable && !lower_label.contains("scrollable") {
        tags.push("scrollable".to_string());
    }

    if visibility.clipped {
        let mut clipped = "clipped".to_string();
        if !visibility.clip_edges.is_empty() {
            clipped.push(' ');
            clipped.push_str(&visibility.clip_edges.join("/"));
        }
        if visibility.visible_fraction_percent < 100 {
            clipped.push_str(&format!(" {}%", visibility.visible_fraction_percent));
        }
        tags.push(clipped);
    }

    if tags.is_empty() {
        None
    } else {
        Some(format!(" [{}]", tags.join("; ")))
    }
}

fn bool_attr(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.attribute(name) == Some("true")
}

fn bool_attr_default_true(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.attribute(name)
        .map(|value| value == "true")
        .unwrap_or(true)
}
