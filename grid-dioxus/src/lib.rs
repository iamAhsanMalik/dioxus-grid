// Columns and props hold `fn` pointers (cell renderers, sort keys), and Dioxus
// requires props to be `PartialEq` so it can skip re-renders. Comparing function
// pointers is only a heuristic — two identical closures may have different
// addresses — but for change detection a false "not equal" just re-renders, which
// is correct if occasionally wasteful. The alternative is hand-writing PartialEq
// for every props struct and ignoring the callbacks, which is strictly worse.
#![allow(unpredictable_function_pointer_comparisons)]
// Several generic helpers and imports are used only by the monomorphized body; in
// erased mode they are compiled but unreferenced.
#![cfg_attr(grid_erased, allow(dead_code, unused_imports))]

//! A headless [Dioxus](https://dioxuslabs.com) renderer over the
//! framework-agnostic [`grid_core`] engine, [`grid_state`] controller and
//! [`grid_plugin_api`] pipeline.
//!
//! Elements carry semantic `data-*` attributes (`data-sorted`, `data-selected`,
//! `data-density`, …) and spread a `Vec<Attribute>`, and the crate ships no
//! styling — bring your own CSS. This follows the same convention as
//! [dioxus components](https://github.com/DioxusLabs/components).
//!
//! ```no_run
//! use dioxus::prelude::*;
//! use grid_dioxus::{DataGrid, GridColumn};
//!
//! #[derive(Clone, PartialEq)]
//! struct Row { name: String, qty: i64 }
//!
//! #[component]
//! fn Demo() -> Element {
//!     let rows = vec![Row { name: "Widget".into(), qty: 12 }];
//!     let columns = vec![
//!         GridColumn::new("name", "Name", |r: &Row| rsx! { "{r.name}" }),
//!         GridColumn::new("qty", "Qty", |r: &Row| rsx! { "{r.qty}" }),
//!     ];
//!     rsx! { DataGrid { rows, columns } }
//! }
//! ```

use dioxus::prelude::*;

mod adaptive_grid;
mod data_grid;
mod date_range;
#[cfg(grid_erased)]
mod erased;
#[cfg(grid_erased)]
mod erased_render;
mod grid_export;
mod mode;
mod storage;

pub use adaptive_grid::AdaptiveDataGrid;
pub use data_grid::{
    Aggregate, CellEdit, DataGrid, DataProvider, FilterKind, GridAction, GridAlign, GridColumn, GridDensity, GridPage,
    GridQuery, LocalProvider,
};
pub use date_range::{Date, DateRangePicker};
pub use grid_export::{ExportData, ExportFormat};

// Re-export the headless engine so consumers can build queries, providers and
// state without adding the core crates to their own manifest.
pub use grid_core;
pub use grid_plugin_api;
pub use grid_state;

/// Extra attributes for [`Spinner`].
#[derive(Props, Clone, PartialEq)]
pub struct SpinnerProps {
    /// Extra attributes (class, style, …) applied to the spinner element.
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,
}

/// Indeterminate loading spinner used by the grid's loading state.
///
/// Headless: it carries `role="status"` and `data-grid-spinner`, so size and
/// colour it by styling the `[data-grid-spinner]` selector.
#[component]
pub fn Spinner(props: SpinnerProps) -> Element {
    rsx! {
        span {
            role: "status",
            "aria-label": "Loading",
            "data-grid-spinner": "true",
            ..props.attributes,
        }
    }
}
