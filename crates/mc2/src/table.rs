//! Plain-text, column-aligned table rendering for CLI output.
//!
//! kubectl/ps-style: no borders or themes — columns are separated by spaces and
//! sized to their widest cell, using `unicode-width` so wide glyphs align.
//! A single shared renderer instead of hand-formatted `{:<N}` strings scattered
//! across command handlers.

use unicode_width::UnicodeWidthStr;

/// A column-aligned table; rows are rendered in the order added.
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

    /// Render without a header row (e.g. key/value blocks).
    pub fn render_body(self) -> String {
        self.render_inner(false)
    }

    fn render_inner(&self, with_header: bool) -> String {
        let ncols = self
            .headers
            .len()
            .max(self.rows.iter().map(Vec::len).max().unwrap_or(0));
        let mut widths = vec![0usize; ncols];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = h.width();
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(cell.width());
                }
            }
        }

        let mut out = String::new();
        let push = |out: &mut String, row: &[String]| {
            let line: Vec<String> = (0..ncols)
                .map(|i| {
                    let cell = row.get(i).map(String::as_str).unwrap_or("");
                    let w = widths[i];
                    if self.right_align.get(i).copied().unwrap_or(false) {
                        format!("{cell:>w$}")
                    } else {
                        format!("{cell:<w$}")
                    }
                })
                .collect();
            // Trailing-column padding is noise; drop trailing spaces.
            out.push_str(line.join(" ").trim_end());
            out.push('\n');
        };
        if with_header && !self.headers.is_empty() {
            push(&mut out, &self.headers);
        }
        for row in &self.rows {
            push(&mut out, row);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns_and_headers() {
        let t = Table::new()
            .header(["NAME", "CPU", "PHASE"])
            .row(["demo-web-0", "1", "Running"])
            .row(["a", "2", "Creating"])
            .right_align([1]);
        let out = t.render();
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "NAME       CPU PHASE");
        assert_eq!(lines.next().unwrap(), "demo-web-0   1 Running");
        assert_eq!(lines.next().unwrap(), "a            2 Creating");
        assert!(lines.next().is_none());
    }

    #[test]
    fn body_omits_header_and_pads_unicode() {
        let t = Table::new()
            .row(["mc2 uses", "0 cpu"])
            .row(["host", "14 cpu"]);
        let out = t.render_body();
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "mc2 uses 0 cpu");
        assert_eq!(lines.next().unwrap(), "host     14 cpu");
    }
}
