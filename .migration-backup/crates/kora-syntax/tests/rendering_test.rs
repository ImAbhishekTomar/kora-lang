//! How the syntax layer renders itself to a person.
//!
//! `TokenKind`'s `Display`, `TypeExpr::display`, `BinOp::symbol`, and
//! `SyntaxError::render` all exist for exactly one audience: someone reading
//! an error message. They are invisible to the rest of the compiler, which is
//! why they drift — a token added with a placeholder name shows up in a
//! diagnostic months later reading `Ident`.

use kora_syntax::ast::{BinOp, TypeExpr};
use kora_syntax::token::{Span, TokenKind};
use kora_syntax::{parse, SyntaxError};

/// Every variant, and the text a diagnostic will show for it.
///
/// Written out rather than generated, so adding a token to the language is a
/// line here too: the compiler refuses this list when a variant is missed
/// only if it is exhaustive, and the point of the table is that a human chose
/// each word.
fn every_token() -> Vec<(TokenKind, &'static str)> {
    use TokenKind::*;
    vec![
        (Int(12), "12"),
        (Float(1.5), "1.5"),
        (Str("hello".into()), "string"),
        (
            FStr {
                parts: vec!["a".into()],
                exprs: vec!["b".into()],
            },
            "f-string",
        ),
        (True, "True"),
        (False, "False"),
        (None, "None"),
        (Ident("total".into()), "total"),
        (Def, "def"),
        (If, "if"),
        (Elif, "elif"),
        (Else, "else"),
        (While, "while"),
        (For, "for"),
        (In, "in"),
        (Return, "return"),
        (Break, "break"),
        (Continue, "continue"),
        (Pass, "pass"),
        (And, "and"),
        (Or, "or"),
        (Not, "not"),
        (Type, "type"),
        (Match, "match"),
        (Case, "case"),
        (Agent, "agent"),
        (Tool, "tool"),
        (Classified, "classified"),
        (Declassify, "declassify"),
        (Use, "use"),
        (Test, "test"),
        (Mock, "mock"),
        (Assert, "assert"),
        (Budget, "budget"),
        (Context, "context"),
        (Parallel, "parallel"),
        (Plus, "+"),
        (Minus, "-"),
        (Star, "*"),
        (Slash, "/"),
        (Percent, "%"),
        (DoubleStar, "**"),
        (DoubleSlash, "//"),
        (Pipe, "|"),
        (Eq, "="),
        (EqEq, "=="),
        (NotEq, "!="),
        (Lt, "<"),
        (Gt, ">"),
        (LtEq, "<="),
        (GtEq, ">="),
        (PlusEq, "+="),
        (MinusEq, "-="),
        (StarEq, "*="),
        (SlashEq, "/="),
        (Arrow, "->"),
        (LParen, "("),
        (RParen, ")"),
        (LBracket, "["),
        (RBracket, "]"),
        (LBrace, "{"),
        (RBrace, "}"),
        (Comma, ","),
        (Colon, ":"),
        (Dot, "."),
        (At, "@"),
        (Newline, "newline"),
        (Indent, "indent"),
        (Dedent, "dedent"),
        (Eof, "end of file"),
    ]
}

#[test]
fn every_token_renders_as_the_thing_a_person_typed() {
    for (kind, expected) in every_token() {
        assert_eq!(
            kind.to_string(),
            expected,
            "the diagnostic for {kind:?} should read `{expected}`"
        );
    }
}

#[test]
fn a_token_never_renders_as_its_rust_name() {
    // The failure mode this guards: a new variant falls through to a derived
    // name and a user is told to expect `RBracket`.
    for (kind, rendered) in every_token() {
        let rust_name = format!("{kind:?}");
        let rust_name = rust_name.split(['(', ' ', '{']).next().unwrap_or_default();
        if matches!(
            kind,
            TokenKind::True | TokenKind::False | TokenKind::None | TokenKind::Type
        ) {
            // These three really are spelled the same in both languages.
            continue;
        }
        assert_ne!(
            rendered, rust_name,
            "{kind:?} is being shown to users by its Rust name"
        );
    }
}

#[test]
fn a_type_prints_the_way_it_was_written() {
    assert_eq!(TypeExpr::Name("int".into()).display(), "int");
    assert_eq!(
        TypeExpr::Generic("list".into(), vec![TypeExpr::Name("str".into())]).display(),
        "list[str]"
    );
    // Nesting, because a message about the wrong element type has to show
    // the whole shape to be worth reading.
    assert_eq!(
        TypeExpr::Generic(
            "list".into(),
            vec![TypeExpr::Generic(
                "list".into(),
                vec![TypeExpr::Name("int".into())]
            )]
        )
        .display(),
        "list[list[int]]"
    );
}

#[test]
fn every_binary_operator_has_its_symbol() {
    let pairs = [
        (BinOp::Add, "+"),
        (BinOp::Sub, "-"),
        (BinOp::Mul, "*"),
        (BinOp::Div, "/"),
        (BinOp::Mod, "%"),
        (BinOp::Pow, "**"),
        (BinOp::FloorDiv, "//"),
        (BinOp::Eq, "=="),
        (BinOp::NotEq, "!="),
        (BinOp::Lt, "<"),
        (BinOp::Gt, ">"),
        (BinOp::LtEq, "<="),
        (BinOp::GtEq, ">="),
        (BinOp::And, "and"),
        (BinOp::Or, "or"),
    ];
    for (op, symbol) in pairs {
        assert_eq!(op.symbol(), symbol, "{op:?} should print as `{symbol}`");
    }
}

#[test]
fn a_rendered_error_shows_the_offending_line_and_points_at_it() {
    let source = "def main():\n    print(nope\n";
    let error = SyntaxError::new("expected `)`", Span::new(26, 27, 2, 15));
    let rendered = error.render(source, "prog.ko");
    assert!(rendered.contains("prog.ko"), "the file: {rendered}");
    assert!(rendered.contains("expected `)`"), "the message: {rendered}");
    assert!(
        rendered.contains("print(nope"),
        "the line itself, so the reader does not have to open the file: {rendered}"
    );
    assert!(rendered.contains('2'), "the line number: {rendered}");
}

#[test]
fn a_hint_is_rendered_when_there_is_one() {
    let source = "x = 1\n";
    let plain = SyntaxError::new("bad", Span::new(0, 1, 1, 1)).render(source, "p.ko");
    let hinted = SyntaxError::new("bad", Span::new(0, 1, 1, 1))
        .with_hint("try the other thing")
        .render(source, "p.ko");
    assert!(!plain.contains("try the other thing"));
    assert!(
        hinted.contains("try the other thing"),
        "the hint should reach the reader: {hinted}"
    );
}

#[test]
fn an_error_displays_as_its_message() {
    let error = SyntaxError::new("something went wrong", Span::new(0, 1, 1, 1));
    assert!(
        error.to_string().contains("something went wrong"),
        "got: {error}"
    );
}

#[test]
fn rendering_survives_a_span_past_the_end_of_the_source() {
    // A truncated file can produce a span pointing past what exists. The
    // renderer is the last thing standing between that and a panic in the
    // middle of reporting someone else's error.
    let error = SyntaxError::new("unexpected end of file", Span::new(900, 901, 99, 1));
    let rendered = error.render("x = 1\n", "p.ko");
    assert!(
        rendered.contains("unexpected end of file"),
        "got: {rendered}"
    );
}

#[test]
fn a_real_parse_failure_renders_the_same_way() {
    // End to end, so the shape above is the shape users actually see.
    let source = "def main():\n    print(\n";
    let error = parse(source).expect_err("this should not parse");
    let rendered = error.render(source, "prog.ko");
    assert!(rendered.contains("prog.ko"), "got: {rendered}");
}
