# Releasing

Two things ship from a tag: **static Linux binaries** on the GitHub release,
and **the crate** on crates.io so that `cargo install radbeeper` works. Both
come out of `.github/workflows/release.yml`, which fires on a `v*` tag and
nothing else.

## The one-time crates.io setup

Do this once, by hand. After it, every release is a tag.

1. **An account with a verified email.** Sign in at
   [crates.io](https://crates.io/) with GitHub, then verify your address on
   [Account Settings](https://crates.io/settings/profile). crates.io will not
   let an unverified account publish.

2. **Claim the name.** As of writing, `radbeeper` is unregistered — crates.io
   is first come, first served and names are never reused, so the name is
   only yours once a version exists under it. From a clean tree:

   ```sh
   make rust-publish-dry            # everything but the upload
   cargo login                      # paste a token from crates.io/settings/tokens
   cd rust && cargo publish --locked
   ```

   That first publish is manual on purpose: it is the only one that needs a
   token on your machine, and it is what creates the crate that step 3
   configures.

   **It cannot be undone.** A version can be *yanked* — hidden from new
   dependency resolution — but never replaced or deleted, and the name stays
   taken. Get `cargo publish --dry-run` clean first.

3. **Turn on trusted publishing, and delete the token.** On
   `https://crates.io/crates/radbeeper/settings`, add a Trusted Publisher:

   | field | value |
   | --- | --- |
   | repository owner | `vonglurt` |
   | repository name | `radbeeper` |
   | workflow filename | `release.yml` |
   | environment | *(leave empty)* |

   Now GitHub Actions authenticates as this repository over OIDC, for the
   length of one job, and there is no API key in the repository to leak or to
   rotate. Revoke the token you used in step 2 at
   [crates.io/settings/tokens](https://crates.io/settings/tokens).

4. **Turn the workflow's publish step on.** It ships switched off, so that a
   tag pushed before any of the above still builds binaries and cuts a GitHub
   release instead of failing on a publish that could not have worked:

   ```sh
   gh variable set CRATES_IO_TRUSTED --body true
   ```

5. **Add owners, if anyone else should be able to publish.**

   ```sh
   cargo owner --add <github-username>
   ```

That is the whole of it. There is no review queue and no approval to wait
for: crates.io lists everything, and `cargo install radbeeper` works the
minute the first publish lands.

## Every release after that

```sh
make release-check V=0.2.0     # clean tree, no such tag, both suites pass
make release       V=0.2.0     # bumps, refreshes the lock, commits, dry-runs, tags
git push origin main && git push origin v0.2.0
```

The workflow then, in this order:

1. **verify** — the tag equals `rust/Cargo.toml`'s version, the crate builds
   with no warnings, the tests pass, `cargo package` succeeds, and the Python
   suite passes.
2. **binaries** — four static musl builds: x86\_64, aarch64, armv7 and armv6.
3. **release** — tarballs plus a `SHA256SUMS` on the GitHub release.
4. **crates-io** — `cargo publish`, last, because it is the irreversible step.

A tag whose version does not match the manifest fails at step 1 and publishes
nothing.

## What is deliberately not automated

- **The version bump is a commit you make**, not something CI infers from
  commit messages. `make release` writes it, you read it, and the tag points
  at it.
- **No `cargo publish` on a branch push.** Only a `v*` tag reaches crates.io.
- **No macOS or Windows binaries.** The port scan reads `/dev/ttyUSB*`,
  `/dev/ttyACM*` and `/sys/bus/usb-serial`; a macOS build would compile and
  then find nothing. `cargo install radbeeper` still works there — it just
  will not see a counter until the scan learns `/dev/cu.*`.

## Yanking

If a published version is broken:

```sh
cargo yank --version 0.2.0            # stop new installs resolving to it
cargo yank --version 0.2.0 --undo     # and back again
```

Yanking does not remove the version and does not free the name. The fix is
always a new version.
