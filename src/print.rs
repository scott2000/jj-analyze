use std::borrow::Cow;
use std::fmt;
use std::ops;
use std::ops::Range;

use colored::Colorize;
use itertools::Itertools as _;
use jj_lib::fileset::FilePattern;
use jj_lib::fileset::FilesetExpression;
use jj_lib::str_util::StringExpression;
use jj_lib::str_util::StringPattern;
use jj_lib::time_util::DatePattern;

use crate::tree::AnalyzeContext;
use crate::tree::AnalyzeCost;
use crate::tree::AnalyzeTree;

pub fn pretty_print(tree: &dyn AnalyzeTree, context: AnalyzeContext, analyze: bool) {
    print_helper(tree, context, 0, analyze);
}

fn print_helper(tree: &dyn AnalyzeTree, context: AnalyzeContext, depth: usize, analyze: bool) {
    let entry = tree.entry(context);
    if analyze {
        let cost = tree.cost(context);
        if cost == AnalyzeCost::Slow {
            print!("{} ", "(EXPENSIVE)".bright_red().bold())
        }
    }
    let name = if analyze {
        match entry.context {
            AnalyzeContext::Eager => entry.name.bright_blue(),
            AnalyzeContext::Lazy => entry.name.bright_cyan(),
            AnalyzeContext::Predicate => entry.name.bright_magenta(),
            AnalyzeContext::Resolved => entry.name.normal(),
        }
    } else if entry.context != AnalyzeContext::Resolved {
        entry.name.blue()
    } else {
        entry.name.normal()
    };
    if entry.children.is_empty() {
        print!("{}", name);
    } else {
        print!("{}", name.bold());
    }
    let (start, end) = if entry.children.iter().any(|child| child.label.is_some()) {
        (" {", "}")
    } else if entry.children.len() == 1 {
        ("(", ")")
    } else {
        (" [", "]")
    };
    if !entry.children.is_empty() {
        print!("{}", start.dimmed());
    }
    println!();
    for child in &entry.children {
        indent(depth + 1);
        if let Some(label) = &child.label {
            print!("{} ", format!("{label}:").dimmed());
            print_helper(child.tree, child.context, depth + 1, analyze);
        } else {
            print_helper(child.tree, child.context, depth + 1, analyze);
        }
    }
    if !entry.children.is_empty() {
        indent(depth);
        println!("{}", end.dimmed());
    }
}

fn indent(depth: usize) {
    print!("{: >depth$}", "", depth = depth * 2)
}

pub fn string_pattern_kind(pattern: &StringPattern) -> &'static str {
    match pattern {
        StringPattern::Exact(_) => "exact",
        StringPattern::ExactI(_) => "exact-i",
        StringPattern::Substring(_) => "substring",
        StringPattern::SubstringI(_) => "substring-i",
        StringPattern::Glob(_) => "glob",
        StringPattern::GlobI(_) => "glob-i",
        StringPattern::Regex(_) => "regex",
        StringPattern::RegexI(_) => "regex-i",
    }
}

pub fn format_string_expression(expr: &StringExpression) -> String {
    match expr {
        StringExpression::Pattern(pattern) => {
            format!("{}:{:?}", string_pattern_kind(pattern), pattern.as_str())
        }
        StringExpression::NotIn(inner) => format!("~{}", format_string_expression(inner)),
        StringExpression::Union(a, b) => format!(
            "({} | {})",
            format_string_expression(a.as_ref()),
            format_string_expression(b.as_ref())
        ),
        StringExpression::Intersection(a, b) => format!(
            "({} & {})",
            format_string_expression(a.as_ref()),
            format_string_expression(b.as_ref())
        ),
    }
}

pub fn format_file_pattern(pattern: &FilePattern) -> String {
    match pattern {
        FilePattern::FilePath(path) => format!("file:{:?}", path.as_internal_file_string()),
        FilePattern::PrefixPath(path) => format!("{:?}", path.as_internal_file_string().to_owned()),
        FilePattern::FileGlob { dir, pattern } => {
            format!("glob:{:?}", dir.to_internal_dir_string() + pattern.glob())
        }
        FilePattern::PrefixGlob { dir, pattern } => format!(
            "prefix-glob:{:?}",
            dir.to_internal_dir_string() + pattern.glob()
        ),
    }
}

pub fn format_fileset_expression(expr: &FilesetExpression) -> Cow<'static, str> {
    match expr {
        FilesetExpression::None => "none()".into(),
        FilesetExpression::All => "all()".into(),
        FilesetExpression::Pattern(pattern) => format_file_pattern(pattern).into(),
        FilesetExpression::UnionAll(exprs) => format!(
            "({})",
            exprs.iter().map(format_fileset_expression).join(" | ")
        )
        .into(),
        FilesetExpression::Intersection(a, b) => format!(
            "({} & {})",
            format_fileset_expression(a.as_ref()),
            format_fileset_expression(b.as_ref())
        )
        .into(),
        FilesetExpression::Difference(a, b) => format!(
            "({} ~ {})",
            format_fileset_expression(a.as_ref()),
            format_fileset_expression(b.as_ref())
        )
        .into(),
    }
}

pub fn format_date_pattern(pattern: &DatePattern) -> String {
    match pattern {
        DatePattern::AtOrAfter(millis_since_epoch) => {
            format!(
                "after:{}",
                chrono::DateTime::from_timestamp_millis(millis_since_epoch.0)
                    .expect("valid date-time")
                    .to_rfc3339()
            )
        }
        DatePattern::Before(millis_since_epoch) => {
            format!(
                "before:{}",
                chrono::DateTime::from_timestamp_millis(millis_since_epoch.0)
                    .expect("valid date-time")
                    .to_rfc3339()
            )
        }
    }
}

pub fn format_range_with_label<T>(
    label: &str,
    range: &Range<T>,
    full_range: Range<T>,
) -> Option<String>
where
    T: Copy + Eq + From<u32> + ops::Sub<Output = T> + fmt::Display,
{
    if range.start == full_range.start && range.end == full_range.end {
        None
    } else if range.start == range.end {
        Some(format!("{label} always out of range"))
    } else if range.start == range.end - T::from(1u32) {
        Some(format!("{label} == {}", range.start))
    } else if range.end == full_range.end {
        Some(format!("{label} >= {}", range.start))
    } else if range.start == full_range.start {
        Some(format!("{label} < {}", range.end))
    } else {
        Some(format!("{} <= {label} < {}", range.start, range.end))
    }
}

pub fn format_range<T>(range: &Range<T>, full_range: Range<T>) -> Option<String>
where
    T: Copy + Eq + From<u32> + ops::Sub<Output = T> + fmt::Display,
{
    if range.start == full_range.start && range.end == full_range.end {
        None
    } else if range.start == range.end {
        Some("empty range".to_owned())
    } else if range.start == range.end - T::from(1u32) {
        Some(range.start.to_string())
    } else if range.end == full_range.end {
        Some(format!("{}..", range.start))
    } else {
        Some(format!("{}..{}", range.start, range.end))
    }
}
