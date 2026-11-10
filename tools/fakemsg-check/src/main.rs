#[path = "../../../src/backend/fakemsg_core.rs"]
mod fakemsg_core;

use fakemsg_core::{FakeIrcHighlight, parse_fake_irc_system_event};
use std::hint::black_box;
use std::time::Instant;

const ITERS: usize = 200_000;

fn main() {
    let samples = samples();
    for sample in &samples {
        let event = parse_fake_irc_system_event(sample.raw, "echo")
            .unwrap_or_else(|| panic!("sample should parse: {}", sample.name));
        assert_eq!(event.text, sample.text, "text mismatch for {}", sample.name);
        assert_eq!(
            event.highlight, sample.highlight,
            "highlight mismatch for {}",
            sample.name
        );
    }

    assert!(
        parse_fake_irc_system_event(
            ":user!user@user.tmi.twitch.tv PRIVMSG #echo :normal chat",
            "echo",
        )
        .is_none()
    );

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    bench_parse_and_convert(&samples);
}

struct Sample {
    name: &'static str,
    raw: &'static str,
    text: &'static str,
    highlight: FakeIrcHighlight,
}

fn samples() -> Vec<Sample> {
    vec![
        Sample {
            name: "notice",
            raw: "@msg-id=slow_on :tmi.twitch.tv NOTICE #echo :This room is now in slow mode.",
            text: "This room is now in slow mode.",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "usernotice-sub",
            raw: "@msg-id=sub;system-msg=SomeUser\\ssubscribed\\sat\\sTier\\s1 :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser subscribed at Tier 1",
            highlight: FakeIrcHighlight::Subscription,
        },
        Sample {
            name: "usernotice-giftpaidupgrade",
            raw: "@msg-id=giftpaidupgrade;system-msg=SomeUser\\sis\\scontinuing\\sthe\\sGift\\sSub\\sthey\\sgot\\sfrom\\sGifter :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser is continuing the Gift Sub they got from Gifter",
            highlight: FakeIrcHighlight::Subscription,
        },
        Sample {
            name: "usernotice-anongiftpaidupgrade",
            raw: "@msg-id=anongiftpaidupgrade;system-msg=SomeUser\\sis\\scontinuing\\sthe\\sGift\\sSub\\sthey\\sgot\\sfrom\\san\\sanonymous\\suser :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser is continuing the Gift Sub they got from an anonymous user",
            highlight: FakeIrcHighlight::Subscription,
        },
        Sample {
            name: "usernotice-primepaidupgrade",
            raw: "@msg-id=primepaidupgrade;system-msg=SomeUser\\sconverted\\sfrom\\sa\\sPrime\\ssub\\sto\\sa\\sTier\\s1\\ssub :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser converted from a Prime sub to a Tier 1 sub",
            highlight: FakeIrcHighlight::Subscription,
        },
        Sample {
            name: "usernotice-communitypayforward",
            raw: "@msg-id=communitypayforward;system-msg=SomeUser\\sis\\spaying\\sforward\\sthe\\sGift\\sthey\\sgot\\sfrom\\sGifter\\sto\\sthe\\scommunity :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser is paying forward the Gift they got from Gifter to the community",
            highlight: FakeIrcHighlight::Subscription,
        },
        Sample {
            name: "usernotice-bitsbadgetier",
            raw: "@msg-id=bitsbadgetier;msg-param-threshold=1000;system-msg=SomeUser\\searned\\sa\\s1,000\\sBits\\sbadge :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser earned a 1,000 Bits badge",
            highlight: FakeIrcHighlight::BitsBadge,
        },
        Sample {
            name: "usernotice-watch-streak",
            raw: "@msg-id=ritual;ritual-name=watch-streak;system-msg=SomeUser\\sis\\son\\sa\\s3-month\\swatch\\sstreak :tmi.twitch.tv USERNOTICE #echo",
            text: "SomeUser is on a 3-month watch streak",
            highlight: FakeIrcHighlight::WatchStreak,
        },
        Sample {
            name: "clearchat-timeout",
            raw: "@ban-duration=600 :tmi.twitch.tv CLEARCHAT #echo :baduser",
            text: "baduser timed out for 600 seconds",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "clearmsg",
            raw: "@login=baduser;target-msg-id=abc123 :tmi.twitch.tv CLEARMSG #echo :bad message",
            text: "Message from baduser was deleted (id: abc123)",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "roomstate",
            raw: "@emote-only=1;followers-only=10;r9k=0;slow=5;subs-only=1 :tmi.twitch.tv ROOMSTATE #echo",
            text: "Room state updated: emote-only=1, followers-only=10, r9k=0, slow=5, subs-only=1",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "globaluserstate",
            raw: "@display-name=EchoUser;color=#7f86ff :tmi.twitch.tv GLOBALUSERSTATE",
            text: "Global user state updated for EchoUser",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "hosttarget",
            raw: ":tmi.twitch.tv HOSTTARGET #echo :otherchannel 42",
            text: "Now hosting otherchannel for 42 viewers",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "reconnect",
            raw: ":tmi.twitch.tv RECONNECT",
            text: "Twitch requested a reconnect",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "ping",
            raw: "PING :tmi.twitch.tv",
            text: "Received server PING from tmi.twitch.tv",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "join",
            raw: ":someuser!someuser@someuser.tmi.twitch.tv JOIN #echo",
            text: "someuser joined #echo",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "part",
            raw: ":someuser!someuser@someuser.tmi.twitch.tv PART #echo",
            text: "someuser left #echo",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "cap",
            raw: ":tmi.twitch.tv CAP * ACK :twitch.tv/tags twitch.tv/commands",
            text: "Capability ACK: twitch.tv/tags twitch.tv/commands",
            highlight: FakeIrcHighlight::None,
        },
        Sample {
            name: "whisper",
            raw: "@display-name=SomeUser :someuser!someuser@someuser.tmi.twitch.tv WHISPER echo :hello there",
            text: "Whisper from SomeUser: hello there",
            highlight: FakeIrcHighlight::Whisper,
        },
        Sample {
            name: "numeric",
            raw: ":tmi.twitch.tv 001 echo :Welcome, GLHF!",
            text: "IRC 001: echo Welcome, GLHF!",
            highlight: FakeIrcHighlight::None,
        },
    ]
}

fn bench_parse_and_convert(samples: &[Sample]) {
    let start = Instant::now();
    let mut checksum = 0usize;
    for i in 0..ITERS {
        let sample = &samples[i % samples.len()];
        let event = parse_fake_irc_system_event(black_box(sample.raw), black_box("echo")).unwrap();
        checksum = checksum.wrapping_add(event.text.len() + event.highlight as usize);
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / ITERS as f64;
    let units_per_second = ITERS as f64 / elapsed.as_secs_f64();
    println!(
        "parse,fakemsg_server_events,{},{:.2},{:.0},{}",
        ITERS, ns_per_unit, units_per_second, checksum
    );
}
