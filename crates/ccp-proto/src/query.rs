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
                    if c2 == '"' {
                        chars.next();
                        break;
                    }
                    s.push(c2);
                    chars.next();
                }
                toks.push(Tok::StrLit(s));
            }
            c if c.is_ascii_digit() => {
                let mut s = String::new();
                s.push(c);
                chars.next();
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
                    if c2.is_alphanumeric() || c2 == '_' {
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
