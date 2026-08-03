//! delphi-parser — Delphi source parser + incremental analysis engine.
//!
//! Vendored into the delphi-devkit workspace (history discarded per the
//! crate's own decision log). Consumed by `ddk-server` (the LSP) through the
//! query surface on [`driver::ProjectSession`] plus the [`query`] result types.
//!
//! Architecture (see the crate's SESSION.md/REVIEW.md carried in-repo):
//! process-global interner/arena ([`globals`]) → logos lexer ([`token`]) →
//! directive cursor ([`token_cursor`]) → grammar parser ([`parser`]) →
//! full-AST [`unit_meta::UnitMeta`] (transparent serde) → moka
//! [`unit_cache`] + [`cache_store`] snapshots. Cross-unit resolution,
//! `{$IF}` evaluation ([`if_eval`]), record layout ([`layout`]), the DFM↔PAS
//! linker ([`dfm_link`]), and the LSP query API ([`query`]) sit on top.

pub mod ast;
pub mod ast_impl;
pub mod cache_store;
pub mod context;
pub mod dfm;
pub mod dfm_link;
pub mod driver;
pub mod globals;
pub mod if_eval;
pub mod layout;
pub mod mem_guard;
pub mod meta;
pub mod parse_state;
pub mod parser;
pub mod pipeline;
pub mod query;
pub mod references;
pub mod source;
pub mod token;
pub mod token_cursor;
pub mod unit_cache;
pub mod unit_loader;
pub mod unit_meta;
pub mod unit_resolution;
pub mod watcher;