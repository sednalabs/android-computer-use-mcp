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
use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};

use crate::emulator_grpc::emulator_controller_client::EmulatorControllerClient;
use crate::emulator_grpc::{ImageFormat, KeyboardEvent, Touch, TouchEvent, image_format};

mod clipboard;
pub(crate) use clipboard::{
    ClipboardRestoreOutcome, ClipboardTransfer, begin_clipboard_transfer, requires_clipboard_text,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrpcMutationError {
    /// Cancellation stopped this gesture before any input RPC was submitted.
    CancelledBeforeDispatch,
    /// A read-only preflight failed before any input RPC was submitted.
    BeforeDispatch(String),
    /// An input RPC was submitted; its effects must not be replayed.
    OutcomeUnknown(String),
}

impl std::fmt::Display for GrpcMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CancelledBeforeDispatch => {
                formatter.write_str("Android request cancelled before gRPC input dispatch")
            }
            Self::BeforeDispatch(detail) => write!(
                formatter,
                "gRPC preflight failed before input dispatch: {detail}"
            ),
            Self::OutcomeUnknown(detail) => {
                write!(formatter, "gRPC input outcome unknown: {detail}")
            }
        }
    }
}

async fn mutation_client(
    port: u16,
    auth_token: Option<&str>,
) -> Result<EmulatorControllerClient<Channel>, GrpcMutationError> {
    ensure_mutation_not_cancelled()?;
    let mut client = grpc_client(port)
        .await
        .map_err(GrpcMutationError::BeforeDispatch)?;
    let request = authorized_request((), auth_token).map_err(GrpcMutationError::BeforeDispatch)?;
    client
        .get_status(request)
        .await
        .map_err(|error| GrpcMutationError::BeforeDispatch(grpc_status(error)))?;
    ensure_mutation_not_cancelled()?;
    Ok(client)
}

fn ensure_mutation_not_cancelled() -> Result<(), GrpcMutationError> {
    if crate::execution::request_cancelled() {
        Err(GrpcMutationError::CancelledBeforeDispatch)
    } else {
        Ok(())
    }
}

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

pub async fn send_tap(
    port: u16,
    auth_token: Option<&str>,
    x: u32,
    y: u32,
) -> Result<(), GrpcMutationError> {
    let mut client = mutation_client(port, auth_token).await?;
    send_tap_with_client(&mut client, auth_token, x, y)
        .await
        .map_err(GrpcMutationError::OutcomeUnknown)
}

pub async fn send_double_tap(
    port: u16,
    auth_token: Option<&str>,
    x: u32,
    y: u32,
) -> Result<(), GrpcMutationError> {
    let mut client = mutation_client(port, auth_token).await?;
    send_tap_with_client(&mut client, auth_token, x, y)
        .await
        .map_err(GrpcMutationError::OutcomeUnknown)?;
    sleep(Duration::from_millis(DOUBLE_TAP_INTER_TAP_INTERVAL_MS)).await;
    send_tap_with_client(&mut client, auth_token, x, y)
        .await
        .map_err(GrpcMutationError::OutcomeUnknown)
}

async fn send_tap_with_client(
    client: &mut EmulatorControllerClient<Channel>,
    auth_token: Option<&str>,
    x: u32,
    y: u32,
) -> Result<(), String> {
    let frames = vec![
        vec![touch_with_pressure(x, y, TOUCH_PRESSURE_DOWN)],
        vec![touch_with_pressure(x, y, TOUCH_PRESSURE_UP)],
    ];
    send_touch_sequence(
        &mut GrpcTouchSender { client, auth_token },
        &frames,
        Duration::ZERO,
    )
    .await
}

/// Submit low-level emulator keyboard translation and await its RPC acknowledgement.
///
/// Success confirms transport completion only; the emulator can acknowledge this
/// request without delivering text to a focused field. MCP text tools therefore
/// preselect ADB for printable ASCII and verified clipboard paste for other text.
pub async fn send_text(
    port: u16,
    auth_token: Option<&str>,
    text: &str,
) -> Result<(), GrpcMutationError> {
    ensure_mutation_not_cancelled()?;
    if requires_clipboard_text(text) {
        return Err(GrpcMutationError::BeforeDispatch(
            "this text requires a verified clipboard transfer; emulator keyboard translation is lossy"
                .to_string(),
        ));
    }
    let Some(payload) = keyboard_text_payload(text) else {
        return Ok(());
    };

    let mut client = mutation_client(port, auth_token).await?;
    client
        .send_key(
            authorized_request(
                KeyboardEvent {
                    text: payload,
                    ..Default::default()
                },
                auth_token,
            )
            .map_err(GrpcMutationError::BeforeDispatch)?,
        )
        .await
        .map_err(|error| GrpcMutationError::OutcomeUnknown(grpc_status(error)))?;
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
) -> Result<(), GrpcMutationError> {
    if !(1..=10_000).contains(&duration_ms) {
        return Err(GrpcMutationError::BeforeDispatch(
            "swipe duration must be between 1 and 10000 ms".to_string(),
        ));
    }
    let mut client = mutation_client(port, auth_token).await?;
    let steps = swipe_step_count(duration_ms);
    let mut frames = (0..=steps)
        .map(|step| {
            vec![touch_with_pressure(
                lerp_u32(x1, x2, step, steps),
                lerp_u32(y1, y2, step, steps),
                TOUCH_PRESSURE_DOWN,
            )]
        })
        .collect::<Vec<_>>();
    frames.push(vec![touch_with_pressure(x2, y2, TOUCH_PRESSURE_UP)]);
    send_touch_sequence(
        &mut GrpcTouchSender {
            client: &mut client,
            auth_token,
        },
        &frames,
        Duration::from_millis(step_interval(duration_ms, steps)),
    )
    .await
    .map_err(GrpcMutationError::OutcomeUnknown)
}

pub async fn send_multi_touch(
    port: u16,
    auth_token: Option<&str>,
    paths: &[MultiTouchPath],
    duration_ms: u64,
) -> Result<(), GrpcMutationError> {
    if !(2..=5).contains(&paths.len()) || !(50..=2_000).contains(&duration_ms) {
        return Err(GrpcMutationError::BeforeDispatch(
            "multi-touch requires two to five pointers and duration between 50 and 2000 ms"
                .to_string(),
        ));
    }
    let mut client = mutation_client(port, auth_token).await?;
    let frames = multi_touch_frames(paths, duration_ms);
    let step_interval = Duration::from_millis(multi_touch_step_interval(
        duration_ms,
        frames.len().saturating_sub(1) as u32,
    ));
    send_touch_sequence(
        &mut GrpcTouchSender {
            client: &mut client,
            auth_token,
        },
        &frames,
        step_interval,
    )
    .await
    .map_err(GrpcMutationError::OutcomeUnknown)
}

trait TouchSender: Send {
    fn send(&mut self, touches: Vec<Touch>) -> impl Future<Output = Result<(), String>> + Send;
}

struct GrpcTouchSender<'a> {
    client: &'a mut EmulatorControllerClient<Channel>,
    auth_token: Option<&'a str>,
}

impl TouchSender for GrpcTouchSender<'_> {
    async fn send(&mut self, touches: Vec<Touch>) -> Result<(), String> {
        send_touch_event(self.client, touches, self.auth_token).await
    }
}

async fn send_touch_sequence(
    sender: &mut impl TouchSender,
    frames: &[Vec<Touch>],
    step_interval: Duration,
) -> Result<(), String> {
    let intervals = frames.len().saturating_sub(2).min(625) as u32;
    let movement_budget = step_interval
        .saturating_mul(intervals)
        .min(Duration::from_secs(10))
        + GRPC_REQUEST_TIMEOUT;
    send_touch_sequence_with_budgets(
        sender,
        frames,
        step_interval,
        movement_budget,
        GRPC_REQUEST_TIMEOUT,
    )
    .await
}

async fn send_touch_sequence_with_budgets(
    sender: &mut impl TouchSender,
    frames: &[Vec<Touch>],
    step_interval: Duration,
    movement_budget: Duration,
    release_budget: Duration,
) -> Result<(), String> {
    let Some((release_frame, movement_frames)) = frames.split_last() else {
        return Err("touch sequence must contain a release frame".to_string());
    };
    let deadline = tokio::time::Instant::now() + movement_budget;
    let mut primary_error = None;
    for (frame_index, touches) in movement_frames.iter().enumerate() {
        if tokio::time::Instant::now() >= deadline {
            primary_error =
                Some("gesture movement deadline exceeded; input outcome unknown".to_string());
            break;
        }
        let result = tokio::time::timeout_at(deadline, sender.send(touches.clone())).await;
        match result {
            Ok(Ok(())) => (),
            Ok(Err(error)) => {
                primary_error = Some(error);
                break;
            }
            Err(_) => {
                primary_error =
                    Some("gesture movement deadline exceeded; input outcome unknown".to_string());
                break;
            }
        }
        if frame_index + 1 < movement_frames.len() && !step_interval.is_zero() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            sleep(step_interval.min(remaining)).await;
        }
    }
    // Release is outside the movement deadline. The device lease remains held
    // while this bounded attempt completes, including after an RPC timed out.
    let release_result = tokio::time::timeout(release_budget, sender.send(release_frame.clone()))
        .await
        .unwrap_or_else(|_| {
            Err("touch release deadline exceeded; pointer state unknown".to_string())
        });
    match (primary_error, release_result) {
        (None, Ok(())) => Ok(()),
        (Some(primary), Ok(())) => Err(primary),
        (None, Err(release)) => Err(format!("touch release failed: {release}")),
        (Some(primary), Err(release)) => {
            Err(format!("{primary}; touch release also failed: {release}"))
        }
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
    if text.is_empty() {
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
    let steps =
        duration_ms.saturating_add(MULTI_TOUCH_STEP_INTERVAL_MS - 1) / MULTI_TOUCH_STEP_INTERVAL_MS;
    steps.clamp(MULTI_TOUCH_MIN_STEPS as u64, MULTI_TOUCH_MAX_STEPS as u64) as u32
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
                touch_with_identifier(path.x2, path.y2, identifier as i32, TOUCH_PRESSURE_UP)
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

    #[derive(Default)]
    struct RecordingTouchSender {
        calls: Vec<Vec<Touch>>,
        failed_calls: Vec<usize>,
    }

    impl TouchSender for RecordingTouchSender {
        async fn send(&mut self, touches: Vec<Touch>) -> Result<(), String> {
            let index = self.calls.len();
            self.calls.push(touches);
            if self.failed_calls.contains(&index) {
                Err(format!("lost acknowledgement at frame {index}"))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn cancellation_rejects_gestures_before_connecting_or_dispatching() {
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        crate::execution::with_cancellation(token, async {
            assert_eq!(
                send_tap(0, None, 10, 20).await,
                Err(GrpcMutationError::CancelledBeforeDispatch)
            );
            assert_eq!(
                send_double_tap(0, None, 10, 20).await,
                Err(GrpcMutationError::CancelledBeforeDispatch)
            );
            assert_eq!(
                send_text(0, None, "fixture").await,
                Err(GrpcMutationError::CancelledBeforeDispatch)
            );
            assert_eq!(
                send_swipe(0, None, 0, 0, 10, 20, 100).await,
                Err(GrpcMutationError::CancelledBeforeDispatch)
            );
            let paths = [MultiTouchPath {
                x1: 0,
                y1: 0,
                x2: 10,
                y2: 20,
            }; 2];
            assert_eq!(
                send_multi_touch(0, None, &paths, 100).await,
                Err(GrpcMutationError::CancelledBeforeDispatch)
            );
        })
        .await;
    }

    struct CancellingTouchSender {
        token: tokio_util::sync::CancellationToken,
        calls: Vec<Vec<Touch>>,
    }

    impl TouchSender for CancellingTouchSender {
        async fn send(&mut self, touches: Vec<Touch>) -> Result<(), String> {
            self.calls.push(touches);
            self.token.cancel();
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancellation_during_a_gesture_preserves_movement_and_release() {
        let token = tokio_util::sync::CancellationToken::new();
        let frames = vec![
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(30, 40, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(30, 40, TOUCH_PRESSURE_UP)],
        ];
        let mut sender = CancellingTouchSender {
            token: token.clone(),
            calls: Vec::new(),
        };
        crate::execution::with_cancellation(token, async {
            send_touch_sequence(&mut sender, &frames, Duration::ZERO)
                .await
                .unwrap();
            assert!(crate::execution::request_cancelled());
            assert_eq!(
                sender.calls, frames,
                "in-flight gesture and release must complete"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn failed_gesture_stops_movement_and_releases_every_pointer_without_replay() {
        let paths = [
            MultiTouchPath {
                x1: 0,
                y1: 0,
                x2: 100,
                y2: 100,
            },
            MultiTouchPath {
                x1: 100,
                y1: 100,
                x2: 0,
                y2: 0,
            },
        ];
        let frames = multi_touch_frames(&paths, 64);
        let mut sender = RecordingTouchSender {
            failed_calls: vec![1],
            ..Default::default()
        };
        let error = send_touch_sequence(&mut sender, &frames, Duration::ZERO)
            .await
            .unwrap_err();
        assert!(error.contains("frame 1"));
        assert_eq!(
            sender.calls.len(),
            3,
            "two movement attempts and one release, no restarted gesture"
        );
        assert_eq!(sender.calls[2], *frames.last().unwrap());
        assert!(
            sender.calls[2]
                .iter()
                .all(|touch| touch.pressure == TOUCH_PRESSURE_UP)
        );
    }

    #[tokio::test]
    async fn failed_touch_down_still_attempts_release_and_preserves_both_errors() {
        let frames = vec![
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_UP)],
        ];
        let mut sender = RecordingTouchSender {
            failed_calls: vec![0, 1],
            ..Default::default()
        };
        let error = send_touch_sequence(&mut sender, &frames, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(sender.calls.len(), 2);
        assert!(error.contains("frame 0"));
        assert!(error.contains("release also failed"));
        assert_eq!(sender.calls[1][0].pressure, TOUCH_PRESSURE_UP);
    }

    #[derive(Default)]
    struct SlowMovementSender {
        calls: Vec<Vec<Touch>>,
        stall_release: bool,
        release_completed: bool,
    }

    impl TouchSender for SlowMovementSender {
        async fn send(&mut self, touches: Vec<Touch>) -> Result<(), String> {
            let is_movement = touches
                .iter()
                .any(|touch| touch.pressure != TOUCH_PRESSURE_UP);
            self.calls.push(touches);
            if is_movement || self.stall_release {
                sleep(Duration::from_secs(60)).await;
            } else {
                sleep(Duration::from_millis(5)).await;
                self.release_completed = true;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn movement_timeout_still_awaits_release_before_returning() {
        let frames = vec![
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(30, 40, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(30, 40, TOUCH_PRESSURE_UP)],
        ];
        let mut sender = SlowMovementSender::default();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send_touch_sequence_with_budgets(
                &mut sender,
                &frames,
                Duration::ZERO,
                Duration::from_millis(5),
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("internal movement/release deadlines must settle promptly");
        assert!(result.unwrap_err().contains("movement deadline"));
        assert_eq!(
            sender.calls.len(),
            2,
            "one movement attempt followed by release, never the next movement"
        );
        assert_eq!(sender.calls[1][0].pressure, TOUCH_PRESSURE_UP);
        assert!(
            sender.release_completed,
            "release must finish before returning"
        );
    }

    #[tokio::test]
    async fn stalled_release_is_bounded_and_keeps_the_movement_failure() {
        let frames = vec![
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_DOWN)],
            vec![touch_with_pressure(10, 20, TOUCH_PRESSURE_UP)],
        ];
        let mut sender = SlowMovementSender {
            stall_release: true,
            ..Default::default()
        };
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send_touch_sequence_with_budgets(
                &mut sender,
                &frames,
                Duration::ZERO,
                Duration::from_millis(5),
                Duration::from_millis(5),
            ),
        )
        .await
        .expect("release must have its own bounded deadline");
        let error = result.unwrap_err();
        assert!(error.contains("movement deadline"));
        assert!(error.contains("release deadline"));
        assert_eq!(sender.calls.len(), 2, "never replay timed-out input");
        assert_eq!(sender.calls[1][0].pressure, TOUCH_PRESSURE_UP);
        assert!(!sender.release_completed);
    }

    #[test]
    fn keyboard_text_payload_keeps_intentional_spaces() {
        assert_eq!(
            keyboard_text_payload("  hello  "),
            Some("  hello  ".to_string())
        );
        assert_eq!(keyboard_text_payload("   "), Some("   ".to_string()));
    }

    #[test]
    fn swipe_step_count_respects_minimum() {
        assert_eq!(swipe_step_count(0), SWIPE_MIN_STEPS);
        assert_eq!(swipe_step_count(32), SWIPE_MIN_STEPS);
        assert_eq!(swipe_step_count(160), 10);
    }

    #[test]
    fn double_tap_interval_is_deliberate_and_bounded() {
        const {
            assert!(DOUBLE_TAP_INTER_TAP_INTERVAL_MS >= 40);
            assert!(DOUBLE_TAP_INTER_TAP_INTERVAL_MS <= 150);
        }
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
