use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use crate::print::format_range;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AnalyzeCost {
    Fast,
    Slow,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum AnalyzeContext {
    Eager,
    Lazy,
    Predicate,
    #[value(skip)]
    Resolved,
}

impl AnalyzeContext {
    pub fn predicate_to_lazy(self) -> Self {
        match self {
            Self::Predicate => Self::Lazy,
            other => other,
        }
    }

    pub fn eager_to_lazy(self) -> Self {
        match self {
            Self::Eager => Self::Lazy,
            other => other,
        }
    }
}

impl fmt::Display for AnalyzeContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eager => write!(f, "eager"),
            Self::Lazy => write!(f, "lazy"),
            Self::Predicate => write!(f, "predicate"),
            Self::Resolved => write!(f, "resolved"),
        }
    }
}

#[derive(Debug)]
pub struct TreeEntry<'a> {
    pub name: Cow<'a, str>,
    pub context: AnalyzeContext,
    pub children: Vec<Child<'a>>,
}

#[derive(Debug)]
pub struct Child<'a> {
    pub label: Option<Cow<'a, str>>,
    pub context: AnalyzeContext,
    pub tree: &'a dyn AnalyzeTree,
}

pub trait AnalyzeTree: fmt::Debug {
    fn entry(&self, context: AnalyzeContext) -> TreeEntry<'_>;
    fn cost(&self, context: AnalyzeContext) -> AnalyzeCost;
}

impl AnalyzeTree for usize {
    fn entry(&self, _context: AnalyzeContext) -> TreeEntry<'_> {
        TreeEntry {
            name: self.to_string().into(),
            context: AnalyzeContext::Resolved,
            children: vec![],
        }
    }

    fn cost(&self, _context: AnalyzeContext) -> AnalyzeCost {
        AnalyzeCost::Fast
    }
}

impl AnalyzeTree for Range<u64> {
    fn entry(&self, _context: AnalyzeContext) -> TreeEntry<'_> {
        TreeEntry {
            name: format_range(self, 0..u64::MAX).unwrap_or_default().into(),
            context: AnalyzeContext::Resolved,
            children: vec![],
        }
    }

    fn cost(&self, _context: AnalyzeContext) -> AnalyzeCost {
        AnalyzeCost::Fast
    }
}

impl AnalyzeTree for Range<u32> {
    fn entry(&self, _context: AnalyzeContext) -> TreeEntry<'_> {
        TreeEntry {
            name: format_range(self, 0..u32::MAX).unwrap_or_default().into(),
            context: AnalyzeContext::Resolved,
            children: vec![],
        }
    }

    fn cost(&self, _context: AnalyzeContext) -> AnalyzeCost {
        AnalyzeCost::Fast
    }
}
