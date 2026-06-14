//! Render-path selection — the single source of truth for whether the grid
//! compiles its **monomorphized** body (a copy per row type) or its **type-erased**
//! body (one shared copy). See the `force-mono` / `force-erased` features in
//! `Cargo.toml`.
//!
//! The resolution is entirely compile-time (it must be — erased-vs-monomorphized
//! is a codegen choice, not a runtime branch). Everywhere else in the crate keys
//! off the two cfg aliases exported here so the policy lives in exactly one place:
//!
//! ```ignore
//! #[cfg(grid_erased)]  fn render() { /* erased path */ }
//! #[cfg(grid_mono)]    fn render() { /* monomorphized path */ }
//! ```
//!
//! (The `grid_mono` / `grid_erased` cfgs are set by `build.rs` from the feature
//! flags + target, so downstream crates don't need to declare them.)

// Enabling both overrides is contradictory — fail fast with a clear message
// rather than silently letting one win.
#[cfg(all(feature = "force-mono", feature = "force-erased"))]
compile_error!("grid-dioxus: `force-mono` and `force-erased` are mutually exclusive — enable at most one.");
