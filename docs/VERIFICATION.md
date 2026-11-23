# Satin verification

The source was recovered from a surviving worktree containing the alpha.13 code and subsequent work on CSS and IntersectionObserver. The existing Windows alpha.14 binary supplied a reference point after recovery.

Run `cargo test --workspace --locked` to verify the Rust workspace. The `tools` directory contains reference scenarios for Chrome through REA and runners for pinned WPT and Test262 revisions.
