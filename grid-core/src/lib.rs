//! Headless data-operations engine for a data grid.
//!
//! This crate owns the vocabulary a grid uses to describe its data: typed cell
//! values, a sort instruction and per-column metadata. There is no UI, no
//! framework and no DOM here, so the data operations can be unit tested and
//! benchmarked on their own.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::cmp::Ordering;

/// A typed cell value — the scalable basis for sorting (and, later, filtering and
/// grouping).
///
/// ## Why this exists
/// String sort keys re-allocate a lowercased `String` for every comparison
/// **re-allocates a lowercased `String` for every comparison** (`O(n log n)`
/// allocations), which is fine for a few hundred rows but won't scale to the
/// 100k-row, server-virtualized grids this engine targets. A column can instead
/// project each row to a `CellValue` *once*; comparisons are then allocation-free.
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
