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
use radbeeper::{clock, log};
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
            other => panic!("unknown directive {:?}", other),
        }
    }
}
