# Contributing

Read `ARCHITECTURE.md` first. It defines crate boundaries and public contracts.

## Workflow

- One branch per feature: `feat/<topic>`, `fix/<topic>`, `docs/<topic>`, `test/<topic>`.
- Open a pull request into `main`. Do not push directly to `main`.
- Commit messages are in English, in Conventional Commits form: `feat(scan): solve object array field order`. Imperative mood, subject under 72 characters, body explains why.
- Keep commits focused. Each commit should build.

Before every push:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Releases

Binaries are only ever built by the `Release` workflow (`.github/workflows/release.yml`). Never build, sign or upload release files by hand.

To cut a release `X.Y.Z`:

1. In a pull request, set `version` under `[workspace.package]` in the root `Cargo.toml` to `X.Y.Z` (and update `Cargo.lock`).
2. In the same pull request, move the `[Unreleased]` entries of `CHANGELOG.md` into a new section headed `## [X.Y.Z] - YYYY-MM-DD`, in the Keep a Changelog format. The section becomes the release notes and must not be empty.
3. Merge the pull request into `main`.
4. Tag the merge commit on `main` and push the tag: `git tag vX.Y.Z` then `git push origin vX.Y.Z`.

The workflow refuses to release if the tag is not on `main`, does not match the workspace version, has no changelog section, or fails any of the CI checks. It publishes a `x86_64-pc-windows-msvc` zip, the raw `ufathom.exe` and `ufathom.dll`, and `SHA256SUMS`, with build provenance attestations. Versions `0.x` and versions with a pre-release suffix are published as pre-releases. A run that fails before the final step publishes nothing. After fixing the cause, delete the tag and push it again on the corrected commit.

## Never commit

- Game executables, pak files, memory dumps, usmap files or generated SDKs from real games.
- Absolute addresses or RVAs of globals in specific game builds. Struct layout offsets are fine.
- Personal paths or machine details. Local paths go in `ufathom.local.toml`, which is ignored.
- Names or versions of specific games we test on. The repository refers to engine versions only.
- Third-party code without a compatible license. **Dumper-7 has no license: do not copy its code**, only independently reimplement ideas. Code derived from MIT projects keeps attribution in `THIRD_PARTY.md`.

## Code style

**Language and tooling.** Stable Rust, edition 2024. Default `rustfmt`. Clippy is clean under `-D warnings`. Dependencies are declared once in `[workspace.dependencies]`; add one only with a clear need.

**Errors.**
- Libraries use `thiserror` enums per crate. `anyhow` only in `ufathom-cli`.
- No `unwrap`, `expect`, `panic!`, `todo!` or indexing that can panic on target-derived data in library code. Tests may use them. `expect` on a true internal invariant needs a message stating the invariant.
- Errors carry context: the address, the object path, the field being solved.

**Target data.** This code runs inside a live game. A bug here must not take the game down.
- Treat every value read from the target as hostile. Use checked arithmetic on target-derived sizes and addresses.
- Read only through `MemoryReader`. **Never dereference a raw pointer to target memory in the dump path**, not even after a validity check.
- Bound every loop over target data with a named constant. Detect cycles in linked structures.
- Allocate only after validating a target-derived count against a limit.
- Never trust a name, flag or size to be consistent across objects. Data may change while being read.

**DLL rules.**
- Nothing in `DllMain` except `DisableThreadLibraryCalls` and `CreateThread`.
- Every thread entry point uses `catch_unwind`. Never set `panic = "abort"`.
- No global `rayon` pool, no detached threads. Everything must stop before unload.
- No global allocator changes, no console output, no window creation in the dump path.

**`unsafe`.**
- Allowed only where required: OS calls, `Pod` reinterpretation, in-process execution.
- Every `unsafe` block has a `// SAFETY:` comment giving the actual reason.
- `#![deny(unsafe_op_in_unsafe_fn)]` in every crate. Crates with no need for `unsafe` use `#![forbid(unsafe_code)]`: `ufathom-model`, `ufathom-codegen`, `ufathom-usmap`, `ufathom-scan`, `ufathom-reflect`, `ufathom-engine`, `ufathom-agent`.

**Naming and structure.**
- Names say what a thing is. Avoid `data`, `info`, `helper`, `util`, `manager`.
- Offsets are `u32` named fields, not tuples. Magic numbers become named constants with a comment on their source.
- Prefer small modules with one responsibility. Split a file past about 600 lines at a natural boundary.
- Public items have doc comments stating behaviour and failure modes. Comments explain why, not what. No commented-out code.
- No game-specific branches in solver code. `if game == "X"` is a design bug. A build quirk must be expressed as a general inference.

**Determinism.** Generators and dumps sort by path. No timestamps, random seeds or hash-map iteration order in output.

**Logging.** Library crates use `tracing`, never `println!`. Only the CLI prints to the terminal.

## Tests

- Test behaviour and failure modes, not implementation text.
- Every parser has a corruption test: unmapped memory, huge counts, cycles, null pointers.
- A bug fix comes with a regression test that failed before the fix.
- Tests that need real game files are `#[ignore]` and read paths from `ufathom.local.toml`. CI never needs them.
- Do not mark anything as supported in `COMPATIBILITY.md` from synthetic results alone.

## Honesty rules

- No stubs presented as features. An unimplemented path returns a clear error, and `ROADMAP.md` lists it.
- A `TODO` needs a reason and a link to a roadmap item.
- Report what was actually verified, including what failed.
