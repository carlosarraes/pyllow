use crate::walker::walk_stmts;
use pyllow_extract::ast::{self, Expr, Ranged, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

/// Default money-shaped name fragments. These are the *terminal* segment
/// (after the last `_`) of identifier names — `unit_price` matches via
/// `price`, `bd_users_total` does NOT match because `total` is excluded
/// here (count vs. dollar ambiguity). Users extend via
/// `[smells.money_as_float].extra_name_patterns` in `pyllow.toml`.
pub(in crate::smells) const DEFAULT_MONEY_WORDS: &[&str] = &[
    "price",
    "amount",
    "cost",
    "fee",
    "subtotal",
    "revenue",
    "payment",
    "discount",
];

pub(in crate::smells) fn check(
    stmts: &[Stmt],
    source: &str,
    path: &Path,
    money_words: &[&str],
    out: &mut Vec<Issue>,
) {
    let mut visit = |stmt: &Stmt| match stmt {
        Stmt::AnnAssign(a) => check_ann_assign(a, source, path, money_words, out),
        Stmt::FunctionDef(f) => check_function_args(&f.args, source, path, money_words, out),
        Stmt::AsyncFunctionDef(f) => check_function_args(&f.args, source, path, money_words, out),
        _ => {}
    };
    walk_stmts(stmts, &mut visit);
}

fn check_ann_assign(
    a: &ast::StmtAnnAssign,
    source: &str,
    path: &Path,
    money_words: &[&str],
    out: &mut Vec<Issue>,
) {
    let Expr::Name(target) = a.target.as_ref() else {
        return;
    };
    let name = target.id.as_str();
    if !is_money_shaped(name, money_words) {
        return;
    }
    if !annotation_contains_float(&a.annotation) {
        return;
    }
    let line = line_at_offset(source, a.range.start().to_usize());
    out.push(Issue::Smell {
        path: path.to_path_buf(),
        line,
        rule: SmellRule::MoneyAsFloat,
        detail: format!(
            "field `{name}` typed as float — use Decimal for monetary values"
        ),
    });
}

fn check_function_args(
    args: &ast::Arguments,
    source: &str,
    path: &Path,
    money_words: &[&str],
    out: &mut Vec<Issue>,
) {
    let all_args = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all_args {
        let name = arg.def.arg.as_str();
        if !is_money_shaped(name, money_words) {
            continue;
        }
        let Some(annotation) = &arg.def.annotation else {
            continue;
        };
        if !annotation_contains_float(annotation) {
            continue;
        }
        let line = line_at_offset(source, annotation.range().start().to_usize());
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::MoneyAsFloat,
            detail: format!(
                "parameter `{name}` typed as float — use Decimal for monetary values"
            ),
        });
    }
}

/// True iff the *terminal* `_`-separated segment of `name` is in
/// `money_words`. So `unit_price` and `monthly_amount` match (terminal is
/// money-shaped) but `bd_users_total` doesn't (terminal `total` isn't in
/// the default set — count/aggregate ambiguity).
fn is_money_shaped(name: &str, money_words: &[&str]) -> bool {
    let last_segment = name.rsplit('_').next().unwrap_or(name);
    money_words.iter().any(|w| *w == last_segment)
}

/// Walks an annotation expression looking for `float` as a leaf Name.
/// Handles bare `float`, `Optional[float]`, `float | None`,
/// `Annotated[float, ...]`, `Union[float, ...]`, etc.
fn annotation_contains_float(expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == "float",
        Expr::BinOp(b) => {
            // PEP 604 union: `float | None`
            annotation_contains_float(&b.left) || annotation_contains_float(&b.right)
        }
        Expr::Subscript(s) => {
            // Optional[float], Annotated[float, Field(...)], dict[str, float], etc.
            annotation_contains_float(&s.value) || annotation_contains_float(&s.slice)
        }
        Expr::Tuple(t) => t.elts.iter().any(annotation_contains_float),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use pyllow_types::Issue;

    fn run_default(src: &str) -> Vec<Issue> {
        let module = parse_source(Path::new("/tmp/test.py"), src).expect("parse");
        let mut out = Vec::new();
        check(
            &module.suite,
            src,
            Path::new("/tmp/test.py"),
            DEFAULT_MONEY_WORDS,
            &mut out,
        );
        out
    }

    fn rule_count(issues: &[Issue]) -> usize {
        issues
            .iter()
            .filter(|i| matches!(i, Issue::Smell { rule: SmellRule::MoneyAsFloat, .. }))
            .count()
    }

    #[test]
    fn fires_on_bare_money_word_field() {
        let src = "from pydantic import BaseModel\nclass O(BaseModel):\n    price: float\n";
        assert_eq!(rule_count(&run_default(src)), 1);
    }

    #[test]
    fn fires_on_suffix_money_field() {
        let src = "class O:\n    unit_price: float\n    total_amount: float | None = None\n";
        assert_eq!(rule_count(&run_default(src)), 2);
    }

    #[test]
    fn fires_on_function_param() {
        let src = "def discount_calc(price: float) -> float:\n    return price\n";
        assert_eq!(rule_count(&run_default(src)), 1);
    }

    #[test]
    fn skips_decimal_typed_field() {
        let src = "from decimal import Decimal\nclass O:\n    price: Decimal\n";
        assert_eq!(rule_count(&run_default(src)), 0);
    }

    #[test]
    fn skips_int_typed_field() {
        let src = "class O:\n    amount: int = 0\n";
        assert_eq!(rule_count(&run_default(src)), 0);
    }

    #[test]
    fn skips_ambiguous_tail_total() {
        // `total` and `tax` are deliberately not in the default words set —
        // `bd_users_total` is a count, `tax_classification` is an identifier.
        let src = "class O:\n    bd_users_total: float\n    tax_classification: float\n";
        assert_eq!(rule_count(&run_default(src)), 0);
    }

    #[test]
    fn skips_percent_named_field() {
        // `discount_percent`'s terminal segment is `percent`, not in money words.
        let src = "class O:\n    discount_percent: float\n    old_discount_percent: float | None = None\n";
        assert_eq!(rule_count(&run_default(src)), 0);
    }

    #[test]
    fn skips_non_money_named_floats() {
        let src = "class O:\n    latitude: float\n    weight_kg: float\n    temperature: float\n";
        assert_eq!(rule_count(&run_default(src)), 0);
    }

    #[test]
    fn fires_through_optional_and_annotated_wrappers() {
        let src = "from typing import Optional, Annotated\nfrom pydantic import Field\nclass O:\n    a_price: Optional[float] = None\n    b_amount: Annotated[float, Field(ge=0.0)] = 0.0\n";
        assert_eq!(rule_count(&run_default(src)), 2);
    }

    #[test]
    fn honors_extra_money_patterns() {
        // With `balance` added to the words, `account_balance: float` fires.
        let src = "class O:\n    account_balance: float\n";
        let module = parse_source(Path::new("/tmp/test.py"), src).unwrap();
        let extra_words = &["price", "amount", "cost", "fee", "balance"];
        let mut out = Vec::new();
        check(&module.suite, src, Path::new("/tmp/test.py"), extra_words, &mut out);
        assert_eq!(rule_count(&out), 1);
    }

    #[test]
    fn name_match_uses_terminal_segment() {
        assert!(is_money_shaped("price", DEFAULT_MONEY_WORDS));
        assert!(is_money_shaped("unit_price", DEFAULT_MONEY_WORDS));
        assert!(is_money_shaped("monthly_revenue", DEFAULT_MONEY_WORDS));
        assert!(!is_money_shaped("price_filter", DEFAULT_MONEY_WORDS));
        assert!(!is_money_shaped("bd_users_total", DEFAULT_MONEY_WORDS));
        assert!(!is_money_shaped("tax_classification", DEFAULT_MONEY_WORDS));
    }
}
