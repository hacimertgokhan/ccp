use crate::exec::{Backend, Executor};
use ccp_mcp::{Meta, Service};
use ccp_proto::engine::RootResolver;

struct SqlService(Executor);

impl Service for SqlService {
    fn schema(&self) -> String {
        self.0.catalog.schema_text()
    }
    fn root(&self) -> &dyn RootResolver {
        &self.0
    }
    fn info(&self) -> String {
        self.0.info()
    }
    fn hint(&self) -> String {
        format!(
            "Tables: {}.",
            self.0
                .catalog
                .tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Entry point shared by every SQL server binary.
pub fn main(
    name: &'static str,
    kind: &'static str,
    connect: fn() -> Result<Box<dyn Backend>, String>,
) {
    let meta = Meta {
        name,
        kind,
        usage: "Root fields are tables; arguments filter by column equality plus limit/offset; \
foreign keys appear as nested fields, so related data comes back in one call.",
        example: "users(limit:5,active:1){id name posts{title}}",
    };
    ccp_mcp::main(meta, || {
        Ok(Box::new(SqlService(Executor::new(connect()?)?)) as Box<dyn Service>)
    });
}
