//! A fast AST pre-filter that rejects obviously unsafe candidates before any
//! process starts.
//!
//! This is NOT the security boundary. Python has many ways to reach the same
//! capabilities; the sandbox runner is what contains a candidate. The list only
//! saves a sandbox run on candidates that are clearly out of bounds.

use std::fmt;

use rustpython_ast::{self as ast, Visitor};
use rustpython_parser::{parse, Mode};

/// Visitor methods that `rustpython_ast::Visitor` leaves as no-ops but that
/// hold expressions: comprehension clauses, keyword arguments, parameter
/// defaults and annotations, `with` items, and `match` cases. Every visitor
/// in aco includes this, so nothing hides in those positions.
#[macro_export]
macro_rules! visit_everything {
    () => {
        fn visit_comprehension(&mut self, c: rustpython_ast::Comprehension) {
            self.visit_expr(c.iter);
            self.visit_expr(c.target);
            for e in c.ifs {
                self.visit_expr(e);
            }
        }
        $crate::visit_everything!(except_comprehension);
    };
    (except_comprehension) => {
        fn visit_keyword(&mut self, k: rustpython_ast::Keyword) {
            self.visit_expr(k.value);
        }
        fn visit_arguments(&mut self, a: rustpython_ast::Arguments) {
            for arg in a.posonlyargs.into_iter().chain(a.args).chain(a.kwonlyargs) {
                if let Some(d) = arg.default {
                    self.visit_expr(*d);
                }
                if let Some(an) = arg.def.annotation {
                    self.visit_expr(*an);
                }
            }
            for v in [a.vararg, a.kwarg].into_iter().flatten() {
                if let Some(an) = v.annotation {
                    self.visit_expr(*an);
                }
            }
        }
        fn visit_withitem(&mut self, w: rustpython_ast::WithItem) {
            self.visit_expr(w.context_expr);
            if let Some(v) = w.optional_vars {
                self.visit_expr(*v);
            }
        }
        fn visit_match_case(&mut self, m: rustpython_ast::MatchCase) {
            self.visit_pattern(m.pattern);
            if let Some(g) = m.guard {
                self.visit_expr(*g);
            }
            for s in m.body {
                self.visit_stmt(s);
            }
        }
    };
}

/// Builtins that are rejected whenever they are referenced by name.
pub const BANNED_NAMES: &[&str] = &[
    "exec",
    "eval",
    "compile",
    "open",
    "__import__",
    "globals",
    "locals",
    "vars",
    "getattr",
    "setattr",
    "delattr",
    "breakpoint",
];

/// Top-level modules that may not be imported.
pub const BANNED_MODULES: &[&str] = &[
    "os",
    "subprocess",
    "socket",
    "ctypes",
    "importlib",
    "pathlib",
    "shutil",
    "builtins",
    "gc",
    "inspect",
    "io",
    "pickle",
    "marshal",
    "multiprocessing",
    "threading",
    "signal",
    "tempfile",
    "urllib",
    "http",
];

/// The parts of `sys` a script may use: its arguments, standard streams,
/// exit, and read-only facts. `sys` itself may not be passed around or used
/// for anything else (`sys.modules`, `sys._getframe`, ...).
pub const SYS_ALLOWED: &[&str] = &[
    "argv",
    "stdin",
    "stdout",
    "stderr",
    "exit",
    "setrecursionlimit",
    "getrecursionlimit",
    "maxsize",
    "version_info",
    "float_info",
    "int_info",
    "byteorder",
    "set_int_max_str_digits",
    "get_int_max_str_digits",
];

/// Dunder names that ordinary scripts use and that grant nothing.
const ALLOWED_DUNDER_NAMES: &[&str] = &["__name__", "__all__", "__slots__", "__version__"];

/// Data-model methods a class may define or call (`super().__init__()`).
/// Introspection hooks (`__class__`, `__dict__`, `__globals__`,
/// `__subclasses__`, `__getattribute__`, `__reduce__`, `__del__`, ...) stay
/// banned.
const ALLOWED_DUNDER_METHODS: &[&str] = &[
    "__init__",
    "__post_init__",
    "__repr__",
    "__str__",
    "__format__",
    "__hash__",
    "__bool__",
    "__eq__",
    "__ne__",
    "__lt__",
    "__le__",
    "__gt__",
    "__ge__",
    "__len__",
    "__iter__",
    "__next__",
    "__reversed__",
    "__contains__",
    "__getitem__",
    "__setitem__",
    "__delitem__",
    "__missing__",
    "__enter__",
    "__exit__",
    "__call__",
    "__add__",
    "__radd__",
    "__iadd__",
    "__sub__",
    "__rsub__",
    "__isub__",
    "__mul__",
    "__rmul__",
    "__imul__",
    "__truediv__",
    "__rtruediv__",
    "__floordiv__",
    "__rfloordiv__",
    "__mod__",
    "__rmod__",
    "__pow__",
    "__rpow__",
    "__matmul__",
    "__neg__",
    "__pos__",
    "__abs__",
    "__invert__",
    "__and__",
    "__rand__",
    "__or__",
    "__ror__",
    "__xor__",
    "__rxor__",
    "__lshift__",
    "__rshift__",
    "__int__",
    "__float__",
    "__index__",
    "__round__",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    Syntax(String),
    BannedName(String),
    DunderName(String),
    DunderAttribute(String),
    BannedImport(String),
    /// `sys` used beyond [`SYS_ALLOWED`].
    SysUse(String),
}

/// A violation and the 1-based line it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub violation: Violation,
    pub line: usize,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Syntax(e) => write!(f, "does not parse: {e}"),
            Violation::BannedName(n) => write!(f, "uses banned builtin `{n}`"),
            Violation::DunderName(n) => write!(f, "uses dunder name `{n}`"),
            Violation::DunderAttribute(n) => write!(f, "accesses dunder attribute `.{n}`"),
            Violation::BannedImport(m) => write!(f, "imports banned module `{m}`"),
            Violation::SysUse(a) => write!(
                f,
                "uses `{a}`; only sys.{} are allowed",
                SYS_ALLOWED.join(", sys.")
            ),
        }
    }
}

/// Parse a module and return its top-level statements.
pub fn parse_module(source: &str) -> Result<Vec<ast::Stmt>, String> {
    match parse(source, Mode::Module, "<candidate>") {
        Ok(ast::Mod::Module(m)) => Ok(m.body),
        Ok(_) => Err("not a module".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// A syntax error with a 1-based line and column (in characters).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    /// The offending source line, without its newline.
    pub text: String,
}

/// Parse `source` and, if it fails, say where.
pub fn syntax_error(source: &str) -> Option<SyntaxError> {
    let e = parse(source, Mode::Module, "<input>").err()?;
    let offset = (u32::from(e.offset) as usize).min(source.len());
    let offset = (0..=offset)
        .rev()
        .find(|i| source.is_char_boundary(*i))
        .unwrap_or(0);
    // An error at end of input points past the last line; show the last
    // line that has code instead of an empty one.
    let trimmed = source.trim_end().len();
    let offset = if offset > trimmed { trimmed } else { offset };
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let column = source[line_start..offset].chars().count() + 1;
    let text = source[line_start..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Some(SyntaxError {
        message: e.error.to_string(),
        line,
        column,
        text,
    })
}

/// True when the module has no statements (empty, blank or comments only).
pub fn is_empty_module(source: &str) -> bool {
    matches!(parse_module(source), Ok(body) if body.is_empty())
}

/// Parse and pre-filter `source`.
pub fn check(source: &str) -> Result<(), Violation> {
    check_located(source).map_err(|f| f.violation)
}

/// Like [`check`], also saying which line the first violation is on.
pub fn check_located(source: &str) -> Result<(), Finding> {
    let body = match parse_module(source) {
        Ok(b) => b,
        Err(e) => {
            let line = syntax_error(source).map(|s| s.line).unwrap_or(1);
            return Err(Finding {
                violation: Violation::Syntax(e),
                line,
            });
        }
    };
    check_ast_located(&body, source)
}

/// Pre-filter an already parsed module.
pub fn check_ast(body: &[ast::Stmt]) -> Result<(), Violation> {
    check_ast_located(body, "").map_err(|f| f.violation)
}

/// Pre-filter a parsed module; `source` (may be empty) maps offsets to lines.
pub fn check_ast_located(body: &[ast::Stmt], source: &str) -> Result<(), Finding> {
    let mut aliases = SysAliases::default();
    for stmt in body {
        aliases.visit_stmt(stmt.clone());
    }
    let mut v = PolicyVisitor {
        found: None,
        source,
        sys: aliases.names,
    };
    for stmt in body {
        if v.found.is_some() {
            break;
        }
        v.visit_stmt(stmt.clone());
    }
    match v.found {
        Some(f) => Err(f),
        None => Ok(()),
    }
}

/// 1-based line of byte `offset` in `source`.
pub fn line_of(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    source.as_bytes()[..offset]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

fn top_module(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

struct PolicyVisitor<'s> {
    found: Option<Finding>,
    source: &'s str,
    /// Names bound to the `sys` module (`import sys`, `import sys as s`).
    sys: Vec<String>,
}

/// Collects every name `sys` is imported as, anywhere in the module.
#[derive(Default)]
struct SysAliases {
    names: Vec<String>,
}

impl Visitor for SysAliases {
    crate::visit_everything!();
    fn visit_stmt_import(&mut self, node: ast::StmtImport) {
        for a in &node.names {
            if a.name.as_str() == "sys" {
                let n = a.asname.as_ref().unwrap_or(&a.name).to_string();
                self.names.push(n);
            }
        }
    }
}

impl PolicyVisitor<'_> {
    fn flag(&mut self, v: Violation, at: ast::text_size::TextSize) {
        if self.found.is_none() {
            self.found = Some(Finding {
                violation: v,
                line: line_of(self.source, u32::from(at) as usize),
            });
        }
    }

    fn check_sys_import(&mut self, name: &str, at: ast::text_size::TextSize) {
        // `import sys.x` does not exist; `import sys` is checked at each use.
        if name != "sys" && top_module(name) == "sys" {
            self.flag(Violation::SysUse(name.to_string()), at);
        }
    }

    fn check_module(&mut self, name: &str, at: ast::text_size::TextSize) {
        let top = top_module(name);
        if BANNED_MODULES.contains(&top) {
            self.flag(Violation::BannedImport(top.to_string()), at);
        }
    }
}

impl Visitor for PolicyVisitor<'_> {
    crate::visit_everything!();

    fn visit_expr_name(&mut self, node: ast::ExprName) {
        let id = node.id.as_str();
        let at = node.range.start();
        if self.sys.iter().any(|s| s == id) {
            // Reached only when not the base of an allowed `sys.attr`.
            self.flag(Violation::SysUse(id.to_string()), at);
        } else if BANNED_NAMES.contains(&id) {
            self.flag(Violation::BannedName(id.to_string()), at);
        } else if is_dunder(id) && !ALLOWED_DUNDER_NAMES.contains(&id) {
            self.flag(Violation::DunderName(id.to_string()), at);
        }
    }

    fn visit_expr_attribute(&mut self, node: ast::ExprAttribute) {
        let attr = node.attr.as_str();
        if let ast::Expr::Name(base) = node.value.as_ref() {
            if self.sys.iter().any(|s| s == base.id.as_str()) {
                if !SYS_ALLOWED.contains(&attr) {
                    self.flag(
                        Violation::SysUse(format!("{}.{attr}", base.id)),
                        node.range.start(),
                    );
                }
                return;
            }
        }
        if is_dunder(attr) && !ALLOWED_DUNDER_METHODS.contains(&attr) {
            self.flag(
                Violation::DunderAttribute(attr.to_string()),
                node.range.start(),
            );
        }
        self.generic_visit_expr_attribute(node);
    }

    fn visit_stmt_import(&mut self, node: ast::StmtImport) {
        for alias in &node.names {
            self.check_module(alias.name.as_str(), node.range.start());
            self.check_sys_import(alias.name.as_str(), node.range.start());
        }
    }

    fn visit_stmt_import_from(&mut self, node: ast::StmtImportFrom) {
        // Relative imports are resolved (and checked) by project analysis.
        if let Some(module) = &node.module {
            if node.level.map(|l| l.to_u32()).unwrap_or(0) == 0 {
                self.check_module(module.as_str(), node.range.start());
                if top_module(module.as_str()) == "sys" {
                    for alias in &node.names {
                        let n = alias.name.as_str();
                        if module.as_str() != "sys" || !SYS_ALLOWED.contains(&n) {
                            self.flag(
                                Violation::SysUse(format!("{module}.{n}")),
                                node.range.start(),
                            );
                        }
                    }
                }
            }
        }
        for alias in &node.names {
            if is_dunder(alias.name.as_str()) {
                self.flag(
                    Violation::DunderName(alias.name.to_string()),
                    node.range.start(),
                );
            }
        }
    }

    fn visit_stmt_function_def(&mut self, node: ast::StmtFunctionDef) {
        let name = node.name.as_str();
        if is_dunder(name) && !ALLOWED_DUNDER_METHODS.contains(&name) {
            self.flag(Violation::DunderName(name.to_string()), node.range.start());
        }
        self.generic_visit_stmt_function_def(node);
    }
}
