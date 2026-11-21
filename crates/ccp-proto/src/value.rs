#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// Appends `s` with the characters that would otherwise let a scalar's own
/// content break out of its quoted slot and desync a client's parser escaped.
fn push_escaped(out: &mut String, s: &str) {
    let mut start = 0;
    for (i, c) in s.char_indices() {
        let rep = match c {
            '\\' => "\\\\",
            '"' => "\\\"",
            '\n' => "\\n",
            '\r' => "\\r",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(rep);
        start = i + c.len_utf8();
    }
    out.push_str(&s[start..]);
}

impl Value {
    /// Writes the compact encoding into one growing buffer (no intermediate strings).
    pub fn write_compact(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => {
                let _ = write!(out, "{}", i);
            }
            Value::Float(f) if f.is_finite() => {
                let _ = write!(out, "{}", f);
            }
            Value::Float(_) => out.push_str("null"),
            Value::Str(s) => {
                out.push('"');
                push_escaped(out, s);
                out.push('"');
            }
            Value::List(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_compact(out);
                }
                out.push(']');
            }
            Value::Object(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(k);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_compact_string(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_compactly_and_escapes() {
        let v = Value::Object(vec![
            ("a".into(), Value::Int(-3)),
            ("b".into(), Value::Str("x\"y\\z\n".into())),
            (
                "c".into(),
                Value::List(vec![Value::Null, Value::Bool(true), Value::Float(1.5)]),
            ),
        ]);
        assert_eq!(
            v.to_compact_string(),
            r#"{a:-3,b:"x\"y\\z\n",c:[null,true,1.5]}"#
        );
    }
}
