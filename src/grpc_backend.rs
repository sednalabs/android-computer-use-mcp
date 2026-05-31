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
const SWIPE_STEP_INTERVAL_MS: u64 = 16;
const SWIPE_MIN_STEPS: u32 = 4;
const TOUCH_IDENTIFIER: i32 = 0;
const TOUCH_PRESSURE_DOWN: i32 = 1;
const TOUCH_PRESSURE_UP: i32 = 0;

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
    send_touch_event(
        &mut client,
        vec![touch_with_pressure(x, y, TOUCH_PRESSURE_DOWN)],
        auth_token,
    )
    .await?;
    send_touch_event(
        &mut client,
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
    Touch {
        x: saturating_u32_to_i32(x),
        y: saturating_u32_to_i32(y),
        identifier: TOUCH_IDENTIFIER,
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
    fn lerp_u32_interpolates_monotonically() {
        assert_eq!(lerp_u32(10, 20, 0, 4), 10);
        assert_eq!(lerp_u32(10, 20, 2, 4), 15);
        assert_eq!(lerp_u32(10, 20, 4, 4), 20);
    }
}
