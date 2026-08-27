//! kora-syntax: lexer, parser, and AST for the Kora language.

pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod token;

pub use ast::Program;
pub use error::SyntaxError;
pub use parser::parse;
