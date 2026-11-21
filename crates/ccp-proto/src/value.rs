#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<Value>),
    Object(Vec<(String, Value)>),
}

// Escapes characters that would otherwise let a scalar's own content break
// out of its quoted slot and desync a client's parser.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

impl Value {
    pub fn to_compact_string(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Str(s) => format!("\"{}\"", escape(s)),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_compact_string()).collect();
                format!("[{}]", parts.join(","))
            }
            Value::Object(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}:{}", k, v.to_compact_string()))
                    .collect();
                format!("{{{}}}", parts.join(","))
            }
        }
    }
}
