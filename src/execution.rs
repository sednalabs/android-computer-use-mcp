//! Shared device leases and durable non-replayable mutation receipts.
use crate::McpError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Instant;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    pub(crate) static REQUEST_CANCELLATION: CancellationToken;
}

pub(crate) fn request_cancelled() -> bool {
    REQUEST_CANCELLATION
        .try_with(CancellationToken::is_cancelled)
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) async fn with_cancellation<F: std::future::Future>(
    token: CancellationToken,
    future: F,
) -> F::Output {
    REQUEST_CANCELLATION.scope(token, future).await
}

const MAX_RECORD_BYTES: u64 = 512 * 1024;
const MAX_RECORDS: usize = 10_000;
#[derive(Default)]
pub(crate) struct ExecutionState {
    locks: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    pub(crate) observations: Mutex<HashMap<String, ObservationRecord>>,
    pub(crate) next_observation: AtomicU64,
}
pub(crate) struct ObservationRecord {
    pub(crate) id: String,
    pub(crate) captured: Instant,
    pub(crate) screenshot_sha256: String,
    pub(crate) boot_id: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) orientation: String,
    pub(crate) window_identity: String,
}
impl ExecutionState {
    pub(crate) async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key.to_owned(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
    pub(crate) async fn invalidate(&self, serial: &str) {
        self.observations.lock().await.remove(serial);
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LedgerRecord {
    pub(crate) schema_version: u32,
    pub(crate) request_sha256: String,
    pub(crate) serial: String,
    pub(crate) provider_epoch: String,
    pub(crate) state: String,
    pub(crate) receipt: Value,
}
pub(crate) enum Claim {
    New(PathBuf, LedgerRecord),
    Existing(LedgerRecord),
}

#[derive(Debug, PartialEq, Eq)]
struct ProviderScope {
    environment_id: String,
    provider_instance_id: String,
    session_id: String,
}

fn receipt_scope(receipt: &Value) -> Result<ProviderScope, McpError> {
    let target = receipt
        .get("execution_target")
        .filter(|target| target.is_object())
        .or_else(|| receipt.get("resolved_target"))
        .ok_or_else(|| invalid("action receipt is missing its provider scope"))?;
    let field = |name: &str| {
        target[name]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid("action receipt has an incomplete provider scope"))
    };
    Ok(ProviderScope {
        environment_id: field("environment_id")?,
        provider_instance_id: field("provider_instance_id")?,
        session_id: field("session_id")?,
    })
}

pub(crate) async fn claim(
    root: &Path,
    action_id: &str,
    request_sha256: &str,
    serial: &str,
    provider_epoch: &str,
    receipt: Value,
) -> Result<Claim, McpError> {
    let scope = receipt_scope(&receipt)?;
    let directory = root.join(".action-ledger");
    fs::create_dir_all(&directory).await.map_err(ledger_error)?;
    if fs::symlink_metadata(&directory)
        .await
        .map_err(ledger_error)?
        .file_type()
        .is_symlink()
    {
        return Err(invalid("action ledger directory must not be a symlink"));
    }
    // The directory entry itself must survive a host failure before any intent
    // can authorise a mutation. Sync every ancestor because create_dir_all may
    // also have created the configured artifact root or its parents.
    sync_directory_ancestors(&directory).await?;
    let path = directory.join(format!("{:x}.json", Sha256::digest(action_id.as_bytes())));
    if let Some(record) = read_record(&path).await? {
        return validate_existing(record, request_sha256, serial, &scope);
    }
    let mut entries = fs::read_dir(&directory).await.map_err(ledger_error)?;
    let mut count = 0;
    while entries.next_entry().await.map_err(ledger_error)?.is_some() {
        count += 1;
        if count >= MAX_RECORDS {
            return Err(invalid(
                "action ledger is full; operator archival required before new mutations",
            ));
        }
    }
    let record = LedgerRecord {
        schema_version: 1,
        request_sha256: request_sha256.into(),
        serial: serial.into(),
        provider_epoch: provider_epoch.into(),
        state: "pending".into(),
        receipt,
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = match options.open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let record = read_record(&path)
                .await?
                .ok_or_else(|| invalid("action claim disappeared; observe before retry"))?;
            return validate_existing(record, request_sha256, serial, &scope);
        }
        Err(error) => return Err(ledger_error(error)),
    };
    let bytes = serde_json::to_vec(&record).map_err(ledger_error)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("receipt exceeds ledger limit"));
    }
    file.write_all(&bytes).await.map_err(ledger_error)?;
    file.sync_all().await.map_err(ledger_error)?;
    sync_directory(&directory).await?;
    Ok(Claim::New(path, record))
}

fn validate_existing(
    record: LedgerRecord,
    digest: &str,
    serial: &str,
    scope: &ProviderScope,
) -> Result<Claim, McpError> {
    if record.schema_version != 1 || record.request_sha256 != digest || record.serial != serial {
        return Err(invalid(
            "action_id belongs to a different request or device; no action executed",
        ));
    }
    if receipt_scope(&record.receipt)? != *scope {
        return Err(invalid(
            "action_id belongs to a different environment, provider or session; no action executed",
        ));
    }
    Ok(Claim::Existing(record))
}

async fn read_record(path: &Path) -> Result<Option<LedgerRecord>, McpError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ledger_error(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_RECORD_BYTES
    {
        return Err(invalid(
            "action record unavailable; its action ID cannot be replayed",
        ));
    }
    let bytes = fs::read(path).await.map_err(ledger_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| invalid("action record incomplete; observe and do not replay this ID"))
}

pub(crate) async fn persist(path: &Path, record: &LedgerRecord) -> Result<(), McpError> {
    let bytes = serde_json::to_vec(record).map_err(ledger_error)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("receipt exceeds durable ledger limit"));
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await.map_err(ledger_error)?;
    if let Err(error) = async {
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await
    }
    .await
    {
        let _ = fs::remove_file(&temporary).await;
        return Err(ledger_error(error));
    }
    sync_directory(path.parent().expect("ledger path has parent")).await
}
async fn sync_directory(directory: &Path) -> Result<(), McpError> {
    #[cfg(unix)]
    {
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || std::fs::File::open(directory)?.sync_all())
            .await
            .map_err(ledger_error)?
            .map_err(ledger_error)?;
    }
    Ok(())
}

async fn sync_directory_ancestors(directory: &Path) -> Result<(), McpError> {
    #[cfg(unix)]
    {
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || {
            sync_directory_chain(&directory, |path| std::fs::File::open(path)?.sync_all())
        })
        .await
        .map_err(ledger_error)?
        .map_err(ledger_error)?;
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn sync_directory_chain(
    directory: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let absolute = if directory.is_absolute() {
        directory.to_owned()
    } else {
        std::env::current_dir()?.join(directory)
    };
    for path in absolute.ancestors() {
        sync(path)?;
    }
    Ok(())
}
fn invalid(message: &str) -> McpError {
    McpError::invalid_params(
        message.to_owned(),
        Some(json!({"do_not_replay":true,"retry_safe":false})),
    )
}
fn ledger_error(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(
        format!("cannot persist mutation receipt: {error}"),
        Some(json!({"outcome":"outcome_unknown","do_not_replay":true,"retry_safe":false})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scoped(mut receipt: Value) -> Value {
        receipt["execution_target"] = json!({
            "environment_id": "environment-a",
            "provider_instance_id": "provider-a",
            "session_id": "session-a",
            "provider_epoch": "original-epoch",
        });
        receipt
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(
                std::env::temp_dir()
                    .join(format!("android-mcp-ledger-{}-{nonce}", std::process::id())),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn pending_intent_survives_restart_without_becoming_executable() {
        let dir = Fixture::new();
        let Claim::New(_, _) = claim(
            &dir.0,
            "once",
            "digest",
            "device",
            "old",
            scoped(json!({"attempted_action_count":1})),
        )
        .await
        .unwrap() else {
            panic!("new claim")
        };
        let Claim::Existing(record) =
            claim(&dir.0, "once", "digest", "device", "new", scoped(json!({})))
                .await
                .unwrap()
        else {
            panic!("must not execute again")
        };
        assert_eq!(record.state, "pending");
        assert_eq!(record.provider_epoch, "old");
        assert_eq!(record.receipt["attempted_action_count"], 1);
    }
    #[tokio::test]
    async fn complete_receipt_persists_and_cannot_change_request_or_device() {
        let dir = Fixture::new();
        let Claim::New(path, mut record) =
            claim(&dir.0, "once", "digest", "a", "epoch", scoped(json!({})))
                .await
                .unwrap()
        else {
            panic!()
        };
        record.state = "complete".into();
        record.receipt = scoped(json!({"executed_action_count":2,"failed_index":2}));
        persist(&path, &record).await.unwrap();
        let Claim::Existing(saved) =
            claim(&dir.0, "once", "digest", "a", "next", scoped(json!({})))
                .await
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(saved.receipt, record.receipt);
        assert_eq!(saved.state, "complete");
        assert!(
            claim(&dir.0, "once", "different", "a", "epoch", scoped(json!({})))
                .await
                .is_err()
        );
        assert!(
            claim(&dir.0, "once", "digest", "b", "epoch", scoped(json!({})))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn incomplete_record_is_never_replaced_or_reclaimed() {
        let dir = Fixture::new();
        let Claim::New(path, _) = claim(&dir.0, "once", "digest", "a", "epoch", scoped(json!({})))
            .await
            .unwrap()
        else {
            panic!()
        };
        fs::write(&path, b"{").await.unwrap();
        assert!(
            claim(&dir.0, "once", "digest", "a", "epoch", scoped(json!({})))
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path).await.unwrap(), b"{");
    }

    #[tokio::test]
    async fn duplicate_claim_rejects_another_stable_provider_scope() {
        let dir = Fixture::new();
        claim(&dir.0, "once", "digest", "device", "old", scoped(json!({})))
            .await
            .unwrap();
        for field in ["environment_id", "provider_instance_id", "session_id"] {
            let mut receipt = scoped(json!({}));
            receipt["execution_target"][field] = json!("different");
            assert!(
                claim(&dir.0, "once", "digest", "device", "new", receipt)
                    .await
                    .is_err(),
                "changed {field} must not recover another scope's receipt"
            );
        }
    }

    #[tokio::test]
    async fn restart_recovery_accepts_legacy_target_key_and_new_epoch() {
        let dir = Fixture::new();
        let mut old_receipt = scoped(json!({}));
        let target = old_receipt
            .as_object_mut()
            .unwrap()
            .remove("execution_target")
            .unwrap();
        old_receipt["resolved_target"] = target;
        claim(&dir.0, "once", "digest", "device", "old", old_receipt)
            .await
            .unwrap();
        let mut new_receipt = scoped(json!({}));
        new_receipt["execution_target"]["provider_epoch"] = json!("new-epoch");
        let Claim::Existing(saved) = claim(&dir.0, "once", "digest", "device", "new", new_receipt)
            .await
            .unwrap()
        else {
            panic!("existing intent must never be executed again")
        };
        assert_eq!(saved.provider_epoch, "old");
    }

    #[tokio::test]
    async fn missing_scope_is_rejected_before_creating_an_intent() {
        let dir = Fixture::new();
        assert!(
            claim(&dir.0, "once", "digest", "device", "epoch", json!({}))
                .await
                .is_err()
        );
        assert!(!dir.0.exists());
    }

    #[test]
    fn durable_directory_creation_syncs_the_ledger_and_all_ancestors() {
        let path = std::env::temp_dir().join("provider/new/artifacts/.action-ledger");
        let mut visited = Vec::new();
        sync_directory_chain(&path, |directory| {
            visited.push(directory.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            visited,
            path.ancestors().map(Path::to_path_buf).collect::<Vec<_>>()
        );
        assert!(visited.contains(&path.parent().unwrap().to_owned()));
    }

    #[test]
    fn parent_sync_failure_is_propagated_before_directory_acknowledgement() {
        let path = std::env::temp_dir().join("provider/artifacts/.action-ledger");
        let parent = path.parent().unwrap();
        let mut visited = Vec::new();
        let result = sync_directory_chain(&path, |directory| {
            visited.push(directory.to_owned());
            if directory == parent {
                Err(std::io::Error::other("directory sync failed"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(visited, vec![path.clone(), parent.to_owned()]);
    }

    #[tokio::test]
    async fn request_cancellation_is_scoped_and_defaults_to_false() {
        assert!(!request_cancelled());
        let token = CancellationToken::new();
        with_cancellation(token.clone(), async {
            assert!(!request_cancelled());
            token.cancel();
            assert!(request_cancelled());
        })
        .await;
        assert!(!request_cancelled());
    }
    #[tokio::test]
    async fn device_lease_excludes_same_device_and_allows_other_devices() {
        let state = Arc::new(ExecutionState::default());
        let first = state.lock("device:a").await;
        let shared = Arc::clone(&state);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                shared.lock("device:a")
            )
            .await
            .is_err()
        );
        let other = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            shared.lock("device:b"),
        )
        .await
        .unwrap();
        drop(first);
        let same = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            shared.lock("device:a"),
        )
        .await
        .unwrap();
        drop((other, same));
        let _last = state.lock("device:c").await;
        assert_eq!(state.locks.lock().unwrap().len(), 1);
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ActionStatusArgs {
    /// ID previously used with android.step or android.install_build_from_run.
    pub action_id: String,
}

#[rmcp::tool_router(router = tool_router_execution, vis = "pub")]
impl crate::server::AndroidEmulatorMcp {
    #[rmcp::tool(
        name = "android.get_action_status",
        description = "Recover a durable Android action receipt, including incomplete operations, without touching the device. Works after provider restart or when the device is offline. Never authorises replay.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn android_get_action_status(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<ActionStatusArgs>,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        if args.action_id.is_empty()
            || args.action_id.len() > 128
            || !args
                .action_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(invalid("invalid action_id"));
        }
        let directory = self.config.artifact_dir.join(".action-ledger");
        if let Ok(metadata) = fs::symlink_metadata(&directory).await
            && metadata.file_type().is_symlink()
        {
            return Err(invalid("ledger directory must not be a symlink"));
        }
        let path = directory.join(format!(
            "{:x}.json",
            Sha256::digest(args.action_id.as_bytes())
        ));
        let record = read_record(&path).await?;
        if let Some(record) = &record {
            let target = record
                .receipt
                .get("execution_target")
                .or_else(|| record.receipt.get("resolved_target"))
                .unwrap_or(&Value::Null);
            let identity = &self.config.execution_identity;
            if target["environment_id"] != identity.environment_id
                || target["provider_instance_id"] != identity.provider_instance_id
                || target["session_id"] != identity.session_id
            {
                return Err(invalid(
                    "action receipt belongs to a different provider scope",
                ));
            }
        }
        Ok(rmcp::model::CallToolResult::structured(json!({
            "ok": true, "action_id": args.action_id,
            "state": record.as_ref().map(|r| r.state.as_str()).unwrap_or("not_found"),
            "operation_complete": record.as_ref().is_some_and(|r| r.state == "complete"),
            "do_not_replay": true, "retry_safe": false,
            "receipt": record.as_ref().map(|r| &r.receipt),
            "original_provider_epoch": record.as_ref().map(|r| &r.provider_epoch),
        })))
    }
}
