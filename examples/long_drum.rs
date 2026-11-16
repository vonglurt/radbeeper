// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// The long drums as plain text, from a log directory: `cargo run --release
// --example long_drum -- DIR [DAYS] [MONTHS]`. A line a day and a line a
// month, newest at the foot, the longest period at the left; a character is
// the loudest band that falls in its column, from `.` (what chance puts
// there) to `#` (sixteen times that and over). It is what the window and
// the monitor draw in ink, without either: for looking at a record over ssh,
// and for seeing what a change to `radbeeper::longdrum` did to real logs.
use radbeeper::longdrum::{rows, LongRow, Paper};
use radbeeper::clock;

fn strip(r: &Option<LongRow>, width: usize, long: f32, short: f32) -> String {
    let Some(r) = r else { return " ".repeat(width) };
    let mut cells = vec![0.0f32; width];
    for (p, v) in &r.bands {
        let x = ((long / p).ln() / (long / short).ln() * width as f32) as isize;
        if x >= 0 && (x as usize) < width {
            cells[x as usize] = cells[x as usize].max(*v);
        }
    }
    cells
        .iter()
        .map(|v| match *v {
            v if v <= 0.0 => ' ',
            v if v < 2.0 => '.',
            v if v < 4.0 => ':',
            v if v < 8.0 => '+',
            v if v < 16.0 => '*',
            _ => '#',
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::PathBuf::from(args.first().map(String::as_str).unwrap_or("."));
    let days: usize = args.get(1).and_then(|d| d.parse().ok()).unwrap_or(30);
    let months: usize = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(6);
    let now = clock::now();
    let t0 = std::time::Instant::now();
    println!("a line a day; 8 h at the left, 2 min at the right; cps and how much of the day was counted");
    for line in rows(&dir, Paper::Day, now, days).iter().rev() {
        match line {
            Some(r) => println!("{} |{}| {:.3} {:3.0}%{}", clock::format(r.start, "%d %b"),
                                strip(line, 100, 28800.0, 120.0), r.cps, r.covered * 100.0,
                                if r.whole { "" } else { "  so far" }),
            None => println!("       |{}|", strip(line, 100, 28800.0, 120.0)),
        }
    }
    println!("\na line a month; 10 d at the left, 20 min at the right");
    for line in rows(&dir, Paper::Month, now, months).iter().rev() {
        match line {
            Some(r) => println!("{} |{}| {:.3} {:3.0}%{}", clock::format(r.start, "%b %y"),
                                strip(line, 100, 10.0 * 86400.0, 1200.0), r.cps, r.covered * 100.0,
                                if r.whole { "" } else { "  so far" }),
            None => println!("       |{}|", strip(line, 100, 10.0 * 86400.0, 1200.0)),
        }
    }
    eprintln!("({:.1} s)", t0.elapsed().as_secs_f64());
}
