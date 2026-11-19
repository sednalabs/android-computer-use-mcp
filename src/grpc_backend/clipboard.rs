//! Private text-only clipboard transfers. No clipboard values enter diagnostics.
//!
//! The emulator acknowledges scheduling, not guest consumption. Callers must
//! dispatch paste once and await their UI verification before restoring. The
//! process-wide lease remains held through that verification and cleanup.
use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard};
use tokio::time::{sleep, timeout};
use tonic::transport::Channel;

use super::{
    GrpcMutationError, authorized_request, ensure_mutation_not_cancelled, mutation_client,
};
use crate::emulator_grpc::ClipData;
use crate::emulator_grpc::emulator_controller_client::EmulatorControllerClient;

const CLIPBOARD_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const CLIPBOARD_PHASE_TIMEOUT: Duration = Duration::from_secs(8);
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
static CLIPBOARD_TRANSFER_LOCK: Mutex<BTreeSet<u16>> = Mutex::const_new(BTreeSet::new());

pub(crate) fn requires_clipboard_text(text: &str) -> bool {
    !text.is_ascii() || text.chars().any(char::is_control) || text.contains("%s")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardRestoreOutcome {
    Restored,
    PreservedInterveningChange,
}

#[must_use = "await restore_if_owned after paste verification, including on errors"]
pub(crate) struct ClipboardTransfer {
    io: GrpcClipboard,
    state: ClipboardState,
    lease: ClipboardLease,
}

struct ClipboardLease {
    port: u16,
    blocked: MutexGuard<'static, BTreeSet<u16>>,
    armed: bool,
}

impl Drop for ClipboardLease {
    fn drop(&mut self) {
        if self.armed {
            // Normal request cancellation is cooperative, but an aborted task
            // during staging or verification must not admit another transfer.
            self.blocked.insert(self.port);
        }
    }
}

impl ClipboardTransfer {
    /// Cleanup deliberately ignores request cancellation. The clipboard API has
    /// no compare-and-set, so this protects observed intervening changes but
    /// cannot exclude a concurrent external write between read and restore.
    pub(crate) async fn restore_if_owned(
        &mut self,
    ) -> Result<ClipboardRestoreOutcome, GrpcMutationError> {
        let result = restore_if_owned(&mut self.io, &mut self.state, CLIPBOARD_PHASE_TIMEOUT).await;
        self.lease.armed = result.is_err();
        result
    }
}

pub(crate) async fn begin_clipboard_transfer(
    port: u16,
    auth_token: Option<&str>,
    text: &str,
) -> Result<ClipboardTransfer, GrpcMutationError> {
    ensure_mutation_not_cancelled()?;
    validate_clipboard_size(text).map_err(GrpcMutationError::BeforeDispatch)?;
    let blocked = timeout(CLIPBOARD_LOCK_TIMEOUT, CLIPBOARD_TRANSFER_LOCK.lock())
        .await
        .map_err(|_| {
            GrpcMutationError::BeforeDispatch("clipboard transfer lease timed out".to_string())
        })?;
    ensure_mutation_not_cancelled()?;
    if blocked.contains(&port) {
        return Err(GrpcMutationError::BeforeDispatch(
            "a prior clipboard transfer left this endpoint uncertain; inspect and reset the emulator clipboard before restarting the provider"
                .to_string(),
        ));
    }
    let mut io = GrpcClipboard {
        client: mutation_client(port, auth_token).await?,
        auth_token: auth_token.map(str::to_owned),
    };
    // Arm before awaiting staging, not just after constructing the transfer.
    // A task dropped while a scheduled write is pending must quarantine too.
    let mut lease = ClipboardLease {
        port,
        blocked,
        armed: true,
    };
    let state = match stage_text(&mut io, text, CLIPBOARD_PHASE_TIMEOUT).await {
        Ok(state) => state,
        Err(error) => {
            // A timed-out scheduled write can still arrive after cleanup.
            // Only a proved pre-dispatch failure can disarm the lease here.
            lease.armed = matches!(error, GrpcMutationError::OutcomeUnknown(_));
            return Err(error);
        }
    };
    Ok(ClipboardTransfer { io, state, lease })
}

trait ClipboardIo {
    fn read(&mut self) -> impl Future<Output = Result<String, String>> + Send;
    fn write(&mut self, text: &str) -> impl Future<Output = Result<(), String>> + Send;
}

struct GrpcClipboard {
    client: EmulatorControllerClient<Channel>,
    auth_token: Option<String>,
}

impl ClipboardIo for GrpcClipboard {
    async fn read(&mut self) -> Result<String, String> {
        self.client
            .get_clipboard(authorized_request((), self.auth_token.as_deref())?)
            .await
            .map(|reply| reply.into_inner().text)
            .map_err(|error| format!("clipboard read failed ({})", error.code()))
    }

    async fn write(&mut self, text: &str) -> Result<(), String> {
        self.client
            .set_clipboard(authorized_request(
                ClipData {
                    text: text.to_owned(),
                },
                self.auth_token.as_deref(),
            )?)
            .await
            .map(|_| ())
            .map_err(|error| format!("clipboard write acknowledgement failed ({})", error.code()))
    }
}

struct ClipboardState {
    previous: String,
    staged: String,
    stage_confirmed: bool,
    cleanup: Option<Result<ClipboardRestoreOutcome, GrpcMutationError>>,
}

fn validate_clipboard_size(text: &str) -> Result<(), String> {
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        Err("clipboard text exceeds the 1 MiB transfer limit".to_string())
    } else {
        Ok(())
    }
}

async fn stage_text(
    io: &mut impl ClipboardIo,
    text: &str,
    budget: Duration,
) -> Result<ClipboardState, GrpcMutationError> {
    validate_clipboard_size(text).map_err(GrpcMutationError::BeforeDispatch)?;
    let previous = timeout(budget, io.read())
        .await
        .map_err(|_| GrpcMutationError::BeforeDispatch("clipboard snapshot timed out".to_string()))?
        .map_err(GrpcMutationError::BeforeDispatch)?;
    validate_clipboard_size(&previous).map_err(GrpcMutationError::BeforeDispatch)?;
    ensure_mutation_not_cancelled()?;
    let mut state = ClipboardState {
        previous,
        staged: text.to_owned(),
        stage_confirmed: false,
        cleanup: None,
    };
    if state.previous == state.staged {
        state.stage_confirmed = true;
        return Ok(state);
    }
    let staging = timeout(budget, async {
        io.write(&state.staged).await?;
        await_clipboard_value(io, &state.staged, &state.previous).await
    })
    .await
    .unwrap_or_else(|_| Err("clipboard staging timed out; write may still complete".to_string()));
    if let Err(error) = staging {
        // Never retry a scheduled write or let a caller fall back to another
        // input backend. Cleanup is the only additional mutation permitted.
        let cleanup = restore_if_owned(io, &mut state, budget).await;
        let detail = match cleanup {
            Ok(ClipboardRestoreOutcome::Restored) => "prior clipboard text restored",
            Ok(ClipboardRestoreOutcome::PreservedInterveningChange) => {
                "clipboard changed; no cleanup overwrite attempted"
            }
            Err(_) => "clipboard restoration could not be confirmed",
        };
        return Err(GrpcMutationError::OutcomeUnknown(format!(
            "{error}; {detail}; no paste dispatched"
        )));
    }
    state.stage_confirmed = true;
    Ok(state)
}

async fn await_clipboard_value(
    io: &mut impl ClipboardIo,
    expected: &str,
    pending: &str,
) -> Result<(), String> {
    loop {
        let current = io.read().await?;
        if current == expected {
            return Ok(());
        }
        if current != pending {
            return Err("clipboard changed while awaiting a scheduled write".to_string());
        }
        sleep(CLIPBOARD_POLL_INTERVAL).await;
    }
}

async fn restore_if_owned(
    io: &mut impl ClipboardIo,
    state: &mut ClipboardState,
    budget: Duration,
) -> Result<ClipboardRestoreOutcome, GrpcMutationError> {
    if let Some(outcome) = &state.cleanup {
        return outcome.clone();
    }
    let result = timeout(budget, async {
        let mut current = io.read().await?;
        while !state.stage_confirmed && current == state.previous && current != state.staged {
            // Failed scheduling acknowledgement does not mean the write was
            // cancelled. Keep the lease while awaiting its delayed side effect.
            sleep(CLIPBOARD_POLL_INTERVAL).await;
            current = io.read().await?;
        }
        if current != state.staged {
            return Ok(ClipboardRestoreOutcome::PreservedInterveningChange);
        }
        if state.previous != state.staged {
            io.write(&state.previous).await?;
            await_clipboard_value(io, &state.previous, &state.staged).await?;
        }
        Ok(ClipboardRestoreOutcome::Restored)
    })
    .await
    .unwrap_or_else(|_| Err("clipboard restoration timed out; state is unknown".to_string()))
    .map_err(GrpcMutationError::OutcomeUnknown);
    state.cleanup = Some(result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingClipboard {
        value: String,
        writes: Vec<String>,
        fail_snapshot: bool,
        lose_next_acknowledgement: bool,
        delayed_next_write_reads: Option<usize>,
        pending_write: Option<(usize, String)>,
        stall_writes: bool,
    }

    impl ClipboardIo for RecordingClipboard {
        async fn read(&mut self) -> Result<String, String> {
            if let Some((remaining, value)) = &mut self.pending_write {
                if *remaining == 0 {
                    self.value = value.clone();
                    self.pending_write = None;
                } else {
                    *remaining -= 1;
                }
            }
            if self.fail_snapshot {
                Err("snapshot unavailable".to_string())
            } else {
                Ok(self.value.clone())
            }
        }

        async fn write(&mut self, text: &str) -> Result<(), String> {
            self.writes.push(text.to_owned());
            if self.stall_writes {
                std::future::pending::<()>().await;
            }
            if let Some(reads) = self.delayed_next_write_reads.take() {
                self.pending_write = Some((reads, text.to_owned()));
            } else {
                self.value = text.to_owned();
            }
            if std::mem::take(&mut self.lose_next_acknowledgement) {
                Err("acknowledgement lost".to_string())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn clipboard_selection_excludes_lossy_keyboard_cases() {
        assert!(requires_clipboard_text("Sedna café Ω %s"));
        assert!(requires_clipboard_text("literal %s"));
        assert!(requires_clipboard_text("line\nnext"));
        assert!(!requires_clipboard_text("  plain ASCII  "));
        assert!(!requires_clipboard_text(""));
    }

    #[tokio::test]
    async fn transfer_preserves_utf8_and_restores_private_text_once() {
        let mut io = RecordingClipboard {
            value: "private previous clipboard".to_string(),
            ..Default::default()
        };
        let text = "  Sedna café Ω %s\n  ";
        let mut state = stage_text(&mut io, text, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(io.value, text);
        assert_eq!(io.writes, [text]);
        assert_eq!(
            restore_if_owned(&mut io, &mut state, Duration::from_secs(1)).await,
            Ok(ClipboardRestoreOutcome::Restored)
        );
        assert_eq!(io.value, "private previous clipboard");
        assert_eq!(io.writes.len(), 2);
        restore_if_owned(&mut io, &mut state, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(io.writes.len(), 2, "cleanup must not replay");
    }

    #[tokio::test]
    async fn cleanup_preserves_observed_intervening_change() {
        let mut io = RecordingClipboard::default();
        let mut state = stage_text(&mut io, "Ω", Duration::from_secs(1))
            .await
            .unwrap();
        io.value = "external update".to_string();
        assert_eq!(
            restore_if_owned(&mut io, &mut state, Duration::from_secs(1)).await,
            Ok(ClipboardRestoreOutcome::PreservedInterveningChange)
        );
        assert_eq!(io.value, "external update");
        assert_eq!(io.writes.len(), 1);
    }

    #[tokio::test]
    async fn lost_stage_acknowledgement_cleans_up_without_replaying() {
        let mut io = RecordingClipboard {
            value: "private original".to_string(),
            lose_next_acknowledgement: true,
            ..Default::default()
        };
        let error = match stage_text(&mut io, "secret Ω", Duration::from_secs(1)).await {
            Ok(_) => panic!("uncertain staging must fail before paste"),
            Err(error) => error,
        };
        assert!(matches!(error, GrpcMutationError::OutcomeUnknown(_)));
        assert!(!error.to_string().contains("private original"));
        assert!(!error.to_string().contains("secret Ω"));
        assert_eq!(io.value, "private original");
        assert_eq!(io.writes, ["secret Ω", "private original"]);
    }

    #[tokio::test]
    async fn failed_snapshot_never_changes_clipboard() {
        let mut io = RecordingClipboard {
            fail_snapshot: true,
            ..Default::default()
        };
        assert!(matches!(
            stage_text(&mut io, "Ω", Duration::from_secs(1)).await,
            Err(GrpcMutationError::BeforeDispatch(_))
        ));
        assert!(io.writes.is_empty());
    }

    #[tokio::test]
    async fn lost_acknowledgement_awaits_delayed_staging_before_cleanup() {
        let mut io = RecordingClipboard {
            value: "private original".to_string(),
            lose_next_acknowledgement: true,
            delayed_next_write_reads: Some(2),
            ..Default::default()
        };
        let result = stage_text(&mut io, "Ω", Duration::from_secs(1)).await;
        assert!(matches!(result, Err(GrpcMutationError::OutcomeUnknown(_))));
        assert_eq!(io.value, "private original");
        assert!(io.pending_write.is_none());
        assert_eq!(io.writes, ["Ω", "private original"]);
    }

    #[tokio::test]
    async fn unresolved_scheduled_write_remains_unknown_after_cleanup_budget() {
        let mut io = RecordingClipboard {
            value: "private original".to_string(),
            lose_next_acknowledgement: true,
            delayed_next_write_reads: Some(usize::MAX),
            ..Default::default()
        };
        let error = match stage_text(&mut io, "Ω", Duration::from_millis(5)).await {
            Ok(_) => panic!("a pending clipboard write must not permit paste"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("restoration could not be confirmed")
        );
        assert!(!error.to_string().contains("private original"));
        assert_eq!(io.writes, ["Ω"]);
    }

    #[tokio::test]
    async fn abandoning_staging_quarantines_the_endpoint_before_releasing_its_lease() {
        let blocked = CLIPBOARD_TRANSFER_LOCK.lock().await;
        let lease = ClipboardLease {
            port: 0,
            blocked,
            armed: true,
        };
        let mut io = RecordingClipboard {
            stall_writes: true,
            ..Default::default()
        };
        let result = timeout(Duration::from_millis(5), async {
            let _lease = lease;
            stage_text(&mut io, "Ω", Duration::from_secs(1)).await
        })
        .await;
        assert!(result.is_err());
        let mut blocked = CLIPBOARD_TRANSFER_LOCK.lock().await;
        assert!(blocked.remove(&0), "dropped staging must quarantine first");
        assert_eq!(io.writes, ["Ω"]);
    }

    #[tokio::test]
    async fn cancellation_does_not_interrupt_restoration() {
        let mut io = RecordingClipboard::default();
        let mut state = stage_text(&mut io, "Ω", Duration::from_secs(1))
            .await
            .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        crate::execution::with_cancellation(token, async {
            assert_eq!(
                restore_if_owned(&mut io, &mut state, Duration::from_secs(1)).await,
                Ok(ClipboardRestoreOutcome::Restored)
            );
        })
        .await;
        assert_eq!(io.value, "");
        assert_eq!(io.writes, ["Ω", ""]);
    }

    #[tokio::test]
    async fn stalled_restoration_is_bounded_and_not_retried() {
        let mut io = RecordingClipboard::default();
        let mut state = stage_text(&mut io, "Ω", Duration::from_secs(1))
            .await
            .unwrap();
        io.stall_writes = true;
        let result = timeout(
            Duration::from_secs(1),
            restore_if_owned(&mut io, &mut state, Duration::from_millis(5)),
        )
        .await
        .expect("cleanup must remain bounded");
        assert!(matches!(result, Err(GrpcMutationError::OutcomeUnknown(_))));
        assert_eq!(io.writes, ["Ω", ""]);
        let repeated = restore_if_owned(&mut io, &mut state, Duration::from_secs(1)).await;
        assert_eq!(repeated, result);
        assert_eq!(io.writes, ["Ω", ""], "uncertain cleanup must not replay");
    }
}
