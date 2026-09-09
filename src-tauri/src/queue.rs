//! Queue model and persistence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::download::{DownloadPlan, Session};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Downloading,
    Paused,
    /// The app stopped without a clean shutdown while this was downloading.
    /// Distinct from `Paused` because only this state auto-resumes.
    Interrupted,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub id: String,
    pub url: String,
    pub plan: DownloadPlan,
    pub status: Status,
    /// Per-segment bytes confirmed on disk. Index-aligned with `plan.ranges`.
    pub done: Vec<u64>,
    pub error: Option<String>,
    pub added_at: u64,
    /// Browser session captured by the extension for this download, replayed
    /// on start and resume. `None` for a plain typed-in URL.
    #[serde(default)]
    pub session: Option<Session>,
}

impl Download {
    pub fn new(id: String, plan: DownloadPlan) -> Self {
        let segments = plan.segment_count();
        Download {
            id,
            url: plan.url.clone(),
            status: Status::Queued,
            done: vec![0; segments],
            error: None,
            added_at: now_secs(),
            session: None,
            plan,
        }
    }

    pub fn downloaded(&self) -> u64 {
        self.done.iter().sum()
    }

    pub fn filename(&self) -> String {
        self.plan.filename()
    }

    /// Finished for good, one way or the other.
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, Status::Completed | Status::Failed)
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a relaunch does to each state.
///
/// A user who explicitly paused (to free bandwidth, say) must not have that
/// decision overridden simply by reopening the app, and a download that has
/// already failed should not silently retry. Only an *unplanned* stop —
/// crash, forced quit, power cut — resumes on its own.
pub fn reconcile_on_launch(status: Status) -> Status {
    match status {
        // Was mid-transfer when the process died.
        Status::Downloading => Status::Interrupted,
        // Explicit user decisions survive a restart untouched.
        Status::Paused => Status::Paused,
        Status::Failed => Status::Failed,
        Status::Completed => Status::Completed,
        Status::Queued => Status::Queued,
        Status::Interrupted => Status::Interrupted,
    }
}

/// Write JSON through a temp file and rename it into place.
///
/// A direct write can be interrupted after truncating and before the new
/// content lands, leaving a zero-byte `queue.json` and losing the entire
/// queue. Rename is atomic, so a reader sees either the old file or the new
/// one and never a half-written one.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize failed: {e}"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    let tmp = tmp_path(path);
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
        file.write_all(&body).map_err(|e| format!("write failed: {e}"))?;
        // Rename only guarantees atomicity of the *directory entry*. Without
        // this the new file can be visible but empty after a power cut.
        file.sync_all().map_err(|e| format!("sync failed: {e}"))?;
    }

    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let body = std::fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table from the plan, asserted directly. The two rows that matter
    /// are Paused and Failed: those are user decisions, and auto-resuming
    /// either one on launch would override a choice the user made deliberately.
    #[test]
    fn restart_state_table() {
        assert_eq!(reconcile_on_launch(Status::Downloading), Status::Interrupted);
        assert_eq!(reconcile_on_launch(Status::Paused), Status::Paused);
        assert_eq!(reconcile_on_launch(Status::Failed), Status::Failed);
        assert_eq!(reconcile_on_launch(Status::Completed), Status::Completed);
        assert_eq!(reconcile_on_launch(Status::Queued), Status::Queued);
    }

    #[test]
    fn atomic_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fetchd-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("queue.json");

        let value = vec![1u64, 2, 3];
        save_json(&path, &value).unwrap();
        assert_eq!(load_json::<Vec<u64>>(&path), Some(vec![1, 2, 3]));

        // Overwriting leaves no temp file behind and no truncated remains.
        save_json(&path, &vec![9u64]).unwrap();
        assert_eq!(load_json::<Vec<u64>>(&path), Some(vec![9]));
        assert!(!dir.join("queue.json.tmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_of_corrupt_file_is_none_not_panic() {
        let dir = std::env::temp_dir().join(format!("fetchd-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        std::fs::write(&path, b"{ truncated").unwrap();

        assert_eq!(load_json::<Vec<u64>>(&path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
