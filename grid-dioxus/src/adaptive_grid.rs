//! AdaptiveDataGrid — the same [`DataGrid`], with its UI density chosen for the
//! data volume.
//!
//! Small datasets don't need an enterprise toolbar. Below a threshold the grid
//! renders [`GridDensity::Minimal`] (sleek table, quick search + Export, no filter
//! funnels / column chooser / group-by, and no footer when it all fits one page);
//! above it, the full grid. The threshold is a *default* — pass an explicit
//! `density` to pin it, because `rows.len()` alone is a poor signal (a 12-row
//! *filtered slice of 50k* still wants the full controls).
//!
//! It's a thin pass-through: every other [`DataGridProps`] field flows straight to
//! [`DataGrid`], so there's exactly one grid implementation to maintain.
//!
//! ```ignore
//! AdaptiveDataGrid::<Member> {
//!     rows: members(),
//!     columns: cols,
//!     row_id: |m| m.id.clone(),
//!     search_text: |m| m.name.clone(),
//!     export_filename: "members.csv",
//!     // density auto-picked from len(); override with `density: GridDensity::Full`
//! }
//! ```

use dioxus::prelude::*;

use crate::data_grid::{DataGrid, DataGridProps, GridDensity};

/// Default cutoff: at or below this many rows we go [`GridDensity::Minimal`].
const MINIMAL_MAX: usize = 30;

/// At or below this many rows, and only when the page provides a `card`, the grid
/// *starts* in gallery view (big cards read better than a sparse table). It's only
/// the initial mode — the user's List⇆Gallery toggle always overrides. Aligned with
/// `MINIMAL_MAX`: a set small enough for the minimal/compact grid is also small
/// enough that cards read better than rows.
const GALLERY_MAX: usize = 30;

/// Wraps [`DataGrid`] and picks its density from the data volume. Takes the exact
/// same props as [`DataGrid`] (it *is* `DataGridProps`), so it's a drop-in
/// replacement — anywhere you write `DataGrid { .. }` you can write
/// `AdaptiveDataGrid { .. }` instead.
///
/// Density rule: if the caller set `density` explicitly (to anything other than
/// the `Full` default) that wins; otherwise it's `Minimal` at/below
/// [`MINIMAL_MAX`] rows and `Full` above. To *force* Full on a small set, the
/// caller can leave `density` at its default — but the usual escape hatch is to
/// just use plain [`DataGrid`] there.
#[component]
pub fn AdaptiveDataGrid<T: Clone + PartialEq + 'static>(props: DataGridProps<T>) -> Element {
    // In remote mode `rows` may be empty (the host paginates), so use the fetched
    // page's total there; otherwise count the local dataset.
    let count = if props.on_query.is_some() {
        props.remote_page.as_ref().map(|p| p.total).unwrap_or(usize::MAX)
    } else {
        props.rows.len()
    };

    // An explicit non-default `density` from the caller is honored; otherwise auto.
    let density = if props.density != GridDensity::Full {
        props.density
    } else if count <= MINIMAL_MAX {
        GridDensity::Minimal
    } else {
        GridDensity::Full
    };

    // Smart *initial* view: a small set opens as a gallery (cards read better than
    // a sparse table — the project rule is "≤30 rows ⇒ grid/gallery by default").
    // The DataGrid auto-builds a card from the columns when the caller didn't pass
    // one, so this holds even without an explicit `card`. This only seeds the
    // starting mode — `DataGrid` owns the live List⇆Gallery toggle, so the user's
    // choice always wins and sticks for the session. An explicit `grid_default`
    // from the caller is also honored. Remote-paginated grids (card-less, unknown
    // shape) stay in list view.
    let gallery_default = props.grid_default || (props.on_query.is_none() && count <= GALLERY_MAX);

    let mut grid = props;
    grid.density = density;
    grid.grid_default = gallery_default;
    rsx! { DataGrid::<T> { ..grid } }
}
