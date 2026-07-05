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
//! trait-like shape (`append` / `load` / `reset`) is intentionally simple so
//! the backing can be swapped for SQLite/Postgres later without touching
//! callers. Key store and quota state remain in-memory for now (rebuilt from
//! config on boot) and are the next persistence iteration.

use std::path::PathBuf;

use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::metrics::metering::MeteringEvent;

/// Append-only JSONL store for [`MeteringEvent`]. One JSON object per line.
pub struct FileMeteringStore {
    path: PathBuf,
}

impl FileMeteringStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append a single event as a JSON line. Safe under concurrency: the file
    /// is opened in append mode, and POSIX guarantees append writes are atomic
    /// for lines smaller than `PIPE_BUF` (our JSON lines are).
    pub async fn append(&self, event: &MeteringEvent) -> std::io::Result<()> {
        // Parent dir must exist for the open to succeed.
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(&line).await?;
        // flush (not fsync) — durability is best-effort; we accept losing the
        // last few in-flight events on a hard crash in exchange for not
        // blocking the hot path on an fsync per request.
        f.flush().await?;
        Ok(())
    }

    /// Load every persisted event, oldest-first. Returns an empty vec if the
    /// file does not exist yet (fresh boot). Malformed lines are skipped with
    /// a warning rather than aborting the whole replay.
    pub async fn load(&self) -> std::io::Result<Vec<MeteringEvent>> {
        let mut f = match tokio::fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut buf = String::new();
        f.read_to_string(&mut buf).await?;
        let mut out = Vec::new();
        for (i, line) in buf.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<MeteringEvent>(line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    tracing::warn!(
                        file = %self.path.display(),
                        line = i,
                        error = %e,
                        "skipping malformed metering event during replay"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Truncate the store — called when the billing window is explicitly reset
    /// via the admin API so a fresh window starts clean on disk too.
    pub async fn reset(&self) -> std::io::Result<()> {
        let _ = tokio::fs::create_dir_all(self.path.parent().unwrap_or_else(|| std::path::Path::new(""))).await;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .await?;
        f.flush().await?;
        Ok(())
    }
}
