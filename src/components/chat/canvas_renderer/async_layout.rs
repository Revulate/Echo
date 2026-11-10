//! Async layout computation for canvas renderer.
//!
//! Moves expensive layout calculations to background threads to prevent
//! UI blocking, enabling smooth 60 FPS even during heavy message loads.

use super::layout::{CanvasMessageLayout, message_content_fingerprint, quantize_width};
use super::text_shaping::build_message_layout;
use crate::backend::ChatMessage;
use crate::backend::lock_utils::{MutexExt, RwLockExt};
use crate::backend::settings;
use crossbeam_channel::{
    Receiver as CbReceiver, Sender as CbSender, TrySendError as CbTrySendError,
};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock, Weak, mpsc};
use std::time::Instant;

/// Five full 192-row visible/prewarm bursts can queue without backpressure, while
/// resize/settings storms remain strictly bounded instead of retaining every stale request.
const LAYOUT_QUEUE_CAPACITY: usize = 1024;

static LAYOUT_QUEUE_PEAK_DEPTH: AtomicUsize = AtomicUsize::new(0);
static LAYOUT_QUEUE_WAIT_MAX_US: AtomicU64 = AtomicU64::new(0);
static LAYOUT_ENQUEUED_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_QUEUE_FULL_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_QUEUE_DISCONNECTED_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_WORKER_PANIC_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_SHAPED_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_PUBLISHED_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_OBSOLETE_BEFORE_SHAPE_TOTAL: AtomicU64 = AtomicU64::new(0);
static LAYOUT_OBSOLETE_AFTER_SHAPE_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Global async-layout worker state. Counters are cumulative for the process lifetime;
/// `queue_depth` is a point-in-time sample of the bounded crossbeam queue.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LayoutWorkerDiagnostics {
    pub(crate) queue_depth: usize,
    pub(crate) queue_capacity: usize,
    pub(crate) queue_peak_depth: usize,
    pub(crate) queue_wait_max_us: u64,
    pub(crate) enqueued_total: u64,
    pub(crate) queue_full_total: u64,
    pub(crate) queue_disconnected_total: u64,
    pub(crate) worker_panic_total: u64,
    pub(crate) shaped_total: u64,
    pub(crate) published_total: u64,
    pub(crate) obsolete_before_shape_total: u64,
    pub(crate) obsolete_after_shape_total: u64,
}

/// Status of an async layout computation
#[derive(Debug, Clone)]
pub enum LayoutStatus {
    /// Not yet requested
    Pending,
    /// Computation in progress on background thread
    Computing,
    /// Layout ready to use
    Ready(Arc<CanvasMessageLayout>),
    /// Computation failed
    Failed(()),
}

/// Request to compute layout for a message
#[derive(Clone)]
pub struct LayoutRequest {
    pub message_id: Arc<str>,
    pub message: Arc<ChatMessage>,
    pub channel: Arc<str>,
    pub available_width: f32,
    pub show_timestamp: bool,
    pub layout_settings_version: u64,
    pub emote_resolution_version: u64,
    pub channel_emote_version: u64,
    pub scale_factor: f32,
    pub mod_buttons: bool,
    /// Layout eviction sequence read before `message` was captured, so an
    /// eviction that lands during the capture still fences the result.
    pub requested_at_eviction_seq: u64,
}

/// Result of layout computation
pub struct LayoutResult {
    pub message_id: Arc<str>,
    pub layout: Arc<CanvasMessageLayout>,
}

/// Full layout identity used for deduplication and guarded publication. Width is stored
/// in quantized bucket space so request/status/cache comparisons cannot disagree during resize.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LayoutIdentity {
    quantized_width: u32,
    content_fingerprint: u64,
    layout_settings_version: u64,
    show_timestamp: bool,
    cache_invalidation_version: u64,
    emote_resolution_version: u64,
    channel_emote_version: u64,
    channel: Arc<str>,
    scale_bits: u32,
    mod_buttons: bool,
}

impl LayoutIdentity {
    fn from_request(request: &LayoutRequest) -> Self {
        Self {
            quantized_width: quantize_width(request.available_width),
            content_fingerprint: message_content_fingerprint(&request.message),
            layout_settings_version: request.layout_settings_version,
            show_timestamp: request.show_timestamp,
            cache_invalidation_version:
                crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch(
                ),
            emote_resolution_version: request.emote_resolution_version,
            channel_emote_version: request.channel_emote_version,
            channel: Arc::clone(&request.channel),
            scale_bits: request.scale_factor.to_bits(),
            mod_buttons: request.mod_buttons,
        }
    }
}

struct LayoutStatusEntry {
    status: LayoutStatus,
    identity: LayoutIdentity,
    request_generation: u64,
    manager_epoch: u64,
    /// Eviction-log sequence a Ready layout is known to be current through,
    /// or `EVICTION_FENCED` once an eviction covered it.
    validated_eviction_seq: AtomicU64,
}

const EVICTION_FENCED: u64 = u64::MAX;

impl LayoutStatusEntry {
    /// False when a message-scoped eviction recorded after this Ready layout
    /// was shaped covers it; advances the validated sequence otherwise.
    fn ready_layout_survives_evictions(&self, layout: &CanvasMessageLayout) -> bool {
        let since = self.validated_eviction_seq.load(Ordering::Acquire);
        if since == EVICTION_FENCED {
            return false;
        }
        match crate::components::chat::canvas_renderer::global_layout_cache::layout_valid_through(
            since, layout,
        ) {
            Some(through) => {
                self.validated_eviction_seq
                    .fetch_max(through, Ordering::AcqRel);
                true
            }
            None => {
                self.validated_eviction_seq
                    .store(EVICTION_FENCED, Ordering::Release);
                false
            }
        }
    }

    #[inline]
    fn is_owned_computing_job(
        &self,
        identity: &LayoutIdentity,
        request_generation: u64,
        manager_epoch: u64,
    ) -> bool {
        matches!(self.status, LayoutStatus::Computing)
            && self.identity == *identity
            && self.request_generation == request_generation
            && self.manager_epoch == manager_epoch
    }
}

/// A manager epoch invalidates all queued work when shared layout context changes (width,
/// timestamp visibility, settings, provider versions, or channel), without touching jobs for
/// other splits. Per-message generations handle content changes within the same epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagerLayoutKey {
    quantized_width: u32,
    layout_settings_version: u64,
    show_timestamp: bool,
    cache_invalidation_version: u64,
    emote_resolution_version: u64,
    channel_emote_version: u64,
    channel: Arc<str>,
    scale_bits: u32,
    mod_buttons: bool,
}

impl From<&LayoutIdentity> for ManagerLayoutKey {
    fn from(identity: &LayoutIdentity) -> Self {
        Self {
            quantized_width: identity.quantized_width,
            layout_settings_version: identity.layout_settings_version,
            show_timestamp: identity.show_timestamp,
            cache_invalidation_version: identity.cache_invalidation_version,
            emote_resolution_version: identity.emote_resolution_version,
            channel_emote_version: identity.channel_emote_version,
            channel: Arc::clone(&identity.channel),
            scale_bits: identity.scale_bits,
            mod_buttons: identity.mod_buttons,
        }
    }
}

#[derive(Default)]
struct ManagerWorkState {
    current_key: Mutex<Option<ManagerLayoutKey>>,
    current_epoch: AtomicU64,
    queue_wait: SplitQueueWait,
}

/// Queue wait of one split's jobs that went on to be shaped, since the split
/// last took the totals.
#[derive(Default)]
struct SplitQueueWait {
    count: AtomicU64,
    sum_us: AtomicU64,
    max_us: AtomicU64,
}

/// One split's shaped-job queue wait since its previous report.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SplitQueueWaitStats {
    pub(crate) count: u64,
    pub(crate) sum_us: u64,
    pub(crate) max_us: u64,
}

impl SplitQueueWait {
    fn record(&self, wait_us: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(wait_us, Ordering::Relaxed);
        self.max_us.fetch_max(wait_us, Ordering::Relaxed);
    }

    fn take(&self) -> SplitQueueWaitStats {
        SplitQueueWaitStats {
            count: self.count.swap(0, Ordering::Relaxed),
            sum_us: self.sum_us.swap(0, Ordering::Relaxed),
            max_us: self.max_us.swap(0, Ordering::Relaxed),
        }
    }
}

impl ManagerWorkState {
    fn epoch_for(&self, key: ManagerLayoutKey) -> u64 {
        let mut current_key = self.current_key.lock_or_recover();
        if current_key.as_ref() == Some(&key) {
            return self.current_epoch.load(Ordering::Acquire);
        }

        let next_epoch = self
            .current_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        *current_key = Some(key);
        next_epoch
    }

    #[inline]
    fn is_current(&self, epoch: u64) -> bool {
        self.current_epoch.load(Ordering::Acquire) == epoch
    }
}

/// Fast in-flight counter that mirrors admitted jobs owned by this manager.
///
/// `has_in_flight()` reads this counter instead of doing an O(n) scan of the
/// `statuses` map every waker tick. Obsolete admitted jobs remain counted only
/// until the bounded worker queue drains them.
#[derive(Default)]
struct InflightCounter {
    /// Number of admitted jobs that have not finished or been rejected.
    count: AtomicUsize,
}

impl InflightCounter {
    fn on_request(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    fn on_complete(&self) {
        let mut current = self.count.load(Ordering::Acquire);
        while current > 0 {
            match self.count.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    fn any(&self) -> bool {
        self.count.load(Ordering::Acquire) > 0
    }
}

struct LayoutJob {
    request: LayoutRequest,
    identity: LayoutIdentity,
    /// Eviction-log sequence when the split captured `request.message`; the
    /// published layout is validated from here, not from when shaping began.
    requested_at_eviction_seq: u64,
    request_generation: u64,
    manager_epoch: u64,
    enqueued_at: Instant,
    statuses: Arc<RwLock<HashMap<Arc<str>, LayoutStatusEntry>>>,
    inflight_counter: Arc<InflightCounter>,
    manager_work: Arc<ManagerWorkState>,
    manager_lifetime: Weak<()>,
    result_tx: mpsc::Sender<LayoutResult>,
    cleanup_status_on_drop: bool,
}

impl Drop for LayoutJob {
    fn drop(&mut self) {
        if self.cleanup_status_on_drop {
            let mut statuses = self.statuses.write_or_recover();
            let owns_entry = statuses.get(&self.request.message_id).is_some_and(|entry| {
                entry.is_owned_computing_job(
                    &self.identity,
                    self.request_generation,
                    self.manager_epoch,
                )
            });
            if owns_entry {
                statuses.remove(&self.request.message_id);
            }
        }
        self.inflight_counter.on_complete();
    }
}

struct GlobalLayoutWorkerPool {
    request_tx: CbSender<LayoutJob>,
}

static GLOBAL_LAYOUT_WORKER_POOL: LazyLock<GlobalLayoutWorkerPool> =
    LazyLock::new(GlobalLayoutWorkerPool::new);

impl GlobalLayoutWorkerPool {
    fn new() -> Self {
        let (request_tx, request_rx) =
            crossbeam_channel::bounded::<LayoutJob>(LAYOUT_QUEUE_CAPACITY);
        let worker_count = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4)
            .clamp(2, 6);

        for worker_id in 0..worker_count {
            let worker_rx = request_rx.clone();
            if let Err(error) = std::thread::Builder::new()
                .name(format!("echo-layout-{worker_id}"))
                .spawn(move || layout_worker_loop(worker_id, worker_rx))
            {
                crate::log_perf_render!(
                    error,
                    worker_id,
                    error = %error,
                    "Failed to spawn async layout worker"
                );
            }
        }

        Self { request_tx }
    }

    fn sender(&self) -> CbSender<LayoutJob> {
        self.request_tx.clone()
    }

    fn queue_depth(&self) -> usize {
        self.request_tx.len()
    }
}

pub(crate) fn layout_worker_diagnostics() -> LayoutWorkerDiagnostics {
    LayoutWorkerDiagnostics {
        queue_depth: GLOBAL_LAYOUT_WORKER_POOL.queue_depth(),
        queue_capacity: LAYOUT_QUEUE_CAPACITY,
        queue_peak_depth: LAYOUT_QUEUE_PEAK_DEPTH.load(Ordering::Relaxed),
        queue_wait_max_us: LAYOUT_QUEUE_WAIT_MAX_US.load(Ordering::Relaxed),
        enqueued_total: LAYOUT_ENQUEUED_TOTAL.load(Ordering::Relaxed),
        queue_full_total: LAYOUT_QUEUE_FULL_TOTAL.load(Ordering::Relaxed),
        queue_disconnected_total: LAYOUT_QUEUE_DISCONNECTED_TOTAL.load(Ordering::Relaxed),
        worker_panic_total: LAYOUT_WORKER_PANIC_TOTAL.load(Ordering::Relaxed),
        shaped_total: LAYOUT_SHAPED_TOTAL.load(Ordering::Relaxed),
        published_total: LAYOUT_PUBLISHED_TOTAL.load(Ordering::Relaxed),
        obsolete_before_shape_total: LAYOUT_OBSOLETE_BEFORE_SHAPE_TOTAL.load(Ordering::Relaxed),
        obsolete_after_shape_total: LAYOUT_OBSOLETE_AFTER_SHAPE_TOTAL.load(Ordering::Relaxed),
    }
}

fn layout_worker_loop(worker_id: usize, worker_rx: CbReceiver<LayoutJob>) {
    loop {
        let job = worker_rx.recv();

        match job {
            Ok(job) => {
                if catch_unwind(AssertUnwindSafe(|| process_layout_job(worker_id, job))).is_err() {
                    LAYOUT_WORKER_PANIC_TOTAL.fetch_add(1, Ordering::Relaxed);
                    crate::log_perf_render!(
                        error,
                        worker_id,
                        "Async layout worker recovered from job panic"
                    );
                }
            }
            Err(_) => {
                crate::log_perf_render!(
                    debug,
                    "Async layout worker {} shutting down (global channel closed)",
                    worker_id
                );
                break;
            }
        }
    }
}

fn request_versions_are_current(request: &LayoutRequest) -> bool {
    settings::get_layout_settings_version() == request.layout_settings_version
        && crate::backend::slots::resolver::get_emote_version() == request.emote_resolution_version
        && crate::state::VersionRegistry::channel_emote_version(&request.channel)
            == request.channel_emote_version
}

fn job_context_is_current(job: &LayoutJob) -> bool {
    job.manager_lifetime.upgrade().is_some()
        && job.manager_work.is_current(job.manager_epoch)
        && request_versions_are_current(&job.request)
        && job.identity.cache_invalidation_version
            == crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch()
}

fn job_owns_status(job: &LayoutJob, entry: &LayoutStatusEntry) -> bool {
    entry.is_owned_computing_job(&job.identity, job.request_generation, job.manager_epoch)
}

fn discard_obsolete_job(job: &LayoutJob, before_shape: bool) {
    {
        let mut statuses = job.statuses.write_or_recover();
        let owns_entry = statuses
            .get(&job.request.message_id)
            .is_some_and(|entry| job_owns_status(job, entry));
        if owns_entry {
            statuses.remove(&job.request.message_id);
        }
    }

    if before_shape {
        LAYOUT_OBSOLETE_BEFORE_SHAPE_TOTAL.fetch_add(1, Ordering::Relaxed);
    } else {
        LAYOUT_OBSOLETE_AFTER_SHAPE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
}

fn process_layout_job(_worker_id: usize, mut job: LayoutJob) {
    let queue_wait_us = job
        .enqueued_at
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    LAYOUT_QUEUE_WAIT_MAX_US.fetch_max(queue_wait_us, Ordering::Relaxed);

    let owns_current_status = {
        let statuses = job.statuses.read_or_recover();
        statuses
            .get(&job.request.message_id)
            .is_some_and(|entry| job_owns_status(&job, entry))
    };
    if !job_context_is_current(&job) || !owns_current_status {
        discard_obsolete_job(&job, true);
        job.cleanup_status_on_drop = false;
        return;
    }
    job.manager_work.queue_wait.record(queue_wait_us);

    let _scale = super::layout_scale::enter(job.request.scale_factor);
    let _mode = super::layout_mode::enter(job.request.mod_buttons);
    let mut layout = build_message_layout(
        &job.request.message,
        &job.request.channel,
        job.request.available_width,
        job.request.show_timestamp,
        None,
    );
    layout.emote_version_at_creation = job.request.emote_resolution_version;
    layout.channel_emote_version_at_creation = job.request.channel_emote_version;
    LAYOUT_SHAPED_TOTAL.fetch_add(1, Ordering::Relaxed);

    let result = LayoutResult {
        message_id: Arc::clone(&job.request.message_id),
        layout: Arc::new(layout),
    };

    let published = {
        let mut statuses = job.statuses.write_or_recover();
        let owns_entry = statuses
            .get(&job.request.message_id)
            .is_some_and(|entry| job_owns_status(&job, entry));

        let valid_through = (job_context_is_current(&job) && owns_entry)
            .then(|| {
                crate::components::chat::canvas_renderer::global_layout_cache::layout_valid_through(
                    job.requested_at_eviction_seq,
                    &result.layout,
                )
            })
            .flatten();
        if let Some(valid_through) = valid_through {
            if let Some(entry) = statuses.get_mut(&job.request.message_id) {
                entry.status = LayoutStatus::Ready(Arc::clone(&result.layout));
                *entry.validated_eviction_seq.get_mut() = valid_through;
            }
            true
        } else {
            if owns_entry {
                statuses.remove(&job.request.message_id);
            }
            false
        }
    };
    job.cleanup_status_on_drop = false;

    if !published {
        LAYOUT_OBSOLETE_AFTER_SHAPE_TOTAL.fetch_add(1, Ordering::Relaxed);
        return;
    }

    LAYOUT_PUBLISHED_TOTAL.fetch_add(1, Ordering::Relaxed);
    let _ = job.result_tx.send(result);
}

pub struct AsyncLayoutManager {
    /// Map of message_id -> layout tracking entry
    /// CRITICAL: the content fingerprint is used to invalidate layouts when message content changes
    /// (e.g., when emotes are loaded and slots are re-tokenized)
    statuses: Arc<RwLock<HashMap<Arc<str>, LayoutStatusEntry>>>,
    /// Channel to send layout requests to the shared worker pool.
    /// crossbeam MPMC sender: workers receive on their own cloned Receivers, so
    /// no Mutex is needed and all workers can wait for jobs concurrently.
    request_tx: CbSender<LayoutJob>,
    /// Sender paired with result_rx so workers can publish completed layouts to this manager.
    result_tx: mpsc::Sender<LayoutResult>,
    /// Channel to receive completed layouts
    result_rx: Mutex<mpsc::Receiver<LayoutResult>>,
    /// Current shared layout context for this split. An epoch change makes older queued jobs
    /// cheap to discard before shaping and prevents them from publishing over newer work.
    manager_work: Arc<ManagerWorkState>,
    /// Jobs hold this weakly so dropping a split cancels its retained queued work.
    manager_lifetime: Arc<()>,
    /// Monotonic per-manager identity used to distinguish replacement jobs for one message.
    next_request_generation: AtomicU64,
    /// Fast total of admitted jobs, including obsolete work until the bounded queue drains it.
    /// Lets `has_in_flight()` avoid an O(n) RwLock scan every waker tick.
    inflight_counter: Arc<InflightCounter>,
}

impl AsyncLayoutManager {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel::<LayoutResult>();
        let statuses = Arc::new(RwLock::new(HashMap::new()));

        Self {
            statuses,
            request_tx: GLOBAL_LAYOUT_WORKER_POOL.sender(),
            result_tx,
            result_rx: Mutex::new(result_rx),
            manager_work: Arc::new(ManagerWorkState::default()),
            manager_lifetime: Arc::new(()),
            next_request_generation: AtomicU64::new(0),
            inflight_counter: Arc::new(InflightCounter::default()),
        }
    }

    /// Request layout computation for a message
    /// Returns current status (will be Computing after this call)
    pub fn request_layout(&self, request: LayoutRequest) -> LayoutStatus {
        let requested_at_eviction_seq = request.requested_at_eviction_seq;
        let message_id: Arc<str> = Arc::clone(&request.message_id);
        let identity = LayoutIdentity::from_request(&request);
        let manager_epoch = self
            .manager_work
            .epoch_for(ManagerLayoutKey::from(&identity));
        let request_generation = self
            .next_request_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        let mut statuses = self.statuses.write_or_recover();
        if !self.manager_work.is_current(manager_epoch) {
            return LayoutStatus::Pending;
        }
        if let Some(entry) = statuses.get(&message_id)
            && entry.identity == identity
            && entry.manager_epoch == manager_epoch
        {
            match &entry.status {
                LayoutStatus::Ready(layout)
                    if !layout.is_stale_for_channel()
                        && entry.ready_layout_survives_evictions(layout) =>
                {
                    return entry.status.clone();
                }
                LayoutStatus::Computing => return entry.status.clone(),
                _ => {}
            }
        }

        let previous_entry = statuses.insert(
            Arc::clone(&message_id),
            LayoutStatusEntry {
                status: LayoutStatus::Computing,
                identity: identity.clone(),
                request_generation,
                manager_epoch,
                validated_eviction_seq: AtomicU64::new(0),
            },
        );
        self.inflight_counter.on_request();

        let job = LayoutJob {
            request,
            identity: identity.clone(),
            requested_at_eviction_seq,
            request_generation,
            manager_epoch,
            enqueued_at: Instant::now(),
            statuses: Arc::clone(&self.statuses),
            inflight_counter: Arc::clone(&self.inflight_counter),
            manager_work: Arc::clone(&self.manager_work),
            manager_lifetime: Arc::downgrade(&self.manager_lifetime),
            result_tx: self.result_tx.clone(),
            cleanup_status_on_drop: true,
        };

        match self.request_tx.try_send(job) {
            Ok(()) => {
                LAYOUT_ENQUEUED_TOTAL.fetch_add(1, Ordering::Relaxed);
                LAYOUT_QUEUE_PEAK_DEPTH.fetch_max(self.request_tx.len(), Ordering::Relaxed);
                LayoutStatus::Computing
            }
            Err(CbTrySendError::Full(job)) => {
                LAYOUT_QUEUE_FULL_TOTAL.fetch_add(1, Ordering::Relaxed);
                LAYOUT_QUEUE_PEAK_DEPTH.fetch_max(LAYOUT_QUEUE_CAPACITY, Ordering::Relaxed);
                if let Some(previous_entry) = previous_entry {
                    statuses.insert(message_id, previous_entry);
                } else {
                    statuses.remove(&message_id);
                }
                drop(statuses);
                drop(job);
                LayoutStatus::Pending
            }
            Err(CbTrySendError::Disconnected(job)) => {
                let disconnected =
                    LAYOUT_QUEUE_DISCONNECTED_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
                if disconnected == 1 {
                    crate::log_perf_render!(error, "Async layout worker queue disconnected");
                }
                if let Some(entry) = statuses.get_mut(&message_id) {
                    entry.status = LayoutStatus::Failed(());
                }
                drop(statuses);
                drop(job);
                LayoutStatus::Failed(())
            }
        }
    }

    /// Return an already-computing or ready status for the exact request identity.
    ///
    /// This lightweight preflight lets paint callers avoid cloning the complete message into a
    /// `LayoutRequest` on repeated frames. `request_layout` still performs the authoritative
    /// check under its write lock, so a result that changes immediately after this read remains
    /// race-safe.
    #[allow(clippy::too_many_arguments)]
    pub fn get_exact_status(
        &self,
        message_id: &str,
        quantized_width: u32,
        channel: &str,
        show_timestamp: bool,
        content_fingerprint: u64,
        layout_settings_version: u64,
        emote_resolution_version: u64,
        channel_emote_version: u64,
    ) -> LayoutStatus {
        let statuses = self.statuses.read_or_recover();
        let current_manager_epoch = self.manager_work.current_epoch.load(Ordering::Acquire);
        let current_cache_invalidation_version =
            crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch();
        statuses
            .get(message_id)
            .and_then(|entry| {
                let identity = &entry.identity;
                if entry.manager_epoch == current_manager_epoch
                    && identity.cache_invalidation_version == current_cache_invalidation_version
                    && identity.quantized_width == quantized_width
                    && identity.channel.as_ref() == channel
                    && identity.show_timestamp == show_timestamp
                    && identity.content_fingerprint == content_fingerprint
                    && identity.layout_settings_version == layout_settings_version
                    && identity.emote_resolution_version == emote_resolution_version
                    && identity.channel_emote_version == channel_emote_version
                    && identity.scale_bits == super::layout_scale::current_bits()
                    && identity.mod_buttons == super::layout_mode::mod_buttons()
                {
                    match &entry.status {
                        LayoutStatus::Ready(layout)
                            if layout.is_stale_for_channel()
                                || !entry.ready_layout_survives_evictions(layout) =>
                        {
                            None
                        }
                        LayoutStatus::Computing | LayoutStatus::Ready(_) => {
                            Some(entry.status.clone())
                        }
                        LayoutStatus::Pending | LayoutStatus::Failed(()) => None,
                    }
                } else {
                    None
                }
            })
            .unwrap_or(LayoutStatus::Pending)
    }

    /// Get layout status for a message
    pub fn get_status(
        &self,
        message_id: &str,
        show_timestamp: bool,
        content_fingerprint: u64,
        emote_resolution_version: u64,
        channel_emote_version: u64,
    ) -> LayoutStatus {
        let statuses = self.statuses.read_or_recover();
        let current_settings_version = settings::get_layout_settings_version();
        let current_manager_epoch = self.manager_work.current_epoch.load(Ordering::Acquire);
        let current_cache_invalidation_version =
            crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch();
        statuses
            .get(message_id)
            .and_then(|entry| {
                if entry.manager_epoch == current_manager_epoch
                    && entry.identity.cache_invalidation_version
                        == current_cache_invalidation_version
                    && entry.identity.layout_settings_version == current_settings_version
                    && entry.identity.show_timestamp == show_timestamp
                    && entry.identity.content_fingerprint == content_fingerprint
                    && entry.identity.emote_resolution_version == emote_resolution_version
                    && entry.identity.channel_emote_version == channel_emote_version
                    && entry.identity.scale_bits == super::layout_scale::current_bits()
                    && entry.identity.mod_buttons == super::layout_mode::mod_buttons()
                {
                    match &entry.status {
                        LayoutStatus::Ready(layout)
                            if layout.is_stale_for_channel()
                                || !entry.ready_layout_survives_evictions(layout) =>
                        {
                            None
                        }
                        _ => Some(entry.status.clone()),
                    }
                } else {
                    None
                }
            })
            .unwrap_or(LayoutStatus::Pending)
    }

    /// Get ready layout if available
    pub fn get_layout(
        &self,
        message_id: &str,
        show_timestamp: bool,
        content_fingerprint: u64,
        emote_resolution_version: u64,
        channel_emote_version: u64,
    ) -> Option<Arc<CanvasMessageLayout>> {
        let statuses = self.statuses.read_or_recover();
        let current_settings_version = settings::get_layout_settings_version();
        let current_manager_epoch = self.manager_work.current_epoch.load(Ordering::Acquire);
        let current_cache_invalidation_version =
            crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch();
        match statuses.get(message_id) {
            Some(
                entry @ LayoutStatusEntry {
                    status: LayoutStatus::Ready(layout),
                    identity,
                    manager_epoch,
                    ..
                },
            ) if *manager_epoch == current_manager_epoch
                && identity.cache_invalidation_version == current_cache_invalidation_version
                && identity.layout_settings_version == current_settings_version
                && identity.show_timestamp == show_timestamp
                && identity.content_fingerprint == content_fingerprint
                && identity.emote_resolution_version == emote_resolution_version
                && identity.channel_emote_version == channel_emote_version
                && identity.scale_bits == super::layout_scale::current_bits()
                && identity.mod_buttons == super::layout_mode::mod_buttons()
                && !layout.is_stale_for_channel()
                && entry.ready_layout_survives_evictions(layout) =>
            {
                Some(layout.clone())
            }
            _ => None,
        }
    }

    /// Get the *quantized width bucket* that was used to compute the layout.
    /// Returns None if layout is not Ready.
    ///
    /// Returns the bucket (u32) directly so callers compare against `LayoutCacheKey`
    /// buckets without re-quantizing a raw f32 (which could spuriously cross a boundary).
    pub fn get_layout_width(
        &self,
        message_id: &str,
        show_timestamp: bool,
        content_fingerprint: u64,
        emote_resolution_version: u64,
        channel_emote_version: u64,
    ) -> Option<u32> {
        let statuses = self.statuses.read_or_recover();
        let current_settings_version = settings::get_layout_settings_version();
        let current_manager_epoch = self.manager_work.current_epoch.load(Ordering::Acquire);
        let current_cache_invalidation_version =
            crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch();
        match statuses.get(message_id) {
            Some(
                entry @ LayoutStatusEntry {
                    status: LayoutStatus::Ready(layout),
                    identity,
                    manager_epoch,
                    ..
                },
            ) if *manager_epoch == current_manager_epoch
                && identity.cache_invalidation_version == current_cache_invalidation_version
                && identity.layout_settings_version == current_settings_version
                && identity.show_timestamp == show_timestamp
                && identity.content_fingerprint == content_fingerprint
                && identity.emote_resolution_version == emote_resolution_version
                && identity.channel_emote_version == channel_emote_version
                && identity.scale_bits == super::layout_scale::current_bits()
                && identity.mod_buttons == super::layout_mode::mod_buttons()
                && entry.ready_layout_survives_evictions(layout) =>
            {
                Some(identity.quantized_width)
            }
            _ => None,
        }
    }

    /// Poll for completed layouts (call from main thread)
    /// Returns Vec of completed message IDs
    pub fn poll_completed(&self) -> Vec<Arc<str>> {
        let mut completed = Vec::new();
        let result_rx = self.result_rx.lock_or_recover();

        while let Ok(result) = result_rx.try_recv() {
            completed.push(result.message_id);
        }

        if !completed.is_empty() {
            crate::log_perf_render!(debug, "Polled {} completed layouts", completed.len());
        }

        completed
    }

    /// Check if any admitted layouts are still queued or computing for this manager.
    ///
    /// PERF: Called every waker tick to decide whether another animation frame is
    /// needed. Reads a fast `AtomicUsize` counter instead of taking the `statuses`
    /// RwLock and doing an O(n) scan. A job-owned drop guard maintains the count across
    /// success, cancellation, queue rejection, and unwinding, while also cleaning up a
    /// still-owned Computing status after unwinding. Obsolete jobs are bounded and discarded
    /// before shaping.
    ///
    /// Note: `Pending` is never stored in `statuses` (entries are only ever
    /// `Computing`/`Ready`/`Failed`). The counter intentionally includes obsolete admitted
    /// jobs until workers dequeue them, avoiding races between separate epoch/count atomics.
    pub fn has_in_flight(&self) -> bool {
        self.inflight_counter.any()
    }

    /// Queue wait of this split's shaped jobs since the previous call.
    pub(crate) fn take_queue_wait(&self) -> SplitQueueWaitStats {
        self.manager_work.queue_wait.take()
    }

    /// Current number of tracked per-message status entries.
    ///
    /// Used by the append path to detect unbounded growth on live (append-only)
    /// channels, where the shrink/rotation-gated prune never fires.
    pub fn statuses_len(&self) -> usize {
        self.statuses.read_or_recover().len()
    }

    /// Drop status entries for `message_ids`, so a layout shaped before their
    /// decorations resolved is neither served nor published.
    pub fn forget_messages(&self, message_ids: &HashSet<Arc<str>>) {
        self.statuses
            .write_or_recover()
            .retain(|message_id, _| !message_ids.contains(message_id));
    }

    /// Prune per-message status entries that no longer correspond to active messages.
    ///
    /// Long-running sessions can rotate thousands of message IDs through the buffer.
    /// Without pruning, `statuses` keeps stale IDs indefinitely and inflates per-frame scans.
    pub fn prune_statuses_for_messages(&self, messages: &[Arc<ChatMessage>]) -> usize {
        let keep_ids: HashSet<&str> = messages.iter().map(|m| m.id.as_ref()).collect();
        let mut statuses = self.statuses.write_or_recover();
        let before = statuses.len();
        statuses.retain(|message_id, _| keep_ids.contains(message_id.as_ref()));
        let removed = before.saturating_sub(statuses.len());

        if removed > 0 {
            crate::log_perf_render!(
                trace,
                removed,
                remaining = statuses.len(),
                "Pruned stale async layout status entries"
            );
        }

        removed
    }
}

impl Default for AsyncLayoutManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn status_entry(
        status: LayoutStatus,
        width: f32,
        content_fingerprint: u64,
        settings_version: u64,
        emote_version: u64,
        channel_version: u64,
    ) -> LayoutStatusEntry {
        LayoutStatusEntry {
            status,
            identity: LayoutIdentity {
                quantized_width: quantize_width(width),
                content_fingerprint,
                layout_settings_version: settings_version,
                show_timestamp: true,
                cache_invalidation_version:
                    crate::components::chat::canvas_renderer::global_layout_cache::layout_content_epoch(),
                emote_resolution_version: emote_version,
                channel_emote_version: channel_version,
                channel: Arc::from("c"),
                scale_bits: 1.0f32.to_bits(),
                mod_buttons: false,
            },
            request_generation: 1,
            manager_epoch: 0,
            validated_eviction_seq: AtomicU64::new(
                crate::components::chat::canvas_renderer::global_layout_cache::layout_eviction_seq(),
            ),
        }
    }

    #[test]
    fn prune_statuses_keeps_only_active_message_ids() {
        let manager = AsyncLayoutManager::new();
        let settings_version = settings::get_layout_settings_version();
        let emote_version = crate::backend::slots::resolver::get_emote_version();
        let channel_version = crate::state::VersionRegistry::channel_emote_version("c");

        {
            let mut statuses = manager.statuses.write_or_recover();
            statuses.insert(
                Arc::from("keep"),
                status_entry(
                    LayoutStatus::Pending,
                    400.0,
                    1,
                    settings_version,
                    emote_version,
                    channel_version,
                ),
            );
            statuses.insert(
                Arc::from("drop"),
                status_entry(
                    LayoutStatus::Computing,
                    400.0,
                    2,
                    settings_version,
                    emote_version,
                    channel_version,
                ),
            );
        }

        let keep_message = ChatMessage {
            id: Arc::from("keep"),
            ..ChatMessage::default()
        };
        let removed = manager.prune_statuses_for_messages(&[Arc::new(keep_message)]);
        assert_eq!(removed, 1);

        let statuses = manager.statuses.read_or_recover();
        assert!(statuses.contains_key("keep"));
        assert!(!statuses.contains_key("drop"));
    }

    #[test]
    fn prune_statuses_clears_all_when_no_messages_remain() {
        let manager = AsyncLayoutManager::new();
        let settings_version = settings::get_layout_settings_version();
        let emote_version = crate::backend::slots::resolver::get_emote_version();
        let channel_version = crate::state::VersionRegistry::channel_emote_version("c");

        {
            let mut statuses = manager.statuses.write_or_recover();
            statuses.insert(
                Arc::from("old_1"),
                status_entry(
                    LayoutStatus::Pending,
                    300.0,
                    10,
                    settings_version,
                    emote_version,
                    channel_version,
                ),
            );
            statuses.insert(
                Arc::from("old_2"),
                status_entry(
                    LayoutStatus::Ready(Arc::new(CanvasMessageLayout::new(
                        "old_2".to_string(),
                        "c",
                    ))),
                    300.0,
                    20,
                    settings_version,
                    emote_version,
                    channel_version,
                ),
            );
        }

        let removed = manager.prune_statuses_for_messages(&[]);
        assert_eq!(removed, 2);
        assert!(manager.statuses.read_or_recover().is_empty());
    }

    #[test]
    fn inflight_deduplication_is_manager_local() {
        let first = AsyncLayoutManager::new();
        let second = AsyncLayoutManager::new();
        let settings_version = settings::get_layout_settings_version();
        let message = Arc::new(ChatMessage {
            id: Arc::from("shared-message-id"),
            structure_hash: 42,
            ..ChatMessage::default()
        });
        let request = LayoutRequest {
            message_id: Arc::from("shared-message-id"),
            message,
            channel: Arc::from("channel"),
            available_width: 420.0,
            show_timestamp: true,
            layout_settings_version: settings_version,
            emote_resolution_version: crate::backend::slots::resolver::get_emote_version(),
            channel_emote_version: crate::state::VersionRegistry::channel_emote_version("channel"),
            scale_factor: 1.0,
            mod_buttons: false,
            requested_at_eviction_seq:
                crate::components::chat::canvas_renderer::global_layout_cache::layout_eviction_seq(),
        };

        assert!(matches!(
            first.request_layout(request.clone()),
            LayoutStatus::Computing | LayoutStatus::Ready(_)
        ));
        assert!(matches!(
            second.request_layout(request),
            LayoutStatus::Computing | LayoutStatus::Ready(_)
        ));

        let second_statuses = second.statuses.read_or_recover();
        assert!(second_statuses.contains_key("shared-message-id"));
    }

    #[test]
    fn fenced_ready_layout_stays_fenced() {
        let layout = Arc::new(CanvasMessageLayout::new("fenced".to_string(), "c"));
        let entry = status_entry(LayoutStatus::Ready(Arc::clone(&layout)), 300.0, 1, 0, 0, 0);
        assert!(entry.ready_layout_survives_evictions(&layout));

        entry
            .validated_eviction_seq
            .store(EVICTION_FENCED, Ordering::Release);
        assert!(!entry.ready_layout_survives_evictions(&layout));
        assert_eq!(
            entry.validated_eviction_seq.load(Ordering::Acquire),
            EVICTION_FENCED
        );
    }
}
