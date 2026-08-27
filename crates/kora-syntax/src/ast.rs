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
    Break,
    Continue,
    Pass,
    /// `match expr:` with `case Pattern:` arms
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<Param>,
    pub return_ty: Option<TypeExpr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
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
