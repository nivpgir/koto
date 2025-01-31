//! Contains the parser and AST format used by the Koto language

#![warn(missing_docs)]

mod ast;
mod constant_index;
mod constant_pool;
mod error;
mod node;
mod parser;

pub use {
    ast::*,
    constant_index::{ConstantIndex, ConstantIndexTryFromOutOfRange},
    constant_pool::{Constant, ConstantPool},
    error::{format_error_with_excerpt, ParserError},
    koto_lexer::{Position, Span},
    node::*,
    parser::Parser,
};
