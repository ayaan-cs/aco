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
    pub rule: String,
    pub source: String,
}

/// With many sites, single-site rewrites are offered for the first few only;
/// the combined rewrite always covers all of them.
pub const MAX_SINGLE_SITES: usize = 6;

/// One place a rule can rewrite: its edits for each variant (same order as
/// the rule's variant names).
struct Site {
    line: usize,
    variants: Vec<Vec<(usize, usize, String)>>,
}

struct RuleSites {
    variants: &'static [&'static str],
    sites: Vec<Site>,
}

/// All rule rewrites of `source`.
///
/// Each rule finds every site it can prove safe to rewrite. With one site it
/// yields one rewrite per variant. With several it yields first all sites
/// rewritten together (`rule ×N`), then each site alone (`rule @L<line>`, for
/// the first [`MAX_SINGLE_SITES`]), so a site whose rewrite is
/// slower (a cold path) cannot hide one that is faster. Rewrites are ordered
/// in tiers so a small population still covers every rule: the primary
/// variant of each rule's combined rewrite first, then alternate variants,
/// then single sites.
pub fn rewrites(source: &str) -> Vec<Rewrite> {
    let Ok(body) = policy::parse_module(source) else {
        return Vec::new();
    };
    let used = all_names(&body);
    let rules = [
        bubble_sort(source, &body),
        concat_join(source, &body, &used),
        set_membership(source, &body, &used),
    ];
    let mut tiers: [Vec<Rewrite>; 4] = Default::default();
    for r in &rules {
        let n = r.sites.len();
        if n == 0 {
            continue;
        }
        for (v, name) in r.variants.iter().enumerate() {
            let all: Vec<_> = r.sites.iter().flat_map(|s| s.variants[v].clone()).collect();
            let rule = if n == 1 {
                name.to_string()
            } else {
                format!("{name} ×{n}")
            };
            tiers[usize::from(v > 0)].push(Rewrite {
                rule,
                source: splice(source, all),
            });
            if n > 1 {
                for site in r.sites.iter().take(MAX_SINGLE_SITES) {
                    tiers[2 + usize::from(v > 0)].push(Rewrite {
                        rule: format!("{name} @L{}", site.line),
                        source: splice(source, site.variants[v].clone()),
                    });
                }
            }
        }
    }
    let mut seen = HashSet::new();
    tiers
        .into_iter()
        .flatten()
        .filter(|r| r.source != source && seen.insert(r.source.clone()))
        .filter(|r| policy::check(&r.source).is_ok())
        .collect()
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

/// Every statement list in one scope, outermost first. Does not descend into
/// function or class bodies (they are scopes of their own).
fn scope_blocks<'a>(body: &'a [Stmt], out: &mut Vec<&'a [Stmt]>) {
    out.push(body);
    for s in body {
        match s {
            Stmt::For(f) => {
                scope_blocks(&f.body, out);
                scope_blocks(&f.orelse, out);
            }
            Stmt::AsyncFor(f) => {
                scope_blocks(&f.body, out);
                scope_blocks(&f.orelse, out);
            }
            Stmt::While(w) => {
                scope_blocks(&w.body, out);
                scope_blocks(&w.orelse, out);
            }
            Stmt::If(i) => {
                scope_blocks(&i.body, out);
                scope_blocks(&i.orelse, out);
            }
            Stmt::With(w) => scope_blocks(&w.body, out),
            Stmt::AsyncWith(w) => scope_blocks(&w.body, out),
            Stmt::Try(t) => {
                scope_blocks(&t.body, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    scope_blocks(&h.body, out);
                }
                scope_blocks(&t.orelse, out);
                scope_blocks(&t.finalbody, out);
            }
            Stmt::TryStar(t) => {
                scope_blocks(&t.body, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    scope_blocks(&h.body, out);
                }
                scope_blocks(&t.orelse, out);
                scope_blocks(&t.finalbody, out);
            }
            Stmt::Match(m) => {
                for c in &m.cases {
                    scope_blocks(&c.body, out);
                }
            }
            _ => {}
        }
    }
}

/// A function, class or module body and facts about it.
struct Scope<'a> {
    body: &'a [Stmt],
    blocks: Vec<&'a [Stmt]>,
    /// Names referenced inside functions, lambdas or classes nested in this
    /// scope. Such a name may be read or mutated from elsewhere at any call.
    nested: HashSet<String>,
}

fn collect_scopes<'a>(body: &'a [Stmt], out: &mut Vec<Scope<'a>>) {
    let mut blocks = Vec::new();
    scope_blocks(body, &mut blocks);
    let mut nested = Nested::default();
    for s in body {
        nested.visit_stmt(s.clone());
    }
    let inner: Vec<&'a [Stmt]> = blocks
        .iter()
        .flat_map(|b| b.iter())
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some(&f.body[..]),
            Stmt::AsyncFunctionDef(f) => Some(&f.body[..]),
            Stmt::ClassDef(c) => Some(&c.body[..]),
            _ => None,
        })
        .collect();
    out.push(Scope {
        body,
        blocks,
        nested: nested.names,
    });
    for b in inner {
        collect_scopes(b, out);
    }
}

fn scopes(body: &[Stmt]) -> Vec<Scope<'_>> {
    let mut out = Vec::new();
    collect_scopes(body, &mut out);
    out
}

#[derive(Default)]
struct Nested {
    depth: usize,
    names: HashSet<String>,
}

impl Visitor for Nested {
    crate::visit_everything!();
    fn visit_expr_name(&mut self, n: ast::ExprName) {
        if self.depth > 0 {
            self.names.insert(n.id.to_string());
        }
    }
    fn visit_stmt_function_def(&mut self, n: ast::StmtFunctionDef) {
        self.depth += 1;
        self.generic_visit_stmt_function_def(n);
        self.depth -= 1;
    }
    fn visit_stmt_async_function_def(&mut self, n: ast::StmtAsyncFunctionDef) {
        self.depth += 1;
        self.generic_visit_stmt_async_function_def(n);
        self.depth -= 1;
    }
    fn visit_stmt_class_def(&mut self, n: ast::StmtClassDef) {
        self.depth += 1;
        self.generic_visit_stmt_class_def(n);
        self.depth -= 1;
    }
    fn visit_expr_lambda(&mut self, n: ast::ExprLambda) {
        self.depth += 1;
        self.generic_visit_expr_lambda(n);
        self.depth -= 1;
    }
}

/// Loads of `names` in `stmts` that read a value bound before them: loads
/// inside a `for` that rebinds the name first do not count.
#[derive(Default)]
struct OuterLoads {
    watch: Vec<String>,
    bound: Vec<String>,
    found: bool,
}

impl Visitor for OuterLoads {
    crate::visit_everything!();
    fn visit_expr_name(&mut self, n: ast::ExprName) {
        let id = n.id.as_str();
        if matches!(n.ctx, ExprContext::Load)
            && self.watch.iter().any(|w| w == id)
            && !self.bound.iter().any(|b| b == id)
        {
            self.found = true;
        }
    }
    fn visit_stmt_for(&mut self, n: ast::StmtFor) {
        self.visit_expr(*n.iter);
        let mut t = Facts::default();
        t.visit_expr(*n.target);
        let k = self.bound.len();
        self.bound.extend(t.names.into_iter().map(|u| u.id));
        for s in n.body.into_iter().chain(n.orelse) {
            self.visit_stmt(s);
        }
        self.bound.truncate(k);
    }
    // Comprehension variables live in their own scope.
    fn visit_expr_list_comp(&mut self, n: ast::ExprListComp) {
        let k = self.bind_generators(&n.generators);
        self.generic_visit_expr_list_comp(n);
        self.bound.truncate(k);
    }
    fn visit_expr_set_comp(&mut self, n: ast::ExprSetComp) {
        let k = self.bind_generators(&n.generators);
        self.generic_visit_expr_set_comp(n);
        self.bound.truncate(k);
    }
    fn visit_expr_dict_comp(&mut self, n: ast::ExprDictComp) {
        let k = self.bind_generators(&n.generators);
        self.generic_visit_expr_dict_comp(n);
        self.bound.truncate(k);
    }
    fn visit_expr_generator_exp(&mut self, n: ast::ExprGeneratorExp) {
        let k = self.bind_generators(&n.generators);
        self.generic_visit_expr_generator_exp(n);
        self.bound.truncate(k);
    }
}

impl OuterLoads {
    fn bind_generators(&mut self, gens: &[ast::Comprehension]) -> usize {
        let k = self.bound.len();
        for g in gens {
            let mut t = Facts::default();
            t.visit_expr(g.target.clone());
            self.bound.extend(t.names.into_iter().map(|u| u.id));
        }
        k
    }
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
    /// Names used where they cannot be mutated or aliased: the iterable of
    /// a `for` or comprehension, or the sole argument of a pure builtin.
    harmless: Vec<String>,
}

/// Builtins that only read their argument.
const READ_ONLY_BUILTINS: &[&str] = &["len", "sum", "min", "max", "sorted", "any", "all", "print"];

impl Visitor for Facts {
    crate::visit_everything!(except_comprehension);

    fn visit_stmt_for(&mut self, n: ast::StmtFor) {
        if let Some(it) = name_of(&n.iter) {
            self.harmless.push(it);
        }
        self.generic_visit_stmt_for(n);
    }

    fn visit_comprehension(&mut self, c: ast::Comprehension) {
        if let Some(it) = name_of(&c.iter) {
            self.harmless.push(it);
        }
        self.visit_expr(c.iter);
        self.visit_expr(c.target);
        for e in c.ifs {
            self.visit_expr(e);
        }
    }

    fn visit_expr_call(&mut self, n: ast::ExprCall) {
        if let Some(f) = name_of(&n.func) {
            if READ_ONLY_BUILTINS.contains(&f.as_str())
                && n.args.len() == 1
                && n.keywords.is_empty()
            {
                if let Some(a) = name_of(&n.args[0]) {
                    self.harmless.push(a);
                }
            }
        }
        self.generic_visit_expr_call(n);
    }
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

fn line_at(src: &str, offset: usize) -> usize {
    policy::line_of(src, offset)
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
///
/// Only the textbook full bubble sort is recognised, because only then is the
/// result the sorted list: an outer `for I in range(A)` or `range(A - 1)`, an
/// inner `for J in range(A - 1 - I)` (or `A - I - 1`, `A - (I + 1)`, `A - 1`),
/// where `A` is `len(X)` or a name assigned `len(X)` just before with nothing
/// touching `X` in between, and a body that compares `X[J]` with `X[J + 1]`
/// and swaps exactly those two. `sorted` and the bubble sort are both stable,
/// so equal elements keep their order. The loop variables must not be read
/// after the loops (they would hold the last indices), nor by nested code.
fn bubble_sort(src: &str, body: &[Stmt]) -> RuleSites {
    let mut sites = Vec::new();
    for scope in scopes(body) {
        for block in &scope.blocks {
            for (k, stmt) in block.iter().enumerate() {
                if let Some(site) = bubble_at(src, &scope, block, k, stmt) {
                    sites.push(site);
                }
            }
        }
    }
    RuleSites {
        variants: &["bubble-sort:sorted", "bubble-sort:list.sort"],
        sites,
    }
}

fn norm(src: &str, e: &Expr) -> String {
    text(src, e.range()).split_whitespace().collect()
}

/// The `range(...)` argument of a `for NAME in range(ARG)` loop.
fn range_loop(f: &ast::StmtFor) -> Option<(String, &Expr)> {
    let var = name_of(&f.target)?;
    let Expr::Call(c) = f.iter.as_ref() else {
        return None;
    };
    if name_of(&c.func).as_deref() != Some("range") || c.args.len() != 1 || !c.keywords.is_empty() {
        return None;
    }
    Some((var, &c.args[0]))
}

fn bubble_at(src: &str, scope: &Scope, block: &[Stmt], k: usize, stmt: &Stmt) -> Option<Site> {
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
    let (i, outer_n) = range_loop(outer)?;
    let (j, inner_n) = range_loop(inner)?;
    if i == j {
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
    let (x, ia) = subscript(src, &cmp.left)?;
    let (xb, ib) = subscript(src, &cmp.comparators[0])?;
    let j1 = format!("{j}+1");
    if xb != x || ia != j || ib != j1 || x == i || x == j {
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
    let a = (x.clone(), j.clone());
    let b = (x.clone(), j1);
    if ts != [a.clone(), b.clone()] || vs != [b, a] {
        return None;
    }

    // The bounds: A is len(X), or a name bound to len(X) before the loop.
    let len_x = format!("len({x})");
    let mut lens = vec![len_x.clone()];
    if let Some(n) = len_alias(src, block, k, &x, &len_x) {
        lens.push(n);
    }
    let on = norm(src, outer_n);
    let inn = norm(src, inner_n);
    let outer_ok = lens.iter().any(|a| on == *a || on == format!("{a}-1"));
    let inner_ok = lens.iter().any(|a| {
        [
            format!("{a}-1-{i}"),
            format!("{a}-{i}-1"),
            format!("{a}-({i}+1)"),
            format!("{a}-1"),
        ]
        .contains(&inn)
    });
    if !outer_ok || !inner_ok {
        return None;
    }
    // Neither loop variable may be read after the loops or from nested code.
    if scope.nested.contains(&i) || scope.nested.contains(&j) {
        return None;
    }
    let mut loads = OuterLoads {
        watch: vec![i, j],
        ..Default::default()
    };
    let here = outer.range;
    for s in scope.body {
        // Statements that finish before the loop cannot see its variables.
        if s.range() == here || s.range().end() <= here.start() {
            continue;
        }
        if s.range().contains_range(here) {
            // The loop is nested in `s`; check the statements around it.
            let mut f = Facts::default();
            f.visit_stmt(s.clone());
            let inside = facts(stmt);
            let outside = f
                .names
                .iter()
                .filter(|u| !u.store && loads.watch.contains(&u.id))
                .count();
            let within = inside
                .names
                .iter()
                .filter(|u| !u.store && loads.watch.contains(&u.id))
                .count();
            if outside > within {
                return None;
            }
            continue;
        }
        loads.visit_stmt(s.clone());
    }
    if loads.found {
        return None;
    }

    let (s, e) = span(outer.range);
    let rev = if descending { ", reverse=True" } else { "" };
    let rev_kw = if descending { "reverse=True" } else { "" };
    Some(Site {
        line: line_at(src, s),
        variants: vec![
            vec![(s, e, format!("{x}[:] = sorted({x}{rev})"))],
            vec![(s, e, format!("{x}.sort({rev_kw})"))],
        ],
    })
}

/// A name `N` assigned `N = len(X)` earlier in `block`, with no statement
/// between that assignment and `block[k]` mentioning `X` or `N`.
fn len_alias(src: &str, block: &[Stmt], k: usize, x: &str, len_x: &str) -> Option<String> {
    for p in (0..k).rev() {
        let f = facts(&block[p]);
        if let Some((n, value)) = single_name_target(&block[p]) {
            if norm(src, value) == len_x {
                let clean = block[p + 1..k]
                    .iter()
                    .all(|s| !facts(s).names.iter().any(|u| u.id == n || u.id == x));
                return clean.then_some(n);
            }
        }
        if f.names.iter().any(|u| u.id == x) {
            return None;
        }
    }
    None
}

/// `s = ""` then `s += piece` in a loop becomes list appends and one `"".join`.
///
/// Sound when `s` is not read inside the loop (only `+=` on it), nothing
/// between the assignment and the loop touches it, and no nested function
/// can see it (it could read the partial string mid-loop).
fn concat_join(src: &str, body: &[Stmt], used: &HashSet<String>) -> RuleSites {
    let mut sites = Vec::new();
    let mut taken = used.clone();
    for scope in scopes(body) {
        for block in &scope.blocks {
            let fs: Vec<Facts> = block.iter().map(facts).collect();
            for (k, stmt) in block.iter().enumerate() {
                let Some((s_name, value)) = single_name_target(stmt) else {
                    continue;
                };
                let is_empty_str = matches!(
                    value,
                    Expr::Constant(c) if matches!(&c.value, ast::Constant::Str(s) if s.is_empty())
                );
                if !is_empty_str || scope.nested.contains(&s_name) {
                    continue;
                }
                for (m, lp) in block.iter().enumerate().skip(k + 1) {
                    let f = &fs[m];
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
                    let parts = fresh(&taken, &format!("{s_name}_parts"));
                    taken.insert(parts.clone());
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
                    sites.push(Site {
                        line: line_at(src, ls),
                        variants: vec![edits],
                    });
                    break;
                }
            }
        }
    }
    RuleSites {
        variants: &["concat:join"],
        sites,
    }
}

/// A brand-new list nothing else refers to: a literal, a comprehension, a
/// `list(...)` call, or a literal repeated with `*`.
fn fresh_list(e: &Expr) -> bool {
    match e {
        Expr::List(_) | Expr::ListComp(_) => true,
        Expr::Call(c) => name_of(&c.func).as_deref() == Some("list"),
        Expr::BinOp(b) if matches!(b.op, Operator::Mult) => {
            matches!(b.left.as_ref(), Expr::List(_) | Expr::ListComp(_))
                || matches!(b.right.as_ref(), Expr::List(_) | Expr::ListComp(_))
        }
        _ => false,
    }
}

/// Membership tests against a list inside a loop use a set built once before
/// the loop.
///
/// `x in L` and `x in set(L)` agree (for elements with consistent `==` and
/// hashing) as long as `L` does not change while the loop runs. So the rule
/// requires that `L` is a fresh list (see [`fresh_list`]) and that, from its assignment to the end of the loop, every use of
/// `L` is a membership test, `len(L)`, or the iterable of a `for`. Anything
/// else (`M = L`, `f(L)`, `L.append`, storing it in a container) could
/// mutate it through another reference, so the rule refuses. It also refuses
/// when nested functions or lambdas mention `L`, since a call could change it
/// mid-loop. Unhashable elements make the rewrite raise, which the output
/// check rejects.
fn set_membership(src: &str, body: &[Stmt], used: &HashSet<String>) -> RuleSites {
    let mut sites = Vec::new();
    let mut taken = used.clone();
    for scope in scopes(body) {
        for block in &scope.blocks {
            let fs: Vec<Facts> = block.iter().map(facts).collect();
            for (k, stmt) in block.iter().enumerate() {
                let Some((l_name, value)) = single_name_target(stmt) else {
                    continue;
                };
                if !fresh_list(value) || scope.nested.contains(&l_name) {
                    continue;
                }
                for (m, lp) in block.iter().enumerate().skip(k + 1) {
                    let f = &fs[m];
                    let mine: Vec<&NameUse> = f.names.iter().filter(|n| n.id == l_name).collect();
                    if mine.iter().any(|n| n.store) {
                        break;
                    }
                    let tests: Vec<TextRange> = f
                        .member_tests
                        .iter()
                        .filter(|(n, _)| *n == l_name)
                        .map(|(_, r)| *r)
                        .collect();
                    let harmless = f.harmless.iter().filter(|n| **n == l_name).count();
                    if mine.len() != tests.len() + harmless {
                        // Some other use: it may alias or mutate the list.
                        break;
                    }
                    if !is_loop(lp) || tests.is_empty() {
                        continue;
                    }
                    let set_name = fresh(&taken, &format!("{l_name}_set"));
                    taken.insert(set_name.clone());
                    let (ls, _) = span(lp.range());
                    let indent = indent_at(src, ls);
                    let build = |ctor: &str| {
                        let mut edits =
                            vec![(ls, ls, format!("{set_name} = {ctor}({l_name})\n{indent}"))];
                        for r in &tests {
                            let (s, e) = span(*r);
                            edits.push((s, e, set_name.clone()));
                        }
                        edits
                    };
                    sites.push(Site {
                        line: line_at(src, ls),
                        variants: vec![build("set"), build("frozenset")],
                    });
                    break;
                }
            }
        }
    }
    RuleSites {
        variants: &["membership:set", "membership:frozenset"],
        sites,
    }
}
