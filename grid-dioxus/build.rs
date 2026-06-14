//! Resolves the render-path cfgs (`grid_mono` / `grid_erased`) from the feature
//! flags and the compile target, so the rest of the crate can `#[cfg(grid_erased)]`
//! without repeating the policy. See `src/mode.rs`.
//!
//! Policy:
//!   force-erased            → erased
//!   force-mono              → monomorphized
//!   (neither) + wasm target → erased
//!   (neither) + native      → monomorphized

use std::env;

fn main() {
    // Declare the cfgs we set so `-D warnings` / `check-cfg` stays quiet on 1.80+.
    println!("cargo::rustc-check-cfg=cfg(grid_mono)");
    println!("cargo::rustc-check-cfg=cfg(grid_erased)");

    let force_mono = env::var_os("CARGO_FEATURE_FORCE_MONO").is_some();
    let force_erased = env::var_os("CARGO_FEATURE_FORCE_ERASED").is_some();
    let is_wasm = env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");

    // `mode.rs` already emits a compile_error! if both are set; pick mono here so
    // the build still produces coherent cfgs for the error path.
    let erased = if force_mono {
        false
    } else if force_erased {
        true
    } else {
        is_wasm
    };

    if erased {
        println!("cargo::rustc-cfg=grid_erased");
    } else {
        println!("cargo::rustc-cfg=grid_mono");
    }
}
