# Security

## Reporting something

**Open a [private security advisory](https://github.com/vonglurt/radbeeper/security/advisories/new)**
rather than an issue. If that is not available to you, email
`paulr@sdf.org` — and say "radbeeper" in the subject so it is not mistaken
for the rest of the post.

Expect an acknowledgement within a week. This is one person's project on one
desk; it is not a vendor with a rota, and pretending otherwise would be the
first dishonest thing on this page.

## What is in scope

RadBeeper reads a serial port, writes tab-separated files, serves a unix
socket and renders HTML. The things worth reporting:

| | |
|---|---|
| **The socket** | `broker.rs` serves a stream to anything that can open the socket. It is filesystem-permissioned and nothing more; a way to make a *client* affect what the server records would be a finding. |
| **The log reader** | A crafted `.tsv`, `.hex` or `.bin` that makes the export or the backfill do something other than refuse it. |
| **The exported HTML** | Log content becomes a web page. Anything in a log that escapes its cell and becomes markup is a finding, and the reason `esc()` exists. |
| **The frame decoder** | `entropy::Frame::decode` parses attacker-shaped bytes if somebody hands you a `.bin`. It is bounds-checked and fuzz-worthy; a panic is a bug and a read past the end is a finding. |

## What is not

- **The random numbers are not certified.** 256 bits of accounted min-entropy
  is a claim about a model of the source, not a proof about the output, and
  [the lab report](docs/the-random.md) says so at length. "It has not passed a
  test battery" is documented, not a vulnerability.
- **The dose figures are not a safety instrument.** This is a hobbyist counter
  on a desk. Do not make a decision about your health with it.
- **Requiring the `dialout` group** is the design. A program that reads
  `/dev/ttyUSB0` needs permission to, and this one deliberately does not want
  root.

## How the supply chain is kept small

- **The crate has one dependency and it is `libc`.** The manifest says so at
  length and the `include` list is explicit, so a stray file cannot reach a
  published `.crate`.
- **The reference implementation has none at all** — one file, the Python
  standard library, which is why it runs on a machine with no network.
- **Every `uses:` in CI is pinned to a commit SHA**, not a tag. A tag is a name
  somebody else can move.
- **Workflows declare least-privilege tokens**, and the only one that can write
  is the page builder.
- **crates.io is reached over OIDC trusted publishing.** There is no long-lived
  API token in this repository to leak or to rotate.
- **`cargo audit` runs on every CI run**, against the crate and the window
  alike.
- **Commits and tags are signed**, and the release tag is what the binaries and
  the published crate are built from.
