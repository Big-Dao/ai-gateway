//! Append-only JSONL persistence for metering events.
//!
//! Goal (P0 stage 4): stop losing billing/cost data on restart. The previous
//! design kept everything in memory AND explicitly zeroed cost counters on
//! every boot (`reset_billing_window`), so a single restart evaporated the
//! day's metering. This store appends each event to a JSONL file and replays
//! it on startup to rebuild the in-memory aggregates.
//!
//! Scope note: this is a minimal durable layer — not a database. It trades
//! query/transactional power for zero new dependencies and low risk. The
//! shape (`append` / `load` / `reset`) is intentionally simple so the backing
//! can be swapped for SQLite/Postgres later without touching callers. Key
//! store and quota state remain in-memory for now (rebuilt from config on
//! boot) and are the next persistence iteration.
//!
//! Integrity note: events are NOT HMAC-signed, so an attacker with write
//! access to the file can forge or alter records. Tamper-resistance (per-event
//! signatures / sealed log) is a tracked follow-up alongside the DB migration;
//! it matches the existing audit writer's threat model.

use std::path::PathBuf;

use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::metrics::metering::MeteringEvent;

/// Refuse to load a metering file larger than this. Defends against an
/// attacker (or a runaway loop) growing the file to OOM the process on
/// replay. 256 MB holds well over a million typical events.
const MAX_LOAD_BYTES: u64 = 256 * 1024 * 1024;

/// Append-only JSONL store for [`MeteringEvent`]. One JSON object per line.
pub struct FileMeteringStore {
    path: PathBuf,
    /// Serializes append / reset / load against each other. Append is the hot
    /// path but the critical section is just the file write; a reset
    /// (admin-only, rare) waits for in-flight appends to finish rather than
    /// truncating mid-write (TOCTOU / lost-write race).
    lock: Mutex<()>,
}

impl FileMeteringStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// Create the parent dir (mode 0700 on Unix). Best-effort.
    async fn ensure_parent_dir(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = tokio::fs::set_permissions(
                        parent,
                        std::fs::Permissions::from_mode(0o700),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    /// Restrict the file to owner-only (mode 0600 on Unix). Best-effort —
    /// metering events carry tenant/key/model detail and must not be
    /// world-readable. Called after the file is (re)opened.
    async fn chmod_private(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .await;
        }
    }

    /// Append a single event as a JSON line. POSIX append is atomic for lines
    /// smaller than `PIPE_BUF`; our JSON lines are.
    pub async fn append(&self, event: &MeteringEvent) -> std::io::Result<()> {
        let _g = self.lock.lock().await;
        self.ensure_parent_dir().await?;
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        self.chmod_private().await;
        f.write_all(&line).await?;
        // flush (not fsync) — durability is best-effort; we accept losing the
        // last few in-flight events on a hard crash in exchange for not
        // blocking the hot path on an fsync per request.
        f.flush().await?;
        Ok(())
    }

    /// Load every persisted event, oldest-first. Returns an empty vec if the
    /// file does not exist (fresh boot) or exceeds [`MAX_LOAD_BYTES`] (start
    /// fresh rather than OOM). Malformed lines are skipped and counted.
    pub async fn load(&self) -> std::io::Result<Vec<MeteringEvent>> {
        let _g = self.lock.lock().await;
        // Size guard before reading — refuse to pull an oversized file into
        // memory (resource-bound defense).
        match tokio::fs::metadata(&self.path).await {
            Ok(m) => {
                let len = m.len();
                if len > MAX_LOAD_BYTES {
                    tracing::error!(
                        file = %self.path.display(),
                        bytes = len,
                        max = MAX_LOAD_BYTES,
                        "metering file exceeds max load size; skipping replay"
                    );
                    return Ok(vec![]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        }
        let mut f = match tokio::fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut buf = String::with_capacity(8 * 1024);
        f.read_to_string(&mut buf).await?;
        let mut out = Vec::new();
        let mut malformed: u64 = 0;
        for (i, line) in buf.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<MeteringEvent>(line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    malformed += 1;
                    tracing::warn!(
                        file = %self.path.display(),
                        line = i,
                        error = %e,
                        "skipping malformed metering event during replay"
                    );
                }
            }
        }
        if malformed > 0 {
            tracing::warn!(
                file = %self.path.display(),
                malformed,
                loaded = out.len(),
                "metering replay completed with malformed lines (possible tampering or partial write)"
            );
        }
        Ok(out)
    }

    /// Truncate the store — called when the billing window is explicitly reset
    /// via the admin API so a fresh window starts clean on disk too.
    pub async fn reset(&self) -> std::io::Result<()> {
        let _g = self.lock.lock().await;
        self.ensure_parent_dir().await?;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .await?;
        self.chmod_private().await;
        f.flush().await?;
        Ok(())
    }
}
