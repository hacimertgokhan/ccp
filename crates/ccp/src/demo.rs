use crate::schema;
use ccp_proto::engine::{Args, FieldResult, Resolvable, RootResolver, execute};
use ccp_proto::query::{ArgValue, Selection, parse_query};
use ccp_proto::serve::serve_stdio;
use ccp_proto::value::Value;
use std::rc::Rc;

#[derive(Clone)]
struct UserRow {
    id: i64,
    name: &'static str,
    email: &'static str,
}

#[derive(Clone)]
struct PostRow {
    id: i64,
    title: &'static str,
    body: &'static str,
    author_id: i64,
}

#[derive(Clone)]
struct CommentRow {
    id: i64,
    body: &'static str,
    author_name: &'static str,
    post_id: i64,
}

struct DbSnapshot {
    users: Vec<UserRow>,
    posts: Vec<PostRow>,
    comments: Vec<CommentRow>,
}

struct Db {
    snapshot: Rc<DbSnapshot>,
}

struct UserNode {
    row: UserRow,
    db: Rc<DbSnapshot>,
}

struct PostNode {
    row: PostRow,
    db: Rc<DbSnapshot>,
}

struct CommentNode {
    row: CommentRow,
}

impl Resolvable for UserNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "id" => FieldResult::Scalar(Value::Int(self.row.id)),
            "name" => FieldResult::Scalar(Value::Str(self.row.name.to_string())),
            "email" => FieldResult::Scalar(Value::Str(self.row.email.to_string())),
            "posts" => {
                let nodes: Vec<Box<dyn Resolvable>> = self
                    .db
                    .posts
                    .iter()
                    .filter(|p| p.author_id == self.row.id)
                    .map(|p| {
                        Box::new(PostNode {
                            row: p.clone(),
                            db: self.db.clone(),
                        }) as Box<dyn Resolvable>
                    })
                    .collect();
                FieldResult::List(nodes)
            }
            _ => FieldResult::Missing,
        }
    }
}

impl Resolvable for PostNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "id" => FieldResult::Scalar(Value::Int(self.row.id)),
            "title" => FieldResult::Scalar(Value::Str(self.row.title.to_string())),
            "body" => FieldResult::Scalar(Value::Str(self.row.body.to_string())),
            "author_id" => FieldResult::Scalar(Value::Int(self.row.author_id)),
            "author" => match self.db.users.iter().find(|u| u.id == self.row.author_id) {
                Some(u) => FieldResult::Node(Box::new(UserNode {
                    row: u.clone(),
                    db: self.db.clone(),
                })),
                None => FieldResult::Missing,
            },
            "comments" => {
                let nodes: Vec<Box<dyn Resolvable>> = self
                    .db
                    .comments
                    .iter()
                    .filter(|c| c.post_id == self.row.id)
                    .map(|c| Box::new(CommentNode { row: c.clone() }) as Box<dyn Resolvable>)
                    .collect();
                FieldResult::List(nodes)
            }
            _ => FieldResult::Missing,
        }
    }
}

impl Resolvable for CommentNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "id" => FieldResult::Scalar(Value::Int(self.row.id)),
            "body" => FieldResult::Scalar(Value::Str(self.row.body.to_string())),
            "author_name" => FieldResult::Scalar(Value::Str(self.row.author_name.to_string())),
            _ => FieldResult::Missing,
        }
    }
}

impl RootResolver for Db {
    fn resolve_root(&self, name: &str, args: &Args, _sub: &[Selection]) -> FieldResult {
        match name {
            "user" => {
                let id = match args.get("id") {
                    Some(ArgValue::Int(i)) => *i,
                    _ => return FieldResult::Missing,
                };
                match self.snapshot.users.iter().find(|u| u.id == id) {
                    Some(u) => FieldResult::Node(Box::new(UserNode {
                        row: u.clone(),
                        db: self.snapshot.clone(),
                    })),
                    None => FieldResult::Missing,
                }
            }
            "users" => {
                let nodes: Vec<Box<dyn Resolvable>> = self
                    .snapshot
                    .users
                    .iter()
                    .map(|u| {
                        Box::new(UserNode {
                            row: u.clone(),
                            db: self.snapshot.clone(),
                        }) as Box<dyn Resolvable>
                    })
                    .collect();
                FieldResult::List(nodes)
            }
            "post" => {
                let id = match args.get("id") {
                    Some(ArgValue::Int(i)) => *i,
                    _ => return FieldResult::Missing,
                };
                match self.snapshot.posts.iter().find(|p| p.id == id) {
                    Some(p) => FieldResult::Node(Box::new(PostNode {
                        row: p.clone(),
                        db: self.snapshot.clone(),
                    })),
                    None => FieldResult::Missing,
                }
            }
            _ => FieldResult::Missing,
        }
    }
}

fn demo_db() -> Db {
    let snapshot = DbSnapshot {
        users: vec![
            UserRow {
                id: 1,
                name: "Mert",
                email: "mert@example.com",
            },
            UserRow {
                id: 2,
                name: "Ayse",
                email: "ayse@example.com",
            },
        ],
        posts: vec![
            PostRow {
                id: 10,
                title: "What is CCP",
                body: "CCP, the Core Context Protocol, delivers the core context a model needs in a single request.",
                author_id: 1,
            },
            PostRow {
                id: 11,
                title: "One request, nested fields",
                body: "Related data comes back nested in one call, cutting round trips.",
                author_id: 1,
            },
            PostRow {
                id: 12,
                title: "Schema advertisement",
                body: "The schema is sent once and never repeated per call.",
                author_id: 2,
            },
        ],
        comments: vec![
            CommentRow {
                id: 100,
                body: "Nice summary.",
                author_name: "Ayse",
                post_id: 10,
            },
            CommentRow {
                id: 101,
                body: "The core-context idea fits well.",
                author_name: "Mert",
                post_id: 10,
            },
            CommentRow {
                id: 102,
                body: "Fewer round trips really makes a difference.",
                author_name: "Mert",
                post_id: 12,
            },
        ],
    };
    Db {
        snapshot: Rc::new(snapshot),
    }
}

pub fn serve() {
    let db = demo_db();
    serve_stdio(schema::SCHEMA, &db, &|| Ok("ccp demo".to_string()));
}

fn naive_multi_call_bytes() -> (String, String) {
    // Simulates what a flat MCP-style tool flow would send as three separate
    // round trips: get_user, get_posts(author_id), get_comments(post_id),
    // each returning full objects (no field selection = overfetch).
    let calls = [
        r#"{"tool":"get_user","args":{"id":1}}"#.to_string(),
        r#"{"id":1,"name":"Mert","email":"mert@example.com"}"#.to_string(),
        r#"{"tool":"get_posts","args":{"author_id":1}}"#.to_string(),
        r#"[{"id":10,"title":"What is CCP","body":"CCP, the Core Context Protocol, delivers the core context a model needs in a single request.","author_id":1},{"id":11,"title":"One request, nested fields","body":"Related data comes back nested in one call, cutting round trips.","author_id":1}]"#.to_string(),
        r#"{"tool":"get_comments","args":{"post_id":10}}"#.to_string(),
        r#"[{"id":100,"body":"Nice summary.","author_name":"Ayse","post_id":10},{"id":101,"body":"The core-context idea fits well.","author_name":"Mert","post_id":10}]"#.to_string(),
    ];
    (
        calls.join("\n"),
        "3 round trip, full objects (overfetch)".to_string(),
    )
}

pub fn run_demo() {
    let db = demo_db();

    let query_str = "user(id:1){name posts{title comments{author_name}}}";
    let selections = parse_query(query_str).expect("query parse failed");
    let result = execute(&db, &selections);
    let compact = result.to_compact_string();

    println!("=== ccp query ===");
    println!("{}", query_str);
    println!();
    println!("=== ccp result (1 round trip, only requested fields) ===");
    println!("{}", compact);
    println!("bytes: {}", query_str.len() + compact.len());

    println!();
    let (naive_dump, note) = naive_multi_call_bytes();
    println!("=== naive flat-tool equivalent ({}) ===", note);
    println!("{}", naive_dump);
    println!("bytes: {}", naive_dump.len());
}
