//! Helpers for Android `cmd window dump-visible-window-views` payloads.
//!
//! ## Rationale
//! Provides utilities for capturing and hashing the visual state of the
//! emulator window, allowing for change detection across tool actions.
//!
//! ## Security Boundaries
//! * Artifacts are restricted to the configured output directory.
//! * State captures only include information available via the Android UI hierarchy.
//!
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};

use zip::ZipArchive;

pub(crate) fn normalized_visible_window_dump_fingerprint(payload: &[u8]) -> Result<String, String> {
    normalized_visible_window_dump_fingerprint_for_package(payload, None)
}

pub(crate) fn normalized_visible_window_dump_fingerprint_for_package(
    payload: &[u8],
    target_package: Option<&str>,
) -> Result<String, String> {
    let mut archive = ZipArchive::new(Cursor::new(payload))
        .map_err(|error| format!("failed to open visible-window dump zip: {error}"))?;
    let mut entry_names = archive.file_names().map(str::to_string).collect::<Vec<_>>();
    entry_names.sort();

    let mut hasher = DefaultHasher::new();
    let mut matched_target_package = false;
    for name in entry_names {
        let metadata = visible_window_entry_metadata(&name);
        let include_entry = target_package
            .map(|target_package| {
                let package_matches = metadata.package_name == Some(target_package);
                if package_matches {
                    matched_target_package = true;
                }
                package_matches || metadata.kind.is_shared_with_target_package()
            })
            .unwrap_or(true);
        if !include_entry {
            continue;
        }
        name.hash(&mut hasher);
        let mut entry = archive
            .by_name(&name)
            .map_err(|error| format!("failed to read visible-window entry `{name}`: {error}"))?;
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|error| format!("failed to decode visible-window entry `{name}`: {error}"))?;
        data.hash(&mut hasher);
    }

    if target_package.is_some() && !matched_target_package {
        return normalized_visible_window_dump_fingerprint_for_package(payload, None);
    }

    Ok(format!("{:016x}", hasher.finish()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibleWindowEntryKind {
    PackageWindow,
    InputMethod,
    PopupWindow,
    Other,
}

impl VisibleWindowEntryKind {
    fn is_shared_with_target_package(self) -> bool {
        matches!(self, Self::InputMethod | Self::PopupWindow)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisibleWindowEntryMetadata<'a> {
    package_name: Option<&'a str>,
    kind: VisibleWindowEntryKind,
}

fn visible_window_entry_metadata(entry_name: &str) -> VisibleWindowEntryMetadata<'_> {
    let descriptor = entry_name
        .split_once(' ')
        .and_then(|(prefix, remainder)| {
            (!prefix.is_empty()
                && prefix
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()))
            .then_some(remainder)
        })
        .unwrap_or(entry_name)
        .trim();
    if let Some((package_name, _activity)) = descriptor.split_once('/') {
        let package_name = package_name.trim();
        if !package_name.is_empty() {
            return VisibleWindowEntryMetadata {
                package_name: Some(package_name),
                kind: VisibleWindowEntryKind::PackageWindow,
            };
        }
    }
    let kind = match descriptor {
        "InputMethod" => VisibleWindowEntryKind::InputMethod,
        "Pop-Up Window" => VisibleWindowEntryKind::PopupWindow,
        _ => VisibleWindowEntryKind::Other,
    };
    VisibleWindowEntryMetadata {
        package_name: None,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        VisibleWindowEntryKind, normalized_visible_window_dump_fingerprint,
        normalized_visible_window_dump_fingerprint_for_package, visible_window_entry_metadata,
    };
    use std::io::{Cursor, Write};
    use zip::CompressionMethod;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn build_window_dump_zip(
        entries: &[(&str, &[u8])],
        reverse_order: bool,
        comment: Option<&str>,
    ) -> Vec<u8> {
        let buffer = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(buffer);
        if let Some(comment) = comment {
            writer.set_comment(comment);
        }
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let iter: Box<dyn Iterator<Item = &(&str, &[u8])>> = if reverse_order {
            Box::new(entries.iter().rev())
        } else {
            Box::new(entries.iter())
        };
        for (name, payload) in iter {
            writer
                .start_file(name.to_string(), options)
                .expect("zip entry should start");
            writer
                .write_all(payload)
                .expect("zip entry payload should write");
        }
        writer.finish().expect("zip should finish").into_inner()
    }

    #[test]
    fn normalized_visible_window_dump_fingerprint_ignores_zip_packaging_differences() {
        let first = build_window_dump_zip(
            &[("b-window", b"beta"), ("a-window", b"alpha")],
            false,
            Some("first-archive"),
        );
        let second = build_window_dump_zip(
            &[("a-window", b"alpha"), ("b-window", b"beta")],
            true,
            Some("second-archive"),
        );

        let first_fingerprint =
            normalized_visible_window_dump_fingerprint(&first).expect("first zip should parse");
        let second_fingerprint =
            normalized_visible_window_dump_fingerprint(&second).expect("second zip should parse");

        assert_eq!(first_fingerprint, second_fingerprint);
    }

    #[test]
    fn normalized_visible_window_dump_fingerprint_changes_when_entry_payload_changes() {
        let baseline =
            build_window_dump_zip(&[("main-window", b"alpha"), ("ime", b"beta")], false, None);
        let changed =
            build_window_dump_zip(&[("main-window", b"alpha"), ("ime", b"gamma")], false, None);

        let baseline_fingerprint = normalized_visible_window_dump_fingerprint(&baseline)
            .expect("baseline zip should parse");
        let changed_fingerprint =
            normalized_visible_window_dump_fingerprint(&changed).expect("changed zip should parse");

        assert_ne!(baseline_fingerprint, changed_fingerprint);
    }

    #[test]
    fn invalid_visible_window_dump_zip_is_rejected() {
        let error = normalized_visible_window_dump_fingerprint(b"not-a-zip")
            .expect_err("invalid payload should fail");
        assert!(error.contains("failed to open visible-window dump zip"));
    }

    #[test]
    fn visible_window_entry_metadata_extracts_package_and_shared_kinds() {
        let app_window =
            visible_window_entry_metadata("7c34837 com.sednalabs.solarlab/com.sednalabs.Main");
        assert_eq!(app_window.package_name, Some("com.sednalabs.solarlab"));
        assert_eq!(app_window.kind, VisibleWindowEntryKind::PackageWindow);

        let ime = visible_window_entry_metadata("eff6665 InputMethod");
        assert_eq!(ime.package_name, None);
        assert_eq!(ime.kind, VisibleWindowEntryKind::InputMethod);

        let popup = visible_window_entry_metadata("bd7ea44 Pop-Up Window");
        assert_eq!(popup.package_name, None);
        assert_eq!(popup.kind, VisibleWindowEntryKind::PopupWindow);
    }

    #[test]
    fn package_aware_visible_window_fingerprint_ignores_unrelated_windows() {
        let baseline = build_window_dump_zip(
            &[
                ("7c34837 com.example.app/.MainActivity", b"app"),
                ("eff6665 InputMethod", b"ime"),
                (
                    "a7851fb com.other.launcher/.LauncherActivity",
                    b"launcher-a",
                ),
                ("50d5895 StatusBar", b"status"),
            ],
            false,
            None,
        );
        let changed = build_window_dump_zip(
            &[
                ("7c34837 com.example.app/.MainActivity", b"app"),
                ("eff6665 InputMethod", b"ime"),
                (
                    "a7851fb com.other.launcher/.LauncherActivity",
                    b"launcher-b",
                ),
                ("50d5895 StatusBar", b"status-updated"),
            ],
            false,
            None,
        );

        let baseline_fingerprint = normalized_visible_window_dump_fingerprint_for_package(
            &baseline,
            Some("com.example.app"),
        )
        .expect("baseline zip should parse");
        let changed_fingerprint = normalized_visible_window_dump_fingerprint_for_package(
            &changed,
            Some("com.example.app"),
        )
        .expect("changed zip should parse");

        assert_eq!(baseline_fingerprint, changed_fingerprint);
    }

    #[test]
    fn package_aware_visible_window_fingerprint_keeps_shared_input_method_changes() {
        let baseline = build_window_dump_zip(
            &[
                ("7c34837 com.example.app/.MainActivity", b"app"),
                ("eff6665 InputMethod", b"ime-a"),
            ],
            false,
            None,
        );
        let changed = build_window_dump_zip(
            &[
                ("7c34837 com.example.app/.MainActivity", b"app"),
                ("eff6665 InputMethod", b"ime-b"),
            ],
            false,
            None,
        );

        let baseline_fingerprint = normalized_visible_window_dump_fingerprint_for_package(
            &baseline,
            Some("com.example.app"),
        )
        .expect("baseline zip should parse");
        let changed_fingerprint = normalized_visible_window_dump_fingerprint_for_package(
            &changed,
            Some("com.example.app"),
        )
        .expect("changed zip should parse");

        assert_ne!(baseline_fingerprint, changed_fingerprint);
    }

    #[test]
    fn package_aware_visible_window_fingerprint_falls_back_to_global_when_package_missing() {
        let payload = build_window_dump_zip(
            &[
                ("a7851fb com.other.launcher/.LauncherActivity", b"launcher"),
                ("50d5895 StatusBar", b"status"),
            ],
            false,
            None,
        );

        let global = normalized_visible_window_dump_fingerprint(&payload)
            .expect("global fingerprint should parse");
        let missing_package = normalized_visible_window_dump_fingerprint_for_package(
            &payload,
            Some("com.example.app"),
        )
        .expect("package-aware fingerprint should fall back");

        assert_eq!(global, missing_package);
    }
}
