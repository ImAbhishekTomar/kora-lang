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
            TokenKind::Def | TokenKind::Agent | TokenKind::Tool => self.func_def(),
            TokenKind::Type => self.type_def(),
            TokenKind::Match => self.match_stmt(),
            TokenKind::Budget => self.budget_line(),
            TokenKind::Declassify => self.declassify_stmt(),
            TokenKind::Use => self.use_stmt(),
            TokenKind::Test => self.test_stmt(),
            TokenKind::Assert => self.assert_stmt(),
            TokenKind::Parallel => self.parallel_for(None),
            TokenKind::Classified => self.classified_assign(),
            TokenKind::Ident(name) if name == "with" && self.peek_next_is(&TokenKind::Budget) => {
                self.with_stmt()
            }
            TokenKind::Ident(name) if name == "with" && self.peek_next_is(&TokenKind::Mock) => {
                self.with_mock()
            }
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
                    classified: false,
                },
                span,
            });
        }

        // `target = value`
        if self.check(&TokenKind::Eq) {
            self.validate_assign_target(&expr)?;
            self.advance();
            // `results = parallel for x in xs:` binds the collected results.
            if self.check(&TokenKind::Parallel) {
                let name = match &expr.kind {
                    ExprKind::Name(n) => n.clone(),
                    _ => {
                        return Err(SyntaxError::new(
                            "results of `parallel for` must be assigned to a plain variable",
                            expr.span,
                        ))
                    }
                };
                return self.parallel_for(Some(name));
            }
            let value = self.expression()?;
            self.expect_newline("assignment")?;
            return Ok(Stmt {
                kind: StmtKind::Assign {
                    target: expr,
                    ty: None,
                    value,
                    classified: false,
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

    /// `def` / `agent` / `tool` share one shape; only their powers differ.
    fn func_def(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        let kind = match self.peek_kind() {
            TokenKind::Def => FuncKind::Def,
            TokenKind::Agent => FuncKind::Agent,
            TokenKind::Tool => FuncKind::Tool,
            other => {
                return Err(SyntaxError::new(
                    format!("expected `def`, `agent`, or `tool`, found `{other}`"),
                    span,
                ))
            }
        };
        let keyword = self.peek_kind().to_string();
        self.advance();
        let name = self.expect_ident(&format!("a name after `{keyword}`"))?;
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
        let mut body = self.block("function body")?;
        // A leading string statement is the docstring; tools send it to models
        // as their description, so it is pulled out rather than executed.
        let doc = match body.first().map(|s| &s.kind) {
            Some(StmtKind::Expr(Expr {
                kind: ExprKind::Str(text),
                ..
            })) => {
                let text = text.clone();
                body.remove(0);
                Some(text)
            }
            _ => None,
        };
        // A leading `budget:` line applies to the whole body.
        let budget = match body.first().map(|s| &s.kind) {
            Some(StmtKind::WithBudget { budget, body: b }) if b.is_empty() => {
                let budget = budget.clone();
                body.remove(0);
                Some(budget)
            }
            _ => None,
        };
        Ok(Stmt {
            kind: StmtKind::FuncDef(FuncDef {
                name,
                params,
                return_ty,
                body,
                kind,
                budget,
                doc,
            }),
            span,
        })
    }

    /// Two forms:
    ///   `budget: max_tokens = 20_000, max_calls = 5`   (declaration line)
    ///   `with budget(max_tokens = 500):`               (scoped fence)
    fn budget_line(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // budget
        self.expect(&TokenKind::Colon, "expected `:` after `budget`")?;
        let budget = self.budget_fields(span.line, &[TokenKind::Newline])?;
        self.expect_newline("budget")?;
        Ok(Stmt {
            kind: StmtKind::WithBudget {
                budget,
                body: Vec::new(),
            },
            span,
        })
    }

    /// `name = value, name = value` until one of `terminators` is reached.
    fn budget_fields(
        &mut self,
        line: u32,
        terminators: &[TokenKind],
    ) -> Result<BudgetSpec, SyntaxError> {
        let mut spec = BudgetSpec {
            span_line: line,
            ..Default::default()
        };
        let mut seen_any = false;
        loop {
            if terminators.iter().any(|t| self.check(t)) {
                break;
            }
            let field_span = self.peek_span();
            let field = self.expect_ident("a budget field name")?;
            self.expect(&TokenKind::Eq, "expected `=` after budget field")?;
            let value_span = self.peek_span();
            let value = match self.peek_kind().clone() {
                TokenKind::Int(v) if v >= 0 => {
                    self.advance();
                    v as u64
                }
                other => {
                    return Err(SyntaxError::new(
                        format!("budget values must be whole numbers, found `{other}`"),
                        value_span,
                    )
                    .with_hint("budgets are counted in tokens, calls, or steps"));
                }
            };
            match field.as_str() {
                "max_tokens" => spec.max_tokens = Some(value),
                "max_calls" => spec.max_calls = Some(value),
                "max_steps" => spec.max_steps = Some(value),
                other => {
                    return Err(SyntaxError::new(
                        format!("unknown budget field `{other}`"),
                        field_span,
                    )
                    .with_hint("known fields: max_tokens, max_calls, max_steps"));
                }
            }
            seen_any = true;
            if self.check(&TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        if !seen_any {
            return Err(
                SyntaxError::new("budget needs at least one limit", self.peek_span())
                    .with_hint("for example: `budget: max_tokens = 20_000`"),
            );
        }
        Ok(spec)
    }

    /// `use json` / `use json as j`
    fn use_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // use

        // `use "./lib/tax.ko" as tax` loads another Kora file. A string
        // literal marks it, so file imports and stdlib modules can never be
        // mistaken for one another.
        if let TokenKind::Str(path) = self.peek_kind().clone() {
            self.advance();
            let alias = match self.peek_kind() {
                TokenKind::Ident(word) if word == "as" => {
                    self.advance();
                    self.expect_ident("a name after `as`")?
                }
                // A path has no natural bare name, so require one rather
                // than inventing a binding from the file stem.
                _ => {
                    return Err(
                        SyntaxError::new(format!("`use \"{path}\"` needs a name"), span)
                            .with_hint(format!("write `use \"{path}\" as <name>`")),
                    )
                }
            };
            if path.is_empty() {
                return Err(SyntaxError::new("an import path cannot be empty", span));
            }
            self.expect_newline("use")?;
            return Ok(Stmt {
                kind: StmtKind::UseFile { path, alias },
                span,
            });
        }

        // `use python <module> as <alias>` reaches a Python module through
        // the sidecar worker.
        if matches!(self.peek_kind(), TokenKind::Ident(word) if word == "python") {
            self.advance();
            // Python module names are dotted: `os.path`, `xml.etree`.
            let mut module = self.expect_ident("a module name after `use python`")?;
            while self.check(&TokenKind::Dot) {
                self.advance();
                module.push('.');
                module.push_str(&self.expect_ident("a name after `.`")?);
            }
            let alias = match self.peek_kind() {
                TokenKind::Ident(word) if word == "as" => {
                    self.advance();
                    self.expect_ident("a name after `as`")?
                }
                // A dotted module has no usable bare name, so require one.
                _ if module.contains('.') => {
                    return Err(SyntaxError::new(format!("`{module}` needs a name"), span)
                        .with_hint(format!("write `use python {module} as <name>`")))
                }
                _ => module.clone(),
            };
            self.expect_newline("use")?;
            return Ok(Stmt {
                kind: StmtKind::UsePython { module, alias },
                span,
            });
        }

        // `use pkg <name> as <alias>` names a dependency. The name is a
        // Kora identifier, so unlike a path it has a natural binding and
        // `as` stays optional.
        if matches!(self.peek_kind(), TokenKind::Ident(word) if word == "pkg") {
            self.advance();
            let package = self.expect_ident("a package name after `use pkg`")?;
            let alias = match self.peek_kind() {
                TokenKind::Ident(word) if word == "as" => {
                    self.advance();
                    self.expect_ident("a name after `as`")?
                }
                _ => package.clone(),
            };
            self.expect_newline("use")?;
            return Ok(Stmt {
                kind: StmtKind::UsePkg { package, alias },
                span,
            });
        }

        // `use mcp <server> as <alias>` names a server configured in
        // kora.toml rather than a stdlib module.
        if matches!(self.peek_kind(), TokenKind::Ident(word) if word == "mcp") {
            self.advance();
            let server = self.expect_ident("a server name after `use mcp`")?;
            let alias = match self.peek_kind() {
                TokenKind::Ident(word) if word == "as" => {
                    self.advance();
                    self.expect_ident("a name after `as`")?
                }
                _ => server.clone(),
            };
            self.expect_newline("use")?;
            return Ok(Stmt {
                kind: StmtKind::UseMcp { server, alias },
                span,
            });
        }

        let module = self.expect_ident("a module name after `use`")?;
        let alias = match self.peek_kind() {
            TokenKind::Ident(word) if word == "as" => {
                self.advance();
                self.expect_ident("a name after `as`")?
            }
            _ => module.clone(),
        };
        self.expect_newline("use")?;
        Ok(Stmt {
            kind: StmtKind::Use { module, alias },
            span,
        })
    }

    /// `test "name":` block.
    fn test_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // test
        let name = match self.peek_kind().clone() {
            TokenKind::Str(text) => {
                self.advance();
                text
            }
            other => {
                return Err(SyntaxError::new(
                    format!("expected a test name in quotes, found `{other}`"),
                    self.peek_span(),
                )
                .with_hint("write `test \"does the thing\":`"))
            }
        };
        let body = self.block("test body")?;
        Ok(Stmt {
            kind: StmtKind::Test { name, body },
            span,
        })
    }

    /// `assert <expr>` / `assert <expr>, "message"`
    fn assert_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // assert
        let condition = self.expression()?;
        let message = if self.check(&TokenKind::Comma) {
            self.advance();
            Some(self.expression()?)
        } else {
            None
        };
        self.expect_newline("assert")?;
        Ok(Stmt {
            kind: StmtKind::Assert { condition, message },
            span,
        })
    }

    /// `with mock analyze -> <expr>:` block.
    fn with_mock(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // with
        self.advance(); // mock
        let target = match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                name
            }
            other => {
                return Err(SyntaxError::new(
                    format!("expected something to mock, found `{other}`"),
                    self.peek_span(),
                )
                .with_hint("today only `analyze` can be mocked"))
            }
        };
        self.expect(&TokenKind::Arrow, "expected `->` after the mock target")?;
        let result = self.expression()?;
        let body = self.block("mock block")?;
        Ok(Stmt {
            kind: StmtKind::WithMock {
                target,
                result,
                body,
            },
            span,
        })
    }

    /// `classified name = value` / `classified name: Type = value`
    fn classified_assign(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // classified
        let name_span = self.peek_span();
        let name = self.expect_ident("a variable name after `classified`")?;
        let ty = if self.check(&TokenKind::Colon) {
            self.advance();
            Some(self.type_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "expected `=` in a classified declaration")?;
        let value = self.expression()?;
        self.expect_newline("declaration")?;
        Ok(Stmt {
            kind: StmtKind::Assign {
                target: Expr {
                    kind: ExprKind::Name(name),
                    span: name_span,
                },
                ty,
                value,
                classified: true,
            },
            span,
        })
    }

    /// `declassify <expr> for <sink>:` block, optionally `as <name>`.
    fn declassify_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // declassify
        let value = self.expression()?;

        // `as name` gives the block-local binding an explicit name.
        let explicit_binding = match self.peek_kind() {
            TokenKind::Ident(name) if name == "as" => {
                self.advance();
                Some(self.expect_ident("a name after `as`")?)
            }
            _ => None,
        };

        if !self.check(&TokenKind::For) {
            return Err(SyntaxError::new(
                format!(
                    "expected `for <sink>` after the declassified value, found `{}`",
                    self.peek_kind()
                ),
                self.peek_span(),
            )
            .with_hint("declassification always names where the data may go, e.g. `declassify ssn for local_model:`"));
        }
        self.advance(); // for
        let sink = self.expect_ident("a sink name after `for`")?;

        // Default binding: the variable name when the value is a plain name,
        // so `declassify salary for local_model:` rebinds `salary` in-block.
        let binding = match (explicit_binding, &value.kind) {
            (Some(name), _) => name,
            (None, ExprKind::Name(n)) => n.clone(),
            (None, _) => {
                return Err(
                    SyntaxError::new("this declassified value needs a name", value.span)
                        .with_hint("write `declassify <expr> as <name> for <sink>:`"),
                );
            }
        };

        let body = self.block("declassify block")?;
        Ok(Stmt {
            kind: StmtKind::Declassify {
                value,
                binding,
                sink,
                body,
            },
            span,
        })
    }

    /// `with budget(max_tokens = N):` block.
    fn with_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // `with` (an identifier, not a keyword)
        if !self.check(&TokenKind::Budget) {
            return Err(SyntaxError::new(
                format!(
                    "expected `budget` after `with`, found `{}`",
                    self.peek_kind()
                ),
                self.peek_span(),
            )
            .with_hint("the only `with` form today is `with budget(...):`"));
        }
        self.advance(); // budget
        self.expect(&TokenKind::LParen, "expected `(` after `budget`")?;
        let budget = self.budget_fields(span.line, &[TokenKind::RParen])?;
        self.expect(&TokenKind::RParen, "expected `)` to close budget")?;
        let body = self.block("budget block")?;
        Ok(Stmt {
            kind: StmtKind::WithBudget { budget, body },
            span,
        })
    }

    /// `parallel for x in xs:` — optionally bound as `results = parallel for ...`
    fn parallel_for(&mut self, collect_into: Option<String>) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // parallel
        if !self.check(&TokenKind::For) {
            return Err(SyntaxError::new(
                format!(
                    "expected `for` after `parallel`, found `{}`",
                    self.peek_kind()
                ),
                self.peek_span(),
            )
            .with_hint("write `parallel for item in items:`"));
        }
        self.advance(); // for
        let var = self.expect_ident("loop variable after `for`")?;
        self.expect(&TokenKind::In, "expected `in` after loop variable")?;
        let iter = self.expression()?;
        let body = self.block("parallel for body")?;
        Ok(Stmt {
            kind: StmtKind::ParallelFor {
                var,
                iter,
                body,
                collect_into,
            },
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
            // `classified email: str` marks the field as sensitive; every
            // value read from it carries the label.
            let classified = if self.check(&TokenKind::Classified) {
                self.advance();
                true
            } else {
                false
            };
            let fname = self.expect_ident("field name")?;
            self.expect(&TokenKind::Colon, "expected `:` after field name")?;
            let ty = self.type_expr()?;
            let mut metadata = FieldMetadata::default();
            while self.check(&TokenKind::At) {
                self.field_annotation(&mut metadata)?;
            }
            self.expect_newline("field")?;
            if self.check(&TokenKind::Indent) {
                self.advance();
                self.field_metadata_block(&mut metadata)?;
                self.expect(&TokenKind::Dedent, "expected end of field metadata")?;
            }
            fields.push(FieldDef {
                name: fname,
                ty,
                span: fspan,
                classified,
                metadata,
            });
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

    /// One inline field annotation: `@description("...")`.
    fn field_annotation(&mut self, metadata: &mut FieldMetadata) -> Result<(), SyntaxError> {
        self.advance(); // @
        let name = self.expect_ident("field annotation name after `@`")?;
        self.expect(
            &TokenKind::LParen,
            "expected `(` after field annotation name",
        )?;
        let value = match self.peek_kind().clone() {
            TokenKind::Str(value) => {
                self.advance();
                value
            }
            other => {
                return Err(SyntaxError::new(
                    format!("field annotation `{name}` requires a string, found `{other}`"),
                    self.peek_span(),
                ))
            }
        };
        self.expect(
            &TokenKind::RParen,
            "expected `)` after field annotation value",
        )?;
        self.set_field_metadata(metadata, &name, value)
    }

    /// An indented metadata block beneath one field.
    fn field_metadata_block(&mut self, metadata: &mut FieldMetadata) -> Result<(), SyntaxError> {
        loop {
            self.skip_newlines();
            if self.check(&TokenKind::Dedent) {
                return Ok(());
            }
            let name = self.expect_ident("field metadata name")?;
            self.expect(&TokenKind::Colon, "expected `:` after field metadata name")?;
            let value = match self.peek_kind().clone() {
                TokenKind::Str(value) => {
                    self.advance();
                    value
                }
                other => {
                    return Err(SyntaxError::new(
                        format!("field metadata `{name}` requires a string, found `{other}`"),
                        self.peek_span(),
                    ))
                }
            };
            self.set_field_metadata(metadata, &name, value)?;
            self.expect_newline("field metadata")?;
        }
    }

    fn set_field_metadata(
        &self,
        metadata: &mut FieldMetadata,
        name: &str,
        value: String,
    ) -> Result<(), SyntaxError> {
        let target = match name {
            "description" => &mut metadata.description,
            "pattern" => &mut metadata.pattern,
            _ => {
                return Err(SyntaxError::new(
                    format!("unknown field metadata `{name}`"),
                    self.peek_span(),
                )
                .with_hint("supported metadata: `description` and `pattern`"))
            }
        };
        if target.is_some() {
            return Err(SyntaxError::new(
                format!("field metadata `{name}` is declared more than once"),
                self.peek_span(),
            ));
        }
        *target = Some(value);
        Ok(())
    }

    /// `match expr:` NEWLINE INDENT (`case pattern:` block)+ DEDENT
    fn match_stmt(&mut self) -> Result<Stmt, SyntaxError> {
        let span = self.peek_span();
        self.advance(); // match
        let subject = self.expression()?;
        self.expect(&TokenKind::Colon, "expected `:` after match subject")?;
        self.expect(&TokenKind::Newline, "expected newline after `:`")?;
        self.expect(
            &TokenKind::Indent,
            "expected an indented block of `case` arms",
        )?;
        let mut arms = Vec::new();
        self.skip_newlines();
        while self.check(&TokenKind::Case) {
            let arm_span = self.peek_span();
            self.advance(); // case
            let pattern = self.pattern()?;
            let body = self.block("case body")?;
            arms.push(MatchArm {
                pattern,
                body,
                span: arm_span,
            });
            self.skip_newlines();
        }
        if arms.is_empty() {
            return Err(SyntaxError::new("match block has no `case` arms", span)
                .with_hint("add at least one arm, e.g. `case Ok(value):`"));
        }
        if !self.check(&TokenKind::Dedent) && !self.check(&TokenKind::Eof) {
            return Err(SyntaxError::new(
                format!(
                    "expected `case` or end of match, found `{}`",
                    self.peek_kind()
                ),
                self.peek_span(),
            ));
        }
        if self.check(&TokenKind::Dedent) {
            self.advance();
        }
        Ok(Stmt {
            kind: StmtKind::Match { subject, arms },
            span,
        })
    }

    fn pattern(&mut self) -> Result<Pattern, SyntaxError> {
        let span = self.peek_span();
        match self.peek_kind().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                if name == "_" {
                    return Ok(Pattern::Wildcard);
                }
                if self.check(&TokenKind::LParen) {
                    self.advance();
                    let mut binders = Vec::new();
                    while !self.check(&TokenKind::RParen) {
                        binders.push(self.expect_ident("a binder name in pattern")?);
                        if !self.check(&TokenKind::RParen) {
                            self.expect(&TokenKind::Comma, "expected `,` between binders")?;
                        }
                    }
                    self.advance(); // )
                    return Ok(Pattern::Ctor(name, binders));
                }
                // Capitalized bare name = variant with no payload; else a binding.
                if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    Ok(Pattern::Ctor(name, vec![]))
                } else {
                    Ok(Pattern::Bind(name))
                }
            }
            TokenKind::Int(v) => {
                self.advance();
                Ok(Pattern::LiteralInt(v))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(Pattern::LiteralStr(s))
            }
            TokenKind::True => {
                self.advance();
                Ok(Pattern::LiteralBool(true))
            }
            TokenKind::False => {
                self.advance();
                Ok(Pattern::LiteralBool(false))
            }
            other => Err(
                SyntaxError::new(format!("expected a pattern, found `{other}`"), span)
                    .with_hint("patterns: `Ok(x)`, `Uncertain(reason)`, a literal, a name, or `_`"),
            ),
        }
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
                    let mut kwargs = Vec::new();
                    self.skip_newlines_in_brackets();
                    while !self.check(&TokenKind::RParen) {
                        // `name = value` is a keyword argument; anything else
                        // is positional.
                        let is_kwarg = matches!(self.peek_kind(), TokenKind::Ident(_))
                            && self.peek_next_is(&TokenKind::Eq);
                        if is_kwarg {
                            let name = self.expect_ident("a keyword argument name")?;
                            self.advance(); // =
                            kwargs.push((name, self.expression()?));
                        } else {
                            if !kwargs.is_empty() {
                                return Err(SyntaxError::new(
                                    "positional arguments cannot follow keyword arguments",
                                    self.peek_span(),
                                ));
                            }
                            args.push(self.expression()?);
                        }
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
                            kwargs,
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
    fn type_fields_accept_indented_and_inline_metadata() {
        let p = ok(
            "type Expense:\n    merchant: str\n        description: \"Merchant identifier\"\n        pattern: \"^[A-Za-z0-9]{12}$\"\n    category: str @description(\"Expense category\")\n",
        );
        let StmtKind::TypeDef { fields, .. } = &p.items[0].kind else {
            panic!("expected type definition");
        };
        assert_eq!(
            fields[0].metadata.description.as_deref(),
            Some("Merchant identifier")
        );
        assert_eq!(
            fields[0].metadata.pattern.as_deref(),
            Some("^[A-Za-z0-9]{12}$")
        );
        assert_eq!(
            fields[1].metadata.description.as_deref(),
            Some("Expense category")
        );
    }

    #[test]
    fn type_field_metadata_rejects_unknown_names() {
        let err = parse("type E:\n    name: str @example(\"Ada\")\n").unwrap_err();
        assert!(err.message.contains("unknown field metadata `example`"));
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
    fn file_import_parses_with_an_alias() {
        let p = ok("use \"./lib/tax.ko\" as tax\n");
        match &p.items[0].kind {
            StmtKind::UseFile { path, alias } => {
                assert_eq!(path, "./lib/tax.ko");
                assert_eq!(alias, "tax");
            }
            other => panic!("expected a file import, got {other:?}"),
        }
    }

    #[test]
    fn file_import_without_a_name_is_an_error_with_the_fix() {
        let err = parse("use \"./lib/tax.ko\"\n").unwrap_err();
        assert!(err.message.contains("needs a name"), "{}", err.message);
        assert!(err.hint.unwrap().contains("as <name>"));
    }

    #[test]
    fn a_bare_module_name_is_still_a_stdlib_import() {
        let p = ok("use json as j\n");
        assert!(matches!(p.items[0].kind, StmtKind::Use { .. }));
    }

    #[test]
    fn package_import_binds_its_own_name_without_as() {
        let p = ok("use pkg receipts\n");
        let StmtKind::UsePkg { package, alias } = &p.items[0].kind else {
            panic!("expected a package import, got {:?}", p.items[0].kind);
        };
        assert_eq!(package, "receipts");
        assert_eq!(alias, "receipts");
    }

    #[test]
    fn package_import_takes_an_alias() {
        let p = ok("use pkg receipts as r\n");
        let StmtKind::UsePkg { package, alias } = &p.items[0].kind else {
            panic!("expected a package import");
        };
        assert_eq!(package, "receipts");
        assert_eq!(alias, "r");
    }

    #[test]
    fn package_import_parses_inside_a_function_body() {
        // `use` is an ordinary statement, so a package name can appear
        // anywhere. The resolver relies on finding every one of them.
        let p = ok("def f():\n    use pkg receipts as r\n    return 1\n");
        assert_eq!(p.items.len(), 1);
    }

    #[test]
    fn pkg_is_not_a_reserved_word() {
        // `pkg` is matched in the `use` position only, so it stays usable
        // as an ordinary name everywhere else.
        let p = ok("pkg = 1\n");
        assert_eq!(p.items.len(), 1);
    }

    #[test]
    fn error_on_bad_assign_target() {
        let err = parse("1 + 2 = 3\n").unwrap_err();
        assert!(err.message.contains("cannot assign"));
    }
}
