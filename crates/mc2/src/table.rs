//! Bordered terminal tables for CLI output.
//!
//! A thin wrapper over [`comfy_table`] (the most-downloaded Rust table library)
//! exposing a small builder API used across the command handlers, so every
//! listing shares one style: rounded-corner Unicode borders, a header row, and
//! per-column alignment.

use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, CellAlignment, ContentArrangement, Table as ComfyTable};

/// A bordered table; rows are rendered in the order added.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    right_align: Vec<bool>,
}

impl Table {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the header row (omitted when rendering with `render_body`).
    pub fn header<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.headers = headers.into_iter().map(Into::into).collect();
        self
    }

    /// Append a data row.
    pub fn row<I, S>(mut self, cells: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
        self
    }

    /// Right-align the given 0-based columns (e.g. numeric counters).
    pub fn right_align<I>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = usize>,
    {
        for c in columns {
            if self.right_align.len() <= c {
                self.right_align.resize(c + 1, false);
            }
            self.right_align[c] = true;
        }
        self
    }

    /// Render with the header row.
    pub fn render(self) -> String {
        self.render_inner(true)
    }

    fn render_inner(&self, with_header: bool) -> String {
        let mut t = ComfyTable::new();
        t.load_style(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic);
        if with_header && !self.headers.is_empty() {
            let header: Vec<Cell> = self.headers.iter().map(|h| Cell::new(h.clone())).collect();
            t.set_header(header);
        }
        for row in &self.rows {
            let cells: Vec<Cell> = row
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut cell = Cell::new(c.clone());
                    if self.right_align.get(i).copied().unwrap_or(false) {
                        cell = cell.set_alignment(CellAlignment::Right);
                    }
                    cell
                })
                .collect();
            t.add_row(cells);
        }
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bordered_table_with_header() {
        let t = Table::new()
            .header(["NAME", "CPU", "PHASE"])
            .row(["demo-web-0", "1", "Running"])
            .row(["a", "2", "Creating"])
            .right_align([1]);
        let out = t.render();
        assert!(out.contains('┌'), "top border: {out}");
        assert!(out.contains('└'), "bottom border: {out}");
        assert!(out.contains("NAME"), "header present: {out}");
        assert!(out.contains("demo-web-0"), "row present: {out}");
        assert!(out.contains("Creating"), "row present: {out}");
    }
}
