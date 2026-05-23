//! Headless data-operations engine for a data grid.
//!
//! This crate owns the vocabulary and the pipeline a grid runs over its data:
//! filtering, sorting and pagination, expressed as plain data ([`QueryModel`])
//! evaluated by a [`DataSource`]. There is no UI, no framework and no DOM here —
//! a renderer drives this and draws the resulting [`ResultSet`].
//!
//! Because the query is plain data, the same model can be evaluated in memory
//! by [`ClientSource`] today or handed to a server later.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::cmp::Ordering;

/// A typed cell value — the scalable basis for sorting (and, later, filtering and
/// grouping).
///
/// ## Why this exists
/// A `fn(&T) -> String` sort key re-allocates a lowercased `String` for every
/// comparison, which is fine for a few hundred rows but does not scale to the
/// large, virtualized grids this engine targets. A column can instead project
/// each row to a `CellValue` once; comparisons are then allocation-free.
///
/// ## Ordering
/// [`CellValue`] is **totally ordered** so it can drive `sort_by` directly:
/// * within a variant, the natural order applies (text compared
///   **case-insensitively** to match the legacy string sort; numbers compared by
///   value with **NaN treated as the largest**, so a stray NaN never panics);
/// * [`CellValue::Empty`] sorts **first** (blanks lead in ascending order);
/// * across different non-empty variants, a fixed precedence
///   (`Bool < Int < Number < Text`) keeps the order *stable and total* — mixing
///   types in one column is a caller mistake, but it must never panic.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CellValue {
    /// Absent / blank. Sorts before every other value.
    Empty,
    Bool(bool),
    /// A 64-bit signed integer (counts, ids, quantities).
    Int(i64),
    /// A floating-point number (money, rates, scores).
    Number(f64),
    /// Text. Compared case-insensitively, matching the legacy `sort_key` path.
    Text(String),
}

impl CellValue {
    /// Fixed cross-variant precedence used only when a column mixes types.
    fn rank(&self) -> u8 {
        match self {
            CellValue::Empty => 0,
            CellValue::Bool(_) => 1,
            CellValue::Int(_) => 2,
            CellValue::Number(_) => 3,
            CellValue::Text(_) => 4,
        }
    }

    /// Promote to `f64` for cross-numeric (`Int` ⇄ `Number`) comparison.
    fn as_f64(&self) -> Option<f64> {
        match self {
            CellValue::Int(i) => Some(*i as f64),
            CellValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Total comparison. Numbers are compared with NaN treated as the largest, so
    /// this never panics and never returns `None`.
    fn total_cmp(&self, other: &CellValue) -> Ordering {
        // Same-kind fast paths first.
        match (self, other) {
            (CellValue::Empty, CellValue::Empty) => Ordering::Equal,
            (CellValue::Bool(a), CellValue::Bool(b)) => a.cmp(b),
            (CellValue::Text(a), CellValue::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            _ => {
                // Int/Number compare numerically across the two numeric variants.
                if let (Some(a), Some(b)) = (self.as_f64(), other.as_f64()) {
                    a.partial_cmp(&b).unwrap_or_else(|| {
                        // NaN handling: a NaN sorts after a non-NaN; two NaNs equal.
                        match (a.is_nan(), b.is_nan()) {
                            (true, true) => Ordering::Equal,
                            (true, false) => Ordering::Greater,
                            (false, true) => Ordering::Less,
                            (false, false) => Ordering::Equal, // unreachable
                        }
                    })
                } else {
                    // Genuinely different variants (e.g. Text vs Bool): fixed rank.
                    self.rank().cmp(&other.rank())
                }
            }
        }
    }
}

// `PartialEq`/`Eq` are defined *via* the total order so they stay consistent with
// it. This deliberately differs from raw `f64`: within a `CellValue`,
// `Number(NaN) == Number(NaN)` is **true**, which is what makes [`Ord`]/[`Eq`]
// reflexive (and lets a column of NaNs sort and dedup sanely).
impl PartialEq for CellValue {
    fn eq(&self, other: &Self) -> bool {
        self.total_cmp(other) == Ordering::Equal
    }
}

impl Eq for CellValue {}

impl PartialOrd for CellValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CellValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.total_cmp(other)
    }
}

impl From<&str> for CellValue {
    fn from(s: &str) -> Self {
        CellValue::Text(s.to_string())
    }
}
impl From<String> for CellValue {
    fn from(s: String) -> Self {
        CellValue::Text(s)
    }
}
impl From<i64> for CellValue {
    fn from(i: i64) -> Self {
        CellValue::Int(i)
    }
}
impl From<f64> for CellValue {
    fn from(n: f64) -> Self {
        CellValue::Number(n)
    }
}
impl From<bool> for CellValue {
    fn from(b: bool) -> Self {
        CellValue::Bool(b)
    }
}

/// A single-column sort instruction.
///
/// `ascending == true` yields the column's natural order; `false` reverses it.
///
/// `key` is a [`Cow<'static, str>`] so it costs nothing when built from a
/// `&'static str` column key (the common in-process case) yet still round-trips
/// through serde — deserialization (e.g. a query sent to a server) yields the
/// owned variant. Use [`Sort::key`] to read it as `&str`.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sort {
    pub key: Cow<'static, str>,
    pub ascending: bool,
}

impl Sort {
    pub fn new(key: impl Into<Cow<'static, str>>, ascending: bool) -> Self {
        Self { key: key.into(), ascending }
    }
    pub fn asc(key: impl Into<Cow<'static, str>>) -> Self {
        Self { key: key.into(), ascending: true }
    }
    pub fn desc(key: impl Into<Cow<'static, str>>) -> Self {
        Self { key: key.into(), ascending: false }
    }
    /// The sort column key as a string slice.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// The complete description of a grid view: what to search, sort, and which page.
///
/// This is plain, cloneable data so the same model can drive an in-memory
/// [`ClientSource`] today or a remote `DataSource` tomorrow.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct QueryModel {
    /// Global search text. Empty (after trim) means "no filter".
    pub search: String,
    /// Active sort, if any.
    pub sort: Option<Sort>,
    /// Zero-based page index. Clamped into range by the source.
    pub page: usize,
    /// Rows per page. Clamped to at least 1 by the source.
    pub page_size: usize,
}

impl QueryModel {
    /// A first page of `page_size` rows with no search or sort.
    pub fn new(page_size: usize) -> Self {
        Self { search: String::new(), sort: None, page: 0, page_size }
    }
    pub fn with_search(mut self, q: impl Into<String>) -> Self {
        self.search = q.into();
        self
    }
    pub fn with_sort(mut self, sort: Sort) -> Self {
        self.sort = Some(sort);
        self
    }
    pub fn with_page(mut self, page: usize) -> Self {
        self.page = page;
        self
    }
}

/// Data-relevant metadata for one column: how to *sort* it.
///
/// Note this carries **no rendering** — `render`, alignment, width, etc. are view
/// concerns that stay in the renderer's column type. A numeric key
/// ([`Self::sortable_num`]) takes precedence over a string key
/// ([`Self::sortable`]).
pub struct ColumnDef<T> {
    pub key: &'static str,
    /// Typed value projection — the **preferred**, allocation-light sort path.
    /// When set it wins over `sort_num`/`sort_key`. See [`CellValue`].
    pub value: Option<fn(&T) -> CellValue>,
    pub sort_key: Option<fn(&T) -> String>,
    pub sort_num: Option<fn(&T) -> f64>,
}

impl<T> ColumnDef<T> {
    pub fn new(key: &'static str) -> Self {
        Self { key, value: None, sort_key: None, sort_num: None }
    }
    /// Sort this column by a **typed** [`CellValue`] projection — the scalable
    /// path (no per-comparison allocation). Wins over `sortable`/`sortable_num`.
    pub fn typed(mut self, f: fn(&T) -> CellValue) -> Self {
        self.value = Some(f);
        self
    }
    /// Sort this column by a string projection (compared case-insensitively).
    pub fn sortable(mut self, f: fn(&T) -> String) -> Self {
        self.sort_key = Some(f);
        self
    }
    /// Sort this column by a numeric projection (wins over a string key).
    pub fn sortable_num(mut self, f: fn(&T) -> f64) -> Self {
        self.sort_num = Some(f);
        self
    }
}

impl<T> Clone for ColumnDef<T> {
    fn clone(&self) -> Self {
        Self { key: self.key, value: self.value, sort_key: self.sort_key, sort_num: self.sort_num }
    }
}
/// The materialised window a renderer draws for a given [`QueryModel`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResultSet<T> {
    /// Rows on the (clamped) current page.
    pub rows: Vec<T>,
    /// Total rows after filtering (before pagination).
    pub total: usize,
    /// The current page index after clamping into `0..page_count`.
    pub page: usize,
    /// Number of pages (always at least 1).
    pub page_count: usize,
}

impl<T> ResultSet<T> {
    /// 1-based index of the first visible row (0 when empty) — for "Showing X–Y".
    pub fn from_index(&self, page_size: usize) -> usize {
        if self.total == 0 {
            0
        } else {
            self.page * page_size.max(1) + 1
        }
    }
    /// 1-based index of the last visible row — for "Showing X–Y".
    pub fn to_index(&self, page_size: usize) -> usize {
        (self.page * page_size.max(1) + self.rows.len()).min(self.total)
    }
}

/// The seam that lets the grid run client-side **or** server-side data ops from
/// the same [`QueryModel`]. Implement this over a `Vec<T>` ([`ClientSource`]) or
/// over a REST/GraphQL/OData backend (a future `RemoteSource`).
pub trait DataSource<T> {
    fn query(&self, model: &QueryModel) -> ResultSet<T>;
}

/// In-memory data source running the filter → sort → paginate pipeline.
pub struct ClientSource<T> {
    pub rows: Vec<T>,
    pub columns: Vec<ColumnDef<T>>,
    /// Projection used by the global search box. `None` disables searching
    /// (the search term is ignored).
    pub search_text: Option<fn(&T) -> String>,
}

impl<T: Clone> ClientSource<T> {
    pub fn new(rows: Vec<T>, columns: Vec<ColumnDef<T>>) -> Self {
        Self { rows, columns, search_text: None }
    }
    /// Attach the global-search projection.
    pub fn searchable(mut self, f: fn(&T) -> String) -> Self {
        self.search_text = Some(f);
        self
    }
}

impl<T: Clone> DataSource<T> for ClientSource<T> {
    fn query(&self, m: &QueryModel) -> ResultSet<T> {
        // ── filter ──────────────────────────────────────────────────────────
        let q = m.search.trim().to_lowercase();
        let mut rows: Vec<T> = match self.search_text {
            Some(hay) if !q.is_empty() => {
                self.rows.iter().filter(|r| hay(r).to_lowercase().contains(&q)).cloned().collect()
            }
            _ => self.rows.clone(),
        };

        // ── sort ────────────────────────────────────────────────────────────
        // Precedence: typed `value` (preferred, allocation-light) → `sort_num` →
        // `sort_key`. `ascending == false` reverses the natural order, so all
        // three key paths stay consistent with each other.
        if let Some(Sort { key, ascending }) = &m.sort {
            if let Some(col) = self.columns.iter().find(|c| c.key == key.as_ref()) {
                if let Some(val) = col.value {
                    rows.sort_by_key(val);
                } else if let Some(num) = col.sort_num {
                    rows.sort_by(|a, b| num(a).partial_cmp(&num(b)).unwrap_or(Ordering::Equal));
                } else if let Some(skey) = col.sort_key {
                    rows.sort_by_key(|r| skey(r).to_lowercase());
                }
                // Reverse for descending, once the column is found.
                if !*ascending {
                    rows.reverse();
                }
            }
        }

        // ── paginate ────────────────────────────────────────────────────────
        let total = rows.len();
        let psize = m.page_size.max(1);
        let page_count = total.div_ceil(psize).max(1);
        let page = m.page.min(page_count - 1);
        let page_rows: Vec<T> = rows.iter().skip(page * psize).take(psize).cloned().collect();

        ResultSet { rows: page_rows, total, page, page_count }
    }
}

/// A **server-side** data source: the grid runs filter/sort/paginate on the
/// backend and this returns the page the server sent.
///
/// ## Why a fetcher closure
/// A headless engine must not pick an async runtime or HTTP client — that's the
/// host's choice. So `RemoteSource` is parameterised by a synchronous **fetcher**
/// `F: Fn(&QueryModel) -> ResultSet<T>`. The host wires `F` to its transport:
/// serialize the [`QueryModel`] (enable the `serde` feature), send it, await the
/// response *outside* this call, deserialize the rows + counts into a
/// [`ResultSet`], and hand it back. The engine stays sync, pure, and runtime-free
/// while [`DataSource`] remains the single seam shared with [`ClientSource`].
///
/// In a Dioxus app the typical shape is: a `use_resource` performs the real
/// `async fetch(model)`; once it resolves, the component builds a
/// `RemoteSource { fetch: move |_| resolved.clone() }` (or simply uses the
/// resolved `ResultSet` directly). The trait still lets server- and client-side
/// grids share one code path.
///
/// ```
/// # use grid_core::{RemoteSource, DataSource, QueryModel, ResultSet};
/// // Pretend this closure is backed by a REST endpoint that already did the work.
/// let src = RemoteSource::new(|m: &QueryModel| ResultSet {
///     rows: vec!["server-row".to_string()],
///     total: 1,
///     page: m.page,
///     page_count: 1,
/// });
/// let page = src.query(&QueryModel::new(10).with_search("anything"));
/// assert_eq!(page.rows, ["server-row"]);
/// ```
pub struct RemoteSource<T, F: Fn(&QueryModel) -> ResultSet<T>> {
    fetch: F,
    _marker: core::marker::PhantomData<fn() -> T>,
}

impl<T, F: Fn(&QueryModel) -> ResultSet<T>> RemoteSource<T, F> {
    /// Wrap a fetcher that maps a [`QueryModel`] to the server's [`ResultSet`].
    pub fn new(fetch: F) -> Self {
        Self { fetch, _marker: core::marker::PhantomData }
    }
}

impl<T, F: Fn(&QueryModel) -> ResultSet<T>> DataSource<T> for RemoteSource<T, F> {
    fn query(&self, model: &QueryModel) -> ResultSet<T> {
        (self.fetch)(model)
    }
}
