//! DataGrid — the one table to rule them all (KendoGrid-class, Dioxus-native).
//!
//! Generic over the row type. Headless state (search / sort / page / selection)
//! lives inside; screens describe columns declaratively and receive events:
//!
//! ```ignore
//! DataGrid::<Member> {
//!     rows: members(),
//!     columns: vec![
//!         GridColumn::new("name", "Member").sortable().render(|m| rsx! { "{m.name}" }),
//!         GridColumn::new("status", "Status").render(|m| rsx! { Badge { "{m.status}" } }),
//!     ],
//!     row_id: |m| m.id.clone(),
//!     search_text: |m| format!("{} {}", m.name, m.email),
//!     actions: |_m| vec![GridAction::new("edit", "Edit"), GridAction::danger("disable", "Disable")],
//!     on_action: move |(key, row)| { ... },
//!     selectable: true,
//!     bulk: rsx! { Button { "Disable selected" } },
//! }
//! ```
//!
//! Features: global search, tri-state column sort (asc → desc → none), numeric
//! sort keys, pagination with page-size picker, row selection + select-page,
//! per-row action dropdown (the last column is always a kebab menu when
//! `actions` is set), sticky header, loading / empty states, responsive
//! horizontal scroll. Per-column filters arrive with the filter row toggle.

use std::collections::HashMap;
use std::rc::Rc;

use dioxus::prelude::*;
use grid_plugin_api::{AggValue, CellValue, Filter, FilterModel, FilterOp, MultiSort, Sort};
use grid_state::{GridState, ViewMode};
// Re-exported so screens can write `GridColumn::new(...).aggregate(Aggregate::Sum)`,
// and build remote-mode queries/pages without depending on grid-plugin-api directly.
// These are also used unqualified throughout this module.
pub use grid_plugin_api::{Aggregate, DataProvider, GridPage, GridQuery, LocalProvider};

// ── Header-sort intent (kept OUT of the rsx! macro on purpose) ───────────────
// rustfmt mangles inline `if/else` written inside an `rsx!` event handler (it
// drops the `else {`, producing stray-brace compile errors on save). Pulling the
// branching into a plain free fn keeps it rustfmt-safe; the handler is a one-liner.
fn sort_header_click(
    key: &'static str,
    shift: bool,
    mut grid: Signal<GridState>,
    mut extra_sorts: Signal<Vec<(&'static str, bool)>>,
) {
    if shift {
        // Shift-click: cycle this column in the secondary multi-sort chain
        // (asc → desc → remove), leaving the primary sort untouched.
        let mut xs = extra_sorts.write();
        match xs.iter().position(|(k, _)| *k == key) {
            Some(i) if xs[i].1 => xs[i].1 = false,
            Some(i) => {
                xs.remove(i);
            }
            None => xs.push((key, true)),
        }
    } else {
        // Plain click: primary tri-state sort; reset the tie-breakers.
        grid.write().toggle_sort(key);
        extra_sorts.write().clear();
    }
}

/// Toggle a star-rating filter: clicking the active minimum clears it, else set it.
/// Free fn (not inline rsx) so rustfmt can't mangle the branch on save.
fn rating_star_click(
    key: &'static str,
    chosen: f64,
    star: f64,
    mut filters: Signal<HashMap<&'static str, (FilterOp, String)>>,
    mut grid: Signal<GridState>,
) {
    let next = if (chosen - star).abs() < 0.01 { String::new() } else { star.to_string() };
    if next.is_empty() {
        filters.write().remove(key);
    } else {
        filters.write().insert(key, (FilterOp::GreaterOrEqual, next));
    }
    grid.write().set_page(0);
}

use crate::grid_export::{self, ExportFormat};
use crate::Spinner;

// ── Filter helpers ───────────────────────────────────────────────────────────

/// Operators offered for a column, by its kind. Numeric columns (those with a
/// `sort_num`) get comparison ops; text columns get substring ops; both get
/// equality and emptiness.
fn ops_for(is_numeric: bool) -> &'static [(FilterOp, &'static str)] {
    if is_numeric {
        &[
            (FilterOp::Equals, "="),
            (FilterOp::GreaterThan, ">"),
            (FilterOp::LessThan, "<"),
            (FilterOp::GreaterOrEqual, "≥"),
            (FilterOp::LessOrEqual, "≤"),
            (FilterOp::IsEmpty, "empty"),
        ]
    } else {
        &[
            (FilterOp::Contains, "contains"),
            (FilterOp::Equals, "="),
            (FilterOp::StartsWith, "starts"),
            (FilterOp::IsEmpty, "empty"),
        ]
    }
}

/// True for operators that ignore their value (so an empty value is still active).
fn op_is_valueless(op: &FilterOp) -> bool {
    matches!(op, FilterOp::IsEmpty | FilterOp::IsNotEmpty)
}

/// Caption for an aggregate, shown above its value in the totals row.
pub(crate) fn agg_label(agg: Aggregate) -> &'static str {
    match agg {
        Aggregate::Sum => "Total",
        Aggregate::Avg => "Average",
        Aggregate::Min => "Minimum",
        Aggregate::Max => "Maximum",
        Aggregate::Count => "Count",
    }
}

/// Format an aggregate result for display (whole numbers without a decimal).
pub(crate) fn fmt_agg(v: AggValue) -> String {
    match v {
        AggValue::Count(n) => n.to_string(),
        AggValue::Num(n) => {
            if n.fract() == 0.0 {
                format!("{}", n as i64)
            } else {
                format!("{n:.2}")
            }
        }
        AggValue::Empty => "—".to_string(),
    }
}

/// Build a `Filter` for one column, parsing the value as a number for numeric
/// columns (so `>` / `<` compare numerically against the `sort_num` projection).
fn make_filter(key: &'static str, op: &FilterOp, value: &str, is_numeric: bool) -> Filter {
    if op_is_valueless(op) {
        return Filter::new(key, op.clone(), CellValue::Empty);
    }
    let cv = if is_numeric {
        value.trim().parse::<f64>().map(CellValue::Number).unwrap_or(CellValue::Empty)
    } else {
        CellValue::Text(value.to_string())
    };
    Filter::new(key, op.clone(), cv)
}

/// Translate one column's stored filter `(op, value)` into the actual
/// [`Filter`]s, honoring its [`FilterKind`]. Most kinds yield a single filter;
/// `Range` packs its bounds as `"min|max"` and expands to up to two. Pushes
/// nothing for an inactive entry.
fn push_column_filters(
    fm: FilterModel,
    key: &'static str,
    kind: FilterKind,
    op: &FilterOp,
    value: &str,
    is_numeric: bool,
) -> FilterModel {
    match kind {
        // A min–max range over the numeric projection: `>= min` and/or `<= max`.
        FilterKind::Range => {
            let (lo, hi) = value.split_once('|').unwrap_or((value, ""));
            let mut fm = fm;
            if let Ok(n) = lo.trim().parse::<f64>() {
                fm = fm.with(Filter::new(key, FilterOp::GreaterOrEqual, CellValue::Number(n)));
            }
            if let Ok(n) = hi.trim().parse::<f64>() {
                fm = fm.with(Filter::new(key, FilterOp::LessOrEqual, CellValue::Number(n)));
            }
            fm
        }
        // Star rating: keep only rows scoring at least N (0 = no filter).
        FilterKind::Rating => match value.trim().parse::<f64>() {
            Ok(n) if n > 0.0 => fm.with(Filter::new(key, FilterOp::GreaterOrEqual, CellValue::Number(n))),
            _ => fm,
        },
        // Present / absent — the stored op is already IsNotEmpty / IsEmpty.
        FilterKind::HasValue => fm.with(Filter::new(key, op.clone(), CellValue::Empty)),
        // Multi-select: the value packs the chosen members, one per `\u{1}`.
        // Maps to a single `In` filter (cell equals any member).
        FilterKind::Set => {
            let chosen: Vec<CellValue> =
                value.split('\u{1}').filter(|s| !s.is_empty()).map(|s| CellValue::Text(s.to_string())).collect();
            if chosen.is_empty() {
                fm
            } else {
                fm.with(Filter::one_of(key, chosen))
            }
        }
        // A from–to date range, packed "from|to". Dates are ISO strings, so a
        // lexicographic >= / <= over the text projection is chronological.
        FilterKind::DateRange => {
            let (lo, hi) = value.split_once('|').unwrap_or((value, ""));
            let mut fm = fm;
            if !lo.trim().is_empty() {
                fm = fm.with(Filter::new(key, FilterOp::GreaterOrEqual, CellValue::Text(lo.trim().to_string())));
            }
            if !hi.trim().is_empty() {
                fm = fm.with(Filter::new(key, FilterOp::LessOrEqual, CellValue::Text(hi.trim().to_string())));
            }
            fm
        }
        // Text / Number / Auto: the existing single-filter behavior.
        _ => {
            if op_is_valueless(op) || !value.trim().is_empty() {
                fm.with(make_filter(key, op, value, is_numeric))
            } else {
                fm
            }
        }
    }
}

/// Resolve a column's effective [`FilterKind`], turning `Auto` into `Number` (when
/// the column has a numeric projection) or `Text`.
fn effective_kind<T: Clone + PartialEq + 'static>(col: &GridColumn<T>) -> FilterKind {
    match col.filter_kind {
        FilterKind::Auto if col.sort_num.is_some() => FilterKind::Number,
        FilterKind::Auto => FilterKind::Text,
        k => k,
    }
}

/// Is this stored filter entry actually doing something? Valueless ops (empty /
/// not-empty) are always active; everything else needs a non-blank value.
fn filter_is_active(op: &FilterOp, value: &str) -> bool {
    op_is_valueless(op) || !value.trim().is_empty()
}

/// A short human label for the active-filter chip bar, e.g. `Dept: Eng, Sales` or
/// `Salary > 100000`. Returns `None` when the entry isn't active.
pub(crate) fn chip_label(label: &str, kind: FilterKind, op: &FilterOp, value: &str) -> Option<String> {
    if !filter_is_active(op, value) {
        return None;
    }
    let body = match kind {
        FilterKind::Set => value.split('\u{1}').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        FilterKind::Range | FilterKind::DateRange => {
            let (lo, hi) = value.split_once('|').unwrap_or((value, ""));
            match (lo.trim().is_empty(), hi.trim().is_empty()) {
                (false, false) => format!("{}–{}", lo.trim(), hi.trim()),
                (false, true) => format!("≥ {}", lo.trim()),
                (true, false) => format!("≤ {}", hi.trim()),
                _ => String::new(),
            }
        }
        FilterKind::Rating => format!("≥ {value}★"),
        FilterKind::HasValue => match op {
            FilterOp::IsNotEmpty => "has file".to_string(),
            FilterOp::IsEmpty => "no file".to_string(),
            _ => String::new(),
        },
        _ => {
            let sym = match op {
                FilterOp::Equals => "=",
                FilterOp::Contains => "∋",
                FilterOp::StartsWith => "▷",
                FilterOp::GreaterThan => ">",
                FilterOp::GreaterOrEqual => "≥",
                FilterOp::LessThan => "<",
                FilterOp::LessOrEqual => "≤",
                _ => "",
            };
            return Some(format!("{label} {sym} {value}").trim().to_string());
        }
    };
    Some(format!("{label}: {body}"))
}

/// Distinct, sorted, non-blank values of a column over the dataset — the choices
/// shown by a [`FilterKind::Set`] checkbox list. Projects via `sort_key` (the text
/// projection); a column without one yields nothing. Capped so a high-cardinality
/// column can't render thousands of checkboxes.
fn distinct_values<T: Clone + PartialEq + 'static>(rows: &[T], col: &GridColumn<T>) -> Vec<String> {
    const MAX_DISTINCT: usize = 500;
    let Some(proj) = col.sort_key else { return Vec::new() };
    let mut seen = std::collections::BTreeSet::<String>::new();
    for r in rows {
        let v = proj(r);
        if !v.trim().is_empty() {
            seen.insert(v);
            if seen.len() >= MAX_DISTINCT {
                break;
            }
        }
    }
    seen.into_iter().collect()
}

/// Move column `from` to sit just before column `to` in the display order,
/// writing the full key order back into `order`. `all_keys` is the declared column
/// order, used to seed (when `order` is empty) and to backfill any unlisted keys.
fn reorder_columns(
    order: &mut Signal<Vec<&'static str>>,
    all_keys: &[&'static str],
    from: &'static str,
    to: &'static str,
) {
    let mut keys: Vec<&'static str> = {
        let cur = order.read();
        if cur.is_empty() {
            all_keys.to_vec()
        } else {
            let mut v = cur.clone();
            for k in all_keys {
                if !v.contains(k) {
                    v.push(k);
                }
            }
            v
        }
    };
    keys.retain(|k| *k != from);
    let pos = keys.iter().position(|k| *k == to).unwrap_or(keys.len());
    keys.insert(pos, from);
    order.set(keys);
}

// ── Layout persistence (localStorage) ────────────────────────────────────────

/// The persisted grid *layout* (not query state). Column order, widths, pins, and
/// hidden columns — everything the user arranges and would expect to survive a
/// reload. Serialized to a compact, human-readable line (no serde dep needed).
#[derive(Default)]
struct GridLayout {
    order: Vec<String>,
    hidden: Vec<String>,
    pins: Vec<String>,
    widths: Vec<(String, f64)>,
}

impl GridLayout {
    /// `order=a,b,c|hidden=d|pins=a|w=a:280,b:120`. Sections are omitted when empty;
    /// commas/colons can't appear in column keys (they're Rust identifiers), so no
    /// escaping is needed.
    fn encode(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.order.is_empty() {
            parts.push(format!("order={}", self.order.join(",")));
        }
        if !self.hidden.is_empty() {
            parts.push(format!("hidden={}", self.hidden.join(",")));
        }
        if !self.pins.is_empty() {
            parts.push(format!("pins={}", self.pins.join(",")));
        }
        if !self.widths.is_empty() {
            let w = self.widths.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(",");
            parts.push(format!("w={w}"));
        }
        parts.join("|")
    }

    fn decode(s: &str) -> Self {
        let mut out = GridLayout::default();
        for part in s.split('|') {
            let Some((tag, body)) = part.split_once('=') else { continue };
            let items = || body.split(',').filter(|x| !x.is_empty());
            match tag {
                "order" => out.order = items().map(str::to_string).collect(),
                "hidden" => out.hidden = items().map(str::to_string).collect(),
                "pins" => out.pins = items().map(str::to_string).collect(),
                "w" => {
                    out.widths = items()
                        .filter_map(|kv| kv.split_once(':'))
                        .filter_map(|(k, v)| v.parse::<f64>().ok().map(|n| (k.to_string(), n)))
                        .collect();
                }
                _ => {}
            }
        }
        out
    }
}

/// Re-map a list of persisted string keys onto the column's live `&'static str`
/// keys, dropping any that no longer exist (renamed / removed columns). This is
/// what makes persistence forward-safe: stale storage never references a key the
/// current columns don't have.
fn map_static_keys(stored: &[String], all_keys: &[&'static str]) -> Vec<&'static str> {
    stored.iter().filter_map(|s| all_keys.iter().copied().find(|k| *k == s.as_str())).collect()
}

// ── Grouping (display-layer) ──────────────────────────────────────────────────

/// Partition the (already sorted) page rows into contiguous groups by a column's
/// text projection, preserving order. Returns `(label, rows)` per group. Rows
/// whose projection is blank fall under "—". A column without a `sort_key` yields
/// a single catch-all group (it can't be grouped meaningfully).
fn compute_groups<T: Clone + PartialEq + 'static>(rows: &[T], group_col: &GridColumn<T>) -> Vec<(String, Vec<T>)> {
    let Some(proj) = group_col.sort_key else {
        return vec![(String::new(), rows.to_vec())];
    };
    let mut groups: Vec<(String, Vec<T>)> = Vec::new();
    for r in rows {
        let mut label = proj(r);
        if label.trim().is_empty() {
            label = "—".to_string();
        }
        match groups.last_mut() {
            Some((l, items)) if *l == label => items.push(r.clone()),
            _ => groups.push((label, vec![r.clone()])),
        }
    }
    groups
}

/// Compute one column's [`Aggregate`] over a group's rows (renderer-side, so group
/// subtotals match the footer's semantics). Numeric via `sort_num`; `Count` is
/// type-agnostic. Mirrors the provider's aggregation for a row subset.
fn subtotal_for<T: Clone + PartialEq + 'static>(rows: &[T], col: &GridColumn<T>) -> Option<AggValue> {
    let agg = col.aggregate?;
    if matches!(agg, Aggregate::Count) {
        return Some(AggValue::Count(rows.len()));
    }
    let proj = col.sort_num?;
    let nums: Vec<f64> = rows.iter().map(proj).filter(|n| !n.is_nan()).collect();
    if nums.is_empty() {
        return Some(AggValue::Empty);
    }
    let v = match agg {
        Aggregate::Sum => nums.iter().sum(),
        Aggregate::Avg => nums.iter().sum::<f64>() / nums.len() as f64,
        Aggregate::Min => nums.iter().cloned().fold(f64::INFINITY, f64::min),
        Aggregate::Max => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        Aggregate::Count => unreachable!(),
    };
    Some(AggValue::Num(v))
}

/// Map display columns to the lean engine `ColumnDef`s the provider needs (just
/// the sort/value projections — no rendering). Shared by the live query and export.
fn engine_columns<T: Clone + PartialEq + 'static>(cols: &[GridColumn<T>]) -> Vec<grid_core::ColumnDef<T>> {
    cols.iter()
        .map(|c| {
            let mut cd = grid_core::ColumnDef::new(c.key);
            cd.sort_key = c.sort_key;
            cd.sort_num = c.sort_num;
            cd
        })
        .collect()
}

/// Assemble the transport-agnostic [`GridQuery`] from the live UI state. One
/// definition feeds both the memoized page query and the CSV export (which only
/// overrides `page`/`page_size`).
fn build_grid_query<T: Clone + PartialEq + 'static>(
    cols: &[GridColumn<T>],
    filters: &HashMap<&'static str, (FilterOp, String)>,
    state: &GridState,
    extra_sorts: &[(&'static str, bool)],
    group_by: Option<&'static str>,
) -> GridQuery {
    let numeric = |k: &str| cols.iter().any(|c| c.key == k && c.sort_num.is_some());
    let kind_of =
        |k: &str| -> FilterKind { cols.iter().find(|c| c.key == k).map(|c| c.filter_kind).unwrap_or_default() };
    let mut filter_model = FilterModel::new();
    for (k, (op, v)) in filters.iter() {
        filter_model = push_column_filters(filter_model, k, kind_of(k), op, v, numeric(k));
    }
    let mut sort = MultiSort::new();
    // Grouping requires same-value rows to be contiguous, so the group column sorts
    // FIRST; the user's sort then orders rows within each group.
    if let Some(gk) = group_by {
        sort = sort.then(Sort::new(gk, true));
    }
    if let Some(s) = state.sort() {
        sort = sort.then(s);
    }
    for (k, asc) in extra_sorts.iter() {
        sort = sort.then(Sort::new(*k, *asc));
    }
    let aggregates = cols.iter().filter_map(|c| c.aggregate.map(|a| (c.key.to_string(), a))).collect();
    GridQuery {
        search: state.search().to_string(),
        filters: filter_model,
        sort,
        page: state.page(),
        page_size: state.page_size(),
        aggregates,
    }
}

// ── Column model ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GridAlign {
    Left,
    Right,
    Center,
}

/// UI density — how much enterprise chrome the grid shows. The shared grid stays
/// "Full" by default (every existing call site is unchanged); [`AdaptiveDataGrid`]
/// chooses `Minimal` for small datasets.
///
/// `Minimal` strips the heavy controls — per-header filter funnels, the column
/// chooser, group-by, and the pagination footer when everything fits one page —
/// leaving a sleek table plus the quick search and Export. Sort, selection, the
/// list⇆grid toggle, and search/export all still work.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum GridDensity {
    /// Full enterprise grid (the historical behavior).
    #[default]
    Full,
    /// Sleek minimalist layout for small, glanceable datasets.
    Minimal,
}

/// Which control the filter row shows for a column. Defaults are inferred
/// (`sort_num` → `Number`, else `Text`); set explicitly for rich columns so the
/// filter UI matches the data — a rating gets a star picker, a media column gets a
/// has-/no-file toggle, a quantity gets a numeric range. All of them drive the
/// same underlying [`FilterStage`] over the column's projection; only the *input*
/// differs.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterKind {
    /// Inferred from the column (the historical behavior).
    #[default]
    Auto,
    /// Free-text contains/equals/starts-with.
    Text,
    /// Numeric operators with a number input.
    Number,
    /// A 0–5 star picker filtering by "rating ≥ N" (uses the numeric projection).
    Rating,
    /// A present / absent toggle (empty vs. non-empty over the projection).
    HasValue,
    /// A min–max numeric range (e.g. duration or file size), both bounds optional.
    Range,
    /// A checkbox list of the column's distinct values (multi-select). A row passes
    /// if its value is any of the checked ones — the natural control for enums like
    /// Department or Status. Distinct values are derived from the dataset.
    Set,
    /// A from–to date range (two date pickers). Packs to the same min|max range as
    /// [`FilterKind::Range`] over the column's projection (lexicographic on
    /// ISO `YYYY-MM-DD`, which sorts chronologically).
    DateRange,
}

/// One column definition. `render` is a plain fn pointer so columns stay
/// `Clone + PartialEq`; row-specific interactivity goes through `actions`.
#[derive(Clone, PartialEq)]
pub struct GridColumn<T: Clone + PartialEq + 'static> {
    pub key: &'static str,
    pub label: &'static str,
    pub render: fn(&T) -> Element,
    /// String sort key; used when `sort_num` is None.
    pub sort_key: Option<fn(&T) -> String>,
    /// Numeric sort key — wins over `sort_key` when present.
    pub sort_num: Option<fn(&T) -> f64>,
    /// Plain-text projection for CSV export. When `None`, export falls back to
    /// `sort_key` (then `sort_num`); a column with none of the three is exported
    /// as an empty cell.
    pub csv: Option<fn(&T) -> String>,
    /// Footer aggregate over the current *filtered* set (sum/avg/min/max/count).
    /// `None` = no footer cell for this column. Uses the column's numeric
    /// projection (`sort_num`), except `Count` which is type-agnostic.
    pub aggregate: Option<Aggregate>,
    /// Which filter control this column shows (see [`FilterKind`]). Defaults to
    /// `Auto` — inferred from the sort projection.
    pub filter_kind: FilterKind,
    /// Makes the cell inline-editable: the projection returns the current text to
    /// seed the editor. Double-clicking the cell opens an input; committing fires
    /// the grid's `on_edit` with the new string. The grid never mutates the row
    /// itself (it shares `rows` by `Rc`) — the host applies the edit to its data.
    /// `None` = read-only (the default).
    pub edit: Option<fn(&T) -> String>,
    pub align: GridAlign,
    /// e.g. "12rem" — applied as min-width so long content doesn't crush peers.
    pub width: Option<&'static str>,
    /// Hidden below the `md` breakpoint (keeps phones to the essential columns).
    pub hide_on_mobile: bool,
}

impl<T: Clone + PartialEq + 'static> GridColumn<T> {
    pub fn new(key: &'static str, label: &'static str, render: fn(&T) -> Element) -> Self {
        Self {
            key,
            label,
            render,
            sort_key: None,
            sort_num: None,
            csv: None,
            aggregate: None,
            filter_kind: FilterKind::Auto,
            edit: None,
            align: GridAlign::Left,
            width: None,
            hide_on_mobile: false,
        }
    }
    /// Make this cell inline-editable. `f` returns the cell's current text (used
    /// to seed the editor). Committing an edit fires the grid's `on_edit` with the
    /// row, column key, and new value — the host applies it to its own data.
    pub fn editable(mut self, f: fn(&T) -> String) -> Self {
        self.edit = Some(f);
        self
    }
    /// Show a footer aggregate for this column (over the current filtered set).
    pub fn aggregate(mut self, agg: Aggregate) -> Self {
        self.aggregate = Some(agg);
        self
    }
    /// Choose the filter control for this column (rating picker, has-value toggle,
    /// numeric range, …). See [`FilterKind`].
    pub fn filter(mut self, kind: FilterKind) -> Self {
        self.filter_kind = kind;
        self
    }
    pub fn sortable(mut self, f: fn(&T) -> String) -> Self {
        self.sort_key = Some(f);
        self
    }
    /// Explicit plain-text projection for CSV export (overrides the `sort_key`
    /// fallback). Use when the display differs from what should land in the file.
    pub fn csv(mut self, f: fn(&T) -> String) -> Self {
        self.csv = Some(f);
        self
    }
    pub fn sortable_num(mut self, f: fn(&T) -> f64) -> Self {
        self.sort_num = Some(f);
        self
    }
    pub fn right(mut self) -> Self {
        self.align = GridAlign::Right;
        self
    }
    pub fn center(mut self) -> Self {
        self.align = GridAlign::Center;
        self
    }
    pub fn width(mut self, w: &'static str) -> Self {
        self.width = Some(w);
        self
    }
    pub fn mobile_hidden(mut self) -> Self {
        self.hide_on_mobile = true;
        self
    }
}

/// One entry in a row's kebab menu. `icon` is an SVG inner-HTML path string
/// (e.g. an `icons::*` const from the app crate) rendered before the label.
#[derive(Clone, PartialEq)]
pub struct GridAction {
    pub key: &'static str,
    pub label: String,
    pub danger: bool,
    pub disabled: bool,
    pub icon: Option<&'static str>,
    /// Also surface this action as an inline icon button next to the kebab
    /// (requires an `icon`). The action still appears in the dropdown too.
    pub quick: bool,
    /// Whether this action makes sense applied to a whole selection at once. When
    /// `false` the grid's auto bulk action bar omits it (e.g. "Edit", which needs
    /// a single row). Defaults to `true`; per-row kebab behaviour is unaffected.
    pub bulkable: bool,
}

impl GridAction {
    pub fn new(key: &'static str, label: impl Into<String>) -> Self {
        Self { key, label: label.into(), danger: false, disabled: false, icon: None, quick: false, bulkable: true }
    }
    pub fn danger(key: &'static str, label: impl Into<String>) -> Self {
        Self { key, label: label.into(), danger: true, disabled: false, icon: None, quick: false, bulkable: true }
    }
    /// Attach a leading icon (SVG path inner-HTML, 24×24 viewBox, stroke-based).
    pub fn icon(mut self, path: &'static str) -> Self {
        self.icon = Some(path);
        self
    }
    /// Surface this action inline as a quick icon button (and in the menu).
    pub fn quick(mut self) -> Self {
        self.quick = true;
        self
    }
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
    /// Mark this action as single-row-only, so it's excluded from the bulk action
    /// bar (e.g. an "Edit" that opens one record's form).
    pub fn no_bulk(mut self) -> Self {
        self.bulkable = false;
        self
    }
}

/// Payload of an inline cell edit: which row, which column, and the new text the
/// user typed. The host parses `value` into its field and updates its own data —
/// the grid never mutates the (shared, `Rc`-backed) rows itself.
#[derive(Clone, PartialEq)]
pub struct CellEdit<T: Clone + PartialEq + 'static> {
    pub row: T,
    pub key: &'static str,
    pub value: String,
}

// ── Grid ─────────────────────────────────────────────────────────────────────

#[derive(Props, Clone, PartialEq)]
pub struct DataGridProps<T: Clone + PartialEq + 'static> {
    /// The dataset, shared by reference-counted pointer so neither the parent nor
    /// the grid ever clones the whole thing — only the visible page's rows are
    /// materialised. Build with `Rc::from(vec)` or `vec.into()`.
    pub rows: Rc<[T]>,
    pub columns: Vec<GridColumn<T>>,
    /// Stable row identity (selection, action routing).
    pub row_id: fn(&T) -> String,
    /// Haystack for the global search box. None hides the search box.
    #[props(default)]
    pub search_text: Option<fn(&T) -> String>,
    /// Builds the kebab menu for a row. None hides the action column.
    #[props(default)]
    pub actions: Option<fn(&T) -> Vec<GridAction>>,
    /// Fired with (action_key, row) when a kebab entry is picked.
    #[props(default)]
    pub on_action: Option<EventHandler<(&'static str, T)>>,
    /// Row click (e.g. open detail drawer). Checkbox/kebab clicks don't bubble here.
    #[props(default)]
    pub on_row_click: Option<EventHandler<T>>,
    /// Fired when an inline cell edit is committed (Enter or blur). Requires the
    /// column to be `.editable(...)`. The host applies the new value to its data.
    #[props(default)]
    pub on_edit: Option<EventHandler<CellEdit<T>>>,
    #[props(default = false)]
    pub selectable: bool,
    /// Fired whenever the selected-id set changes.
    #[props(default)]
    pub on_selection: Option<EventHandler<Vec<String>>>,
    /// Rendered in the toolbar when ≥1 row is selected (custom bulk actions). Kept
    /// for host-specific bulk UI; for the common case prefer `on_bulk_action`,
    /// which auto-derives the bar from each row's `actions`.
    #[props(default)]
    pub bulk: Option<Element>,
    /// Fired when a **bulk action** is chosen for the current selection, with the
    /// action key and the selected rows. When set, the grid auto-builds a bulk
    /// action bar from the actions common to every selected row (the same
    /// `GridAction`s the per-row kebab shows), so multi-select isn't limited to a
    /// single hand-wired button. Disabled/rowless actions are filtered out. This
    /// renders in addition to any custom `bulk` slot.
    #[props(default)]
    pub on_bulk_action: Option<EventHandler<(&'static str, Vec<T>)>>,
    /// Extra toolbar content (filters, view toggles) — always rendered.
    #[props(default)]
    pub toolbar: Option<Element>,
    #[props(default = false)]
    pub loading: bool,
    #[props(default = "No records match.".to_string())]
    pub empty_label: String,
    #[props(default = 10)]
    pub page_size: usize,
    /// UI density. Defaults to [`GridDensity::Full`]; `Minimal` hides the filter
    /// funnels, column chooser, group-by, and the single-page footer for a sleek
    /// small-data view. Usually set for you by [`AdaptiveDataGrid`].
    #[props(default)]
    pub density: GridDensity,
    /// Optional card body for the grid (gallery) view. When `None`, a tidy card
    /// is auto-built from the columns. Either way the list⇆grid toggle shows.
    #[props(default)]
    pub card: Option<fn(&T) -> Element>,
    /// Optional **custom list-row renderer**. When set, the list view draws this
    /// in place of the per-column `<td>`s — a single full-width cell the host owns
    /// — while the grid keeps the selection checkbox, the action kebab, row click,
    /// sorting, search, filtering, and pagination. This is the per-page "redesign
    /// the list" slot (the gallery counterpart of `card`). `None` = the default
    /// columnar table. Header sorting still uses `columns`, so keep them declared.
    #[props(default)]
    pub row: Option<fn(&T) -> Element>,
    /// Enable the toolbar's built-in CSV export of the *current* view (rows after
    /// search + filters + sort, all pages). The value is the download filename
    /// (e.g. `"products.csv"`). `None` hides the export button.
    #[props(default)]
    pub export_filename: Option<&'static str>,
    /// Server-side **signed/encrypted PDF** export. When set, the export menu adds a
    /// "Signed PDF" item that fires this handler with the current [`GridQuery`]. The
    /// host posts that query to its backend, which renders, digitally signs and
    /// encrypts the document, then returns the file. The grid never signs: signing
    /// keys and timestamp authorities cannot live in the browser.
    #[props(default)]
    pub on_export_signed: Option<EventHandler<GridQuery>>,
    /// Start in grid (gallery) view instead of the row list.
    #[props(default = false)]
    pub grid_default: bool,
    /// Hide the list⇆grid (card) view toggle. For inherently-tabular data — a
    /// permission matrix, a hash-chain ledger — a gallery view is meaningless, so
    /// the toggle is just noise. The grid stays in list/table view.
    #[props(default = false)]
    pub no_card_toggle: bool,
    /// Today's date as ISO `YYYY-MM-DD`, forwarded to date-range filter pickers so
    /// their presets (This Week, Last 30 Days…) and "today" marker are correct. The
    /// grid stays clock-free; the host supplies it. `None` falls back to a default.
    #[props(default)]
    pub today: Option<String>,
    /// A stable, unique key (e.g. `"users-grid"`) under which the user's **layout**
    /// — column order, widths, pins, and hidden columns — is saved to localStorage
    /// and restored on the next visit. `None` disables persistence. Filters / sort /
    /// page are intentionally NOT persisted (they're query state, not layout).
    #[props(default)]
    pub persist_key: Option<&'static str>,

    // ── Remote (server-side) mode ────────────────────────────────────────────
    // When `remote_page` is supplied the grid is **source-agnostic**: instead of
    // querying `rows` locally it draws exactly the page the host fetched, and emits
    // the query it needs through `on_query`. This is how the grid scales past what
    // a browser can hold — the server filters/sorts/paginates and returns one page.
    /// Fires whenever the effective query changes (search / filter / sort / page /
    /// page-size). The host runs its own async fetch and feeds the result back via
    /// `remote_page`. Presence of this handler puts the grid in remote mode.
    #[props(default)]
    pub on_query: Option<EventHandler<GridQuery>>,
    /// The page the host fetched for the latest `on_query`. In remote mode the grid
    /// renders this directly (and `rows` may be empty). `None` while loading.
    #[props(default)]
    pub remote_page: Option<GridPage<T>>,
}

/// Erased entry point: does the `T`-specific projection, then hands a `T`-free
/// snapshot to the single non-generic renderer. Only this thin shell is generated
/// per row type, so the renderer body itself compiles once no matter how many row
/// types a build uses. Selected on wasm by default, or with `force-erased`.
#[cfg(grid_erased)]
#[component]
pub fn DataGrid<T: Clone + PartialEq + 'static>(props: DataGridProps<T>) -> Element {
    use crate::erased::{ErasedCallbacks, ErasedColumn, ErasedGrid, ErasedRow};

    // Erased columns (display metadata only — projections are applied per row).
    // Filter metadata (kind, distinct Set values) is computed here since it needs T.
    let columns: Vec<ErasedColumn> = props
        .columns
        .iter()
        .map(|c| {
            let filterable = c.sort_key.is_some() || c.sort_num.is_some();
            let kind = effective_kind(c);
            let set_values = if kind == FilterKind::Set { distinct_values(&props.rows, c) } else { Vec::new() };
            ErasedColumn {
                key: c.key,
                label: c.label,
                align: c.align,
                width: c.width,
                hide_on_mobile: c.hide_on_mobile,
                aggregate: c.aggregate,
                sortable: filterable,
                editable: c.edit.is_some(),
                filterable,
                filter_kind: kind,
                set_values,
                numeric: c.sort_num.is_some(),
                groupable: c.sort_key.is_some(),
            }
        })
        .collect();

    use crate::erased::ErasedState;
    use std::rc::Rc;

    // ── Interaction state (owned here; the renderer mutates these signals, and
    //    the page memo below re-queries reactively). All non-generic. ──
    let grid = use_signal(|| GridState::new(props.page_size.max(1), props.grid_default));
    let filters = use_signal(HashMap::<&'static str, (FilterOp, String)>::new);
    let extra_sorts = use_signal(Vec::<(&'static str, bool)>::new);
    let group_by = use_signal(|| None::<&'static str>);
    let state = ErasedState { grid, filters, extra_sorts, group_by };

    // Mirror `rows` into a signal so the page memo reacts to dataset changes.
    let mut rows_sig = use_signal(|| props.rows.clone());
    if *rows_sig.peek() != props.rows {
        rows_sig.set(props.rows.clone());
    }

    // Run the engine query → the visible page (indices into the dataset). Memoized
    // so it only recomputes when search/filter/sort/page/rows change.
    let page = {
        let cols = props.columns.clone();
        let search_text = props.search_text;
        use_memo(move || {
            let q = build_grid_query(&cols, &filters.read(), &grid.read(), &extra_sorts.read(), *group_by.read());
            let ecols = engine_columns(&cols);
            LocalProvider::new(rows_sig.read().clone(), &ecols).search_opt(search_text).query(&q)
        })
    };
    let page_val = page.read().clone();

    // Project ONLY the visible page's rows, capturing per-row callbacks that close
    // over the concrete `T` (so the renderer never holds `T`).
    // The group column's text projection, when grouping is active.
    let group_col = group_by.read().and_then(|gk| props.columns.iter().find(|c| c.key == gk).cloned());
    let rows: Vec<ErasedRow> = page_val
        .rows
        .iter()
        .map(|row| {
            let id = (props.row_id)(row);
            ErasedRow {
                id: id.clone(),
                group_value: group_col.as_ref().and_then(|c| c.sort_key.map(|f| f(row))),
                cells: props.columns.iter().map(|c| (c.render)(row)).collect(),
                edit_seed: props.columns.iter().map(|c| c.edit.map(|f| f(row)).unwrap_or_default()).collect(),
                actions: props.actions.map(|f| f(row)).unwrap_or_default(),
                card: props.card.map(|f| f(row)),
                row_slot: props.row.map(|f| f(row)),
                on_click: props.on_row_click.map(|h| {
                    let row = row.clone();
                    Rc::new(move || h.call(row.clone())) as Rc<dyn Fn()>
                }),
                on_action: props.on_action.map(|h| {
                    let row = row.clone();
                    Rc::new(move |k: &'static str| h.call((k, row.clone()))) as Rc<dyn Fn(&'static str)>
                }),
                on_edit: props.on_edit.map(|h| {
                    let row = row.clone();
                    Rc::new(move |key: &'static str, value: String| {
                        h.call(CellEdit { row: row.clone(), key, value });
                    }) as Rc<dyn Fn(&'static str, String)>
                }),
            }
        })
        .collect();

    // Grid-level callbacks. Bulk maps the selected ids back to concrete rows.
    let bulk_lookup = {
        let rows_rc = props.rows.clone();
        let row_id = props.row_id;
        move |ids: &[String]| -> Vec<T> { rows_rc.iter().filter(|r| ids.contains(&row_id(r))).cloned().collect() }
    };
    let callbacks = ErasedCallbacks {
        on_bulk_action: props.on_bulk_action.map(|h| {
            let lookup = bulk_lookup.clone();
            Rc::new(move |k: &'static str, ids: Vec<String>| h.call((k, lookup(&ids))))
                as Rc<dyn Fn(&'static str, Vec<String>)>
        }),
        on_selection: props
            .on_selection
            .map(|h| Rc::new(move |ids: Vec<String>| h.call(ids)) as Rc<dyn Fn(Vec<String>)>),
        on_export_signed: props
            .on_export_signed
            .map(|h| Rc::new(move |q: GridQuery| h.call(q)) as Rc<dyn Fn(GridQuery)>),
    };

    let model = ErasedGrid {
        columns,
        rows,
        callbacks,
        density: props.density,
        selectable: props.selectable,
        loading: props.loading,
        empty_label: props.empty_label.clone(),
        grid_default: props.grid_default,
        no_card_toggle: props.no_card_toggle,
        has_card: props.card.is_some(),
        has_row_slot: props.row.is_some(),
        has_search: props.search_text.is_some(),
        export_filename: props.export_filename,
        today: props.today.clone(),
        persist_key: props.persist_key,
        total: page_val.total,
        page_count: page_val.page_count,
        page: page_val.page,
        aggregates: page_val.aggregates.clone(),
        toolbar: props.toolbar.clone(),
        bulk: props.bulk.clone(),
    };

    rsx! { crate::erased_render::ErasedDataGrid { grid: model, state } }
}

/// The monomorphized renderer: a full copy per row type. Selected on native
/// targets by default, or with `force-mono`.
#[cfg(grid_mono)]
#[component]
pub fn DataGrid<T: Clone + PartialEq + 'static>(props: DataGridProps<T>) -> Element {
    // ── headless controller: the whole interaction state machine lives here ──
    // `grid_state::GridState` owns search / sort / page / page-size / selection /
    // view as one pure, unit-tested value. The renderer holds it in a single
    // signal and calls *intent* methods (set_search, toggle_sort, toggle_row…)
    // on interaction — it no longer reasons about tri-state sort, page resets,
    // or selection toggles itself. (See doc/architecture/headless-extraction.md.)
    let mut grid = use_signal(|| GridState::new(props.page_size.max(1), props.grid_default));
    // Load the saved layout (column order/widths/pins/hidden) once, if a
    // `persist_key` was given. Stored keys are re-mapped to the live `&'static str`
    // column keys (dropping any that no longer exist) so stale storage is harmless.
    let saved_layout = use_hook(|| {
        let all: Vec<&'static str> = props.columns.iter().map(|c| c.key).collect();
        props
            .persist_key
            .and_then(|k| crate::storage::get(&format!("grid:{k}")))
            .map(|raw| {
                let l = GridLayout::decode(&raw);
                (
                    map_static_keys(&l.order, &all),
                    map_static_keys(&l.hidden, &all),
                    map_static_keys(&l.pins, &all),
                    l.widths
                        .iter()
                        .filter_map(|(s, w)| all.iter().copied().find(|k| *k == s.as_str()).map(|k| (k, *w)))
                        .collect::<Vec<(&'static str, f64)>>(),
                )
            })
            .unwrap_or_default()
    });
    let mut menu_for = use_signal(|| None::<String>);
    // The row kebab menu renders in a FIXED layer at the click point so it
    // escapes the table's `overflow-x-auto` clip box (which otherwise hides the
    // menu when there are few rows). We cap its height with CSS so it never
    // runs off the bottom — no JS measuring, which keeps the wasm executor calm.
    let mut menu_xy = use_signal(|| (0.0f64, 0.0f64));
    // Vertical scroll offset of the list viewport (px), updated by the native
    // `onscroll` event. Only consulted when the current page is large enough to
    // virtualize (see `VIRTUAL_THRESHOLD`); otherwise the grid renders every row
    // exactly as before. No JS listeners — `ScrollData` gives us scroll_top
    // directly, keeping the wasm executor calm.
    let mut scroll_top = use_signal(|| 0.0f64);
    // ── plugin state (grid-plugin-api) ───────────────────────────────────────
    // Per-column "contains" filters (empty string = inactive), the filter-row
    // toggle, and secondary multi-sort columns. The PRIMARY sort still lives in
    // `grid-state`; `extra_sorts` are the shift-clicked tie-breakers after it.
    // Per-column filter: an operator + a value string. Empty value (for value-
    // taking ops) = inactive. `IsEmpty`/`IsNotEmpty` need no value.
    let mut filters = use_signal(HashMap::<&'static str, (FilterOp, String)>::new);
    // Which column's filter popover is open (by key), if any. Replaces the old
    // always-on filter row with a per-column popover (a bottom-sheet on mobile).
    let mut filter_popover = use_signal(|| None::<&'static str>);
    let extra_sorts = use_signal(Vec::<(&'static str, bool)>::new);
    // Columns the user has hidden via the column chooser, plus the chooser's
    // open/closed state. Hiding is purely a display concern — a hidden column's
    // sort/filter still applies, and it still exports. Seeded from saved layout.
    let mut hidden_cols = use_signal({
        let h = saved_layout.1.clone();
        move || h.into_iter().collect::<std::collections::HashSet<&'static str>>()
    });
    let mut show_col_menu = use_signal(|| false);
    // Export dropdown (format × scope) open/closed.
    let mut show_export_menu = use_signal(|| false);
    // User-set column widths (px), keyed by column key. A column absent here uses
    // its declared `width`/auto sizing. Driven by the header resize handles.
    let mut col_widths = use_signal({
        let w = saved_layout.3.clone();
        move || w.into_iter().collect::<HashMap<&'static str, f64>>()
    });
    // Active resize drag: (column key, start mouse x, start width). `None` = idle.
    let mut resizing = use_signal(|| None::<(&'static str, f64, f64)>);
    // User column order (display order, by key). Empty = the declared order. Driven
    // by header drag-and-drop reordering. Seeded from saved layout.
    let mut col_order = use_signal({
        let o = saved_layout.0.clone();
        move || o
    });
    // Columns frozen to the left (sticky during horizontal scroll), by key.
    let mut pinned_cols = use_signal({
        let p = saved_layout.2.clone();
        move || p.into_iter().collect::<std::collections::HashSet<&'static str>>()
    });
    // The header currently being dragged for reorder (its key), if any.
    let mut dragging_col = use_signal(|| None::<&'static str>);
    // The cell currently being inline-edited: (row id, column key). `None` = none.
    // A double-click on an editable cell opens its input; Enter/blur commits via
    // `on_edit`, Escape cancels. Only one cell edits at a time.
    let mut editing = use_signal(|| None::<(String, &'static str)>);
    // Live text of the open editor. Tracked separately because in Dioxus 0.7 a
    // keydown/blur event can't read the input's value — only `oninput` carries it.
    let mut edit_draft = use_signal(String::new);
    // Keyboard focus: the (row-within-page, visible-column) index of the active
    // cell, for arrow-key navigation. `None` until the user tabs/clicks into the
    // grid. The focused cell carries `tabindex=0` (roving tabindex); all others
    // are `-1`, so Tab lands on the grid once and arrows move within it — the
    // standard accessible data-grid pattern.
    let mut focus_cell = use_signal(|| None::<(usize, usize)>);
    // Show the cell focus ring ONLY during keyboard navigation. A plain mouse
    // click also focuses a cell (for editing / a11y), but painting the teal box on
    // every click looked like accumulating selection boxes. We flip this true on
    // arrow-key nav and false on pointerdown, so the ring is a keyboard affordance.
    let mut kbd_nav = use_signal(|| false);
    // Group-by: the column key to group the visible rows under (None = flat list),
    // plus which group labels are collapsed and whether the group menu is open.
    // Grouping is a display concern over the current page — subtotals use the same
    // per-column `Aggregate`s as the footer.
    let mut group_by = use_signal(|| None::<&'static str>);
    let mut collapsed_groups = use_signal(std::collections::HashSet::<String>::new);
    let mut show_group_menu = use_signal(|| false);

    // Persist the layout whenever the user rearranges it. Reading the four signals
    // inside the effect registers them as dependencies, so this re-runs exactly
    // when order / hidden / pins / widths change — never on scroll, filter, etc.
    if let Some(pkey) = props.persist_key {
        use_effect(move || {
            // Sort the set/map-derived fields so the encoded string is stable (a
            // HashSet/HashMap iterates in arbitrary order); `order` keeps its
            // meaningful sequence.
            let mut hidden: Vec<String> = hidden_cols.read().iter().map(|k| k.to_string()).collect();
            hidden.sort();
            let mut pins: Vec<String> = pinned_cols.read().iter().map(|k| k.to_string()).collect();
            pins.sort();
            let mut widths: Vec<(String, f64)> = col_widths.read().iter().map(|(k, w)| (k.to_string(), *w)).collect();
            widths.sort_by(|a, b| a.0.cmp(&b.0));
            let layout =
                GridLayout { order: col_order.read().iter().map(|k| k.to_string()).collect(), hidden, pins, widths };
            crate::storage::set(&format!("grid:{pkey}"), &layout.encode());
        });
    }

    // ── pipeline: delegated to grid-core + grid-plugin-api ───────────────────
    // search (grid-core semantics) → per-column filters (FilterStage) →
    // multi-column sort (MultiSortStage) → paginate, all in pure, golden-tested
    // crates. The renderer only maps its columns into `ColumnDef`s, assembles the
    // filter/sort models from its signals, and draws the returned page.
    let has_aggregates = props.columns.iter().any(|c| c.aggregate.is_some());

    // The page is computed by a memo: it re-runs only when a *query-defining*
    // signal it reads changes (search / filters / sort / page / page-size / rows),
    // NOT on scroll, hover, or menu toggles. That's what keeps a million-row grid
    // from re-filtering on every frame. The `LocalProvider` is index-based, so the
    // recompute clones only the page's rows, never the dataset.
    let remote = props.on_query.is_some();

    // In **remote mode** the host owns the data: we emit the effective query on
    // every change and draw whatever page the host fetched. In **local mode** the
    // memoized `LocalProvider` answers from `rows`. Either way we end with one
    // `GridPage<T>` to render — the renderer below is source-agnostic.
    // Mirror the `rows` prop into a signal so the page memo *reacts* to a changed
    // dataset. `use_memo` only re-runs when a signal it reads changes; a plainly
    // captured `Rc` would freeze the memo at the first render's data (an inline
    // cell edit upstream would then never show). Cloning an `Rc` is a refcount
    // bump, so this stays allocation-free.
    let mut rows_sig = use_signal(|| props.rows.clone());
    if *rows_sig.peek() != props.rows {
        rows_sig.set(props.rows.clone());
    }
    let local_page = {
        let cols = props.columns.clone();
        let search_text = props.search_text;
        use_memo(move || {
            if remote {
                // Don't run the local pipeline in remote mode (rows may be empty).
                return GridPage { rows: Vec::new(), total: 0, page: 0, page_count: 1, aggregates: Vec::new() };
            }
            let q = build_grid_query(&cols, &filters.read(), &grid.read(), &extra_sorts.read(), *group_by.read());
            let ecols = engine_columns(&cols);
            let provider = LocalProvider::new(rows_sig.read().clone(), &ecols).search_opt(search_text);
            provider.query(&q)
        })
    };

    // Remote mode: push the query to the host whenever it changes. Reading the
    // query-defining signals inside the effect makes it re-fire exactly then.
    if remote {
        let cols = props.columns.clone();
        let on_query = props.on_query;
        use_effect(move || {
            let q = build_grid_query(&cols, &filters.read(), &grid.read(), &extra_sorts.read(), *group_by.read());
            if let Some(h) = on_query {
                h.call(q);
            }
        });
    }

    // The page to draw: the host's fetched page in remote mode, else the local one.
    let page: GridPage<T> = if remote {
        props.remote_page.clone().unwrap_or(GridPage {
            rows: Vec::new(),
            total: 0,
            page: grid.read().page(),
            page_count: 1,
            aggregates: Vec::new(),
        })
    } else {
        local_page.read().clone()
    };
    let total = page.total;
    let psize = grid.read().page_size();
    let pages = page.page_count;
    let cur = page.page;
    let page_rows: Vec<T> = page.rows.clone();
    let agg_results: Vec<(String, AggValue)> = page.aggregates.clone();

    // ── Export of the current view (CSV / Excel / PDF, filtered or all) ──
    // `scope_all=false` exports the filtered/sorted set (what the user sees);
    // `scope_all=true` ignores search+filters and exports every row (still in the
    // current sort). Re-runs the index-based provider with an unbounded page so the
    // file matches exactly, then projects each row via `csv → sort_key → sort_num`.
    let do_export = {
        let rows = props.rows.clone();
        let cols = props.columns.clone();
        let search = props.search_text;
        let fname = props.export_filename;
        move |format: ExportFormat, scope_all: bool| {
            let Some(fname) = fname else { return };
            // Export keeps the user's sort (group_by = None) — a flat file regardless
            // of the on-screen grouping.
            let mut q = build_grid_query(&cols, &filters.read(), &grid.read(), &extra_sorts.read(), None);
            if scope_all {
                // Whole dataset: drop search + filters, keep the sort.
                q.search.clear();
                q.filters = FilterModel::new();
            }
            q.page = 0;
            q.page_size = usize::MAX;
            q.aggregates.clear();
            let ecols = engine_columns(&cols);
            let out = LocalProvider::new(rows.clone(), &ecols).search_opt(search).query(&q);
            let headers: Vec<String> = cols.iter().map(|c| c.label.to_string()).collect();
            let body: Vec<Vec<String>> = out
                .rows
                .iter()
                .map(|row| {
                    cols.iter()
                        .map(|c| {
                            if let Some(f) = c.csv {
                                f(row)
                            } else if let Some(f) = c.sort_key {
                                f(row)
                            } else if let Some(f) = c.sort_num {
                                let n = f(row);
                                if n.fract() == 0.0 {
                                    format!("{}", n as i64)
                                } else {
                                    n.to_string()
                                }
                            } else {
                                String::new()
                            }
                        })
                        .collect()
                })
                .collect();
            // Filename stem (drop a trailing ".csv" the host may have supplied) +
            // the format's real extension.
            let stem = fname.strip_suffix(".csv").unwrap_or(fname);
            let filename = format!("{stem}.{}", format.extension());
            let data = grid_export::ExportData { headers, rows: body, title: stem.to_string() };
            grid_export::export(&data, format, &filename);
        }
    };

    let sel_count = grid.read().selection_count();
    let is_card = grid.read().view().is_card();
    let page_ids: Vec<String> = page_rows.iter().map(|r| (props.row_id)(r)).collect();
    let all_page_selected = grid.read().is_page_selected(&page_ids);

    // ── Derived bulk actions ──────────────────────────────────────────────────
    // Resolve the selected ids back to rows (scanning `rows`), then compute the
    // actions COMMON to every selected row — the same `GridAction`s the per-row
    // kebab offers, minus disabled ones. This drives the auto bulk action bar so
    // multi-select exposes every applicable action (not just one wired button).
    let selected_rows: Vec<T> = if props.on_bulk_action.is_some() && sel_count > 0 {
        let sel = grid.read().selected_ids();
        let want: std::collections::HashSet<&str> = sel.iter().map(|s| s.as_str()).collect();
        props.rows.iter().filter(|r| want.contains((props.row_id)(r).as_str())).cloned().collect()
    } else {
        Vec::new()
    };
    // Intersection of per-row actions across the selection, preserving the order
    // of the first row's action list. An action survives only if every selected
    // row offers it and none of them mark it disabled.
    let bulk_actions: Vec<GridAction> = match (props.actions, selected_rows.first()) {
        (Some(actions_fn), Some(first)) if !selected_rows.is_empty() => {
            let per_row: Vec<Vec<GridAction>> = selected_rows.iter().map(&actions_fn).collect();
            actions_fn(first)
                .into_iter()
                .filter(|a| a.bulkable)
                .filter(|a| per_row.iter().all(|acts| acts.iter().any(|x| x.key == a.key && !x.disabled)))
                .collect()
        }
        _ => Vec::new(),
    };
    // A column is filterable if it can be projected (it is sortable). The toolbar
    // only offers the filter row when at least one column qualifies.
    let any_filterable = props.columns.iter().any(|c| c.sort_key.is_some() || c.sort_num.is_some());
    // Minimal density: strip the enterprise chrome (filter funnels, column chooser,
    // group-by) and only show the footer when it's actually needed (>1 page).
    let minimal = props.density == GridDensity::Minimal;
    // Columns that can be grouped by (have a text projection). Label of the active
    // group column, for the toolbar button.
    let groupable_cols: Vec<(&'static str, &'static str)> =
        props.columns.iter().filter(|c| c.sort_key.is_some()).map(|c| (c.key, c.label)).collect();
    let group_label = group_by.read().and_then(|k| props.columns.iter().find(|c| c.key == k).map(|c| c.label));
    let active_filter_count = filters.read().values().filter(|(op, v)| filter_is_active(op, v)).count();
    // Total rows ignoring search/filters (for the "Export all" label). In remote
    // mode the host owns the data, so this is a best-effort `rows` length.
    let full_count = props.rows.len();
    // Export formats this build can actually produce (CSV always; Excel/PDF need
    // the `export-rich` feature + a font for PDF). Shown in the export menu.
    // Inline SVG path-data per format (the `ui` crate has no icon module). CSV →
    // table/grid glyph, Excel → spreadsheet sheet, PDF → document. Keeps the menu
    // rows scannable and gives them width so the popover isn't awkwardly narrow.
    const ICON_CSV: &str = r#"<path d="M3 3h18v18H3z"/><path d="M3 9h18"/><path d="M3 15h18"/><path d="M9 3v18"/>"#;
    const ICON_XLSX: &str =
        r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M8 8l8 8"/><path d="M16 8l-8 8"/>"#;
    const ICON_PDF: &str = r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M9 13h6"/><path d="M9 17h6"/>"#;
    let export_formats: Vec<(ExportFormat, &'static str, &'static str)> = [
        (ExportFormat::Csv, "CSV", ICON_CSV),
        (ExportFormat::Xlsx, "Excel", ICON_XLSX),
        (ExportFormat::Pdf, "PDF", ICON_PDF),
    ]
    .into_iter()
    .filter(|(f, _, _)| grid_export::format_available(*f))
    .collect();
    // Active-filter chips: (column key, human label). Built in declared column
    // order so the bar is stable as the user adds/removes filters.
    let active_chips: Vec<(&'static str, String)> = props
        .columns
        .iter()
        .filter_map(|c| {
            filters
                .read()
                .get(c.key)
                .and_then(|(op, v)| chip_label(c.label, effective_kind(c), op, v).map(|lbl| (c.key, lbl)))
        })
        .collect();
    // Columns to actually render (the chooser hides, never removes — engine ops
    // still see every column). Cloned so the rsx closures own their list.
    // All column keys in declared order — cheap (Copy &'static str), captured by
    // the header drag-drop handlers to seed/backfill the reorder.
    let all_keys: Vec<&'static str> = props.columns.iter().map(|c| c.key).collect();

    // Columns to render, in display order: pinned columns first (frozen left),
    // then the user's drag-order, then any declared columns not yet in that order
    // — minus the hidden ones. `order_rank` gives a stable sort key from
    // `col_order` (declared order for keys the user hasn't moved).
    let visible_cols: Vec<GridColumn<T>> = {
        let order = col_order.read();
        let pinned = pinned_cols.read();
        let order_rank = |k: &str| order.iter().position(|x| *x == k).unwrap_or(usize::MAX);
        let mut cols: Vec<GridColumn<T>> =
            props.columns.iter().filter(|c| !hidden_cols.read().contains(c.key)).cloned().collect();
        // Stable sort: pinned-first, then by user order, preserving declared order
        // as the tie-breaker (sort_by is stable).
        cols.sort_by(|a, b| {
            let pa = !pinned.contains(a.key);
            let pb = !pinned.contains(b.key);
            pa.cmp(&pb).then(order_rank(a.key).cmp(&order_rank(b.key)))
        });
        cols
    };
    // Precompute the sticky-left offset (px) for each pinned column, in display
    // order, so frozen columns stack without overlapping during horizontal scroll.
    let pinned_offsets: HashMap<&'static str, f64> = {
        let mut acc = if props.selectable { 40.0 } else { 0.0 }; // checkbox col width
        let mut m = HashMap::new();
        for c in visible_cols.iter().filter(|c| pinned_cols.read().contains(c.key)) {
            m.insert(c.key, acc);
            acc += *col_widths.read().get(c.key).unwrap_or(&160.0);
        }
        m
    };

    // After any selection intent, surface the new id set to the screen.
    let notify_sel = {
        let on_selection = props.on_selection;
        move || {
            if let Some(h) = on_selection {
                h.call(grid.read().selected_ids());
            }
        }
    };

    let from = if total == 0 { 0 } else { cur * psize + 1 };
    let to = (cur * psize + page_rows.len()).min(total);

    // ── row virtualization (list view only) ──────────────────────────────────
    // Above `VIRTUAL_THRESHOLD` rows on a page (e.g. "100 / page"), rendering
    // every `<tr>` floods the DOM and the wasm diff. Instead we render only the
    // window the user can see, plus an overscan, and pad it top & bottom with two
    // spacer rows of the exact missing height so the scrollbar geometry is right.
    //
    // Below the threshold — the entire common case (page sizes 10/25/50) —
    // `virtualize` is false and we render exactly as before: zero behaviour
    // change, no fixed heights, natural row flow.
    const VIRTUAL_THRESHOLD: usize = 60;
    // Fixed virtual-row height. The spacer math needs every row to be *exactly* this
    // tall, so when virtualizing we both force the row to `ROW_H` and clip its cells
    // (`.dg-vrow` below) — content can't grow it, so the estimate can never drift and
    // the scrollbar can't lurch to the end. 48px comfortably fits a single line plus
    // a badge/avatar; denser than that is what virtualization is for anyway.
    const ROW_H: f64 = 48.0; // px.
    const VIEWPORT_H: f64 = 560.0; // px; the `max-h` of the scroll container below.
    const OVERSCAN: usize = 6; // extra rows above & below the viewport.
                               // Grouping changes the row structure (header rows + collapsible sections), so
                               // it's incompatible with the simple fixed-height virtualization window — we
                               // turn virtualization off while grouped (grouping is for moderate sets anyway).
    let grouped = !is_card && group_by.read().is_some();
    // A custom `row` slot (timeline-style lists with avatars/icons) has variable,
    // non-uniform heights that the single-height virtualization window can't model,
    // so those render flat. Only the plain, uniform-height column table virtualizes
    // (and even there rows are pinned + clipped to ROW_H so the assumption holds).
    let virtualize = !is_card && !grouped && props.row.is_none() && page_rows.len() > VIRTUAL_THRESHOLD;
    let (vstart, vend) = if virtualize {
        let st = *scroll_top.read();
        let first = (st / ROW_H).floor() as usize;
        let visible = (VIEWPORT_H / ROW_H).ceil() as usize;
        let start = first.saturating_sub(OVERSCAN);
        let end = (first + visible + OVERSCAN).min(page_rows.len());
        (start, end)
    } else {
        (0, page_rows.len())
    };
    let pad_top = vstart as f64 * ROW_H;
    let pad_bottom = (page_rows.len() - vend) as f64 * ROW_H;

    // ── keyboard navigation model ─────────────────────────────────────────────
    // Per visible column: its key and whether it's editable. The grid-level
    // keydown handler uses these (plus the page row ids) to move the focused cell
    // with arrows, open an editor on Enter, and toggle selection on Space — the
    // roving-tabindex pattern that makes the table operable without a mouse.
    let nav_cols: Vec<(&'static str, bool)> =
        visible_cols.iter().map(|c| (c.key, c.edit.is_some() && props.on_edit.is_some())).collect();
    // Edit projections by key, so Enter can seed the editor for a focused cell.
    let nav_edit_fns: HashMap<&'static str, fn(&T) -> String> =
        visible_cols.iter().filter_map(|c| c.edit.map(|f| (c.key, f))).collect();
    let nav_rows = page_rows.clone();
    let nav_max_row = page_rows.len();
    let nav_max_col = nav_cols.len();

    // The kebab dropdown, reused by both the row list and the card grid. Renders
    // a click-away veil plus a fixed-position menu at the recorded click point.
    let render_menu = move |row: T, acts: Vec<GridAction>| -> Element {
        let first_danger = acts.iter().position(|a| a.danger);
        // `menu_xy` is the click point (viewport CSS px). The menu is fixed and
        // anchored to the right edge under the kebab. Estimate its height from the
        // item count and FLIP it above the click when there isn't room below, and
        // CLAMP so it never runs off-screen — otherwise a kebab low on the page
        // opens a menu that falls off the bottom.
        let (mx, my) = *menu_xy.read();
        let n_items = acts.len().max(1);
        // ~38px per row + 8px padding; capped like the real menu.
        let est_h = (n_items as f64 * 38.0 + 8.0).min(360.0);
        // Use a sensible viewport height fallback; the menu also has its own
        // max-height + scroll, so an estimate is fine for the flip decision.
        let flip_up = my > 560.0; // lower portion of a typical viewport → open upward
        let vpos = if flip_up {
            // Anchor the menu's BOTTOM just above the click point.
            format!("bottom:calc(100vh - {my}px + 8px); max-height:calc({my}px - 16px);")
        } else {
            format!("top:calc({my}px + 8px); max-height:calc(100vh - {my}px - 24px);")
        };
        let _ = est_h;
        let menu_style = format!(
            "position:fixed; right:calc(100vw - {mx}px - 4px); {vpos} z-index:60; overflow-y:auto; box-shadow: 0 16px 40px -16px rgba(2,6,23,0.8);"
        );
        rsx! {
            div {
                class: "dxg-veil",
                onclick: move |_| menu_for.set(None),
            }
            div {
                // Position (flip/clamp) is runtime → inline; look is CSS (.dxg-menu).
                class: "dxg-menu dxg-action-menu",
                role: "menu",
                style: "{menu_style}",
                for (i , a) in acts.into_iter().enumerate() {
                    {
                        let akey = a.key;
                        let row_a = row.clone();
                        let show_sep = first_danger == Some(i) && i > 0;
                        let variant = if a.danger { "danger" } else { "default" };
                        rsx! {
                            if show_sep {
                                div { class: "dxg-menu-divider" }
                            }
                            button {
                                r#type: "button",
                                class: "dxg-menu-item",
                                role: "menuitem",
                                "data-variant": variant,
                                disabled: a.disabled,
                                onclick: move |e: MouseEvent| {
                                    e.stop_propagation();
                                    menu_for.set(None);
                                    if let Some(h) = props.on_action {
                                        h.call((akey, row_a.clone()));
                                    }
                                },
                                if let Some(p) = a.icon {
                                    svg {
                                        class: "dxg-menu-icon",
                                        width: "15",
                                        height: "15",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "1.9",
                                        stroke_linecap: "round",
                                        stroke_linejoin: "round",
                                        dangerous_inner_html: "{p}",
                                    }
                                }
                                span { class: "dxg-menu-item-label", "{a.label}" }
                            }
                        }
                    }
                }
            }
        }
    };

    // The body of a column's filter popover — the type-correct control(s) for that
    // column. Extracted so the desktop popover and the mobile bottom-sheet share
    // one implementation. Writing a filter resets to page 0.
    let render_filter_body = {
        let columns = props.columns.clone();
        let rows_for_distinct = rows_sig;
        let today = props.today.clone();
        move |key: &'static str| -> Element {
            let Some(col) = columns.iter().find(|c| c.key == key).cloned() else {
                return rsx! {};
            };
            let kind = effective_kind(&col);
            let cur = filters.read().get(key).cloned();
            let cur_val = cur.as_ref().map(|(_, v)| v.clone()).unwrap_or_default();
            match kind {
                // ── Multi-select set: checkbox list of distinct values ──
                FilterKind::Set => {
                    let chosen: std::collections::HashSet<String> =
                        cur_val.split('\u{1}').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                    let values = distinct_values(&rows_for_distinct.read(), &col);
                    rsx! {
                        div { class: "dxg-filter-set",
                            if values.is_empty() {
                                span { class: "dxg-filter-empty", "No values" }
                            }
                            for v in values {
                                {
                                    let on = chosen.contains(&v);
                                    let vv = v.clone();
                                    rsx! {
                                        label { class: "dxg-filter-option",
                                            input {
                                                r#type: "checkbox",
                                                class: "dxg-checkbox",
                                                checked: on,
                                                onchange: move |_| {
                                                    let mut set: std::collections::BTreeSet<String> = filters
                                                        .read()
                                                        .get(key)
                                                        .map(|(_, s)| {
                                                            s
                                                                .split('\u{1}')
                                                                .filter(|x| !x.is_empty())
                                                                .map(|x| x.to_string())
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();
                                                    if set.contains(&vv) {
                                                        set.remove(&vv);
                                                    } else {
                                                        set.insert(vv.clone());
                                                    }
                                                    if set.is_empty() {
                                                        filters.write().remove(key);
                                                    } else {
                                                        filters
                                                            .write()
                                                            .insert(
                                                                key,
                                                                (FilterOp::In, set.into_iter().collect::<Vec<_>>().join("\u{1}")),
                                                            );
                                                    }
                                                    grid.write().set_page(0);
                                                },
                                            }
                                            span { class: "dxg-filter-option-label", "{v}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // ── Star rating: pick a minimum (click again to clear) ──
                FilterKind::Rating => {
                    let chosen: f64 = cur_val.parse().unwrap_or(0.0);
                    rsx! {
                        div { class: "dxg-filter-rating",
                            for star in 1..=5u8 {
                                {
                                    let s = star as f64;
                                    let on = s <= chosen;
                                    rsx! {
                                        span {
                                            class: "dxg-star",
                                            "data-on": if on { "true" },
                                            title: "at least {star} stars",
                                            onclick: move |_| {
                                                rating_star_click(key, chosen, s, filters, grid);
                                            },
                                            if on {
                                                "★"
                                            } else {
                                                "☆"
                                            }
                                        }
                                    }
                                }
                            }
                            if chosen > 0.0 {
                                span { class: "dxg-filter-hint", "≥ {chosen as u8}" }
                            }
                        }
                    }
                }
                // ── Present / absent toggle ──
                FilterKind::HasValue => {
                    let sel = match cur.as_ref().map(|(o, _)| o) {
                        Some(FilterOp::IsNotEmpty) => "has",
                        Some(FilterOp::IsEmpty) => "none",
                        _ => "any",
                    };
                    rsx! {
                        select {
                            class: "dxg-filter-select",
                            value: "{sel}",
                            onchange: move |e: FormEvent| {
                                match e.value().as_str() {
                                    "has" => {
                                        filters.write().insert(key, (FilterOp::IsNotEmpty, String::new()));
                                    }
                                    "none" => {
                                        filters.write().insert(key, (FilterOp::IsEmpty, String::new()));
                                    }
                                    _ => {
                                        filters.write().remove(key);
                                    }
                                }
                                grid.write().set_page(0);
                            },
                            option { value: "any", selected: sel == "any", "Any" }
                            option { value: "has", selected: sel == "has", "Has file" }
                            option { value: "none", selected: sel == "none", "No file" }
                        }
                    }
                }
                // ── Date range: a real calendar range picker (presets + month grid) ──
                FilterKind::DateRange => {
                    let (lo, hi) =
                        cur_val.split_once('|').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                    rsx! {
                        crate::DateRangePicker {
                            from: lo,
                            to: hi,
                            today: today.clone(),
                            hide_presets: false,
                            onchange: move |(f, t): (String, String)| {
                                // Store the packed "from|to"; `push_column_filters` re-expands
                                // it to >= / <= over the ISO-date text projection.
                                if f.trim().is_empty() && t.trim().is_empty() {
                                    filters.write().remove(key);
                                } else {
                                    filters.write().insert(key, (FilterOp::GreaterOrEqual, format!("{f}|{t}")));
                                }
                                grid.write().set_page(0);
                                filter_popover.set(None);
                            },
                        }
                    }
                }
                // ── Numeric range: a compact min–max pair (value packed "lo|hi") ──
                FilterKind::Range => {
                    let (lo, hi) =
                        cur_val.split_once('|').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                    let set_range = move |lo: String, hi: String| {
                        if lo.trim().is_empty() && hi.trim().is_empty() {
                            filters.write().remove(key);
                        } else {
                            filters.write().insert(key, (FilterOp::GreaterOrEqual, format!("{lo}|{hi}")));
                        }
                        grid.write().set_page(0);
                    };
                    rsx! {
                        div { class: "dxg-filter-range",
                            input {
                                r#type: "number",
                                placeholder: "from",
                                value: "{lo}",
                                class: "dxg-filter-input",
                                oninput: {
                                    let hi = hi.clone();
                                    let mut set = set_range;
                                    move |e: FormEvent| set(e.value(), hi.clone())
                                },
                            }
                            span { class: "dxg-filter-range-sep", "–" }
                            input {
                                r#type: "number",
                                placeholder: "to",
                                value: "{hi}",
                                class: "dxg-filter-input",
                                oninput: {
                                    let lo = lo.clone();
                                    let mut set = set_range;
                                    move |e: FormEvent| set(lo.clone(), e.value())
                                },
                            }
                        }
                    }
                }
                // ── Text / Number: operator select + value input ──
                _ => {
                    let ops = ops_for(kind == FilterKind::Number);
                    let cur_op = cur.as_ref().map(|(o, _)| o.clone()).unwrap_or_else(|| ops[0].0.clone());
                    let valueless = op_is_valueless(&cur_op);
                    let op_now = cur_op.clone();
                    rsx! {
                        div { class: "dxg-filter-textnum",
                            select {
                                class: "dxg-filter-select",
                                title: "Filter operator",
                                value: "{cur_op:?}",
                                onchange: move |e: FormEvent| {
                                    let picked = e.value();
                                    if let Some(op) = ops
                                        .iter()
                                        .find(|(o, _)| format!("{o:?}") == picked)
                                        .map(|(o, _)| o.clone())
                                    {
                                        let prev_val = filters
                                            .read()
                                            .get(key)
                                            .map(|(_, v)| v.clone())
                                            .unwrap_or_default();
                                        if op_is_valueless(&op) {
                                            filters.write().insert(key, (op, String::new()));
                                        } else if prev_val.trim().is_empty() {
                                            filters.write().remove(key);
                                            filters.write().insert(key, (op, prev_val));
                                        } else {
                                            filters.write().insert(key, (op, prev_val));
                                        }
                                        grid.write().set_page(0);
                                    }
                                },
                                for (op , label) in ops.iter() {
                                    option {
                                        value: "{op:?}",
                                        selected: format!("{op:?}") == format!("{cur_op:?}"),
                                        "{label}"
                                    }
                                }
                            }
                            if !valueless {
                                input {
                                    r#type: if kind == FilterKind::Number { "number" } else { "text" },
                                    class: "dxg-filter-input",
                                    placeholder: "Value…",
                                    value: "{cur_val}",
                                    oninput: move |e: FormEvent| {
                                        let v = e.value();
                                        let op = filters
                                            .read()
                                            .get(key)
                                            .map(|(o, _)| o.clone())
                                            .unwrap_or_else(|| op_now.clone());
                                        if v.trim().is_empty() {
                                            filters.write().remove(key);
                                        } else {
                                            filters.write().insert(key, (op, v));
                                        }
                                        grid.write().set_page(0);
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    // One data `<tr>`, shared by the flat list and the grouped sections. `r_idx` is
    // the row's focus coordinate (page-relative); `fixed_h` applies the virtual row
    // height. Extracted so grouping can interleave group-header rows without
    // duplicating the cell / editing / focus / actions markup.
    let render_data_row = {
        let visible_cols = visible_cols.clone();
        let pinned_offsets = pinned_offsets.clone();
        move |row: T, r_idx: usize, fixed_h: bool| -> Element {
            let id = (props.row_id)(&row);
            let is_sel = grid.read().is_selected(&id);
            let row_for_click = row.clone();
            // Whether the custom-row cell should look/act clickable.
            let clickable_row = props.on_row_click.is_some();
            // When virtualizing, pin the row to exactly ROW_H so the rendered height
            // can never diverge from the spacer math (a few px of drift per row makes
            // the scrollbar lurch to the end). We force it on the row AND clip cells
            // (below) so tall content can't push past it. Non-virtual rows flow free.
            let h = if fixed_h { format!("height:{ROW_H}px") } else { String::new() };
            rsx! {
                tr {
                    // Selection + virtualization are state; borders/hover/fill are CSS
                    // ([data-selected], .dxg-row[data-virtualized] clips to ROW_H).
                    class: "dxg-row",
                    style: "{h}",
                    role: "row",
                    "data-selected": if is_sel { "true" },
                    "data-virtualized": if fixed_h { "true" },
                    "data-clickable": if props.on_row_click.is_some() { "true" },
                    "aria-selected": if is_sel { "true" } else { "false" },
                    if props.selectable {
                        td {
                            class: "dxg-cell dxg-select-cell",
                            "data-row-slot": if props.row.is_some() { "true" },
                            role: "gridcell",
                            input {
                                r#type: "checkbox",
                                class: "dxg-checkbox",
                                "aria-label": "Select row",
                                checked: is_sel,
                                onclick: move |e: MouseEvent| e.stop_propagation(),
                                onchange: {
                                    let id = id.clone();
                                    move |_| {
                                        grid.write().toggle_row(&id);
                                        notify_sel();
                                    }
                                },
                            }
                        }
                    }
                    // Custom list-row slot: one full-width cell the host renders,
                    // spanning all data columns. Keeps the checkbox + kebab cells.
                    if let Some(row_fn) = props.row {
                        td {
                            // Full-width custom row slot spanning all data columns.
                            class: "dxg-cell dxg-row-slot",
                            "data-has-checkbox": if props.selectable { "true" },
                            "data-clickable": if clickable_row { "true" },
                            colspan: "{visible_cols.len().max(1)}",
                            role: "gridcell",
                            onclick: {
                                let row_c2 = row_for_click.clone();
                                move |_| {
                                    if let Some(h) = props.on_row_click {
                                        h.call(row_c2.clone());
                                    }
                                }
                            },
                            {(row_fn)(&row)}
                        }
                    }
                    for (c_idx , col) in visible_cols.iter().enumerate() {
                        if props.row.is_none() {
                        {
                            let align_attr = match col.align {
                                GridAlign::Right => "end",
                                GridAlign::Center => "center",
                                _ => "start",
                            };
                            let key = col.key;
                            let edit_fn = col.edit;
                            let render_fn = col.render;
                            let editable = edit_fn.is_some() && props.on_edit.is_some();
                            let is_editing = editable && *editing.read() == Some((id.clone(), key));
                            let clickable = props.on_row_click.is_some() && !editable;
                            let row_c = row_for_click.clone();
                            // Runtime-computed geometry stays inline (widths, pin left).
                            // The pinned *appearance* is CSS via [data-pinned].
                            let mut style = match col_widths.read().get(col.key) {
                                Some(w) => format!("width:{w}px; min-width:{w}px; max-width:{w}px;"),
                                None => String::new(),
                            };
                            let is_pinned = pinned_offsets.contains_key(col.key);
                            if let Some(off) = pinned_offsets.get(col.key) {
                                style.push_str(&format!(" left:{off}px;"));
                            }
                            let is_focused = *focus_cell.read() == Some((r_idx, c_idx));
                            // Keyboard-nav focus ring is state; the *look* (inset ring)
                            // is CSS via .dxg-cell[data-kbd-focus]. No inline color here.
                            let kbd_focused = is_focused && *kbd_nav.read();
                            let tab_index = if is_focused
                                || (focus_cell.read().is_none() && r_idx == 0 && c_idx == 0)
                            {
                                0
                            } else {
                                -1
                            };
                            rsx! {
                                td {
                                    class: "dxg-cell dxg-body-cell",
                                    style: "{style}",
                                    role: "gridcell",
                                    "data-align": align_attr,
                                    "data-hide-mobile": if col.hide_on_mobile { "true" },
                                    "data-pinned": if is_pinned { "true" },
                                    "data-clickable": if clickable { "true" },
                                    "data-kbd-focus": if kbd_focused { "true" },
                                    tabindex: "{tab_index}",
                                    onpointerdown: move |_| kbd_nav.set(false),
                                    onfocusin: move |_| focus_cell.set(Some((r_idx, c_idx))),
                                    ondoubleclick: {
                                        let row_d = row.clone();
                                        let id_d = id.clone();
                                        move |e: MouseEvent| {
                                            if editable {
                                                e.stop_propagation();
                                                if let Some(f) = edit_fn {
                                                    edit_draft.set(f(&row_d));
                                                }
                                                editing.set(Some((id_d.clone(), key)));
                                            }
                                        }
                                    },
                                    onclick: move |_| {
                                        if clickable {
                                            if let Some(h) = props.on_row_click {
                                                h.call(row_c.clone());
                                            }
                                        }
                                    },
                                    if is_editing {
                                        {
                                            let seed = (edit_fn.unwrap())(&row);
                                            let row_e = row.clone();
                                            let mut commit = move || {
                                                if editing.peek().is_none() {
                                                    return;
                                                }
                                                if let Some(h) = props.on_edit {
                                                    h.call(CellEdit {
                                                        row: row_e.clone(),
                                                        key,
                                                        value: edit_draft.peek().clone(),
                                                    });
                                                }
                                                editing.set(None);
                                            };
                                            rsx! {
                                                input {
                                                    r#type: "text",
                                                    class: "dxg-edit-input",
                                                    autofocus: true,
                                                    initial_value: "{seed}",
                                                    onclick: move |e: MouseEvent| e.stop_propagation(),
                                                    oninput: move |e: FormEvent| edit_draft.set(e.value()),
                                                    onkeydown: {
                                                        let mut commit = commit.clone();
                                                        move |e: KeyboardEvent| {
                                                            match e.key() {
                                                                Key::Enter => {
                                                                    e.prevent_default();
                                                                    commit();
                                                                }
                                                                Key::Escape => {
                                                                    e.prevent_default();
                                                                    editing.set(None);
                                                                }
                                                                _ => {}
                                                            }
                                                        }
                                                    },
                                                    onblur: move |_| commit(),
                                                }
                                            }
                                        }
                                    } else {
                                        {(render_fn)(&row)}
                                    }
                                }
                            }
                        }
                        }
                    }
                    if let Some(actions_fn) = props.actions {
                        td { class: "dxg-cell dxg-action-cell",
                            div { class: "dxg-action-cell-inner",
                                for qa in actions_fn(&row).into_iter().filter(|a| a.quick && a.icon.is_some()) {
                                    {
                                        let akey = qa.key;
                                        let row_q = row.clone();
                                        let p = qa.icon.unwrap();
                                        rsx! {
                                            button {
                                                r#type: "button",
                                                title: "{qa.label}",
                                                "aria-label": "{qa.label}",
                                                class: "dxg-icon-button dxg-quick-action",
                                                onclick: move |e: MouseEvent| {
                                                    e.stop_propagation();
                                                    menu_for.set(None);
                                                    if let Some(h) = props.on_action {
                                                        h.call((akey, row_q.clone()));
                                                    }
                                                },
                                                svg {
                                                    width: "15",
                                                    height: "15",
                                                    view_box: "0 0 24 24",
                                                    fill: "none",
                                                    stroke: "currentColor",
                                                    stroke_width: "1.9",
                                                    stroke_linecap: "round",
                                                    stroke_linejoin: "round",
                                                    dangerous_inner_html: "{p}",
                                                }
                                            }
                                        }
                                    }
                                }
                                button {
                                    r#type: "button",
                                    "aria-label": "More actions",
                                    "aria-haspopup": "menu",
                                    class: "dxg-icon-button dxg-kebab",
                                    onclick: {
                                        let id = id.clone();
                                        move |e: MouseEvent| {
                                            e.stop_propagation();
                                            let c = e.client_coordinates();
                                            menu_xy.set((c.x, c.y));
                                            let open = menu_for.read().as_deref() == Some(id.as_str());
                                            menu_for.set(if open { None } else { Some(id.clone()) });
                                        }
                                    },
                                    svg {
                                        width: "15",
                                        height: "15",
                                        view_box: "0 0 24 24",
                                        fill: "currentColor",
                                        circle { cx: "12", cy: "5", r: "1.7" }
                                        circle { cx: "12", cy: "12", r: "1.7" }
                                        circle { cx: "12", cy: "19", r: "1.7" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        div {
            class: "dxg-root",
            "data-view": if is_card { "card" } else { "table" },
            "data-loading": if props.loading { "true" },
            // Drag overlay: only mounted while a column is being resized. It covers
            // the viewport so the pointer keeps reporting moves even outside the
            // header, and commits the new width on release — all via Dioxus events
            // (no global listeners, which would corrupt the wasm executor).
            if let Some((key, start_x, start_w)) = *resizing.read() {
                div {
                    class: "dxg-resize-overlay",
                    onmousemove: move |e: MouseEvent| {
                        let dx = e.client_coordinates().x - start_x;
                        let w = (start_w + dx).clamp(64.0, 800.0);
                        col_widths.write().insert(key, w);
                    },
                    onmouseup: move |_| resizing.set(None),
                    onmouseleave: move |_| resizing.set(None),
                }
            }
            // ── Toolbar: [select-page] · search · custom slot · bulk slot ────
            div { class: "dxg-toolbar",
                // Select-all-on-page — the FIRST control in the filter row, before
                // search, on every grid (list, card, custom-row). An icon-first
                // toggle: a checkbox-grid glyph + short "Page" label, tooltip spells
                // out the scope. Flips to "Deselect" once the page is selected.
                if props.selectable {
                    button {
                        r#type: "button",
                        title: if all_page_selected { "Deselect all rows on this page" } else { "Select all rows on this page" },
                        "aria-pressed": all_page_selected,
                        class: "dxg-button",
                        "data-active": if all_page_selected { "true" },
                        onclick: {
                            let ids = page_ids.clone();
                            move |_| {
                                grid.write().toggle_page(&ids);
                                notify_sel();
                            }
                        },
                        // Checkbox-grid icon (rows of a page, one ticked).
                        svg {
                            width: "15", height: "15", view_box: "0 0 24 24",
                            fill: "none", stroke: "currentColor", stroke_width: "2",
                            stroke_linecap: "round", stroke_linejoin: "round",
                            rect { x: "3", y: "3", width: "18", height: "18", rx: "2" }
                            path { d: "m8 12 2.5 2.5L16 9" }
                        }
                        span { class: "dxg-button-label dxg-sm-only",
                            if all_page_selected { "Deselect" } else { "Page" }
                        }
                    }
                }
                if props.search_text.is_some() {
                    div { class: "dxg-search", role: "search",
                        span { class: "dxg-search-icon", "aria-hidden": "true",
                            svg {
                                width: "14",
                                height: "14",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                circle { cx: "11", cy: "11", r: "8" }
                                path { d: "m21 21-4.3-4.3" }
                            }
                        }
                        input {
                            r#type: "text",
                            class: "dxg-search-input",
                            placeholder: "Search…",
                            value: grid.read().search().to_string(),
                            oninput: move |e: FormEvent| {
                                grid.write().set_search(e.value());
                            },
                        }
                        if !grid.read().search().trim().is_empty() {
                            button {
                                r#type: "button",
                                "aria-label": "Clear search",
                                class: "dxg-search-clear",
                                onclick: move |_| {
                                    grid.write().clear_search();
                                },
                                svg {
                                    width: "12",
                                    height: "12",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2.4",
                                    stroke_linecap: "round",
                                    path { d: "M18 6 6 18" }
                                    path { d: "m6 6 12 12" }
                                }
                            }
                        }
                    }
                }
                if let Some(tb) = props.toolbar.clone() {
                    {tb}
                }
                // Right side: export + filters toggle + list ⇆ grid view toggle.
                div { class: "dxg-toolbar-spacer" }
                if props.export_filename.is_some() || props.on_export_signed.is_some() {
                    div { class: "dxg-export",
                        button {
                            r#type: "button",
                            title: "Export data",
                            class: "dxg-button dxg-export-trigger",
                            onclick: move |_| {
                                let v = *show_export_menu.peek();
                                show_export_menu.set(!v);
                            },
                            svg {
                                width: "14",
                                height: "14",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                path { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }
                                polyline { points: "7 10 12 15 17 10" }
                                line {
                                    x1: "12",
                                    y1: "15",
                                    x2: "12",
                                    y2: "3",
                                }
                            }
                            span { class: "dxg-button-label", "Export" }
                            svg {
                                class: "dxg-chevron",
                                width: "12",
                                height: "12",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2.4",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                polyline { points: "6 9 12 15 18 9" }
                            }
                        }
                        if *show_export_menu.read() {
                            // Click-away veil + dropdown: a format column × a scope choice.
                            // `onpointerdown` (Dioxus's delegated `click` doesn't fire on a
                            // bare veil div) and a fixed `w-60` so the popover has a calm,
                            // standard width rather than hugging the short format labels.
                            div {
                                class: "dxg-veil",
                                onpointerdown: move |_| show_export_menu.set(false),
                            }
                            div { class: "dxg-menu dxg-export-menu", role: "menu",
                                if props.export_filename.is_some() {
                                    div { class: "dxg-menu-label", "Filtered ({total} rows)" }
                                    for (fmt , label , icon) in export_formats.iter().cloned() {
                                        {
                                            let export = do_export.clone();
                                            rsx! {
                                                button {
                                                    r#type: "button",
                                                    class: "dxg-menu-item",
                                                    role: "menuitem",
                                                    onclick: move |_| {
                                                        export(fmt, false);
                                                        show_export_menu.set(false);
                                                    },
                                                    svg {
                                                        class: "dxg-menu-icon",
                                                        width: "16", height: "16", view_box: "0 0 24 24",
                                                        fill: "none", stroke: "currentColor", stroke_width: "2",
                                                        stroke_linecap: "round", stroke_linejoin: "round",
                                                        dangerous_inner_html: "{icon}",
                                                    }
                                                    span { class: "dxg-menu-item-label", "{label}" }
                                                }
                                            }
                                        }
                                    }
                                    div { class: "dxg-menu-divider" }
                                    div { class: "dxg-menu-label", "All ({full_count} rows)" }
                                    for (fmt , label , icon) in export_formats.iter().cloned() {
                                        {
                                            let export = do_export.clone();
                                            rsx! {
                                                button {
                                                    r#type: "button",
                                                    class: "dxg-menu-item",
                                                    role: "menuitem",
                                                    onclick: move |_| {
                                                        export(fmt, true);
                                                        show_export_menu.set(false);
                                                    },
                                                    svg {
                                                        class: "dxg-menu-icon",
                                                        width: "16", height: "16", view_box: "0 0 24 24",
                                                        fill: "none", stroke: "currentColor", stroke_width: "2",
                                                        stroke_linecap: "round", stroke_linejoin: "round",
                                                        dangerous_inner_html: "{icon}",
                                                    }
                                                    span { class: "dxg-menu-item-label", "{label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                                // Signed / encrypted PDF — handled server-side. The grid
                                // only emits the current query; the host's backend renders,
                                // signs (PAdES), and encrypts, then returns the file.
                                if props.on_export_signed.is_some() {
                                    div { class: "dxg-menu-divider" }
                                    button {
                                        r#type: "button",
                                        class: "dxg-menu-item",
                                        role: "menuitem",
                                        onclick: {
                                            let cols = props.columns.clone();
                                            move |_| {
                                                if let Some(h) = props.on_export_signed {
                                                    let q = build_grid_query(
                                                        &cols,
                                                        &filters.read(),
                                                        &grid.read(),
                                                        &extra_sorts.read(),
                                                        *group_by.read(),
                                                    );
                                                    h.call(q);
                                                }
                                                show_export_menu.set(false);
                                            }
                                        },
                                        svg {
                                            class: "dxg-menu-icon dxg-menu-icon-accent",
                                            width: "14",
                                            height: "14",
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                            rect {
                                                x: "3",
                                                y: "11",
                                                width: "18",
                                                height: "11",
                                                rx: "2",
                                            }
                                            path { d: "M7 11V7a5 5 0 0 1 10 0v4" }
                                        }
                                        span { class: "dxg-menu-item-label", "Signed PDF" }
                                        span { class: "dxg-menu-item-hint", "server" }
                                    }
                                }
                            }
                        }
                    }
                }
                // Column chooser (list view only): show/hide columns. Hidden in
                // Minimal density (few columns, nothing to manage).
                if !is_card && props.columns.len() > 1 && !minimal {
                    div { class: "dxg-dropdown",
                        button {
                            r#type: "button",
                            title: "Show / hide columns",
                            class: "dxg-button",
                            "data-active": if !hidden_cols.read().is_empty() { "true" },
                            onclick: move |_| {
                                let v = *show_col_menu.peek();
                                show_col_menu.set(!v);
                            },
                            svg {
                                width: "14",
                                height: "14",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                rect {
                                    x: "3",
                                    y: "3",
                                    width: "18",
                                    height: "18",
                                    rx: "2",
                                }
                                line {
                                    x1: "9",
                                    y1: "3",
                                    x2: "9",
                                    y2: "21",
                                }
                                line {
                                    x1: "15",
                                    y1: "3",
                                    x2: "15",
                                    y2: "21",
                                }
                            }
                            span { class: "dxg-button-label", "Columns" }
                        }
                        if *show_col_menu.read() {
                            // click-away veil + dropdown
                            div {
                                class: "dxg-veil",
                                onclick: move |_| show_col_menu.set(false),
                            }
                            div { class: "dxg-menu dxg-column-menu", role: "menu",
                                for col in props.columns.iter() {
                                    {
                                        let key = col.key;
                                        let shown = !hidden_cols.read().contains(key);
                                        // Don't let the user hide the last visible column.
                                        let is_last = shown && hidden_cols.read().len() + 1 >= props.columns.len();
                                        let pinned = pinned_cols.read().contains(key);
                                        rsx! {
                                            div { class: "dxg-column-menu-row",
                                                label { class: "dxg-column-menu-toggle",
                                                    input {
                                                        r#type: "checkbox",
                                                        class: "dxg-checkbox",
                                                        checked: shown,
                                                        disabled: is_last,
                                                        onchange: move |_| {
                                                            let mut h = hidden_cols.write();
                                                            if h.contains(key) {
                                                                h.remove(key);
                                                            } else {
                                                                h.insert(key);
                                                            }
                                                        },
                                                    }
                                                    span { class: "dxg-column-menu-label", "{col.label}" }
                                                }
                                                // Pin/unpin (freeze left). 📌 when pinned.
                                                button {
                                                    r#type: "button",
                                                    title: if pinned { "Unpin column" } else { "Pin column (freeze left)" },
                                                    class: "dxg-pin-button",
                                                    "data-pinned": if pinned { "true" },
                                                    onclick: move |_| {
                                                        let mut p = pinned_cols.write();
                                                        if p.contains(key) {
                                                            p.remove(key);
                                                        } else {
                                                            p.insert(key);
                                                        }
                                                    },
                                                    "📌"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Group-by (list view): group the visible rows under a column, with
                // collapsible sections + per-column subtotals.
                if !is_card && !groupable_cols.is_empty() && !minimal {
                    div { class: "dxg-dropdown",
                        button {
                            r#type: "button",
                            title: "Group rows by a column",
                            class: "dxg-button",
                            "data-active": if group_by.read().is_some() { "true" },
                            onclick: move |_| {
                                let v = *show_group_menu.peek();
                                show_group_menu.set(!v);
                            },
                            svg {
                                width: "14",
                                height: "14",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                line {
                                    x1: "8",
                                    y1: "6",
                                    x2: "21",
                                    y2: "6",
                                }
                                line {
                                    x1: "8",
                                    y1: "12",
                                    x2: "21",
                                    y2: "12",
                                }
                                line {
                                    x1: "8",
                                    y1: "18",
                                    x2: "21",
                                    y2: "18",
                                }
                                circle { cx: "4", cy: "6", r: "1" }
                                circle { cx: "4", cy: "12", r: "1" }
                                circle { cx: "4", cy: "18", r: "1" }
                            }
                            if let Some(l) = group_label {
                                span { class: "dxg-button-label", "Grouped: {l}" }
                            } else {
                                span { class: "dxg-button-label", "Group" }
                            }
                        }
                        if *show_group_menu.read() {
                            div {
                                class: "dxg-veil",
                                onclick: move |_| show_group_menu.set(false),
                            }
                            div { class: "dxg-menu dxg-group-menu", role: "menu",
                                button {
                                    r#type: "button",
                                    class: "dxg-menu-item",
                                    role: "menuitemradio",
                                    "data-active": if group_by.read().is_none() { "true" },
                                    onclick: move |_| {
                                        group_by.set(None);
                                        collapsed_groups.write().clear();
                                        show_group_menu.set(false);
                                    },
                                    span { class: "dxg-menu-item-label", "No grouping" }
                                }
                                for (gkey , glabel) in groupable_cols.iter().cloned() {
                                    button {
                                        r#type: "button",
                                        class: "dxg-menu-item",
                                        role: "menuitemradio",
                                        "data-active": if *group_by.read() == Some(gkey) { "true" },
                                        onclick: move |_| {
                                            group_by.set(Some(gkey));
                                            collapsed_groups.write().clear();
                                            show_group_menu.set(false);
                                        },
                                        span { class: "dxg-menu-item-label", "{glabel}" }
                                    }
                                }
                            }
                        }
                    }
                }
                // Per-column filtering is now driven by a funnel on each header (a
                // popover, or a bottom-sheet on mobile) plus the active-filter chip
                // bar below. The toolbar only offers a one-click "Clear filters"
                // when any are active.
                if any_filterable && !is_card && active_filter_count > 0 {
                    button {
                        r#type: "button",
                        title: "Clear all filters",
                        class: "dxg-button",
                        "data-active": "true",
                        onclick: move |_| {
                            filters.write().clear();
                            grid.write().set_page(0);
                        },
                        svg {
                            width: "14",
                            height: "14",
                            view_box: "0 0 24 24",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2",
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            polygon { points: "22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" }
                            line {
                                x1: "3",
                                y1: "3",
                                x2: "21",
                                y2: "21",
                            }
                        }
                        span { class: "dxg-button-label", "Clear filters" }
                        span { class: "dxg-count-badge", "{active_filter_count}" }
                    }
                }
                if !props.no_card_toggle {
                div { class: "dxg-view-toggle", role: "group", "aria-label": "View mode",
                    button {
                        r#type: "button",
                        "aria-label": "List view",
                        title: "List view",
                        class: "dxg-view-button",
                        "data-active": if !is_card { "true" },
                        onclick: move |_| grid.write().set_view(ViewMode::List),
                        svg {
                            width: "15",
                            height: "15",
                            view_box: "0 0 24 24",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2",
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            line {
                                x1: "8",
                                y1: "6",
                                x2: "21",
                                y2: "6",
                            }
                            line {
                                x1: "8",
                                y1: "12",
                                x2: "21",
                                y2: "12",
                            }
                            line {
                                x1: "8",
                                y1: "18",
                                x2: "21",
                                y2: "18",
                            }
                            line {
                                x1: "3",
                                y1: "6",
                                x2: "3.01",
                                y2: "6",
                            }
                            line {
                                x1: "3",
                                y1: "12",
                                x2: "3.01",
                                y2: "12",
                            }
                            line {
                                x1: "3",
                                y1: "18",
                                x2: "3.01",
                                y2: "18",
                            }
                        }
                    }
                    button {
                        r#type: "button",
                        "aria-label": "Grid view",
                        title: "Grid view",
                        class: "dxg-view-button",
                        "data-active": if is_card { "true" },
                        onclick: move |_| grid.write().set_view(ViewMode::Card),
                        svg {
                            width: "15",
                            height: "15",
                            view_box: "0 0 24 24",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "2",
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            rect {
                                x: "3",
                                y: "3",
                                width: "7",
                                height: "7",
                                rx: "1.5",
                            }
                            rect {
                                x: "14",
                                y: "3",
                                width: "7",
                                height: "7",
                                rx: "1.5",
                            }
                            rect {
                                x: "14",
                                y: "14",
                                width: "7",
                                height: "7",
                                rx: "1.5",
                            }
                            rect {
                                x: "3",
                                y: "14",
                                width: "7",
                                height: "7",
                                rx: "1.5",
                            }
                        }
                    }
                }
                }
            }

            // ── Bulk action bar ───────────────────────────────────────────────
            // Its own full-width row, separate from the toolbar's flex-wrap flow —
            // sharing that row let a wide action set wrap unpredictably and strand
            // "Clear" alone on its own line. Never wraps: the action group scrolls
            // horizontally instead, and Clear is a fixed icon button on the right
            // so it always has a stable home.
            if sel_count > 0 {
                div { class: "dxg-bulk-bar", role: "toolbar", "aria-label": "Bulk actions",
                    span { class: "dxg-bulk-count",
                        svg {
                            width: "15", height: "15", view_box: "0 0 24 24",
                            fill: "none", stroke: "currentColor", stroke_width: "2.2",
                            stroke_linecap: "round", stroke_linejoin: "round",
                            path { d: "M20 6 9 17l-5-5" }
                        }
                        "{sel_count} selected"
                    }
                    div { class: "dxg-bulk-divider" }
                    div { class: "dxg-bulk-actions",
                        if let Some(b) = props.bulk.clone() {
                            {b}
                        }
                        // Auto-derived actions: every action common to the selection,
                        // as a button that applies it to all selected rows at once.
                        if let Some(handler) = props.on_bulk_action {
                            for act in bulk_actions.iter().cloned() {
                                {
                                    let rows_for = selected_rows.clone();
                                    let key = act.key;
                                    let variant = if act.danger { "danger" } else { "default" };
                                    rsx! {
                                        button {
                                            r#type: "button",
                                            class: "dxg-bulk-action",
                                            "data-variant": variant,
                                            onclick: move |_| handler.call((key, rows_for.clone())),
                                            if let Some(icon) = act.icon {
                                                svg {
                                                    width: "14", height: "14", view_box: "0 0 24 24",
                                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                                    stroke_linecap: "round", stroke_linejoin: "round",
                                                    dangerous_inner_html: "{icon}",
                                                }
                                            }
                                            "{act.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "dxg-toolbar-spacer" }
                    button {
                        r#type: "button",
                        "aria-label": "Clear selection",
                        title: "Clear selection",
                        class: "dxg-bulk-clear",
                        onclick: move |_| {
                            grid.write().clear_selection();
                            notify_sel();
                        },
                        svg {
                            width: "14", height: "14", view_box: "0 0 24 24",
                            fill: "none", stroke: "currentColor", stroke_width: "2.4",
                            stroke_linecap: "round", stroke_linejoin: "round",
                            path { d: "M18 6 6 18" }
                            path { d: "m6 6 12 12" }
                        }
                    }
                }
            }

            // ── Active-filter chip bar ───────────────────────────────────────
            // Each active filter shows as a removable pill, so what's filtering is
            // visible at a glance and clearable in one click. Hidden when empty.
            if !active_chips.is_empty() && !is_card {
                div { class: "dxg-chip-bar",
                    span { class: "dxg-chip-bar-label", "Filters:" }
                    for (key , label) in active_chips.iter().cloned() {
                        div { class: "dxg-chip",
                            span { class: "dxg-chip-label", "{label}" }
                            button {
                                r#type: "button",
                                "aria-label": "Remove filter",
                                class: "dxg-chip-remove",
                                onclick: move |_| {
                                    filters.write().remove(key);
                                    grid.write().set_page(0);
                                },
                                svg {
                                    width: "10",
                                    height: "10",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "3",
                                    stroke_linecap: "round",
                                    path { d: "M18 6 6 18" }
                                    path { d: "m6 6 12 12" }
                                }
                            }
                        }
                    }
                    button {
                        r#type: "button",
                        class: "dxg-chip-clear-all",
                        onclick: move |_| {
                            filters.write().clear();
                            grid.write().set_page(0);
                        },
                        "Clear all"
                    }
                }
            }

            // ── Body: row list ⇆ card (gallery) grid ─────────────────────────
            if !is_card {
                div {
                    // When virtualizing, the container itself scrolls vertically with a
                    // capped height (set inline) and reports scroll_top; otherwise it's a
                    // horizontal-only scroll box. Overflow direction is CSS via
                    // [data-virtualized].
                    class: "dxg-scroll",
                    "data-virtualized": if virtualize { "true" },
                    // `overflow-anchor:none` is critical: each scroll tick swaps the
                    // virtual window's spacer-row heights, and the browser's scroll
                    // anchoring would "correct" for that DOM change by nudging
                    // scrollTop — which fires onscroll again, which re-windows, which
                    // nudges again… a feedback loop that accelerated the scroll to the
                    // end. Disabling anchoring on the scroll container breaks the loop.
                    style: if virtualize { format!("max-height:{VIEWPORT_H}px;overflow-anchor:none;") } else { String::new() },
                    onscroll: move |e: ScrollEvent| {
                        // Native scroll position — no JS listener. Cheap signal write;
                        // Dioxus re-renders the windowed rows from the new offset.
                        scroll_top.set(e.data().scroll_top());
                    },
                    table {
                        class: "dxg-table",
                        role: "grid",
                        "aria-rowcount": "{total}",
                        "aria-colcount": "{nav_max_col}",
                        // Grid-level keyboard navigation (roving tabindex). Arrows move
                        // the focused cell; Home/End jump to row ends; Enter opens an
                        // editable cell's editor; Space toggles row selection. We act
                        // only when a cell is focused (set on cell focus/click), so the
                        // toolbar inputs above keep their own key handling.
                        onkeydown: {
                            let nav_cols = nav_cols.clone();
                            let nav_rows = nav_rows.clone();
                            let nav_edit_fns = nav_edit_fns.clone();
                            let row_id = props.row_id;
                            move |e: KeyboardEvent| {
                                // Don't hijack typing inside the inline editor input.
                                if editing.peek().is_some() {
                                    return;
                                }
                                let Some((r, c)) = *focus_cell.peek() else { return };
                                // Any navigation key means the user is driving by keyboard
                                // — show the focus ring from here on.
                                if matches!(e.key(), Key::ArrowDown | Key::ArrowUp | Key::ArrowRight | Key::ArrowLeft | Key::Home | Key::End) {
                                    kbd_nav.set(true);
                                }
                                let mut handled = true;
                                match e.key() {
                                    Key::ArrowDown => {
                                        focus_cell.set(Some(((r + 1).min(nav_max_row.saturating_sub(1)), c)))
                                    }
                                    Key::ArrowUp => focus_cell.set(Some((r.saturating_sub(1), c))),
                                    Key::ArrowRight => {
                                        focus_cell.set(Some((r, (c + 1).min(nav_max_col.saturating_sub(1)))))
                                    }
                                    Key::ArrowLeft => focus_cell.set(Some((r, c.saturating_sub(1)))),
                                    Key::Home => focus_cell.set(Some((r, 0))),
                                    Key::End => focus_cell.set(Some((r, nav_max_col.saturating_sub(1)))),
                                    Key::Enter => {
                                        if let (Some((key, true)), Some(row)) = (
                                            nav_cols.get(c).copied(),
                                            nav_rows.get(r),
                                        ) {
                                            if let Some(f) = nav_edit_fns.get(key) {
                                                edit_draft.set(f(row));
                                                editing.set(Some((row_id(row), key)));
                                            }
                                        }
                                    }
                                    Key::Character(s) if s == " " && props.selectable => {
                                        if let Some(row) = nav_rows.get(r) {
                                            grid.write().toggle_row(&row_id(row));
                                            notify_sel();
                                        }
                                    }
                                    _ => handled = false,
                                }
                                if handled {
                                    e.prevent_default();
                                }
                            }
                        },
                        // Hidden in custom-row mode: a columnar header makes no sense
                        // over full-width list rows.
                        thead {
                            "data-grid-thead": "true",
                            "data-hidden": if props.row.is_some() { "true" },
                            tr {
                                // Header row. Sticky positioning + fill come from the reference
                                // CSS ([data-grid-headrow]); the grid only marks the role.
                                "data-grid-headrow": "true",
                                role: "row",
                                if props.selectable {
                                    th { class: "dxg-cell dxg-select-cell", "data-grid-select-head": "true",
                                        input {
                                            r#type: "checkbox",
                                            class: "dxg-checkbox",
                                            "aria-label": "Select all rows on this page",
                                            checked: all_page_selected,
                                            onchange: {
                                                let ids = page_ids.clone();
                                                move |_| {
                                                    grid.write().toggle_page(&ids);
                                                    notify_sel();
                                                }
                                            },
                                        }
                                    }
                                }
                                for col in visible_cols.iter() {
                                    {
                                        let key = col.key;
                                        let is_sortable = col.sort_key.is_some() || col.sort_num.is_some();
                                        // Sort direction shown on this header: the primary sort (from
                                        // grid-state), or a shift-clicked secondary column. `rank` is the
                                        // 1-based position in the multi-sort chain (None when not sorted).
                                        let primary = grid.read().sort_dir_for(key);
                                        let extra = extra_sorts
                                            .read()
                                            .iter()
                                            .enumerate() // Rank offset by whether a primary sort exists.
                                            .find(|(_, (k, _))| *k == key)
                                            .map(|(i, (_, asc))| (i, *asc));
                                        let (sorted, rank): (Option<bool>, Option<usize>) = match (primary, &extra) {
                                            (Some(asc), _) => (Some(asc), Some(1)), // A user-dragged width wins; else the declared min-width; else auto.
                                            (None, Some((i, asc))) => {
                                                let base = if grid.read().sort().is_some() { 2 } else { 1 };
                                                (Some(*asc), Some(base + i))
                                            }
                                            (None, None) => (None, None), // ARIA sort state for screen readers (none unless this header sorts).
                                        };
                                        // Alignment + mobile visibility become data-* the
                                        // reference CSS keys on ([data-align], [data-hide-mobile]).
                                        let align_attr = match col.align {
                                            GridAlign::Right => "end",
                                            GridAlign::Center => "center",
                                            _ => "start",
                                        };
                                        // Runtime-computed structural style stays inline (widths + pin
                                        // offset); the pinned *look* (background/shadow) is CSS via
                                        // [data-pinned]. Only `left`/`position` must be emitted here.
                                        let mut style = match col_widths.read().get(key) {
                                            Some(w) => format!("width:{w}px; min-width:{w}px; max-width:{w}px;"),
                                            None => col.width.map(|w| format!("min-width:{w};")).unwrap_or_default(),
                                        };
                                        let is_pinned = pinned_offsets.contains_key(key);
                                        if let Some(off) = pinned_offsets.get(key) {
                                            style.push_str(&format!(" left:{off}px;"));
                                        }
                                        let drag_over = *dragging_col.read() == Some(key);
                                        let aria_sort = match sorted {
                                            Some(true) => "ascending",
                                            Some(false) => "descending",
                                            None => "none",
                                        };
                                        // data-sorted mirrors aria-sort for CSS (asc/desc/none).
                                        let sorted_attr = match sorted {
                                            Some(true) => "asc",
                                            Some(false) => "desc",
                                            None => "none",
                                        };
                                        let col_filterable = col.sort_key.is_some() || col.sort_num.is_some();
                                        let col_has_filter = filters
                                            .read()
                                            .get(key)
                                            .map(|(op, v)| filter_is_active(op, v))
                                            .unwrap_or(false);
                                        rsx! {
                                            th {
                                                class: "dxg-cell dxg-header-cell",
                                                style: "{style}",
                                                role: "columnheader",
                                                scope: "col",
                                                "aria-sort": aria_sort,
                                                "data-align": align_attr,
                                                "data-sorted": sorted_attr,
                                                "data-hide-mobile": if col.hide_on_mobile { "true" },
                                                "data-dragging": if drag_over { "true" },
                                                "data-pinned": if is_pinned { "true" },
                                                draggable: true,
                                                ondragstart: move |_| dragging_col.set(Some(key)),
                                                ondragover: move |e: DragEvent| {
                                                    e.prevent_default();
                                                },
                                                ondrop: {
                                                    let all_keys = all_keys.clone();
                                                    move |e: DragEvent| {
                                                        e.prevent_default(); // Resize handle: drag the right edge to set a fixed width.  Resize handle: drag the right edge to set a fixed width.
                                                        if let Some(from) = *dragging_col.peek() {
                                                            if from != key {
                                                                reorder_columns(&mut col_order, &all_keys, from, key);
                                                            }
                                                        }
                                                        dragging_col.set(None);
                                                    }
                                                }, // Shift-click: cycle this column in the secondary
                                                if is_sortable {
                                                    button {
                                                        r#type: "button",
                                                        title: "Click to sort · Shift-click to add a tie-breaker",
                                                        class: "dxg-sort-button",
                                                        "data-sorted": sorted_attr,
                                                        onclick: move |e: MouseEvent| {
                                                            sort_header_click(key, e.modifiers().shift(), grid, extra_sorts);
                                                        },
                                                        span { class: "dxg-header-label", "{col.label}" }
                                                        // Sort indicator: the glyph reflects state and the
                                                        // reference CSS colors it via [data-sorted].
                                                        match sorted {
                                                            Some(true) => rsx! {
                                                                span { class: "dxg-sort-indicator", "data-dir": "asc", "▲" }
                                                            },
                                                            Some(false) => rsx! {
                                                                span { class: "dxg-sort-indicator", "data-dir": "desc", "▼" }
                                                            },
                                                            None => rsx! {
                                                                span { class: "dxg-sort-indicator", "data-dir": "none", "↕" }
                                                            },
                                                        }
                                                        if let Some(r) = rank {
                                                            if extra_sorts.read().len() + (grid.read().sort().is_some() as usize) > 1 {
                                                                span { class: "dxg-sort-rank", "{r}" }
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    span { class: "dxg-header-label", "{col.label}" }
                                                }
                                                // Funnel: opens this column's filter popover. Tinted when a
                                                // filter is active on the column. The popover itself renders
                                                // at the grid root (so it escapes the table's clip box).
                                                // Hidden in Minimal density (small data needs no per-column filters).
                                                if col_filterable && !minimal {
                                                    button {
                                                        r#type: "button",
                                                        title: "Filter this column",
                                                        "aria-label": "Filter {col.label}",
                                                        class: "dxg-funnel",
                                                        "data-active": if col_has_filter { "true" },
                                                        onmousedown: move |e: MouseEvent| e.stop_propagation(),
                                                        onclick: move |e: MouseEvent| {
                                                            e.stop_propagation();
                                                            let c = e.client_coordinates();
                                                            menu_xy.set((c.x, c.y));
                                                            let open = *filter_popover.peek() == Some(key);
                                                            filter_popover.set(if open { None } else { Some(key) });
                                                        },
                                                        svg {
                                                            width: "12",
                                                            height: "12",
                                                            view_box: "0 0 24 24",
                                                            fill: "none",
                                                            stroke: "currentColor",
                                                            stroke_width: "2.2",
                                                            stroke_linecap: "round",
                                                            stroke_linejoin: "round",
                                                            polygon { points: "22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" }
                                                        }
                                                    }
                                                }
                                                // Resize handle: drag the right edge to set a fixed width.
                                                // Records the start point; the drag overlay (below) tracks
                                                // the move and commits the new width.
                                                div {
                                                    class: "dxg-resize-handle",
                                                    onmousedown: move |e: MouseEvent| {
                                                        e.stop_propagation();
                                                        let start_x = e.client_coordinates().x;
                                                        let cur_w = *col_widths.read().get(key).unwrap_or(&160.0);
                                                        resizing.set(Some((key, start_x, cur_w)));
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                                if props.actions.is_some() {
                                    th { class: "dxg-cell dxg-action-head" }
                                }
                            }
                        }
                        tbody {
                            if props.loading {
                                tr {
                                    td {
                                        colspan: "99",
                                        class: "dxg-state-cell",
                                        div { class: "dxg-loading",
                                            Spinner {}
                                            span { class: "dxg-state-text", "Loading…" }
                                        }
                                    }
                                }
                            } else if page_rows.is_empty() {
                                tr {
                                    td {
                                        colspan: "99",
                                        class: "dxg-state-cell dxg-empty",
                                        "{props.empty_label}"
                                    }
                                }
                            } else if grouped {
                                // ── Grouped view: a header row per group (collapsible) with
                                // per-column subtotals, then that group's rows. Groups are
                                // computed over the current page in its current sort order.
                                {
                                    let gcol = props
                                        .columns // running focus index across all visible rows
                                        .iter()
                                        .find(|c| Some(c.key) == *group_by.read())
                                        .cloned(); // Per-column subtotals over the whole group (even when collapsed).
                                    let groups = gcol
                                        .as_ref()
                                        .map(|gc| compute_groups(&page_rows, gc))
                                        .unwrap_or_default();
                                    let mut r_idx = 0usize;
                                    rsx! {
                                        for (label , rows) in groups.into_iter() {
                                            {
                                                let is_collapsed = collapsed_groups.read().contains(&label);
                                                let count = rows.len();
                                                // Per-column subtotals over the whole group (even when collapsed).
                                                let subtotals: Vec<(usize, String)> = visible_cols
                                                    .iter()
                                                    .enumerate() // Rows for this group get sequential focus indices.  Rows for this group get sequential focus indices.
                                                    .filter_map(|(ci, c)| {
                                                        subtotal_for(&rows, c)
                                                            .map(|v| (
                                                                ci,
                                                                format!("{} {}", agg_label(c.aggregate.unwrap()), fmt_agg(v)),
                                                            ))
                                                    })
                                                    .collect();
                                                let label_for_toggle = label.clone();
                                                let start_idx = r_idx;
                                                if !is_collapsed {
                                                    r_idx += count;
                                                }
                                                rsx! {
                                                    tr { class: "dxg-group-header", "data-collapsed": if is_collapsed { "true" },
                                                        td { colspan: "99", class: "dxg-group-header-cell",
                                                            div { class: "dxg-group-header-inner",
                                                                button {
                                                                    r#type: "button",
                                                                    class: "dxg-group-toggle",
                                                                    "aria-label": if is_collapsed { "Expand group" } else { "Collapse group" },
                                                                    "aria-expanded": if is_collapsed { "false" } else { "true" },
                                                                    onclick: move |_| {
                                                                        let mut c = collapsed_groups.write();
                                                                        if c.contains(&label_for_toggle) {
                                                                            c.remove(&label_for_toggle);
                                                                        } else {
                                                                            c.insert(label_for_toggle.clone());
                                                                        }
                                                                    },
                                                                    if is_collapsed {
                                                                        svg {
                                                                            width: "13",
                                                                            height: "13",
                                                                            view_box: "0 0 24 24",
                                                                            fill: "none",
                                                                            stroke: "currentColor",
                                                                            stroke_width: "2.4",
                                                                            stroke_linecap: "round",
                                                                            stroke_linejoin: "round",
                                                                            polyline { points: "9 18 15 12 9 6" }
                                                                        }
                                                                    } else {
                                                                        svg {
                                                                            width: "13",
                                                                            height: "13",
                                                                            view_box: "0 0 24 24",
                                                                            fill: "none",
                                                                            stroke: "currentColor",
                                                                            stroke_width: "2.4",
                                                                            stroke_linecap: "round",
                                                                            stroke_linejoin: "round",
                                                                            polyline { points: "6 9 12 15 18 9" }
                                                                        }
                                                                    }
                                                                }
                                                                span { class: "dxg-group-label", "{label}" }
                                                                span { class: "dxg-group-count", "({count})" }
                                                                div { class: "dxg-toolbar-spacer" }
                                                                for (_ , sub) in subtotals.iter() {
                                                                    span { class: "dxg-group-subtotal", "{sub}" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    if !is_collapsed {
                                                        for (gi , row) in rows.into_iter().enumerate() {
                                                            {render_data_row(row, start_idx + gi, false)}
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Top spacer: stands in for the `vstart` rows scrolled
                                // off the top, preserving scrollbar position.
                                if virtualize && pad_top > 0.0 {
                                    tr { style: "height:{pad_top}px",
                                        td { colspan: "99" }
                                    }
                                }
                                for (wi , row) in page_rows[vstart..vend].iter().cloned().enumerate() {
                                    {render_data_row(row, vstart + wi, virtualize)}
                                }
                                // Bottom spacer: stands in for the rows below the window.
                                if virtualize && pad_bottom > 0.0 {
                                    tr { style: "height:{pad_bottom}px",
                                        td { colspan: "99" }
                                    }
                                }
                            }
                        }
                        // ── Aggregation footer (over the current filtered set) ────
                        // A sticky "totals" bar: each aggregated column shows a tiny
                        // caption (Total / Average / …) above its value in the accent
                        // color, so the row reads as a summary, not just smaller text.
                        if has_aggregates && !page_rows.is_empty() {
                            tfoot {
                                tr { class: "dxg-totals-row",
                                    if props.selectable {
                                        td { class: "dxg-cell" }
                                    }
                                    for col in visible_cols.iter() {
                                        {
                                            let align_attr = match col.align {
                                                GridAlign::Right => "end",
                                                GridAlign::Center => "center",
                                                _ => "start",
                                            };
                                            let cell = col.aggregate.and_then(|agg| {
                                                agg_results
                                                    .iter()
                                                    .find(|(k, _)| k == col.key)
                                                    .map(|(_, v)| (agg_label(agg), fmt_agg(*v)))
                                            });
                                            rsx! {
                                                td {
                                                    class: "dxg-cell dxg-totals-cell",
                                                    "data-align": align_attr,
                                                    "data-hide-mobile": if col.hide_on_mobile { "true" },
                                                    if let Some((label, value)) = cell {
                                                        div { class: "dxg-total",
                                                            span { class: "dxg-total-label", "{label}" }
                                                            span { class: "dxg-total-value", "{value}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if props.actions.is_some() {
                                        td { class: "dxg-cell" }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                // ── Card (gallery) grid ──────────────────────────────────────
                if props.loading {
                    div { class: "dxg-state-box dxg-loading",
                        Spinner {}
                        span { class: "dxg-state-text", "Loading…" }
                    }
                } else if page_rows.is_empty() {
                    div { class: "dxg-state-box dxg-empty", "{props.empty_label}" }
                } else {
                    div { class: "dxg-card-grid",
                        for row in page_rows.iter() {
                            {
                                let row = row.clone();
                                let id = (props.row_id)(&row);
                                let is_sel = grid.read().is_selected(&id);
                                let row_click = row.clone();
                                rsx! {
                                    div {
                                        class: "dxg-card",
                                        "data-selected": if is_sel { "true" },
                                        onclick: move |_| {
                                            if let Some(h) = props.on_row_click {
                                                h.call(row_click.clone());
                                            }
                                        },
                                        div { class: "dxg-card-overlay",
                                            if props.selectable {
                                                input {
                                                    r#type: "checkbox",
                                                    class: "dxg-checkbox",
                                                    "aria-label": "Select row",
                                                    checked: is_sel,
                                                    onclick: move |e: MouseEvent| e.stop_propagation(),
                                                    onchange: {
                                                        let id = id.clone();
                                                        move |_| {
                                                            grid.write().toggle_row(&id);
                                                            notify_sel();
                                                        }
                                                    },
                                                }
                                            }
                                            if props.actions.is_some() {
                                                button {
                                                    r#type: "button",
                                                    "aria-label": "More actions",
                                                    "aria-haspopup": "menu",
                                                    class: "dxg-icon-button dxg-kebab",
                                                    onclick: {
                                                        let id = id.clone();
                                                        move |e: MouseEvent| {
                                                            e.stop_propagation();
                                                            let c = e.client_coordinates();
                                                            menu_xy.set((c.x, c.y));
                                                            let open = menu_for.read().as_deref() == Some(id.as_str());
                                                            menu_for.set(if open { None } else { Some(id.clone()) });
                                                        }
                                                    },
                                                    svg {
                                                        width: "15",
                                                        height: "15",
                                                        view_box: "0 0 24 24",
                                                        fill: "currentColor",
                                                        circle { cx: "12", cy: "5", r: "1.7" }
                                                        circle { cx: "12", cy: "12", r: "1.7" }
                                                        circle { cx: "12", cy: "19", r: "1.7" }
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(cardfn) = props.card {
                                            // Reserve a clear lane on the top-right so the
                                            // host card's content never slides under the
                                            // checkbox/kebab overlay (status badges etc.).
                                            div { class: "dxg-card-slot", {cardfn(&row)} }
                                        } else {
                                            // Auto-built card: shows EVERY visible column (same set the
                                            // List View shows, honoring the column chooser) so the two
                                            // views stay consistent. First column is the card title; the
                                            // rest are label → value rows.
                                            div { class: "dxg-card-auto",
                                                if let Some(first) = visible_cols.first() {
                                                    div { class: "dxg-card-title", {(first.render)(&row)} }
                                                }
                                                div { class: "dxg-card-fields",
                                                    for col in visible_cols.iter().skip(1) {
                                                        div { class: "dxg-card-field",
                                                            span { class: "dxg-card-field-label", "{col.label}" }
                                                            div { class: "dxg-card-field-value", {(col.render)(&row)} }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // ── Aggregation summary (card view) ───────────────────────
                    // The card grid has no <tfoot>, so the same per-column totals
                    // render here as a labeled chip bar — keeping the Aggregate Row
                    // visible and consistent with the List View.
                    if has_aggregates {
                        div { class: "dxg-totals-chips",
                            span { class: "dxg-totals-chips-label", "Totals" }
                            for col in visible_cols.iter() {
                                {
                                    let cell = col.aggregate.and_then(|agg| {
                                        agg_results
                                            .iter()
                                            .find(|(k, _)| k == col.key)
                                            .map(|(_, v)| (agg_label(agg), fmt_agg(*v)))
                                    });
                                    rsx! {
                                        if let Some((label, value)) = cell {
                                            div { class: "dxg-total-chip",
                                                span { class: "dxg-total-chip-label", "{col.label} · {label}" }
                                                span { class: "dxg-total-chip-value", "{value}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Row kebab menu (rendered once, at the grid root) ─────────────
            // Kept outside the cards/rows so the fixed-position menu never sits
            // inside a transformed ancestor (the card hover-lift), which would
            // re-anchor and jitter it.
            if let Some(open_id) = menu_for() {
                if let Some(actions_fn) = props.actions {
                    if let Some(row) = props.rows.iter().find(|r| (props.row_id)(r) == open_id).cloned() {
                        {render_menu(row.clone(), actions_fn(&row))}
                    }
                }
            }

            // ── Column filter popover (desktop) / bottom-sheet (mobile) ───────
            // Rendered once at the grid root so it escapes the table's overflow
            // clip. Anchored under the clicked funnel on `sm+`; a full-width
            // bottom-sheet on phones (where an anchored popover is too cramped).
            if let Some(fkey) = *filter_popover.read() {
                if let Some(col) = props.columns.iter().find(|c| c.key == fkey) {
                    {
                        let (mx, my) = *menu_xy.read();
                        let has_filter = filters
                            .read()
                            .get(fkey)
                            .map(|(op, v)| filter_is_active(op, v))
                            .unwrap_or(false);
                        let anchor = format!(
                            "--pop-top:calc({my}px + 14px); --pop-right:max(12px, calc(100vw - {mx}px - 8px)); --pop-maxh:calc(100vh - {my}px - 32px);",
                        );
                        rsx! {
                            // Click-away veil (also dims on mobile for the sheet). The
                            // `sheet-overlay` marker triggers the global :has() rule that
                            // hides the mobile bottom nav while the sheet is open — the same
                            // convention Modal/Tabs use — so the bottom-sheet (and its
                            // calendar / Apply-Clear footer) isn't covered by the nav.
                            div {
                                class: "dxg-sheet-overlay",
                                onclick: move |_| filter_popover.set(None),
                            }
                            div {
                                // Anchored popover on wide viewports, full-width bottom-sheet
                                // on mobile — the positioning + variant switch live in CSS
                                // (.dxg-filter-popover); the anchor point is passed via the
                                // --pop-* custom props (runtime, so inline).
                                class: "dxg-filter-popover",
                                style: "{anchor}",
                                div { class: "dxg-popover-head",
                                    span { class: "dxg-popover-title", "Filter · {col.label}" }
                                    button {
                                        r#type: "button",
                                        "aria-label": "Close",
                                        class: "dxg-icon-button",
                                        onclick: move |_| filter_popover.set(None),
                                        svg {
                                            width: "13",
                                            height: "13",
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2.4",
                                            stroke_linecap: "round",
                                            path { d: "M18 6 6 18" }
                                            path { d: "m6 6 12 12" }
                                        }
                                    }
                                }
                                {render_filter_body(fkey)}
                                if has_filter {
                                    button {
                                        r#type: "button",
                                        class: "dxg-filter-clear",
                                        onclick: move |_| {
                                            filters.write().remove(fkey);
                                            grid.write().set_page(0);
                                        },
                                        "Clear this filter"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Footer: range · page-size · pager ────────────────────────────
            // Full: always shown when there are rows. Minimal: only when the data
            // actually spills past one page (otherwise a small set shows no footer).
            if !props.loading && total > 0 && (!minimal || pages > 1) {
                div { class: "dxg-pagination",
                    span { class: "dxg-page-range", "Showing {from}–{to} of {total}" }
                    div { class: "dxg-toolbar-spacer" }
                    select {
                        class: "dxg-page-size",
                        "aria-label": "Rows per page",
                        value: "{psize}",
                        onchange: move |e: FormEvent| {
                            if let Ok(n) = e.value().parse::<usize>() {
                                grid.write().set_page_size(n);
                            }
                        },
                        option { value: "10", "10 / page" }
                        option { value: "25", "25 / page" }
                        option { value: "50", "50 / page" }
                        option { value: "100", "100 / page" }
                    }
                    div { class: "dxg-pager",
                        button {
                            r#type: "button",
                            class: "dxg-page-button",
                            "aria-label": "Previous page",
                            disabled: cur == 0,
                            onclick: move |_| {
                                grid.write().prev_page();
                            },
                            "‹"
                        }
                        span { class: "dxg-page-current", "{cur + 1} / {pages}" }
                        button {
                            r#type: "button",
                            class: "dxg-page-button",
                            "aria-label": "Next page",
                            disabled: cur + 1 >= pages,
                            onclick: move |_| {
                                grid.write().next_page(pages);
                            },
                            "›"
                        }
                    }
                }
            }
        }
    }
}
