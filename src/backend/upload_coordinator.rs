//! Process-wide admission control and FIFO ownership for image uploads.
//!
//! Upload transport stays in `image_uploader`; this module only bounds retained
//! work, serializes provider requests, and makes every accepted item own an
//! accounting reservation until completion.

use crate::backend::image_uploader::ImageUploaderConfig;
use crate::backend::lock_utils::MutexExt;
use crate::state::SplitId;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(crate) const MAX_UPLOAD_ITEMS: usize = 16;
pub(crate) const MAX_RETAINED_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
const PROVIDER_REQUEST_SPACING: Duration = Duration::from_secs(2);

pub(crate) enum UploadSource {
    ClipboardRgba {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    DroppedFile {
        path: PathBuf,
    },
}

pub(crate) struct UploadJob {
    pub channel: String,
    pub split_id: SplitId,
    pub config: Arc<ImageUploaderConfig>,
    pub source: UploadSource,
}

impl UploadJob {
    fn retained_bytes(&self) -> usize {
        let source_bytes = match &self.source {
            UploadSource::ClipboardRgba { rgba, .. } => rgba.capacity(),
            UploadSource::DroppedFile { path } => path.to_string_lossy().len(),
        };
        std::mem::size_of::<Self>()
            .saturating_add(self.channel.capacity())
            .saturating_add(source_bytes)
    }
}

struct QueuedUpload {
    job: UploadJob,
    retained_bytes: usize,
}

#[derive(Default)]
struct CoordinatorState {
    queue: VecDeque<QueuedUpload>,
    admitted_items: usize,
    retained_bytes: usize,
    active_worker: Option<u64>,
    next_worker_token: u64,
    last_provider_request_start: Option<Instant>,
}

static COORDINATOR: LazyLock<Mutex<CoordinatorState>> =
    LazyLock::new(|| Mutex::new(CoordinatorState::default()));

fn coordinator() -> MutexGuard<'static, CoordinatorState> {
    COORDINATOR.lock_or_recover()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    ItemLimit,
    ByteLimit,
}

/// Enqueue a batch atomically. A returned token means the caller owns starting
/// the sole queue worker; `None` means an existing worker will drain the batch.
pub(crate) fn enqueue_batch(jobs: Vec<UploadJob>) -> Result<Option<u64>, AdmissionError> {
    if jobs.is_empty() {
        return Ok(None);
    }

    let additional_items = jobs.len();
    let additional_bytes = jobs
        .iter()
        .try_fold(0usize, |total, job| total.checked_add(job.retained_bytes()));
    let Some(additional_bytes) = additional_bytes else {
        return Err(AdmissionError::ByteLimit);
    };

    let mut state = coordinator();
    if state
        .admitted_items
        .checked_add(additional_items)
        .is_none_or(|total| total > MAX_UPLOAD_ITEMS)
    {
        return Err(AdmissionError::ItemLimit);
    }
    if state
        .retained_bytes
        .checked_add(additional_bytes)
        .is_none_or(|total| total > MAX_RETAINED_UPLOAD_BYTES)
    {
        return Err(AdmissionError::ByteLimit);
    }

    state.admitted_items += additional_items;
    state.retained_bytes += additional_bytes;
    state.queue.extend(jobs.into_iter().map(|job| QueuedUpload {
        retained_bytes: job.retained_bytes(),
        job,
    }));

    if state.active_worker.is_some() {
        return Ok(None);
    }

    state.next_worker_token = state.next_worker_token.wrapping_add(1).max(1);
    let token = state.next_worker_token;
    state.active_worker = Some(token);
    Ok(Some(token))
}

/// Reservation for one admitted upload. Its accounting is released on every
/// exit path, including cancellation and panic unwinding.
pub(crate) struct UploadReservation {
    job: Option<UploadJob>,
    retained_bytes: usize,
}

impl UploadReservation {
    pub(crate) fn channel(&self) -> &str {
        self.job
            .as_ref()
            .map(|job| job.channel.as_str())
            .unwrap_or_default()
    }

    pub(crate) fn take_job(&mut self) -> Option<UploadJob> {
        self.job.take()
    }

    /// Reserve memory before reading an active dropped file. This prevents the
    /// active file buffer from bypassing the process-wide retained-byte cap.
    pub(crate) fn reserve_additional_bytes(&mut self, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }

        let mut state = coordinator();
        let Some(next_total) = state.retained_bytes.checked_add(bytes) else {
            return false;
        };
        if next_total > MAX_RETAINED_UPLOAD_BYTES {
            return false;
        }
        let Some(next_reservation) = self.retained_bytes.checked_add(bytes) else {
            return false;
        };

        state.retained_bytes = next_total;
        self.retained_bytes = next_reservation;
        true
    }
}

impl Drop for UploadReservation {
    fn drop(&mut self) {
        let mut state = coordinator();
        state.admitted_items = state.admitted_items.saturating_sub(1);
        state.retained_bytes = state.retained_bytes.saturating_sub(self.retained_bytes);
    }
}

/// Pop the next FIFO item for the current worker. A stale worker token cannot
/// take ownership after another worker generation has started.
pub(crate) fn take_next(worker_token: u64) -> Option<UploadReservation> {
    let mut state = coordinator();
    if state.active_worker != Some(worker_token) {
        return None;
    }

    let Some(queued) = state.queue.pop_front() else {
        state.active_worker = None;
        return None;
    };
    Some(UploadReservation {
        job: Some(queued.job),
        retained_bytes: queued.retained_bytes,
    })
}

/// Generation-aware worker lease. Unexpected worker unwinding releases the
/// ownership flag without clobbering a newer worker generation.
pub(crate) struct WorkerLease {
    worker_token: u64,
}

pub(crate) fn worker_lease(worker_token: u64) -> WorkerLease {
    WorkerLease { worker_token }
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        let mut state = coordinator();
        if state.active_worker == Some(self.worker_token) {
            state.active_worker = None;
        }
    }
}

/// Wait until the next provider request may start, then reserve that start
/// instant. The timestamp remains process-wide across worker restarts.
pub(crate) async fn wait_for_provider_request_slot() {
    loop {
        let wait = {
            let mut state = coordinator();
            let now = Instant::now();
            match state.last_provider_request_start {
                Some(last) if now < last + PROVIDER_REQUEST_SPACING => {
                    Some((last + PROVIDER_REQUEST_SPACING).saturating_duration_since(now))
                }
                _ => {
                    state.last_provider_request_start = Some(now);
                    None
                }
            }
        };

        let Some(wait) = wait else {
            return;
        };
        tokio::time::sleep(wait).await;
    }
}
