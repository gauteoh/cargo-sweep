⚠️⚠️ Warning: `cargo-sweep` is currently unmaintained. ⚠️⚠️

The project has issues and is lacking a dedicated maintainer to contribute and/or review contributions from others.

If you're looking for an alternative, check [`cargo-clean-all`](https://github.com/dnlmlr/cargo-clean-all).

If you want to discuss the possibility of a handover, reach out to one of these two previous maintainers:

1. @jyn514 ("jyn" in the Rust-Lang's Zulip)
2. @marcospb19 (contact info [here](https://github.com/marcospb19#--contact-info))

Or open an issue.

---

# cargo-sweep

`cargo-sweep` is a Cargo subcommand for deleting old or unused build artifacts from `target/` without doing a full `cargo clean`.

It is useful when you want to:

- shrink large `target/` directories
- avoid bloated CI caches
- clean many Rust projects at once

## Why use it?

`cargo clean` removes everything, `cargo-sweep` lets you clean more selectively.

For example, you can remove artifacts:

- older than a number of days
- not built by currently installed toolchains
- until a `target/` directory drops below a size limit

## Install

```bash
cargo install cargo-sweep
```

After installation, use it as a Cargo subcommand:

```bash
cargo sweep --help
```

## Quick examples

Clean artifacts older than 30 days in the current project:

```bash
cargo sweep --time 30
```

Preview what would be deleted:

```bash
cargo sweep --dry-run --time 30
```

Clean artifacts not built by currently installed Rust toolchains:

```bash
cargo sweep --installed
```

Keep only artifacts for specific toolchains:

```bash
cargo sweep --toolchains stable,nightly
```

Shrink `target/` until it is below 10 GiB:

```bash
cargo sweep --maxsize 10GiB
```

Clean everything in a specific project:

```bash
cargo sweep --all path/to/project
```

Recursively sweep all Cargo projects under a directory:

```bash
cargo sweep --recursive --time 30 path/to/workspace-root
```

## Timestamp workflow

If you want to keep only artifacts used after a certain point in time:

```bash
cargo sweep --stamp

# run builds, tests, benches, etc.

cargo sweep --file
```

This writes a timestamp marker, then later removes artifacts older than that marker.

## Toolchain behavior

In a `rustup` environment, `cargo-sweep` can identify artifacts from installed toolchains.

If `rustup` is unavailable, it falls back to invoking `rustc` directly.

## Help

For full usage:

```bash
cargo sweep --help
```

Standalone binary help:

```bash
cargo-sweep --help
```

## Alternatives

- `cargo clean` (built-in).
- [`cargo-clean-all`](https://github.com/dnlmlr/cargo-clean-all).
