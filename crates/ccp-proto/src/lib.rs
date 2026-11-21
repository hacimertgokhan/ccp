//! CCP protocol core. Dependency-free on purpose: the CLI host and every
//! plugin (e.g. `mysql-ccp`) build on this one small crate.

pub mod engine;
pub mod query;
pub mod serve;
pub mod value;
