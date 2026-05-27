//! Headless interaction controller for a data grid.
//!
//! Where [`grid_core`] owns the data pipeline (filter, sort, paginate), this
//! crate owns the interaction state machine: the live [`QueryModel`], the
//! selection set and the view mode, plus the intent methods a renderer calls in
//! response to user actions.
//!
//! There is no UI and no framework here. A renderer holds a [`GridState`], calls
//! an intent on interaction, then re-queries its [`grid_core::DataSource`] with
//! [`GridState::model`] to draw the next frame. Keeping the transitions here
//! makes them unit-testable and identical across renderers.
//!
//! The transitions worth knowing:
//! * sort is tri-state per column: unsorted, ascending, descending, unsorted;
//!   switching to a different column starts at ascending,
//! * changing search, sort or page size resets the page to 0,
//! * page selection deselects the page when it is already fully selected,
//!   otherwise selects all of it,
//! * paging never steps out of `0..page_count`.

#![forbid(unsafe_code)]

use std::collections::HashSet;

use grid_core::{QueryModel, Sort};

/// List (row) vs. card (gallery) presentation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    List,
    Card,
}

impl ViewMode {
    pub fn is_card(self) -> bool {
        matches!(self, ViewMode::Card)
    }
}

/// The grid's live interaction state. Construct with [`GridState::new`], drive it
/// with the intent methods, and read [`GridState::model`] to query a data source.
#[derive(Clone, Debug, PartialEq)]
pub struct GridState {
    model: QueryModel,
    selection: HashSet<String>,
    view: ViewMode,
}

impl GridState {
    /// A fresh state: first page of `page_size` rows, no search/sort, nothing
    /// selected. `default_card` picks the initial [`ViewMode`].
    pub fn new(page_size: usize, default_card: bool) -> Self {
        Self {
            model: QueryModel::new(page_size.max(1)),
            selection: HashSet::new(),
            view: if default_card { ViewMode::Card } else { ViewMode::List },
        }
    }

    // ── reads ──────────────────────────────────────────────────────────────

    /// The current query — pass to a [`grid_core::DataSource`] to materialise rows.
    pub fn model(&self) -> &QueryModel {
        &self.model
    }
    pub fn search(&self) -> &str {
        &self.model.search
    }
    pub fn sort(&self) -> Option<Sort> {
        self.model.sort.clone()
    }
    pub fn page(&self) -> usize {
        self.model.page
    }
    pub fn page_size(&self) -> usize {
        self.model.page_size
    }
    pub fn view(&self) -> ViewMode {
        self.view
    }

    /// The sort direction currently applied to `key`, if it is the sorted column.
    /// `Some(true)` = ascending, `Some(false)` = descending, `None` = not sorted
    /// by this column.
    pub fn sort_dir_for(&self, key: &str) -> Option<bool> {
        self.model.sort.as_ref().filter(|s| s.key() == key).map(|s| s.ascending)
    }

    // ── query intents (all reset the page, like the component) ───────────────

    pub fn set_search(&mut self, q: impl Into<String>) {
        self.model.search = q.into();
        self.model.page = 0;
    }
    pub fn clear_search(&mut self) {
        self.model.search.clear();
        self.model.page = 0;
    }

    /// Tri-state header toggle: `unsorted → asc → desc → unsorted`. Clicking a
    /// different column jumps straight to ascending. Always resets the page.
    pub fn toggle_sort(&mut self, key: &'static str) {
        self.model.sort = match &self.model.sort {
            Some(s) if s.key() == key && s.ascending => Some(Sort::desc(key)),
            Some(s) if s.key() == key && !s.ascending => None,
            _ => Some(Sort::asc(key)),
        };
        self.model.page = 0;
    }

    pub fn set_view(&mut self, view: ViewMode) {
        self.view = view;
    }

    // ── pagination intents ───────────────────────────────────────────────────

    /// Jump to a page (caller may pass any value; clamp against `page_count`
    /// from the latest [`grid_core::ResultSet`] when rendering).
    pub fn set_page(&mut self, page: usize) {
        self.model.page = page;
    }
    /// Step forward, staying within `0..page_count`.
    pub fn next_page(&mut self, page_count: usize) {
        if self.model.page + 1 < page_count {
            self.model.page += 1;
        }
    }
    /// Step back, never below 0.
    pub fn prev_page(&mut self) {
        if self.model.page > 0 {
            self.model.page -= 1;
        }
    }
    /// Change rows-per-page (clamped ≥ 1) and return to the first page.
    pub fn set_page_size(&mut self, n: usize) {
        self.model.page_size = n.max(1);
        self.model.page = 0;
    }

    // ── selection intents (keyed by the grid's `row_id`) ─────────────────────

    pub fn is_selected(&self, id: &str) -> bool {
        self.selection.contains(id)
    }
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }
    /// The selected ids (unordered — back the call site with a stable sort if the
    /// order matters for display).
    pub fn selected_ids(&self) -> Vec<String> {
        self.selection.iter().cloned().collect()
    }
    /// Whether every id on the current page is selected (and the page is non-empty).
    pub fn is_page_selected(&self, page_ids: &[String]) -> bool {
        !page_ids.is_empty() && page_ids.iter().all(|i| self.selection.contains(i))
    }
    /// Toggle one row's membership.
    pub fn toggle_row(&mut self, id: &str) {
        if !self.selection.remove(id) {
            self.selection.insert(id.to_string());
        }
    }
    /// Select the whole page, or deselect it if it is already fully selected.
    pub fn toggle_page(&mut self, page_ids: &[String]) {
        if self.is_page_selected(page_ids) {
            for i in page_ids {
                self.selection.remove(i);
            }
        } else {
            for i in page_ids {
                self.selection.insert(i.clone());
            }
        }
    }
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_cycles_tri_state_and_resets_page() {
        let mut s = GridState::new(10, false);
        s.set_page(3);
        s.toggle_sort("name"); // → asc, page reset
        assert_eq!(s.sort(), Some(Sort::asc("name")));
        assert_eq!(s.page(), 0);
        s.toggle_sort("name"); // → desc
        assert_eq!(s.sort(), Some(Sort::desc("name")));
        s.toggle_sort("name"); // → none
        assert_eq!(s.sort(), None);
    }

    #[test]
    fn switching_sort_column_starts_ascending() {
        let mut s = GridState::new(10, false);
        s.toggle_sort("name"); // asc
        s.toggle_sort("name"); // desc
        s.toggle_sort("score"); // different column → asc
        assert_eq!(s.sort(), Some(Sort::asc("score")));
    }

    #[test]
    fn sort_dir_for_reports_only_active_column() {
        let mut s = GridState::new(10, false);
        s.toggle_sort("name");
        assert_eq!(s.sort_dir_for("name"), Some(true));
        assert_eq!(s.sort_dir_for("score"), None);
    }

    #[test]
    fn search_and_page_size_reset_page() {
        let mut s = GridState::new(10, false);
        s.set_page(5);
        s.set_search("acme");
        assert_eq!(s.page(), 0);
        s.set_page(5);
        s.set_page_size(25);
        assert_eq!((s.page(), s.page_size()), (0, 25));
        s.set_page(5);
        s.clear_search();
        assert_eq!((s.page(), s.search()), (0, ""));
    }

    #[test]
    fn page_size_clamped_to_one() {
        let mut s = GridState::new(0, false);
        assert_eq!(s.page_size(), 1);
        s.set_page_size(0);
        assert_eq!(s.page_size(), 1);
    }

    #[test]
    fn paging_stays_in_range() {
        let mut s = GridState::new(10, false);
        s.prev_page(); // already at 0 → no-op
        assert_eq!(s.page(), 0);
        s.next_page(3); // 0 → 1
        s.next_page(3); // 1 → 2
        s.next_page(3); // 2 → 2 (last; no-op)
        assert_eq!(s.page(), 2);
        s.prev_page();
        assert_eq!(s.page(), 1);
    }

    #[test]
    fn row_selection_toggles() {
        let mut s = GridState::new(10, false);
        assert!(!s.is_selected("a"));
        s.toggle_row("a");
        assert!(s.is_selected("a"));
        assert_eq!(s.selection_count(), 1);
        s.toggle_row("a");
        assert!(!s.is_selected("a"));
        assert_eq!(s.selection_count(), 0);
    }

    #[test]
    fn page_selection_selects_then_deselects() {
        let mut s = GridState::new(10, false);
        let page = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(!s.is_page_selected(&page));
        s.toggle_page(&page);
        assert!(s.is_page_selected(&page));
        assert_eq!(s.selection_count(), 3);
        s.toggle_page(&page); // fully selected → deselect page
        assert_eq!(s.selection_count(), 0);
    }

    #[test]
    fn empty_page_is_never_fully_selected() {
        let s = GridState::new(10, false);
        assert!(!s.is_page_selected(&[]));
    }

    #[test]
    fn page_selection_tops_up_a_partial_page() {
        let mut s = GridState::new(10, false);
        s.toggle_row("b");
        let page = vec!["a".to_string(), "b".to_string()];
        s.toggle_page(&page); // not fully selected → select all
        assert!(s.is_selected("a") && s.is_selected("b"));
    }

    #[test]
    fn view_mode_toggles() {
        let mut s = GridState::new(10, true);
        assert_eq!(s.view(), ViewMode::Card);
        s.set_view(ViewMode::List);
        assert!(!s.view().is_card());
    }
}
