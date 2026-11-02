use aco::mutate::rewrites;
use aco::policy::{check, Violation};

const BUBBLE: &str = include_str!("../fixtures/bubble.py");
const CONCAT: &str = include_str!("../fixtures/concat.py");
const LOOKUP: &str = include_str!("../fixtures/lookup.py");
const SLOWER: &str = include_str!("../fixtures/slower.py");

fn rule_names(src: &str) -> Vec<String> {
    rewrites(src).into_iter().map(|r| r.rule).collect()
}

#[test]
fn fixtures_pass_policy() {
    for src in [BUBBLE, CONCAT, LOOKUP, SLOWER] {
        assert_eq!(check(src), Ok(()));
    }
}

#[test]
fn bubble_rule_rewrites_bubble_fixture() {
    let rw = rewrites(BUBBLE);
    assert_eq!(
        rule_names(BUBBLE),
        ["bubble-sort:sorted", "bubble-sort:list.sort"]
    );
    assert!(rw[0]
        .source
        .contains("    xs[:] = sorted(xs)\n    return xs"));
    assert!(!rw[0].source.contains("for j in range"));
    assert!(rw[1].source.contains("xs.sort()"));
}

#[test]
fn bubble_rule_handles_descending() {
    let src = "a = [3, 1, 2]\nfor i in range(len(a)):\n    for j in range(len(a) - 1):\n        if a[j] < a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\nprint(a)\n";
    let rw = rewrites(src);
    assert_eq!(
        rw[0].source,
        "a = [3, 1, 2]\na[:] = sorted(a, reverse=True)\nprint(a)\n"
    );
}

#[test]
fn concat_rule_rewrites_concat_fixture() {
    let rw = rewrites(CONCAT);
    assert_eq!(rule_names(CONCAT), ["concat:join"]);
    let s = &rw[0].source;
    assert!(s.contains("report_parts = []"));
    assert!(s.contains("    report_parts.append(str(i * 31 % 9973) + \",\")"));
    assert!(s.contains("\nreport = \"\".join(report_parts)\n"));
    assert!(!s.contains("report +="));
}

#[test]
fn lookup_rule_rewrites_lookup_fixture() {
    let rw = rewrites(LOOKUP);
    assert_eq!(
        rule_names(LOOKUP),
        ["membership:set", "membership:frozenset"]
    );
    let s = &rw[0].source;
    assert!(s.contains("allowed_set = set(allowed)\nfor k in range"));
    assert!(s.contains("if k in allowed_set:"));
}

#[test]
fn slower_edit_is_produced() {
    // The set rule fires on slower.py. The search must measure and reject it.
    let rw = rewrites(SLOWER);
    assert_eq!(
        rule_names(SLOWER),
        ["membership:set", "membership:frozenset"]
    );
    assert!(rw[0]
        .source
        .contains("    small_set = set(small)\n    for v in (1, 2, 3):"));
}

#[test]
fn non_matching_file_is_unchanged() {
    let src = "total = 0\nfor i in range(10):\n    total += i\nprint(total)\n";
    assert!(rewrites(src).is_empty());
    // A list that is mutated inside the loop must not be turned into a set.
    let src = "xs = [1]\nfor i in range(5):\n    if i in xs:\n        pass\n    xs.append(i)\nprint(xs)\n";
    assert!(rewrites(src).is_empty());
    // A string accumulator that is read inside the loop must stay a string.
    let src = "s = \"\"\nfor i in range(5):\n    s += str(len(s))\nprint(s)\n";
    assert!(rewrites(src).is_empty());
}

#[test]
fn rewrites_are_idempotent_on_their_own_output() {
    for src in [BUBBLE, CONCAT, LOOKUP] {
        for r in rewrites(src) {
            assert!(rewrites(&r.source).is_empty(), "{} fired twice", r.rule);
        }
    }
}

#[test]
fn policy_rejects_escape_routes() {
    let cases: &[(&str, Violation)] = &[
        ("import os\n", Violation::BannedImport("os".into())),
        (
            "import sys\nm = sys.modules\n",
            Violation::SysUse("sys.modules".into()),
        ),
        (
            "import sys as s\nf = s._getframe\n",
            Violation::SysUse("s._getframe".into()),
        ),
        ("import sys\nx = [sys]\n", Violation::SysUse("sys".into())),
        (
            "from sys import modules\n",
            Violation::SysUse("sys.modules".into()),
        ),
        (
            "from os.path import join\n",
            Violation::BannedImport("os".into()),
        ),
        (
            "import urllib.request\n",
            Violation::BannedImport("urllib".into()),
        ),
        ("eval('1')\n", Violation::BannedName("eval".into())),
        ("f = exec\n", Violation::BannedName("exec".into())),
        (
            "getattr(1, 'real')\n",
            Violation::BannedName("getattr".into()),
        ),
        (
            "open('/etc/passwd')\n",
            Violation::BannedName("open".into()),
        ),
        (
            "().__class__.__subclasses__()\n",
            Violation::DunderAttribute("__subclasses__".into()),
        ),
        (
            "x = __builtins__\n",
            Violation::DunderName("__builtins__".into()),
        ),
        (
            "class A:\n    def __del__(self):\n        pass\n",
            Violation::DunderName("__del__".into()),
        ),
        (
            "x = ().__class__\n",
            Violation::DunderAttribute("__class__".into()),
        ),
    ];
    for (src, want) in cases {
        assert_eq!(check(src).as_ref(), Err(want), "source: {src}");
    }
    assert!(matches!(check("def f(:\n"), Err(Violation::Syntax(_))));
    assert_eq!(check("if __name__ == '__main__':\n    print(1)\n"), Ok(()));
    // Ordinary classes are fine: data-model methods and super().__init__().
    let class = "class P(Base):\n    __slots__ = ('x',)\n    def __init__(self, x):\n        super().__init__()\n        self.x = x\n    def __lt__(self, o):\n        return self.x < o.x\n";
    assert_eq!(check(class), Ok(()));
    // Scripts may read their arguments and streams and exit.
    let io = "import sys\nn = int(sys.argv[1])\ndata = sys.stdin.read()\nsys.stdout.write(data)\nsys.setrecursionlimit(10000)\nsys.exit(0)\n";
    assert_eq!(check(io), Ok(()));
    assert_eq!(
        check("from sys import argv, stdin\nprint(argv, stdin.readline())\n"),
        Ok(())
    );
    // Relative imports are left to project analysis.
    assert_eq!(check("from . import x\nfrom .m import y\n"), Ok(()));
}

#[test]
fn violations_report_their_line() {
    let src = "def f(xs):\n    total = 0\n\n    for x in xs:\n        total += eval(x)\n    return total\n";
    let f = aco::policy::check_located(src).unwrap_err();
    assert_eq!(f.violation, Violation::BannedName("eval".into()));
    assert_eq!(f.line, 5);
    let f = aco::policy::check_located("x = 1\nimport os.path\n").unwrap_err();
    assert_eq!(
        (f.violation, f.line),
        (Violation::BannedImport("os".into()), 2)
    );
}

#[test]
fn rules_refuse_unsound_rewrites() {
    // Each of these fired before the rules were tightened (the output check
    // then had to catch the wrong program).
    for (name, src) in [
        (
            "alias",
            include_str!("../fixtures/stress/traps/alias_set.py"),
        ),
        (
            "global mutation",
            include_str!("../fixtures/stress/traps/global_mutation.py"),
        ),
        (
            "partial bubble",
            include_str!("../fixtures/stress/traps/partial_bubble.py"),
        ),
        (
            "loop variables read later",
            include_str!("../fixtures/stress/traps/loopvar_bubble.py"),
        ),
        (
            "order-preserving dedupe",
            include_str!("../fixtures/stress/traps/hashseed.py"),
        ),
    ] {
        assert!(rule_names(src).is_empty(), "{name}: {:?}", rule_names(src));
    }
    // More shapes that must stay untouched.
    for src in [
        // The list escapes into a call before the loop.
        "xs = [1, 2]\nkeep(xs)\nfor i in range(9):\n    if i in xs:\n        pass\n",
        // A lambda can see the list.
        "xs = [1, 2]\nf = lambda v: xs.append(v)\nfor i in range(9):\n    if i in xs:\n        f(i)\n",
        // Stored in a container that is mutated later.
        "xs = [1]\nbox = [xs]\nfor i in range(9):\n    if i in xs:\n        box[0].append(i)\n",
        // Wrong indices: compares j and j + 2.
        "a = [3, 1, 2]\nfor i in range(len(a)):\n    for j in range(len(a) - 2):\n        if a[j] > a[j + 2]:\n            a[j], a[j + 2] = a[j + 2], a[j]\n",
        // Inner bound is not tied to the list's length.
        "a = [3, 1, 2]\nfor i in range(len(a)):\n    for j in range(2):\n        if a[j] > a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\n",
        // n is not len(a).
        "a = [3, 1, 2, 0]\nn = 3\nfor i in range(n):\n    for j in range(n - 1 - i):\n        if a[j] > a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\n",
        // n = len(a), but a grows before the sort.
        "a = [3, 1, 2]\nn = len(a)\na.append(0)\nfor i in range(n):\n    for j in range(n - 1 - i):\n        if a[j] > a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\n",
        // A nested function reads the accumulator mid-loop.
        "s = ''\ndef peek():\n    return len(s)\nfor i in range(5):\n    s += str(peek())\n",
    ] {
        assert!(rule_names(src).is_empty(), "{src}: {:?}", rule_names(src));
    }
}

#[test]
fn rules_accept_safe_variants() {
    // Duplicates and order do not matter to membership.
    assert_eq!(
        rule_names(include_str!("../fixtures/stress/traps/duplicates_order.py")),
        ["membership:set", "membership:frozenset"]
    );
    // Read-only uses of the list around the loop are fine.
    let src = "xs = [1, 2, 3]\nprint(len(xs), sum(xs))\nfor v in xs:\n    pass\nfor i in range(9):\n    if i in xs:\n        print(i)\n";
    assert_eq!(rule_names(src), ["membership:set", "membership:frozenset"]);
    // `range(len(a) - 1)` outer and `A - (i + 1)` inner are full sorts too.
    let src = "a = [3, 1, 2]\nfor i in range(len(a) - 1):\n    for j in range(len(a) - (i + 1)):\n        if a[j] > a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\nprint(a)\n";
    assert_eq!(
        rule_names(src),
        ["bubble-sort:sorted", "bubble-sort:list.sort"]
    );
    // Loop variables reused by a later loop are rebound first.
    let src = "a = [3, 1, 2]\nfor i in range(len(a)):\n    for j in range(len(a) - 1 - i):\n        if a[j] > a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\nfor i in range(2):\n    print(i)\n";
    assert_eq!(
        rule_names(src),
        ["bubble-sort:sorted", "bubble-sort:list.sort"]
    );
}

#[test]
fn several_sites_give_combined_and_single_rewrites() {
    let src = "def f(ids):\n    known = [1, 2, 3]\n    n = 0\n    for x in ids:\n        if x in known:\n            n += 1\n    return n\n\n\ndef g(ids):\n    other = [4, 5]\n    n = 0\n    for x in ids:\n        if x in other:\n            n += 1\n    return n\n\n\nprint(f(range(9)), g(range(9)))\n";
    let names = rule_names(src);
    assert_eq!(
        names,
        [
            "membership:set ×2",
            "membership:frozenset ×2",
            "membership:set @L4",
            "membership:set @L13",
            "membership:frozenset @L4",
            "membership:frozenset @L13",
        ]
    );
    let all = &rewrites(src)[0].source;
    assert!(all.contains("known_set = set(known)") && all.contains("other_set = set(other)"));
    // A large file with many sites stays fast to analyse.
    let big: String = (0..400)
        .map(|k| format!("def f{k}(ids):\n    known = [1, 2, 3]\n    n = ''\n    for x in ids:\n        if x in known:\n            n += 'a'\n    return n\n\n\n"))
        .collect();
    let t = std::time::Instant::now();
    let rw = rewrites(&big);
    // set ×400, frozenset ×400, join ×400, then 6 singles of each variant.
    assert_eq!(
        rw.len(),
        3 + 3 * aco::mutate::MAX_SINGLE_SITES,
        "{}",
        rw.len()
    );
    let all = rw.iter().find(|r| r.rule == "membership:set ×400").unwrap();
    assert_eq!(all.source.matches(" = set(known)\n").count(), 400);
    assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
}
