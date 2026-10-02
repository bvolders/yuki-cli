use crate::error::YukiError;
use comfy_table::{Cell, Color, Table, presets::UTF8_FULL_CONDENSED};
use serde_json::{Map, Value, json};

/// Pagination and field-selection options shared by all list commands.
#[derive(Default)]
pub struct ListOptions<'a> {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub fields: Option<&'a str>,
}

/// Apply offset then limit to a row slice in place.
pub fn apply_pagination(rows: &mut Vec<Vec<String>>, opts: &ListOptions<'_>) {
    if let Some(off) = opts.offset {
        if off >= rows.len() {
            rows.clear();
            return;
        }
        rows.drain(..off);
    }
    if let Some(lim) = opts.limit {
        rows.truncate(lim);
    }
}

/// Restrict `headers` and `rows` to the comma-separated columns named in `opts.fields`.
///
/// Column names match case-insensitively and keep the order the caller asked for.
/// An unrecognised name is an error: emitting the full row set for `--fields bogus`
/// makes an unsupported query indistinguishable from a satisfied one.
pub fn select_fields(
    headers: &mut Vec<String>,
    rows: &mut [Vec<String>],
    opts: &ListOptions<'_>,
) -> Result<(), YukiError> {
    let Some(spec) = opts.fields else {
        return Ok(());
    };

    let mut indices = Vec::new();
    let mut selected = Vec::new();
    for requested in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let position = headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(requested))
            .ok_or_else(|| {
                YukiError::Config(format!(
                    "unknown field: {requested} (available: {})",
                    headers.join(", ")
                ))
            })?;
        indices.push(position);
        selected.push(headers[position].clone());
    }
    if indices.is_empty() {
        return Err(YukiError::Config(
            "--fields was given no column names".to_string(),
        ));
    }

    for row in rows.iter_mut() {
        *row = indices
            .iter()
            .map(|&i| row.get(i).cloned().unwrap_or_default())
            .collect();
    }
    *headers = selected;
    Ok(())
}

pub enum OutputFormat {
    Table,
    Json,
}

impl OutputFormat {
    pub fn from_flag(flag: Option<&str>, is_tty: bool) -> Self {
        match flag {
            Some("json") => Self::Json,
            // "text" and "table" both map to table output
            Some("text") | Some("table") => Self::Table,
            // Explicit "auto" or no flag: defer to TTY detection
            Some("auto") | None => {
                if is_tty {
                    Self::Table
                } else {
                    Self::Json
                }
            }
            // Any other unrecognized value: treat as auto
            Some(_) => {
                if is_tty {
                    Self::Table
                } else {
                    Self::Json
                }
            }
        }
    }
}

/// Format rows as a clispec v0.2 items envelope: `{"items": [...], "total": N}`.
pub fn format_json(headers: &[String], rows: &[Vec<String>]) -> String {
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut map = Map::new();
            for (i, header) in headers.iter().enumerate() {
                let val = row.get(i).cloned().unwrap_or_default();
                map.insert(header.clone(), Value::String(val));
            }
            Value::Object(map)
        })
        .collect();
    let total = items.len();
    serde_json::to_string_pretty(&json!({
        "items": items,
        "total": total
    }))
    .unwrap_or_else(|_| r#"{"items":[],"total":0}"#.into())
}

pub fn format_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    let header_cells: Vec<Cell> = headers
        .iter()
        .map(|h| {
            Cell::new(h)
                .fg(Color::White)
                .add_attribute(comfy_table::Attribute::Bold)
        })
        .collect();
    table.set_header(header_cells);
    for row in rows {
        table.add_row(row);
    }
    table.to_string()
}

/// Format a structured error as the clispec v0.2 envelope.
///
/// The spec requires the last line of stderr to be:
/// `{"error": {"kind": "<kind>", "message": "<message>"}}`
pub fn format_error_json(message: &str, kind: &str) -> String {
    serde_json::to_string(&json!({
        "error": {
            "kind": kind,
            "message": message
        }
    }))
    .unwrap_or_else(|_| format!(r#"{{"error":{{"kind":"{kind}","message":"{message}"}}}}"#))
}

/// `2048` → `2.0 KB`, for a human.
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

pub fn is_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}
