#![allow(dead_code)]

use echo_text_core::{SimilaritySettings, calculate_similarity, levenshtein_distance};

fn main() {
    println!("kind,name,value,checksum");

    print_fixture(
        "similarity_default_settings",
        &settings_summary(&SimilaritySettings::default()),
        "enabled=false,threshold=0.90,hide=false,color=true,same_user=false,delay=5,max=3",
    );
    print_fixture(
        "similarity_levenshtein_identical",
        &levenshtein_distance("hello", "hello").to_string(),
        "0",
    );
    print_fixture(
        "similarity_levenshtein_delete",
        &levenshtein_distance("hello", "helo").to_string(),
        "1",
    );
    print_fixture(
        "similarity_levenshtein_unicode",
        &levenshtein_distance("hello 👋", "hello 🙂").to_string(),
        "1",
    );
    print_fixture(
        "similarity_exact",
        &format!("{:.3}", calculate_similarity("hello", "hello")),
        "1.000",
    );
    print_fixture(
        "similarity_high",
        &format!("{:.3}", calculate_similarity("hello world", "helo world")),
        "0.909",
    );
    print_fixture(
        "similarity_medium",
        &format!("{:.3}", calculate_similarity("hello", "helo")),
        "0.800",
    );
    print_fixture(
        "similarity_low",
        &format!("{:.3}", calculate_similarity("hello", "xyz")),
        "0.000",
    );
    print_fixture(
        "similarity_empty_pair",
        &format!("{:.3}", calculate_similarity("", "")),
        "1.000",
    );
    print_fixture(
        "similarity_empty_single",
        &format!("{:.3}", calculate_similarity("", "hello")),
        "0.000",
    );
}

fn settings_summary(settings: &SimilaritySettings) -> String {
    format!(
        "enabled={},threshold={:.2},hide={},color={},same_user={},delay={},max={}",
        settings.enabled,
        settings.threshold,
        settings.hide_similar,
        settings.color_similar,
        settings.same_user_only,
        settings.max_delay_secs,
        settings.max_messages_to_check
    )
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},{actual},{}", checksum(actual));
}

fn checksum(value: &str) -> usize {
    value.bytes().map(usize::from).sum()
}
