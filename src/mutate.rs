//! Deterministic rule mutators.
//!
//! Each rule recognises one slow pattern in the AST and rewrites it by
//! splicing the original source text, so untouched code keeps its formatting.
//! Rules fire only on patterns they recognise and only emit source that passes
//! the policy pre-filter. A rule is a guess: the search measures every rewrite
//! and discards the ones that are not faster.

use std::collections::HashSet;

use rustpython_ast::{self as ast, CmpOp, Expr, ExprContext, Operator, Ranged, Stmt, Visitor};
use rustpython_parser::text_size::TextRange;

use crate::policy;

/// One rewritten file and the rule variant that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub rule: &'static str,
    pub source: String,
}

/// All rule rewrites of `source`. Each rule rewrites its first match only; the
/// search feeds the elite back in, so later matches are handled next generation.
pub fn rewrites(source: &str) -> Vec<Rewrite> {
    let Ok(body) = policy::parse_module(source) else {
        return Vec::new();
    };
    let used = all_names(&body);
    let mut out = Vec::new();
    out.extend(bubble_sort(source, &body));
    out.extend(concat_join(source, &body, &used));
    out.extend(set_membership(source, &body, &used));
    out.retain(|r| r.source != source && policy::check(&r.source).is_ok());
    out
}

// ---------------------------------------------------------------- helpers

fn span(r: TextRange) -> (usize, usize) {
    (usize::from(r.start()), usize::from(r.end()))
}

fn text(src: &str, r: TextRange) -> &str {
    let (s, e) = span(r);
    &src[s..e]
}

fn indent_at(src: &str, offset: usize) -> String {
    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    src[line_start..offset]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

fn splice(src: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = src.to_string();
    for (s, e, with) in edits {
        out.replace_range(s..e, &with);
    }
    out
}

fn name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        _ => None,
    }
}

fn fresh(used: &HashSet<String>, base: &str) -> String {
    let mut name = base.to_string();
    let mut i = 2;
    while used.contains(&name) {
        name = format!("{base}{i}");
        i += 1;
    }
    name
}

/// Every statement list in the module, outermost first.
fn blocks<'a>(body: &'a [Stmt], out: &mut Vec<&'a [Stmt]>) {
    out.push(body);
    for s in body {
        match s {
            Stmt::FunctionDef(f) => blocks(&f.body, out),
            Stmt::AsyncFunctionDef(f) => blocks(&f.body, out),
            Stmt::ClassDef(c) => blocks(&c.body, out),
            Stmt::For(f) => {
                blocks(&f.body, out);
                blocks(&f.orelse, out);
            }
            Stmt::AsyncFor(f) => {
                blocks(&f.body, out);
                blocks(&f.orelse, out);
            }
            Stmt::While(w) => {
                blocks(&w.body, out);
                blocks(&w.orelse, out);
            }
            Stmt::If(i) => {
                blocks(&i.body, out);
                blocks(&i.orelse, out);
            }
            Stmt::With(w) => blocks(&w.body, out),
            Stmt::Try(t) => {
                blocks(&t.body, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    blocks(&h.body, out);
                }
                blocks(&t.orelse, out);
                blocks(&t.finalbody, out);
            }
            _ => {}
        }
    }
}

fn all_blocks(body: &[Stmt]) -> Vec<&[Stmt]> {
    let mut out = Vec::new();
    blocks(body, &mut out);
    out
}

struct NameUse {
    id: String,
    store: bool,
}

struct Aug {
    target: String,
    add: bool,
    range: TextRange,
    value: TextRange,
}

/// Names, `+=` statements, and `x in NAME` tests inside one subtree.
#[derive(Default)]
struct Facts {
    names: Vec<NameUse>,
    augs: Vec<Aug>,
    member_tests: Vec<(String, TextRange)>,
}

impl Visitor for Facts {
    fn visit_expr_name(&mut self, n: ast::ExprName) {
        self.names.push(NameUse {
            id: n.id.to_string(),
            store: !matches!(n.ctx, ExprContext::Load),
        });
    }

    fn visit_stmt_aug_assign(&mut self, n: ast::StmtAugAssign) {
        if let Some(target) = name_of(&n.target) {
            self.augs.push(Aug {
                target,
                add: matches!(n.op, Operator::Add),
                range: n.range,
                value: n.value.range(),
            });
        }
        self.generic_visit_stmt_aug_assign(n);
    }

    fn visit_expr_compare(&mut self, n: ast::ExprCompare) {
        if n.ops.len() == 1 && matches!(n.ops[0], CmpOp::In | CmpOp::NotIn) {
            if let Expr::Name(c) = &n.comparators[0] {
                self.member_tests.push((c.id.to_string(), c.range));
            }
        }
        self.generic_visit_expr_compare(n);
    }
}

fn facts(stmt: &Stmt) -> Facts {
    let mut f = Facts::default();
    f.visit_stmt(stmt.clone());
    f
}

fn all_names(body: &[Stmt]) -> HashSet<String> {
    let mut f = Facts::default();
    for s in body {
        f.visit_stmt(s.clone());
    }
    f.names.into_iter().map(|n| n.id).collect()
}

fn is_loop(s: &Stmt) -> bool {
    matches!(s, Stmt::For(_) | Stmt::While(_))
}

fn single_name_target(s: &Stmt) -> Option<(String, &Expr)> {
    let Stmt::Assign(a) = s else { return None };
    if a.targets.len() != 1 {
        return None;
    }
    Some((name_of(&a.targets[0])?, a.value.as_ref()))
}

// ---------------------------------------------------------------- rules

/// `NAME[index]` with whitespace-normalised index text.
fn subscript(src: &str, e: &Expr) -> Option<(String, String)> {
    let Expr::Subscript(s) = e else { return None };
    let base = name_of(&s.value)?;
    let index: String = text(src, s.slice.range()).split_whitespace().collect();
    Some((base, index))
}

/// Nested compare-and-swap over a list becomes a call to `sorted`.
fn bubble_sort(src: &str, body: &[Stmt]) -> Vec<Rewrite> {
    for block in all_blocks(body) {
        for stmt in block {
            if let Some(r) = bubble_at(src, stmt) {
                return r;
            }
        }
    }
    Vec::new()
}

fn bubble_at(src: &str, stmt: &Stmt) -> Option<Vec<Rewrite>> {
    let Stmt::For(outer) = stmt else { return None };
    if !outer.orelse.is_empty() || outer.body.len() != 1 {
        return None;
    }
    let Stmt::For(inner) = &outer.body[0] else {
        return None;
    };
    if !inner.orelse.is_empty() || inner.body.len() != 1 {
        return None;
    }
    let Stmt::If(cond) = &inner.body[0] else {
        return None;
    };
    if !cond.orelse.is_empty() || cond.body.len() != 1 {
        return None;
    }
    let Expr::Compare(cmp) = cond.test.as_ref() else {
        return None;
    };
    if cmp.ops.len() != 1 || cmp.comparators.len() != 1 {
        return None;
    }
    let descending = match cmp.ops[0] {
        CmpOp::Gt => false,
        CmpOp::Lt => true,
        _ => return None,
    };
    let (xa, ia) = subscript(src, &cmp.left)?;
    let (xb, ib) = subscript(src, &cmp.comparators[0])?;
    if xa != xb || ia == ib {
        return None;
    }
    let Stmt::Assign(swap) = &cond.body[0] else {
        return None;
    };
    if swap.targets.len() != 1 {
        return None;
    }
    let (Expr::Tuple(t), Expr::Tuple(v)) = (&swap.targets[0], swap.value.as_ref()) else {
        return None;
    };
    if t.elts.len() != 2 || v.elts.len() != 2 {
        return None;
    }
    let ts: Vec<_> = t
        .elts
        .iter()
        .map(|e| subscript(src, e))
        .collect::<Option<_>>()?;
    let vs: Vec<_> = v
        .elts
        .iter()
        .map(|e| subscript(src, e))
        .collect::<Option<_>>()?;
    let a = (xa.clone(), ia);
    let b = (xa.clone(), ib);
    if ts != [a.clone(), b.clone()] || vs != [b, a] {
        return None;
    }
    let x = xa;
    let (s, e) = span(outer.range);
    let rev = if descending { ", reverse=True" } else { "" };
    let rev_kw = if descending { "reverse=True" } else { "" };
    Some(vec![
        Rewrite {
            rule: "bubble-sort:sorted",
            source: splice(src, vec![(s, e, format!("{x}[:] = sorted({x}{rev})"))]),
        },
        Rewrite {
            rule: "bubble-sort:list.sort",
            source: splice(src, vec![(s, e, format!("{x}.sort({rev_kw})"))]),
        },
    ])
}

/// `s = ""` then `s += piece` in a loop becomes list appends and one `"".join`.
fn concat_join(src: &str, body: &[Stmt], used: &HashSet<String>) -> Vec<Rewrite> {
    for block in all_blocks(body) {
        for (k, stmt) in block.iter().enumerate() {
            let Some((s_name, value)) = single_name_target(stmt) else {
                continue;
            };
            let is_empty_str = matches!(
                value,
                Expr::Constant(c) if matches!(&c.value, ast::Constant::Str(s) if s.is_empty())
            );
            if !is_empty_str {
                continue;
            }
            for lp in &block[k + 1..] {
                let f = facts(lp);
                let refs = f.names.iter().filter(|n| n.id == s_name).count();
                if !is_loop(lp) {
                    if refs > 0 {
                        break;
                    }
                    continue;
                }
                let augs: Vec<&Aug> = f.augs.iter().filter(|a| a.target == s_name).collect();
                if augs.is_empty() || augs.iter().any(|a| !a.add) || refs != augs.len() {
                    break;
                }
                let parts = fresh(used, &format!("{s_name}_parts"));
                let (as_, ae) = span(stmt.range());
                let (ls, le) = span(lp.range());
                let indent = indent_at(src, ls);
                let mut edits = vec![
                    (as_, ae, format!("{parts} = []")),
                    (le, le, format!("\n{indent}{s_name} = \"\".join({parts})")),
                ];
                for a in augs {
                    let (s, e) = span(a.range);
                    edits.push((s, e, format!("{parts}.append({})", text(src, a.value))));
                }
                return vec![Rewrite {
                    rule: "concat:join",
                    source: splice(src, edits),
                }];
            }
        }
    }
    Vec::new()
}

/// Membership tests against a list inside a loop use a set built once before the loop.
fn set_membership(src: &str, body: &[Stmt], used: &HashSet<String>) -> Vec<Rewrite> {
    for block in all_blocks(body) {
        for (k, stmt) in block.iter().enumerate() {
            let Some((l_name, value)) = single_name_target(stmt) else {
                continue;
            };
            let is_list = match value {
                Expr::List(_) | Expr::ListComp(_) => true,
                Expr::Call(c) => name_of(&c.func).as_deref() == Some("list"),
                _ => false,
            };
            if !is_list {
                continue;
            }
            for lp in &block[k + 1..] {
                let f = facts(lp);
                let mine: Vec<&NameUse> = f.names.iter().filter(|n| n.id == l_name).collect();
                if !is_loop(lp) {
                    if mine.iter().any(|n| n.store) {
                        break;
                    }
                    continue;
                }
                let tests: Vec<TextRange> = f
                    .member_tests
                    .iter()
                    .filter(|(n, _)| *n == l_name)
                    .map(|(_, r)| *r)
                    .collect();
                if tests.is_empty() || mine.len() != tests.len() {
                    if mine.iter().any(|n| n.store) {
                        break;
                    }
                    continue;
                }
                let set_name = fresh(used, &format!("{l_name}_set"));
                let (ls, _) = span(lp.range());
                let indent = indent_at(src, ls);
                let build = |ctor: &str| {
                    let mut edits =
                        vec![(ls, ls, format!("{set_name} = {ctor}({l_name})\n{indent}"))];
                    for r in &tests {
                        let (s, e) = span(*r);
                        edits.push((s, e, set_name.clone()));
                    }
                    splice(src, edits)
                };
                return vec![
                    Rewrite {
                        rule: "membership:set",
                        source: build("set"),
                    },
                    Rewrite {
                        rule: "membership:frozenset",
                        source: build("frozenset"),
                    },
                ];
            }
        }
    }
    Vec::new()
}
