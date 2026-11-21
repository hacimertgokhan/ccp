use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum ArgValue {
    Int(i64),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub name: String,
    pub args: HashMap<String, ArgValue>,
    pub sub: Vec<Selection>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    IntLit(i64),
    StrLit(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Comma,
}

fn tokenize(input: &str) -> Vec<Tok> {
    let mut chars = input.chars().peekable();
    let mut toks = Vec::new();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '{' => {
                chars.next();
                toks.push(Tok::LBrace);
            }
            '}' => {
                chars.next();
                toks.push(Tok::RBrace);
            }
            ':' => {
                chars.next();
                toks.push(Tok::Colon);
            }
            ',' => {
                chars.next();
                toks.push(Tok::Comma);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(&c2) = chars.peek() {
                    chars.next();
                    match c2 {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other), // \" and \\ (and any other char) stay literal
                            None => break,
                        },
                        other => s.push(other),
                    }
                }
                toks.push(Tok::StrLit(s));
            }
            c if c.is_ascii_digit() || c == '-' => {
                let mut s = String::new();
                s.push(c);
                chars.next();
                if c == '-' && !matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                    continue;
                }
                while let Some(&c2) = chars.peek() {
                    if c2.is_ascii_digit() {
                        s.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::IntLit(s.parse().unwrap_or(0)));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut s = String::new();
                s.push(c);
                chars.next();
                while let Some(&c2) = chars.peek() {
                    if c2.is_alphanumeric() || c2 == '_' || c2 == '.' || c2 == '-' {
                        s.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Ident(s));
            }
            _ => {
                chars.next();
            }
        }
    }
    toks
}

// Caps how deeply selections may nest. Without this, a single query like
// `a{a{a{...}}}` recurses once per nesting level and crashes the process
// with a stack overflow instead of returning a normal error.
const MAX_SELECTION_DEPTH: usize = 64;

pub fn parse_query(input: &str) -> Result<Vec<Selection>, String> {
    let tokens = tokenize(input);
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < tokens.len() {
        let (sel, next) = parse_selection(&tokens, pos, 0)?;
        out.push(sel);
        pos = next;
    }
    Ok(out)
}

fn parse_selection(
    toks: &[Tok],
    mut pos: usize,
    depth: usize,
) -> Result<(Selection, usize), String> {
    if depth > MAX_SELECTION_DEPTH {
        return Err(format!(
            "selection nesting exceeds max depth ({})",
            MAX_SELECTION_DEPTH
        ));
    }
    let name = match toks.get(pos) {
        Some(Tok::Ident(s)) => s.clone(),
        other => return Err(format!("expected field name, got {:?}", other)),
    };
    pos += 1;

    let mut args = HashMap::new();
    if matches!(toks.get(pos), Some(Tok::LParen)) {
        pos += 1;
        loop {
            match toks.get(pos) {
                Some(Tok::RParen) => {
                    pos += 1;
                    break;
                }
                Some(Tok::Comma) => {
                    pos += 1;
                }
                Some(Tok::Ident(argname)) => {
                    let argname = argname.clone();
                    pos += 1;
                    if !matches!(toks.get(pos), Some(Tok::Colon)) {
                        return Err("expected ':' in args".to_string());
                    }
                    pos += 1;
                    let val = match toks.get(pos) {
                        Some(Tok::IntLit(i)) => ArgValue::Int(*i),
                        Some(Tok::StrLit(s)) => ArgValue::Str(s.clone()),
                        Some(Tok::Ident(s)) if s == "true" => ArgValue::Bool(true),
                        Some(Tok::Ident(s)) if s == "false" => ArgValue::Bool(false),
                        other => return Err(format!("expected arg value, got {:?}", other)),
                    };
                    pos += 1;
                    args.insert(argname, val);
                }
                other => return Err(format!("unexpected token in args: {:?}", other)),
            }
        }
    }

    let mut sub = Vec::new();
    if matches!(toks.get(pos), Some(Tok::LBrace)) {
        pos += 1;
        loop {
            match toks.get(pos) {
                Some(Tok::RBrace) => {
                    pos += 1;
                    break;
                }
                Some(Tok::Ident(_)) => {
                    let (child, next) = parse_selection(toks, pos, depth + 1)?;
                    sub.push(child);
                    pos = next;
                }
                other => return Err(format!("unexpected token in selection set: {:?}", other)),
            }
        }
    }

    Ok((Selection { name, args, sub }, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_ints_dotted_and_dashed_names() {
        let q = parse_query("list(key:\"k\",start:0,stop:-1){a.b content-type}").unwrap();
        assert!(matches!(q[0].args["stop"], ArgValue::Int(-1)));
        let names: Vec<_> = q[0].sub.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a.b", "content-type"]);
    }

    #[test]
    fn string_escapes() {
        let q = parse_query(r#"a(x:"he said \"hi\" \\ \n")"#).unwrap();
        match &q[0].args["x"] {
            ArgValue::Str(s) => assert_eq!(s, "he said \"hi\" \\ \n"),
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn depth_is_capped() {
        let deep = format!("{}x{}", "a{".repeat(100), "}".repeat(100));
        assert!(parse_query(&deep).is_err());
    }
}
