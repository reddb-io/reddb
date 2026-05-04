# Convert reddb repo to Cargo workspace skeleton [AFK]

GitHub issue: reddb-io/reddb#55
Parent PRD: reddb-io/reddb#54

Convert the single-crate repository into a Cargo workspace. No code moves yet — the existing `reddb` crate stays at root. The workspace `Cargo.toml` declares the workspace root with `members = ["crates/*"]` so later slices can drop new crates under `crates/`. CI continues to build `red` and run the full test suite green.

## Acceptance Criteria
- [ ] Workspace `Cargo.toml` exists at repo root declaring at least one workspace member layout (root crate + `crates/*`)
- [ ] `cargo build --bin red` produces an unchanged binary (size and behavior parity)
- [ ] `cargo test` runs the existing test suite green
- [ ] CI passes on the new layout
- [ ] No source files outside `Cargo.toml`/workspace plumbing are modified

## Feedback Loops (Rust project — NOT pnpm)
- `cargo check`
- `cargo build --bin red`
- `cargo test`
