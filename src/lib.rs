#![recursion_limit = "512"]

pub mod cli;
pub mod config;
pub mod folders;
pub mod money;
pub mod output;
pub mod schema;

// The SOAP client, error type, and period helpers now live in the `yuki-client`
// crate. Re-export them so existing `crate::{client,error,period}` paths in the
// CLI and `yuki_cli::{client,error,period}` paths in tests keep resolving.
pub use yuki_client::{client, error, period};
