//! `DateRangePicker` — a generic, dependency-free calendar range picker.
//!
//! One popover (a bottom-sheet on mobile) with quick presets, a month grid that
//! highlights the selected span, month navigation, and Today / Clear / Apply. The
//! value is an ISO `YYYY-MM-DD` `(from, to)` pair — both optional — so it drops
//! straight into the grid's `DateRange` filter, but it's a standalone component
//! usable anywhere (report ranges, dashboards, scheduling).
//!
//! No `chrono`/`time` dependency: the calendar only needs days-in-month, the
//! weekday of the 1st, and month stepping — a few lines of civil-date math (same
//! lean approach as the dependency-free charts). All dates are proleptic Gregorian.

use dioxus::prelude::*;

// ── tiny civil-date helpers (no external crate) ──────────────────────────────

/// A calendar date. Comparable and cheap to copy.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32, // 1..=12
    pub day: u32,   // 1..=31
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// Parse ISO `YYYY-MM-DD`. Lenient: returns `None` on anything malformed.
    pub fn parse(s: &str) -> Option<Date> {
        let s = s.trim();
        let mut it = s.splitn(3, '-');
        let y = it.next()?.parse::<i32>().ok()?;
        let m = it.next()?.parse::<u32>().ok()?;
        let d = it.next()?.parse::<u32>().ok()?;
        if (1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d) {
            Some(Date::new(y, m, d))
        } else {
            None
        }
    }

    /// Format as ISO `YYYY-MM-DD` (the wire format the grid filters on).
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Day count since 1970-01-01 (proleptic Gregorian). Used for arithmetic like
    /// "today minus 7 days" without a date library. Howard Hinnant's algorithm.
    fn days_from_epoch(self) -> i64 {
        let y = if self.month <= 2 { self.year - 1 } else { self.year } as i64;
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400; // [0, 399]
        let m = self.month as i64;
        let d = self.day as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146097 + doe - 719468
    }

    fn from_epoch(days: i64) -> Date {
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // [0, 11]
        let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
        let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
        Date::new((if m <= 2 { y + 1 } else { y }) as i32, m as u32, d as u32)
    }

    /// This date shifted by `n` days (negative = earlier).
    pub fn add_days(self, n: i64) -> Date {
        Date::from_epoch(self.days_from_epoch() + n)
    }

    /// Weekday, 0 = Sunday … 6 = Saturday (matches the calendar's S-M-T-W-T-F-S).
    fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday (=4 in Sun=0 indexing).
        (((self.days_from_epoch() % 7 + 4) % 7 + 7) % 7) as u32
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Step a (year, month) one month in `dir` (+1 / -1), wrapping the year.
fn step_month(year: i32, month: u32, dir: i32) -> (i32, u32) {
    let m0 = month as i32 - 1 + dir;
    let year = year + m0.div_euclid(12);
    let month = m0.rem_euclid(12) as u32 + 1;
    (year, month)
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Short month names for the month-picker grid.
const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Which picker the calendar body is showing.
#[derive(Clone, Copy, PartialEq)]
enum CalView {
    Days,
    Months,
    Years,
}

// ── component ────────────────────────────────────────────────────────────────

#[derive(Props, Clone, PartialEq)]
pub struct DateRangePickerProps {
    /// Current range as ISO `YYYY-MM-DD` strings; either side may be empty.
    #[props(default)]
    pub from: String,
    #[props(default)]
    pub to: String,
    /// Today's date as ISO `YYYY-MM-DD`, for presets + the "today" marker. The host
    /// supplies it (the component is pure — no clock access). Defaults sensibly.
    #[props(default)]
    pub today: Option<String>,
    /// Fired on Apply / preset / Clear with the new `(from, to)` ISO pair.
    pub onchange: EventHandler<(String, String)>,
    /// Hide the preset chips (This Week, Last 7 Days, …) for a bare calendar.
    #[props(default = false)]
    pub hide_presets: bool,
}

/// The picker body — render it inside your own popover/sheet, or use it inline.
#[component]
pub fn DateRangePicker(props: DateRangePickerProps) -> Element {
    let today = props.today.as_deref().and_then(Date::parse).unwrap_or_else(|| Date::new(2026, 1, 1));

    // Selected endpoints (None until picked). Seeded from props once.
    let mut sel_from = use_signal(|| Date::parse(&props.from));
    let mut sel_to = use_signal(|| Date::parse(&props.to));
    // The month the grid is showing — defaults to the start of the range, else today.
    let init = sel_from.peek().or(*sel_to.peek()).unwrap_or(today);
    let mut view_y = use_signal(|| init.year);
    let mut view_m = use_signal(|| init.month);
    // Which picker the body shows: the day grid, a month grid, or a year grid.
    // Clicking the month/year title in the header switches into these.
    let mut view_mode = use_signal(|| CalView::Days);
    // The first year shown in the year grid (a rolling 12-year window).
    let mut year_page = use_signal(|| init.year - (init.year.rem_euclid(12)));

    let commit = {
        let onchange = props.onchange;
        move || {
            let f = (*sel_from.peek()).map(|d| d.iso()).unwrap_or_default();
            let t = (*sel_to.peek()).map(|d| d.iso()).unwrap_or_default();
            onchange.call((f, t));
        }
    };

    // Click a day: first click sets `from` and clears `to`; second sets `to`
    // (swapping if the user picked an earlier day). A third click restarts.
    let mut pick_day = move |d: Date| {
        let (f, t) = (*sel_from.peek(), *sel_to.peek());
        match (f, t) {
            (None, _) | (Some(_), Some(_)) => {
                sel_from.set(Some(d));
                sel_to.set(None);
            }
            (Some(start), None) => {
                if d < start {
                    sel_from.set(Some(d));
                    sel_to.set(Some(start));
                } else {
                    sel_to.set(Some(d));
                }
            }
        }
    };

    // Presets compute their span from `today` and apply immediately.
    let mut apply_preset = {
        move |from: Date, to: Date| {
            sel_from.set(Some(from));
            sel_to.set(Some(to));
            view_y.set(from.year);
            view_m.set(from.month);
            commit();
        }
    };

    let (vy, vm) = (*view_y.read(), *view_m.read());
    let first = Date::new(vy, vm, 1);
    let lead = first.weekday() as usize; // blank cells before day 1
    let dim = days_in_month(vy, vm) as usize;

    // Range bounds for highlighting (handle the in-progress single-endpoint case).
    let (lo, hi) = {
        let f = *sel_from.read();
        let t = *sel_to.read();
        match (f, t) {
            (Some(a), Some(b)) => (Some(a.min(b)), Some(a.max(b))),
            (Some(a), None) => (Some(a), Some(a)),
            (None, Some(b)) => (Some(b), Some(b)),
            (None, None) => (None, None),
        }
    };

    rsx! {
        div { class: "dxg-cal",
            // ── Presets ──────────────────────────────────────────────────────
            if !props.hide_presets {
                div { class: "dxg-cal-presets",
                    {
                        // (label, from, to) computed off today.
                        let monday = today.add_days(-((today.weekday() as i64 + 6) % 7)); // week starts Mon
                        let presets: [(&str, Date, Date); 5] = [
                            ("This week", monday, today),
                            ("7 days", today.add_days(-6), today),
                            ("This month", Date::new(today.year, today.month, 1), today),
                            ("30 days", today.add_days(-29), today),
                            (
                                "Last month",
                                {
                                    let (py, pm) = step_month(today.year, today.month, -1);
                                    Date::new(py, pm, 1)
                                },
                                {
                                    let (py, pm) = step_month(today.year, today.month, -1);
                                    Date::new(py, pm, days_in_month(py, pm))
                                },
                            ),
                        ];
                        rsx! {
                            for (label , pf , pt) in presets {
                                button {
                                    r#type: "button",
                                    class: "dxg-cal-preset",
                                    onclick: move |_| apply_preset(pf, pt),
                                    "{label}"
                                }
                            }
                        }
                    }
                }
            }

            // ── Header + nav ─────────────────────────────────────────────────
            // The title is clickable: month → month picker, year → year picker.
            // The arrows step by the unit the current view shows (month / year /
            // 12-year page), so navigation stays meaningful in every mode.
            {
                let mode = *view_mode.read();
                let yp = *year_page.read();
                let (prev_label, next_label) = match mode {
                    CalView::Days => ("Previous month", "Next month"),
                    CalView::Months => ("Previous year", "Next year"),
                    CalView::Years => ("Previous years", "Next years"),
                };
                rsx! {
                    div { class: "dxg-cal-head",
                        button {
                            r#type: "button",
                            "aria-label": "{prev_label}",
                            class: "dxg-cal-nav",
                            onclick: move |_| match *view_mode.peek() {
                                CalView::Days => {
                                    let (y, m) = step_month(*view_y.peek(), *view_m.peek(), -1);
                                    view_y.set(y);
                                    view_m.set(m);
                                }
                                CalView::Months => {
                                    let y = *view_y.peek() - 1;
                                    view_y.set(y);
                                }
                                CalView::Years => {
                                    let p = *year_page.peek() - 12;
                                    year_page.set(p);
                                }
                            },
                            svg {
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2.4",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                polyline { points: "15 18 9 12 15 6" }
                            }
                        }
                        // Clickable title — month + year each open their own picker.
                        div { class: "dxg-cal-title-row",
                            match mode {
                                CalView::Days => rsx! {
                                    button {
                                        r#type: "button",
                                        class: "dxg-cal-title dxg-cal-title-btn",
                                        onclick: move |_| view_mode.set(CalView::Months),
                                        "{MONTHS[(vm - 1) as usize]}"
                                    }
                                    button {
                                        r#type: "button",
                                        class: "dxg-cal-title-year dxg-cal-title-btn",
                                        onclick: move |_| {
                                            year_page.set(vy - vy.rem_euclid(12));
                                            view_mode.set(CalView::Years);
                                        },
                                        "{vy}"
                                    }
                                },
                                CalView::Months => rsx! {
                                    button {
                                        r#type: "button",
                                        class: "dxg-cal-title dxg-cal-title-btn",
                                        onclick: move |_| {
                                            year_page.set(vy - vy.rem_euclid(12));
                                            view_mode.set(CalView::Years);
                                        },
                                        "{vy}"
                                    }
                                },
                                CalView::Years => rsx! {
                                    span { class: "dxg-cal-title", "{yp}\u{2013}{yp + 11}" }
                                },
                            }
                        }
                        button {
                            r#type: "button",
                            "aria-label": "{next_label}",
                            class: "dxg-cal-nav",
                            onclick: move |_| match *view_mode.peek() {
                                CalView::Days => {
                                    let (y, m) = step_month(*view_y.peek(), *view_m.peek(), 1);
                                    view_y.set(y);
                                    view_m.set(m);
                                }
                                CalView::Months => {
                                    let y = *view_y.peek() + 1;
                                    view_y.set(y);
                                }
                                CalView::Years => {
                                    let p = *year_page.peek() + 12;
                                    year_page.set(p);
                                }
                            },
                            svg {
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2.4",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                polyline { points: "9 18 15 12 9 6" }
                            }
                        }
                    }
                }
            }

            // ── Body: day grid · month picker · year picker ──────────────────
            match *view_mode.read() {
                CalView::Days => rsx! {
                    // Weekday header (7-col grid via the .dxg-cal-grid component class).
                    div { class: "dxg-cal-grid",
                        for wd in ["S", "M", "T", "W", "T", "F", "S"] {
                            div { class: "dxg-cal-weekday", "{wd}" }
                        }
                    }
                    div { class: "dxg-cal-grid",
                        // Leading blanks for the weekday offset of the 1st.
                        for _ in 0..lead {
                            div { class: "dxg-cal-cell" }
                        }
                        for day in 1..=dim {
                            {
                                let d = Date::new(vy, vm, day as u32);
                                let is_today = d == today;
                                let is_endpoint = *sel_from.read() == Some(d) || *sel_to.read() == Some(d);
                                let in_range = match (lo, hi) {
                                    (Some(a), Some(b)) => d >= a && d <= b,
                                    _ => false,
                                };
                                // Continuous band for a real (2-endpoint) range, rounded
                                // at the true ends; a single day shows just the endpoint.
                                let real_range = matches!((lo, hi), (Some(a), Some(b)) if a != b);
                                let mut cell_cls = String::from("dxg-cal-cell");
                                if real_range && in_range {
                                    cell_cls.push_str(" is-band");
                                    if Some(d) == lo {
                                        cell_cls.push_str(" is-band-start");
                                    }
                                    if Some(d) == hi {
                                        cell_cls.push_str(" is-band-end");
                                    }
                                }
                                let mut day_cls = String::from("dxg-cal-day");
                                if is_endpoint {
                                    day_cls.push_str(" is-endpoint");
                                } else if is_today { // Month picker: pick any month of the shown year, then drop to days.
                                    day_cls.push_str(" is-today");
                                }
                                rsx! {
                                    div { class: "{cell_cls}",
                                        button { r#type: "button", class: "{day_cls}", onclick: move |_| pick_day(d), "{day}" }
                                    }
                                }
                            }
                        }
                    }
                },
                CalView::Months => rsx! {
                    div { class: "dxg-cal-monthgrid",
                        for mi in 0..12u32 {
                            {
                                let m1 = mi + 1;
                                let is_cur = m1 == vm;
                                let is_this = today.year == vy && today.month == m1;
                                let mut cls = String::from("dxg-cal-mybtn");
                                if is_cur {
                                    cls.push_str(" is-endpoint");
                                } else if is_this {
                                    cls.push_str(" is-today");
                                }
                                rsx! {
                                    button {
                                        r#type: "button",
                                        class: "{cls}",
                                        onclick: move |_| {
                                            view_m.set(m1);
                                            view_mode.set(CalView::Days);
                                        },
                                        "{MONTHS_SHORT[mi as usize]}"
                                    }
                                }
                            }
                        }
                    }
                },
                CalView::Years => rsx! {
                    div { class: "dxg-cal-monthgrid",
                        for yi in 0..12i32 {
                            {
                                let yy = *year_page.read() + yi;
                                let is_cur = yy == vy;
                                let is_this = yy == today.year;
                                let mut cls = String::from("dxg-cal-mybtn");
                                if is_cur {
                                    cls.push_str(" is-endpoint");
                                } else if is_this {
                                    cls.push_str(" is-today");
                                }
                                rsx! {
                                    button {
                                        r#type: "button",
                                        class: "{cls}",
                                        onclick: move |_| {
                                            view_y.set(yy);
                                            view_mode.set(CalView::Months);
                                        },
                                        "{yy}"
                                    }
                                }
                            }
                        }
                    }
                },
            }

            // ── Footer: Today · Clear · Apply ────────────────────────────────
            div { class: "dxg-cal-foot",
                button {
                    r#type: "button",
                    class: "dxg-cal-today-btn",
                    onclick: move |_| {
                        view_y.set(today.year);
                        view_m.set(today.month);
                    },
                    "TODAY"
                }
                div { class: "dxg-cal-foot-actions",
                    button {
                        r#type: "button",
                        class: "dxg-cal-foot-btn dxg-cal-foot-clear",
                        onclick: {
                            move |_| {
                                sel_from.set(None);
                                sel_to.set(None);
                                commit();
                            }
                        },
                        "Clear"
                    }
                    button {
                        r#type: "button",
                        class: "dxg-cal-foot-btn dxg-cal-foot-apply",
                        onclick: move |_| commit(),
                        "Apply"
                    }
                }
            }
        }
    }
}
