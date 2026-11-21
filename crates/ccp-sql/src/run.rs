use crate::exec::{Backend, Executor};
use ccp_proto::serve::serve_stdio;

/// Entry point shared by every database server binary.
///
/// * no flags: MCP over stdio (what Claude launches)
/// * `--ccp`: raw CCP line protocol (`SCHEMA`, `PING`, query lines)
pub fn main(
    name: &'static str,
    kind: &'static str,
    connect: fn() -> Result<Box<dyn Backend>, String>,
) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", name, env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "{name} {ver}\nRead-only {kind} server for CCP.\n\nUSAGE: {name} [--ccp]\n\n  (default)  MCP over stdio, for Claude and other MCP clients\n  --ccp      raw CCP line protocol over stdio\n\nENV: see https://github.com/hacimertgokhan/ccp#configuration",
            ver = env!("CARGO_PKG_VERSION")
        );
        return;
    }
    let make = || Executor::new(connect()?);
    if !args.iter().any(|a| a == "--ccp") {
        return crate::mcp::run(name, kind, &make);
    }
    match make() {
        Ok(ex) => {
            let info = ex.info();
            serve_stdio(&ex.catalog.schema_text(), &ex, &|| Ok(info.clone()));
        }
        Err(e) => {
            eprintln!("{}: {}", name, e);
            std::process::exit(1);
        }
    }
}
