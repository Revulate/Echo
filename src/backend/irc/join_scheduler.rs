//! Process-wide pacing for Twitch IRC JOIN commands.
//!
//! `twitch-irc` pools sockets but deliberately leaves JOIN pacing to callers.
//! This transport keeps control and chat traffic on their normal socket path
//! while one actor releases JOINs across the pool under a sliding window kept
//! below Twitch's 20-per-10-seconds account limit. A normal-sized restore fits
//! in the window and joins at once; only larger ones are paced.

use async_trait::async_trait;
use futures_util::{Sink, SinkExt, Stream, StreamExt, stream::FusedStream, task::AtomicWaker};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;
use twitch_irc::message::IRCMessage;
use twitch_irc::transport::Transport;

type BaseTransport = super::super::proxy_transport::ProxySecureTCPTransport;

const JOIN_WINDOW: Duration = Duration::from_secs(10);
const JOIN_WINDOW_LIMIT: usize = 15;
const MAX_PENDING_JOINS: usize = 1_024;
const JOIN_REQUEST_CHANNEL_CAPACITY: usize = MAX_PENDING_JOINS / 2;

static NEXT_ENDPOINT_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_INTENT_EPOCH: AtomicU64 = AtomicU64::new(1);
static CHANNEL_INTENTS: LazyLock<Mutex<HashMap<String, Arc<ChannelIntent>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CONFIRMED_MEMBERSHIPS: LazyLock<Mutex<HashMap<String, ConfirmedMembership>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct ChannelIntent {
    epoch: u64,
    active: AtomicBool,
    latest_owner: AtomicU64,
}

impl ChannelIntent {
    fn is_current(&self, endpoint: &Endpoint) -> bool {
        self.active.load(Ordering::Acquire)
            && self.latest_owner.load(Ordering::Acquire) == endpoint.owner
            && endpoint.generation == super::CONNECTION_GENERATION.load(Ordering::Acquire)
    }
}

struct ConfirmedMembership {
    endpoint: Weak<Endpoint>,
    owner: u64,
    generation: u64,
    intent: Arc<ChannelIntent>,
    committed: bool,
}

/// Called under DESIRED_CHANNELS' write lock so close/reopen establishes a new epoch.
pub(super) fn open_channel_intent(channel: &str) {
    CHANNEL_INTENTS
        .lock()
        .entry(channel.to_owned())
        .or_insert_with(|| {
            Arc::new(ChannelIntent {
                epoch: NEXT_INTENT_EPOCH.fetch_add(1, Ordering::Relaxed),
                active: AtomicBool::new(true),
                latest_owner: AtomicU64::new(0),
            })
        });
}

pub(super) fn close_channel_intent(channel: &str) {
    let Some(intent) = CHANNEL_INTENTS.lock().remove(channel) else {
        return;
    };
    intent.active.store(false, Ordering::Release);
    let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
    if memberships
        .get(channel)
        .is_some_and(|member| Arc::ptr_eq(&member.intent, &intent))
    {
        memberships.remove(channel);
        super::SELF_JOINED_CHANNELS.write().remove(channel);
    }
}

pub(super) fn reset_confirmed_memberships() {
    let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
    memberships.clear();
    super::SELF_JOINED_CHANNELS.write().clear();
}

pub(super) fn commit_self_join(channel: &str) -> Option<bool> {
    let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
    let member = memberships.get_mut(channel)?;
    let endpoint = member.endpoint.upgrade()?;
    if member.committed || !endpoint.is_alive() || !member.intent.is_current(&endpoint) {
        return None;
    }
    member.committed = true;
    Some(
        super::SELF_JOINED_CHANNELS
            .write()
            .insert(channel.to_owned()),
    )
}

pub(super) fn is_join_confirmed(channel: &str) -> bool {
    let memberships = CONFIRMED_MEMBERSHIPS.lock();
    memberships.get(channel).is_some_and(|member| {
        member.endpoint.upgrade().is_some_and(|endpoint| {
            endpoint.is_alive()
                && member.intent.is_current(&endpoint)
                && super::SELF_JOINED_CHANNELS.read().contains(channel)
        })
    })
}

type IncomingItem = <<BaseTransport as Transport>::Incoming as Stream>::Item;

/// A TCP transport which queues only `JOIN`s behind the global Twitch budget.
pub struct JoinTransport {
    incoming: JoinIncoming,
    outgoing: JoinSink,
}

impl fmt::Debug for JoinTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinTransport").finish()
    }
}

#[async_trait]
impl Transport for JoinTransport {
    type ConnectError = <BaseTransport as Transport>::ConnectError;
    type IncomingError = <BaseTransport as Transport>::IncomingError;
    type OutgoingError = io::Error;
    type Incoming = JoinIncoming;
    type Outgoing = JoinSink;

    async fn new() -> Result<Self, Self::ConnectError> {
        // Keep this fence before the await: a connection created after a
        // supervisor restart must never dispatch that retired generation's JOINs.
        let generation = super::CONNECTION_GENERATION.load(Ordering::Acquire);
        let transport = BaseTransport::new().await?;
        let (incoming, outgoing) = transport.split();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let endpoint = Arc::new(Endpoint::new(generation, writer_tx));
        tokio::spawn(run_socket_writer(endpoint.clone(), outgoing, writer_rx));

        Ok(Self {
            incoming: JoinIncoming {
                inner: incoming,
                endpoint: endpoint.clone(),
            },
            outgoing: JoinSink {
                endpoint,
                normal_ack: None,
            },
        })
    }

    fn split(self) -> (Self::Incoming, Self::Outgoing) {
        (self.incoming, self.outgoing)
    }
}

struct Endpoint {
    owner: u64,
    generation: u64,
    writer: mpsc::UnboundedSender<WriterRequest>,
    closing: AtomicBool,
    failed: AtomicBool,
    incoming_closed: AtomicBool,
    failure_waker: AtomicWaker,
    cancel: Notify,
    login: Mutex<Option<String>>,
    owned_channels: Mutex<HashMap<String, Arc<ChannelIntent>>>,
}

impl Endpoint {
    fn new(generation: u64, writer: mpsc::UnboundedSender<WriterRequest>) -> Self {
        Self {
            owner: NEXT_ENDPOINT_OWNER.fetch_add(1, Ordering::Relaxed),
            generation,
            writer,
            closing: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            incoming_closed: AtomicBool::new(false),
            failure_waker: AtomicWaker::new(),
            cancel: Notify::new(),
            login: Mutex::new(None),
            owned_channels: Mutex::new(HashMap::new()),
        }
    }

    fn is_alive(&self) -> bool {
        !self.closing.load(Ordering::Acquire)
            && !self.failed.load(Ordering::Acquire)
            && !self.incoming_closed.load(Ordering::Acquire)
    }

    fn queue_normal(&self, message: IRCMessage) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        self.queue_write(message, None)
    }

    fn queue_write(
        &self,
        message: IRCMessage,
        join_intent: Option<Arc<ChannelIntent>>,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        if !self.is_alive() {
            return Err(disconnected_error());
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        let request = WriterRequest {
            message,
            join_intent,
            ack: Some(ack_tx),
        };
        self.writer
            .send(request)
            .map_err(|_| disconnected_error())?;
        Ok(ack_rx)
    }

    fn queue_join_from_arc(
        endpoint: &Arc<Self>,
        message: IRCMessage,
        channel: String,
    ) -> io::Result<()> {
        if !endpoint.is_alive()
            || endpoint.generation != super::CONNECTION_GENERATION.load(Ordering::Acquire)
        {
            return Err(disconnected_error());
        }
        let Some(intent) = CHANNEL_INTENTS.lock().get(&channel).cloned() else {
            return Ok(());
        };
        // Pool selection may reuse an older socket; admission order owns the channel.
        intent.latest_owner.store(endpoint.owner, Ordering::Release);
        if !intent.is_current(endpoint) {
            return Ok(());
        }
        join_sender()
            .try_send(JoinRequest {
                endpoint: Arc::downgrade(endpoint),
                generation: endpoint.generation,
                intent,
                channel,
                message,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(request) => {
                    crate::log_irc!(
                        warn,
                        "JOIN scheduler queue is full; refusing #{}",
                        request.channel
                    );
                    endpoint.fail();
                    io::Error::new(io::ErrorKind::WouldBlock, "JOIN scheduler queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => disconnected_error(),
            })
    }

    fn queue_due_join(
        &self,
        message: IRCMessage,
        intent: Arc<ChannelIntent>,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        self.queue_write(message, Some(intent))
    }

    fn record_join(&self, channel: &str, intent: Arc<ChannelIntent>) {
        self.owned_channels
            .lock()
            .insert(channel.to_string(), intent);
    }

    fn record_part(&self, channel: &str) {
        self.owned_channels.lock().remove(channel);
        let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
        if memberships.get(channel).is_some_and(|member| {
            member.owner == self.owner && member.generation == self.generation
        }) {
            memberships.remove(channel);
            super::SELF_JOINED_CHANNELS.write().remove(channel);
        }
    }

    fn observe_self_join(endpoint: &Arc<Self>, message: &IRCMessage) {
        let Some(channel) = join_channel(message) else {
            return;
        };
        let Some(twitch_irc::message::IRCPrefix::Full { nick, .. }) = &message.prefix else {
            return;
        };
        if !endpoint
            .login
            .lock()
            .as_ref()
            .is_some_and(|login| nick.eq_ignore_ascii_case(login))
        {
            return;
        }
        let Some(intent) = endpoint.owned_channels.lock().get(&channel).cloned() else {
            return;
        };
        let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
        if endpoint.is_alive() && intent.is_current(endpoint) {
            memberships.insert(
                channel,
                ConfirmedMembership {
                    endpoint: Arc::downgrade(endpoint),
                    owner: endpoint.owner,
                    generation: endpoint.generation,
                    intent,
                    committed: false,
                },
            );
        }
    }

    fn fail(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            self.failure_waker.wake();
            self.cancel.notify_one();
        }
    }

    fn close(&self) {
        if !self.closing.swap(true, Ordering::AcqRel) {
            self.failure_waker.wake();
            self.cancel.notify_one();
        }
    }

    fn incoming_ended(&self) {
        if self.incoming_closed.swap(true, Ordering::AcqRel)
            || super::CONNECTION_GENERATION.load(Ordering::Acquire) != self.generation
        {
            return;
        }

        let owned = std::mem::take(&mut *self.owned_channels.lock());
        if owned.is_empty() {
            return;
        }
        {
            let mut memberships = CONFIRMED_MEMBERSHIPS.lock();
            let mut self_joined = super::SELF_JOINED_CHANNELS.write();
            let disconnected: Vec<_> = owned
                .into_iter()
                .filter_map(|(channel, intent)| {
                    if super::CONNECTION_GENERATION.load(Ordering::Acquire) != self.generation
                        || !memberships.get(&channel).is_some_and(|member| {
                            member.owner == self.owner && member.generation == self.generation
                        })
                    {
                        return None;
                    }
                    memberships.remove(&channel);
                    let was_joined = self_joined.remove(&channel);
                    (was_joined && intent.active.load(Ordering::Acquire)).then_some(channel)
                })
                .collect();
            drop(self_joined);
            // Establish the gap before a replacement's self-JOIN can commit.
            for channel in disconnected {
                super::record_channel_disconnect(&channel);
            }
        }
    }
}

struct WriterRequest {
    message: IRCMessage,
    join_intent: Option<Arc<ChannelIntent>>,
    ack: Option<oneshot::Sender<io::Result<()>>>,
}

async fn run_socket_writer(
    endpoint: Arc<Endpoint>,
    mut sink: <BaseTransport as Transport>::Outgoing,
    mut requests: mpsc::UnboundedReceiver<WriterRequest>,
) {
    loop {
        if !endpoint.is_alive() {
            let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
            break;
        }
        let request = tokio::select! {
            _ = endpoint.cancel.notified() => {
                let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
                break;
            },
            request = requests.recv() => match request {
                Some(request) => request,
                None => break,
            },
        };
        if !endpoint.is_alive() {
            let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
            break;
        }
        let joined_channel = join_channel(&request.message);
        let WriterRequest {
            message,
            join_intent,
            ack,
        } = request;
        let result: io::Result<bool> = tokio::select! {
            _ = endpoint.cancel.notified() => break,
            result = async {
                futures_util::future::poll_fn(|cx| sink.poll_ready_unpin(cx)).await?;
                if let Some(channel) = joined_channel.as_deref() {
                    let Some(intent) = join_intent.filter(|intent| intent.is_current(&endpoint)) else {
                        crate::log_irc!(debug, channel = %channel, owner = endpoint.owner, "Discarded stale JOIN before socket write");
                        return Ok(false);
                    };
                    endpoint.record_join(channel, intent);
                }
                // No await between the final intent fence and handing bytes to the sink.
                sink.start_send_unpin(message)?;
                sink.flush().await?;
                Ok(true)
            } => result,
        };
        match result {
            Ok(written) => {
                if written && let Some(channel) = joined_channel {
                    crate::log_irc!(debug, channel = %channel, owner = endpoint.owner, "Paced IRC JOIN written");
                }
                if let Some(ack) = ack {
                    let _ = ack.send(Ok(()));
                }
            }
            Err(error) => {
                let message = error.to_string();
                if let Some(ack) = ack {
                    let _ = ack.send(Err(error));
                }
                crate::log_irc!(warn, "IRC socket write failed: {message}");
                endpoint.fail();
                break;
            }
        }
    }
}

pub struct JoinIncoming {
    inner: <BaseTransport as Transport>::Incoming,
    endpoint: Arc<Endpoint>,
}

impl Stream for JoinIncoming {
    type Item = IncomingItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.endpoint.failure_waker.register(cx.waker());
        if !this.endpoint.is_alive() {
            this.endpoint.incoming_ended();
            return Poll::Ready(None);
        }
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(message))) => {
                Endpoint::observe_self_join(&this.endpoint, &message);
                Poll::Ready(Some(Ok(message)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.endpoint.incoming_ended();
                this.endpoint.close();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.endpoint.incoming_ended();
                this.endpoint.close();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl FusedStream for JoinIncoming {
    fn is_terminated(&self) -> bool {
        !self.endpoint.is_alive() || self.inner.is_terminated()
    }
}

impl Drop for JoinIncoming {
    fn drop(&mut self) {
        self.endpoint.incoming_ended();
        self.endpoint.close();
    }
}

pub struct JoinSink {
    endpoint: Arc<Endpoint>,
    normal_ack: Option<oneshot::Receiver<io::Result<()>>>,
}

impl JoinSink {
    fn poll_normal_ack(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(ack) = self.normal_ack.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(ack).poll(cx) {
            Poll::Ready(Ok(result)) => {
                self.normal_ack = None;
                Poll::Ready(result)
            }
            Poll::Ready(Err(_)) => {
                self.normal_ack = None;
                Poll::Ready(Err(disconnected_error()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Sink<IRCMessage> for JoinSink {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.as_mut().get_mut().poll_normal_ack(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, message: IRCMessage) -> io::Result<()> {
        let this = self.as_mut().get_mut();
        if this.normal_ack.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "IRC socket writer has a pending message",
            ));
        }

        if let Some(channel) = join_channel(&message) {
            return Endpoint::queue_join_from_arc(&this.endpoint, message, channel);
        }
        if let Some(channel) = part_channel(&message) {
            this.endpoint.record_part(&channel);
        }
        if message.command.eq_ignore_ascii_case("NICK") {
            *this.endpoint.login.lock() = message.params.first().cloned();
        }
        this.normal_ack = Some(this.endpoint.queue_normal(message)?);
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.as_mut().get_mut().poll_normal_ack(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.as_mut().get_mut().poll_normal_ack(cx) {
            Poll::Ready(result) => {
                self.as_mut().get_mut().endpoint.close();
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for JoinSink {
    fn drop(&mut self) {
        self.endpoint.close();
    }
}

fn join_channel(message: &IRCMessage) -> Option<String> {
    message
        .command
        .eq_ignore_ascii_case("JOIN")
        .then(|| message.params.first())
        .flatten()
        .map(|channel| normalize_channel(channel))
        .filter(|channel| !channel.is_empty())
}

fn part_channel(message: &IRCMessage) -> Option<String> {
    message
        .command
        .eq_ignore_ascii_case("PART")
        .then(|| message.params.first())
        .flatten()
        .map(|channel| normalize_channel(channel))
        .filter(|channel| !channel.is_empty())
}

fn normalize_channel(channel: &str) -> String {
    channel.trim().trim_start_matches('#').to_lowercase()
}

fn disconnected_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "IRC socket is unavailable")
}

struct JoinRequest {
    endpoint: Weak<Endpoint>,
    generation: u64,
    intent: Arc<ChannelIntent>,
    channel: String,
    message: IRCMessage,
}

static JOIN_SENDER: OnceLock<mpsc::Sender<JoinRequest>> = OnceLock::new();

fn join_sender() -> &'static mpsc::Sender<JoinRequest> {
    JOIN_SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel(JOIN_REQUEST_CHANNEL_CAPACITY);
        tokio::spawn(run_join_scheduler(receiver));
        sender
    })
}

async fn run_join_scheduler(mut receiver: mpsc::Receiver<JoinRequest>) {
    let mut pending = VecDeque::new();
    let mut next_join_at = None;
    let mut recent_joins: VecDeque<Instant> = VecDeque::with_capacity(JOIN_WINDOW_LIMIT);

    loop {
        if pending.is_empty() {
            let Some(request) = receiver.recv().await else {
                return;
            };
            enqueue_join(&mut pending, request);
            continue;
        }

        if let Some(deadline) = next_join_at
            && Instant::now() < deadline
        {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => next_join_at = None,
                request = receiver.recv() => match request {
                    Some(request) => enqueue_join(&mut pending, request),
                    None => return,
                },
            }
            continue;
        }

        while let Ok(request) = receiver.try_recv() {
            enqueue_join(&mut pending, request);
        }

        let now = Instant::now();
        while recent_joins
            .front()
            .is_some_and(|sent| now.duration_since(*sent) >= JOIN_WINDOW)
        {
            recent_joins.pop_front();
        }
        if recent_joins.len() >= JOIN_WINDOW_LIMIT {
            next_join_at = recent_joins.front().map(|sent| *sent + JOIN_WINDOW);
            continue;
        }

        let priorities = channel_priorities();
        let Some(index) = next_pending_join(&pending, &priorities) else {
            pending.clear();
            continue;
        };
        let request = pending.remove(index).expect("pending join index is valid");

        let Some(endpoint) = request.endpoint.upgrade() else {
            continue;
        };
        if endpoint.generation != request.generation
            || super::CONNECTION_GENERATION.load(Ordering::Acquire) != request.generation
            || !request.intent.is_current(&endpoint)
            || !endpoint.is_alive()
        {
            continue;
        }

        match endpoint.queue_due_join(request.message, request.intent) {
            Ok(ack) => match ack.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    crate::log_irc!(warn, "JOIN write failed for #{}: {error}", request.channel);
                    endpoint.fail();
                }
                Err(_) => endpoint.fail(),
            },
            Err(error) => {
                crate::log_irc!(
                    warn,
                    "JOIN dispatch failed for #{}: {error}",
                    request.channel
                );
                endpoint.fail();
            }
        }
        // Failed writes count too: Twitch may still have seen the attempt.
        recent_joins.push_back(Instant::now());
    }
}

fn enqueue_join(pending: &mut VecDeque<JoinRequest>, request: JoinRequest) {
    if !request
        .endpoint
        .upgrade()
        .is_some_and(|endpoint| endpoint.is_alive() && request.intent.is_current(&endpoint))
    {
        return;
    }
    if let Some(index) = pending.iter().position(|pending_request| {
        pending_request.generation == request.generation
            && pending_request.channel == request.channel
    }) {
        // The library can move a channel while the old socket is still alive.
        crate::log_irc!(debug, channel = %request.channel, epoch = request.intent.epoch, "Superseded pending channel JOIN");
        pending[index] = request;
        return;
    }
    if pending.len() >= MAX_PENDING_JOINS {
        crate::log_irc!(
            warn,
            "JOIN scheduler pending queue is full; refusing #{}",
            request.channel
        );
        if let Some(endpoint) = request.endpoint.upgrade() {
            endpoint.fail();
        }
        return;
    }
    pending.push_back(request);
}

fn next_pending_join(
    pending: &VecDeque<JoinRequest>,
    priorities: &HashMap<String, u8>,
) -> Option<usize> {
    pending
        .iter()
        .enumerate()
        .filter(|(_, request)| {
            request.endpoint.upgrade().is_some_and(|endpoint| {
                endpoint.generation == request.generation
                    && super::CONNECTION_GENERATION.load(Ordering::Acquire) == request.generation
                    && request.intent.is_current(&endpoint)
                    && endpoint.is_alive()
            })
        })
        .min_by_key(|(_, request)| priorities.get(&request.channel).copied().unwrap_or(0))
        .map(|(index, _)| index)
}

#[derive(Default)]
struct PriorityCache {
    refreshed_at: Option<Instant>,
    priorities: HashMap<String, u8>,
}

static PRIORITY_CACHE: LazyLock<Mutex<PriorityCache>> =
    LazyLock::new(|| Mutex::new(PriorityCache::default()));

/// Sort startup JOIN candidates by the persisted visible workspace. The cache
/// refreshes at most once per second, so an unsaved layout change can take one
/// scheduler refresh to affect background JOIN ordering.
pub(super) fn prioritize_channels(channels: &mut [String]) {
    let priorities = channel_priorities();
    channels.sort_by_key(|channel| {
        priorities
            .get(&normalize_channel(channel))
            .copied()
            .unwrap_or(0)
    });
}

fn channel_priorities() -> HashMap<String, u8> {
    let now = Instant::now();
    let mut cache = PRIORITY_CACHE.lock();
    if cache
        .refreshed_at
        .is_some_and(|refreshed| now.duration_since(refreshed) < Duration::from_secs(1))
    {
        return cache.priorities.clone();
    }

    cache.priorities = read_persisted_priorities();
    cache.refreshed_at = Some(now);
    cache.priorities.clone()
}

fn read_persisted_priorities() -> HashMap<String, u8> {
    let mut priorities = HashMap::new();
    let Some(config_root) = crate::benchmark_profile::config_root() else {
        return priorities;
    };
    let tabs_path = config_root.join("layouts").join("tabs.json");
    let tabs: Vec<crate::config::TabDescriptor> = std::fs::read(&tabs_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let selected = tabs
        .iter()
        .position(|tab| tab.selected)
        .or_else(|| (!tabs.is_empty()).then_some(0));

    for (tab_index, tab) in tabs.iter().enumerate() {
        let channels = tab.layout.channels();
        let active = tab
            .layout
            .get_channel_at_path(&tab.active_split_path)
            .or_else(|| channels.first().cloned());
        for channel in channels {
            let is_active = active.as_ref() == Some(&channel);
            let Some(channel) = crate::backend::browser_watching::resolve_layout_channel(&channel)
            else {
                continue;
            };
            let channel = normalize_channel(&channel);
            if channel.is_empty() {
                continue;
            }
            let rank = if Some(tab_index) == selected && is_active {
                0
            } else if Some(tab_index) == selected {
                1
            } else if selected.is_some_and(|selected| selected.abs_diff(tab_index) == 1) {
                2
            } else {
                3
            };
            priorities
                .entry(channel)
                .and_modify(|existing| *existing = (*existing).min(rank))
                .or_insert(rank);
        }
    }

    for channel in crate::backend::settings::get_open_chat_popup_channels() {
        let channel = normalize_channel(&channel);
        if !channel.is_empty() {
            priorities
                .entry(channel)
                .and_modify(|existing| *existing = (*existing).min(1))
                .or_insert(1);
        }
    }
    priorities
}
