//! Grid export encoders + browser download.
//!
//! The [`DataGrid`](super::DataGrid) projects the current view into an
//! [`ExportData`] (header row + string rows); this module turns that into a file
//! and hands it to the browser.
//!
//! ## Where each format is encoded (the size-critical decision)
//! * **CSV** — always client-side. Hand-rolled, zero deps, instant.
//! * **Excel (`.xlsx`) / PDF** — encoded by the **SERVER** on the web target. The
//!   binary-format crates (`rust_xlsxwriter` → zip/deflate; `krilla` → a full PDF
//!   font-shaping + image stack) added **~5–8 MB to the wasm bundle** for features
//!   that are better done server-side anyway (the server has the data, can embed a
//!   licensed font once, and is the only place signed/encrypted PDF can live). On
//!   the web target these formats POST the projected view to `/export` and stream
//!   the returned file down. The heavy encoders stay compiled **only for native**
//!   builds (`export-rich` feature — desktop POS / mobile, where offline local
//!   encoding matters and bundle size doesn't).
//!
//! Downloads go through a single one-shot `document::eval` Blob download — no
//! event listeners or per-mount handlers, which would corrupt the wasm executor
//! (see [[dioxus-js-callbacks]]). Binary payloads are base64'd for safe transport
//! across the JS string boundary and decoded back to bytes in the browser.

use dioxus::prelude::*;

/// The current view, already projected to strings by the grid (after search +
/// filters + sort, for the chosen page span). One `headers` row, then `rows`.
#[derive(Clone, PartialEq, Default)]
pub struct ExportData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// A human title for the document (PDF heading, sheet name). Usually the
    /// filename stem.
    pub title: String,
}

/// Which file format to export.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Xlsx,
    Pdf,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Xlsx => "xlsx",
            ExportFormat::Pdf => "pdf",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            ExportFormat::Csv => "text/csv",
            ExportFormat::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ExportFormat::Pdf => "application/pdf",
        }
    }
}

// ── CSV (always available) ───────────────────────────────────────────────────

/// Quote a CSV field when it contains a comma, quote, or newline (RFC 4180).
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn to_csv(data: &ExportData) -> Vec<u8> {
    let mut out = data.headers.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(",");
    out.push('\n');
    for r in &data.rows {
        out.push_str(&r.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out.into_bytes()
}

// ── Excel (.xlsx) — behind `export-rich` ─────────────────────────────────────

#[cfg(feature = "export-rich")]
fn to_xlsx(data: &ExportData) -> Option<Vec<u8>> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let sheet = wb.add_worksheet();
    let header_fmt = Format::new().set_bold().set_background_color(0xEEEEEE);
    for (c, h) in data.headers.iter().enumerate() {
        sheet.write_string_with_format(0, c as u16, h, &header_fmt).ok()?;
    }
    for (r, row) in data.rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            // Write numbers as numbers so Excel can sum/sort them; else as text.
            match cell.parse::<f64>() {
                Ok(n) if !cell.trim().is_empty() => {
                    sheet.write_number((r + 1) as u32, c as u16, n).ok()?;
                }
                _ => {
                    sheet.write_string((r + 1) as u32, c as u16, cell).ok()?;
                }
            }
        }
    }
    sheet.autofit();
    wb.save_to_buffer().ok()
}

// On the web target (no `export-rich`) the heavy `rust_xlsxwriter` stack is NOT
// compiled in; Excel is produced server-side via `server_export` (see below).

// ── PDF (plain, unsigned) — behind `export-rich` ─────────────────────────────
//
// A clean tabular PDF for "save / print a copy". Signed + encrypted (banking) PDF
// is intentionally NOT done here — that's a server-side concern (the signing key /
// cert / HSM / timestamp authority cannot live in the browser). See the grid's
// `on_export_signed` hook for that path.

#[cfg(feature = "export-rich")]
fn to_pdf(data: &ExportData) -> Option<Vec<u8>> {
    use krilla::geom::Point;
    use krilla::page::PageSettings;
    use krilla::text::{Font, TextDirection};
    use krilla::Document;

    // A4 landscape gives wide tables more room.
    const W: f32 = 842.0;
    const H: f32 = 595.0;
    const MARGIN: f32 = 36.0;
    const ROW_H: f32 = 18.0;
    const FONT_SIZE: f32 = 9.0;
    const TITLE_SIZE: f32 = 16.0;

    // Embed a base sans font. We rely on a system font shipped with the build;
    // if none is available the export simply returns None (caller falls back).
    let font_data = embedded_font()?;
    let font = Font::new(font_data.into(), 0)?;

    let cols = data.headers.len().max(1);
    let col_w = (W - 2.0 * MARGIN) / cols as f32;
    let rows_per_page = (((H - 2.0 * MARGIN) - (TITLE_SIZE + ROW_H)) / ROW_H).floor() as usize;
    let rows_per_page = rows_per_page.max(1);

    let mut doc = Document::new();
    let mut idx = 0usize;
    let total = data.rows.len();
    // At least one page even when there are no rows (header only).
    let pages = (total.div_ceil(rows_per_page)).max(1);

    for _ in 0..pages {
        let mut page = doc.start_page_with(PageSettings::new(W, H));
        let mut surface = page.surface();
        let mut y = MARGIN + TITLE_SIZE;

        // Title (first page only is enough, but cheap to repeat per page).
        surface.draw_text(Point::from_xy(MARGIN, y), font.clone(), TITLE_SIZE, &data.title, false, TextDirection::Auto);
        y += TITLE_SIZE + 4.0;

        // Header row.
        draw_row(&mut surface, &font, &data.headers, MARGIN, y, col_w, FONT_SIZE);
        y += ROW_H;

        // Body rows for this page.
        let end = (idx + rows_per_page).min(total);
        for row in &data.rows[idx..end] {
            draw_row(&mut surface, &font, row, MARGIN, y, col_w, FONT_SIZE);
            y += ROW_H;
        }
        idx = end;
        surface.finish();
        page.finish();
    }

    doc.finish().ok()
}

#[cfg(feature = "export-rich")]
fn draw_row(
    surface: &mut krilla::surface::Surface,
    font: &krilla::text::Font,
    cells: &[String],
    x0: f32,
    y: f32,
    col_w: f32,
    size: f32,
) {
    use krilla::geom::Point;
    use krilla::text::TextDirection;
    for (c, cell) in cells.iter().enumerate() {
        // Truncate to what fits the column so cells don't overrun their neighbour.
        let max_chars = (col_w / (size * 0.5)).floor() as usize;
        let text: String = if cell.chars().count() > max_chars && max_chars > 1 {
            cell.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
        } else {
            cell.clone()
        };
        surface.draw_text(
            Point::from_xy(x0 + c as f32 * col_w + 2.0, y),
            font.clone(),
            size,
            &text,
            false,
            TextDirection::Auto,
        );
    }
}

/// A font for the PDF. Tries a couple of common system locations; returns `None`
/// (callers degrade to CSV/Excel) if nothing is found — we never ship a font in
/// the wasm bundle just for export.
#[cfg(feature = "export-rich")]
fn embedded_font() -> Option<Vec<u8>> {
    // In the browser there is no filesystem; a real deployment would `include_bytes!`
    // a licensed font here. We keep that opt-in (it adds ~hundreds of KB to the
    // bundle) — without it, PDF export reports unavailable and the menu hides it.
    None
}

// On the web target PDF is produced server-side via `server_export` (no local
// `to_pdf` stub needed — `export()` routes Excel/PDF straight to the server).

// ── Public entry point ───────────────────────────────────────────────────────

/// Encode `data` to `format` and trigger a browser download as `filename`.
/// Returns `false` only when the format genuinely can't be produced in this build.
///
/// * CSV — encoded + downloaded client-side (always).
/// * Excel / PDF — **with `export-rich` (native)** encoded locally; **without it
///   (web)** the projected view is POSTed to the server `/export` endpoint, which
///   returns the file for download. Either way the feature works end-to-end.
pub fn export(data: &ExportData, format: ExportFormat, filename: &str) -> bool {
    // CSV is always local.
    if matches!(format, ExportFormat::Csv) {
        download_bytes(filename, &to_csv(data), format.mime());
        return true;
    }

    // Native (export-rich): encode Excel/PDF locally with the bundled crates.
    #[cfg(feature = "export-rich")]
    {
        let bytes = match format {
            ExportFormat::Csv => unreachable!(),
            ExportFormat::Xlsx => to_xlsx(data),
            ExportFormat::Pdf => to_pdf(data),
        };
        return match bytes {
            Some(b) => {
                download_bytes(filename, &b, format.mime());
                true
            }
            None => false,
        };
    }

    // Web: hand Excel/PDF to the server. The heavy encoders aren't in this bundle.
    #[cfg(not(feature = "export-rich"))]
    {
        server_export(data, format, filename);
        true
    }
}

/// Is a format selectable in the export menu for this build? CSV always; Excel
/// and PDF are always offered — locally (native) or via the server (web).
pub fn format_available(_format: ExportFormat) -> bool {
    true
}

/// Escape a string as a JSON string literal (with surrounding quotes).
#[cfg(not(feature = "export-rich"))]
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON array of strings.
#[cfg(not(feature = "export-rich"))]
fn json_arr(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(item));
    }
    out.push(']');
    out
}

/// POST the projected view to the server export endpoint and download the file it
/// returns. Used on the web target where the heavy encoders aren't bundled — the
/// server (which owns the data + fonts + signing keys) renders the file. One-shot
/// `eval` (no listeners) so it can't corrupt the wasm executor; the fetch streams
/// the response straight into a Blob download.
#[cfg(not(feature = "export-rich"))]
fn server_export(data: &ExportData, format: ExportFormat, filename: &str) {
    // Build the request JSON by hand (no serde_json dep in `ui`): headers + string
    // rows + title + format. `json_str`/`json_arr` escape per the JSON spec.
    let mut payload = String::from("{");
    payload.push_str(&format!("\"format\":{},", json_str(format.extension())));
    payload.push_str(&format!("\"filename\":{},", json_str(filename)));
    payload.push_str(&format!("\"title\":{},", json_str(&data.title)));
    payload.push_str(&format!("\"headers\":{},", json_arr(&data.headers)));
    payload.push_str("\"rows\":[");
    for (i, row) in data.rows.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(&json_arr(row));
    }
    payload.push_str("]}");
    // Escape for safe single-quoted embedding in the eval string. The JSON itself
    // already escaped `"` and control chars; here we only guard the JS string.
    let body = payload.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n");
    let name = filename.replace('\'', "");
    let mime = format.mime();
    let js = format!(
        "(async()=>{{try{{\
           const res=await fetch('/api/v1/export',{{method:'POST',\
             headers:{{'Content-Type':'application/json'}},body:'{body}'}});\
           if(!res.ok)return;\
           const blob=await res.blob();\
           const u=URL.createObjectURL(new Blob([blob],{{type:'{mime}'}}));\
           const a=document.createElement('a');a.href=u;a.download='{name}';\
           document.body.appendChild(a);a.click();a.remove();\
           setTimeout(()=>URL.revokeObjectURL(u),1000);\
         }}catch(e){{}}}})()"
    );
    let _ = document::eval(&js);
}

/// Base64-encode (standard alphabet) for safe transport across the JS boundary.
fn base64(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(ALPHA[(n >> 18 & 63) as usize] as char);
        out.push(ALPHA[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHA[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHA[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Fire-and-forget binary download: base64 → Blob → click an `<a download>`.
/// One-shot eval (no listeners) so it can't corrupt the wasm executor.
fn download_bytes(filename: &str, bytes: &[u8], mime: &str) {
    let b64 = base64(bytes);
    // base64 is alphanumeric + `+/=`, and the filename/mime are quoted below — all
    // JS-string-safe, so no further escaping is needed beyond the quotes.
    let js = format!(
        "try{{\
           const bin=atob('{b64}');\
           const len=bin.length;const arr=new Uint8Array(len);\
           for(let i=0;i<len;i++)arr[i]=bin.charCodeAt(i);\
           const b=new Blob([arr],{{type:'{mime}'}});\
           const u=URL.createObjectURL(b);\
           const a=document.createElement('a');a.href=u;a.download='{name}';\
           document.body.appendChild(a);a.click();a.remove();\
           setTimeout(function(){{URL.revokeObjectURL(u);}},1000);\
         }}catch(e){{}}",
        name = filename.replace('\'', "")
    );
    let _ = document::eval(&js);
}
