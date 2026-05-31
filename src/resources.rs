//! Discovery resources for android-computer-use-mcp.
//!
//! ## Rationale
//! Exposes lightweight read-only discovery material so a cold-start agent can
//! understand the harness without repo spelunking.

use std::collections::BTreeMap;

use mcp_toolkit_core::rmcp_models;
use rmcp::model::{Annotated, RawResource, ReadResourceResult, Resource, ResourceContents};
use serde_json::json;

use crate::tool_surface::{ToolSurfaceEntry, tool_surface_entries};

const ABOUT_URI: &str = "android-computer-use-mcp://about";
const HELP_URI: &str = "android-computer-use-mcp://help";
const TOOL_CATALOG_URI: &str = "android-computer-use-mcp://tool-catalog";
const SCENARIO_CATALOG_URI: &str = "android-computer-use-mcp://scenario-catalog";

const MIME_MARKDOWN: &str = "text/markdown";
const MIME_JSON: &str = "application/json";

pub fn list_resources() -> Vec<Resource> {
    let about_text = build_about_text();
    let help_text = build_help_text();
    let tool_catalog = build_tool_catalog();
    let scenario_catalog = build_scenario_catalog();

    vec![
        resource_for_text(
            ABOUT_URI,
            "about",
            "Android Computer Use MCP",
            "Purpose, scope, and current harness posture.",
            MIME_MARKDOWN,
            Some(about_text.len()),
        ),
        resource_for_text(
            HELP_URI,
            "help",
            "Android Computer Use MCP help",
            "Recommended interaction loops and lane guidance.",
            MIME_MARKDOWN,
            Some(help_text.len()),
        ),
        resource_for_text(
            TOOL_CATALOG_URI,
            "tool-catalog",
            "Android tool catalog",
            "Structured inventory of public Android and Solar Lab tools.",
            MIME_JSON,
            Some(tool_catalog.len()),
        ),
        resource_for_text(
            SCENARIO_CATALOG_URI,
            "scenario-catalog",
            "Solar Lab scenario catalog",
            "Structured Stage First scenario entry points and notes.",
            MIME_JSON,
            Some(scenario_catalog.len()),
        ),
    ]
}

pub fn read_resource(uri: &str) -> Result<ReadResourceResult, rmcp::ErrorData> {
    let (mime_type, text) = match uri {
        ABOUT_URI => (MIME_MARKDOWN, build_about_text()),
        HELP_URI => (MIME_MARKDOWN, build_help_text()),
        TOOL_CATALOG_URI => (MIME_JSON, build_tool_catalog()),
        SCENARIO_CATALOG_URI => (MIME_JSON, build_scenario_catalog()),
        _ => {
            return Err(rmcp::ErrorData::resource_not_found(
                "resource not found",
                None,
            ));
        }
    };

    Ok(rmcp_models::read_resource_result(vec![
        ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some(mime_type.to_string()),
            text,
            meta: None,
        },
    ]))
}

fn build_about_text() -> String {
    [
        "# Android Computer Use MCP",
        "",
        "Purpose:",
        "- Provide a structured Android execution substrate for emulator lifecycle, app control, observation, semantic UI interaction, raw input fallback, and Solar Lab scenarios.",
        "",
        "Current posture:",
        "- Hosted interactive-session tools can reuse a live runner-backed emulator without rebuilding or rebooting it for each new APK.",
        "- Semantic Android tools are the preferred interaction lane.",
        "- Raw input tools remain available as fallback when semantic selection is insufficient.",
        "- Solar Lab scenarios are first-class tools rather than private hand-assembled scripts.",
        "",
        "Contract:",
        "- Public tool exposure is governed by explicit tool inventory registration.",
        "- Discovery resources are read-only and intended for cold-start orientation.",
        "- Tool schema and resource catalog snapshots guard the public contract.",
        "",
    ]
    .join("\n")
}

fn build_help_text() -> String {
    [
        "# Android Computer Use MCP help",
        "",
        "Recommended interaction order:",
        "1. interactive_session.get_status when working against a hosted runner-backed session.",
        "2. android.health or android.list_devices to understand the current harness state.",
        "3. android.wait_for_boot if a device may still be starting.",
        "4. interactive_session.install_build_from_run to swap a fresh remote-built APK into a live hosted session.",
        "5. android.launch_app or a Solar Lab scenario to reach the target surface.",
        "6. android.wait_for_stable_ui before acting through semantic UI tools.",
        "7. android.tap_element or android.type_into_element for semantic interaction.",
        "8. android.input.* only when semantic tools are insufficient.",
        "",
        "Guidance:",
        "- Prefer semantic UI tools over raw input whenever a selector can describe the target.",
        "- Prefer scenario tools for durable Solar Lab flows instead of rebuilding them ad hoc.",
        "- Use observation tools when you need artifacts or structured state, not just one action.",
        "",
        "Hosted usage notes:",
        "- This server works well as a runner-backed Android substrate over Streamable HTTP.",
        "- interactive_session.* tools exist to avoid restarting the hosted runner for each new build.",
        "- Discovery resources exist so a fresh agent can orient without local repo context.",
        "",
    ]
    .join("\n")
}

fn build_tool_catalog() -> String {
    let entries = tool_surface_entries();
    let mut groups = BTreeMap::<&str, usize>::new();
    for entry in entries {
        *groups.entry(entry.group).or_default() += 1;
    }

    let payload = json!({
        "server": "android-computer-use-mcp",
        "preferred_lane": "semantic-ui-first",
        "fallback_lane": "raw-input",
        "groups": groups,
        "tools": entries.iter().map(entry_json).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn build_scenario_catalog() -> String {
    let scenarios = tool_surface_entries()
        .iter()
        .filter(|entry| entry.group == "solarlab-scenarios")
        .map(entry_json)
        .collect::<Vec<_>>();
    let payload = json!({
        "domain": "solarlab",
        "notes": [
            "Scenario tools are the preferred entry points for durable Solar Lab validation flows.",
            "Use solarlab.semantic_action for bounded domain actions when a full scenario is too broad."
        ],
        "scenarios": scenarios,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn entry_json(entry: &ToolSurfaceEntry) -> serde_json::Value {
    json!({
        "name": entry.name,
        "group": entry.group,
        "read_only": entry.read_only,
        "description": entry.description,
        "use_when": entry.use_when,
    })
}

fn resource_for_text(
    uri: &str,
    name: &str,
    title: &str,
    description: &str,
    mime_type: &str,
    size: Option<usize>,
) -> Resource {
    Annotated::new(
        RawResource {
            uri: uri.to_string(),
            name: name.to_string(),
            title: Some(title.to_string()),
            description: Some(description.to_string()),
            mime_type: Some(mime_type.to_string()),
            size: size.map(|value| value.min(u32::MAX as usize) as u32),
            icons: None,
            meta: None,
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mcp_toolkit_testing::assert_json_contract_snapshot;
    use serde_json::json;

    use super::{
        ABOUT_URI, HELP_URI, SCENARIO_CATALOG_URI, TOOL_CATALOG_URI, list_resources, read_resource,
    };

    fn read_text(uri: &str) -> String {
        let resource = read_resource(uri).expect("resource should exist");
        match resource.contents.as_slice() {
            [rmcp::model::ResourceContents::TextResourceContents { text, .. }] => text.clone(),
            other => panic!("expected single text resource, got {other:?}"),
        }
    }

    #[test]
    fn resource_catalog_snapshot_is_stable() {
        let payload = json!({
            "resources": list_resources(),
            "reads": {
                "about": read_text(ABOUT_URI),
                "help": read_text(HELP_URI),
                "tool_catalog": serde_json::from_str::<serde_json::Value>(&read_text(TOOL_CATALOG_URI))
                    .expect("tool catalog should be valid json"),
                "scenario_catalog": serde_json::from_str::<serde_json::Value>(&read_text(SCENARIO_CATALOG_URI))
                    .expect("scenario catalog should be valid json"),
            },
        });
        let snapshot_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("spec/resource_catalog_snapshot.v1.json");
        assert_json_contract_snapshot(
            &snapshot_path,
            "android_computer_use_mcp_resource_catalog",
            1,
            &payload,
        );
    }
}
