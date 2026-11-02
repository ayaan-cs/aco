use aco::mutate::rewrites;
use aco::policy::{check, Violation};

const BUBBLE: &str = include_str!("../fixtures/bubble.py");
const CONCAT: &str = include_str!("../fixtures/concat.py");
const LOOKUP: &str = include_str!("../fixtures/lookup.py");
const SLOWER: &str = include_str!("../fixtures/slower.py");

fn rule_names(src: &str) -> Vec<&'static str> {
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
    assert_eq!(rule_names(BUBBLE), ["bubble-sort:sorted", "bubble-sort:list.sort"]);
    assert!(rw[0].source.contains("    xs[:] = sorted(xs)\n    return xs"));
    assert!(!rw[0].source.contains("for j in range"));
    assert!(rw[1].source.contains("xs.sort()"));
}

#[test]
fn bubble_rule_handles_descending() {
    let src = "a = [3, 1, 2]\nfor i in range(3):\n    for j in range(2):\n        if a[j] < a[j + 1]:\n            a[j], a[j + 1] = a[j + 1], a[j]\nprint(a)\n";
    let rw = rewrites(src);
    assert_eq!(rw[0].source, "a = [3, 1, 2]\na[:] = sorted(a, reverse=True)\nprint(a)\n");
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
    assert_eq!(rule_names(LOOKUP), ["membership:set", "membership:frozenset"]);
    let s = &rw[0].source;
    assert!(s.contains("allowed_set = set(allowed)\nfor k in range"));
    assert!(s.contains("if k in allowed_set:"));
}

#[test]
fn slower_edit_is_produced() {
    // The set rule fires on slower.py. The search must measure and reject it.
    let rw = rewrites(SLOWER);
    assert_eq!(rule_names(SLOWER), ["membership:set", "membership:frozenset"]);
    assert!(rw[0].source.contains("    small_set = set(small)\n    for v in (1, 2, 3):"));
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
        ("import sys\n", Violation::BannedImport("sys".into())),
        ("from os.path import join\n", Violation::BannedImport("os".into())),
        ("import urllib.request\n", Violation::BannedImport("urllib".into())),
        ("eval('1')\n", Violation::BannedName("eval".into())),
        ("f = exec\n", Violation::BannedName("exec".into())),
        ("getattr(1, 'real')\n", Violation::BannedName("getattr".into())),
        ("open('/etc/passwd')\n", Violation::BannedName("open".into())),
        (
            "().__class__.__subclasses__()\n",
            Violation::DunderAttribute("__subclasses__".into()),
        ),
        ("x = __builtins__\n", Violation::DunderName("__builtins__".into())),
        ("from . import x\n", Violation::RelativeImport),
    ];
    for (src, want) in cases {
        assert_eq!(check(src).as_ref(), Err(want), "source: {src}");
    }
    assert!(matches!(check("def f(:\n"), Err(Violation::Syntax(_))));
    assert_eq!(check("if __name__ == '__main__':\n    print(1)\n"), Ok(()));
}
