//! A fast AST pre-filter that rejects obviously unsafe candidates before any
//! process starts.
//!
//! This is NOT the security boundary. Python has many ways to reach the same
//! capabilities; the sandbox runner is what contains a candidate. The list only
//! saves a sandbox run on candidates that are clearly out of bounds.

use std::fmt;

use rustpython_ast::{self as ast, Visitor};
use rustpython_parser::{parse, Mode};

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
    "sys",
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

/// Dunder names that ordinary scripts use and that grant nothing.
const ALLOWED_DUNDER_NAMES: &[&str] = &["__name__"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    Syntax(String),
    BannedName(String),
    DunderName(String),
    DunderAttribute(String),
    BannedImport(String),
    RelativeImport,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Syntax(e) => write!(f, "does not parse: {e}"),
            Violation::BannedName(n) => write!(f, "uses banned builtin `{n}`"),
            Violation::DunderName(n) => write!(f, "uses dunder name `{n}`"),
            Violation::DunderAttribute(n) => write!(f, "accesses dunder attribute `.{n}`"),
            Violation::BannedImport(m) => write!(f, "imports banned module `{m}`"),
            Violation::RelativeImport => write!(f, "uses a relative import"),
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

/// Parse and pre-filter `source`.
pub fn check(source: &str) -> Result<(), Violation> {
    let body = parse_module(source).map_err(Violation::Syntax)?;
    check_ast(&body)
}

/// Pre-filter an already parsed module.
pub fn check_ast(body: &[ast::Stmt]) -> Result<(), Violation> {
    let mut v = PolicyVisitor { found: None };
    for stmt in body {
        if v.found.is_some() {
            break;
        }
        v.visit_stmt(stmt.clone());
    }
    match v.found {
        Some(violation) => Err(violation),
        None => Ok(()),
    }
}

fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

fn top_module(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

struct PolicyVisitor {
    found: Option<Violation>,
}

impl PolicyVisitor {
    fn flag(&mut self, v: Violation) {
        if self.found.is_none() {
            self.found = Some(v);
        }
    }

    fn check_module(&mut self, name: &str) {
        let top = top_module(name);
        if BANNED_MODULES.contains(&top) {
            self.flag(Violation::BannedImport(top.to_string()));
        }
    }
}

impl Visitor for PolicyVisitor {
    fn visit_expr_name(&mut self, node: ast::ExprName) {
        let id = node.id.as_str();
        if BANNED_NAMES.contains(&id) {
            self.flag(Violation::BannedName(id.to_string()));
        } else if is_dunder(id) && !ALLOWED_DUNDER_NAMES.contains(&id) {
            self.flag(Violation::DunderName(id.to_string()));
        }
    }

    fn visit_expr_attribute(&mut self, node: ast::ExprAttribute) {
        if is_dunder(node.attr.as_str()) {
            self.flag(Violation::DunderAttribute(node.attr.to_string()));
        }
        self.generic_visit_expr_attribute(node);
    }

    fn visit_stmt_import(&mut self, node: ast::StmtImport) {
        for alias in &node.names {
            self.check_module(alias.name.as_str());
        }
    }

    fn visit_stmt_import_from(&mut self, node: ast::StmtImportFrom) {
        let relative = node.level.map(|l| l.to_u32() > 0).unwrap_or(false);
        if relative {
            self.flag(Violation::RelativeImport);
        }
        if let Some(module) = &node.module {
            self.check_module(module.as_str());
        }
        for alias in &node.names {
            if is_dunder(alias.name.as_str()) {
                self.flag(Violation::DunderName(alias.name.to_string()));
            }
        }
    }

    fn visit_stmt_function_def(&mut self, node: ast::StmtFunctionDef) {
        if is_dunder(node.name.as_str()) {
            self.flag(Violation::DunderName(node.name.to_string()));
        }
        self.generic_visit_stmt_function_def(node);
    }
}
