# signalsmith-stretch, with one addition

A copy of the [`signalsmith-stretch`](https://crates.io/crates/signalsmith-stretch)
crate, version 0.1.3 (MIT, Colin Marc), which wraps
[Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch) (MIT,
Signalsmith Audio), the pitch shifter behind earshot's keylock and key shift
(`engine/src/keylock.rs`). The workspace's `Cargo.toml` uses this copy in place of the
published crate (`[patch.crates-io]`).

## Why

The C++ library can spread each slice's processing over the 30 ms until the next slice
(`splitComputation`), instead of doing it all in one audio block. Left off, about one
block in six costs many times the others, which is what makes keylock drop out on a
slow CPU (lewinfox/earshot#32). The published wrapper has no way to turn it on.

## What was changed

- `src/wrapper.h`, `src/wrapper.cpp`: `signalsmith_stretch_create_preset_default_split`,
  which calls `presetDefault(channels, sampleRate, true)`.
- `src/lib.rs`: `Stretch::preset_default_split`, which calls it.
- `Cargo.toml`: written by hand (the published one is generated), without the examples
  and their dependencies.
- Left out, as nothing here builds them: the crate's examples, and the C++ libraries'
  command-line tool, web build, tests and build files. The C++ headers are untouched.

Both additions are marked `earshot:` in the source.

## Dropping this copy

If the published crate gains a way to set `splitComputation`, use that in
`engine/src/keylock.rs`, delete this folder and the `[patch.crates-io]` entry in the
workspace's `Cargo.toml`, and run `cargo update -p signalsmith-stretch`.
