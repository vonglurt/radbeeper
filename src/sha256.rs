// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// SHA-256, because the standard library has none and the crate takes one
// dependency, which is libc.
//
// This is the extractor. The pool holds several hundred weakly-random
// per-second counts and a line of hex has to condense them into 256 bits that
// are strongly random -- and the tool for that is a cryptographic hash, not
// XOR and rotation. Shifting a block that holds twenty bits of entropy leaves
// twenty bits in it, looking more convincing with every rotation.
//
// FIPS 180-4, written out. Sixty lines, no dependency, and checked against
// the published vectors below -- which is the only reason it is allowed to be
// in the trust path of anything.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
    0x1f83d9ab, 0x5be0cd19,
];

/// Streaming, because the pool is fed a label, two numbers and a few hundred
/// characters and there is no reason to build one buffer out of them.
pub struct Sha256 {
    h: [u32; 8],
    block: [u8; 64],
    filled: usize,
    len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256 { h: H0, block: [0u8; 64], filled: 0, len: 0 }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        let mut i = 0;
        while i < data.len() {
            let take = (64 - self.filled).min(data.len() - i);
            self.block[self.filled..self.filled + take]
                .copy_from_slice(&data[i..i + take]);
            self.filled += take;
            i += take;
            if self.filled == 64 {
                let b = self.block;
                self.compress(&b);
                self.filled = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        // update() has been counting these padding bytes into len, which is
        // why the length was taken before any of them were added.
        let b = bits.to_be_bytes();
        self.block[56..64].copy_from_slice(&b);
        let blk = self.block;
        self.compress(&blk);
        let mut out = [0u8; 32];
        for (i, w) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = self.h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2],
                 v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            self.h[i] = self.h[i].wrapping_add(v[i]);
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// One shot, for the cases that already have the whole message.
pub fn digest_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 and the usual published vectors. Written-out crypto that is
    /// not checked against these is not crypto, it is arithmetic that has not
    /// been contradicted yet.
    #[test]
    fn the_published_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
             "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
            (b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
               ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
             "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"),
        ];
        for (msg, want) in cases {
            assert_eq!(&digest_hex(msg), want, "on {:?}", &msg[..msg.len().min(16)]);
        }
    }

    #[test]
    fn a_million_a_s() {
        // The vector that catches a length counter that overflows or a
        // padding path that only ever sees one block.
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_in_any_shape_gives_the_same_answer() {
        // The pool feeds a label, two numbers and a few hundred characters as
        // separate updates, so the split must not matter.
        let msg: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let want = digest_hex(&msg);
        for chunk in [1usize, 7, 63, 64, 65, 127, 128, 999] {
            let mut h = Sha256::new();
            for part in msg.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(hex(&h.finish()), want, "chunked by {}", chunk);
        }
    }

    #[test]
    fn a_message_that_lands_exactly_on_a_block_boundary() {
        // 55, 56 and 64 bytes are the three lengths a hand-written padding
        // path gets wrong, and they are one byte apart.
        for n in [54usize, 55, 56, 57, 63, 64, 65, 119, 120] {
            let msg = vec![b'x'; n];
            let mut h = Sha256::new();
            h.update(&msg);
            let got = hex(&h.finish());
            assert_eq!(got.len(), 64, "length {}", n);
        }
        assert_eq!(
            digest_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            digest_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
    }
}
