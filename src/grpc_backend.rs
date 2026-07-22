//! Narrow emulator gRPC adapter used by the Android harness.
//!
//! ## Rationale
//! This module intentionally owns only the transport-specific pieces for
//! screenshot capture and a small set of input primitives. Tool orchestration
//! and fallback policy stay in `tools.rs`.
//!
//! ## Security Boundaries
//! * Limits emulator interaction to approved gRPC endpoints.
//! * Enforces communication over loopback only.
//!
use std::time::Duration;

use tokio::time::sleep;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};

use crate::emulator_grpc::emulator_controller_client::EmulatorControllerClient;
use crate::emulator_grpc::{ImageFormat, KeyboardEvent, Touch, TouchEvent, image_format};

const GRPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const GRPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DOUBLE_TAP_INTER_TAP_INTERVAL_MS: u64 = 80;
const SWIPE_STEP_INTERVAL_MS: u64 = 16;
const SWIPE_MIN_STEPS: u32 = 4;
const MULTI_TOUCH_STEP_INTERVAL_MS: u64 = 16;
const MULTI_TOUCH_MIN_STEPS: u32 = 4;
const MULTI_TOUCH_MAX_STEPS: u32 = 120;
const TOUCH_IDENTIFIER: i32 = 0;
const TOUCH_PRESSURE_DOWN: i32 = 1;
const TOUCH_PRESSURE_UP: i32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiTouchPath {
    pub x1: u32,
    pub y1: u32,
    pub x2: u32,
    pub y2: u32,
}

pub async fn capture_screenshot_png(
    port: u16,
    auth_token: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut client = grpc_client(port).await?;
    let image = client
        .get_screenshot(authorized_request(
            ImageFormat {
                format: image_format::ImgFormat::Png as i32,
                ..Default::default()
            },
            auth_token,
        )?)
        .await
        .map_err(grpc_status)?;
    Ok(image.into_inner().image)
}

pub async fn send_tap(port: u16, auth_token: Option<&str>, x: u32, y: u32) -> Result<(), String> {
    let mut client = grpc_client(port).await?;
    send_tap_with_client(&mut client, auth_token, x, y).await
}

pub async fn send_double_tap(
    port: u16,
    auth_token: Option<&str>,
    x: u32,
    y: u32,
) -> Result<(), String> {
    let mut client = grpc_client(port).await?;
    send_tap_with_client(&mut client, auth_token, x, y).await?;
    // Keep both taps on one channel and leave a deliberate, bounded gap so
    // Android gesture detectors receive two distinct taps inside one gesture.
    sleep(Duration::from_millis(DOUBLE_TAP_INTER_TAP_INTERVAL_MS)).await;
    send_tap_with_client(&mut client, auth_token, x, y).await
}

async fn send_tap_with_client(
    client: &mut EmulatorControllerClient<Channel>,
    auth_token: Option<&str>,
    x: u32,
    y: u32,
) -> Result<(), String> {
    send_touch_event(
        client,
        vec![touch_with_pressure(x, y, TOUCH_PRESSURE_DOWN)],
        auth_token,
    )
    .await?;
    send_touch_event(
        client,
        vec![touch_with_pressure(x, y, TOUCH_PRESSURE_UP)],
        auth_token,
    )
    .await?;
    Ok(())
}

pub async fn send_text(port: u16, auth_token: Option<&str>, text: &str) -> Result<(), String> {
    let Some(payload) = keyboard_text_payload(text) else {
        return Ok(());
    };

    let mut client = grpc_client(port).await?;
    client
        .send_key(authorized_request(
            KeyboardEvent {
                text: payload,
                ..Default::default()
            },
            auth_token,
        )?)
        .await
        .map_err(grpc_status)?;
    Ok(())
}

pub async fn send_swipe(
    port: u16,
    auth_token: Option<&str>,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    duration_ms: u64,
) -> Result<(), String> {
    let mut client = grpc_client(port).await?;
    let steps = swipe_step_count(duration_ms);

    send_touch_event(
        &mut client,
        vec![touch_with_pressure(x1, y1, TOUCH_PRESSURE_DOWN)],
        auth_token,
    )
    .await?;

    for step in 1..steps {
        let x = lerp_u32(x1, x2, step, steps);
        let y = lerp_u32(y1, y2, step, steps);
        send_touch_event(
            &mut client,
            vec![touch_with_pressure(x, y, TOUCH_PRESSURE_DOWN)],
            auth_token,
        )
        .await?;
        sleep(Duration::from_millis(step_interval(duration_ms, steps))).await;
    }

    send_touch_event(
        &mut client,
        vec![touch_with_pressure(x2, y2, TOUCH_PRESSURE_UP)],
        auth_token,
    )
    .await?;
    Ok(())
}

pub async fn send_multi_touch(
    port: u16,
    auth_token: Option<&str>,
    paths: &[MultiTouchPath],
    duration_ms: u64,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("multi-touch gesture requires at least one pointer".to_string());
    }
    let mut client = grpc_client(port).await?;
    let frames = multi_touch_frames(paths, duration_ms);
    let (release_frame, movement_frames) = frames
        .split_last()
        .expect("validated multi-touch paths always produce a release frame");
    let step_interval = Duration::from_millis(multi_touch_step_interval(
        duration_ms,
        movement_frames.len() as u32,
    ));

    let mut primary_error = None;
    for (frame_index, touches) in movement_frames.iter().enumerate() {
        if let Err(err) = send_touch_event(&mut client, touches.clone(), auth_token).await {
            primary_error = Some(err);
            break;
        }
        if frame_index + 1 < movement_frames.len() {
            sleep(step_interval).await;
        }
    }

    let release_result = send_touch_event(&mut client, release_frame.clone(), auth_token).await;
    match (primary_error, release_result) {
        (None, Ok(())) => Ok(()),
        (Some(primary), Ok(())) => Err(primary),
        (None, Err(release)) => Err(format!("multi-touch release failed: {release}")),
        (Some(primary), Err(release)) => Err(format!(
            "{primary}; multi-touch release also failed: {release}"
        )),
    }
}

async fn grpc_client(port: u16) -> Result<EmulatorControllerClient<Channel>, String> {
    let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .map_err(|err| format!("failed to build emulator gRPC endpoint for port {port}: {err}"))?
        .connect_timeout(GRPC_CONNECT_TIMEOUT)
        .timeout(GRPC_REQUEST_TIMEOUT);
    let channel = endpoint
        .connect()
        .await
        .map_err(|err| format!("failed to connect to emulator gRPC on port {port}: {err}"))?;
    Ok(EmulatorControllerClient::new(channel))
}

async fn send_touch_event(
    client: &mut EmulatorControllerClient<Channel>,
    touches: Vec<Touch>,
    auth_token: Option<&str>,
) -> Result<(), String> {
    client
        .send_touch(authorized_request(
            TouchEvent {
                touches,
                ..Default::default()
            },
            auth_token,
        )?)
        .await
        .map_err(grpc_status)?;
    Ok(())
}

fn authorized_request<T>(message: T, auth_token: Option<&str>) -> Result<Request<T>, String> {
    let mut request = Request::new(message);
    if let Some(token) = auth_token.map(str::trim).filter(|value| !value.is_empty()) {
        let value = MetadataValue::try_from(format!("Bearer {token}"))
            .map_err(|err| format!("failed to encode emulator gRPC authorization header: {err}"))?;
        request.metadata_mut().insert("authorization", value);
    }
    Ok(request)
}

fn grpc_status(status: tonic::Status) -> String {
    format!("emulator gRPC request failed: {status}")
}

fn keyboard_text_payload(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn touch_with_pressure(x: u32, y: u32, pressure: i32) -> Touch {
    touch_with_identifier(x, y, TOUCH_IDENTIFIER, pressure)
}

fn touch_with_identifier(x: u32, y: u32, identifier: i32, pressure: i32) -> Touch {
    Touch {
        x: saturating_u32_to_i32(x),
        y: saturating_u32_to_i32(y),
        identifier,
        pressure,
        ..Default::default()
    }
}

fn saturating_u32_to_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

fn swipe_step_count(duration_ms: u64) -> u32 {
    let steps = (duration_ms / SWIPE_STEP_INTERVAL_MS).max(SWIPE_MIN_STEPS as u64);
    steps.min(u32::MAX as u64) as u32
}

fn multi_touch_step_count(duration_ms: u64) -> u32 {
    let steps = duration_ms.saturating_add(MULTI_TOUCH_STEP_INTERVAL_MS - 1)
        / MULTI_TOUCH_STEP_INTERVAL_MS;
    steps.clamp(
        MULTI_TOUCH_MIN_STEPS as u64,
        MULTI_TOUCH_MAX_STEPS as u64,
    ) as u32
}

fn multi_touch_step_interval(duration_ms: u64, steps: u32) -> u64 {
    if steps <= 1 {
        return duration_ms.max(1);
    }
    (duration_ms / (steps - 1) as u64).max(1)
}

fn multi_touch_frames(paths: &[MultiTouchPath], duration_ms: u64) -> Vec<Vec<Touch>> {
    let steps = multi_touch_step_count(duration_ms);
    let last_step = steps.saturating_sub(1);
    let mut frames = Vec::with_capacity(steps as usize + 1);

    for step in 0..steps {
        frames.push(
            paths
                .iter()
                .enumerate()
                .map(|(identifier, path)| {
                    touch_with_identifier(
                        lerp_u32(path.x1, path.x2, step, last_step),
                        lerp_u32(path.y1, path.y2, step, last_step),
                        identifier as i32,
                        TOUCH_PRESSURE_DOWN,
                    )
                })
                .collect(),
        );
    }

    frames.push(
        paths
            .iter()
            .enumerate()
            .map(|(identifier, path)| {
                touch_with_identifier(
                    path.x2,
                    path.y2,
                    identifier as i32,
                    TOUCH_PRESSURE_UP,
                )
            })
            .collect(),
    );
    frames
}

fn step_interval(duration_ms: u64, steps: u32) -> u64 {
    if steps <= 1 {
        return duration_ms.max(1);
    }
    (duration_ms / steps as u64).max(1)
}

fn lerp_u32(start: u32, end: u32, step: u32, total_steps: u32) -> u32 {
    if total_steps == 0 {
        return end;
    }
    let start = start as i64;
    let end = end as i64;
    let delta = end - start;
    let value = start + (delta * step as i64) / total_steps as i64;
    value.clamp(0, u32::MAX as i64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_text_payload_keeps_intentional_spaces() {
        assert_eq!(
            keyboard_text_payload("  hello  "),
            Some("  hello  ".to_string())
        );
        assert_eq!(keyboard_text_payload("   "), None);
    }

    #[test]
    fn swipe_step_count_respects_minimum() {
        assert_eq!(swipe_step_count(0), SWIPE_MIN_STEPS);
        assert_eq!(swipe_step_count(32), SWIPE_MIN_STEPS);
        assert_eq!(swipe_step_count(160), 10);
    }

    #[test]
    fn double_tap_interval_is_deliberate_and_bounded() {
        assert!(DOUBLE_TAP_INTER_TAP_INTERVAL_MS >= 40);
        assert!(DOUBLE_TAP_INTER_TAP_INTERVAL_MS <= 150);
    }

    #[test]
    fn multi_touch_step_count_is_bounded() {
        assert_eq!(multi_touch_step_count(50), MULTI_TOUCH_MIN_STEPS);
        assert_eq!(multi_touch_step_count(300), 19);
        assert_eq!(multi_touch_step_count(2_000), MULTI_TOUCH_MAX_STEPS);
    }

    #[test]
    fn multi_touch_frames_keep_identifiers_and_release_every_pointer() {
        let paths = [
            MultiTouchPath {
                x1: 10,
                y1: 20,
                x2: 40,
                y2: 50,
            },
            MultiTouchPath {
                x1: 90,
                y1: 100,
                x2: 60,
                y2: 70,
            },
        ];
        let frames = multi_touch_frames(&paths, 64);

        assert_eq!(frames.len(), MULTI_TOUCH_MIN_STEPS as usize + 1);
        assert_eq!(frames[0].len(), paths.len());
        assert_eq!(frames[0][0].identifier, 0);
        assert_eq!(frames[0][1].identifier, 1);
        assert_eq!(frames[0][0].pressure, TOUCH_PRESSURE_DOWN);
        assert_eq!(frames[2][0].x, 30);
        assert_eq!(frames[2][1].x, 70);

        let release = frames.last().expect("release frame should exist");
        assert_eq!(release[0].x, 40);
        assert_eq!(release[0].y, 50);
        assert_eq!(release[0].pressure, TOUCH_PRESSURE_UP);
        assert_eq!(release[1].x, 60);
        assert_eq!(release[1].y, 70);
        assert_eq!(release[1].pressure, TOUCH_PRESSURE_UP);
    }

    #[test]
    fn lerp_u32_interpolates_monotonically() {
        assert_eq!(lerp_u32(10, 20, 0, 4), 10);
        assert_eq!(lerp_u32(10, 20, 2, 4), 15);
        assert_eq!(lerp_u32(10, 20, 4, 4), 20);
    }
}
