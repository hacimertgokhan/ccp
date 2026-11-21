//! csv-ccp: CCP server for CSV / TSV files, built for reporting.
//!
//! Every file becomes a table (named after the file). The files are loaded once
//! into an in-memory SQLite database, so filtering, ordering, grouping and
//! aggregation (`sales(group:"region",order:"-sum_total"){region sum_total}`) run
//! inside the server and only the small answer reaches the model.
//!
//! Env: CCP_PATH  a file, or a directory of .csv/.tsv files
//!      CCP_DELIMITER  optional single character (default: `,` for .csv, tab for .tsv)
//!      CCP_MAX_MB  refuse to load more than this (default 256)

mod csv;

use ccp_sql::Backend;
use rusqlite::{Connection, params_from_iter};
use sqlite_ccp::Sqlite;
use std::path::{Path, PathBuf};

fn table_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("data");
    let mut n: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if n.is_empty() || n.chars().next().unwrap().is_ascii_digit() {
        n.insert(0, 't');
    }
    n
}

fn files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("csv" | "tsv" | "CSV" | "TSV")
                )
            })
            .collect();
        v.sort();
        if v.is_empty() {
            return Err(format!("no .csv/.tsv files in {}", path.display()));
        }
        Ok(v)
    } else if path.is_file() {
        Ok(vec![path.to_path_buf()])
    } else {
        Err(format!("{} does not exist", path.display()))
    }
}

fn load(
    conn: &Connection,
    path: &Path,
    delim_override: Option<u8>,
    budget: &mut u64,
) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > *budget {
        return Err(format!("{} exceeds the CCP_MAX_MB limit", path.display()));
    }
    *budget -= meta.len();
    let data = std::fs::read(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let text = String::from_utf8_lossy(&data);
    let delim = delim_override.unwrap_or(
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("tsv"))
        {
            b'\t'
        } else {
            b','
        },
    );
    let mut it = csv::Records::new(&text, delim);
    let header = it.next().ok_or("file has no header row")?;
    let cols = csv::column_names(&header);
    // pass 1: infer types without keeping the rows
    let mut inf = csv::Inferer::new(cols.len());
    for rec in &mut it {
        inf.push(&rec);
    }
    if let Some(e) = it.error() {
        return Err(format!("{}: {}", path.display(), e));
    }
    let types = inf.finish();
    let table = table_name(path);
    let ddl = cols
        .iter()
        .zip(&types)
        .map(|(c, t)| format!("\"{}\" {}", c, t.sql()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(&format!("CREATE TABLE \"{}\" ({})", table, ddl), [])
        .map_err(|e| e.to_string())?;
    let ph = vec!["?"; cols.len()].join(",");
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    {
        // pass 2: stream the records into SQLite
        let mut st = conn
            .prepare(&format!("INSERT INTO \"{}\" VALUES ({})", table, ph))
            .map_err(|e| e.to_string())?;
        let mut it = csv::Records::new(&text, delim);
        it.next(); // header
        for r in it {
            let vals =
                (0..cols.len()).map(|i| types[i].value(r.get(i).map(String::as_str).unwrap_or("")));
            st.execute(params_from_iter(vals))
                .map_err(|e| e.to_string())?;
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())
}

fn connect() -> Result<Box<dyn Backend>, String> {
    let path = ccp_sql::env_required("CCP_PATH")?;
    let delim = match std::env::var("CCP_DELIMITER") {
        Ok(d) if d == "\\t" || d == "tab" => Some(b'\t'),
        Ok(d) if d.len() == 1 => Some(d.as_bytes()[0]),
        Ok(_) => return Err("CCP_DELIMITER must be a single character (or `tab`)".into()),
        Err(_) => None,
    };
    let mut budget: u64 = std::env::var("CCP_MAX_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
        * 1024
        * 1024;
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    for f in files(Path::new(&path))? {
        load(&conn, &f, delim, &mut budget)?;
    }
    Ok(Box::new(Sqlite::from_connection(conn, "CSV")))
}

fn main() {
    ccp_sql::main("csv-ccp", "CSV", connect);
}
