use echo_cache_policy::preload_plan::{
    EAGER_CHANNEL_PRELOAD_LIMIT, MAX_CHANNEL_PRELOAD_URLS, plan_channel_preload,
    summarize_channel_preload_plans,
};
use std::io::{self, Write};

fn write_report(mut output: impl Write) -> io::Result<()> {
    let cases = [
        (0, 0, 0, 0, 0),
        (1, 1, 1, 0, 0),
        (
            EAGER_CHANNEL_PRELOAD_LIMIT,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            0,
            0,
        ),
        (
            EAGER_CHANNEL_PRELOAD_LIMIT + 1,
            EAGER_CHANNEL_PRELOAD_LIMIT + 1,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            1,
            0,
        ),
        (
            MAX_CHANNEL_PRELOAD_URLS,
            MAX_CHANNEL_PRELOAD_URLS,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            MAX_CHANNEL_PRELOAD_URLS - EAGER_CHANNEL_PRELOAD_LIMIT,
            0,
        ),
        (
            MAX_CHANNEL_PRELOAD_URLS + 1,
            MAX_CHANNEL_PRELOAD_URLS,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            MAX_CHANNEL_PRELOAD_URLS - EAGER_CHANNEL_PRELOAD_LIMIT,
            1,
        ),
        (
            4_414,
            MAX_CHANNEL_PRELOAD_URLS,
            EAGER_CHANNEL_PRELOAD_LIMIT,
            MAX_CHANNEL_PRELOAD_URLS - EAGER_CHANNEL_PRELOAD_LIMIT,
            4_414 - MAX_CHANNEL_PRELOAD_URLS,
        ),
    ];
    writeln!(
        output,
        "kind,channels,discovered,queued,eager,lazy,truncated"
    )?;
    for (discovered, queued, eager, lazy, truncated) in cases {
        let plan = plan_channel_preload(discovered);
        assert_eq!(
            (plan.queued, plan.eager, plan.lazy, plan.truncated),
            (queued, eager, lazy, truncated)
        );
        writeln!(
            output,
            "channel_preload,1,{},{},{},{},{}",
            plan.discovered, plan.queued, plan.eager, plan.lazy, plan.truncated
        )?;
    }
    let multi = [
        ("empty", vec![], 0, 0, 0, 0, 0, 0),
        (
            "active_pair",
            vec![1, EAGER_CHANNEL_PRELOAD_LIMIT + 1],
            2,
            EAGER_CHANNEL_PRELOAD_LIMIT + 2,
            EAGER_CHANNEL_PRELOAD_LIMIT + 2,
            EAGER_CHANNEL_PRELOAD_LIMIT + 1,
            1,
            0,
        ),
        (
            "six_heavy_channels",
            vec![4_414; 6],
            6,
            4_414 * 6,
            MAX_CHANNEL_PRELOAD_URLS * 6,
            EAGER_CHANNEL_PRELOAD_LIMIT * 6,
            (MAX_CHANNEL_PRELOAD_URLS - EAGER_CHANNEL_PRELOAD_LIMIT) * 6,
            (4_414 - MAX_CHANNEL_PRELOAD_URLS) * 6,
        ),
    ];
    for (label, counts, channels, discovered, queued, eager, lazy, truncated) in multi {
        let summary = summarize_channel_preload_plans(counts.into_iter().map(plan_channel_preload));
        assert_eq!(
            (
                summary.channels,
                summary.discovered,
                summary.queued,
                summary.eager,
                summary.lazy,
                summary.truncated
            ),
            (channels, discovered, queued, eager, lazy, truncated)
        );
        writeln!(
            output,
            "multi_channel_preload_{},{},{},{},{},{},{}",
            label,
            summary.channels,
            summary.discovered,
            summary.queued,
            summary.eager,
            summary.lazy,
            summary.truncated
        )?;
    }
    Ok(())
}

pub fn run(args: &[String], output: impl Write) -> Result<(), String> {
    if !args.is_empty() {
        return Err("cache-preload-check takes no arguments".into());
    }
    write_report(output).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn no_argument_output_is_byte_exact() {
        let mut bytes = Vec::new();
        write_report(&mut bytes).unwrap();
        let expected = "kind,channels,discovered,queued,eager,lazy,truncated\nchannel_preload,1,0,0,0,0,0\nchannel_preload,1,1,1,1,0,0\nchannel_preload,1,4,4,4,0,0\nchannel_preload,1,5,5,4,1,0\nchannel_preload,1,24,24,4,20,0\nchannel_preload,1,25,24,4,20,1\nchannel_preload,1,4414,24,4,20,4390\nmulti_channel_preload_empty,0,0,0,0,0,0\nmulti_channel_preload_active_pair,2,6,6,5,1,0\nmulti_channel_preload_six_heavy_channels,6,26484,144,24,120,26340\n";
        assert_eq!(bytes, expected.as_bytes());
    }
}
