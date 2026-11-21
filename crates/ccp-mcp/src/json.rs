//! Field projection over JSON: return only the fields the query names.
//! This is where most of the token savings for APIs and event streams come from.

use ccp_proto::query::Selection;
use ccp_proto::value::Value;
use serde_json::Value as J;

/// Convert a whole JSON value (no selection): objects and arrays recurse.
pub fn project_all(v: &J) -> Value {
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        J::String(s) => Value::Str(s.clone()),
        J::Array(a) => Value::List(a.iter().map(project_all).collect()),
        J::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), project_all(v))).collect()),
    }
}

/// Look up `name` in an object: exact key first, then a dotted path (`a.b.c`,
/// with numeric segments indexing arrays).
pub fn lookup<'a>(v: &'a J, name: &str) -> Option<&'a J> {
    if let Some(x) = v.get(name) {
        return Some(x);
    }
    if !name.contains('.') {
        return None;
    }
    let mut cur = v;
    for seg in name.split('.') {
        cur = match cur {
            J::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => cur.get(seg)?,
        };
    }
    Some(cur)
}

/// Look up one selection (`name` or dotted path, with its own sub-selection) in `v`.
pub fn project_field(v: &J, sel: &Selection) -> Value {
    match lookup(v, &sel.name) {
        Some(x) => project(x, &sel.sub),
        None => Value::Null,
    }
}

/// Project `v` through a selection set. Arrays map element-wise; with no
/// selection an object is returned whole.
pub fn project(v: &J, sub: &[Selection]) -> Value {
    match v {
        J::Array(a) => Value::List(a.iter().map(|x| project(x, sub)).collect()),
        J::Object(_) if !sub.is_empty() => Value::Object(
            sub.iter()
                .map(|s| {
                    let val = match lookup(v, &s.name) {
                        Some(x) => project(x, &s.sub),
                        None => Value::Null,
                    };
                    (s.name.clone(), val)
                })
                .collect(),
        ),
        other => project_all(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::query::parse_query;
    use serde_json::json;

    #[test]
    fn selects_nested_and_dotted_fields() {
        let doc = json!({"id": 1, "name": "x", "address": {"city": "Izmir", "zip": "35"}, "tags": [{"k": "a", "v": 1}, {"k": "b", "v": 2}], "content-type": "t"});
        let sel =
            parse_query("x{name address{city} address.zip tags{k} content-type missing}").unwrap();
        let out = project(&doc, &sel[0].sub);
        assert_eq!(
            out.to_compact_string(),
            r#"{name:"x",address:{city:"Izmir"},address.zip:"35",tags:[{k:"a"},{k:"b"}],content-type:"t",missing:null}"#
        );
    }
}
