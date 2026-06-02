//! The unified data-provider abstraction.
//!
//! A grid renderer should not need to know whether its rows live in memory or
//! behind an API. It builds a [`GridQuery`] from its UI state (search,
//! filters, sort, page, which aggregates) and hands it to a [`DataProvider`],
//! which returns one [`GridPage`] to draw. Two providers implement the seam:
//!
//! * [`LocalProvider`] — an in-memory dataset. It works index-based over a shared
//!   [`Rc<[T]>`], so a query produces a `Vec<usize>` and only the visible page's
//!   rows are cloned. A large local set costs one shared pointer, an `O(n)`
//!   comparison scan and `page_size` clones, never a clone of the whole dataset.
//! * a remote provider (defined by the caller) sends the same `GridQuery` to a
//!   server and returns the page it computed there, so the browser holds only
//!   what is on screen.
//!
//! Because the query and page types are identical for both, switching a grid from
//! local to server-side is a provider swap — no renderer change.

use std::rc::Rc;

use grid_core::ColumnDef;

use crate::{AggValue, Aggregate, CellValue, Combine, Filter, FilterModel, MultiSort, Sort};

/// A projection borrowed from the provider's own column slice.
type BorrowedProjection<'a, T> = Box<dyn Fn(&T) -> CellValue + 'a>;

/// Everything a provider needs to compute one page: the global search text, the
/// column filters, the sort order, the page window, and which columns want a
/// footer aggregate. This is the complete, transport-agnostic request — the same
/// shape whether it's answered locally or shipped to a server.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GridQuery {
    pub search: String,
    pub filters: FilterModel,
    pub sort: MultiSort,
    pub page: usize,
    pub page_size: usize,
    /// `(column key, aggregate)` pairs to compute over the filtered set.
    pub aggregates: Vec<(String, Aggregate)>,
}

impl GridQuery {
    pub fn new(page_size: usize) -> Self {
        Self { page_size: page_size.max(1), ..Default::default() }
    }
}

/// One page of results plus the totals and aggregates the grid renders. Identical
/// for local and remote providers.
#[derive(Clone, Debug, PartialEq)]
pub struct GridPage<T> {
    /// The rows on the current (clamped) page — at most `page_size` of them.
    pub rows: Vec<T>,
    /// Total rows after filtering, before pagination.
    pub total: usize,
    /// Current page after clamping into `0..page_count`.
    pub page: usize,
    /// Number of pages (≥ 1).
    pub page_count: usize,
    /// Aggregate results in the same order they were requested.
    pub aggregates: Vec<(String, AggValue)>,
}

/// The seam the renderer talks to. Synchronous — a [`LocalProvider`] answers
/// immediately. A remote source is inherently async, so the renderer drives it via
/// an async fetcher rather than this trait; both still produce a [`GridPage`].
pub trait DataProvider<T> {
    fn query(&self, q: &GridQuery) -> GridPage<T>;
}

/// In-browser provider over a shared, immutable dataset.
///
/// Holds the rows as [`Rc<[T]>`], so handing the same data to many grids (or
/// re-rendering) is a refcount bump, never a copy. Filtering and sorting run over
/// **indices**, and only the page's rows are materialised — see the module docs.
pub struct LocalProvider<'c, T> {
    rows: Rc<[T]>,
    columns: &'c [ColumnDef<T>],
    search_text: Option<fn(&T) -> String>,
}

impl<'c, T: Clone + 'static> LocalProvider<'c, T> {
    pub fn new(rows: Rc<[T]>, columns: &'c [ColumnDef<T>]) -> Self {
        Self { rows, columns, search_text: None }
    }
    pub fn search(mut self, f: fn(&T) -> String) -> Self {
        self.search_text = Some(f);
        self
    }
    pub fn search_opt(mut self, f: Option<fn(&T) -> String>) -> Self {
        self.search_text = f;
        self
    }

    /// Resolve a column's value projection (same precedence as the stages).
    fn project(&self, key: &str) -> Option<BorrowedProjection<'_, T>> {
        let col = self.columns.iter().find(|c| c.key == key)?;
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

    /// Whether one row passes a single filter (mirrors `FilterStage::matches` but
    /// over a row reference, so no row is cloned to test it).
    fn row_matches(&self, row: &T, f: &Filter) -> bool {
        match self.project(&f.key) {
            Some(p) => f.matches_value(&p(row)),
            None => true, // a filter on an unprojectable column is inert
        }
    }
}

impl<T: Clone + 'static> DataProvider<T> for LocalProvider<'_, T> {
    fn query(&self, q: &GridQuery) -> GridPage<T> {
        // 1) Start from all indices, narrow by global search, then by each filter.
        //    We carry indices — never the rows — through the whole pipeline.
        let needle = q.search.trim().to_lowercase();
        let mut idx: Vec<usize> = (0..self.rows.len())
            .filter(|&i| {
                let row = &self.rows[i];
                if !needle.is_empty() {
                    if let Some(hay) = self.search_text {
                        if !hay(row).to_lowercase().contains(&needle) {
                            return false;
                        }
                    }
                }
                match q.filters.combine {
                    Combine::All => q.filters.filters.iter().all(|f| self.row_matches(row, f)),
                    Combine::Any => {
                        q.filters.filters.is_empty() || q.filters.filters.iter().any(|f| self.row_matches(row, f))
                    }
                }
            })
            .collect();

        // 2) Sort the surviving indices (stable, tie-broken left→right). Projections
        //    are resolved once per sort column, not per comparison.
        if !q.sort.sorts.is_empty() {
            let keys: Vec<(BorrowedProjection<'_, T>, bool)> =
                q.sort.sorts.iter().filter_map(|s: &Sort| self.project(s.key()).map(|p| (p, s.ascending))).collect();
            if !keys.is_empty() {
                idx.sort_by(|&a, &b| {
                    for (p, asc) in &keys {
                        let ord = p(&self.rows[a]).cmp(&p(&self.rows[b]));
                        let ord = if *asc { ord } else { ord.reverse() };
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
            }
        }

        // 3) Aggregate over the filtered set (still index-only; reuses `aggregate`
        //    by materialising just the projected values, not the rows).
        let aggregates = q
            .aggregates
            .iter()
            .map(|(key, agg)| {
                let val = self.aggregate_indices(&idx, key, *agg);
                (key.clone(), val)
            })
            .collect();

        // 4) Paginate the index list, then clone ONLY the page's rows.
        let total = idx.len();
        let psize = q.page_size.max(1);
        let page_count = total.div_ceil(psize).max(1);
        let page = q.page.min(page_count - 1);
        let rows: Vec<T> = idx.into_iter().skip(page * psize).take(psize).map(|i| self.rows[i].clone()).collect();

        GridPage { rows, total, page, page_count, aggregates }
    }
}

impl<T: Clone + 'static> LocalProvider<'_, T> {
    /// Aggregate `agg` over column `key` across the given row indices, without
    /// materialising the rows. `Count` is the index count; numeric aggregates fold
    /// the projected values.
    fn aggregate_indices(&self, idx: &[usize], key: &str, agg: Aggregate) -> AggValue {
        if let Aggregate::Count = agg {
            return AggValue::Count(idx.len());
        }
        let Some(p) = self.project(key) else { return AggValue::Empty };
        let nums: Vec<f64> = idx
            .iter()
            .filter_map(|&i| match p(&self.rows[i]) {
                CellValue::Int(n) => Some(n as f64),
                CellValue::Number(n) if !n.is_nan() => Some(n),
                _ => None,
            })
            .collect();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use grid_core::ColumnDef;

    #[derive(Clone, PartialEq)]
    struct Row {
        name: &'static str,
        age: i64,
    }

    fn provider(rows: Vec<Row>, cols: &[ColumnDef<Row>]) -> LocalProvider<'_, Row> {
        LocalProvider::new(Rc::from(rows), cols).search(|r| r.name.to_string())
    }

    fn cols() -> Vec<ColumnDef<Row>> {
        vec![
            ColumnDef::new("name").sortable(|r: &Row| r.name.to_string()),
            ColumnDef::new("age").sortable_num(|r: &Row| r.age as f64),
        ]
    }

    fn data() -> Vec<Row> {
        vec![
            Row { name: "Carol", age: 30 },
            Row { name: "alice", age: 10 },
            Row { name: "Bob", age: 20 },
            Row { name: "Dave", age: 40 },
        ]
    }

    #[test]
    fn paginates_filtered_sorted_indices_cloning_only_the_page() {
        let c = cols();
        let p = provider(data(), &c);
        let mut q = GridQuery::new(2);
        q.sort = MultiSort::new().then(Sort::asc("name"));
        let page0 = p.query(&q);
        assert_eq!(page0.total, 4);
        assert_eq!(page0.page_count, 2);
        assert_eq!(page0.rows.iter().map(|r| r.name).collect::<Vec<_>>(), ["alice", "Bob"]);
    }

    #[test]
    fn search_and_filter_narrow_the_index_set() {
        let c = cols();
        let p = provider(data(), &c);
        let mut q = GridQuery::new(10);
        q.filters = FilterModel::new().with(Filter::greater_than("age", CellValue::Int(20)));
        let page = p.query(&q);
        // age > 20 → Carol(30), Dave(40)
        assert_eq!(page.total, 2);
    }

    #[test]
    fn aggregates_compute_over_the_filtered_set() {
        let c = cols();
        let p = provider(data(), &c);
        let mut q = GridQuery::new(10);
        q.aggregates = vec![("age".into(), Aggregate::Sum), ("name".into(), Aggregate::Count)];
        let page = p.query(&q);
        assert_eq!(page.aggregates[0], ("age".to_string(), AggValue::Num(100.0)));
        assert_eq!(page.aggregates[1], ("name".to_string(), AggValue::Count(4)));
    }

    #[test]
    fn shared_rows_are_not_cloned_wholesale() {
        // Two providers can share the same Rc dataset — proves shared ownership.
        let rows: Rc<[Row]> = Rc::from(data());
        let c = cols();
        let p1 = LocalProvider::new(rows.clone(), &c);
        let p2 = LocalProvider::new(rows.clone(), &c);
        assert_eq!(Rc::strong_count(&rows), 3); // rows + p1 + p2
        let _ = (p1.query(&GridQuery::new(10)), p2.query(&GridQuery::new(10)));
    }
}
