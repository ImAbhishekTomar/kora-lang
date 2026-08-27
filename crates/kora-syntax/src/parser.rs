//! Recursive-descent parser with Pratt-style expression precedence.

use crate::ast::*;
use crate::error::SyntaxError;
use crate::lexer::Lexer;
use crate::token::{Span, Token, TokenKind};

pub fn parse(source: &str) -> Result<Program, SyntaxError> {
    let tokens = Lexer::new(source).tokenize()?;
    Parser::new(tokens).parse_program()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn parse_program(&mut self) -> Result<Program, SyntaxError> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::Eof) {
            items.push(self.statement()?);
            self.skip_newlines();
        }
        Ok(Program { items })
    }

    // --- statements ---

    fn statement(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        match self.peek_kind().clone() {
            TokenKind::Def => self.func_def(),
            TokenKind::Type => self.type_def(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::For => self.for_stmt(),
            TokenKind::Return => {
                self.advance();
                let value = if self.check(&TokenKind::Newline) {
                    None
                } else {
                    Some(self.expression()?)
                };
                self.expect_newline("return")?;
                Ok(Stmt {
                    kind: StmtKind::Return(value),
                    span,
                })
            }
            TokenKind::Break => {
                self.advance();
                self.expect_newline("break")?;
                Ok(Stmt {
                    kind: StmtKind::Break,
                    span,
                })
            }
            TokenKind::Continue => {
                self.advance();
                self.expect_newline("continue")?;
                Ok(Stmt {
                    kind: StmtKind::Continue,
                    span,
                })
            }
            TokenKind::Pass => {
                self.advance();
                self.expect_newline("pass")?;
                Ok(Stmt {
                    kind: StmtKind::Pass,
                    span,
                })
            }
            _ => self.expr_or_assign(),
        }
    }

    /// Expression statement, assignment, annotated assignment, or aug-assign.
    fn expr_or_assign(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        let expr = self.expression()?;

        // `x: Type = value`
        if self.check(&TokenKind::Colon) {
            if !matches!(expr.kind, ExprKind::Name(_)) {
                return Err(SyntaxError::new(
                    "type annotation is only allowed on a simple variable",
                    span,
                ));
            }
            self.advance();
            let ty = self.type_expr()?;
            self.expect(&TokenKind::Eq, "expected `=` after type annotation")?;
            let value = self.expression()?;
            self.expect_newline("assignment")?;
            return Ok(Stmt {
                kind: StmtKind::Assign {
                    target: expr,
                    ty: Some(ty),
                    value,
                },
                span,
            });
        }

        // `target = value`
        if self.check(&TokenKind::Eq) {
            self.validate_assign_target(&expr)?;
            self.advance();
            let value = self.expression()?;
            self.expect_newline("assignment")?;
            return Ok(Stmt {
                kind: StmtKind::Assign {
                    target: expr,
                    ty: None,
                    value,
                },
                span,
            });
        }

        // `target op= value`
        let aug = match self.peek_kind() {
            TokenKind::PlusEq => Some(BinOp::Add),
            TokenKind::MinusEq => Some(BinOp::Sub),
            TokenKind::StarEq => Some(BinOp::Mul),
            TokenKind::SlashEq => Some(BinOp::Div),
            _ => None,
        };
        if let Some(op) = aug {
            self.validate_assign_target(&expr)?;
            self.advance();
            let value = self.expression()?;
            self.expect_newline("assignment")?;
            return Ok(Stmt {
                kind: StmtKind::AugAssign {
                    target: expr,
                    op,
                    value,
                },
                span,
            });
        }

        self.expect_newline("expression")?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span,
        })
    }

    fn validate_assign_target(&self, expr: &Expr) -> Result<(), SyntaxError> {
        match &expr.kind {
            ExprKind::Name(_) | ExprKind::Attr { .. } | ExprKind::Index { .. } => Ok(()),
            _ => Err(
                SyntaxError::new("cannot assign to this expression", expr.span).with_hint(
                    "assignment targets are variables, attributes (`a.b`), or items (`a[i]`)",
                ),
            ),
        }
    }

    fn func_def(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // def
        let name = self.expect_ident("function name after `def`")?;
        self.expect(&TokenKind::LParen, "expected `(` after function name")?;
        let mut params = Vec::new();
        while !self.check(&TokenKind::RParen) {
            let pspan = self.peek_span();
            let pname = self.expect_ident("parameter name")?;
            let ty = if self.check(&TokenKind::Colon) {
                self.advance();
                Some(self.type_expr()?)
            } else {
                None
            };
            params.push(Param {
                name: pname,
                ty,
                span: pspan,
            });
            if !self.check(&TokenKind::RParen) {
                self.expect(&TokenKind::Comma, "expected `,` between parameters")?;
            }
        }
        self.advance(); // )
        let return_ty = if self.check(&TokenKind::Arrow) {
            self.advance();
            Some(self.type_expr()?)
        } else {
            None
        };
        let body = self.block("function body")?;
        Ok(Stmt {
            kind: StmtKind::FuncDef(FuncDef {
                name,
                params,
                return_ty,
                body,
            }),
            span,
        })
    }

    fn type_def(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // type
        let name = self.expect_ident("type name after `type`")?;
        self.expect(&TokenKind::Colon, "expected `:` after type name")?;
        self.expect(&TokenKind::Newline, "expected newline after `:`")?;
        self.expect(&TokenKind::Indent, "expected an indented block of fields")?;
        let mut fields = Vec::new();
        while !self.check(&TokenKind::Dedent) {
            self.skip_newlines();
            if self.check(&TokenKind::Dedent) {
                break;
            }
            let fspan = self.peek_span();
            let fname = self.expect_ident("field name")?;
            self.expect(&TokenKind::Colon, "expected `:` after field name")?;
            let ty = self.type_expr()?;
            fields.push(FieldDef {
                name: fname,
                ty,
                span: fspan,
            });
            self.expect_newline("field")?;
        }
        self.advance(); // dedent
        if fields.is_empty() {
            return Err(
                SyntaxError::new(format!("type `{name}` has no fields"), span)
                    .with_hint("a type block needs at least one `name: type` line"),
            );
        }
        Ok(Stmt {
            kind: StmtKind::TypeDef { name, fields },
            span,
        })
    }

    fn if_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // if
        let mut branches = Vec::new();
        let cond = self.expression()?;
        let body = self.block("if body")?;
        branches.push((cond, body));
        let mut else_body = None;
        loop {
            self.skip_newlines();
            if self.check(&TokenKind::Elif) {
                self.advance();
                let c = self.expression()?;
                let b = self.block("elif body")?;
                branches.push((c, b));
            } else if self.check(&TokenKind::Else) {
                self.advance();
                else_body = Some(self.block("else body")?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt {
            kind: StmtKind::If {
                branches,
                else_body,
            },
            span,
        })
    }

    fn while_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance();
        let cond = self.expression()?;
        let body = self.block("while body")?;
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            span,
        })
    }

    fn for_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance();
        let var = self.expect_ident("loop variable after `for`")?;
        self.expect(&TokenKind::In, "expected `in` after loop variable")?;
        let iter = self.expression()?;
        let body = self.block("for body")?;
        Ok(Stmt {
            kind: StmtKind::For { var, iter, body },
            span,
        })
    }

    /// `: NEWLINE INDENT stmt+ DEDENT`
    fn block(&mut self, what: &str) -> Result<Vec<Stmt>, SyntaxError> {
        self.expect(&TokenKind::Colon, &format!("expected `:` to start {what}"))?;
        self.expect(&TokenKind::Newline, "expected newline after `:`")?;
        if !self.check(&TokenKind::Indent) {
            return Err(
                SyntaxError::new(format!("expected an indented {what}"), self.peek_span())
                    .with_hint("indent the block with 4 spaces"),
            );
        }
        self.advance();
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.check(&TokenKind::Dedent) && !self.check(&TokenKind::Eof) {
            stmts.push(self.statement()?);
            self.skip_newlines();
        }
        if self.check(&TokenKind::Dedent) {
            self.advance();
        }
        Ok(stmts)
    }

    // --- types ---

    fn type_expr(&mut self) -> Result<TypeExpr, SyntaxError> {
        let name = self.expect_ident("a type name")?;
        if self.check(&TokenKind::LBracket) {
            self.advance();
            let mut args = Vec::new();
            loop {
                args.push(self.type_expr()?);
                if self.check(&TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "expected `]` to close type arguments")?;
            Ok(TypeExpr::Generic(name, args))
        } else {
            Ok(TypeExpr::Name(name))
        }
    }

    // --- expressions (precedence climbing) ---

    fn expression(&mut self) -> Result<Expr, SyntaxError> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.and_expr()?;
        while self.check(&TokenKind::Or) {
            self.advance();
            let right = self.and_expr()?;
            left = binary(BinOp::Or, left, right);
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.not_expr()?;
        while self.check(&TokenKind::And) {
            self.advance();
            let right = self.not_expr()?;
            left = binary(BinOp::And, left, right);
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr, SyntaxError> {
        if self.check(&TokenKind::Not) {
            let span = self.peek_span();
            self.advance();
            let operand = self.not_expr()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                TokenKind::In => BinOp::In,
                TokenKind::Not if self.peek_next_is(&TokenKind::In) => {
                    self.advance(); // not
                    self.advance(); // in
                    let right = self.additive()?;
                    left = binary(BinOp::NotIn, left, right);
                    continue;
                }
                _ => break,
            };
            self.advance();
            let right = self.additive()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.multiplicative()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::DoubleSlash => BinOp::FloorDiv,
                TokenKind::Percent => BinOp::Mod,
                TokenKind::DoubleStar => BinOp::Pow,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            left = binary(op, left, right);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, SyntaxError> {
        if self.check(&TokenKind::Minus) {
            let span = self.peek_span();
            self.advance();
            let operand = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        self.postfix()
    }

    /// Calls, attribute access, indexing, slicing — left-associative chains.
    fn postfix(&mut self) -> Result<Expr, SyntaxError> {
        let mut expr = self.primary()?;
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    let span = self.peek_span();
                    self.advance();
                    let mut args = Vec::new();
                    self.skip_newlines_in_brackets();
                    while !self.check(&TokenKind::RParen) {
                        args.push(self.expression()?);
                        self.skip_newlines_in_brackets();
                        if !self.check(&TokenKind::RParen) {
                            self.expect(&TokenKind::Comma, "expected `,` between arguments")?;
                            self.skip_newlines_in_brackets();
                        }
                    }
                    self.advance();
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    };
                }
                TokenKind::Dot => {
                    let span = self.peek_span();
                    self.advance();
                    let name = self.expect_ident("attribute name after `.`")?;
                    expr = Expr {
                        kind: ExprKind::Attr {
                            object: Box::new(expr),
                            name,
                        },
                        span,
                    };
                }
                TokenKind::LBracket => {
                    let span = self.peek_span();
                    self.advance();
                    // Slice or index?
                    let start = if self.check(&TokenKind::Colon) {
                        None
                    } else {
                        Some(Box::new(self.expression()?))
                    };
                    if self.check(&TokenKind::Colon) {
                        self.advance();
                        let stop = if self.check(&TokenKind::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.expression()?))
                        };
                        self.expect(&TokenKind::RBracket, "expected `]` to close slice")?;
                        expr = Expr {
                            kind: ExprKind::Slice {
                                object: Box::new(expr),
                                start,
                                stop,
                            },
                            span,
                        };
                    } else {
                        self.expect(&TokenKind::RBracket, "expected `]` to close index")?;
                        expr = Expr {
                            kind: ExprKind::Index {
                                object: Box::new(expr),
                                index: start
                                    .ok_or_else(|| SyntaxError::new("empty index", span))?,
                            },
                            span,
                        };
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, SyntaxError> {
        let span = self.peek_span();
        let kind = self.peek_kind().clone();
        match kind {
            TokenKind::Int(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Int(v),
                    span,
                })
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Float(v),
                    span,
                })
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Str(s),
                    span,
                })
            }
            TokenKind::FStr { parts, exprs } => {
                self.advance();
                // Parse each hole's source text as an expression.
                let mut parsed = Vec::new();
                for src in &exprs {
                    let mut sub = parse(src).map_err(|e| {
                        SyntaxError::new(
                            format!("invalid expression in f-string: {}", e.message),
                            span,
                        )
                    })?;
                    match sub.items.pop().map(|s| s.kind) {
                        Some(StmtKind::Expr(e)) if sub.items.is_empty() => parsed.push(e),
                        _ => {
                            return Err(SyntaxError::new(
                                "f-string hole must be a single expression",
                                span,
                            ));
                        }
                    }
                }
                Ok(Expr {
                    kind: ExprKind::FString {
                        parts,
                        exprs: parsed,
                    },
                    span,
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Bool(true),
                    span,
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Bool(false),
                    span,
                })
            }
            TokenKind::None => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::None,
                    span,
                })
            }
            TokenKind::Ident(name) => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Name(name),
                    span,
                })
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.expression()?;
                self.expect(&TokenKind::RParen, "expected `)` to close `(`")?;
                Ok(inner)
            }
            TokenKind::LBracket => {
                self.advance();
                let mut items = Vec::new();
                self.skip_newlines_in_brackets();
                while !self.check(&TokenKind::RBracket) {
                    items.push(self.expression()?);
                    self.skip_newlines_in_brackets();
                    if !self.check(&TokenKind::RBracket) {
                        self.expect(&TokenKind::Comma, "expected `,` between list items")?;
                        self.skip_newlines_in_brackets();
                    }
                }
                self.advance();
                Ok(Expr {
                    kind: ExprKind::List(items),
                    span,
                })
            }
            TokenKind::LBrace => {
                self.advance();
                let mut pairs = Vec::new();
                self.skip_newlines_in_brackets();
                while !self.check(&TokenKind::RBrace) {
                    let key = self.expression()?;
                    self.expect(&TokenKind::Colon, "expected `:` between key and value")?;
                    let value = self.expression()?;
                    pairs.push((key, value));
                    self.skip_newlines_in_brackets();
                    if !self.check(&TokenKind::RBrace) {
                        self.expect(&TokenKind::Comma, "expected `,` between dict entries")?;
                        self.skip_newlines_in_brackets();
                    }
                }
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Dict(pairs),
                    span,
                })
            }
            other => Err(SyntaxError::new(format!("unexpected `{other}` here"), span)),
        }
    }

    // --- token helpers ---

    fn peek_kind(&self) -> &TokenKind {
        &self.tokens[self.pos.min(self.tokens.len() - 1)].kind
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos.min(self.tokens.len() - 1)].span
    }

    fn peek_next_is(&self, kind: &TokenKind) -> bool {
        self.tokens
            .get(self.pos + 1)
            .map(|t| &t.kind == kind)
            .unwrap_or(false)
    }

    fn check(&self, kind: &TokenKind) -> bool {
        self.peek_kind() == kind
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
    }

    fn expect(&mut self, kind: &TokenKind, msg: &str) -> Result<(), SyntaxError> {
        if self.check(kind) {
            self.advance();
            Ok(())
        } else {
            Err(SyntaxError::new(
                format!("{msg}, found `{}`", self.peek_kind()),
                self.peek_span(),
            ))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, SyntaxError> {
        if let TokenKind::Ident(name) = self.peek_kind().clone() {
            self.advance();
            Ok(name)
        } else {
            Err(SyntaxError::new(
                format!("expected {what}, found `{}`", self.peek_kind()),
                self.peek_span(),
            ))
        }
    }

    fn expect_newline(&mut self, after: &str) -> Result<(), SyntaxError> {
        match self.peek_kind() {
            TokenKind::Newline => {
                self.advance();
                Ok(())
            }
            TokenKind::Eof | TokenKind::Dedent => Ok(()),
            other => Err(SyntaxError::new(
                format!("unexpected `{other}` after {after}"),
                self.peek_span(),
            )
            .with_hint("each statement goes on its own line")),
        }
    }

    fn skip_newlines(&mut self) {
        while self.check(&TokenKind::Newline) {
            self.advance();
        }
    }

    /// Inside brackets the lexer suppresses Newline tokens, but Indent/Dedent
    /// bookkeeping from multiline literals can still appear; skip defensively.
    fn skip_newlines_in_brackets(&mut self) {
        while matches!(
            self.peek_kind(),
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.advance();
        }
    }
}

fn binary(op: BinOp, left: Expr, right: Expr) -> Expr {
    let span = left.span;
    Expr {
        kind: ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        },
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Program {
        parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\nsource:\n{src}"))
    }

    #[test]
    fn assignment_and_types() {
        let p = ok("x = 1\ny: int = 2\n");
        assert_eq!(p.items.len(), 2);
        match &p.items[1].kind {
            StmtKind::Assign { ty: Some(t), .. } => assert_eq!(t.display(), "int"),
            other => panic!("expected typed assign, got {other:?}"),
        }
    }

    #[test]
    fn function_def_full() {
        let p = ok("def add(a: int, b: int) -> int:\n    return a + b\n");
        match &p.items[0].kind {
            StmtKind::FuncDef(f) => {
                assert_eq!(f.name, "add");
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.return_ty.as_ref().unwrap().display(), "int");
            }
            other => panic!("expected func def, got {other:?}"),
        }
    }

    #[test]
    fn type_def_fields() {
        let p = ok("type Expense:\n    merchant: str\n    amount: float\n");
        match &p.items[0].kind {
            StmtKind::TypeDef { name, fields } => {
                assert_eq!(name, "Expense");
                assert_eq!(fields.len(), 2);
            }
            other => panic!("expected type def, got {other:?}"),
        }
    }

    #[test]
    fn if_elif_else() {
        let p = ok("if a:\n    x = 1\nelif b:\n    x = 2\nelse:\n    x = 3\n");
        match &p.items[0].kind {
            StmtKind::If {
                branches,
                else_body,
            } => {
                assert_eq!(branches.len(), 2);
                assert!(else_body.is_some());
            }
            other => panic!("expected if, got {other:?}"),
        }
    }

    #[test]
    fn for_loop_over_call() {
        let p = ok("for row in load_csv(\"x.csv\"):\n    print(row)\n");
        assert!(matches!(p.items[0].kind, StmtKind::For { .. }));
    }

    #[test]
    fn precedence() {
        // 1 + 2 * 3 parses as 1 + (2 * 3)
        let p = ok("x = 1 + 2 * 3\n");
        match &p.items[0].kind {
            StmtKind::Assign { value, .. } => match &value.kind {
                ExprKind::Binary {
                    op: BinOp::Add,
                    right,
                    ..
                } => {
                    assert!(matches!(
                        right.kind,
                        ExprKind::Binary { op: BinOp::Mul, .. }
                    ));
                }
                other => panic!("expected add at top, got {other:?}"),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn attr_call_chain() {
        ok("result = db.query(\"select 1\").first()\n");
    }

    #[test]
    fn fstring_hole_parsed() {
        let p = ok("print(f\"total: {a + b}\")\n");
        // Drill to the f-string expr.
        match &p.items[0].kind {
            StmtKind::Expr(e) => match &e.kind {
                ExprKind::Call { args, .. } => match &args[0].kind {
                    ExprKind::FString { exprs, .. } => {
                        assert!(matches!(exprs[0].kind, ExprKind::Binary { .. }))
                    }
                    other => panic!("expected fstring, got {other:?}"),
                },
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn multiline_list_and_dict() {
        ok("x = [1,\n     2,\n     3]\nd = {\"a\": 1, \"b\": 2}\n");
    }

    #[test]
    fn slice_and_index() {
        ok("a = xs[0]\nb = xs[1:3]\nc = xs[:2]\nd = xs[2:]\n");
    }

    #[test]
    fn aug_assign() {
        let p = ok("x += 1\n");
        assert!(matches!(
            p.items[0].kind,
            StmtKind::AugAssign { op: BinOp::Add, .. }
        ));
    }

    #[test]
    fn error_has_hint() {
        let err = parse("if x:\nprint(1)\n").unwrap_err();
        assert!(
            err.hint.is_some(),
            "missing-indent error should carry a hint"
        );
    }

    #[test]
    fn error_on_bad_assign_target() {
        let err = parse("1 + 2 = 3\n").unwrap_err();
        assert!(err.message.contains("cannot assign"));
    }
}
