# dioxus-grid

A headless data grid for Rust. The engine is framework-agnostic; the renderer targets
[Dioxus](https://dioxuslabs.com).

Still early — see the roadmap below for what's landed and what's next.

## Layout

| Crate | What it does |
| --- | --- |
| `grid-core` | Column definitions, the query model, and the filter → sort → paginate pipeline. No UI, no framework. |
| `grid-state` | View state (selection, sort, paging) as a small state machine. |
| `grid-plugin-api` | Extension points for custom cell renderers, filters and exporters. |
| `grid-dioxus` | The Dioxus renderer. Ships behavior and stable class hooks, no styling. |

Non-Dioxus front ends depend on `grid-core` alone.

## Roadmap

- [ ] Core query pipeline (columns, sorting, filtering, pagination)
- [ ] View state + selection
- [ ] Dioxus renderer
- [ ] Virtualization
- [ ] Accessibility pass
- [ ] Docs + examples

## License

Dual-licensed under either

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
