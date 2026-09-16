// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// What this crate would write, printed so something else can diff it.
//
// Reads a tiny script on stdin, one directive per line, and prints the result
// on stdout. tests/test_differential.py drives it with the same directives it
// gives the Python's own functions, and asserts the two outputs are identical
// characters -- not equivalent, identical.
//
//   g <float>                              Python's %g
//   header <span,span,...>                 the header line
//   stamp <unix seconds>                   the timestamp column
//   row <when> <cps> <counts> <seconds> <avg,..> <peak,..> <src> <site>
//        -- an empty average or peak is written as the word `none`
//   path <when> <serial>                   the file a row belongs in
//   slot <when> <every>                    the slot a time falls in
//   sha256 <ascii message>                 the digest of that message
//   digest <seq> <started> <packed counts> an emission's line
//   pack <count,count,...>                 counts as hex nibbles
//   mcv <count,count,...>                  measured min-entropy per sample
//   poisson <rate>                         what the model would have claimed
//   history <hex image>                    every decoded record, one per line
//   rows <hex image> <spans> <every> <max gap> <offset>
//                                          the log rows a backfill would fold in
use radbeeper::{clock, entropy, history, log, sha256};
use std::io::Read;

fn opts(text: &str) -> Vec<Option<f64>> {
    if text == "-" {
        return Vec::new();
    }
    text.split(',')
        .map(|p| if p == "none" { None } else { p.parse().ok() })
        .collect()
}

fn floats(text: &str) -> Vec<f64> {
    text.split(',').filter_map(|p| p.parse().ok()).collect()
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Tab-separated, because a site name has spaces in it.
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "g" => println!("{}", log::g(f[1].parse().unwrap())),
            "header" => println!("{}", log::header(&floats(f[1]))),
            "stamp" => println!("{}", clock::stamp(f[1].parse().unwrap())),
            "slot" => println!(
                "{}",
                log::slot_of(f[1].parse().unwrap(), f[2].parse().unwrap())
            ),
            "path" => println!(
                "{}",
                log::path(
                    f[1].parse().unwrap(),
                    std::path::Path::new(""),
                    Some(f[2])
                )
                .display()
            ),
            "row" => println!(
                "{}",
                log::row(
                    f[1].parse().unwrap(),
                    f[2].parse().unwrap(),
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    &opts(f[5]),
                    &opts(f[6]),
                    f[7],
                    f.get(8).copied().unwrap_or(""),
                )
            ),
            "sha256" => println!("{}", sha256::digest_hex(f[1].as_bytes())),
            "pack" => println!(
                "{}",
                entropy::pack_counts(
                    &f[1].split(',').filter_map(|p| p.parse().ok()).collect::<Vec<u32>>()
                )
            ),
            "mcv" => println!(
                "{:.12}",
                entropy::mcv_min_entropy(
                    &f[1].split(',').filter_map(|p| p.parse().ok()).collect::<Vec<u32>>()
                )
            ),
            "poisson" => println!(
                "{:.12}",
                entropy::poisson_min_entropy(f[1].parse().unwrap())
            ),
            "digest" => {
                let mut e = entropy::Entropy::default();
                e.started = Some(f[2].parse().unwrap());
                e.counts = entropy::unpack_counts(f[3]);
                e.total = e.counts.iter().map(|c| *c as u64).sum();
                println!("{}", e.digest(f[1].parse().unwrap()));
            }
            "history" => {
                let blob: Vec<u8> = f[1]
                    .as_bytes()
                    .chunks(2)
                    .map(|p| {
                        u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16)
                            .unwrap()
                    })
                    .collect();
                for r in history::records(&blob) {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        r.off,
                        r.when.map(|w| format!("{:.6}", w))
                            .unwrap_or_else(|| "-".into()),
                        r.dt.map(|d| format!("{:.9}", d))
                            .unwrap_or_else(|| "-".into()),
                        r.count.map(|c| c.to_string())
                            .unwrap_or_else(|| "-".into()),
                        r.note
                    );
                }
                println!("--");
            }
            "rows" => {
                let blob: Vec<u8> = f[1]
                    .as_bytes()
                    .chunks(2)
                    .map(|p| {
                        u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16)
                            .unwrap()
                    })
                    .collect();
                let spans = floats(f[2]);
                let mut s = history::samples(&blob, f[5].parse().unwrap());
                s.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let none = |_: f64| String::new();
                for (_when, line) in history::rows_from_samples(
                    &s, &spans, f[3].parse().unwrap(), f[4].parse().unwrap(),
                    log::SRC_FLASH, &none,
                ) {
                    println!("{}", line);
                }
                println!("--");
            }
            other => panic!("unknown directive {:?}", other),
        }
    }
}
