use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use jj_lib::fileset::FilesetExpression;
use jj_lib::revset::GENERATION_RANGE_FULL;
use jj_lib::revset::PARENTS_RANGE_FULL;
use jj_lib::revset::ResolvedExpression;
use jj_lib::revset::ResolvedPredicateExpression;
use jj_lib::revset::RevsetFilterPredicate;

use crate::parse::ReferenceMap;
use crate::print::format_date_pattern;
use crate::print::format_fileset_expression;
use crate::print::format_range;
use crate::print::format_string_expression;
use crate::tree::AnalyzeContext;
use crate::tree::AnalyzeCost;
use crate::tree::AnalyzeTree;
use crate::tree::Child;
use crate::tree::TreeEntry;

#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ResolvedReference<'a>(pub Cow<'a, str>);

impl ResolvedReference<'static> {
    pub const fn new_static(reference: &'static str) -> Self {
        Self(Cow::Borrowed(reference))
    }

    pub const fn root() -> Self {
        Self::new_static("root()")
    }

    pub const fn visible_heads() -> Self {
        Self::new_static("visible_heads()")
    }

    pub const fn visible_heads_or_referenced() -> Self {
        Self::new_static("visible_heads() and referenced revisions")
    }

    pub const fn working_copy() -> Self {
        Self::new_static("@")
    }

    pub fn new_owned(reference: String) -> Self {
        Self(Cow::Owned(reference))
    }
}

impl fmt::Display for ResolvedReference<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AnalyzeTree for ResolvedReference<'_> {
    fn entry(&self, _context: AnalyzeContext) -> TreeEntry<'_> {
        TreeEntry {
            name: self.0.as_ref().into(),
            context: AnalyzeContext::Resolved,
            children: vec![],
        }
    }

    fn cost(&self, _context: AnalyzeContext) -> AnalyzeCost {
        AnalyzeCost::Fast
    }
}

#[derive(Debug)]
pub enum Predicate<'a> {
    Filter(RevsetFilterPredicate),
    Divergent {
        // This isn't actually an expression, but it's easier for us to print it as if it were.
        visible_heads: Box<Expr<'a>>,
    },
    Set(Box<Expr<'a>>),
    NotIn(Box<Self>),
    Union(Vec<Self>),
    Intersection(Vec<Self>),
}

impl<'a> Predicate<'a> {
    pub fn parse(
        predicate_expression: ResolvedPredicateExpression,
        reference_map: &'a ReferenceMap,
    ) -> Self {
        match predicate_expression {
            ResolvedPredicateExpression::Filter(filter) => Self::Filter(filter),
            ResolvedPredicateExpression::Divergent { visible_heads } => Self::Divergent {
                visible_heads: Box::new(Expr::parse(
                    ResolvedExpression::Commits(visible_heads),
                    reference_map,
                )),
            },
            ResolvedPredicateExpression::Set(expr) => {
                Self::Set(Box::new(Expr::parse(*expr, reference_map)))
            }
            ResolvedPredicateExpression::NotIn(expr) => {
                Self::NotIn(Box::new(Self::parse(*expr, reference_map)))
            }
            ResolvedPredicateExpression::Union(expr1, expr2) => {
                let mut result = Vec::new();
                let mut stack = vec![expr2, expr1];
                while let Some(next) = stack.pop() {
                    match *next {
                        ResolvedPredicateExpression::Union(a, b) => {
                            stack.push(b);
                            stack.push(a);
                        }
                        ResolvedPredicateExpression::Set(set) => {
                            match Expr::parse(*set, reference_map) {
                                Expr::Union(exprs) => result.extend(
                                    exprs.into_iter().map(|expr| Self::Set(Box::new(expr))),
                                ),
                                parsed => result.push(Self::Set(Box::new(parsed))),
                            }
                        }
                        _ => result.push(Self::parse(*next, reference_map)),
                    }
                }
                Self::Union(result)
            }
            ResolvedPredicateExpression::Intersection(expr1, expr2) => {
                let mut result = Vec::new();
                let mut stack = vec![expr2, expr1];
                while let Some(next) = stack.pop() {
                    if let ResolvedPredicateExpression::Intersection(a, b) = *next {
                        stack.push(b);
                        stack.push(a);
                    } else {
                        result.push(Self::parse(*next, reference_map));
                    }
                }
                Self::Intersection(result)
            }
        }
    }
}

impl AnalyzeTree for Predicate<'_> {
    fn entry(&self, _context: AnalyzeContext) -> TreeEntry<'_> {
        match self {
            Self::Filter(RevsetFilterPredicate::File(FilesetExpression::All)) => TreeEntry {
                name: "~empty()".to_string().into(),
                context: AnalyzeContext::Predicate,
                children: vec![],
            },
            Self::Filter(filter) => TreeEntry {
                name: filter_to_string(filter),
                context: AnalyzeContext::Predicate,
                children: vec![],
            },
            Self::Divergent { visible_heads } => TreeEntry {
                name: "Divergent".into(),
                context: AnalyzeContext::Predicate,
                children: vec![Child {
                    label: Some("visible_heads".into()),
                    context: AnalyzeContext::Eager,
                    tree: visible_heads.as_ref(),
                }],
            },
            Self::Set(expr) => expr.entry(AnalyzeContext::Predicate),
            Self::NotIn(expr) => match expr.as_ref() {
                Self::Filter(RevsetFilterPredicate::File(FilesetExpression::All)) => TreeEntry {
                    name: "empty()".to_string().into(),
                    context: AnalyzeContext::Predicate,
                    children: vec![],
                },
                Self::Filter(filter) => TreeEntry {
                    name: format!("~{}", filter_to_string(filter)).into(),
                    context: AnalyzeContext::Predicate,
                    children: vec![],
                },
                inner => TreeEntry {
                    name: "NotIn".into(),
                    context: AnalyzeContext::Predicate,
                    children: vec![Child {
                        label: None,
                        context: AnalyzeContext::Predicate,
                        tree: inner,
                    }],
                },
            },
            Self::Union(exprs) => TreeEntry {
                name: "Union".into(),
                context: AnalyzeContext::Predicate,
                children: exprs
                    .iter()
                    .map(|expr| Child {
                        label: None,
                        context: AnalyzeContext::Predicate,
                        tree: expr,
                    })
                    .collect(),
            },
            Self::Intersection(exprs) => TreeEntry {
                name: "Intersection".into(),
                context: AnalyzeContext::Predicate,
                children: exprs
                    .iter()
                    .map(|expr| Child {
                        label: None,
                        context: AnalyzeContext::Predicate,
                        tree: expr,
                    })
                    .collect(),
            },
        }
    }

    fn cost(&self, _context: AnalyzeContext) -> AnalyzeCost {
        if let Self::Set(expr) = self {
            expr.cost(AnalyzeContext::Predicate)
        } else {
            AnalyzeCost::Fast
        }
    }
}

fn filter_to_string(filter: &RevsetFilterPredicate) -> Cow<'static, str> {
    match filter {
        RevsetFilterPredicate::ParentCount(range) => {
            if *range == (2..u32::MAX) {
                "merges()".into()
            } else {
                format!("parent_count({})", format_range(range, PARENTS_RANGE_FULL)).into()
            }
        }
        RevsetFilterPredicate::Description(pattern) => {
            format!("description({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::Subject(pattern) => {
            format!("subject({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::AuthorName(pattern) => {
            format!("author_name({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::AuthorEmail(pattern) => {
            format!("author_email({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::AuthorDate(date_pattern) => {
            format!("author_date({})", format_date_pattern(date_pattern)).into()
        }
        RevsetFilterPredicate::CommitterName(pattern) => {
            format!("committer_name({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::CommitterEmail(pattern) => {
            format!("committer_email({})", format_string_expression(pattern)).into()
        }
        RevsetFilterPredicate::CommitterDate(date_pattern) => {
            format!("committer_date({})", format_date_pattern(date_pattern)).into()
        }
        RevsetFilterPredicate::File(files) => {
            format!("files({})", format_fileset_expression(files)).into()
        }
        RevsetFilterPredicate::DiffLines { text, files } => format!(
            "diff_lines({}, {})",
            format_string_expression(text),
            format_fileset_expression(files)
        )
        .into(),
        RevsetFilterPredicate::HasConflict => "conflicts()".into(),
        RevsetFilterPredicate::Signed => "signed()".into(),
        RevsetFilterPredicate::Extension(ext) => format!("extension({ext:?})").into(),
    }
}

#[derive(Debug)]
pub enum Expr<'a> {
    None,
    Reference(ResolvedReference<'a>),
    Ancestors {
        heads: Box<Self>,
        generation: Range<u64>,
        parents_range: Range<u32>,
    },
    Range {
        roots: Box<Self>,
        heads: Box<Self>,
        generation: Range<u64>,
        parents_range: Range<u32>,
    },
    DagRange {
        roots: Box<Self>,
        heads: Box<Self>,
        generation_from_roots: Range<u64>,
    },
    Reachable {
        sources: Box<Self>,
        domain: Box<Self>,
    },
    Heads(Box<Self>),
    HeadsRange {
        roots: Box<Self>,
        heads: Box<Self>,
        parents_range: Range<u32>,
        filter: Option<Predicate<'a>>,
    },
    Roots(Box<Self>),
    ForkPoint(Box<Self>),
    Bisect(Box<Self>),
    HasSize {
        candidates: Box<Self>,
        count: usize,
    },
    Latest {
        candidates: Box<Self>,
        count: usize,
    },
    Coalesce(Vec<Self>),
    Union(Vec<Self>),
    FilterWithin {
        candidates: Box<Self>,
        predicate: Predicate<'a>,
    },
    Intersection(Vec<Self>),
    Difference(Box<Self>, Box<Self>),
}

impl<'a> Expr<'a> {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn is_root_or_none(&self) -> bool {
        match self {
            Self::None => true,
            Self::Reference(reference) if reference == &ResolvedReference::root() => true,
            Self::Coalesce(exprs) => exprs.iter().all(|expr| expr.is_root_or_none()),
            Self::Intersection(exprs) => exprs.iter().any(|expr| expr.is_root_or_none()),
            Self::Union(exprs) => exprs.iter().all(|expr| expr.is_root_or_none()),
            _ => false,
        }
    }

    pub fn parse(backend_expr: ResolvedExpression, reference_map: &'a ReferenceMap) -> Self {
        let parse = |expr| Box::new(Self::parse(expr, reference_map));

        match backend_expr {
            ResolvedExpression::Commits(commit_ids) if commit_ids.is_empty() => Self::None,
            ResolvedExpression::Commits(commit_ids) if commit_ids.len() == 1 => {
                Self::Reference(reference_map.get(&commit_ids[0]))
            }
            ResolvedExpression::Commits(commit_ids)
                if commit_ids.iter().any(|commit_id| {
                    reference_map.get(commit_id) == ResolvedReference::visible_heads()
                }) =>
            {
                Self::Reference(ResolvedReference::visible_heads_or_referenced())
            }
            ResolvedExpression::Commits(commit_ids) => Self::Union(
                commit_ids
                    .iter()
                    .map(|commit_id| Self::Reference(reference_map.get(commit_id)))
                    .collect(),
            ),
            ResolvedExpression::Ancestors {
                heads,
                generation,
                parents_range,
            } => Self::Ancestors {
                heads: parse(*heads),
                generation,
                parents_range,
            },
            ResolvedExpression::Range {
                roots,
                heads,
                generation,
                parents_range,
            } => Self::Range {
                roots: parse(*roots),
                heads: parse(*heads),
                generation,
                parents_range,
            },
            ResolvedExpression::DagRange {
                roots,
                heads,
                generation_from_roots,
            } => Self::DagRange {
                roots: parse(*roots),
                heads: parse(*heads),
                generation_from_roots,
            },
            ResolvedExpression::Reachable { sources, domain } => Self::Reachable {
                sources: parse(*sources),
                domain: parse(*domain),
            },
            ResolvedExpression::Heads(expr) => Self::Heads(parse(*expr)),
            ResolvedExpression::HeadsRange {
                roots,
                heads,
                parents_range,
                filter,
            } => Self::HeadsRange {
                roots: parse(*roots),
                heads: parse(*heads),
                parents_range,
                filter: filter.map(|predicate| Predicate::parse(predicate, reference_map)),
            },
            ResolvedExpression::Roots(expr) => Self::Roots(parse(*expr)),
            ResolvedExpression::ForkPoint(expr) => Self::ForkPoint(parse(*expr)),
            ResolvedExpression::Bisect(expr) => Self::Bisect(parse(*expr)),
            ResolvedExpression::HasSize { candidates, count } => Self::HasSize {
                candidates: parse(*candidates),
                count,
            },
            ResolvedExpression::Latest { candidates, count } => Self::Latest {
                candidates: parse(*candidates),
                count,
            },
            ResolvedExpression::Coalesce(expr1, expr2) => {
                let mut result = Vec::new();
                let mut stack = vec![expr2, expr1];
                while let Some(next) = stack.pop() {
                    if let ResolvedExpression::Coalesce(a, b) = *next {
                        stack.push(b);
                        stack.push(a);
                    } else {
                        result.push(Self::parse(*next, reference_map));
                    }
                }
                Self::Coalesce(result)
            }
            ResolvedExpression::Union(expr1, expr2) => {
                let mut result = Vec::new();
                let mut stack = vec![expr2, expr1];
                while let Some(next) = stack.pop() {
                    match *next {
                        ResolvedExpression::Union(a, b) => {
                            stack.push(b);
                            stack.push(a);
                        }
                        ResolvedExpression::Commits(commit_ids)
                            if !commit_ids.iter().any(|commit_id| {
                                reference_map.get(commit_id) == ResolvedReference::visible_heads()
                            }) =>
                        {
                            result.extend(
                                commit_ids
                                    .iter()
                                    .map(|commit_id| Self::Reference(reference_map.get(commit_id))),
                            )
                        }
                        _ => result.push(Self::parse(*next, reference_map)),
                    }
                }
                Self::Union(result)
            }
            ResolvedExpression::FilterWithin {
                candidates,
                predicate,
            } => Self::FilterWithin {
                candidates: parse(*candidates),
                predicate: Predicate::parse(predicate, reference_map),
            },
            ResolvedExpression::Intersection(expr1, expr2) => {
                let mut result = Vec::new();
                let mut stack = vec![expr2, expr1];
                while let Some(next) = stack.pop() {
                    if let ResolvedExpression::Intersection(a, b) = *next {
                        stack.push(b);
                        stack.push(a);
                    } else {
                        result.push(Self::parse(*next, reference_map));
                    }
                }
                Self::Intersection(result)
            }
            ResolvedExpression::Difference(expr1, expr2) => {
                Self::Difference(parse(*expr1), parse(*expr2))
            }
        }
    }
}

impl AnalyzeTree for Expr<'_> {
    fn entry(&self, context: AnalyzeContext) -> TreeEntry<'_> {
        match self {
            Self::None => TreeEntry {
                name: "none()".into(),
                context: AnalyzeContext::Resolved,
                children: vec![],
            },
            Self::Reference(reference) => reference.entry(context),
            Self::Ancestors {
                heads,
                generation,
                parents_range,
            } => TreeEntry {
                name: "Ancestors".into(),
                context: context.predicate_to_lazy(),
                children: only_present(vec![
                    (*generation != GENERATION_RANGE_FULL).then(|| Child {
                        label: Some("generation".into()),
                        context,
                        tree: generation,
                    }),
                    (*parents_range != PARENTS_RANGE_FULL).then(|| Child {
                        label: Some("parent_index".into()),
                        context,
                        tree: parents_range,
                    }),
                    Some(Child {
                        label: Some("heads".into()),
                        context: AnalyzeContext::Eager,
                        tree: heads.as_ref(),
                    }),
                ]),
            },
            Self::Range {
                roots,
                heads,
                generation,
                parents_range,
            } => TreeEntry {
                name: "Range".into(),
                context: context.predicate_to_lazy(),
                children: only_present(vec![
                    (*generation != GENERATION_RANGE_FULL).then(|| Child {
                        label: Some("generation".into()),
                        context,
                        tree: generation,
                    }),
                    (*parents_range != PARENTS_RANGE_FULL).then(|| Child {
                        label: Some("parent_index".into()),
                        context,
                        tree: parents_range,
                    }),
                    Some(Child {
                        label: Some("roots".into()),
                        context: AnalyzeContext::Eager,
                        tree: roots.as_ref(),
                    }),
                    Some(Child {
                        label: Some("heads".into()),
                        context: AnalyzeContext::Eager,
                        tree: heads.as_ref(),
                    }),
                ]),
            },
            Self::DagRange {
                roots,
                heads,
                generation_from_roots,
            } => TreeEntry {
                name: "DagRange".into(),
                context: if generation_from_roots == &(1..2) {
                    context.predicate_to_lazy()
                } else {
                    AnalyzeContext::Eager
                },
                children: only_present(vec![
                    (*generation_from_roots != GENERATION_RANGE_FULL).then(|| Child {
                        label: Some("generation_from_roots".into()),
                        context,
                        tree: generation_from_roots,
                    }),
                    Some(Child {
                        label: Some("roots".into()),
                        context: AnalyzeContext::Eager,
                        tree: roots.as_ref(),
                    }),
                    Some(Child {
                        label: Some("heads".into()),
                        context: AnalyzeContext::Eager,
                        tree: heads.as_ref(),
                    }),
                ]),
            },
            Self::Reachable { sources, domain } => TreeEntry {
                name: "Reachable".into(),
                context: AnalyzeContext::Eager,
                children: vec![
                    Child {
                        label: Some("sources".into()),
                        context: AnalyzeContext::Predicate,
                        tree: sources.as_ref(),
                    },
                    Child {
                        label: Some("domain".into()),
                        context: AnalyzeContext::Eager,
                        tree: domain.as_ref(),
                    },
                ],
            },
            Self::Heads(expr) => TreeEntry {
                name: "Heads".into(),
                context: AnalyzeContext::Eager,
                children: vec![Child {
                    label: None,
                    context: AnalyzeContext::Eager,
                    tree: expr.as_ref(),
                }],
            },
            Self::HeadsRange {
                roots,
                heads,
                parents_range,
                filter,
            } => TreeEntry {
                name: "HeadsRange".into(),
                context: AnalyzeContext::Eager,
                children: only_present(vec![
                    (*parents_range != PARENTS_RANGE_FULL).then(|| Child {
                        label: Some("parent_index".into()),
                        context,
                        tree: parents_range,
                    }),
                    Some(Child {
                        label: Some("roots".into()),
                        context: AnalyzeContext::Eager,
                        tree: roots.as_ref(),
                    }),
                    Some(Child {
                        label: Some("heads".into()),
                        context: AnalyzeContext::Eager,
                        tree: heads.as_ref(),
                    }),
                    filter.as_ref().map(|filter| Child {
                        label: Some("filter".into()),
                        context: AnalyzeContext::Predicate,
                        tree: filter,
                    }),
                ]),
            },
            Self::Roots(expr) => TreeEntry {
                name: "Roots".into(),
                context: AnalyzeContext::Eager,
                children: vec![Child {
                    label: None,
                    context: AnalyzeContext::Eager,
                    tree: expr.as_ref(),
                }],
            },
            Self::ForkPoint(expr) => TreeEntry {
                name: "ForkPoint".into(),
                context: AnalyzeContext::Eager,
                children: vec![Child {
                    label: None,
                    context: AnalyzeContext::Eager,
                    tree: expr.as_ref(),
                }],
            },
            Self::Bisect(expr) => TreeEntry {
                name: "Bisect".into(),
                context: AnalyzeContext::Eager,
                children: vec![Child {
                    label: None,
                    context: AnalyzeContext::Eager,
                    tree: expr.as_ref(),
                }],
            },
            Self::HasSize { candidates, count } => TreeEntry {
                name: "HasSize".into(),
                context: AnalyzeContext::Eager,
                children: vec![
                    Child {
                        label: Some("count".into()),
                        context: AnalyzeContext::Resolved,
                        tree: count,
                    },
                    Child {
                        label: Some("candidates".into()),
                        context: AnalyzeContext::Lazy,
                        tree: candidates.as_ref(),
                    },
                ],
            },
            Self::Latest { candidates, count } => TreeEntry {
                name: "Latest".into(),
                context: AnalyzeContext::Eager,
                children: vec![
                    Child {
                        label: Some("count".into()),
                        context: AnalyzeContext::Resolved,
                        tree: count,
                    },
                    Child {
                        label: Some("candidates".into()),
                        context: AnalyzeContext::Eager,
                        tree: candidates.as_ref(),
                    },
                ],
            },
            Self::Coalesce(exprs) => TreeEntry {
                name: "Coalesce".into(),
                context,
                children: exprs
                    .iter()
                    .map(|expr| Child {
                        label: None,
                        context,
                        tree: expr,
                    })
                    .collect(),
            },
            Self::Union(exprs) => TreeEntry {
                name: "Union".into(),
                context,
                children: exprs
                    .iter()
                    .map(|expr| Child {
                        label: None,
                        context,
                        tree: expr,
                    })
                    .collect(),
            },
            Self::FilterWithin {
                candidates,
                predicate,
            } => TreeEntry {
                name: "FilterWithin".into(),
                context,
                children: vec![
                    Child {
                        label: Some("candidates".into()),
                        context,
                        tree: candidates.as_ref(),
                    },
                    Child {
                        label: Some("predicate".into()),
                        context: AnalyzeContext::Predicate,
                        tree: predicate,
                    },
                ],
            },
            Self::Intersection(exprs) => TreeEntry {
                name: "Intersection".into(),
                context,
                children: exprs
                    .iter()
                    .map(|expr| Child {
                        label: None,
                        context: context.eager_to_lazy(),
                        tree: expr,
                    })
                    .collect(),
            },
            Self::Difference(expr1, expr2) => TreeEntry {
                name: "Difference".into(),
                context,
                children: vec![
                    Child {
                        label: Some("candidates".into()),
                        context,
                        tree: expr1.as_ref(),
                    },
                    Child {
                        label: Some("excluded".into()),
                        context: context.eager_to_lazy(),
                        tree: expr2.as_ref(),
                    },
                ],
            },
        }
    }

    fn cost(&self, context: AnalyzeContext) -> AnalyzeCost {
        match self {
            Expr::Ancestors {
                heads,
                generation,
                parents_range,
            } if context == AnalyzeContext::Eager
                && !heads.is_root_or_none()
                && is_large_range(generation) =>
            {
                AnalyzeCost::Slow
            }
            Expr::Range {
                roots,
                heads,
                generation,
                ..
            } if context == AnalyzeContext::Eager
                && roots.is_root_or_none()
                && !heads.is_root_or_none()
                && is_large_range(generation) =>
            {
                AnalyzeCost::Slow
            }
            Expr::DagRange {
                roots,
                heads,
                generation_from_roots,
            } if !roots.is_none()
                && roots.is_root_or_none()
                && !heads.is_root_or_none()
                && is_large_range(generation_from_roots) =>
            {
                AnalyzeCost::Slow
            }
            Expr::Intersection(exprs)
                if exprs
                    .iter()
                    .all(|expr| expr.cost(context) == AnalyzeCost::Slow) =>
            {
                AnalyzeCost::Slow
            }
            _ => AnalyzeCost::Fast,
        }
    }
}

fn only_present(children: Vec<Option<Child>>) -> Vec<Child> {
    children.into_iter().flatten().collect()
}

fn is_large_range(range: &Range<u64>) -> bool {
    range.end.saturating_sub(range.start) >= 10_000
}
