// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// The crate as a library, so that the parts with an opinion about a file
// format can be tested from outside the binary.
//
// WHY THIS EXISTS. The log format is owned by the Python program and being
// ported here one piece at a time, and the only thing that makes such a port
// safe is being able to run both against the same input and diff the bytes.
// That needs a Rust side something else can call: `examples/format_oracle.rs`
// prints what this crate would write, `tests/test_differential.py` prints what
// the Python would write, and the test is that they are the same characters.
pub mod analysis;
pub mod clock;
pub mod entropy;
pub mod log;
pub mod sha256;
