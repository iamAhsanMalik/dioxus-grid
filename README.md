# dioxus-grid

A headless data grid for Rust. The engine is framework-agnostic; the renderer
targets [Dioxus](https://dioxuslabs.com).

**[Documentation](https://iamahsanmalik.github.io/dioxus-grid/)** ·
**[Live demo](https://iamahsanmalik.github.io/dioxus-grid/demo/)**

Sorting, per-column filters, pagination, selection, grouping, inline editing,
column reorder/resize/pin and CSV export — with no styling shipped, so the grid
looks like your app rather than like a widget library.

## Layout

| Crate | What it does |
| --- | --- |
| `grid-core` | Column definitions, the query model, and the filter → sort → paginate pipeline. No UI, no framework. |
| `grid-state` | Interaction state: selection, sort cycling, paging, view mode. |
| `grid-plugin-api` | Composable query stages (filters, multi-sort, aggregates) and the data-provider seam. |
| `grid-dioxus` | The Dioxus renderer. Behavior and stable class hooks, no CSS. |

A non-Dioxus front end depends on `grid-core` alone. `grid-dioxus` re-exports the
other three, so consumers only add one dependency.

## Getting started

```toml
[dependencies]
grid-dioxus = { version = "0.1", features = ["web"] }
```

```rust
use std::rc::Rc;

use dioxus::prelude::*;
use grid_dioxus::{DataGrid, GridColumn};

#[derive(Clone, PartialEq)]
struct Row {
    id: u32,
    name: String,
    qty: i64,
}

fn search_text(r: &Row) -> String {
    r.name.clone()
}

#[component]
fn Demo() -> Element {
    let rows: Rc<[Row]> = vec![Row { id: 1, name: "Widget".into(), qty: 12 }].into();
    let columns = vec![
        GridColumn::new("name", "Name", |r: &Row| rsx! { "{r.name}" }).sortable(|r| r.name.clone()),
        GridColumn::new("qty", "Qty", |r: &Row| rsx! { "{r.qty}" }).sortable_num(|r| r.qty as f64),
    ];
    rsx! {
        DataGrid {
            rows,
            columns,
            row_id: |r: &Row| r.id.to_string(),
            search_text: Some(search_text as fn(&Row) -> String),
            selectable: true,
        }
    }
}
```

Rows are passed as an `Rc<[T]>`, so neither your component nor the grid ever clones
the whole dataset — only the visible page is materialised. `row_id` gives rows a
stable identity for selection.

A column that declares a sort key is filterable automatically — the filter stage
reuses the same projection, so there is no second set of configuration.

There is a runnable app in [`examples/basic`](examples/basic), deployed from `main`
to <https://iamahsanmalik.github.io/dioxus-grid/demo/>:

```
dx serve --package grid-basic-example --web
```

## Styling

The renderer is headless: it ships behavior and a documented set of hooks, and no
CSS. Copy [`examples/basic/assets/grid.css`](examples/basic/assets/grid.css) as a
starting point. Everything the grid renders falls into one of three tiers.

**1. Structural classes — `dxg-<part>`.** Stable, semantic names that never change
with state: `dxg-root`, `dxg-toolbar`, `dxg-header-cell`, `dxg-sort-button`,
`dxg-row`, `dxg-cell`, `dxg-checkbox`, `dxg-pagination`, `dxg-filter-popover`,
`dxg-bulk-bar`, `dxg-card`, and so on.

**2. State attributes — `data-<state>`.** Style with attribute selectors, e.g.
`.dxg-row[data-selected] { … }`. Booleans are present-or-absent; enums carry a
value.

| Attribute | On | Values |
| --- | --- | --- |
| `data-sorted` | header cell, sort button | `asc` / `desc` / `none` |
| `data-align` | any cell | `start` / `center` / `end` |
| `data-selected` | row, card | present = selected |
| `data-pinned` | cell | present = pinned |
| `data-dragging` | header cell | present = being reordered |
| `data-hide-mobile` | cell | present = hidden when narrow |
| `data-clickable` | row | present = the row has a click handler |
| `data-view` | root | `table` / `card` |
| `data-loading` | root | present = loading |

The empty and loading states also carry the classes `dxg-empty` and
`dxg-loading`. The full list is in the
[styling guide](https://iamahsanmalik.github.io/dioxus-grid/styling.html).

**3. Inline `style` — geometry only.** Column widths and pinned-column offsets can
only be known at render time, so they are emitted inline. Colors and spacing never
are; the appearance of a pinned cell is yours via `[data-pinned]`.

The reference sheet is driven by `--dxg-*` custom properties, so overriding those
reskins the grid without touching any rules.

## Accessibility

The grid emits its own ARIA — `role="grid"`, `columnheader` with `scope`,
`aria-sort`, `aria-rowcount`, and labels on controls and selection checkboxes. The
`data-*` attributes mirror that state for styling, so the two cannot drift.

## Bundle size

`DataGrid<T>` is generic, so the render body is normally code-generated once per
row type. That is the fast choice on native and the wrong one for wasm, where a
dozen row types means a dozen copies of the renderer.

So there are two paths, chosen at compile time:

- **wasm targets** use a type-erased renderer. A thin generic shell projects rows
  into a `T`-free snapshot and one non-generic body renders it, so the body
  compiles once regardless of how many row types you use.
- **native targets** use the monomorphized renderer.

Override with the `force-erased` or `force-mono` features. Enabling both is a
compile error.

## Features

| Feature | Effect |
| --- | --- |
| `web` | Browser glue; persists column layout to `localStorage`. |
| `export-rich` | Client-side Excel and PDF export. CSV always works without it. |
| `force-mono` / `force-erased` | Override the render path above. |

## Versioning

[Semantic Versioning](https://semver.org). While `0.x`, minor versions may contain
breaking changes; see [CHANGELOG.md](CHANGELOG.md).

## License

Dual-licensed under either

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
