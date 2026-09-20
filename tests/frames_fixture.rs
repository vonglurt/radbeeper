// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// A committed frame file, and the reading of it that three implementations
// have to agree on.
//
// WHY A FIXTURE AND NOT A ROUND TRIP. src/entropy.rs already proves it can
// read what it writes, which is the one property an encoder can establish on
// its own and the one that matters least: a format that drifts drifts in both
// directions at once and round-trips happily the whole way. The bytes here
// were produced once, are checked in, and are decoded by
// src/frames.js (through tests/test_frames_js.py) as well as by this test. A
// change to the encoder that this fixture does not notice is a change the
// browser will not notice either -- until somebody opens a page written last
// month.
//
// TO REGENERATE, deliberately and with a reason:
//
//     RB_WRITE_FIXTURE=1 cargo test --test frames_fixture
//
// and say in the commit message why the bytes moved.
use radbeeper::entropy::{self, Entropy, Frame, GENESIS_LINK};
use std::fs;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Four emissions with every case the encoder has a branch for: an ordinary
/// second, a second over 0xFD, a gap, and a suspect spectrum.
fn built() -> Vec<Frame> {
    let mut frames = Vec::new();
    for seq in 0..4u64 {
        let mut pool = Entropy::default();
        let t0 = 1_700_000_000.0 + seq as f64 * 600.0;
        let mut when = t0;
        for i in 0..24u32 {
            // A hole in the middle of the third pool: the counter was away.
            if seq == 2 && i == 12 {
                when += 17.0;
            } else if i > 0 {
                when += 1.0;
            }
            // One second per pool carries more than a byte can hold inline.
            let counts = if i == 7 { 300 + seq as u32 } else { (seq + i as u64) as u32 % 7 };
            pool.add_at(when, counts);
        }
        frames.push(pool.frame(seq, seq == 3));
    }
    entropy::relink(&mut frames, GENESIS_LINK);
    frames
}

fn encoded(frames: &[Frame]) -> Vec<u8> {
    frames.iter().flat_map(|f| f.encode()).collect()
}

/// What the fixture means, in a form another language can check itself against.
fn described(frames: &[Frame]) -> String {
    let mut s = String::from("[\n");
    for (i, f) in frames.iter().enumerate() {
        let samples: Vec<String> = f
            .samples
            .iter()
            .map(|(g, c)| format!("[{},{}]", g, c))
            .collect();
        s.push_str(&format!(
            "  {{\"seq\":{},\"started\":{},\"suspect\":{},\"key\":\"{}\",\
             \"link\":\"{}\",\"samples\":[{}]}}{}\n",
            f.seq,
            f.started,
            f.suspect,
            f.key,
            f.link,
            samples.join(","),
            if i + 1 == frames.len() { "" } else { "," }
        ));
    }
    s.push_str("]\n");
    s
}

#[test]
fn the_committed_frames_are_the_frames_this_encoder_still_writes() {
    let frames = built();
    let bytes = encoded(&frames);
    let json = described(&frames);
    let bin = fixture("frames.bin");
    let meta = fixture("frames.json");

    if std::env::var("RB_WRITE_FIXTURE").is_ok() {
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, &bytes).unwrap();
        fs::write(&meta, &json).unwrap();
        eprintln!("wrote {} ({} bytes) and {}", bin.display(), bytes.len(), meta.display());
        return;
    }

    let on_disk = fs::read(&bin).unwrap_or_else(|_| {
        panic!("no {} -- regenerate with RB_WRITE_FIXTURE=1", bin.display())
    });
    assert_eq!(
        on_disk, bytes,
        "the encoder no longer writes the committed fixture. If that is \
         intended, regenerate with RB_WRITE_FIXTURE=1 and say why in the \
         commit; if it is not, a format change has escaped."
    );
    assert_eq!(fs::read_to_string(&meta).unwrap_or_default(), json);
}

/// And the reader gets back exactly what the writer put in.
#[test]
fn the_fixture_reads_back_as_itself() {
    let want = built();
    let got = entropy::read_frames(&fixture("frames.bin"));
    assert_eq!(got, want);
    assert!(got.iter().all(|f| f.verifies()), "a fixture frame stopped verifying");
    // The chain is continuous across all four.
    let mut prev = GENESIS_LINK.to_string();
    for f in &got {
        prev = entropy::chain(&prev, &f.key);
        assert_eq!(f.link, prev, "frame {} is off the chain", f.seq);
    }
}
