fn main() {
    let result = echo_gpui::backend::emotes::benchmark_provider_index_refresh(41);
    println!(
        "entries={} iterations={}",
        result.entries, result.iterations
    );
    println!("equivalent={}", result.equivalent);
    println!("full_median_ns={}", result.full_median_ns);
    println!("incremental_median_ns={}", result.incremental_median_ns);
    println!("speedup={:.3}", result.speedup());
    let passed = result.equivalent && result.speedup() >= 2.0;
    println!("result={}", if passed { "PASS" } else { "FAIL" });
    if !passed {
        std::process::exit(1);
    }
}
