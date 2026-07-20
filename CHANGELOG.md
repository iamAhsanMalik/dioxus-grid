# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows
[Semantic Versioning](https://semver.org).

While the project is `0.x`, minor releases may contain breaking changes.

## [Unreleased]

### Added

- A documentation site under `docs/`, published to GitHub Pages alongside the
  wasm demo. Static HTML and one stylesheet, no build step.
- The example is now a fuller showcase: 124 rows with custom cell rendering,
  several filter kinds, footer aggregates and bulk actions.

### Fixed

- The README's styling table listed three things the renderer does not emit:
  `data-density` (`GridDensity` is `Full`/`Minimal` and selects which chrome
  renders, not spacing), `data-empty` (the empty state is the `dxg-empty` class),
  and `data-selected` on checkboxes (it is on rows and cards).

### Known issues

- The type-erased renderer does not draw the export control, so the CSV, Excel
  and PDF menu is missing on wasm builds unless `force-mono` is enabled.
- A `Set` filter builds its checklist from the string sort key, so a column that
  declares only `.sortable_num(…)` shows an empty filter with no error.

## [0.1.0] - 2026-07-11

First release. Four crates: a framework-agnostic engine, an interaction
controller, composable query stages, and a Dioxus renderer.

### Added

- `grid-core` — typed `CellValue` with a total ordering, `ColumnDef`, `Sort`, a
  plain-data `QueryModel`, and `ClientSource` running filter → sort → paginate.
  `DataSource` is the seam a server-backed source implements.
- `grid-state` — `GridState` holds the live query, selection set and view mode, and
  exposes the intent methods a renderer calls. Sorting a column cycles ascending,
  descending, off; changing search, sort or page size resets to the first page.
- `grid-plugin-api` — `QueryStage` plus column filters, multi-column sort and
  aggregates built on it, a `Pipeline` that chains them, and the `DataProvider`
  seam with an index-based `LocalProvider`.
- `grid-dioxus` — the renderer: search, sorting with shift-click tie-breakers,
  per-column filter popovers, pagination, row and page selection with a bulk bar,
  grouping with subtotals, column reorder/resize/pin/hide, inline editing, card
  view, CSV export (Excel and PDF behind `export-rich`), and a remote mode driven
  by `GridQuery`/`GridPage`. Headless: stable `dxg-*` classes and `data-*` state
  attributes, no CSS shipped.
- A type-erased render path so the renderer body compiles once per build rather
  than once per row type, chosen automatically on wasm targets and overridable with
  `force-mono` / `force-erased`.
- Column layout (order, widths, hidden, pinned) persists to `localStorage` behind
  the `web` feature.
- A runnable example with a reference stylesheet in `examples/basic`.

### Fixed

- Selection checkboxes had no accessible name, and header cells were missing
  `scope="col"`.
- The crate-level doctest did not compile: `rows` takes an `Rc<[T]>` and `row_id`
  is required.

### Changed

- Minimum supported Rust version is 1.85. Dioxus 0.7 depends on `prelude_2024`,
  which is not available on 1.84 despite what the dependency metadata declares.
- The workspace uses Cargo's resolver 3.

[Unreleased]: https://github.com/iamAhsanMalik/dioxus-grid/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/iamAhsanMalik/dioxus-grid/releases/tag/v0.1.0
