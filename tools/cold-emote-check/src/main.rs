//! No-send fresh-profile diagnostic for channel catalog publication.
use echo_gpui::backend::emotes::{
    EMOTE_CACHE, EmoteProvider, channel_catalog_ready_for_history, channel_has_lookup_entry,
    channel_initial_catalog_pending,
};
use std::time::{Duration, Instant};

fn main() {
    std::thread::Builder::new()
        .name("cold-emote-check".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    assert!(
        std::env::var_os("ECHO_BENCH_PROFILE_ROOT").is_some(),
        "Use an isolated profile"
    );
    echo_gpui::storage_roots::initialize()
        .and_then(|()| echo_gpui::bench::initialize())
        .expect("isolated profile");
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        !args.is_empty() && args.len().is_multiple_of(2),
        "Provide numeric-id/login pairs"
    );
    let channels: Vec<_> = args
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[id, login]| (id.clone(), login.clone()))
        .collect();
    let start = Instant::now();
    let runtime = echo_gpui::backend::bridge::get_runtime();
    let tasks: Vec<_> = channels
        .iter()
        .cloned()
        .map(|(id, channel)| {
            runtime.spawn(async move {
                let result = EMOTE_CACHE
                    .load_channel_emotes_reporting(&id, &channel, false)
                    .await;
                println!("channel={channel} load_result={result:?}");
            })
        })
        .collect();
    let mut last = vec![None; channels.len()];
    for _ in 0..180 {
        let mut complete = true;
        for (index, (_, channel)) in channels.iter().enumerate() {
            let emotes = EMOTE_CACHE.get_channel_emotes(channel);
            let seven = emotes
                .iter()
                .filter(|emote| emote.provider == EmoteProvider::SevenTV)
                .count();
            let state = (
                emotes.len(),
                seven,
                channel_has_lookup_entry(channel),
                channel_initial_catalog_pending(channel),
                channel_catalog_ready_for_history(channel),
            );
            if last[index] != Some(state) {
                println!(
                    "channel={channel} elapsed_ms={} total={} seventv={} lookup={} pending={} history_ready={}",
                    start.elapsed().as_millis(),
                    state.0,
                    state.1,
                    state.2,
                    state.3,
                    state.4
                );
                last[index] = Some(state);
            }
            complete &= seven > 0 && state.4 && tasks[index].is_finished();
        }
        if complete {
            for (_, channel) in &channels {
                let mut names: Vec<_> = EMOTE_CACHE
                    .get_channel_emotes(channel)
                    .into_iter()
                    .filter(|emote| {
                        emote.provider == EmoteProvider::SevenTV
                            && emote.name.len() > 1
                            && emote.name.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    })
                    .map(|emote| emote.name.clone())
                    .collect();
                names.sort();
                names.truncate(3);
                let text = names
                    .iter()
                    .map(|name| name.as_ref())
                    .collect::<Vec<_>>()
                    .join(" ");
                let slots = echo_gpui::backend::slots::parser::parse_to_slots(
                    &text,
                    channel,
                    echo_gpui::backend::emotes::EmoteSender::NONE,
                    &Default::default(),
                );
                let resolved = slots.iter().filter(|slot| matches!(slot,
                    echo_gpui::backend::slots::MessageSlot::Emote(emote) if emote.provider == EmoteProvider::SevenTV)).count();
                assert_eq!(
                    resolved,
                    names.len(),
                    "fresh catalog must resolve into real 7TV slots"
                );
                println!("channel={channel} parsed_7tv={resolved} probes={text}");
            }
            println!("PASS: fresh channels published 7TV before history-ready");
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("fresh channel load did not become ready within 45s");
}
