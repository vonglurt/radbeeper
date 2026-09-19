<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# A Signed Chain of Custody for a Single-Maintainer Rust Crate

**Paul Richeson** · RadBeeper · Copal Linux

---

## Abstract

*A hobbyist instrument that publishes a binary to strangers has the same
supply-chain obligations as one that does not, and fewer people to meet them.
This report describes the chain of custody RadBeeper uses to carry a
measurement from a counter on a desk to a crate on crates.io without a
long-lived credential existing anywhere along the way. The chain has four
links — a signed commit, a signed tag, a build pinned by content hash, and a
publish authenticated by a short-lived OIDC token — and each is verifiable by
a third party after the fact. We report the configuration in full, the three
defects found while establishing it, and, explicitly, what the arrangement
does not protect against. The development and build environment is a Copal
Linux guest, which is itself part of the claim: the machine that signs is a
machine whose contents are described.*

**Index Terms** — software supply chain, code signing, SSH signatures, OpenID
Connect, reproducible release, Rust, crates.io, provenance.

---

## I. Introduction

A release is a claim made to people who cannot inspect the claimant. When
`cargo install radbeeper` fetches a crate, the fetcher is trusting that the
bytes correspond to the source in a public repository, that the source was
written by the person named in it, and that nothing was substituted in
between. None of those is self-evident, and for a single-maintainer project
none is backed by an organisation.

The conventional answer is a published API token held in repository secrets.
That answer fails in a specific way: the token is long-lived, is valid from
anywhere, and its compromise is silent. It converts a supply-chain problem
into a credential-storage problem and then stores the credential in the thing
being defended.

This report describes what RadBeeper does instead. §II gives the threat model.
§III–§VI describe the four links. §VII reports three defects the process
exposed. §VIII states the limits honestly, because a security document that
claims only successes is advertising.

---

## II. Threat Model

We consider an adversary who can do one or more of the following:

| | Capability |
|---|---|
| **T1** | Push commits to the repository, as a stolen account or a compromised token |
| **T2** | Move a tag, or create one, to point at code the maintainer did not write |
| **T3** | Alter a GitHub Action the build depends on, by moving a mutable tag it references |
| **T4** | Obtain a credential from CI logs, repository secrets, or a developer machine |
| **T5** | Publish a crate version under the project's name |

We do **not** consider an adversary who controls the maintainer's machine
while it is signing, who controls GitHub's or crates.io's infrastructure, or
who can forge Ed25519. Those are out of scope and no configuration in this
repository addresses them.

The goal is not to make the above impossible. It is to make each of them
**leave evidence that a third party can find afterwards** — which is the only
property a single maintainer can actually deliver.

---

## III. Link One — The Signing Identity

### A. Why SSH and not GPG

RadBeeper signs with an Ed25519 SSH key rather than an OpenPGP key. The
reasoning is operational rather than cryptographic:

1. The key **already exists and is already trusted** for pushing to the
   remote. Introducing a second key introduces a second thing to lose.
2. `ssh-keygen -Y verify` needs no keyring daemon, no agent socket protocol,
   and no expiry management.
3. The failure modes are legible. A GPG signing failure in a release script is
   a famously opaque event.

### B. Configuration

```sh
git config gpg.format ssh
git config user.signingkey ~/.ssh/id_ed25519.pub
git config commit.gpgsign true
git config tag.gpgsign true
```

`commit.gpgsign` and `tag.gpgsign` are both set. Signing only commits is a
common and incomplete configuration: a tag is the object a release is built
from, so an unsigned tag leaves T2 unaddressed no matter how well the commits
are signed.

### C. Local verification

A signature nobody can check is a record, not a control. An allowed-signers
file makes verification local and offline:

```sh
printf '%s namespaces="git" %s\n' \
  "$(git config user.email)" "$(cut -d' ' -f1,2 ~/.ssh/id_ed25519.pub)" \
  > ~/.config/git/allowed_signers
git config gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers
```

`git log --format='%G?'` then reports `G` for a good signature rather than the
`N` it reports when there is nothing to check against. This distinction is
worth insisting on: without the allowed-signers file, `git` will happily
create signatures and never once tell you whether they verify.

### D. The identity binding, which is the part that fails

A signature proves possession of a key. It does not, by itself, bind that key
to a person. The forge performs that binding, and it requires **two**
registrations that are easy to conflate:

1. The public key registered as a **Signing Key** — a distinct entry from the
   Authentication Key, even when the bytes are identical.
2. The commit's author email registered **and verified** on the same account.

If either is missing the commit displays as unverified while being
cryptographically valid, which is the most confusing possible failure: the
local tooling reports success and the forge reports nothing wrong, only
absence. The verification state is machine-readable and should be checked
rather than eyeballed:

```sh
curl -s https://api.github.com/repos/OWNER/REPO/commits/SHA \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["commit"]["verification"])'
```

A `reason` of `valid` is the only acceptable answer. Anything else — `no_user`,
`unverified_email`, `unknown_key` — names the missing registration precisely.

### E. Vigilant mode

With vigilant mode enabled, the forge marks every *unsigned* commit attributed
to the author as such. This inverts the default: absence of a signature
becomes a visible claim rather than a silent one. It is the setting that makes
T1 expensive, because an adversary pushing as the maintainer now produces a
commit that is visibly *unlike* every other commit by that author.

---

## IV. Link Two — The Build Environment

The signing machine is a **Copal Linux guest**, a distillation of Alpine
Linux, running under UTM/QEMU. This is not incidental to the claim.

A signature attests that a key was present. It says nothing about what else
was present. A general-purpose desktop accumulated over years is a poor
answer to "what was on the machine that signed this"; a declaratively staged
distribution whose contents are enumerated in its own repository is a better
one. Copal carries RadBeeper as a stage and installs the `linux-lts` kernel
the counter requires, so the environment that builds the instrument is
described by the same kind of artefact as the instrument.

Two practical consequences observed:

- **The guest is reproducible; the host clipboard is not.** Moving a public
  key to the forge crosses the guest/host boundary through
  `copal-clip bridge` and `spice-vdagent`. This is a convenience path, not a
  trusted one, and nothing secret should cross it. Public keys may.
- **Busybox is not GNU.** `sed '0,/re/s|…|…|'` — the idiom for bounding a
  substitution to the first match — is accepted by busybox `sed`, matches
  nothing, and exits zero. A release script relying on it bumps no version and
  reports success. See §VII-B.

---

## V. Link Three — Pinning the Build

A workflow that references `actions/checkout@v4` is trusting whatever commit
that tag currently names. A tag is a mutable pointer in a repository the
project does not control; T3 is the act of moving one.

Every `uses:` in this repository is therefore pinned to a commit SHA, with the
human-readable version retained as a comment:

```yaml
- uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4
```

**A pin is only as good as the process that updates it.** An unmaintained pin
becomes an argument for un-pinning. Dependabot is configured for the
`github-actions` ecosystem alongside the two cargo manifests, so a pin advance
arrives as a reviewable diff rather than as a silent retag.

Token scope is declared per workflow rather than inherited. The default for a
repository that has never set one is read-and-write across the board; the
build workflow writes nothing and says so:

```yaml
permissions:
  contents: read
```

Only the page-building workflow holds `contents: write`, and it is the one
whose output is regenerated from files already in the repository.

---

## VI. Link Four — Publishing Without a Credential

The final link carries the greatest consequence: a published crate version is
permanent. It may be yanked — hidden from new resolution — but never replaced
and never deleted, and the name is never reused.

RadBeeper publishes over **OpenID Connect trusted publishing**. The workflow
exchanges a GitHub-issued identity token, scoped to this repository and this
workflow file, for a registry credential valid for the length of one job:

```yaml
permissions:
  contents: read
  id-token: write
```

The security property is the absence of an artefact. There is **no long-lived
token in the repository to leak, to rotate, or to forget to revoke.** T4
against this project yields nothing that is useful tomorrow. The registry-side
configuration names the repository owner, the repository, and the workflow
filename, so a token minted for a different workflow in the same repository
will not authenticate.

### A. Default-deny, and why the first three releases published nothing

The publish job is gated:

```yaml
crates-io:
  needs: [verify, binaries, release]
  if: vars.CRATES_IO_TRUSTED == 'true'
```

The variable ships unset. A tag pushed before the registry-side setup is
complete therefore builds binaries and cuts a forge release, and **skips** the
publish, rather than failing inside an authentication step whose error text
would send the maintainer looking for a credential problem that does not
exist.

This is correct behaviour and it is also a trap, which §VII-C records.

### B. Ordering

Publication is last, after the tag has been checked against the manifest and
after every binary has built, because it is the only step in the pipeline that
cannot be undone.

---

## VII. Defects Found While Establishing the Chain

A process is characterised by its failures. Three are worth recording.

### A. Version drift across two implementations

RadBeeper ships a Rust implementation and a single-file Python reference
implementation whose exported HTML is compared byte for byte. The release
target bumped three version strings and missed a fourth — the reference
program's own constant. Both programs write their version into that HTML, so a
one-line omission failed the entire differential suite, reporting an offset in
the middle of a generated document.

The repository already contained a test whose docstring predicted this
outcome, and it correctly identified the cause. The fix was threefold: bump
the fourth string; assert afterwards that all four landed; and **run the suite
a second time after the bump**. The pre-bump run cannot, structurally, observe
a bump that only reached some of its targets.

### B. A silent no-op in the version bump

The bump initially used `sed` without an address, which rewrites *every*
matching line. The GUI manifest contains two `version =` lines — the package's
and a dependency's — so the dependency was rewritten to the package's version.
The build then failed on an unknown *feature name*, with nothing in the error
pointing at the manifest that had been edited.

The GNU remedy, `sed '0,/re/s|…|…|'`, is silently inert under busybox (§IV).
Both were replaced with a purpose-built tool that edits only the first match,
refuses a non-semver argument, and refuses a manifest whose first `version =`
is not under `[package]`.

### C. A correct default, silently retained

The default-deny gate of §VI-A had never been switched on. Three tagged
releases built binaries, cut forge releases, and skipped publication. The
registry showed a single old version while the repository showed four tags,
and nothing anywhere was *failing*.

**A skipped job is not a red mark.** Default-deny is the right design, but a
default that is safe and invisible will be retained by accident. The
recommendation is to treat "did the publish job run?" as an explicit
post-release check rather than inferring it from the absence of failure.

### D. Advisory scope across two dependency surfaces

`cargo audit --deny warnings` applied uniformly failed the build on three
advisories in the window's 389-crate tree — two unmaintained crates and one
unsoundness — none of them vulnerabilities, all of them transitive, none
fixable in this repository.

Applying one standard to two surfaces produces a permanently red build, and a
permanently red build is an unread build. The crate proper, which resolves two
dependencies, is held to `--deny warnings` with nothing excused. The window
excuses advisories **by identifier**, each documented with its nature and
rationale, so that a *new* advisory still fails while a known and unfixable
one does not. This was verified by removing one identifier and confirming the
build returned to red.

---

## VIII. What This Does Not Establish

- **It is not reproducible-build provenance.** The chain attests who
  authorised a release and that the artefacts were built from that tag by that
  workflow. It does not let a third party rebuild the binary bit-for-bit and
  compare.
- **It does not protect a compromised signing machine.** A key present on a
  machine an adversary controls signs whatever that adversary asks.
- **It says nothing about correctness.** A signed, pinned, OIDC-published
  crate can be wrong. The differential suite and the advisory check are
  separate claims with separate evidence.
- **The entropy claims are separately scoped.** RadBeeper derives random
  numbers from decay timing; the accounting for that is a claim about a model
  of a physical source, not about this chain. See
  [Random numbers out of decay](the-random.md).
- **It is one person.** There is no second reviewer, no separation of duties,
  and no rota. The chain makes actions attributable; it does not make them
  reviewed.

---

## IX. Conclusion

The chain is four links, each verifiable after the fact by someone who trusts
neither the maintainer nor their machine: a signed commit, a signed tag, a
build pinned by content hash under least-privilege tokens, and a publication
authenticated by a credential that expires before it could be stolen.

The defects in §VII are more instructive than the design. Two of the three
were **silent successes** — a `sed` that changed nothing and exited zero, and
a job that skipped rather than failed. The signing and pinning arrangements
described here are routine; the discipline that matters is asserting
afterwards that each step did what it claimed, because the failures that
survive a review are exactly the ones that do not announce themselves.

---

## References

[1] RadBeeper, "Releasing," `RELEASING.md`, 2026. [Online]. Available:
    https://github.com/vonglurt/radbeeper

[2] RadBeeper, "Security," `SECURITY.md`, 2026.

[3] Copal Linux. [Online]. Available: https://github.com/vonglurt/copal

[4] GitHub, "About commit signature verification," GitHub Docs.

[5] Rust Foundation, "Trusted Publishing," crates.io Documentation.

[6] RustSec Advisory Database. [Online]. Available: https://rustsec.org

[7] OpenID Foundation, "OpenID Connect Core 1.0."

[8] RadBeeper, "Random numbers out of decay," `docs/the-random.md`, 2026.

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
