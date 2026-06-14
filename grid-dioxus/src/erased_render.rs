//! The non-generic grid renderer (compiled only in `grid_erased` mode).
//!
//! Consumes an [`ErasedGrid`] — which already has `T` projected away — so this
//! whole body compiles exactly once regardless of how many row types the app has.
//! It reuses the same `dxg-*` / `data-*` headless markup as the monomorphized
//! renderer; only the data source differs (an `ErasedRow` snapshot instead of live
//! `fn(&T)` projections), and interactions write back through `ErasedState`, which
//! the generic shell re-queries on.
//!
//! Implemented: search, tri-state sort, pagination, per-column filters (funnel +
//! popover), active-filter chips, row + page selection, bulk-action bar, per-row
//! actions (kebab menu), card/gallery view + toggle, inline cell editing, and
//! group-by (header rows on value change).
//!
//! Not yet in erased mode (fall back to `force-mono` if you need them): the
//! **column chooser** (show/hide + pin columns) and the **export menu** (CSV /
//! signed-PDF). Both require extra shell-side projection/serialization that the
//! monomorphized renderer does inline; they're the only feature gaps between the
//! two paths. Tracked as follow-ups.

#![cfg(grid_erased)]

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::data_grid::{FilterKind, GridAlign};
use crate::erased::{ErasedColumn, ErasedGrid, ErasedState};
use crate::grid_plugin_api::FilterOp;
use crate::grid_state::ViewMode;

fn align_attr(a: GridAlign) -> &'static str {
    match a {
        GridAlign::Right => "end",
        GridAlign::Center => "center",
        _ => "start",
    }
}

/// Is a stored filter entry actually doing something? (Mirrors the mono helper;
/// non-generic so shared freely.)
fn filter_active(op: &FilterOp, value: &str) -> bool {
    matches!(op, FilterOp::IsEmpty | FilterOp::IsNotEmpty) || !value.trim().is_empty()
}

/// Renders a prepared [`ErasedGrid`]. `DataGrid<T>` builds the snapshot + owns the
/// interaction state; this non-generic body draws it and writes interactions back
/// through `state` (which the shell re-queries on).
#[component]
pub fn ErasedDataGrid(grid: ReadSignal<ErasedGrid>, state: ErasedState) -> Element {
    let g = grid.read();
    let mut gs = state.grid;
    let mut filters = state.filters;
    // Which column's filter popover is open (by key), + the funnel click anchor.
    let mut filter_popover = use_signal(|| None::<&'static str>);
    let mut popover_xy = use_signal(|| (0.0f64, 0.0f64));
    // Which row's kebab action menu is open (by row id). The menu renders inline,
    // CSS-positioned relative to its button — no coordinates.
    let mut menu_for = use_signal(|| None::<String>);
    // Which cell is being inline-edited: (row_id, column_key), + the live draft.
    let mut editing = use_signal(|| None::<(String, &'static str)>);
    let mut edit_draft = use_signal(String::new);
    let mut group_by = state.group_by;
    let mut show_group_menu = use_signal(|| false);

    let is_loading = g.loading;
    let is_empty = g.rows.is_empty();
    let selectable = g.selectable;
    let is_card = gs.read().view().is_card();
    let show_card_toggle = g.has_card && !g.no_card_toggle;
    let sel_count = gs.read().selection_count();
    // Bulk actions: those common to every selected VISIBLE row (bulkable + enabled).
    // (The mono grid derives from the whole selection; the erased path uses the
    // selected rows currently in view — the common case for a page-bounded UI.)
    let selected_ids: Vec<String> = gs.read().selected_ids();
    let bulk_actions: Vec<crate::data_grid::GridAction> = {
        let sel_rows: Vec<&crate::erased::ErasedRow> = g.rows.iter().filter(|r| selected_ids.contains(&r.id)).collect();
        match sel_rows.first() {
            Some(first) => first
                .actions
                .iter()
                .filter(|a| a.bulkable && !a.disabled)
                .filter(|a| sel_rows.iter().all(|r| r.actions.iter().any(|x| x.key == a.key)))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    };
    let page_ids: Vec<String> = g.rows.iter().map(|r| r.id.clone()).collect();
    let all_page_selected = !page_ids.is_empty() && gs.read().is_page_selected(&page_ids);
    let has_actions = g.rows.iter().any(|r| !r.actions.is_empty());

    rsx! {
        div {
            class: "dxg-root",
            "data-view": "table",
            "data-loading": if is_loading { "true" },

            // ── Toolbar (select-page · search · group · view toggle) ─────────
            if g.has_search || show_card_toggle || selectable {
                div { class: "dxg-toolbar",
                    // Select-all-on-page — first control, shown in every view (the
                    // card view has no header checkbox, so this is how you bulk-select).
                    if selectable {
                        button {
                            r#type: "button",
                            title: if all_page_selected { "Deselect all rows on this page" } else { "Select all rows on this page" },
                            "aria-pressed": all_page_selected,
                            class: "dxg-button",
                            "data-active": if all_page_selected { "true" },
                            onclick: {
                                let ids = page_ids.clone();
                                move |_| gs.write().toggle_page(&ids)
                            },
                            svg {
                                width: "15", height: "15", view_box: "0 0 24 24",
                                fill: "none", stroke: "currentColor", stroke_width: "2",
                                stroke_linecap: "round", stroke_linejoin: "round",
                                rect { x: "3", y: "3", width: "18", height: "18", rx: "2" }
                                path { d: "m8 12 2.5 2.5L16 9" }
                            }
                            span { class: "dxg-button-label dxg-sm-only",
                                if all_page_selected { "Deselect" } else { "Page" }
                            }
                        }
                    }
                    if g.has_search {
                        div { class: "dxg-search", role: "search",
                            input {
                                r#type: "text",
                                class: "dxg-search-input",
                                placeholder: "Search…",
                                value: "{gs.read().search()}",
                                oninput: move |e: FormEvent| gs.write().set_search(e.value()),
                            }
                            if !gs.read().search().trim().is_empty() {
                                button {
                                    r#type: "button",
                                    "aria-label": "Clear search",
                                    class: "dxg-search-clear",
                                    onclick: move |_| gs.write().clear_search(),
                                    "×"
                                }
                            }
                        }
                    }
                    div { class: "dxg-toolbar-spacer" }
                    {
                        let groupables: Vec<(&'static str, &'static str)> = g.columns.iter().filter(|c| c.groupable).map(|c| (c.key, c.label)).collect();
                        if !groupables.is_empty() && !is_card {
                            rsx! {
                                div { class: "dxg-dropdown",
                                    button {
                                        r#type: "button",
                                        class: "dxg-button",
                                        "data-active": if group_by.read().is_some() { "true" },
                                        onclick: move |_| { let v = *show_group_menu.peek(); show_group_menu.set(!v); },
                                        span { class: "dxg-button-label",
                                            {
                                                match *group_by.read() {
                                                    Some(gk) => groupables.iter().find(|(k, _)| *k == gk).map(|(_, l)| format!("Grouped: {l}")).unwrap_or_else(|| "Group".into()),
                                                    None => "Group".to_string(),
                                                }
                                            }
                                        }
                                    }
                                    if *show_group_menu.read() {
                                        div { class: "dxg-veil", onclick: move |_| show_group_menu.set(false) }
                                        div { class: "dxg-menu dxg-group-menu", role: "menu",
                                            button {
                                                r#type: "button",
                                                class: "dxg-menu-item",
                                                "data-active": if group_by.read().is_none() { "true" },
                                                onclick: move |_| { group_by.set(None); show_group_menu.set(false); },
                                                span { class: "dxg-menu-item-label", "No grouping" }
                                            }
                                            for (gk , gl) in groupables.iter().cloned() {
                                                button {
                                                    r#type: "button",
                                                    class: "dxg-menu-item",
                                                    "data-active": if *group_by.read() == Some(gk) { "true" },
                                                    onclick: move |_| { group_by.set(Some(gk)); show_group_menu.set(false); },
                                                    span { class: "dxg-menu-item-label", "{gl}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else { rsx! {} }
                    }
                    if show_card_toggle {
                        div { class: "dxg-view-toggle", role: "group", "aria-label": "View mode",
                            button {
                                r#type: "button",
                                "aria-label": "List view",
                                title: "List view",
                                class: "dxg-view-button",
                                "data-active": if !is_card { "true" },
                                onclick: move |_| gs.write().set_view(ViewMode::List),
                                svg {
                                    width: "15", height: "15", view_box: "0 0 24 24",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    line { x1: "8", y1: "6", x2: "21", y2: "6" }
                                    line { x1: "8", y1: "12", x2: "21", y2: "12" }
                                    line { x1: "8", y1: "18", x2: "21", y2: "18" }
                                    line { x1: "3", y1: "6", x2: "3.01", y2: "6" }
                                    line { x1: "3", y1: "12", x2: "3.01", y2: "12" }
                                    line { x1: "3", y1: "18", x2: "3.01", y2: "18" }
                                }
                            }
                            button {
                                r#type: "button",
                                "aria-label": "Grid view",
                                title: "Grid view",
                                class: "dxg-view-button",
                                "data-active": if is_card { "true" },
                                onclick: move |_| gs.write().set_view(ViewMode::Card),
                                svg {
                                    width: "15", height: "15", view_box: "0 0 24 24",
                                    fill: "none", stroke: "currentColor", stroke_width: "2",
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    rect { x: "3", y: "3", width: "7", height: "7", rx: "1.5" }
                                    rect { x: "14", y: "3", width: "7", height: "7", rx: "1.5" }
                                    rect { x: "14", y: "14", width: "7", height: "7", rx: "1.5" }
                                    rect { x: "3", y: "14", width: "7", height: "7", rx: "1.5" }
                                }
                            }
                        }
                    }
                }
            }
            // ── Bulk-action bar (when rows are selected) ─────────────────────
            if sel_count > 0 {
                div { class: "dxg-bulk-bar", role: "toolbar", "aria-label": "Bulk actions",
                    span { class: "dxg-bulk-count", "{sel_count} selected" }
                    div { class: "dxg-bulk-divider" }
                    div { class: "dxg-bulk-actions",
                        if let Some(b) = g.bulk.clone() { {b} }
                        for act in bulk_actions.iter().cloned() {
                            {
                                let cb = g.callbacks.on_bulk_action.clone();
                                let ids = selected_ids.clone();
                                let key = act.key;
                                rsx! {
                                    button {
                                        r#type: "button",
                                        class: "dxg-bulk-action",
                                        "data-variant": if act.danger { "danger" } else { "default" },
                                        onclick: move |_| { if let Some(f) = &cb { f(key, ids.clone()); } },
                                        "{act.label}"
                                    }
                                }
                            }
                        }
                    }
                    div { class: "dxg-toolbar-spacer" }
                    button {
                        r#type: "button",
                        "aria-label": "Clear selection",
                        class: "dxg-bulk-clear",
                        onclick: move |_| gs.write().clear_selection(),
                        "×"
                    }
                }
            }
            // ── Active-filter chip bar ───────────────────────────────────────
            {
                let chips: Vec<(&'static str, String)> = g.columns.iter().filter_map(|c| {
                    filters.read().get(c.key).and_then(|(op, v)|
                        crate::data_grid::chip_label(c.label, c.filter_kind, op, v).map(|l| (c.key, l)))
                }).collect();
                if !chips.is_empty() && !is_card {
                    rsx! {
                        div { class: "dxg-chip-bar",
                            span { class: "dxg-chip-bar-label", "Filters:" }
                            for (key , label) in chips {
                                div { class: "dxg-chip",
                                    span { class: "dxg-chip-label", "{label}" }
                                    button {
                                        r#type: "button",
                                        "aria-label": "Remove filter",
                                        class: "dxg-chip-remove",
                                        onclick: move |_| { filters.write().remove(key); gs.write().set_page(0); },
                                        "×"
                                    }
                                }
                            }
                            button {
                                r#type: "button",
                                class: "dxg-chip-clear-all",
                                onclick: move |_| { filters.write().clear(); gs.write().set_page(0); },
                                "Clear all"
                            }
                        }
                    }
                } else {
                    rsx! {}
                }
            }

            if is_loading {
                div { class: "dxg-state-box dxg-loading",
                    span { role: "status", "aria-label": "Loading", "data-grid-spinner": "true" }
                    span { class: "dxg-state-text", "Loading…" }
                }
            } else if is_empty {
                div { class: "dxg-state-box dxg-empty", "{g.empty_label}" }
            } else if is_card {
                // ── Card / gallery view ──────────────────────────────────────
                div { class: "dxg-card-grid",
                    for row in g.rows.iter() {
                        {
                            let rid = row.id.clone();
                            let is_sel = gs.read().is_selected(&rid);
                            let on_click = row.on_click.clone();
                            let card = row.card.clone();
                            rsx! {
                                div {
                                    key: "{row.id}",
                                    class: "dxg-card",
                                    "data-selected": if is_sel { "true" },
                                    onclick: move |_| { if let Some(f) = &on_click { f(); } },
                                    div { class: "dxg-card-overlay",
                                        if selectable {
                                            input {
                                                r#type: "checkbox",
                                                class: "dxg-checkbox",
                                                checked: is_sel,
                                                onclick: move |e: MouseEvent| e.stop_propagation(),
                                                onchange: {
                                                    let rid = rid.clone();
                                                    move |_| gs.write().toggle_row(&rid)
                                                },
                                            }
                                        }
                                    }
                                    if let Some(c) = card {
                                        div { class: "dxg-card-slot", {c} }
                                    } else {
                                        // Auto card: first cell as title, rest as label/value.
                                        div { class: "dxg-card-auto",
                                            if let Some(first) = row.cells.first().cloned() {
                                                div { class: "dxg-card-title", {first} }
                                            }
                                            div { class: "dxg-card-fields",
                                                for (ci , col) in g.columns.iter().enumerate().skip(1) {
                                                    div { class: "dxg-card-field",
                                                        span { class: "dxg-card-field-label", "{col.label}" }
                                                        div { class: "dxg-card-field-value", {row.cells.get(ci).cloned()} }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                div { class: "dxg-scroll",
                    table { class: "dxg-table", role: "grid", "aria-rowcount": "{g.total}",
                        thead { "data-grid-thead": "true",
                            tr { class: "dxg-headrow", role: "row",
                                if selectable {
                                    th { class: "dxg-cell dxg-select-cell",
                                        input {
                                            r#type: "checkbox",
                                            class: "dxg-checkbox",
                                            checked: all_page_selected,
                                            onchange: {
                                                let ids = page_ids.clone();
                                                move |_| gs.write().toggle_page(&ids)
                                            },
                                        }
                                    }
                                }
                                for col in g.columns.iter() {
                                    {
                                        let key = col.key;
                                        let sorted = gs.read().sort_dir_for(key);
                                        let sorted_attr = match sorted {
                                            Some(true) => "asc",
                                            Some(false) => "desc",
                                            None => "none",
                                        };
                                        let aria = match sorted {
                                            Some(true) => "ascending",
                                            Some(false) => "descending",
                                            None => "none",
                                        };
                                        // Declared column width → min-width so `table-layout:auto`
                                        // sizes the column (else a media/empty column can balloon).
                                        let wstyle = col.width.map(|w| format!("min-width:{w};width:{w};")).unwrap_or_default();
                                        rsx! {
                                            th {
                                                class: "dxg-cell dxg-header-cell",
                                                style: "{wstyle}",
                                                role: "columnheader",
                                                "aria-sort": aria,
                                                "data-align": align_attr(col.align),
                                                "data-sorted": sorted_attr,
                                                "data-hide-mobile": if col.hide_on_mobile { "true" },
                                                if col.sortable {
                                                    button {
                                                        r#type: "button",
                                                        class: "dxg-sort-button",
                                                        "data-sorted": sorted_attr,
                                                        onclick: move |_| gs.write().toggle_sort(key),
                                                        span { class: "dxg-header-label", "{col.label}" }
                                                        match sorted {
                                                            Some(true) => rsx! { span { class: "dxg-sort-indicator", "data-dir": "asc", "▲" } },
                                                            Some(false) => rsx! { span { class: "dxg-sort-indicator", "data-dir": "desc", "▼" } },
                                                            None => rsx! { span { class: "dxg-sort-indicator", "data-dir": "none", "↕" } },
                                                        }
                                                    }
                                                } else {
                                                    span { class: "dxg-header-label", "{col.label}" }
                                                }
                                                if col.filterable {
                                                    {
                                                        let has_f = filters.read().get(key).map(|(o, v)| filter_active(o, v)).unwrap_or(false);
                                                        rsx! {
                                                            button {
                                                                r#type: "button",
                                                                title: "Filter this column",
                                                                "aria-label": "Filter {col.label}",
                                                                class: "dxg-funnel",
                                                                "data-active": if has_f { "true" },
                                                                onclick: move |e: MouseEvent| {
                                                                    e.stop_propagation();
                                                                    let c = e.client_coordinates();
                                                                    popover_xy.set((c.x, c.y));
                                                                    let open = *filter_popover.peek() == Some(key);
                                                                    filter_popover.set(if open { None } else { Some(key) });
                                                                },
                                                                "▾"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if has_actions {
                                    th { class: "dxg-cell dxg-action-head" }
                                }
                            }
                        }
                        tbody {
                            for (ri , row) in g.rows.iter().enumerate() {
                                {
                                    let rid = row.id.clone();
                                    let is_sel = gs.read().is_selected(&rid);
                                    let on_click = row.on_click.clone();
                                    // Group header: emit when this row's group value differs
                                    // from the previous row's (rows are pre-sorted by group).
                                    let group_header: Option<String> = row.group_value.as_ref().and_then(|gv| {
                                        let prev = if ri == 0 { None } else { g.rows[ri - 1].group_value.as_ref() };
                                        if prev != Some(gv) { Some(gv.clone()) } else { None }
                                    });
                                    let ncols = g.columns.len() + (g.selectable as usize) + (has_actions as usize);
                                    rsx! {
                                        if let Some(gh) = group_header {
                                            tr {
                                                key: "g-{row.id}",
                                                class: "dxg-group-header",
                                                td { class: "dxg-group-header-cell", colspan: "{ncols.max(1)}",
                                                    div { class: "dxg-group-header-inner",
                                                        span { class: "dxg-group-label", "{gh}" }
                                                    }
                                                }
                                            }
                                        }
                                        tr {
                                            class: "dxg-row",
                                            role: "row",
                                            "data-selected": if is_sel { "true" },
                                            "data-clickable": if on_click.is_some() { "true" },
                                            onclick: move |_| { if let Some(f) = &on_click { f(); } },
                                            if selectable {
                                                td { class: "dxg-cell dxg-select-cell",
                                                    input {
                                                        r#type: "checkbox",
                                                        class: "dxg-checkbox",
                                                        checked: is_sel,
                                                        onclick: move |e: MouseEvent| e.stop_propagation(),
                                                        onchange: {
                                                            let rid = rid.clone();
                                                            move |_| gs.write().toggle_row(&rid)
                                                        },
                                                    }
                                                }
                                            }
                                            if let Some(slot) = row.row_slot.clone() {
                                                td {
                                                    class: "dxg-cell dxg-row-slot",
                                                    colspan: "{g.columns.len().max(1)}",
                                                    {slot}
                                                }
                                            } else {
                                                for (ci , col) in g.columns.iter().enumerate() {
                                                    {
                                                        let ckey = col.key;
                                                        let editable = col.editable && row.on_edit.is_some();
                                                        let is_editing = editable && *editing.read() == Some((row.id.clone(), ckey));
                                                        let seed = row.edit_seed.get(ci).cloned().unwrap_or_default();
                                                        let on_edit = row.on_edit.clone();
                                                        let rid = row.id.clone();
                                                        let wstyle = col.width.map(|w| format!("min-width:{w};width:{w};")).unwrap_or_default();
                                                        rsx! {
                                                            td {
                                                                class: "dxg-cell dxg-body-cell",
                                                                style: "{wstyle}",
                                                                role: "gridcell",
                                                                "data-align": align_attr(col.align),
                                                                "data-hide-mobile": if col.hide_on_mobile { "true" },
                                                                "data-editable": if editable { "true" },
                                                                ondoubleclick: {
                                                                    let rid = rid.clone();
                                                                    let seed = seed.clone();
                                                                    move |e: MouseEvent| {
                                                                        if editable {
                                                                            e.stop_propagation();
                                                                            edit_draft.set(seed.clone());
                                                                            editing.set(Some((rid.clone(), ckey)));
                                                                        }
                                                                    }
                                                                },
                                                                if is_editing {
                                                                    {
                                                                        let on_edit = on_edit.clone();
                                                                        let mut commit = move || {
                                                                            if editing.peek().is_none() { return; }
                                                                            if let Some(f) = &on_edit { f(ckey, edit_draft.peek().clone()); }
                                                                            editing.set(None);
                                                                        };
                                                                        rsx! {
                                                                            input {
                                                                                r#type: "text",
                                                                                class: "dxg-edit-input",
                                                                                autofocus: true,
                                                                                initial_value: "{seed}",
                                                                                onclick: move |e: MouseEvent| e.stop_propagation(),
                                                                                oninput: move |e: FormEvent| edit_draft.set(e.value()),
                                                                                onkeydown: {
                                                                                    let mut commit = commit.clone();
                                                                                    move |e: KeyboardEvent| match e.key() {
                                                                                        Key::Enter => { e.prevent_default(); commit(); }
                                                                                        Key::Escape => { e.prevent_default(); editing.set(None); }
                                                                                        _ => {}
                                                                                    }
                                                                                },
                                                                                onblur: move |_| commit(),
                                                                            }
                                                                        }
                                                                    }
                                                                } else {
                                                                    {row.cells.get(ci).cloned()}
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            if has_actions {
                                                td { class: "dxg-cell dxg-action-cell",
                                                    div { class: "dxg-action-cell-inner",
                                                        if !row.actions.is_empty() {
                                                            {
                                                            // The menu is rendered INLINE here, absolutely positioned relative
                                                            // to this button's wrapper — no coordinate math, so it can never
                                                            // land off (client_coordinates / get_client_rect were both
                                                            // page-offset-relative here). CSS (.dxg-kebab-wrap) escapes the
                                                            // row/table clip.
                                                            let rid = row.id.clone();
                                                            let is_open = menu_for.read().as_deref() == Some(rid.as_str());
                                                            let acts = row.actions.clone();
                                                            let on_action = row.on_action.clone();
                                                            let first_danger = acts.iter().position(|a| a.danger);
                                                            rsx! {
                                                            div { class: "dxg-kebab-wrap",
                                                                button {
                                                                    r#type: "button",
                                                                    "aria-label": "More actions",
                                                                    "aria-haspopup": "menu",
                                                                    "aria-expanded": if is_open { "true" } else { "false" },
                                                                    class: "dxg-icon-button dxg-kebab",
                                                                    onclick: move |e: MouseEvent| {
                                                                        e.stop_propagation();
                                                                        let open = menu_for.peek().as_deref() == Some(rid.as_str());
                                                                        menu_for.set(if open { None } else { Some(rid.clone()) });
                                                                    },
                                                                    "⋮"
                                                                }
                                                                if is_open {
                                                                    div { class: "dxg-veil", onclick: move |_| menu_for.set(None) }
                                                                    div { class: "dxg-menu dxg-action-menu", role: "menu",
                                                                        for (i , act) in acts.iter().cloned().enumerate() {
                                                                            {
                                                                                let on_action = on_action.clone();
                                                                                let key = act.key;
                                                                                let show_sep = first_danger == Some(i) && i > 0;
                                                                                rsx! {
                                                                                    if show_sep { div { class: "dxg-menu-divider" } }
                                                                                    button {
                                                                                        r#type: "button",
                                                                                        class: "dxg-menu-item",
                                                                                        role: "menuitem",
                                                                                        "data-variant": if act.danger { "danger" } else { "default" },
                                                                                        disabled: act.disabled,
                                                                                        onclick: move |e: MouseEvent| {
                                                                                            e.stop_propagation();
                                                                                            menu_for.set(None);
                                                                                            if let Some(f) = &on_action { f(key); }
                                                                                        },
                                                                                        span { class: "dxg-menu-item-label", "{act.label}" }
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            } // end rsx!
                                                            } // end expr block
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // ── Footer aggregate (totals) row ──────────────────────
                        // MUST be a direct child of <table> (sibling of <tbody>).
                        // A <tfoot> nested inside <tbody> is invalid HTML — the
                        // browser detaches it into an anonymous content-sized box,
                        // so its cells stop tracking the column grid and the
                        // totals row misaligns from its columns.
                        if g.columns.iter().any(|c| c.aggregate.is_some()) {
                            tfoot {
                                tr { class: "dxg-totals-row",
                                    if selectable {
                                        td { class: "dxg-cell dxg-select-cell" }
                                    }
                                    for col in g.columns.iter() {
                                        {
                                            let cell = col.aggregate.and_then(|agg| {
                                                g.aggregates.iter().find(|(k, _)| k == col.key)
                                                    .map(|(_, v)| (crate::data_grid::agg_label(agg), crate::data_grid::fmt_agg(*v)))
                                            });
                                            let wstyle = col.width.map(|w| format!("min-width:{w};width:{w};")).unwrap_or_default();
                                            rsx! {
                                                td {
                                                    class: "dxg-cell dxg-totals-cell",
                                                    style: "{wstyle}",
                                                    "data-align": align_attr(col.align),
                                                    "data-hide-mobile": if col.hide_on_mobile { "true" },
                                                    if let Some((label, value)) = cell {
                                                        div { class: "dxg-total",
                                                            span { class: "dxg-total-label", "{label}" }
                                                            span { class: "dxg-total-value", "{value}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if has_actions {
                                        td { class: "dxg-cell dxg-action-cell" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Aggregate summary chips (card view) ──────────────────────────
            if is_card && g.columns.iter().any(|c| c.aggregate.is_some()) {
                div { class: "dxg-totals-chips",
                    span { class: "dxg-totals-chips-label", "Totals" }
                    for col in g.columns.iter() {
                        {
                            let cell = col.aggregate.and_then(|agg| {
                                g.aggregates.iter().find(|(k, _)| k == col.key)
                                    .map(|(_, v)| (crate::data_grid::agg_label(agg), crate::data_grid::fmt_agg(*v)))
                            });
                            rsx! {
                                if let Some((label, value)) = cell {
                                    div { class: "dxg-total-chip",
                                        span { class: "dxg-total-chip-label", "{col.label} · {label}" }
                                        span { class: "dxg-total-chip-value", "{value}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Pagination footer (both table + card views) ──────────────────
            if !is_loading && !is_empty && (g.page_count > 1 || g.total > gs.read().page_size()) {
                {
                    let cur = g.page;
                    let pages = g.page_count;
                    let total = g.total;
                    let psize = gs.read().page_size();
                    let from = if total == 0 { 0 } else { cur * psize + 1 };
                    let to = ((cur + 1) * psize).min(total);
                    rsx! {
                        div { class: "dxg-pagination",
                            span { class: "dxg-page-range", "Showing {from}–{to} of {total}" }
                            div { class: "dxg-toolbar-spacer" }
                            div { class: "dxg-pager",
                                button {
                                    r#type: "button",
                                    class: "dxg-page-button",
                                    "aria-label": "Previous page",
                                    disabled: cur == 0,
                                    onclick: move |_| gs.write().prev_page(),
                                    "‹"
                                }
                                span { class: "dxg-page-current", "{cur + 1} / {pages}" }
                                button {
                                    r#type: "button",
                                    class: "dxg-page-button",
                                    "aria-label": "Next page",
                                    disabled: cur + 1 >= pages,
                                    onclick: move |_| gs.write().next_page(pages),
                                    "›"
                                }
                            }
                        }
                    }
                }
            }

            // (Row kebab menus now render inline within each action cell — see
            //  the .dxg-kebab-wrap block in the table body above.)

            // ── Per-column filter popover ────────────────────────────────────
            if let Some(fkey) = *filter_popover.read() {
                if let Some(col) = g.columns.iter().find(|c| c.key == fkey).cloned() {
                    {
                    // Anchor under the clicked funnel (runtime click point → inline
                    // custom props the CSS positions from). Matches the mono grid.
                    let (mx, my) = *popover_xy.read();
                    let anchor = format!(
                        "--pop-top:calc({my}px + 14px); --pop-right:max(12px, calc(100vw - {mx}px - 8px)); --pop-maxh:calc(100vh - {my}px - 32px);",
                    );
                    rsx! {
                    div {
                        class: "dxg-sheet-overlay",
                        onclick: move |_| filter_popover.set(None),
                    }
                    div { class: "dxg-filter-popover", style: "{anchor}",
                        div { class: "dxg-popover-head",
                            span { class: "dxg-popover-title", "Filter · {col.label}" }
                            button {
                                r#type: "button",
                                "aria-label": "Close",
                                class: "dxg-icon-button",
                                onclick: move |_| filter_popover.set(None),
                                "×"
                            }
                        }
                        FilterBody { col: col.clone(), filters, grid: gs }
                        if filters.read().contains_key(fkey) {
                            button {
                                r#type: "button",
                                class: "dxg-filter-clear",
                                onclick: move |_| {
                                    filters.write().remove(fkey);
                                    gs.write().set_page(0);
                                },
                                "Clear this filter"
                            }
                        }
                    }
                    } // end rsx!
                    } // end anchor expr block
                }
            }
        }
    }
}

/// The type-correct control(s) for a column's filter. Non-generic — it works off
/// the erased column metadata (kind + pre-computed set values), writing into the
/// shared `filters` map the shell re-queries on.
#[component]
fn FilterBody(
    col: ErasedColumn,
    filters: Signal<HashMap<&'static str, (FilterOp, String)>>,
    grid: Signal<crate::grid_state::GridState>,
) -> Element {
    let mut filters = filters;
    let mut grid = grid;
    let key = col.key;
    let cur_val = filters.read().get(key).map(|(_, v)| v.clone()).unwrap_or_default();

    match col.filter_kind {
        // Multi-select set (checkbox list of distinct values).
        FilterKind::Set => {
            let chosen: std::collections::HashSet<String> =
                cur_val.split('\u{1}').filter(|s| !s.is_empty()).map(String::from).collect();
            rsx! {
                div { class: "dxg-filter-set",
                    if col.set_values.is_empty() {
                        span { class: "dxg-filter-empty", "No values" }
                    }
                    for v in col.set_values.iter().cloned() {
                        {
                            let on = chosen.contains(&v);
                            let vv = v.clone();
                            rsx! {
                                label { class: "dxg-filter-option",
                                    input {
                                        r#type: "checkbox",
                                        class: "dxg-checkbox",
                                        checked: on,
                                        onchange: move |_| {
                                            let mut set: std::collections::BTreeSet<String> = filters
                                                .read().get(key)
                                                .map(|(_, s)| s.split('\u{1}').filter(|x| !x.is_empty()).map(String::from).collect())
                                                .unwrap_or_default();
                                            if set.contains(&vv) { set.remove(&vv); } else { set.insert(vv.clone()); }
                                            if set.is_empty() { filters.write().remove(key); }
                                            else { filters.write().insert(key, (FilterOp::In, set.into_iter().collect::<Vec<_>>().join("\u{1}"))); }
                                            grid.write().set_page(0);
                                        },
                                    }
                                    span { class: "dxg-filter-option-label", "{v}" }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Text / Number: operator select + a value input.
        _ => {
            let ops: &[(FilterOp, &str)] = if col.numeric {
                &[
                    (FilterOp::Equals, "="),
                    (FilterOp::GreaterThan, ">"),
                    (FilterOp::LessThan, "<"),
                    (FilterOp::IsEmpty, "empty"),
                ]
            } else {
                &[
                    (FilterOp::Contains, "contains"),
                    (FilterOp::Equals, "="),
                    (FilterOp::StartsWith, "starts"),
                    (FilterOp::IsEmpty, "empty"),
                ]
            };
            let cur_op = filters.read().get(key).map(|(o, _)| o.clone()).unwrap_or_else(|| ops[0].0.clone());
            let valueless = matches!(cur_op, FilterOp::IsEmpty | FilterOp::IsNotEmpty);
            let op_now = cur_op.clone();
            rsx! {
                div { class: "dxg-filter-textnum",
                    select {
                        class: "dxg-filter-select",
                        title: "Filter operator",
                        value: "{cur_op:?}",
                        onchange: move |e: FormEvent| {
                            if let Some(op) = ops.iter().find(|(o, _)| format!("{o:?}") == e.value()).map(|(o, _)| o.clone()) {
                                let prev = filters.read().get(key).map(|(_, v)| v.clone()).unwrap_or_default();
                                if matches!(op, FilterOp::IsEmpty | FilterOp::IsNotEmpty) { filters.write().insert(key, (op, String::new())); }
                                else if prev.trim().is_empty() { filters.write().remove(key); }
                                else { filters.write().insert(key, (op, prev)); }
                                grid.write().set_page(0);
                            }
                        },
                        for (op , label) in ops.iter() {
                            option { value: "{op:?}", selected: format!("{op:?}") == format!("{cur_op:?}"), "{label}" }
                        }
                    }
                    if !valueless {
                        input {
                            r#type: if col.numeric { "number" } else { "text" },
                            class: "dxg-filter-input",
                            placeholder: "Value…",
                            value: "{cur_val}",
                            oninput: move |e: FormEvent| {
                                let v = e.value();
                                let op = filters.read().get(key).map(|(o, _)| o.clone()).unwrap_or_else(|| op_now.clone());
                                if v.trim().is_empty() { filters.write().remove(key); }
                                else { filters.write().insert(key, (op, v)); }
                                grid.write().set_page(0);
                            },
                        }
                    }
                }
            }
        }
    }
}
