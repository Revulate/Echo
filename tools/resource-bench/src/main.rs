//! Benchmark: `MessageBuffer` insert throughput, estimated heap and RSS growth for
//! plain and rich messages, and (`--buffer-allocation-only`) the buffer's own
//! storage allocations, with FIFO-eviction assertions.

use echo_core::message::{ChatMessage, MessageFlags, ReplyInfo};
use echo_core::message_buffer::{MessageBuffer, MessageBufferPorts, install_message_buffer_ports};
use echo_core::slot::MessageSlot;
use smallvec::smallvec;
use std::alloc::{GlobalAlloc, Layout, System as StdSystem};
use std::hint::black_box;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

/// Counts requested allocation sizes that remain live. This deliberately
/// excludes allocator bookkeeping and page rounding, making before/after
/// buffer-storage comparisons repeatable across allocator implementations.
struct RequestedLiveBytesAllocator;

static REQUESTED_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL_ALLOCATOR: RequestedLiveBytesAllocator = RequestedLiveBytesAllocator;

unsafe impl GlobalAlloc for RequestedLiveBytesAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the exact layout supplied by the allocator caller.
        let pointer = unsafe { StdSystem.alloc(layout) };
        if !pointer.is_null() {
            REQUESTED_LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding the exact layout supplied by the allocator caller.
        let pointer = unsafe { StdSystem.alloc_zeroed(layout) };
        if !pointer.is_null() {
            REQUESTED_LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarding the exact pointer and layout supplied by the allocator caller.
        unsafe { StdSystem.dealloc(pointer, layout) };
        REQUESTED_LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarding the exact pointer, old layout, and requested size.
        let replacement = unsafe { StdSystem.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            match new_size.cmp(&layout.size()) {
                std::cmp::Ordering::Greater => {
                    REQUESTED_LIVE_BYTES.fetch_add(new_size - layout.size(), Ordering::Relaxed);
                }
                std::cmp::Ordering::Less => {
                    REQUESTED_LIVE_BYTES.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        replacement
    }
}

const BENCH_DEFAULT_CAPACITY: usize = 10_000;

fn main() {
    // A 10k default capacity, and no layout cache or embed state to reclaim on
    // eviction.
    install_message_buffer_ports(MessageBufferPorts {
        default_capacity: || BENCH_DEFAULT_CAPACITY,
        queue_evicted_id: |_| {},
        queue_evicted_ids: |_| {},
        prune_message_id: |_| {},
    });

    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--buffer-allocation-only")
    {
        run_buffer_allocation_only();
        return;
    }

    println!(
        "kind,name,input_messages,retained_messages,capacity,elapsed_ms,messages_per_second,estimated_bytes,estimated_bytes_per_message,rss_delta_bytes,rss_bytes,checksum"
    );

    run_insert_case("plain_10k", 10_000, 10_000, Workload::Plain);
    run_insert_case("rich_10k", 10_000, 10_000, Workload::Rich);
    run_insert_case("evict_rich_50k_to_5k", 50_000, 5_000, Workload::Rich);
}

fn requested_live_delta<T>(operation: impl FnOnce() -> T) -> (T, i128) {
    let before = REQUESTED_LIVE_BYTES.load(Ordering::Relaxed);
    let result = operation();
    let after = REQUESTED_LIVE_BYTES.load(Ordering::Relaxed);
    (result, after as i128 - before as i128)
}

/// GPUI-free storage baseline for the bounded chat buffer. Message payloads and
/// snapshots are deliberately prepared or released outside each measured
/// interval so rows report only the requested bytes retained by the operation.
fn run_buffer_allocation_only() {
    println!(
        "kind,name,message_count,logical_capacity,requested_live_delta_bytes,retained_messages,first_id,last_id,status"
    );

    run_new_buffer_allocation_case("new_1000", 1_000);
    run_new_buffer_allocation_case("new_10000", 10_000);
    for count in [0, 10, 100, 1_000] {
        run_quiet_buffer_allocation_case(count);
    }
    run_grow_shrink_allocation_case();
    assert_maximum_one_normalization();
}

fn run_new_buffer_allocation_case(name: &str, capacity: usize) {
    let (buffer, requested_live_delta_bytes) =
        requested_live_delta(|| MessageBuffer::new(capacity));
    assert_eq!(buffer.capacity(), capacity);
    assert!(buffer.is_empty());
    drop(buffer);

    println!("buffer_alloc,{name},0,{capacity},{requested_live_delta_bytes},0,,,pass");
}

fn run_quiet_buffer_allocation_case(count: usize) {
    // Prebuild payloads before measuring so this row isolates MessageBuffer
    // storage rather than string/Arc allocation for the messages themselves.
    let mut messages = quiet_messages(0, count);
    let (buffer, requested_live_delta_bytes) = requested_live_delta(|| {
        let mut buffer = MessageBuffer::new(1_000);
        for message in messages.drain(..) {
            buffer.push(message);
        }
        buffer
    });
    assert_fifo_ids(&buffer, 0, count);
    let first_id = buffer.get(0).map_or("", |message| message.id.as_ref());
    let last_id = buffer.latest_id().unwrap_or("");
    println!(
        "buffer_alloc,quiet_{count},{count},1000,{requested_live_delta_bytes},{count},{first_id},{last_id},pass"
    );
    drop(buffer);
    drop(messages);
}

fn run_grow_shrink_allocation_case() {
    let mut buffer = MessageBuffer::new(100);
    for message in quiet_messages(0, 10) {
        buffer.push(message);
    }
    assert_fifo_ids(&buffer, 0, 10);

    // Snapshots and the zebra comparison data are created outside measurement.
    let snapshot_before_grow = buffer.snapshot();
    let zebra_before_grow: Vec<bool> = buffer
        .iter()
        .map(|message| message.flags.is_alternate_row)
        .collect();
    let (_, grow_requested_live_delta_bytes) = requested_live_delta(|| {
        assert!(!buffer.set_capacity(1_000));
    });
    let snapshot_after_grow = buffer.snapshot();
    assert!(Arc::ptr_eq(&snapshot_before_grow, &snapshot_after_grow));
    assert_eq!(
        buffer
            .iter()
            .map(|message| message.flags.is_alternate_row)
            .collect::<Vec<_>>(),
        zebra_before_grow
    );
    assert_fifo_ids(&buffer, 0, 10);
    println!(
        "buffer_alloc,grow_100_to_1000,10,1000,{grow_requested_live_delta_bytes},10,quiet-0,quiet-9,pass"
    );

    let zebra_before_shrink: Vec<bool> = buffer
        .iter()
        .skip(5)
        .map(|message| message.flags.is_alternate_row)
        .collect();
    let (_, shrink_requested_live_delta_bytes) = requested_live_delta(|| {
        assert!(buffer.set_capacity(5));
    });
    let snapshot_after_shrink = buffer.snapshot();
    assert!(!Arc::ptr_eq(&snapshot_before_grow, &snapshot_after_shrink));
    assert_eq!(
        buffer
            .iter()
            .map(|message| message.flags.is_alternate_row)
            .collect::<Vec<_>>(),
        zebra_before_shrink
    );
    assert_fifo_ids(&buffer, 5, 5);
    println!(
        "buffer_alloc,shrink_1000_to_5,10,5,{shrink_requested_live_delta_bytes},5,quiet-5,quiet-9,pass"
    );

    // Refill past the physical end to preserve FIFO behavior through wraparound.
    for message in quiet_messages(10, 5) {
        buffer.push(message);
    }
    assert_fifo_ids(&buffer, 10, 5);

    // All snapshot references are released outside the captured intervals.
    drop(snapshot_after_shrink);
    drop(snapshot_after_grow);
    drop(snapshot_before_grow);
    drop(buffer);

    let mut full_buffer = MessageBuffer::new(1_000);
    for message in quiet_messages(0, 1_000) {
        full_buffer.push(message);
    }
    assert_fifo_ids(&full_buffer, 0, 1_000);
    for message in quiet_messages(1_000, 4_096) {
        full_buffer.push(message);
    }
    assert_fifo_ids(&full_buffer, 4_096, 1_000);
    println!("buffer_assert,full_1000_wraparound,5096,1000,0,1000,quiet-4096,quiet-5095,pass");
    drop(full_buffer);
}

fn assert_maximum_one_normalization() {
    let mut buffer = MessageBuffer::new(0);
    assert_eq!(buffer.capacity(), 1);
    buffer.push(quiet_messages(0, 1).pop().expect("one quiet message"));
    assert!(!buffer.set_capacity(0));
    assert_eq!(buffer.capacity(), 1);
    assert_fifo_ids(&buffer, 0, 1);
    println!("buffer_assert,max1_normalization,1,1,0,1,quiet-0,quiet-0,pass");
}

fn quiet_messages(start: usize, count: usize) -> Vec<ChatMessage> {
    (start..start + count)
        .map(|index| ChatMessage {
            id: Arc::from(format!("quiet-{index}")),
            ..Default::default()
        })
        .collect()
}

fn assert_fifo_ids(buffer: &MessageBuffer, first_index: usize, count: usize) {
    assert_eq!(buffer.len(), count);
    for (offset, message) in buffer.iter().enumerate() {
        assert_eq!(
            message.id.as_ref(),
            format!("quiet-{}", first_index + offset)
        );
    }
}

#[derive(Clone, Copy)]
enum Workload {
    Plain,
    Rich,
}

fn run_insert_case(name: &str, input_messages: usize, capacity: usize, workload: Workload) {
    let mut system = System::new().expect("sysinfo supports this platform");
    let rss_before = current_rss_bytes(&mut system);
    let start = Instant::now();
    let mut buffer = MessageBuffer::new(capacity);
    let mut checksum = 0usize;

    for index in 0..input_messages {
        let message = make_message(index, workload);
        checksum = checksum.wrapping_add(message.content.len());
        buffer.push(black_box(message));
    }

    let elapsed = start.elapsed();
    let retained_messages = buffer.len();
    let estimated_bytes = estimate_buffer_heap_bytes(&buffer);
    let estimated_bytes_per_message = estimated_bytes.checked_div(retained_messages).unwrap_or(0);
    let rss_after = current_rss_bytes(&mut system);
    let rss_delta_bytes = rss_after.saturating_sub(rss_before);
    let messages_per_second = input_messages as f64 / elapsed.as_secs_f64();

    println!(
        "insert,{name},{input_messages},{retained_messages},{capacity},{:.3},{:.0},{estimated_bytes},{estimated_bytes_per_message},{rss_delta_bytes},{rss_after},{checksum}",
        elapsed.as_secs_f64() * 1000.0,
        messages_per_second
    );

    black_box(buffer);
}

/// Tool-local heuristic estimate of the buffer's heap footprint.
///
/// `MessageBuffer::estimated_heap_bytes` was removed from src with no
/// replacement, so this bench reimplements the estimate here. It walks each
/// retained `ChatMessage` and sums the heap behind its `Arc<str>` string
/// fields (length only — Arc control-block overhead is ignored), the badge
/// strings, the resolved message slots (via the local `MessageSlot`
/// `estimated_heap_bytes()` stub), and the optional reply metadata. This is an
/// approximation for relative comparison across workloads, not an exact RSS
/// accounting.
fn estimate_buffer_heap_bytes(buffer: &MessageBuffer) -> usize {
    buffer.iter().map(estimate_message_heap_bytes).sum()
}

fn estimate_message_heap_bytes(msg: &ChatMessage) -> usize {
    let mut bytes = 0usize;

    // Owned string fields (Arc<str> backing storage).
    bytes += msg.id.len();
    bytes += msg.channel.len();
    bytes += msg.sender.len();
    bytes += msg.display_name.len();
    bytes += msg.content.len();
    if let Some(color) = msg.color.as_ref() {
        bytes += color.len();
    }
    if let Some(tag) = msg.twitch_emotes_tag.as_ref() {
        bytes += tag.len();
    }
    if let Some(user_id) = msg.user_id.as_ref() {
        bytes += user_id.len();
    }

    // Badge strings plus the SmallVec heap spill.
    for badge in msg.badges.iter() {
        bytes += badge.len();
    }

    // Resolved message slots: defer to the local MessageSlot heuristic.
    for slot in msg.slots.iter() {
        bytes += estimated_slot_heap_bytes(slot);
        bytes += std::mem::size_of::<MessageSlot>();
    }

    // Reply metadata strings.
    if let Some(reply) = msg.reply.as_ref() {
        bytes += reply.parent_msg_id.len();
        bytes += reply.parent_user_login.len();
        bytes += reply.parent_display_name.len();
        if let Some(body) = reply.parent_msg_body.as_ref() {
            bytes += body.len();
        }
    }

    bytes
}

fn estimated_slot_heap_bytes(slot: &MessageSlot) -> usize {
    match slot {
        MessageSlot::Text(text) => estimated_arc_str_heap(text),
        MessageSlot::Emote(_) => 0,
        MessageSlot::TwitchEmote { id, name } => {
            estimated_arc_str_heap(id) + estimated_arc_str_heap(name)
        }
        MessageSlot::Url { url, .. } => estimated_arc_str_heap(url),
        MessageSlot::Mention { username } => estimated_arc_str_heap(username),
        MessageSlot::Emoji { emoji } => estimated_arc_str_heap(emoji),
        MessageSlot::TwitchGif(gif) => {
            estimated_arc_str_heap(&gif.id)
                + estimated_arc_str_heap(&gif.url)
                + estimated_arc_str_heap(&gif.alt_text)
        }
    }
}

fn estimated_arc_str_heap(value: &Arc<str>) -> usize {
    std::mem::size_of::<usize>() * 2 + value.len()
}

fn make_message(index: usize, workload: Workload) -> ChatMessage {
    match workload {
        Workload::Plain => make_plain_message(index),
        Workload::Rich => make_rich_message(index),
    }
}

fn make_plain_message(index: usize) -> ChatMessage {
    let content = format!("plain chat line {index} with short text");
    let slots = Arc::<[MessageSlot]>::from(vec![MessageSlot::text(content.clone())]);

    ChatMessage {
        id: Arc::from(format!("msg-{index}")),
        channel: Arc::from("bench_channel"),
        sender: Arc::from(format!("user{}", index % 250)),
        display_name: Arc::from(format!("User{}", index % 250)),
        content: Arc::from(content),
        color: Some(Arc::from("#9147FF")),
        badges: smallvec![Arc::from("subscriber:12")],
        timestamp: index as i64,
        slots,
        structure_hash: index as u64,
        user_id: Some(Arc::from(format!("{}", 10_000 + index))),
        ..Default::default()
    }
}

fn make_rich_message(index: usize) -> ChatMessage {
    let content = format!(
        "@viewer{} check https://example.com/{index} Kappa 😀 this is a richer twitch message with reply metadata",
        index % 500
    );
    let slots = Arc::<[MessageSlot]>::from(vec![
        MessageSlot::mention(format!("viewer{}", index % 500)),
        MessageSlot::text(" check "),
        MessageSlot::Url {
            url: Arc::from(format!("https://example.com/{index}")),
            embed_id: index as u32,
        },
        MessageSlot::text(" "),
        MessageSlot::twitch_emote("Kappa", "25"),
        MessageSlot::text(" "),
        MessageSlot::emoji("😀"),
        MessageSlot::text(" this is a richer twitch message with reply metadata"),
    ]);

    ChatMessage {
        id: Arc::from(format!("rich-msg-{index}")),
        channel: Arc::from("bench_channel"),
        sender: Arc::from(format!("rich_user{}", index % 500)),
        display_name: Arc::from(format!("RichUser{}", index % 500)),
        content: Arc::from(content),
        color: Some(Arc::from("#00AEEF")),
        badges: smallvec![
            Arc::from("moderator:1"),
            Arc::from("subscriber:24"),
            Arc::from("bits:1000")
        ],
        timestamp: index as i64,
        slots,
        structure_hash: index as u64,
        reply: Some(ReplyInfo {
            parent_msg_id: Arc::from(format!("parent-{index}")),
            parent_user_login: Arc::from("parent_user"),
            parent_display_name: Arc::from("ParentUser"),
            parent_user_id: None,
            parent_msg_body: Some(Arc::from("original message body for reply preview")),
        }),
        flags: MessageFlags {
            is_first_message: index.is_multiple_of(97),
            is_shared_chat: index.is_multiple_of(101),
            ..Default::default()
        },
        twitch_emotes_tag: Some(Arc::from("25:37-41")),
        user_id: Some(Arc::from(format!("{}", 20_000 + index))),
        ..Default::default()
    }
}

fn current_rss_bytes(system: &mut System) -> u64 {
    let Ok(pid) = get_current_pid() else {
        return 0;
    };

    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .map(|process| process.memory())
        .unwrap_or(0)
}
