//! Project mode: the local modules a script imports.
//!
//! Starting from the target, aco follows `import` statements the way Python
//! would for `python target.py` (the target's directory first, then the
//! project root), collects every local module reached, and classifies every
//! other import as standard library or third-party. Only that closure is
//! copied into the sandbox, read-only; only one file (the target, or the one
//! named by `--mutate`) is ever rewritten.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use rustpython_ast::{self as ast, Stmt};
use serde::Serialize;

use crate::policy;
use crate::source;
use crate::stdlib::is_stdlib;

/// At most this many local files are copied into the sandbox.
pub const MAX_FILES: usize = 1000;
/// At most this many bytes of local source.
pub const MAX_BYTES: u64 = 20 << 20;

/// Directories never searched for local modules.
pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "venv",
    ".venv",
    "__pycache__",
    "node_modules",
    "site-packages",
    "dist-packages",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
];

/// One local file in the closure.
#[derive(Clone, Debug, Serialize)]
pub struct LocalFile {
    /// Path relative to the project root, `/`-separated.
    pub rel: String,
    /// Dotted module name (`__main__` for the target).
    pub module: String,
    pub lines: usize,
    pub bytes: u64,
    #[serde(skip)]
    pub path: PathBuf,
}

/// An import that is not local and not standard library.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalImport {
    /// Top-level module name, e.g. `numpy`.
    pub module: String,
    /// First place it is imported: `rel:line`.
    pub at: String,
    /// Imported inside `try:` (has a fallback).
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Project {
    #[serde(skip)]
    pub root: PathBuf,
    /// The script that runs, relative to the root.
    pub target: String,
    /// The file aco rewrites, relative to the root.
    pub mutated: String,
    /// Every local file in the import closure, target first.
    pub files: Vec<LocalFile>,
    /// Third-party imports, by top-level name.
    pub external: Vec<ExternalImport>,
    #[serde(skip)]
    pub notes: Vec<String>,
}

#[derive(Debug)]
struct Import {
    /// Dotted module, relative dots stripped.
    module: String,
    level: u32,
    /// Names after `from X import`; empty for `import X`.
    names: Vec<String>,
    line: usize,
    optional: bool,
}

fn collect_imports(body: &[Stmt], src: &str, optional: bool, out: &mut Vec<Import>) {
    for s in body {
        match s {
            Stmt::Import(i) => {
                for a in &i.names {
                    out.push(Import {
                        module: a.name.to_string(),
                        level: 0,
                        names: vec![],
                        line: policy::line_of(src, u32::from(i.range.start()) as usize),
                        optional,
                    });
                }
            }
            Stmt::ImportFrom(i) => out.push(Import {
                module: i.module.as_ref().map(|m| m.to_string()).unwrap_or_default(),
                level: i.level.map(|l| l.to_u32()).unwrap_or(0),
                names: i.names.iter().map(|a| a.name.to_string()).collect(),
                line: policy::line_of(src, u32::from(i.range.start()) as usize),
                optional,
            }),
            Stmt::FunctionDef(f) => collect_imports(&f.body, src, optional, out),
            Stmt::AsyncFunctionDef(f) => collect_imports(&f.body, src, optional, out),
            Stmt::ClassDef(c) => collect_imports(&c.body, src, optional, out),
            Stmt::For(f) => {
                collect_imports(&f.body, src, optional, out);
                collect_imports(&f.orelse, src, optional, out);
            }
            Stmt::AsyncFor(f) => {
                collect_imports(&f.body, src, optional, out);
                collect_imports(&f.orelse, src, optional, out);
            }
            Stmt::While(w) => {
                collect_imports(&w.body, src, optional, out);
                collect_imports(&w.orelse, src, optional, out);
            }
            Stmt::If(i) => {
                // `if TYPE_CHECKING:` never runs.
                let tc = matches!(&*i.test, ast::Expr::Name(n) if n.id.as_str() == "TYPE_CHECKING");
                collect_imports(&i.body, src, optional || tc, out);
                collect_imports(&i.orelse, src, optional, out);
            }
            Stmt::With(w) => collect_imports(&w.body, src, optional, out),
            Stmt::AsyncWith(w) => collect_imports(&w.body, src, optional, out),
            Stmt::Try(t) => {
                collect_imports(&t.body, src, true, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_imports(&h.body, src, true, out);
                }
                collect_imports(&t.orelse, src, optional, out);
                collect_imports(&t.finalbody, src, optional, out);
            }
            Stmt::TryStar(t) => {
                collect_imports(&t.body, src, true, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_imports(&h.body, src, true, out);
                }
                collect_imports(&t.orelse, src, optional, out);
                collect_imports(&t.finalbody, src, optional, out);
            }
            Stmt::Match(m) => {
                for c in &m.cases {
                    collect_imports(&c.body, src, optional, out);
                }
            }
            _ => {}
        }
    }
}

fn ignored(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| IGNORED_DIRS.contains(&n))
        || dir.join("pyvenv.cfg").is_file()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// What one module name resolves to under one directory.
enum Found {
    /// A module file, or a package's `__init__.py`.
    Module(PathBuf, bool),
    /// A directory without `__init__.py` (namespace package portion).
    Namespace(PathBuf),
}

fn find_in(dir: &Path, name: &str) -> Option<Found> {
    if !is_ident(name) {
        return None;
    }
    let pkg = dir.join(name);
    if pkg.is_dir() && !ignored(&pkg) {
        let init = pkg.join("__init__.py");
        if init.is_file() {
            return Some(Found::Module(init, true));
        }
    }
    let file = dir.join(format!("{name}.py"));
    if file.is_file() {
        return Some(Found::Module(file, false));
    }
    if pkg.is_dir() && !ignored(&pkg) && has_python(&pkg) {
        return Some(Found::Namespace(pkg));
    }
    None
}

fn has_python(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let p = e.path();
            p.extension().is_some_and(|x| x == "py")
                || (p.is_dir() && p.join("__init__.py").is_file())
        })
    })
}

struct Resolver {
    root: PathBuf,
    search: Vec<PathBuf>,
}

/// Files a module import executes, outermost package first, plus the
/// directories it may hold submodules in.
struct Resolution {
    files: Vec<(PathBuf, String, bool)>,
    dirs: Vec<PathBuf>,
}

impl Resolver {
    /// Resolve dotted `module` from the top, as `import module` would.
    fn absolute(&self, module: &str) -> Option<Resolution> {
        let parts: Vec<&str> = module.split('.').collect();
        // Top level: first search dir with a regular module wins; namespace
        // portions are only used if nothing regular exists.
        let mut found = None;
        let mut ns = Vec::new();
        for d in &self.search {
            match find_in(d, parts[0]) {
                Some(Found::Module(p, pkg)) => {
                    found = Some((p, pkg));
                    break;
                }
                Some(Found::Namespace(p)) => ns.push(p),
                None => {}
            }
        }
        let mut res = Resolution {
            files: vec![],
            dirs: vec![],
        };
        match found {
            Some((p, pkg)) => {
                if pkg {
                    res.dirs = vec![p.parent()?.to_path_buf()];
                }
                res.files.push((p, parts[0].to_string(), pkg));
            }
            None if !ns.is_empty() && !is_stdlib(parts[0]) => res.dirs = ns,
            None => return None,
        }
        self.descend(res, &parts, 1)
    }

    fn descend(&self, mut res: Resolution, parts: &[&str], from: usize) -> Option<Resolution> {
        for i in from..parts.len() {
            let name = parts[..=i].join(".");
            let mut next = None;
            let mut ns = Vec::new();
            for d in &res.dirs {
                match find_in(d, parts[i]) {
                    Some(Found::Module(p, pkg)) => {
                        next = Some((p, pkg));
                        break;
                    }
                    Some(Found::Namespace(p)) => ns.push(p),
                    None => {}
                }
            }
            match next {
                Some((p, pkg)) => {
                    res.dirs = if pkg {
                        vec![p.parent()?.to_path_buf()]
                    } else {
                        vec![]
                    };
                    res.files.push((p, name, pkg));
                }
                None if !ns.is_empty() => res.dirs = ns,
                None => return None,
            }
        }
        Some(res)
    }

    fn rel(&self, p: &Path) -> Option<String> {
        let c = p.canonicalize().ok()?;
        let r = c.strip_prefix(&self.root).ok()?;
        Some(
            r.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }
}

/// Characters that cannot appear in a project path (they would break the
/// sandbox mount specification).
fn bad_path_char(c: char) -> bool {
    c.is_control() || matches!(c, ',' | '"' | '\'' | '\\' | ':')
}

/// Suggest a distribution name for an import name that differs from it.
pub fn dist_hint(module: &str) -> &str {
    match module {
        "sklearn" => "scikit-learn",
        "cv2" => "opencv-python-headless",
        "PIL" => "Pillow",
        "yaml" => "PyYAML",
        "bs4" => "beautifulsoup4",
        "dateutil" => "python-dateutil",
        "attr" => "attrs",
        "Crypto" => "pycryptodome",
        "dotenv" => "python-dotenv",
        "jwt" => "PyJWT",
        "skimage" => "scikit-image",
        "google.protobuf" | "google" => "protobuf",
        _ => module,
    }
}

/// Analyse the import closure of `target`.
///
/// `root` defaults to the target's directory. `mutate` (default: the target)
/// must be inside the closure.
pub fn analyze(
    target: &Path,
    root: Option<&Path>,
    mutate: Option<&Path>,
) -> Result<Project, String> {
    let target_c = target
        .canonicalize()
        .map_err(|e| format!("cannot resolve `{}`: {e}", target.display()))?;
    let target_dir = target_c.parent().unwrap_or(Path::new("/")).to_path_buf();
    let root_c = match root {
        Some(r) => {
            let c = r
                .canonicalize()
                .map_err(|e| format!("--project-root `{}`: {e}", r.display()))?;
            if !c.is_dir() {
                return Err(format!(
                    "--project-root `{}` is not a directory",
                    r.display()
                ));
            }
            c
        }
        None => target_dir.clone(),
    };
    if !target_c.starts_with(&root_c) {
        return Err(format!(
            "`{}` is not inside the project root `{}`",
            target.display(),
            root_c.display()
        ));
    }
    let mut search = vec![target_dir.clone()];
    if root_c != target_dir {
        search.push(root_c.clone());
    }
    let r = Resolver {
        root: root_c.clone(),
        search,
    };

    let target_rel = r.rel(&target_c).ok_or("target outside root")?;
    let mut files: BTreeMap<String, LocalFile> = BTreeMap::new();
    let mut external: BTreeMap<String, ExternalImport> = BTreeMap::new();
    let mut notes = Vec::new();
    let mut total: u64 = 0;
    let mut queue: VecDeque<(PathBuf, String, bool)> = VecDeque::new();
    queue.push_back((target_c.clone(), "__main__".into(), false));

    while let Some((path, module, is_package)) = queue.pop_front() {
        let Some(rel) = r.rel(&path) else {
            notes.push(format!(
                "`{}` resolves outside the project root; it was not copied",
                path.display()
            ));
            continue;
        };
        if files.contains_key(&rel) {
            continue;
        }
        if let Some(c) = rel.chars().find(|c| bad_path_char(*c)) {
            return Err(format!(
                "`{rel}`: project file paths may not contain {c:?}; rename it"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read `{rel}`: {e}"))?;
        total += bytes.len() as u64;
        if files.len() + 1 > MAX_FILES || total > MAX_BYTES {
            return Err(format!(
                "the project is too large for the sandbox: the target imports more than \
                 {MAX_FILES} local files or {} MB of source. Point --project-root at a \
                 smaller directory, or move generated or vendored code out of it",
                MAX_BYTES >> 20
            ));
        }
        let src = source::decode(&bytes).map_err(|e| format!("`{rel}`: {e}"))?;
        let body = match policy::parse_module(&src.text) {
            Ok(b) => b,
            Err(_) => {
                let se = policy::syntax_error(&src.text);
                let (line, msg) = se
                    .map(|s| (s.line, s.message))
                    .unwrap_or((1, "syntax error".into()));
                return Err(format!("{rel}:{line}: does not parse: {msg}"));
            }
        };
        let mut imports = Vec::new();
        collect_imports(&body, &src.text, false, &mut imports);
        files.insert(
            rel.clone(),
            LocalFile {
                rel: rel.clone(),
                module: module.clone(),
                lines: src.text.lines().count(),
                bytes: bytes.len() as u64,
                path: path.clone(),
            },
        );
        for imp in imports {
            let at = format!("{rel}:{}", imp.line);
            let shown = format!("{}{}", ".".repeat(imp.level as usize), imp.module);
            let resolved = if imp.level > 0 {
                if module == "__main__" {
                    return Err(format!(
                        "{at}: relative import `from {shown} import ...` in the script being \
                         run. Python only allows relative imports inside a package, so \
                         `python {rel}` fails the same way. Use an absolute import"
                    ));
                }
                // The package this module is in (itself, for an __init__).
                let mut pkg: Vec<&str> = module.split('.').collect();
                if !is_package {
                    pkg.pop();
                }
                for _ in 1..imp.level {
                    if pkg.pop().is_none() {
                        break;
                    }
                }
                if pkg.is_empty() {
                    return Err(format!(
                        "{at}: relative import `{shown}` goes above the top-level package"
                    ));
                }
                let mut full = pkg.join(".");
                if !imp.module.is_empty() {
                    full = format!("{full}.{}", imp.module);
                }
                match r.absolute(&full) {
                    Some(res) => Some(res),
                    None => {
                        return Err(format!(
                            "{at}: relative import `{shown}` does not resolve to a file in the \
                             project"
                        ))
                    }
                }
            } else {
                r.absolute(&imp.module)
            };
            match resolved {
                Some(res) => {
                    // `from pkg import sub` may name submodules.
                    let base = res.files.last().map(|f| f.1.clone());
                    let dirs = res.dirs.clone();
                    for (p, m, pkg) in res.files {
                        queue.push_back((p, m, pkg));
                    }
                    for name in &imp.names {
                        if name == "*" {
                            continue;
                        }
                        for d in &dirs {
                            if let Some(Found::Module(p, pkg)) = find_in(d, name) {
                                let m = match &base {
                                    Some(b) => format!("{b}.{name}"),
                                    None => name.clone(),
                                };
                                queue.push_back((p, m, pkg));
                                break;
                            }
                        }
                    }
                }
                None => {
                    let top = imp.module.split('.').next().unwrap_or_default().to_string();
                    if top.is_empty() || is_stdlib(&top) {
                        continue;
                    }
                    let e = external.entry(top.clone()).or_insert(ExternalImport {
                        module: top,
                        at: at.clone(),
                        optional: imp.optional,
                    });
                    // Required anywhere means required.
                    if !imp.optional && e.optional {
                        e.optional = false;
                        e.at = at;
                    }
                }
            }
        }
    }

    let mutate_rel = match mutate {
        None => target_rel.clone(),
        Some(m) => {
            let c = m
                .canonicalize()
                .map_err(|e| format!("--mutate `{}`: {e}", m.display()))?;
            let rel = r
                .rel(&c)
                .ok_or_else(|| format!("--mutate `{}` is outside the project root", m.display()))?;
            if !files.contains_key(&rel) {
                let list: Vec<&str> = files.keys().map(|s| s.as_str()).collect();
                return Err(format!(
                    "--mutate `{}` is not imported by `{target_rel}` (directly or \
                     indirectly), so rewriting it could not change the run. Local files \
                     the target imports: {}",
                    m.display(),
                    list.join(", ")
                ));
            }
            rel
        }
    };

    for f in files.values() {
        let top = f.module.split('.').next().unwrap_or_default();
        if f.module != "__main__" && is_stdlib(top) && !f.module.contains('.') {
            notes.push(format!(
                "local `{}` shadows the standard library module `{top}`",
                f.rel
            ));
        }
    }

    let mut ordered: Vec<LocalFile> = Vec::with_capacity(files.len());
    if let Some(t) = files.remove(&target_rel) {
        ordered.push(t);
    }
    ordered.extend(files.into_values());
    Ok(Project {
        root: root_c,
        target: target_rel,
        mutated: mutate_rel,
        files: ordered,
        external: external.into_values().collect(),
        notes,
    })
}

impl Project {
    /// True when the run needs the project directory (more than one file,
    /// or the rewritten file is not the target).
    pub fn is_multi_file(&self) -> bool {
        self.files.len() > 1 || self.mutated != self.target
    }

    pub fn total_lines(&self) -> usize {
        self.files.iter().map(|f| f.lines).sum()
    }

    /// Run the policy pre-filter over every local file except the mutated
    /// one (its candidates are checked one by one). Errors are `rel:line:`.
    pub fn check_policy(&self) -> Result<(), String> {
        for f in &self.files {
            if f.rel == self.mutated {
                continue;
            }
            let bytes = std::fs::read(&f.path).map_err(|e| format!("{}: {e}", f.rel))?;
            let src = source::decode(&bytes).map_err(|e| format!("{}: {e}", f.rel))?;
            if let Err(found) = policy::check_located(&src.text) {
                return Err(format!("{}:{}: {}", f.rel, found.line, found.violation));
            }
        }
        Ok(())
    }

    /// Check third-party imports against what is installed. Returns notes for
    /// optional imports that will take their fallback.
    pub fn check_external(
        &self,
        installed: &BTreeSet<String>,
        reqs: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut notes = Vec::new();
        for e in &self.external {
            if installed.contains(&e.module) {
                continue;
            }
            if e.optional {
                notes.push(format!(
                    "{}: `{}` is imported inside try/except and is not installed; the \
                     fallback path runs",
                    e.at, e.module
                ));
                continue;
            }
            let dist = dist_hint(&e.module);
            let fix = match reqs {
                Some(src) => format!("Add `{dist}` to {src}"),
                None => format!(
                    "Add `{dist}` to a requirements.txt next to the script (or pass \
                     --requirements FILE)"
                ),
            };
            return Err(format!(
                "{}: imports `{}`, which is not in the standard library, the project, or \
                 the installed requirements. {fix}",
                e.at, e.module
            ));
        }
        Ok(notes)
    }

    /// Copy the closure into a fresh directory for the sandbox.
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "aco-proj-{}-{}",
            std::process::id(),
            crate::runner::nonce_public()
        ));
        let snap = Snapshot { dir: dir.clone() };
        let mkdir = |d: &Path| -> Result<(), String> {
            std::fs::create_dir_all(d).map_err(|e| format!("snapshot: {e}"))?;
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("snapshot: {e}"))
        };
        mkdir(&dir)?;
        for f in &self.files {
            let dest = dir.join(&f.rel);
            let mut cur = dest.parent();
            let mut chain = Vec::new();
            while let Some(c) = cur {
                if c == dir {
                    break;
                }
                chain.push(c.to_path_buf());
                cur = c.parent();
            }
            for c in chain.iter().rev() {
                mkdir(c)?;
            }
            std::fs::copy(&f.path, &dest).map_err(|e| format!("snapshot {}: {e}", f.rel))?;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644))
                .map_err(|e| format!("snapshot: {e}"))?;
        }
        Ok(snap)
    }

    /// Absolute path of the file aco rewrites.
    pub fn mutated_path(&self) -> PathBuf {
        self.root.join(&self.mutated)
    }

    /// Local files other than the mutated one in which `has_sites` finds
    /// something; used to tell the user the hot code may be elsewhere.
    pub fn other_files_where(&self, has_sites: impl Fn(&str) -> bool) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.rel != self.mutated)
            .filter(|f| {
                std::fs::read(&f.path)
                    .ok()
                    .and_then(|b| source::decode(&b).ok())
                    .is_some_and(|s| has_sites(&s.text))
            })
            .map(|f| f.rel.clone())
            .collect()
    }
}

/// A copy of the project's local files; removed on drop.
#[derive(Debug)]
pub struct Snapshot {
    pub dir: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
