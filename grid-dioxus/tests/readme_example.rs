//! Compile-check for the example in the README, so the docs cannot drift from the
//! real API.
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
#[allow(dead_code)]
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

#[test]
fn readme_example_compiles() {}
