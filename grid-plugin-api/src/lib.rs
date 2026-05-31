//! Composable query-pipeline stages for the headless grid.
//!
//! A feature like column filtering or multi-column sort is, at its core, a
//! transform on the row pipeline: it takes the rows so far plus a slice of model
//! state and returns the rows that survive, or a reordering. That shared shape is
//! the [`QueryStage`] trait, which is this crate's extension seam.
//!
//! [`grid_core`] owns the base vocabulary ([`grid_core::QueryModel`],
//! [`grid_core::CellValue`], [`grid_core::DataSource`]). This crate composes with
//! those types rather than changing them: it carries its own extended model
//! ([`FilterModel`], [`MultiSort`]), and a [`Pipeline`] runs global search, then
//! the stages, then pagination, yielding a [`grid_core::ResultSet`] the same
//! renderer can draw.
//!
//! There is no UI and no framework here.

#![forbid(unsafe_code)]

use grid_core::{ColumnDef, QueryModel, ResultSet};

// Re-export the `grid-core` types that appear in this crate's public API, so a
// caller can build filters/sorts depending on `grid-plugin-api` alone.
pub use grid_core::{CellValue, Sort};

/// An owned/borrowed column key, mirroring [`grid_core::Sort`]'s key type: free to
/// build from a `&'static str` column key, yet round-trips through serde (it
/// deserializes to the owned variant).
pub type Cow = std::borrow::Cow<'static, str>;

// ── The extension seam ───────────────────────────────────────────────────────

/// One step of the grid's row pipeline: given the rows so far and the columns
/// (for value projections), return the rows that survive — filtered, reordered,
/// grouped, whatever the stage does.
///
/// This is the whole plugin contract. A new capability (range filters, grouping,
/// aggregation) is a new `QueryStage`; nothing else in the engine changes.
pub trait QueryStage<T> {
    fn apply(&self, rows: Vec<T>, columns: &[ColumnDef<T>]) -> Vec<T>;
}

/// A boxed projection from a row to its typed cell value for one column.
pub type Projection<T> = Box<dyn Fn(&T) -> CellValue>;

/// Resolve a column's value projection by key, with the same precedence as
/// [`grid_core::ClientSource`]'s sort: typed `value`, then numeric `sort_num`
/// (wrapped as [`CellValue::Number`]), then string `sort_key` (wrapped as
/// [`CellValue::Text`]). Filtering and multi-sort therefore work on any column
/// that is already sortable, with no extra configuration.
///
/// Returns `None` for a column with no projection at all, or an unknown key; the
/// stages then skip that filter or sort rather than dropping rows.
fn project<T: 'static>(columns: &[ColumnDef<T>], key: &str) -> Option<Projection<T>> {
    let col = columns.iter().find(|c| c.key == key)?;
    if let Some(v) = col.value {
        Some(Box::new(move |r: &T| v(r)))
    } else if let Some(n) = col.sort_num {
        Some(Box::new(move |r: &T| CellValue::Number(n(r))))
    } else if let Some(s) = col.sort_key {
        Some(Box::new(move |r: &T| CellValue::Text(s(r))))
    } else {
        None
    }
}

// ── Feature 1: column filters ────────────────────────────────────────────────

/// How a [`Filter`] compares a cell's [`CellValue`] against its operand.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FilterOp {
    Equals,
    NotEquals,
    /// Text contains (case-insensitive). Non-text cells never match.
    Contains,
    /// Text starts with (case-insensitive).
    StartsWith,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
    /// Cell is [`CellValue::Empty`]. The filter's `value` is ignored.
    IsEmpty,
    /// Cell is anything but [`CellValue::Empty`]. The filter's `value` is ignored.
    IsNotEmpty,
    /// Cell equals **any** of the operands in [`Filter::values`] (a set / multi-select
    /// filter, e.g. "Department in {Engineering, Sales}"). The scalar `value` is
    /// ignored. An empty set matches nothing.
    In,
}

/// A single per-column predicate: "column `key` `op` `value`".
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Filter {
    pub key: Cow,
    pub op: FilterOp,
    /// The operand. Ignored by [`FilterOp::IsEmpty`] / [`FilterOp::IsNotEmpty`] / [`FilterOp::In`].
    pub value: CellValue,
    /// The operand set for [`FilterOp::In`] (a cell matches if it equals any member).
    /// Unused by every other operator; defaults to empty.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Vec::is_empty"))]
    pub values: Vec<CellValue>,
}

impl Filter {
    pub fn new(key: impl Into<Cow>, op: FilterOp, value: impl Into<CellValue>) -> Self {
        Self { key: key.into(), op, value: value.into(), values: Vec::new() }
    }
    /// A set / multi-select filter: cell matches if it equals any of `values`.
    pub fn one_of(key: impl Into<Cow>, values: Vec<CellValue>) -> Self {
        Self { key: key.into(), op: FilterOp::In, value: CellValue::Empty, values }
    }
    /// `key == value`.
    pub fn equals(key: impl Into<Cow>, value: impl Into<CellValue>) -> Self {
        Self::new(key, FilterOp::Equals, value)
    }
    /// Text `key` contains `value` (case-insensitive).
    pub fn contains(key: impl Into<Cow>, value: impl Into<CellValue>) -> Self {
        Self::new(key, FilterOp::Contains, value)
    }
    /// `key > value`.
    pub fn greater_than(key: impl Into<Cow>, value: impl Into<CellValue>) -> Self {
        Self::new(key, FilterOp::GreaterThan, value)
    }
    /// `key < value`.
    pub fn less_than(key: impl Into<Cow>, value: impl Into<CellValue>) -> Self {
        Self::new(key, FilterOp::LessThan, value)
    }
    pub fn is_empty(key: impl Into<Cow>) -> Self {
        Self::new(key, FilterOp::IsEmpty, CellValue::Empty)
    }

    /// Evaluate this filter against one already-projected cell value. Public so an
    /// index-based provider can test a row's projected value without cloning the row.
    pub fn matches_value(&self, cell: &CellValue) -> bool {
        use std::cmp::Ordering::*;
        // "Empty" means no meaningful content: a literal `Empty`, or text that is
        // blank/whitespace. This lets a column expose presence with a plain string
        // projection (`""` for absent) rather than needing a typed `CellValue`.
        let is_empty = matches!(cell, CellValue::Empty) || matches!(cell, CellValue::Text(t) if t.trim().is_empty());
        match self.op {
            FilterOp::IsEmpty => is_empty,
            FilterOp::IsNotEmpty => !is_empty,
            FilterOp::Equals => cell == &self.value,
            FilterOp::NotEquals => cell != &self.value,
            FilterOp::Contains => {
                text_of(cell).map(|h| h.to_lowercase().contains(&needle(&self.value))).unwrap_or(false)
            }
            FilterOp::StartsWith => {
                text_of(cell).map(|h| h.to_lowercase().starts_with(&needle(&self.value))).unwrap_or(false)
            }
            FilterOp::GreaterThan => matches!(cell.cmp(&self.value), Greater),
            FilterOp::GreaterOrEqual => matches!(cell.cmp(&self.value), Greater | Equal),
            FilterOp::LessThan => matches!(cell.cmp(&self.value), Less),
            FilterOp::LessOrEqual => matches!(cell.cmp(&self.value), Less | Equal),
            // Set membership: equal to any operand (empty set matches nothing).
            FilterOp::In => self.values.iter().any(|v| cell == v),
        }
    }
}

fn text_of(v: &CellValue) -> Option<&str> {
    match v {
        CellValue::Text(s) => Some(s),
        _ => None,
    }
}
fn needle(v: &CellValue) -> String {
    match v {
        CellValue::Text(s) => s.to_lowercase(),
        _ => String::new(),
    }
}

/// How a [`FilterModel`]'s filters combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Combine {
    /// A row passes only if it matches **every** filter (the usual case).
    #[default]
    All,
    /// A row passes if it matches **any** filter.
    Any,
}

/// A set of column filters and how they combine. Empty = pass-through.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FilterModel {
    pub filters: Vec<Filter>,
    pub combine: Combine,
}

impl FilterModel {
    pub fn new() -> Self {
        Self::default()
    }
    /// Add a filter (ANDed by default — change with [`FilterModel::any`]).
    pub fn with(mut self, f: Filter) -> Self {
        self.filters.push(f);
        self
    }
    /// Switch combination to OR.
    pub fn any(mut self) -> Self {
        self.combine = Combine::Any;
        self
    }
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

/// The [`QueryStage`] that applies a [`FilterModel`]. A filter on a column with no
/// `value` projection is skipped (it cannot be evaluated), never silently drops
/// rows.
pub struct FilterStage<'a> {
    pub model: &'a FilterModel,
}

impl<'a> FilterStage<'a> {
    pub fn new(model: &'a FilterModel) -> Self {
        Self { model }
    }
}

impl<T: 'static> QueryStage<T> for FilterStage<'_> {
    fn apply(&self, rows: Vec<T>, columns: &[ColumnDef<T>]) -> Vec<T> {
        if self.model.is_empty() {
            return rows;
        }
        // Resolve each filter's projection once (skipping unprojectable columns).
        let active: Vec<(Projection<T>, &Filter)> =
            self.model.filters.iter().filter_map(|f| project(columns, &f.key).map(|p| (p, f))).collect();
        if active.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| match self.model.combine {
                Combine::All => active.iter().all(|(p, f)| f.matches_value(&p(row))),
                Combine::Any => active.iter().any(|(p, f)| f.matches_value(&p(row))),
            })
            .collect()
    }
}

// ── Feature 2: multi-column sort ─────────────────────────────────────────────

/// Sort by several columns, tie-broken left-to-right (the first [`Sort`] is
/// primary). Each column uses its `value` projection; columns without one are
/// skipped in the comparison.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MultiSort {
    pub sorts: Vec<Sort>,
}

impl MultiSort {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn then(mut self, sort: Sort) -> Self {
        self.sorts.push(sort);
        self
    }
    pub fn is_empty(&self) -> bool {
        self.sorts.is_empty()
    }
}

/// The [`QueryStage`] that applies a [`MultiSort`].
pub struct MultiSortStage<'a> {
    pub model: &'a MultiSort,
}

impl<'a> MultiSortStage<'a> {
    pub fn new(model: &'a MultiSort) -> Self {
        Self { model }
    }
}

impl<T: 'static> QueryStage<T> for MultiSortStage<'_> {
    fn apply(&self, mut rows: Vec<T>, columns: &[ColumnDef<T>]) -> Vec<T> {
        if self.model.is_empty() {
            return rows;
        }
        // Resolve (projection, ascending) for each sortable column, in order.
        let keys: Vec<(Projection<T>, bool)> =
            self.model.sorts.iter().filter_map(|s| project(columns, s.key()).map(|p| (p, s.ascending))).collect();
        if keys.is_empty() {
            return rows;
        }
        rows.sort_by(|a, b| {
            for (p, ascending) in &keys {
                let ord = p(a).cmp(&p(b));
                let ord = if *ascending { ord } else { ord.reverse() };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
        rows
    }
}

// ── Aggregation ──────────────────────────────────────────────────────────────

/// A summary computed over a column's values across a row set (the *filtered*
/// set, in a grid). Numeric aggregates use the column's `value` → `sort_num`
/// projection (via the same fallback as filtering/sorting); [`Aggregate::Count`]
/// counts rows and is type-agnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Aggregate {
    Sum,
    Avg,
    Min,
    Max,
    /// Number of rows (ignores the column's values).
    Count,
}

/// The result of an [`Aggregate`] over a column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AggValue {
    /// A numeric result (Sum/Avg/Min/Max).
    Num(f64),
    /// A row count (Count).
    Count(usize),
    /// No numeric values were present (e.g. Sum over an all-text column).
    Empty,
}

/// Pull a numeric value from a [`CellValue`] for aggregation (Int/Number only).
fn numeric_of(v: &CellValue) -> Option<f64> {
    match v {
        CellValue::Int(i) => Some(*i as f64),
        CellValue::Number(n) if !n.is_nan() => Some(*n),
        _ => None,
    }
}

/// Compute `agg` over column `key` across `rows`, using the same projection
/// fallback (`value → sort_num → sort_key`) as the stages. Returns
/// [`AggValue::Empty`] when there is nothing numeric to aggregate.
pub fn aggregate<T: 'static>(rows: &[T], columns: &[ColumnDef<T>], key: &str, agg: Aggregate) -> AggValue {
    if let Aggregate::Count = agg {
        return AggValue::Count(rows.len());
    }
    let Some(proj) = project(columns, key) else { return AggValue::Empty };
    let nums: Vec<f64> = rows.iter().filter_map(|r| numeric_of(&proj(r))).collect();
    if nums.is_empty() {
        return AggValue::Empty;
    }
    let v = match agg {
        Aggregate::Sum => nums.iter().sum(),
        Aggregate::Avg => nums.iter().sum::<f64>() / nums.len() as f64,
        Aggregate::Min => nums.iter().cloned().fold(f64::INFINITY, f64::min),
        Aggregate::Max => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        Aggregate::Count => unreachable!(),
    };
    AggValue::Num(v)
}

// ── Pipeline: search → stages → paginate ─────────────────────────────────────

/// Composes global search (from [`grid_core::QueryModel`]), an ordered list of
/// [`QueryStage`]s, and pagination into a single [`grid_core::ResultSet`] — so a
/// plugin-powered grid drives the exact same renderer as a plain one.
///
/// ```
/// use grid_core::{ColumnDef, CellValue, QueryModel};
/// use grid_plugin_api::{Pipeline, FilterModel, Filter, FilterStage};
///
/// #[derive(Clone)] struct Row { name: &'static str, age: i64 }
/// let rows = vec![Row{name:"Ann",age:30}, Row{name:"Bo",age:18}, Row{name:"Cy",age:40}];
/// let cols = vec![
///     ColumnDef::new("name").typed(|r: &Row| r.name.into()),
///     ColumnDef::new("age").typed(|r: &Row| CellValue::Int(r.age)),
/// ];
/// let model = QueryModel::new(10);
/// let filter = FilterModel::new().with(Filter::greater_than("age", CellValue::Int(20)));
/// let out = Pipeline::new(rows, &cols)
///     .search(|r: &Row| r.name.to_string())
///     .stage(FilterStage::new(&filter))
///     .run(&model);
/// assert_eq!(out.total, 2); // Ann(30), Cy(40)
/// ```
pub struct Pipeline<'c, T> {
    rows: Vec<T>,
    columns: &'c [ColumnDef<T>],
    search_text: Option<fn(&T) -> String>,
    stages: Vec<Box<dyn QueryStage<T> + 'c>>,
}

impl<'c, T: Clone + 'static> Pipeline<'c, T> {
    pub fn new(rows: Vec<T>, columns: &'c [ColumnDef<T>]) -> Self {
        Self { rows, columns, search_text: None, stages: Vec::new() }
    }
    /// Attach the global-search projection (mirrors `grid_core::ClientSource`).
    pub fn search(mut self, f: fn(&T) -> String) -> Self {
        self.search_text = Some(f);
        self
    }
    /// Attach an *optional* global-search projection. `None` disables searching
    /// (the search term is ignored), matching `grid_core::ClientSource`. Handy for
    /// a renderer that already holds `Option<fn(&T)->String>`.
    pub fn search_opt(mut self, f: Option<fn(&T) -> String>) -> Self {
        self.search_text = f;
        self
    }
    /// Append a stage; stages run in the order added, after global search.
    pub fn stage(mut self, stage: impl QueryStage<T> + 'c) -> Self {
        self.stages.push(Box::new(stage));
        self
    }

    /// Run search → stages, returning **all** matching rows (after filters + sort,
    /// before pagination). Useful for aggregation and full-set CSV export.
    pub fn filtered(self, model: &QueryModel) -> Vec<T> {
        // Global search, matching grid-core's ClientSource.
        let q = model.search.trim().to_lowercase();
        let mut rows: Vec<T> = match self.search_text {
            Some(hay) if !q.is_empty() => {
                self.rows.into_iter().filter(|r| hay(r).to_lowercase().contains(&q)).collect()
            }
            _ => self.rows,
        };
        for stage in &self.stages {
            rows = stage.apply(rows, self.columns);
        }
        rows
    }

    /// Run search → stages → paginate for `model`, returning the page to draw.
    pub fn run(self, model: &QueryModel) -> ResultSet<T> {
        let rows = self.filtered(model);
        paginate(rows, model)
    }
}

/// Window a fully-filtered row list into the `model`'s page (the same arithmetic
/// as [`grid_core::ResultSet`]).
pub fn paginate<T>(rows: Vec<T>, model: &QueryModel) -> ResultSet<T> {
    let total = rows.len();
    let psize = model.page_size.max(1);
    let page_count = total.div_ceil(psize).max(1);
    let page = model.page.min(page_count - 1);
    let page_rows: Vec<T> = rows.into_iter().skip(page * psize).take(psize).collect();
    ResultSet { rows: page_rows, total, page, page_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Row {
        dept: &'static str,
        name: &'static str,
        age: i64,
        note: Option<&'static str>,
    }

    fn data() -> Vec<Row> {
        vec![
            Row { dept: "eng", name: "Ann", age: 30, note: Some("lead") },
            Row { dept: "eng", name: "Bob", age: 30, note: None },
            Row { dept: "ops", name: "Cy", age: 18, note: Some("intern") },
            Row { dept: "ops", name: "Di", age: 40, note: None },
        ]
    }

    fn cols() -> Vec<ColumnDef<Row>> {
        vec![
            ColumnDef::new("dept").typed(|r: &Row| r.dept.into()),
            ColumnDef::new("name").typed(|r: &Row| r.name.into()),
            ColumnDef::new("age").typed(|r: &Row| CellValue::Int(r.age)),
            ColumnDef::new("note").typed(|r: &Row| r.note.map(|n| n.into()).unwrap_or(CellValue::Empty)),
        ]
    }

    fn names(rs: &ResultSet<Row>) -> Vec<&'static str> {
        rs.rows.iter().map(|r| r.name).collect()
    }

    #[test]
    fn filter_equals_and_greater_than_and_with_all() {
        let c = cols();
        let model = FilterModel::new()
            .with(Filter::equals("dept", "ops"))
            .with(Filter::greater_than("age", CellValue::Int(20)));
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.iter().map(|r| r.name).collect::<Vec<_>>(), ["Di"]);
    }

    #[test]
    fn filter_any_combines_with_or() {
        let c = cols();
        let model = FilterModel::new().with(Filter::equals("name", "Ann")).with(Filter::equals("name", "Cy")).any();
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn filter_contains_is_case_insensitive_text_only() {
        let c = cols();
        let model = FilterModel::new().with(Filter::contains("note", "LEAD"));
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.iter().map(|r| r.name).collect::<Vec<_>>(), ["Ann"]);
    }

    #[test]
    fn filter_in_matches_any_of_a_set() {
        // A set / multi-select filter: dept in {eng, hr} keeps the eng rows and
        // drops ops. An empty set matches nothing. ANDs with other columns.
        let c = cols();
        let model = FilterModel::new().with(Filter::one_of("dept", vec!["eng".into(), "hr".into()]));
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.iter().map(|r| r.name).collect::<Vec<_>>(), ["Ann", "Bob"]);

        let empty = FilterStage::new(&FilterModel::new().with(Filter::one_of("dept", vec![]))).apply(data(), &c);
        assert!(empty.is_empty());
    }

    #[test]
    fn filter_is_empty_finds_blanks() {
        let c = cols();
        let model = FilterModel::new().with(Filter::is_empty("note"));
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.iter().map(|r| r.name).collect::<Vec<_>>(), ["Bob", "Di"]);
    }

    #[test]
    fn is_empty_treats_blank_text_as_empty() {
        // A column projecting to "" (no file) for some rows: IsEmpty / IsNotEmpty
        // must split on blank-vs-present text, which powers the grid's
        // "has file / no file" media filters.
        #[derive(Clone)]
        struct Doc {
            name: &'static str,
            file: &'static str,
        }
        let rows = vec![
            Doc { name: "a", file: "x.mp3" },
            Doc { name: "b", file: "" },
            Doc { name: "c", file: "   " },
            Doc { name: "d", file: "y.mp4" },
        ];
        let cols = vec![ColumnDef::new("file").sortable(|d: &Doc| d.file.to_string())];
        let present =
            FilterStage::new(&FilterModel::new().with(Filter::new("file", FilterOp::IsNotEmpty, CellValue::Empty)))
                .apply(rows.clone(), &cols);
        assert_eq!(present.iter().map(|d| d.name).collect::<Vec<_>>(), ["a", "d"]);
        let absent = FilterStage::new(&FilterModel::new().with(Filter::is_empty("file"))).apply(rows, &cols);
        assert_eq!(absent.iter().map(|d| d.name).collect::<Vec<_>>(), ["b", "c"]);
    }

    #[test]
    fn filter_on_unprojected_column_is_skipped_not_dropping_rows() {
        // "missing" has no column → the filter is inert, all rows survive.
        let c = cols();
        let model = FilterModel::new().with(Filter::equals("missing", "x"));
        let rows = FilterStage::new(&model).apply(data(), &c);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn projection_falls_back_to_legacy_sort_keys() {
        // Columns declared the OLD way (sort_key / sort_num, no `.typed`) must
        // still be filterable + multi-sortable — this is what makes plugin-api
        // work on every existing grid with zero call-site changes.
        let legacy: Vec<ColumnDef<Row>> = vec![
            ColumnDef::new("name").sortable(|r: &Row| r.name.to_string()),
            ColumnDef::new("age").sortable_num(|r: &Row| r.age as f64),
        ];
        // filter age > 20 via the numeric fallback
        let fm = FilterModel::new().with(Filter::greater_than("age", CellValue::Number(20.0)));
        let rows = FilterStage::new(&fm).apply(data(), &legacy);
        assert_eq!(rows.len(), 3); // Ann(30), Bob(30), Di(40)
                                   // multi-sort name asc via the string fallback
        let ms = MultiSort::new().then(Sort::asc("name"));
        let sorted = MultiSortStage::new(&ms).apply(data(), &legacy);
        assert_eq!(sorted.iter().map(|r| r.name).collect::<Vec<_>>(), ["Ann", "Bob", "Cy", "Di"]);
    }

    #[test]
    fn multi_sort_tie_breaks_left_to_right() {
        // Primary: dept asc; secondary: age desc. Within "eng" both are 30 →
        // stable; within "ops" 40 before 18.
        let c = cols();
        let ms = MultiSort::new().then(Sort::asc("dept")).then(Sort::desc("age"));
        let rows = MultiSortStage::new(&ms).apply(data(), &c);
        let order: Vec<_> = rows.iter().map(|r| (r.dept, r.age)).collect();
        assert_eq!(order, [("eng", 30), ("eng", 30), ("ops", 40), ("ops", 18)]);
    }

    #[test]
    fn pipeline_composes_search_filter_multisort_paginate() {
        let c = cols();
        let filter = FilterModel::new().with(Filter::greater_than("age", CellValue::Int(20)));
        let ms = MultiSort::new().then(Sort::desc("age"));
        let model = QueryModel::new(10);
        let out = Pipeline::new(data(), &c)
            .search(|r: &Row| r.name.to_string())
            .stage(FilterStage::new(&filter))
            .stage(MultiSortStage::new(&ms))
            .run(&model);
        // age>20 → Ann(30),Bob(30),Di(40); sorted desc → Di(40),Ann(30),Bob(30)
        assert_eq!(out.total, 3);
        assert_eq!(names(&out), ["Di", "Ann", "Bob"]);
    }

    #[test]
    fn aggregate_sum_avg_min_max_count_over_rows() {
        let c = cols();
        let rows = data(); // ages 30, 30, 18, 40
        assert_eq!(aggregate(&rows, &c, "age", Aggregate::Sum), AggValue::Num(118.0));
        assert_eq!(aggregate(&rows, &c, "age", Aggregate::Avg), AggValue::Num(29.5));
        assert_eq!(aggregate(&rows, &c, "age", Aggregate::Min), AggValue::Num(18.0));
        assert_eq!(aggregate(&rows, &c, "age", Aggregate::Max), AggValue::Num(40.0));
        assert_eq!(aggregate(&rows, &c, "age", Aggregate::Count), AggValue::Count(4));
    }

    #[test]
    fn aggregate_over_text_column_is_empty_except_count() {
        let c = cols();
        let rows = data();
        assert_eq!(aggregate(&rows, &c, "name", Aggregate::Sum), AggValue::Empty);
        assert_eq!(aggregate(&rows, &c, "name", Aggregate::Count), AggValue::Count(4));
    }

    #[test]
    fn aggregate_respects_the_filtered_set() {
        // Aggregate over only the rows the pipeline keeps (age > 20 → 30,30,40).
        let c = cols();
        let filter = FilterModel::new().with(Filter::greater_than("age", CellValue::Int(20)));
        let kept = Pipeline::new(data(), &c).stage(FilterStage::new(&filter)).filtered(&QueryModel::new(10));
        assert_eq!(aggregate(&kept, &c, "age", Aggregate::Sum), AggValue::Num(100.0));
        assert_eq!(aggregate(&kept, &c, "age", Aggregate::Count), AggValue::Count(3));
    }

    #[test]
    fn pipeline_global_search_runs_before_stages() {
        let c = cols();
        let model = QueryModel::new(10).with_search("eng");
        // search projection is over `dept` here so "eng" keeps the two eng rows.
        let out = Pipeline::new(data(), &c).search(|r: &Row| r.dept.to_string()).run(&model);
        assert_eq!(out.total, 2);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn filter_model_round_trips_through_json() {
        let m = FilterModel::new()
            .with(Filter::contains("name", "an"))
            .with(Filter::greater_than("age", CellValue::Int(18)))
            .any();
        let json = serde_json::to_string(&m).unwrap();
        let back: FilterModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
