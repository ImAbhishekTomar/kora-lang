//! Abstract syntax tree for Kora.

use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub items: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `x = expr` / `x: Type = expr` / `obj.field = expr` / `xs[i] = expr`
    Assign {
        target: Expr,
        ty: Option<TypeExpr>,
        value: Expr,
        /// Declared with `classified`, so the bound value carries the label.
        classified: bool,
        /// `answer: str = analyze(...) on token(t):` — a block run for each
        /// piece of the answer as the model writes it.
        ///
        /// It hangs off the assignment rather than replacing it because the
        /// call still returns an outcome to match on. Watching a stream and
        /// getting an answer are two things one call does, not two calls.
        on_token: Option<TokenHandler>,
        /// `result: T = analyze(..., tools=[...]) on tool_call(name, args):`
        /// — a block run before each tool call the model asks for.
        ///
        /// The same reasoning as `on_token` applies: it hangs off the
        /// assignment rather than becoming a separate call, because the
        /// tool loop still produces one outcome to match on. Mutually
        /// exclusive with `on_token` — a call streams prose or watches its
        /// own tool calls, not both, so the parser accepts at most one.
        on_tool_call: Option<ToolCallHandler>,
        /// `answer: str = analyze(...) stream` writes text pieces to standard
        /// output with the safe default renderer. It is shorthand for the
        /// common `on token(piece): write(piece)` handler, including closing
        /// the line when the stream reaches a terminal outcome.
        stream: bool,
    },
    /// `x += expr` and friends (desugared op stored explicitly)
    AugAssign {
        target: Expr,
        op: BinOp,
        value: Expr,
    },
    /// A bare expression statement, e.g. a call: `print(x)`
    Expr(Expr),
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    FuncDef(FuncDef),
    /// `type Name:` block with typed fields
    TypeDef {
        name: String,
        fields: Vec<FieldDef>,
    },
    Return(Option<Expr>),
    /// `parallel for x in xs:` — fan out across threads, collect results.
    ParallelFor {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
        /// Name bound to the result list, when written as
        /// `results = parallel for ...`.
        collect_into: Option<String>,
    },
    /// `use json` / `use json as j` — bring a stdlib module into scope.
    Use {
        module: String,
        alias: String,
    },
    /// `use "./lib/tax.ko" as tax` — bring another Kora file into scope.
    ///
    /// The path is a string literal so it can never be confused with a
    /// stdlib module name, and it is resolved relative to the file that
    /// writes it, so a program moves as a directory.
    UseFile {
        /// The path exactly as written, before resolution.
        path: String,
        alias: String,
    },
    /// `use python statistics as stats` — call into a Python module.
    ///
    /// Data in, data out: there are no live Python objects on this side.
    UsePython {
        module: String,
        alias: String,
    },
    /// `use pkg receipts as r` — bring a dependency into scope.
    ///
    /// The name is resolved against the `[dependencies]` table of the
    /// package that wrote it, never a global one, so two packages may
    /// bind the same bare name to different sources.
    UsePkg {
        package: String,
        alias: String,
    },
    /// `use mcp github as gh` — connect to a configured MCP server.
    ///
    /// The program names *which* server; how to launch it (command, args,
    /// credentials) lives in kora.toml, so secrets stay out of source.
    UseMcp {
        server: String,
        alias: String,
    },
    /// `test "name":` — a test case, collected by `kora test`.
    Test {
        name: String,
        body: Vec<Stmt>,
    },
    /// `assert <expr>` / `assert <expr>, "message"`
    Assert {
        condition: Expr,
        message: Option<Expr>,
    },
    /// `with mock analyze -> Ok(...):` — replace model calls inside the block.
    ///
    /// Mocks are checked against the declared result type, so one returning
    /// the wrong shape is an error rather than a passing test.
    WithMock {
        /// What is being replaced; only `analyze` today.
        target: String,
        /// The value model calls should produce.
        result: Expr,
        body: Vec<Stmt>,
    },
    /// `declassify <expr> for <sink>:` — a bounded region in which a
    /// classified value may reach one named sink. Scoped on purpose: the
    /// exposure is the block, not the rest of the program.
    Declassify {
        /// The value being declassified.
        value: Expr,
        /// Name it is bound to inside the block (defaults to the expression
        /// text when written as `declassify x for sink:`).
        binding: String,
        /// Where it is allowed to flow, e.g. `local_model`.
        sink: String,
        body: Vec<Stmt>,
    },
    /// `with budget(max_tokens = N):` — a nested spending fence.
    WithBudget {
        budget: BudgetSpec,
        body: Vec<Stmt>,
    },
    /// `with context(max_input_tokens = N):` — a scoped context-window fence.
    WithContext {
        context: ContextSpec,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Pass,
    /// `match expr:` with `case Pattern:` arms
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
    },
    /// `x: T = <outcome> else:` — bind the payload of a successful outcome,
    /// or run the else block.
    ///
    /// This exists because chained model calls otherwise nest one level per
    /// call: every `analyze` returns `Ok` / `Uncertain` / `Exhausted`, and
    /// handling each with a full `match` buries the happy path. Here the
    /// happy path stays at the statement's own indentation and the failure
    /// path is the exception.
    ///
    /// The else block must diverge (`return`, `break`, or `continue`), so
    /// `x` is always bound below the statement. That is checked, not assumed.
    BindOrElse {
        /// Name bound to the *payload*, not the outcome.
        name: String,
        /// Declared payload type, which `analyze` also uses as its schema.
        ty: Option<TypeExpr>,
        value: Expr,
        /// Declared with `classified`, so the bound payload carries the label.
        classified: bool,
        /// `else (why):` binds the reason the outcome was not successful.
        reason: Option<String>,
        /// `else (why, kind):` also binds the stable lower-case outcome tag,
        /// such as `uncertain`, `exhausted`, or `failed`.
        status: Option<String>,
        /// The concise terminal streaming modifier on the wrapped analyze
        /// call, e.g. `answer: str = analyze(...) stream else:`.
        stream: bool,
        else_body: Vec<Stmt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// `case Ok(j) if j.has_punchline:` — an extra condition, evaluated with
    /// the pattern's binders already in scope but before the body runs.
    ///
    /// A guard is deliberately restricted to pure expressions: it may be
    /// evaluated for arms that do not end up running, and a condition that
    /// could spend budget or call a model would make the cost of a `match`
    /// depend on how many arms were tried. See `guard_is_pure`.
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// Patterns for `case` arms. Deliberately small for now.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `case _:`
    Wildcard,
    /// `case name:` -- binds the whole subject
    Bind(String),
    /// `case Ok(x)` / `case Uncertain(reason)` -- variant with binders
    Ctor(String, Vec<String>),
    /// `case 3:` / `case "high":` / `case True:`
    LiteralInt(i64),
    LiteralStr(String),
    LiteralBool(bool),
    /// `case A(x) | B(x):` — alternatives with identical binders.
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<Param>,
    pub return_ty: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    /// What kind of callable this is.
    pub kind: FuncKind,
    /// `budget:` line declared at the top of an agent or function body.
    pub budget: Option<BudgetSpec>,
    /// Leading docstring, used as the tool description sent to models.
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuncKind {
    /// `def` — ordinary deterministic function.
    Def,
    /// `agent` — may call models and suspend; carries a budget.
    Agent,
    /// `tool` — exposed to models; signature becomes a schema.
    Tool,
}

/// `budget: max_tokens = 20_000, max_calls = 5`
///
/// Token-denominated by decision (DECISIONS.md): tokens are what the runtime
/// can measure directly, money is a display layer only.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BudgetSpec {
    pub max_tokens: Option<u64>,
    pub max_calls: Option<u64>,
    pub max_steps: Option<u64>,
    /// Wall-clock seconds the scope may spend before its next model call is
    /// refused. The one meter that is not counted from the program's own
    /// spending, which is why it is the one meter a durable run has to
    /// journal rather than recompute.
    pub max_seconds: Option<u64>,
    pub span_line: u32,
}

/// `with context(max_input_tokens = 12_000, reserve_output_tokens = 1_000):`
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextSpec {
    pub max_input_tokens: Option<u64>,
    pub reserve_output_tokens: Option<u64>,
    pub span_line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

/// The `on token(t):` block of a streaming `analyze()` call.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenHandler {
    /// Name bound to each piece of the answer inside the block.
    pub var: String,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// The `on tool_call(name, args):` block of an `analyze(..., tools=[...])`
/// call. Runs once before each tool the model asks for, with `name` bound
/// to the tool's name and `args` to its arguments as a mutable `dict` --
/// editing `args` in place rewrites what the tool actually receives. A
/// `return <str>` inside the block skips running the tool and hands that
/// string back to the model as the tool's result instead.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallHandler {
    pub name_var: String,
    pub args_var: String,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
    /// `classified` marker on the field: values read from it carry the label.
    pub classified: bool,
    /// Human-readable guidance and executable constraints for this field.
    pub metadata: FieldMetadata,
}

/// Native field metadata. Both indented metadata and `@name(...)` syntax
/// populate this structure, so they have identical behavior.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldMetadata {
    pub description: Option<String>,
    pub pattern: Option<String>,
}

/// Type annotations. Kept simple for Phase 1; grows with the checker.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// `int`, `str`, `Expense`, ...
    Name(String),
    /// `list[int]`, `dict[str, int]`
    Generic(String, Vec<TypeExpr>),
}

impl TypeExpr {
    pub fn display(&self) -> String {
        match self {
            TypeExpr::Name(n) => n.clone(),
            TypeExpr::Generic(n, args) => {
                let inner: Vec<String> = args.iter().map(|a| a.display()).collect();
                format!("{n}[{}]", inner.join(", "))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    /// f-string with parsed hole expressions.
    FString {
        parts: Vec<String>,
        exprs: Vec<Expr>,
    },
    Bool(bool),
    None,
    Name(String),
    List(Vec<Expr>),
    /// Dict literal; keys are expressions (usually strings).
    Dict(Vec<(Expr, Expr)>),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// Keyword arguments, e.g. `analyze(data, prompt, tools=[t])`.
        kwargs: Vec<(String, Expr)>,
    },
    /// `obj.attr`
    Attr {
        object: Box<Expr>,
        name: String,
    },
    /// `xs[i]` or `xs[a:b]` slices
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        stop: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    In,
    NotIn,
}

impl BinOp {
    pub fn symbol(&self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            FloorDiv => "//",
            Mod => "%",
            Pow => "**",
            Eq => "==",
            NotEq => "!=",
            Lt => "<",
            Gt => ">",
            LtEq => "<=",
            GtEq => ">=",
            And => "and",
            Or => "or",
            In => "in",
            NotIn => "not in",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}
