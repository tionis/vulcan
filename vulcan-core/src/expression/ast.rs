#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Regex {
        pattern: String,
        flags: String,
    },

    /// Bare identifier — property name like `status`, `due_date`, `my-property`
    Identifier(String),

    /// Dot access — `expr.field` (e.g., `file.name`, `date_prop.year`)
    FieldAccess(Box<Expr>, String),

    /// Bracket access / indexing — `expr[key]`, `array[0]`
    IndexAccess(Box<Expr>, Box<Expr>),

    /// Reference to another formula — `formula.some_name`
    FormulaRef(String),

    /// Binary operation — `a + b`, `a == b`, `a && b`
    BinaryOp(Box<Expr>, BinOp, Box<Expr>),

    /// Unary operation — `-x`, `!x`
    UnaryOp(UnOp, Box<Expr>),

    /// Global function call — `if(cond, a, b)`, `now()`
    FunctionCall(String, Vec<Expr>),

    /// Method call — `expr.method(args...)`
    MethodCall(Box<Expr>, String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}
