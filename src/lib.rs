//! MiniVectorDB — a small vector database server for the 2026 OceanBase
//! competition round-2 problem set.
//!
//! The binary in `src/main.rs` is a thin wrapper around this library so that the
//! protocol, engine and storage layers can be tested without spawning a
//! process. Layers, bottom-up:
//!
//! * [`jsonl`] — official JSON Lines wire protocol;
//! * [`proto`] — optional text console statement framing;
//! * [`error`] — error codes and the `ERROR <code>: <message>` block;
//! * [`response`] — result-set / status rendering;
//! * [`runner`] — the read-execute-write loop and its [`runner::Handler`] seam;
//! * engine and storage modules follow in later steps.

pub mod ast;
pub mod distance;
pub mod error;
pub mod executor;
pub mod lexer;
pub mod jsonl;
pub mod parser;
pub mod proto;
pub mod response;
pub mod runner;
pub mod schema;
pub mod storage;

pub use ast::{Statement, Value};
pub use error::{DbError, DbResult, ErrorCode};
pub use executor::Session;
pub use response::{Cell, Response, ResultSet};
pub use runner::Handler;
pub use schema::TableSchema;
