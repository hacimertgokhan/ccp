//! A small RFC 4180 CSV reader (quotes, escaped quotes, CRLF, newlines in fields)
//! plus column naming and incremental type inference. No dependencies.
//!
//! Records are produced one at a time, so a big file is parsed twice (infer, then
//! load) instead of being held in memory as a table of strings.

use rusqlite::types::Value as SqlValue;

pub struct Records<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    delim: char,
    done: bool,
    error: Option<String>,
}

impl<'a> Records<'a> {
    pub fn new(text: &'a str, delim: u8) -> Records<'a> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        Records {
            chars: text.chars().peekable(),
            delim: delim as char,
            done: false,
            error: None,
        }
    }

    /// Set when the input ended inside a quoted field.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

impl Iterator for Records<'_> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        if self.done {
            return None;
        }
        loop {
            let mut row: Vec<String> = Vec::new();
            let mut field = String::new();
            let mut in_q = false;
            let mut any = false;
            loop {
                let Some(c) = self.chars.next() else {
                    self.done = true;
                    if in_q {
                        self.error = Some("unterminated quoted field".into());
                        return None;
                    }
                    if any && (!field.is_empty() || !row.is_empty()) {
                        row.push(field);
                        return Some(row);
                    }
                    return None;
                };
                any = true;
                if in_q {
                    if c == '"' {
                        if self.chars.peek() == Some(&'"') {
                            field.push('"');
                            self.chars.next();
                        } else {
                            in_q = false;
                        }
                    } else {
                        field.push(c);
                    }
                } else if c == '"' && field.is_empty() {
                    in_q = true;
                } else if c == self.delim {
                    row.push(std::mem::take(&mut field));
                } else if c == '\n' || c == '\r' {
                    if c == '\r' && self.chars.peek() == Some(&'\n') {
                        self.chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    break;
                } else {
                    field.push(c);
                }
            }
            if !(row.len() == 1 && row[0].is_empty()) {
                return Some(row); // skip blank lines
            }
        }
    }
}

/// CCP identifiers are `[A-Za-z0-9_]`; make header cells addressable and unique.
pub fn column_names(header: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, h) in header.iter().enumerate() {
        let mut n: String = h
            .trim()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        if n.is_empty() {
            n = format!("col{}", i + 1);
        }
        if n.chars().next().unwrap().is_ascii_digit() {
            n.insert(0, 'c');
        }
        let base = n.clone();
        let mut k = 2;
        while out.contains(&n) {
            n = format!("{}_{}", base, k);
            k += 1;
        }
        out.push(n);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColType {
    Int,
    Real,
    Text,
}

impl ColType {
    pub fn sql(self) -> &'static str {
        match self {
            ColType::Int => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
        }
    }

    /// Empty cells become NULL in numeric columns, so they don't poison sums.
    pub fn value(self, cell: &str) -> SqlValue {
        let t = cell.trim();
        match self {
            ColType::Int if t.is_empty() => SqlValue::Null,
            ColType::Int => t.parse().map(SqlValue::Integer).unwrap_or(SqlValue::Null),
            ColType::Real if t.is_empty() => SqlValue::Null,
            ColType::Real => t.parse().map(SqlValue::Real).unwrap_or(SqlValue::Null),
            ColType::Text => SqlValue::Text(cell.to_string()),
        }
    }
}

/// Infers each column's type one record at a time: integers, else floats, else text.
pub struct Inferer {
    types: Vec<ColType>,
    seen: Vec<bool>,
}

impl Inferer {
    pub fn new(cols: usize) -> Inferer {
        Inferer {
            types: vec![ColType::Int; cols],
            seen: vec![false; cols],
        }
    }

    pub fn push(&mut self, rec: &[String]) {
        for (i, t) in self.types.iter_mut().enumerate() {
            if *t == ColType::Text {
                continue;
            }
            let cell = rec.get(i).map(|s| s.trim()).unwrap_or("");
            if cell.is_empty() {
                continue;
            }
            self.seen[i] = true;
            if *t == ColType::Int && cell.parse::<i64>().is_err() {
                *t = ColType::Real;
            }
            if *t == ColType::Real && cell.parse::<f64>().is_err() {
                *t = ColType::Text;
            }
        }
    }

    pub fn finish(self) -> Vec<ColType> {
        self.types
            .into_iter()
            .zip(self.seen)
            .map(|(t, seen)| if seen { t } else { ColType::Text })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<Vec<String>> {
        Records::new(text, b',').collect()
    }

    #[test]
    fn parses_quotes_newlines_and_crlf() {
        let r = all("a,b\r\n1,\"x,\"\"y\"\"\nz\"\r\n\r\n2,\r\n");
        assert_eq!(
            r,
            vec![vec!["a", "b"], vec!["1", "x,\"y\"\nz"], vec!["2", ""]]
        );
    }

    #[test]
    fn detects_unterminated_quote_and_handles_bom() {
        let mut r = Records::new("a\n\"oops", b',');
        let rows: Vec<_> = r.by_ref().collect();
        assert_eq!(rows, vec![vec!["a"]]);
        assert!(r.error().is_some());
        assert_eq!(
            all("\u{feff}x,y\n1,2"),
            vec![vec!["x", "y"], vec!["1", "2"]]
        );
    }

    #[test]
    fn names_are_addressable_and_unique() {
        let n = column_names(&[
            "Total $".into(),
            "".into(),
            "total_".into(),
            "3d".into(),
            "Total $".into(),
        ]);
        assert_eq!(n, ["Total__", "col2", "total_", "c3d", "Total___2"]);
    }

    #[test]
    fn types_are_inferred_incrementally() {
        let mut inf = Inferer::new(4);
        for r in [vec!["1", "1.5", "x", ""], vec!["2", "2", "1", ""]] {
            inf.push(&r.into_iter().map(String::from).collect::<Vec<_>>());
        }
        assert_eq!(
            inf.finish(),
            [ColType::Int, ColType::Real, ColType::Text, ColType::Text]
        );
    }
}
