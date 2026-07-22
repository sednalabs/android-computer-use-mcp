//! Verification and postcondition data models, async runners, and pure helpers.
//!
//! ## Rationale
//! Implements the core logic for ensuring that tool executions succeed in
//! the dynamic environment of an Android emulator (e.g., waiting for screen
//! stabilization or text input confirmation).
//!
//! ## Security Boundaries
//! * All verification checks are performed on captured state files.
//! * Deterministic timeouts prevent infinite wait loops during verification.
//!
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tokio::time::sleep;

use crate::McpError;
use crate::server::AndroidEmulatorMcp;
use crate::tools::{
    AndroidWindowState, filename_or_timestamp, has_meaningful_observation_budget,
    is_command_timeout_error, remaining_until, remove_artifact_if_exists,
};
use crate::ui::{
    NormalizedUiNode, SelectionFailure, SelectorCandidateSummary, UiBounds, UiSelector,
    matching_nodes, resolve_node_selection, selector_matches,
};

/// Tracks the outcome of a tap-based interaction.
#[derive(Debug, Clone)]
pub(crate) struct TapVerification {
    /// Whether a tap was explicitly requested.
    pub(crate) requested: bool,
    /// Whether the operation required the element to disappear.
    pub(crate) wait_until_absent: bool,
    /// Optional post-tap selector to wait for.
    pub(crate) wait_for_selector: Option<UiSelector>,
    /// Whether the overall tap operation succeeded.
    pub(crate) satisfied: bool,
    /// Whether the UI stabilized after the tap.
    pub(crate) stabilized: bool,
    /// Number of stable poll cycles observed.
    pub(crate) stable_polls_observed: u32,
    /// Number of stable poll cycles required.
    pub(crate) stable_polls_required: u32,
    /// Whether the operation timed out.
    pub(crate) timed_out: bool,
    /// Elapsed time of the verification process.
    pub(crate) elapsed_ms: u128,
    /// Path to the captured hierarchy state.
    pub(crate) hierarchy_path: Option<String>,
    /// Was the target element present before the tap?
    pub(crate) tapped_selector_present_pre_tap: Option<bool>,
    /// Was the post-tap verification element present before the tap?
    pub(crate) post_selector_matched_pre_tap: Option<bool>,
    /// Is the target element still present after the tap?
    pub(crate) tapped_selector_still_present: Option<bool>,
    /// Did the post-tap verification element match after the tap?
    pub(crate) post_selector_matched: Option<bool>,
    /// Was a change in the UI detected post-tap?
    pub(crate) ui_changed_from_pre_tap: Option<bool>,
}

pub(crate) struct TapVerificationStatus {
    pub(crate) tapped_present: bool,
    pub(crate) post_present: Option<bool>,
    #[allow(dead_code)]
    pub(crate) pre_post_present: Option<bool>,
    pub(crate) ui_changed_from_pre_tap: Option<bool>,
    pub(crate) satisfied: bool,
}

/// Tracks the outcome of a text input-based interaction.
#[derive(Debug, Clone)]
pub(crate) struct TextVerification {
    /// Whether a text input was explicitly requested.
    pub(crate) requested: bool,
    /// The target element selector for input.
    pub(crate) target_selector: Option<UiSelector>,
    /// Optional post-input selector to wait for.
    pub(crate) wait_for_selector: Option<UiSelector>,
    /// Whether the overall input operation succeeded.
    pub(crate) satisfied: bool,
    /// Whether the UI stabilized after input.
    pub(crate) stabilized: bool,
    /// Number of stable poll cycles observed.
    pub(crate) stable_polls_observed: u32,
    /// Number of stable poll cycles required.
    pub(crate) stable_polls_required: u32,
    /// Whether the operation timed out.
    pub(crate) timed_out: bool,
    /// Elapsed time of the verification process.
    pub(crate) elapsed_ms: u128,
    /// Path to the captured hierarchy state.
    pub(crate) hierarchy_path: Option<String>,
    /// Was the target element present before input?
    pub(crate) target_selector_present_pre_text: Option<bool>,
    /// Was the post-input verification element present before input?
    pub(crate) post_selector_matched_pre_text: Option<bool>,
    /// Is the target element still present after input?
    pub(crate) target_selector_still_present: Option<bool>,
    /// Did the input text match the requested value?
    pub(crate) target_text_matches_requested: Option<bool>,
    /// Did the post-input verification element match after input?
    pub(crate) post_selector_matched: Option<bool>,
    /// Was a change in the UI detected post-input?
    pub(crate) ui_changed_from_pre_text: Option<bool>,
}

pub(crate) struct TextVerificationStatus {
    pub(crate) target_present: Option<bool>,
    pub(crate) post_present: Option<bool>,
    pub(crate) pre_post_present: Option<bool>,
    pub(crate) target_text_matches_requested: Option<bool>,
    pub(crate) ui_changed_from_pre_text: Option<bool>,
    pub(crate) satisfied: bool,
}

pub(crate) struct TextVerificationRequest<'a> {
    pub(crate) serial: &'a str,
    pub(crate) pre_text_nodes: &'a [NormalizedUiNode],
    pub(crate) public_target_selector: Option<&'a UiSelector>,
    pub(crate) target_tracker: Option<&'a InternalNodeTracker>,
    pub(crate) target_selector: Option<&'a UiSelector>,
    pub(crate) expected_text: Option<&'a str>,
    pub(crate) wait_for_selector: Option<&'a UiSelector>,
    pub(crate) deadline: Instant,
    pub(crate) hierarchy_filename: Option<String>,
}

pub(crate) struct VerifiedTextDispatchRequest<'a> {
    pub(crate) serial: &'a str,
    pub(crate) text: &'a str,
    pub(crate) pre_text_nodes: &'a [NormalizedUiNode],
    pub(crate) public_target_selector: Option<&'a UiSelector>,
    pub(crate) target_tracker: Option<&'a InternalNodeTracker>,
    pub(crate) target_selector: Option<&'a UiSelector>,
    pub(crate) wait_for_selector: Option<&'a UiSelector>,
    pub(crate) deadline: Instant,
    pub(crate) hierarchy_filename: Option<String>,
    pub(crate) retry_with_adb_on_no_change: bool,
    pub(crate) replace_existing_text_on_adb: bool,
    pub(crate) existing_text_for_adb_replace: Option<&'a str>,
}

pub(crate) struct ToolPostconditionRequest<'a> {
    pub(crate) serial: &'a str,
    pub(crate) selector: Option<&'a UiSelector>,
    pub(crate) match_index: Option<usize>,
    pub(crate) wait_for_activity: Option<&'a str>,
    pub(crate) wait_for_package: Option<&'a str>,
    pub(crate) deadline: Instant,
    pub(crate) include_screenshot: bool,
    pub(crate) artifact_prefix: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolPostconditionEvidenceSource {
    WindowState,
    UiHierarchy,
}

impl ToolPostconditionEvidenceSource {
    pub(crate) fn for_launch(wait_for_selector_present: bool) -> Self {
        if wait_for_selector_present {
            Self::UiHierarchy
        } else {
            Self::WindowState
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::WindowState => "window_state",
            Self::UiHierarchy => "ui_hierarchy",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ToolPostconditionResult {
    pub(crate) requested: bool,
    pub(crate) satisfied: bool,
    pub(crate) timed_out: bool,
    pub(crate) elapsed_ms: u128,
    pub(crate) evidence_source: Option<ToolPostconditionEvidenceSource>,
    pub(crate) hierarchy_path: Option<String>,
    pub(crate) screenshot_path: Option<String>,
    pub(crate) observed_activity: Option<String>,
    pub(crate) observed_package: Option<String>,
    pub(crate) node: Option<NormalizedUiNode>,
    pub(crate) match_count: usize,
    pub(crate) selected_match_index: Option<usize>,
    pub(crate) candidate_summary: Vec<SelectorCandidateSummary>,
}

pub(crate) struct TapVerificationRequest<'a> {
    pub(crate) serial: &'a str,
    pub(crate) tapped_selector: &'a UiSelector,
    pub(crate) pre_tap_nodes: &'a [NormalizedUiNode],
    pub(crate) wait_until_absent: bool,
    pub(crate) wait_for_tracker: Option<&'a InternalNodeTracker>,
    pub(crate) wait_for_selector: Option<&'a UiSelector>,
    pub(crate) deadline: Instant,
    pub(crate) hierarchy_filename: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum InternalNodeTracker {
    Selector(UiSelector),
    Identity {
        resource_id: Option<String>,
        class_name: Option<String>,
        bounds: UiBounds,
        focused: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SemanticUiNodeFingerprint {
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
}

impl InternalNodeTracker {
    pub(crate) fn from_target_node(node: &NormalizedUiNode) -> Option<Self> {
        Self::from_target_node_with_focus(node, None)
    }

    pub(crate) fn from_target_node_with_focus(
        node: &NormalizedUiNode,
        focused: Option<bool>,
    ) -> Option<Self> {
        node.bounds.map(|bounds| Self::Identity {
            resource_id: node.resource_id.clone(),
            class_name: node.class_name.clone(),
            bounds,
            focused,
        })
    }
}

impl AndroidEmulatorMcp {
    pub(crate) async fn verify_tap_outcome(
        &self,
        request: TapVerificationRequest<'_>,
    ) -> Result<TapVerification, McpError> {
        let TapVerificationRequest {
            serial,
            tapped_selector,
            pre_tap_nodes,
            wait_until_absent,
            wait_for_tracker,
            wait_for_selector,
            deadline,
            hierarchy_filename,
        } = request;
        let requested =
            wait_until_absent || wait_for_tracker.is_some() || wait_for_selector.is_some();
        let pre_status = tap_verification_status(
            pre_tap_nodes,
            pre_tap_nodes,
            tapped_selector,
            false,
            wait_for_tracker,
            wait_for_selector,
            None,
        );
        let pre_tapped_present = pre_status.tapped_present;
        let pre_post_present = pre_status.post_present;
        if !requested {
            return Ok(TapVerification {
                requested: false,
                wait_until_absent,
                wait_for_selector: wait_for_selector.cloned(),
                satisfied: true,
                stabilized: true,
                stable_polls_observed: 0,
                stable_polls_required: 0,
                timed_out: false,
                elapsed_ms: 0,
                hierarchy_path: None,
                tapped_selector_present_pre_tap: Some(pre_tapped_present),
                post_selector_matched_pre_tap: pre_post_present,
                tapped_selector_still_present: None,
                post_selector_matched: None,
                ui_changed_from_pre_tap: Some(false),
            });
        }

        let poll_interval = Duration::from_millis(250);
        let required_stable_polls = 2u32;
        let mut last_hierarchy_path = None;
        let mut tapped_selector_still_present = None;
        let mut post_selector_matched = None;
        let mut last_fingerprint: Option<String> = None;
        let mut last_fast_fingerprint: Option<String> = None;
        let mut use_fast_fingerprint_backend: Option<bool> = None;
        let mut stable_polls_observed = 0u32;
        let mut satisfied_polls_observed = 0u32;
        let started = Instant::now();
        let mut ui_changed_from_pre_tap = Some(false);
        let required_satisfied_polls = 2u32;
        let mut force_followup_semantic_capture = false;
        let target_package = infer_unique_package_name(&relevant_selector_snapshot(
            pre_tap_nodes,
            tapped_selector,
            wait_for_tracker,
            wait_for_selector,
        ));
        while Instant::now() < deadline {
            if !has_meaningful_observation_budget(deadline) {
                break;
            }
            let final_poll = remaining_until(deadline) <= poll_interval;
            let mut fast_fingerprint = if use_fast_fingerprint_backend == Some(false) {
                None
            } else {
                self.capture_fast_ui_fingerprint_until_deadline(
                    serial,
                    deadline,
                    target_package.as_deref(),
                )
                .await?
            };
            if use_fast_fingerprint_backend.is_none() {
                use_fast_fingerprint_backend = Some(fast_fingerprint.is_some());
            } else if use_fast_fingerprint_backend == Some(true) && fast_fingerprint.is_none() {
                use_fast_fingerprint_backend = Some(false);
            }
            let should_capture_semantic = should_refresh_semantic_observation(
                last_fast_fingerprint.as_deref(),
                fast_fingerprint.as_deref(),
                force_followup_semantic_capture,
                final_poll,
            );
            if let Some(fast_fingerprint) = fast_fingerprint.take() {
                last_fast_fingerprint = Some(fast_fingerprint);
            }
            if !should_capture_semantic {
                sleep(poll_interval).await;
                continue;
            }
            let Some(observation) = self
                .capture_ui_observation_until_deadline(
                    serial,
                    filename_or_timestamp(hierarchy_filename.clone(), "tap-element-verify", "xml"),
                    false,
                    filename_or_timestamp(hierarchy_filename.clone(), "tap-element-verify", "png"),
                    deadline,
                )
                .await?
            else {
                break;
            };
            remove_artifact_if_exists(last_hierarchy_path.clone()).await;
            let fingerprint = tap_verification_fingerprint(
                &observation.nodes,
                tapped_selector,
                wait_for_tracker,
                wait_for_selector,
            );
            if last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                stable_polls_observed += 1;
            } else {
                stable_polls_observed = 1;
                last_fingerprint = Some(fingerprint);
            }
            let status = tap_verification_status(
                pre_tap_nodes,
                &observation.nodes,
                tapped_selector,
                wait_until_absent,
                wait_for_tracker,
                wait_for_selector,
                Some(pre_tapped_present),
            );
            tapped_selector_still_present = Some(status.tapped_present);
            post_selector_matched = status.post_present;
            ui_changed_from_pre_tap = status.ui_changed_from_pre_tap;
            last_hierarchy_path = Some(observation.hierarchy_path.display().to_string());
            if status.satisfied {
                satisfied_polls_observed += 1;
                force_followup_semantic_capture =
                    satisfied_polls_observed < required_satisfied_polls;
            } else {
                satisfied_polls_observed = 0;
                force_followup_semantic_capture = false;
            }

            if tap_verification_is_confirmed(
                &status,
                satisfied_polls_observed,
                required_satisfied_polls,
                final_poll,
            ) {
                return Ok(TapVerification {
                    requested: true,
                    wait_until_absent,
                    wait_for_selector: wait_for_selector.cloned(),
                    satisfied: true,
                    stabilized: stable_polls_observed >= required_stable_polls,
                    stable_polls_observed,
                    stable_polls_required: required_stable_polls,
                    timed_out: false,
                    elapsed_ms: started.elapsed().as_millis(),
                    hierarchy_path: last_hierarchy_path,
                    tapped_selector_present_pre_tap: Some(pre_tapped_present),
                    post_selector_matched_pre_tap: pre_post_present,
                    tapped_selector_still_present,
                    post_selector_matched,
                    ui_changed_from_pre_tap,
                });
            }

            if final_poll {
                break;
            }
            sleep(poll_interval).await;
        }

        Ok(TapVerification {
            requested: true,
            wait_until_absent,
            wait_for_selector: wait_for_selector.cloned(),
            satisfied: false,
            stabilized: false,
            stable_polls_observed,
            stable_polls_required: required_stable_polls,
            timed_out: true,
            elapsed_ms: started.elapsed().as_millis(),
            hierarchy_path: last_hierarchy_path,
            tapped_selector_present_pre_tap: Some(pre_tapped_present),
            post_selector_matched_pre_tap: pre_post_present,
            tapped_selector_still_present,
            post_selector_matched,
            ui_changed_from_pre_tap,
        })
    }

    pub(crate) async fn verify_text_outcome(
        &self,
        request: TextVerificationRequest<'_>,
    ) -> Result<TextVerification, McpError> {
        let TextVerificationRequest {
            serial,
            pre_text_nodes,
            public_target_selector,
            target_tracker,
            target_selector,
            expected_text,
            wait_for_selector,
            deadline,
            hierarchy_filename,
        } = request;
        let requested =
            target_tracker.is_some() || target_selector.is_some() || wait_for_selector.is_some();
        let pre_status = text_verification_status(
            pre_text_nodes,
            pre_text_nodes,
            target_tracker,
            target_selector,
            expected_text,
            wait_for_selector,
        );
        let pre_target_present = pre_status.target_present;
        let pre_post_present = pre_status.pre_post_present;
        if !requested {
            return Ok(TextVerification {
                requested: false,
                target_selector: public_target_selector
                    .cloned()
                    .or_else(|| target_selector.cloned()),
                wait_for_selector: wait_for_selector.cloned(),
                satisfied: true,
                stabilized: true,
                stable_polls_observed: 0,
                stable_polls_required: 0,
                timed_out: false,
                elapsed_ms: 0,
                hierarchy_path: None,
                target_selector_present_pre_text: pre_target_present,
                post_selector_matched_pre_text: pre_post_present,
                target_selector_still_present: None,
                target_text_matches_requested: pre_status.target_text_matches_requested,
                post_selector_matched: None,
                ui_changed_from_pre_text: Some(false),
            });
        }

        let poll_interval = Duration::from_millis(250);
        let required_stable_polls = 2u32;
        let required_satisfied_polls = 2u32;
        let mut last_hierarchy_path = None;
        let mut target_selector_still_present = None;
        let mut target_text_matches_requested = None;
        let mut post_selector_matched = None;
        let mut last_fingerprint: Option<String> = None;
        let mut last_fast_fingerprint: Option<String> = None;
        let mut use_fast_fingerprint_backend: Option<bool> = None;
        let mut stable_polls_observed = 0u32;
        let mut satisfied_polls_observed = 0u32;
        let started = Instant::now();
        let mut ui_changed_from_pre_text = Some(false);
        let mut force_followup_semantic_capture = false;
        let target_package = infer_unique_package_name(&relevant_text_snapshot(
            pre_text_nodes,
            target_tracker,
            target_selector,
            wait_for_selector,
        ));

        while Instant::now() < deadline {
            if !has_meaningful_observation_budget(deadline) {
                break;
            }
            let final_poll = remaining_until(deadline) <= poll_interval;
            let mut fast_fingerprint = if use_fast_fingerprint_backend == Some(false) {
                None
            } else {
                self.capture_fast_ui_fingerprint_until_deadline(
                    serial,
                    deadline,
                    target_package.as_deref(),
                )
                .await?
            };
            if use_fast_fingerprint_backend.is_none() {
                use_fast_fingerprint_backend = Some(fast_fingerprint.is_some());
            } else if use_fast_fingerprint_backend == Some(true) && fast_fingerprint.is_none() {
                use_fast_fingerprint_backend = Some(false);
            }
            let should_capture_semantic = should_refresh_semantic_observation(
                last_fast_fingerprint.as_deref(),
                fast_fingerprint.as_deref(),
                force_followup_semantic_capture,
                final_poll,
            );
            if let Some(fast_fingerprint) = fast_fingerprint.take() {
                last_fast_fingerprint = Some(fast_fingerprint);
            }
            if !should_capture_semantic {
                sleep(poll_interval).await;
                continue;
            }
            let Some(observation) = self
                .capture_ui_observation_until_deadline(
                    serial,
                    filename_or_timestamp(hierarchy_filename.clone(), "input-text-verify", "xml"),
                    false,
                    filename_or_timestamp(hierarchy_filename.clone(), "input-text-verify", "png"),
                    deadline,
                )
                .await?
            else {
                break;
            };
            remove_artifact_if_exists(last_hierarchy_path.clone()).await;
            let fingerprint = text_verification_fingerprint(
                &observation.nodes,
                target_tracker,
                target_selector,
                wait_for_selector,
            );
            if last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                stable_polls_observed += 1;
            } else {
                stable_polls_observed = 1;
                last_fingerprint = Some(fingerprint);
            }
            let status = text_verification_status(
                pre_text_nodes,
                &observation.nodes,
                target_tracker,
                target_selector,
                expected_text,
                wait_for_selector,
            );
            target_selector_still_present = status.target_present;
            target_text_matches_requested = status.target_text_matches_requested;
            post_selector_matched = status.post_present;
            ui_changed_from_pre_text = status.ui_changed_from_pre_text;
            last_hierarchy_path = Some(observation.hierarchy_path.display().to_string());
            if status.satisfied {
                satisfied_polls_observed += 1;
                force_followup_semantic_capture =
                    satisfied_polls_observed < required_satisfied_polls;
            } else {
                satisfied_polls_observed = 0;
                force_followup_semantic_capture = false;
            }

            if text_verification_is_confirmed(
                &status,
                satisfied_polls_observed,
                required_satisfied_polls,
                final_poll,
            ) {
                return Ok(TextVerification {
                    requested: true,
                    target_selector: public_target_selector
                        .cloned()
                        .or_else(|| target_selector.cloned()),
                    wait_for_selector: wait_for_selector.cloned(),
                    satisfied: true,
                    stabilized: stable_polls_observed >= required_stable_polls,
                    stable_polls_observed,
                    stable_polls_required: required_stable_polls,
                    timed_out: false,
                    elapsed_ms: started.elapsed().as_millis(),
                    hierarchy_path: last_hierarchy_path,
                    target_selector_present_pre_text: pre_target_present,
                    post_selector_matched_pre_text: pre_post_present,
                    target_selector_still_present,
                    target_text_matches_requested,
                    post_selector_matched,
                    ui_changed_from_pre_text,
                });
            }

            if final_poll {
                break;
            }
            sleep(poll_interval).await;
        }

        Ok(TextVerification {
            requested: true,
            target_selector: public_target_selector
                .cloned()
                .or_else(|| target_selector.cloned()),
            wait_for_selector: wait_for_selector.cloned(),
            satisfied: false,
            stabilized: false,
            stable_polls_observed,
            stable_polls_required: required_stable_polls,
            timed_out: true,
            elapsed_ms: started.elapsed().as_millis(),
            hierarchy_path: last_hierarchy_path,
            target_selector_present_pre_text: pre_target_present,
            post_selector_matched_pre_text: pre_post_present,
            target_selector_still_present,
            target_text_matches_requested,
            post_selector_matched,
            ui_changed_from_pre_text,
        })
    }

    pub(crate) async fn wait_for_window_state_postcondition(
        &self,
        serial: &str,
        wait_for_activity: Option<&str>,
        wait_for_package: Option<&str>,
        deadline: Instant,
    ) -> Result<ToolPostconditionResult, McpError> {
        let requested = wait_for_activity.is_some() || wait_for_package.is_some();
        if !requested {
            return Ok(ToolPostconditionResult {
                requested: false,
                satisfied: true,
                timed_out: false,
                elapsed_ms: 0,
                evidence_source: None,
                hierarchy_path: None,
                screenshot_path: None,
                observed_activity: None,
                observed_package: None,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            });
        }

        let poll_interval = Duration::from_millis(250);
        let started = Instant::now();
        let mut last_result = ToolPostconditionResult {
            requested: true,
            satisfied: false,
            timed_out: false,
            elapsed_ms: 0,
            evidence_source: Some(ToolPostconditionEvidenceSource::WindowState),
            hierarchy_path: None,
            screenshot_path: None,
            observed_activity: None,
            observed_package: None,
            node: None,
            match_count: 0,
            selected_match_index: None,
            candidate_summary: Vec::new(),
        };

        while Instant::now() < deadline {
            if !has_meaningful_observation_budget(deadline) {
                break;
            }
            let window_state = match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.window_state_internal_with_timeout(serial, remaining_until(deadline)),
            )
            .await
            {
                Ok(Ok(state)) => state,
                Ok(Err(error)) if is_command_timeout_error(&error) => break,
                Ok(Err(error)) => return Err(error),
                Err(_) => break,
            };
            let (observed_activity, observed_package, satisfied) =
                window_state_postcondition_matches(
                    &window_state,
                    wait_for_activity,
                    wait_for_package,
                );
            last_result = ToolPostconditionResult {
                requested: true,
                satisfied,
                timed_out: false,
                elapsed_ms: started.elapsed().as_millis(),
                evidence_source: Some(ToolPostconditionEvidenceSource::WindowState),
                hierarchy_path: None,
                screenshot_path: None,
                observed_activity,
                observed_package,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            };
            if satisfied {
                return Ok(last_result);
            }

            if remaining_until(deadline) <= poll_interval {
                break;
            }
            sleep(poll_interval).await;
        }

        last_result.timed_out = true;
        last_result.elapsed_ms = started.elapsed().as_millis();
        Ok(last_result)
    }

    pub(crate) async fn wait_for_tool_postcondition(
        &self,
        request: ToolPostconditionRequest<'_>,
    ) -> Result<ToolPostconditionResult, McpError> {
        let ToolPostconditionRequest {
            serial,
            selector,
            match_index,
            wait_for_activity,
            wait_for_package,
            deadline,
            include_screenshot,
            artifact_prefix,
        } = request;
        let requested =
            selector.is_some() || wait_for_activity.is_some() || wait_for_package.is_some();
        if !requested {
            return Ok(ToolPostconditionResult {
                requested: false,
                satisfied: true,
                timed_out: false,
                elapsed_ms: 0,
                evidence_source: None,
                hierarchy_path: None,
                screenshot_path: None,
                observed_activity: None,
                observed_package: None,
                node: None,
                match_count: 0,
                selected_match_index: None,
                candidate_summary: Vec::new(),
            });
        }

        let poll_interval = Duration::from_millis(250);
        let started = Instant::now();
        let selector_only_fast_path =
            selector.is_some() && wait_for_activity.is_none() && wait_for_package.is_none();
        let mut last_fast_fingerprint: Option<String> = None;
        let mut use_fast_fingerprint_backend: Option<bool> = None;
        let mut last_result = ToolPostconditionResult {
            requested: true,
            satisfied: false,
            timed_out: false,
            elapsed_ms: 0,
            evidence_source: Some(ToolPostconditionEvidenceSource::UiHierarchy),
            hierarchy_path: None,
            screenshot_path: None,
            observed_activity: None,
            observed_package: None,
            node: None,
            match_count: 0,
            selected_match_index: None,
            candidate_summary: Vec::new(),
        };

        while Instant::now() < deadline {
            if !has_meaningful_observation_budget(deadline) {
                break;
            }
            let final_poll = remaining_until(deadline) <= poll_interval;
            let mut fast_fingerprint =
                if !selector_only_fast_path || use_fast_fingerprint_backend == Some(false) {
                    None
                } else {
                    self.capture_fast_ui_fingerprint_until_deadline(serial, deadline, None)
                        .await?
                };
            if selector_only_fast_path {
                if use_fast_fingerprint_backend.is_none() {
                    use_fast_fingerprint_backend = Some(fast_fingerprint.is_some());
                } else if use_fast_fingerprint_backend == Some(true) && fast_fingerprint.is_none() {
                    use_fast_fingerprint_backend = Some(false);
                }
                let should_capture_semantic = should_refresh_tool_postcondition_observation(
                    last_fast_fingerprint.as_deref(),
                    fast_fingerprint.as_deref(),
                    selector_only_fast_path,
                    final_poll,
                );
                if let Some(fast_fingerprint) = fast_fingerprint.take() {
                    last_fast_fingerprint = Some(fast_fingerprint);
                }
                if !should_capture_semantic {
                    sleep(poll_interval).await;
                    continue;
                }
            }
            let Some(observation) = self
                .capture_ui_observation_until_deadline(
                    serial,
                    filename_or_timestamp(None, artifact_prefix, "xml"),
                    include_screenshot,
                    filename_or_timestamp(None, artifact_prefix, "png"),
                    deadline,
                )
                .await?
            else {
                break;
            };
            remove_artifact_if_exists(last_result.hierarchy_path.clone()).await;
            let (observed_activity, observed_package, window_state_ok) =
                window_state_postcondition_matches(
                    &observation.window_state,
                    wait_for_activity,
                    wait_for_package,
                );
            let (node, match_count, selected_match_index, candidate_summary, selector_ok) =
                if let Some(selector) = selector {
                    let matches = matching_nodes(&observation.nodes, selector).matches;
                    match resolve_node_selection(matches, match_index) {
                        Ok(selection) => (
                            Some(selection.node),
                            selection.match_count,
                            Some(selection.selected_match_index),
                            selection.candidates,
                            true,
                        ),
                        Err(SelectionFailure::NoMatches) => (None, 0, None, Vec::new(), false),
                        Err(error) => (
                            None,
                            match &error {
                                SelectionFailure::Ambiguous { match_count, .. } => *match_count,
                                SelectionFailure::MatchIndexOutOfRange { match_count, .. } => {
                                    *match_count
                                }
                                SelectionFailure::NoMatches => 0,
                            },
                            None,
                            match error {
                                SelectionFailure::Ambiguous { candidates, .. }
                                | SelectionFailure::MatchIndexOutOfRange { candidates, .. } => {
                                    candidates
                                }
                                SelectionFailure::NoMatches => Vec::new(),
                            },
                            false,
                        ),
                    }
                } else {
                    (None, 0, None, Vec::new(), true)
                };
            let satisfied = selector_ok && window_state_ok;
            last_result = ToolPostconditionResult {
                requested: true,
                satisfied,
                timed_out: false,
                elapsed_ms: started.elapsed().as_millis(),
                evidence_source: Some(ToolPostconditionEvidenceSource::UiHierarchy),
                hierarchy_path: Some(observation.hierarchy_path.display().to_string()),
                screenshot_path: observation
                    .screenshot_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                observed_activity,
                observed_package,
                node,
                match_count,
                selected_match_index,
                candidate_summary,
            };
            if satisfied {
                return Ok(last_result);
            }
            if final_poll {
                break;
            }
            sleep(poll_interval).await;
        }

        last_result.timed_out = true;
        last_result.elapsed_ms = started.elapsed().as_millis();
        Ok(last_result)
    }
}

pub(crate) fn tap_verification_json(verification: &TapVerification) -> serde_json::Value {
    json!({
        "requested": verification.requested,
        "wait_until_absent": verification.wait_until_absent,
        "wait_for_selector": verification.wait_for_selector,
        "satisfied": verification.satisfied,
        "stabilized": verification.stabilized,
        "stable_polls_observed": verification.stable_polls_observed,
        "stable_polls_required": verification.stable_polls_required,
        "timed_out": verification.timed_out,
        "elapsed_ms": verification.elapsed_ms,
        "tapped_selector_present_pre_tap": verification.tapped_selector_present_pre_tap,
        "post_selector_matched_pre_tap": verification.post_selector_matched_pre_tap,
        "tapped_selector_still_present": verification.tapped_selector_still_present,
        "post_selector_matched": verification.post_selector_matched,
        "ui_changed_from_pre_tap": verification.ui_changed_from_pre_tap,
    })
}

pub(crate) fn tap_verification_summary(verification: &TapVerification) -> String {
    tap_verification_json(verification).to_string()
}

pub(crate) fn text_verification_json(verification: &TextVerification) -> serde_json::Value {
    json!({
        "requested": verification.requested,
        "target_selector": verification.target_selector,
        "wait_for_selector": verification.wait_for_selector,
        "satisfied": verification.satisfied,
        "stabilized": verification.stabilized,
        "stable_polls_observed": verification.stable_polls_observed,
        "stable_polls_required": verification.stable_polls_required,
        "timed_out": verification.timed_out,
        "elapsed_ms": verification.elapsed_ms,
        "target_selector_present_pre_text": verification.target_selector_present_pre_text,
        "post_selector_matched_pre_text": verification.post_selector_matched_pre_text,
        "target_selector_still_present": verification.target_selector_still_present,
        "post_selector_matched": verification.post_selector_matched,
        "ui_changed_from_pre_text": verification.ui_changed_from_pre_text,
    })
}

pub(crate) fn text_verification_summary(verification: &TextVerification) -> String {
    text_verification_json(verification).to_string()
}

pub(crate) fn tool_postcondition_json(result: &ToolPostconditionResult) -> serde_json::Value {
    json!({
        "requested": result.requested,
        "satisfied": result.satisfied,
        "timed_out": result.timed_out,
        "elapsed_ms": result.elapsed_ms,
        "evidence_source": result.evidence_source.map(ToolPostconditionEvidenceSource::as_str),
        "artifacts": {
            "hierarchy_path": result.hierarchy_path,
            "screenshot_path": result.screenshot_path,
        },
        "observed_activity": result.observed_activity,
        "observed_package": result.observed_package,
        "match_count": result.match_count,
        "selected_match_index": result.selected_match_index,
        "candidate_summary": result.candidate_summary,
        "node": result.node,
    })
}

pub(crate) fn ensure_tool_postcondition_satisfied(
    tool_name: &str,
    stage: &str,
    postcondition: &ToolPostconditionResult,
) -> Result<(), McpError> {
    if postcondition.requested && !postcondition.satisfied {
        return Err(McpError::internal_error(
            format!(
                "{tool_name} {stage}: {}",
                tool_postcondition_json(postcondition)
            ),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn ensure_action_outcome_satisfied(
    tool_name: &str,
    stage: &str,
    requested: bool,
    satisfied: bool,
    details: serde_json::Value,
) -> Result<(), McpError> {
    if requested && !satisfied {
        return Err(McpError::internal_error(
            format!("{tool_name} {stage}: {details}"),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn tap_verification_fingerprint(
    nodes: &[NormalizedUiNode],
    tapped_selector: &UiSelector,
    wait_for_tracker: Option<&InternalNodeTracker>,
    wait_for_selector: Option<&UiSelector>,
) -> String {
    let mut relevant = semantic_relevant_selector_snapshot(
        nodes,
        tapped_selector,
        wait_for_tracker,
        wait_for_selector,
    );
    relevant.sort();
    serde_json::to_string(&relevant).unwrap_or_else(|_| format!("{relevant:?}"))
}

pub(crate) fn text_verification_fingerprint(
    nodes: &[NormalizedUiNode],
    target_tracker: Option<&InternalNodeTracker>,
    target_selector: Option<&UiSelector>,
    wait_for_selector: Option<&UiSelector>,
) -> String {
    let mut relevant =
        semantic_relevant_text_snapshot(nodes, target_tracker, target_selector, wait_for_selector);
    relevant.sort();
    serde_json::to_string(&relevant).unwrap_or_else(|_| format!("{relevant:?}"))
}

pub(crate) fn tap_verification_status(
    pre_tap_nodes: &[NormalizedUiNode],
    nodes: &[NormalizedUiNode],
    tapped_selector: &UiSelector,
    wait_until_absent: bool,
    wait_for_tracker: Option<&InternalNodeTracker>,
    wait_for_selector: Option<&UiSelector>,
    tapped_selector_present_pre_tap: Option<bool>,
) -> TapVerificationStatus {
    let tapped_present = nodes
        .iter()
        .any(|node| selector_matches(node, tapped_selector));
    let post_present = if let Some(tracker) = wait_for_tracker {
        Some(nodes.iter().any(|node| tracker_matches_node(node, tracker)))
    } else {
        wait_for_selector.map(|selector| nodes.iter().any(|node| selector_matches(node, selector)))
    };
    let pre_post_present = if let Some(tracker) = wait_for_tracker {
        Some(
            pre_tap_nodes
                .iter()
                .any(|node| tracker_matches_node(node, tracker)),
        )
    } else {
        wait_for_selector.map(|selector| {
            pre_tap_nodes
                .iter()
                .any(|node| selector_matches(node, selector))
        })
    };
    let ui_changed_from_pre_tap = relevant_selector_snapshot(
        pre_tap_nodes,
        tapped_selector,
        wait_for_tracker,
        wait_for_selector,
    ) != relevant_selector_snapshot(
        nodes,
        tapped_selector,
        wait_for_tracker,
        wait_for_selector,
    );
    let absent_ok = if wait_until_absent {
        tapped_selector_present_pre_tap.unwrap_or(true) && !tapped_present
    } else {
        true
    };
    let post_ok = match (wait_for_tracker, wait_for_selector) {
        (Some(_), _) | (None, Some(_)) => {
            let present_after_tap = post_present.unwrap_or(false);
            if !present_after_tap {
                false
            } else if wait_until_absent {
                true
            } else if pre_post_present.unwrap_or(false) {
                ui_changed_from_pre_tap
            } else {
                true
            }
        }
        (None, None) => true,
    };

    TapVerificationStatus {
        tapped_present,
        post_present,
        pre_post_present,
        ui_changed_from_pre_tap: Some(ui_changed_from_pre_tap),
        satisfied: absent_ok && post_ok,
    }
}

pub(crate) fn text_verification_status(
    pre_text_nodes: &[NormalizedUiNode],
    nodes: &[NormalizedUiNode],
    target_tracker: Option<&InternalNodeTracker>,
    target_selector: Option<&UiSelector>,
    expected_text: Option<&str>,
    wait_for_selector: Option<&UiSelector>,
) -> TextVerificationStatus {
    let matching_target_nodes = match (target_tracker, target_selector) {
        (Some(tracker), _) => Some(
            nodes
                .iter()
                .filter(|node| tracker_matches_node(node, tracker))
                .collect::<Vec<_>>(),
        ),
        (None, Some(selector)) => Some(
            nodes
                .iter()
                .filter(|node| selector_matches(node, selector))
                .collect::<Vec<_>>(),
        ),
        (None, None) => None,
    };
    let target_present = matching_target_nodes
        .as_ref()
        .map(|matching_target_nodes| !matching_target_nodes.is_empty());
    let post_present =
        wait_for_selector.map(|selector| nodes.iter().any(|node| selector_matches(node, selector)));
    let pre_post_present = wait_for_selector.map(|selector| {
        pre_text_nodes
            .iter()
            .any(|node| selector_matches(node, selector))
    });
    let target_text_matches_requested = matching_target_nodes.as_ref().zip(expected_text).map(
        |(matching_target_nodes, expected_text)| {
            matching_target_nodes.iter().any(|node| {
                crate::ui::matches_text(node.text.as_deref(), expected_text, true)
                    || crate::ui::matches_text(node.semantic_label.as_deref(), expected_text, true)
                    || crate::ui::matches_text(node.content_desc.as_deref(), expected_text, true)
            })
        },
    );
    let ui_changed_from_pre_text =
        relevant_text_snapshot(
            pre_text_nodes,
            target_tracker,
            target_selector,
            wait_for_selector,
        ) != relevant_text_snapshot(nodes, target_tracker, target_selector, wait_for_selector);
    let target_ok = match (target_tracker, target_selector) {
        (Some(_), _) | (None, Some(_)) => {
            if target_present.unwrap_or(false) {
                target_text_matches_requested.unwrap_or(ui_changed_from_pre_text)
            } else {
                wait_for_selector.is_some() && ui_changed_from_pre_text
            }
        }
        (None, None) => true,
    };
    let post_ok = match wait_for_selector {
        Some(_) => {
            let present_after_text = post_present.unwrap_or(false);
            if !present_after_text {
                false
            } else if pre_post_present.unwrap_or(false) {
                ui_changed_from_pre_text
            } else {
                true
            }
        }
        None => true,
    };

    TextVerificationStatus {
        target_present,
        post_present,
        pre_post_present,
        target_text_matches_requested,
        ui_changed_from_pre_text: Some(ui_changed_from_pre_text),
        satisfied: target_ok && post_ok,
    }
}

fn relevant_selector_snapshot(
    nodes: &[NormalizedUiNode],
    tapped_selector: &UiSelector,
    wait_for_tracker: Option<&InternalNodeTracker>,
    wait_for_selector: Option<&UiSelector>,
) -> Vec<NormalizedUiNode> {
    nodes
        .iter()
        .filter(|node| {
            selector_matches(node, tapped_selector)
                || wait_for_tracker
                    .map(|tracker| tracker_matches_node(node, tracker))
                    .unwrap_or(false)
                || wait_for_selector
                    .map(|selector| selector_matches(node, selector))
                    .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn relevant_text_snapshot(
    nodes: &[NormalizedUiNode],
    target_tracker: Option<&InternalNodeTracker>,
    target_selector: Option<&UiSelector>,
    wait_for_selector: Option<&UiSelector>,
) -> Vec<NormalizedUiNode> {
    nodes
        .iter()
        .filter(|node| {
            target_tracker
                .map(|tracker| tracker_matches_node(node, tracker))
                .or_else(|| target_selector.map(|selector| selector_matches(node, selector)))
                .unwrap_or(false)
                || wait_for_selector
                    .map(|selector| selector_matches(node, selector))
                    .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn semantic_relevant_selector_snapshot(
    nodes: &[NormalizedUiNode],
    tapped_selector: &UiSelector,
    wait_for_tracker: Option<&InternalNodeTracker>,
    wait_for_selector: Option<&UiSelector>,
) -> Vec<SemanticUiNodeFingerprint> {
    relevant_selector_snapshot(nodes, tapped_selector, wait_for_tracker, wait_for_selector)
        .into_iter()
        .map(|node| SemanticUiNodeFingerprint {
            class_name: node.class_name,
            package_name: node.package_name,
            text: node.text,
            semantic_label: node.semantic_label,
            content_desc: node.content_desc,
            resource_id: node.resource_id,
            clickable: node.clickable,
            focusable: node.focusable,
            enabled: node.enabled,
            selected: node.selected,
            checked: node.checked,
            focused: node.focused,
            scrollable: node.scrollable,
            long_clickable: node.long_clickable,
        })
        .collect()
}

fn semantic_relevant_text_snapshot(
    nodes: &[NormalizedUiNode],
    target_tracker: Option<&InternalNodeTracker>,
    target_selector: Option<&UiSelector>,
    wait_for_selector: Option<&UiSelector>,
) -> Vec<SemanticUiNodeFingerprint> {
    relevant_text_snapshot(nodes, target_tracker, target_selector, wait_for_selector)
        .into_iter()
        .map(|node| SemanticUiNodeFingerprint {
            class_name: node.class_name,
            package_name: node.package_name,
            text: node.text,
            semantic_label: node.semantic_label,
            content_desc: node.content_desc,
            resource_id: node.resource_id,
            clickable: node.clickable,
            focusable: node.focusable,
            enabled: node.enabled,
            selected: node.selected,
            checked: node.checked,
            focused: node.focused,
            scrollable: node.scrollable,
            long_clickable: node.long_clickable,
        })
        .collect()
}

fn infer_unique_package_name(nodes: &[NormalizedUiNode]) -> Option<String> {
    let mut packages = nodes
        .iter()
        .filter_map(|node| node.package_name.as_ref())
        .map(|package_name| package_name.trim())
        .filter(|package_name| !package_name.is_empty())
        .collect::<Vec<_>>();
    packages.sort_unstable();
    packages.dedup();
    (packages.len() == 1).then(|| packages[0].to_string())
}

pub(crate) fn tracker_matches_node(node: &NormalizedUiNode, tracker: &InternalNodeTracker) -> bool {
    match tracker {
        InternalNodeTracker::Selector(selector) => selector_matches(node, selector),
        InternalNodeTracker::Identity {
            resource_id,
            class_name,
            bounds,
            focused,
        } => {
            if let (Some(expected_id), Some(node_id)) =
                (resource_id.as_deref(), node.resource_id.as_deref())
                && expected_id == node_id
            {
                let class_matches = class_name
                    .as_ref()
                    .map(|expected_class| node.class_name.as_ref() == Some(expected_class))
                    .unwrap_or(true);
                let focus_matches = focused
                    .map(|expected_focus| node.focused == expected_focus)
                    .unwrap_or(true);
                return class_matches && focus_matches;
            }

            node.bounds
                .map(|node_bounds| bounds_look_like_same_control(bounds, &node_bounds))
                .unwrap_or(false)
                && class_name
                    .as_ref()
                    .map(|expected_class| node.class_name.as_ref() == Some(expected_class))
                    .unwrap_or(true)
                && focused
                    .map(|expected_focus| node.focused == expected_focus)
                    .unwrap_or(true)
        }
    }
}

pub(crate) fn tap_verification_is_confirmed(
    status: &TapVerificationStatus,
    satisfied_polls_observed: u32,
    required_satisfied_polls: u32,
    final_poll: bool,
) -> bool {
    status.satisfied && (satisfied_polls_observed >= required_satisfied_polls || final_poll)
}

pub(crate) fn should_refresh_semantic_observation(
    last_fast_fingerprint: Option<&str>,
    new_fast_fingerprint: Option<&str>,
    force_followup_semantic_capture: bool,
    final_poll: bool,
) -> bool {
    final_poll
        || force_followup_semantic_capture
        || new_fast_fingerprint.is_none()
        || last_fast_fingerprint.is_none()
        || last_fast_fingerprint != new_fast_fingerprint
}

pub(crate) fn should_refresh_tool_postcondition_observation(
    last_fast_fingerprint: Option<&str>,
    new_fast_fingerprint: Option<&str>,
    selector_only_fast_path: bool,
    final_poll: bool,
) -> bool {
    !selector_only_fast_path
        || final_poll
        || new_fast_fingerprint.is_none()
        || last_fast_fingerprint.is_none()
        || last_fast_fingerprint != new_fast_fingerprint
}

pub(crate) fn text_verification_is_confirmed(
    status: &TextVerificationStatus,
    satisfied_polls_observed: u32,
    required_satisfied_polls: u32,
    final_poll: bool,
) -> bool {
    status.satisfied
        && (status.target_text_matches_requested == Some(true)
            || satisfied_polls_observed >= required_satisfied_polls
            || final_poll)
}

pub(crate) fn derive_observed_package(window_state: &AndroidWindowState) -> Option<String> {
    [
        window_state.focused_app.as_deref(),
        window_state.resumed_activity.as_deref(),
        window_state.current_focus.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(component_package_name)
}

fn window_state_postcondition_matches(
    window_state: &AndroidWindowState,
    wait_for_activity: Option<&str>,
    wait_for_package: Option<&str>,
) -> (Option<String>, Option<String>, bool) {
    let observed_activity = window_state.resumed_activity.clone();
    let observed_package = derive_observed_package(window_state);
    let activity_ok = wait_for_activity
        .map(|wanted| {
            observed_activity
                .as_deref()
                .map(|candidate| activity_matches(candidate, wanted))
                .unwrap_or(false)
        })
        .unwrap_or(true);
    let package_ok = wait_for_package
        .map(|wanted| {
            observed_package
                .as_deref()
                .map(|candidate| candidate.contains(wanted))
                .unwrap_or(false)
        })
        .unwrap_or(true);

    (observed_activity, observed_package, activity_ok && package_ok)
}

fn component_package_name(component: &str) -> Option<String> {
    let package = component.split('/').next()?.trim();
    (!package.is_empty()).then(|| package.to_string())
}

fn activity_matches(candidate: &str, wanted: &str) -> bool {
    candidate.contains(wanted)
        || component_activity_identity(candidate)
            .zip(component_activity_identity(wanted))
            .map(|(candidate_identity, wanted_identity)| candidate_identity == wanted_identity)
            .unwrap_or(false)
}

fn component_activity_identity(component: &str) -> Option<String> {
    let trimmed = component.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((package, activity)) = trimmed.split_once('/') {
        let package = package.trim();
        let activity = activity.trim();
        if package.is_empty() || activity.is_empty() {
            return None;
        }
        return Some(normalize_activity_name(package, activity));
    }

    Some(trimmed.to_string())
}

fn normalize_activity_name(package: &str, activity: &str) -> String {
    if activity.starts_with('.') {
        format!("{package}{activity}")
    } else if activity.contains('.') {
        activity.to_string()
    } else {
        format!("{package}.{activity}")
    }
}

fn bounds_intersect(left: &UiBounds, right: &UiBounds) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

fn bounds_look_like_same_control(original: &UiBounds, candidate: &UiBounds) -> bool {
    if !bounds_intersect(original, candidate) {
        return false;
    }

    let overlap_width = original.right.min(candidate.right) - original.left.max(candidate.left);
    let overlap_height = original.bottom.min(candidate.bottom) - original.top.max(candidate.top);
    let overlap_area = overlap_width * overlap_height;
    let original_area = (original.right - original.left) * (original.bottom - original.top);
    let candidate_area = (candidate.right - candidate.left) * (candidate.bottom - candidate.top);
    if overlap_area == 0 || original_area == 0 || candidate_area == 0 {
        return false;
    }

    let overlap_ratio = overlap_area as f32 / original_area.min(candidate_area) as f32;
    let candidate_center_x = (candidate.left + candidate.right) / 2;
    let candidate_center_y = (candidate.top + candidate.bottom) / 2;

    overlap_ratio >= 0.6
        && candidate_center_x >= original.left
        && candidate_center_x <= original.right
        && candidate_center_y >= original.top
        && candidate_center_y <= original.bottom
}

#[cfg(test)]
mod tests {
    use super::{
        ToolPostconditionEvidenceSource, ToolPostconditionResult, activity_matches,
        component_activity_identity, infer_unique_package_name, should_refresh_semantic_observation,
        should_refresh_tool_postcondition_observation, tool_postcondition_json,
        window_state_postcondition_matches,
    };
    use crate::tools::AndroidWindowState;
    use crate::ui::NormalizedUiNode;
    use serde_json::json;

    fn node_with_package(package_name: Option<&str>) -> NormalizedUiNode {
        NormalizedUiNode {
            class_name: None,
            package_name: package_name.map(ToOwned::to_owned),
            text: None,
            semantic_label: None,
            content_desc: None,
            resource_id: None,
            clickable: false,
            focusable: false,
            enabled: true,
            selected: false,
            checked: false,
            focused: false,
            scrollable: false,
            long_clickable: false,
            bounds: None,
            center: None,
        }
    }

    #[test]
    fn semantic_observation_refreshes_on_first_fast_poll() {
        assert!(should_refresh_semantic_observation(
            None,
            Some("fingerprint-a"),
            false,
            false
        ));
    }

    #[test]
    fn semantic_observation_skips_redundant_full_capture_when_fast_fingerprint_is_unchanged() {
        assert!(!should_refresh_semantic_observation(
            Some("fingerprint-a"),
            Some("fingerprint-a"),
            false,
            false
        ));
    }

    #[test]
    fn semantic_observation_forces_follow_up_capture_after_satisfied_semantic_poll() {
        assert!(should_refresh_semantic_observation(
            Some("fingerprint-a"),
            Some("fingerprint-a"),
            true,
            false
        ));
    }

    #[test]
    fn semantic_observation_refreshes_on_fast_fingerprint_change_or_final_poll() {
        assert!(should_refresh_semantic_observation(
            Some("fingerprint-a"),
            Some("fingerprint-b"),
            false,
            false
        ));
        assert!(should_refresh_semantic_observation(
            Some("fingerprint-a"),
            Some("fingerprint-a"),
            false,
            true
        ));
    }

    #[test]
    fn tool_postcondition_observation_skips_redundant_selector_only_semantic_capture() {
        assert!(!should_refresh_tool_postcondition_observation(
            Some("fingerprint-a"),
            Some("fingerprint-a"),
            true,
            false
        ));
        assert!(should_refresh_tool_postcondition_observation(
            Some("fingerprint-a"),
            Some("fingerprint-b"),
            true,
            false
        ));
    }

    #[test]
    fn tool_postcondition_observation_never_skips_non_selector_only_polls() {
        assert!(should_refresh_tool_postcondition_observation(
            Some("fingerprint-a"),
            Some("fingerprint-a"),
            false,
            false
        ));
    }

    #[test]
    fn text_verification_confirmation_accepts_exact_match_without_second_poll() {
        let status = super::TextVerificationStatus {
            target_present: Some(true),
            post_present: None,
            pre_post_present: None,
            target_text_matches_requested: Some(true),
            ui_changed_from_pre_text: Some(true),
            satisfied: true,
        };

        assert!(super::text_verification_is_confirmed(&status, 1, 2, false));
    }

    #[test]
    fn text_verification_confirmation_still_requires_extra_poll_without_exact_match() {
        let status = super::TextVerificationStatus {
            target_present: Some(true),
            post_present: None,
            pre_post_present: None,
            target_text_matches_requested: Some(false),
            ui_changed_from_pre_text: Some(true),
            satisfied: true,
        };

        assert!(!super::text_verification_is_confirmed(&status, 1, 2, false));
        assert!(super::text_verification_is_confirmed(&status, 2, 2, false));
    }

    #[test]
    fn infer_unique_package_name_returns_single_non_empty_package() {
        let nodes = vec![
            node_with_package(Some("com.example.app")),
            node_with_package(Some("com.example.app")),
            node_with_package(None),
        ];

        assert_eq!(
            infer_unique_package_name(&nodes),
            Some("com.example.app".to_string())
        );
    }

    #[test]
    fn infer_unique_package_name_returns_none_for_mixed_or_missing_packages() {
        let mixed = vec![
            node_with_package(Some("com.example.app")),
            node_with_package(Some("com.other.app")),
        ];
        let missing = vec![node_with_package(None), node_with_package(Some(" "))];

        assert_eq!(infer_unique_package_name(&mixed), None);
        assert_eq!(infer_unique_package_name(&missing), None);
    }

    #[test]
    fn component_activity_identity_normalizes_short_component_form() {
        assert_eq!(
            component_activity_identity("com.sednalabs.solarlab/.MainActivity"),
            Some("com.sednalabs.solarlab.MainActivity".to_string())
        );
        assert_eq!(
            component_activity_identity(
                "com.sednalabs.solarlab/com.sednalabs.solarlab.MainActivity"
            ),
            Some("com.sednalabs.solarlab.MainActivity".to_string())
        );
    }

    #[test]
    fn activity_match_accepts_equivalent_component_formats() {
        assert!(activity_matches(
            "com.sednalabs.solarlab/.MainActivity",
            "com.sednalabs.solarlab.MainActivity"
        ));
        assert!(activity_matches(
            "com.sednalabs.solarlab/com.sednalabs.solarlab.MainActivity",
            "com.sednalabs.solarlab.MainActivity"
        ));
        assert!(!activity_matches(
            "com.sednalabs.solarlab/.SearchActivity",
            "com.sednalabs.solarlab.MainActivity"
        ));
    }

    #[test]
    fn window_state_postcondition_accepts_matching_resumed_activity_and_package() {
        let window_state = AndroidWindowState {
            current_focus: Some("com.sednalabs.solarlab/.MainActivity".to_string()),
            focused_app: Some("com.sednalabs.solarlab/.MainActivity".to_string()),
            resumed_activity: Some("com.sednalabs.solarlab/.MainActivity".to_string()),
            input_method_visible: false,
            input_method_target: None,
        };

        let (activity, package, satisfied) = window_state_postcondition_matches(
            &window_state,
            Some(".MainActivity"),
            Some("com.sednalabs.solarlab"),
        );

        assert!(satisfied);
        assert_eq!(activity.as_deref(), Some("com.sednalabs.solarlab/.MainActivity"));
        assert_eq!(package.as_deref(), Some("com.sednalabs.solarlab"));
    }

    #[test]
    fn window_state_postcondition_rejects_a_wrong_resumed_package() {
        let window_state = AndroidWindowState {
            current_focus: Some("com.android.settings/.Settings".to_string()),
            focused_app: Some("com.android.settings/.Settings".to_string()),
            resumed_activity: Some("com.android.settings/.Settings".to_string()),
            input_method_visible: false,
            input_method_target: None,
        };

        let (_, package, satisfied) = window_state_postcondition_matches(
            &window_state,
            None,
            Some("com.sednalabs.solarlab"),
        );

        assert!(!satisfied);
        assert_eq!(package.as_deref(), Some("com.android.settings"));

        let missing_window_state = AndroidWindowState {
            current_focus: None,
            focused_app: None,
            resumed_activity: None,
            input_method_visible: false,
            input_method_target: None,
        };
        let (_, package, satisfied) = window_state_postcondition_matches(
            &missing_window_state,
            None,
            Some("com.sednalabs.solarlab"),
        );
        assert!(!satisfied);
        assert_eq!(package, None);
    }

    #[test]
    fn launch_postcondition_uses_hierarchy_only_for_selector_proof() {
        assert_eq!(
            ToolPostconditionEvidenceSource::for_launch(false),
            ToolPostconditionEvidenceSource::WindowState
        );
        assert_eq!(
            ToolPostconditionEvidenceSource::for_launch(true),
            ToolPostconditionEvidenceSource::UiHierarchy
        );
    }

    #[test]
    fn window_state_postcondition_serializes_readiness_without_ui_artifacts() {
        let result = ToolPostconditionResult {
            requested: true,
            satisfied: true,
            timed_out: false,
            elapsed_ms: 42,
            evidence_source: Some(ToolPostconditionEvidenceSource::WindowState),
            hierarchy_path: None,
            screenshot_path: None,
            observed_activity: Some("com.sednalabs.solarlab/.MainActivity".to_string()),
            observed_package: Some("com.sednalabs.solarlab".to_string()),
            node: None,
            match_count: 0,
            selected_match_index: None,
            candidate_summary: Vec::new(),
        };

        assert_eq!(
            tool_postcondition_json(&result),
            json!({
                "requested": true,
                "satisfied": true,
                "timed_out": false,
                "elapsed_ms": 42,
                "evidence_source": "window_state",
                "artifacts": {
                    "hierarchy_path": null,
                    "screenshot_path": null,
                },
                "observed_activity": "com.sednalabs.solarlab/.MainActivity",
                "observed_package": "com.sednalabs.solarlab",
                "match_count": 0,
                "selected_match_index": null,
                "candidate_summary": [],
                "node": null,
            })
        );
    }
}
