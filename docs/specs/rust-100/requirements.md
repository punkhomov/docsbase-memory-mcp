# Rust 1.100 — 2026-11-13

Pin stable Rust/Cargo/rustfmt/Clippy and CI to 1.100.0. Regenerate Cargo.lock from scratch under the 14-day deny policy. Preserve Cargo.toml manifest declarations. Independently audit every registry lock entry, failing missing pubtime and unsupported external sources; Cargo itself exempts existing lock entries. Update both PRs against main and prepare a tested integration order.
