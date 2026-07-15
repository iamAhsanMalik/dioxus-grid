//! A runnable showcase of `grid-dioxus`: sortable + filterable columns, a Set
//! filter, an editable cell, per-row actions with a bulk bar, a footer aggregate,
//! a card view, and pagination — styled entirely by the reference stylesheet.
//!
//!   dx serve --package grid-basic-example --web
//!   dx serve --package grid-basic-example --web --features force-erased  # small wasm

use dioxus::prelude::*;
use grid_dioxus::{Aggregate, DataGrid, FilterKind, GridAction, GridColumn};

const GRID_CSS: Asset = asset!("/assets/grid.css");
const FAVICON: Asset = asset!("/assets/favicon.svg");

fn main() {
    dioxus::launch(App);
}

#[derive(Clone, PartialEq)]
struct Product {
    id: u32,
    name: String,
    category: String,
    price: f64,
    stock: i64,
}

fn seed() -> Vec<Product> {
    let cats = ["Apparel", "Home", "Tech", "Outdoor"];
    (1..=42)
        .map(|i| Product {
            id: i,
            name: format!("Product {i:02}"),
            category: cats[(i as usize) % cats.len()].into(),
            price: ((i * 37) % 400) as f64 + 9.99,
            stock: ((i * 11) % 120) as i64,
        })
        .collect()
}

#[component]
fn App() -> Element {
    let mut products = use_signal(seed);
    let mut log = use_signal(|| String::from("Interact with the grid…"));

    // Columns are declared once; each carries its own projections (render / sort /
    // filter / edit / aggregate). The grid reads them — you never write table markup.
    let columns = vec![
        GridColumn::new("name", "Name", |p: &Product| rsx! { strong { "{p.name}" } })
            .sortable(|p: &Product| p.name.clone())
            .editable(|p: &Product| p.name.clone()),
        GridColumn::new("category", "Category", |p: &Product| rsx! { "{p.category}" })
            .sortable(|p: &Product| p.category.clone())
            .filter(FilterKind::Set),
        GridColumn::new("price", "Price", |p: &Product| rsx! { "${p.price:.2}" })
            .sortable_num(|p: &Product| p.price)
            .aggregate(Aggregate::Sum),
        GridColumn::new("stock", "Stock", |p: &Product| rsx! { "{p.stock}" })
            .sortable_num(|p: &Product| p.stock as f64),
    ];

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Stylesheet { href: GRID_CSS }
        main { style: "max-width:960px;margin:2rem auto;padding:0 1rem;font-family:system-ui;",
            h1 { style: "font-weight:600;", "grid-dioxus — basic example" }
            p { style: "color:#666;", "{log}" }
            DataGrid {
                rows: std::rc::Rc::from(products()),
                columns,
                row_id: |p: &Product| p.id.to_string(),
                search_text: Some((|p: &Product| format!("{} {}", p.name, p.category)) as fn(&Product) -> String),
                actions: Some((|_p: &Product| vec![
                    GridAction::new("view", "View"),
                    GridAction::danger("delete", "Delete"),
                ]) as fn(&Product) -> Vec<GridAction>),
                on_action: move |(key, p): (&'static str, Product)| {
                    if key == "delete" {
                        products.write().retain(|x| x.id != p.id);
                    }
                    log.set(format!("action `{key}` on {}", p.name));
                },
                on_bulk_action: move |(key, rows): (&'static str, Vec<Product>)| {
                    log.set(format!("bulk `{key}` on {} rows", rows.len()));
                },
                on_edit: move |edit: grid_dioxus::CellEdit<Product>| {
                    if edit.key == "name" {
                        if let Some(p) = products.write().iter_mut().find(|x| x.id == edit.row.id) {
                            p.name = edit.value.clone();
                        }
                    }
                    log.set(format!("edited {} → {}", edit.key, edit.value));
                },
                selectable: true,
                page_size: 8,
            }
        }
    }
}
