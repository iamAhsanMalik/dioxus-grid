//! A showcase of `grid-dioxus` over a realistic dataset: rich cell rendering,
//! several filter kinds, footer aggregates, inline editing, per-row and bulk
//! actions, selection, grouping, the card view and pagination.
//!
//!   dx serve --package grid-basic-example --web
//!   dx serve --package grid-basic-example --web --features force-erased  # small wasm

use dioxus::prelude::*;
use grid_dioxus::{Aggregate, DataGrid, FilterKind, GridAction, GridColumn};

const GRID_CSS: Asset = asset!("/assets/grid.css");
const DEMO_CSS: Asset = asset!("/assets/demo.css");
const FAVICON: Asset = asset!("/assets/favicon.svg");

fn main() {
    dioxus::launch(App);
}

#[derive(Clone, PartialEq)]
struct Order {
    id: u32,
    reference: String,
    customer: String,
    region: String,
    channel: String,
    status: Status,
    total: f64,
    items: i64,
    rating: u8,
    placed: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Paid,
    Pending,
    Shipped,
    Refunded,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Paid => "Paid",
            Status::Pending => "Pending",
            Status::Shipped => "Shipped",
            Status::Refunded => "Refunded",
        }
    }
    fn tone(self) -> &'static str {
        match self {
            Status::Paid => "ok",
            Status::Pending => "warn",
            Status::Shipped => "info",
            Status::Refunded => "bad",
        }
    }
}

/// A deterministic pseudo-random dataset, so the demo looks the same on every
/// load and nobody has to wire up a backend to see the grid work.
fn seed() -> Vec<Order> {
    const FIRST: [&str; 12] = [
        "Amelia", "Hina", "Marcus", "Priya", "Jonas", "Leila", "Diego", "Yuki", "Omar", "Sofia", "Noah", "Zara",
    ];
    const LAST: [&str; 8] = ["Khan", "Silva", "Okafor", "Nakamura", "Rossi", "Haddad", "Novak", "Reyes"];
    const REGIONS: [&str; 5] = ["North America", "Europe", "Middle East", "South Asia", "East Asia"];
    const CHANNELS: [&str; 4] = ["Web", "Mobile app", "Marketplace", "In store"];
    const STATUSES: [Status; 4] = [Status::Paid, Status::Pending, Status::Shipped, Status::Refunded];

    (1..=124u32)
        .map(|i| {
            let n = i as usize;
            let day = 1 + (n * 7) % 28;
            let month = 1 + (n * 3) % 12;
            Order {
                id: i,
                reference: format!("ORD-{:05}", 10_000 + i * 37 % 9000),
                customer: format!("{} {}", FIRST[n % FIRST.len()], LAST[(n / 2) % LAST.len()]),
                region: REGIONS[n % REGIONS.len()].into(),
                channel: CHANNELS[(n / 3) % CHANNELS.len()].into(),
                status: STATUSES[(n * 5) % STATUSES.len()],
                total: (((n * 419) % 240_000) as f64) / 100.0 + 12.5,
                items: 1 + ((n * 13) % 9) as i64,
                rating: 1 + ((n * 7) % 5) as u8,
                // ISO so the DateRange filter's lexicographic compare is correct.
                placed: format!("2026-{month:02}-{day:02}"),
            }
        })
        .collect()
}

/// Is the app running inside the docs' `<iframe>` (`?embed=1`)? In embed mode the
/// page heading and footer are dropped, because the surrounding docs page already
/// provides them — the frame then holds only the grid and needs no scrollbar of its
/// own. Non-web builds are never embedded.
fn embedded() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .is_some_and(|s| s.contains("embed=1"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}

#[component]
fn App() -> Element {
    let mut orders = use_signal(seed);
    let mut log = use_signal(|| String::from("Sort a column, open a filter, select some rows."));
    let embed = use_hook(embedded);

    // Columns are declared once. Each carries its own projections: how to render,
    // sort, filter, edit, aggregate and export. The grid reads them, so there is
    // no table markup anywhere in this file.
    let columns = vec![
        GridColumn::new("reference", "Order", |o: &Order| {
            rsx! { span { class: "mono", "{o.reference}" } }
        })
        .sortable(|o: &Order| o.reference.clone())
        .width("8.5rem"),
        GridColumn::new("customer", "Customer", |o: &Order| {
            rsx! {
                div { class: "cust",
                    span { class: "avatar", "{initials(&o.customer)}" }
                    div {
                        strong { "{o.customer}" }
                        span { class: "sub", "{o.region}" }
                    }
                }
            }
        })
        .sortable(|o: &Order| o.customer.clone())
        .editable(|o: &Order| o.customer.clone())
        .csv(|o: &Order| o.customer.clone()),
        GridColumn::new("channel", "Channel", |o: &Order| rsx! { "{o.channel}" })
            .sortable(|o: &Order| o.channel.clone())
            .filter(FilterKind::Set),
        GridColumn::new("status", "Status", |o: &Order| {
            rsx! {
                span { class: "chip chip-{o.status.tone()}", "{o.status.label()}" }
            }
        })
        // A Set filter builds its checklist from the string sort key, so this
        // column sorts by label rather than by `rank()`. Numeric sorting and a
        // Set filter are mutually exclusive on the same column.
        .sortable(|o: &Order| o.status.label().to_string())
        .filter(FilterKind::Set)
        .csv(|o: &Order| o.status.label().to_string()),
        GridColumn::new("placed", "Placed", |o: &Order| {
            rsx! { span { class: "mono sub", "{o.placed}" } }
        })
        .sortable(|o: &Order| o.placed.clone())
        .filter(FilterKind::DateRange)
        .mobile_hidden(),
        GridColumn::new("rating", "Rating", |o: &Order| {
            rsx! {
                span { class: "stars", title: "{o.rating} of 5",
                    "{stars(o.rating)}"
                }
            }
        })
        .sortable_num(|o: &Order| o.rating as f64)
        .filter(FilterKind::Rating)
        .center()
        .mobile_hidden(),
        GridColumn::new("items", "Items", |o: &Order| rsx! { "{o.items}" })
            .sortable_num(|o: &Order| o.items as f64)
            .aggregate(Aggregate::Sum)
            .right()
            .width("5rem"),
        GridColumn::new("total", "Total", |o: &Order| {
            rsx! { strong { "${o.total:.2}" } }
        })
        .sortable_num(|o: &Order| o.total)
        // Without this the CSV would carry the raw f64 (20.880000000000003); the
        // export uses `csv` in preference to the sort key, so money stays money.
        .csv(|o: &Order| format!("{:.2}", o.total))
        .filter(FilterKind::Range)
        .aggregate(Aggregate::Sum)
        .right()
        .width("7rem"),
    ];

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Stylesheet { href: GRID_CSS }
        document::Stylesheet { href: DEMO_CSS }

        div { class: if embed { "page page-embed" } else { "page" },
            if !embed {
                header { class: "page-head",
                    div {
                        p { class: "eyebrow", "grid-dioxus" }
                        h1 { "Orders" }
                        p { class: "lead",
                            "124 rows, rendered by a headless grid. Sort any column, open a filter,
                             edit a customer name inline, select rows for the bulk bar, or switch to
                             card view. Nothing here is styled by the crate — this page brings its own CSS."
                        }
                    }
                    a { class: "docs-link", href: "../index.html", "Documentation →" }
                }
            }

            p { class: "log", "{log}" }

            DataGrid {
                rows: std::rc::Rc::from(orders()),
                columns,
                row_id: |o: &Order| o.id.to_string(),
                search_text: Some((|o: &Order| {
                    format!("{} {} {} {}", o.reference, o.customer, o.region, o.channel)
                }) as fn(&Order) -> String),
                actions: Some((|_o: &Order| {
                    vec![
                        GridAction::new("view", "View order"),
                        GridAction::new("invoice", "Download invoice"),
                        GridAction::danger("refund", "Refund"),
                    ]
                }) as fn(&Order) -> Vec<GridAction>),
                on_action: move |(key, o): (&'static str, Order)| {
                    if key == "refund" {
                        if let Some(row) = orders.write().iter_mut().find(|x| x.id == o.id) {
                            row.status = Status::Refunded;
                        }
                    }
                    log.set(format!("{key} — {}", o.reference));
                },
                on_bulk_action: move |(key, rows): (&'static str, Vec<Order>)| {
                    log.set(format!("bulk {key} on {} orders", rows.len()));
                },
                on_edit: move |edit: grid_dioxus::CellEdit<Order>| {
                    if edit.key == "customer" {
                        if let Some(row) = orders.write().iter_mut().find(|x| x.id == edit.row.id) {
                            row.customer = edit.value.clone();
                        }
                    }
                    log.set(format!("{} set to “{}”", edit.key, edit.value));
                },
                selectable: true,
                // A shorter page when embedded: the docs frame is a fixed height, and
                // the grid must fit it without a scrollbar of its own even once the
                // filter-chip bar appears.
                page_size: if embed { 8 } else { 12 },
                export_filename: "orders.csv",
                // This demo is a static page with no backend. On web, Excel and PDF
                // are encoded by the host's `/export` endpoint, so offering them here
                // would be offering something that cannot finish — CSV is encoded in
                // the browser and works standalone.
                export_formats: Some(&[grid_dioxus::ExportFormat::Csv] as &[_]),
                // Card body for the grid/list toggle. Same row, laid out for a tile —
                // the grid renders whichever view is active; nothing else changes.
                card: Some((|o: &Order| {
                    rsx! {
                        div { class: "ocard",
                            div { class: "ocard-top",
                                span { class: "avatar", "{initials(&o.customer)}" }
                                div { class: "ocard-who",
                                    strong { "{o.customer}" }
                                    span { class: "sub", "{o.region}" }
                                }
                                span { class: "chip chip-{o.status.tone()}", "{o.status.label()}" }
                            }
                            div { class: "ocard-mid",
                                span { class: "mono sub", "{o.reference}" }
                                span { class: "stars", "{stars(o.rating)}" }
                            }
                            div { class: "ocard-foot",
                                span { class: "sub", "{o.channel} · {o.items} items" }
                                strong { "${o.total:.2}" }
                            }
                        }
                    }
                }) as fn(&Order) -> Element),
            }

            if !embed {
                footer { class: "page-foot",
                    span { "Headless: every element carries a " code { "dxg-" } " class and " code { "data-" } " state." }
                    a { href: "https://github.com/iamAhsanMalik/dioxus-grid", "Source" }
                }
            }
        }
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

fn stars(n: u8) -> String {
    let n = n.min(5) as usize;
    format!("{}{}", "★".repeat(n), "☆".repeat(5 - n))
}
