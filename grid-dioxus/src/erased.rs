//! The type-erased render model (compiled only in `grid_erased` mode).
//!
//! In erased mode the public `DataGrid<T>` does all the `T`-specific work up front
//! — projecting each visible row's columns to plain `Element`s / strings and boxing
//! the callbacks — producing an [`ErasedGrid`] that carries **no `T`**. A single,
//! non-generic renderer then draws that snapshot, so the ~2,500-line render body
//! compiles exactly once no matter how many row types the app uses.
//!
//! The engine work (query/sort/filter/paginate via `grid-core`/`grid-state`) still
//! runs generically in the thin `DataGrid<T>` shell — it's small; the size lives in
//! the renderer. Callbacks that hand a row back to the host are captured as index
//! closures over the original `Rc<[T]>`, so identity/round-trips stay correct.

#![cfg(grid_erased)]

use std::rc::Rc;

use dioxus::prelude::*;

use std::collections::HashMap;

use crate::data_grid::{FilterKind, GridAlign, GridDensity};
use crate::grid_plugin_api::Aggregate;
use crate::grid_plugin_api::{AggValue, FilterOp};
use crate::grid_state::GridState;

/// A column, with `T` already erased: its per-row projections have been applied,
/// leaving only display metadata. Built once per render from `GridColumn<T>`.
#[derive(Clone, PartialEq)]
pub struct ErasedColumn {
    pub key: &'static str,
    pub label: &'static str,
    pub align: GridAlign,
    pub width: Option<&'static str>,
    pub hide_on_mobile: bool,
    pub aggregate: Option<Aggregate>,
    pub sortable: bool,
    pub editable: bool,
    /// Whether this column offers a per-header filter funnel (it can be projected).
    pub filterable: bool,
    /// The resolved filter control kind (Auto already collapsed to Text/Number).
    pub filter_kind: FilterKind,
    /// Distinct values for a `Set` filter (pre-computed in the shell). Empty otherwise.
    pub set_values: Vec<String>,
    /// Whether this column is numeric (drives the operator list + range control).
    pub numeric: bool,
    /// Whether this column can be grouped by (has a text projection).
    pub groupable: bool,
}

/// One visible row, fully projected. `cells[i]` is the rendered `Element` for
/// `columns[i]`; `edit_seed[i]` is that cell's editable text (empty if read-only).
///
/// Per-row callbacks are captured here, each already closing over this row's
/// concrete `T`, so the renderer routes an interaction back to the host without
/// ever holding `T`. `id` is the identity used for selection.
#[derive(Clone)]
pub struct ErasedRow {
    pub id: String,
    pub cells: Vec<Element>,
    pub edit_seed: Vec<String>,
    /// The row's kebab actions (already computed from the host's `actions` fn).
    pub actions: Vec<crate::data_grid::GridAction>,
    /// Auto/host card body for gallery view.
    pub card: Option<Element>,
    /// Custom full-width list-row body, when the host supplied a `row` renderer.
    pub row_slot: Option<Element>,
    /// The row's value in the active group-by column (text), when grouping is on —
    /// so the renderer can insert group-header rows on value changes. `None` when
    /// not grouping.
    pub group_value: Option<String>,
    /// Fire the host's row-click with this row.
    pub on_click: Option<Rc<dyn Fn()>>,
    /// Fire the host's action handler with (action_key) for this row.
    pub on_action: Option<Rc<dyn Fn(&'static str)>>,
    /// Commit an inline edit of `(column_key, new_value)` for this row.
    pub on_edit: Option<Rc<dyn Fn(&'static str, String)>>,
}

/// A bulk action fired for `(action_key, selected_row_ids)`. The shell maps the ids
/// back to concrete rows before calling the host.
pub type BulkActionFn = Rc<dyn Fn(&'static str, Vec<String>)>;

/// A client-side export of `(format, scope_all)`, built by the shell.
pub type ExportFn = Rc<dyn Fn(crate::grid_export::ExportFormat, bool)>;

/// Grid-level callbacks (per-row ones live on [`ErasedRow`]). Bulk actions take
/// the set of selected row ids; the shell maps those back to concrete rows.
#[derive(Clone)]
pub struct ErasedCallbacks {
    pub on_bulk_action: Option<BulkActionFn>,
    pub on_selection: Option<Rc<dyn Fn(Vec<String>)>>,
    pub on_export_signed: Option<Rc<dyn Fn(crate::data_grid::GridQuery)>>,
    /// Run a client-side export of `(format, scope_all)`. Built in the generic shell
    /// — it re-runs the query unbounded and projects every row to text, both of which
    /// need `T` — so the renderer can offer the export menu without seeing `T`.
    /// `scope_all` ignores search + filters; otherwise the filtered set is exported.
    pub on_export: Option<ExportFn>,
}

/// Everything the non-generic renderer needs — no `T` anywhere.
#[derive(Clone)]
pub struct ErasedGrid {
    pub columns: Vec<ErasedColumn>,
    /// The projected rows of the CURRENT visible page (after search/filter/sort/
    /// paginate). `source_index` on each still points into the full dataset.
    pub rows: Vec<ErasedRow>,
    pub callbacks: ErasedCallbacks,
    pub density: GridDensity,
    pub selectable: bool,
    pub loading: bool,
    pub empty_label: String,
    pub grid_default: bool,
    pub no_card_toggle: bool,
    pub has_card: bool,
    pub has_row_slot: bool,
    pub has_search: bool,
    pub export_filename: Option<&'static str>,
    /// Formats the export menu may offer; `None` = every available format.
    pub export_formats: Option<&'static [crate::grid_export::ExportFormat]>,
    pub today: Option<String>,
    pub persist_key: Option<&'static str>,
    /// Row count of the whole dataset, ignoring search + filters — the "Export all"
    /// label. (`total` is the filtered count.)
    pub full_count: usize,
    // Query results for the current page (from the engine, computed in the shell).
    pub total: usize,
    pub page_count: usize,
    pub page: usize,
    pub aggregates: Vec<(String, AggValue)>,
    // Slots the host passed as already-built Elements (naturally erased).
    pub toolbar: Option<Element>,
    pub bulk: Option<Element>,
}

/// The interaction state the renderer mutates and the shell re-queries on. Owned
/// by the shell, passed to the renderer as signals so event handlers write back
/// and the shell's page memo recomputes. All non-generic — no `T`.
#[derive(Clone, Copy, PartialEq)]
pub struct ErasedState {
    pub grid: Signal<GridState>,
    pub filters: Signal<HashMap<&'static str, (FilterOp, String)>>,
    pub extra_sorts: Signal<Vec<(&'static str, bool)>>,
    pub group_by: Signal<Option<&'static str>>,
}
