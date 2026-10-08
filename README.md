# packpub

Small Rust CLI for publishing public binary download URLs to a Homebrew tap and the AUR.

## Setup

```sh
cargo install --path .
packpub setup
```

Setup asks for tap (`owner/repo` or Git URL), whether to add `-bin` to AUR names (default: yes), package description, homepage, and SPDX license. Config lives at `~/.config/packpub/config.toml`.

Git credentials must already work for both destinations: write access to the tap and SSH access to `aur@aur.archlinux.org`.

## Publish

Supply at least one platform-specific **public HTTP(S) URL**. URLs must return the artifact directly without authentication. `packpub` streams each artifact to calculate SHA-256, then inserts URLs and checksums into both package definitions.

```sh
packpub publish widget --version 1.2.3 \
  --linux-x86-64 https://downloads.example.com/widget-1.2.3-linux-x86_64 \
  --linux-aarch64 https://downloads.example.com/widget-1.2.3-linux-aarch64 \
  --macos-x86-64 https://downloads.example.com/widget-1.2.3-darwin-x86_64 \
  --macos-aarch64 https://downloads.example.com/widget-1.2.3-darwin-aarch64
```

Available platforms: `linux-x86-64`, `linux-aarch64`, `macos-x86-64`, `macos-aarch64`. `--description`, `--homepage`, and `--license` override setup metadata for a release.

AUR `PKGBUILD` currently expects each downloaded artifact itself to be the executable. Homebrew formula extracts standard archives when applicable. Each publish clones the tap into a temporary directory, writes/commits/pushes `Formula/<name>.rb`, then clones/updates/commits/pushes the AUR package repository. Git identity must be configured.

## Build

```sh
cargo build --release
cargo test
```

Build and zip release binaries for Linux x86-64/AArch64 and macOS x86-64/Apple Silicon (requires Zig, `cargo-zigbuild`, Rust targets, and `zip`):

```sh
cargo install --locked cargo-zigbuild
scripts/build-release.sh 0.1.0
```

Archives are written to `dist/`.
