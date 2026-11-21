use crate::query::{ArgValue, Selection};
use crate::value::Value;
use std::collections::HashMap;

pub type Args = HashMap<String, ArgValue>;

pub enum FieldResult {
    Scalar(Value),
    Node(Box<dyn Resolvable>),
    List(Vec<Box<dyn Resolvable>>),
    Missing,
    /// A resolver-side failure (e.g. a database error); rendered as `{error:"..."}`.
    Error(String),
}

pub trait Resolvable {
    fn resolve_field(&self, field: &str, args: &Args) -> FieldResult;
}

pub trait RootResolver {
    /// `sub` is the requested sub-selection, so a data source can fetch only
    /// the fields that were actually asked for.
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult;
}

pub fn execute(root: &dyn RootResolver, selections: &[Selection]) -> Value {
    let mut out = Vec::new();
    for sel in selections {
        let result = root.resolve_root(&sel.name, &sel.args, &sel.sub);
        let value = materialize(result, sel);
        out.push((sel.name.clone(), value));
    }
    Value::Object(out)
}

fn materialize(result: FieldResult, sel: &Selection) -> Value {
    match result {
        FieldResult::Scalar(v) => v,
        FieldResult::Missing => Value::Null,
        FieldResult::Error(e) => Value::Object(vec![("error".to_string(), Value::Str(e))]),
        FieldResult::Node(node) => resolve_node(node.as_ref(), &sel.sub),
        FieldResult::List(nodes) => Value::List(
            nodes
                .iter()
                .map(|n| resolve_node(n.as_ref(), &sel.sub))
                .collect(),
        ),
    }
}

fn resolve_node(node: &dyn Resolvable, sub: &[Selection]) -> Value {
    let mut fields = Vec::new();
    for s in sub {
        let fr = node.resolve_field(&s.name, &s.args);
        let v = materialize(fr, s);
        fields.push((s.name.clone(), v));
    }
    Value::Object(fields)
}
