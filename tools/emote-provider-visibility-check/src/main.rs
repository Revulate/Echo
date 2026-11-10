use echo_emote_core::visibility::{
    EmoteProviderVisibility, EmoteProviderVisibilityKey, emote_provider_visible,
};

fn main() {
    let cases = [
        (
            "all_enabled",
            EmoteProviderVisibility::default(),
            [true, true, true, true, true],
        ),
        (
            "bttv_disabled",
            EmoteProviderVisibility {
                bttv: false,
                ..Default::default()
            },
            [true, true, false, true, true],
        ),
        (
            "ffz_disabled",
            EmoteProviderVisibility {
                ffz: false,
                ..Default::default()
            },
            [true, true, true, false, true],
        ),
        (
            "seventv_disabled",
            EmoteProviderVisibility {
                seventv: false,
                ..Default::default()
            },
            [true, false, true, true, true],
        ),
        (
            "third_party_disabled",
            EmoteProviderVisibility {
                seventv: false,
                bttv: false,
                ffz: false,
            },
            [true, false, false, false, true],
        ),
    ];

    let providers = [
        ("twitch", EmoteProviderVisibilityKey::Twitch),
        ("7tv", EmoteProviderVisibilityKey::SevenTv),
        ("bttv", EmoteProviderVisibilityKey::Bttv),
        ("ffz", EmoteProviderVisibilityKey::Ffz),
        ("emoji", EmoteProviderVisibilityKey::Emoji),
    ];

    println!("case,provider,visible");
    for (case_name, visibility, expected) in cases {
        for ((provider_name, provider), expected_visible) in providers.iter().zip(expected) {
            let visible = emote_provider_visible(*provider, visibility);
            assert_eq!(
                visible, expected_visible,
                "{case_name}/{provider_name} visibility mismatch"
            );
            println!("{case_name},{provider_name},{visible}");
        }
    }
}
