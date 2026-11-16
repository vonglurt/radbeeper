// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// Made-up logs to look at the papers with: `cargo run --release --example
// sample_logs -- DIR [DAYS] [SERIAL]` fills DIR with DAYS days (45 unless
// said) of counts and temperatures ending now, in the real format, for a
// counter of that serial (the fake counter's unless said). The counts carry
// three things that are not decay -- something every twenty minutes, a daily
// rise and fall, and an hour's rhythm through one week -- and the
// temperatures one afternoon that was not like the others. See
// `radbeeper::sample`. Every row says `sample` where a place would be.
use radbeeper::{clock, sample};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first().map(std::path::PathBuf::from) else {
        eprintln!("usage: sample_logs DIR [DAYS] [SERIAL]");
        std::process::exit(2);
    };
    let days: f64 = args.get(1).and_then(|d| d.parse().ok()).unwrap_or(45.0);
    let serial = args.get(2).map(String::as_str).unwrap_or("123456789ABCDE");
    let _ = std::fs::create_dir_all(&dir);
    let now = clock::now();
    let begin = now - days * 86400.0;
    // The hourly week sits in the middle of whatever span was asked for.
    let week = (begin + days * 0.4 * 86400.0, begin + (days * 0.4 + 7.0).min(days) * 86400.0);
    sample::counts(&dir, serial, begin, now, 0.7,
                   &[(1200.0, 0.06), (86400.0, 0.05)], &[], 21);
    sample::counts(&dir, "5A4D504C453031", week.0, week.1, 0.7, &[(3600.0, 0.25)], &[], 22);
    let odd = clock::day_start(now - 4.0 * 86400.0) + 14.0 * 3600.0;
    sample::temps(&dir, serial, begin, now, 22.0, 2.4, &[(odd, odd + 3.0 * 3600.0, 4.0)], 23);
    println!("sample logs for {} in {}: {} days of counts and temperatures", serial, dir.display(), days);
}
