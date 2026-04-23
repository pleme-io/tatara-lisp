//! Source position tracking for the spanned reader/expander pipeline.
//!
//! A `Span` records a half-open byte range `[start, end)` into the original
//! source string the reader was given. Spans are not portable across source
//! inputs — they are meaningful only relative to the string that produced
//! them, which the caller is responsible for holding onto.
//!
//! Nodes produced by macro expansion (not present in the user's source)
//! carry `Span::synthetic()`, which is still a valid `Span` but compares
//! unequal to any real source span.

use std::fmt;

/// A half-open byte range `[start, end)` into the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// A span for nodes produced by macro expansion that have no direct
    /// source origin. `start == end == usize::MAX` is the sentinel.
    pub const fn synthetic() -> Self {
        Self {
            start: usize::MAX,
            end: usize::MAX,
        }
    }

    pub const fn is_synthetic(&self) -> bool {
        self.start == usize::MAX && self.end == usize::MAX
    }

    /// Merge two spans into the smallest span that covers both.
    /// If either is synthetic, returns the other; if both are synthetic,
    /// returns synthetic.
    pub fn merge(self, other: Span) -> Span {
        if self.is_synthetic() {
            return other;
        }
        if other.is_synthetic() {
            return self;
        }
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Resolve `(line, column)` for `byte_offset` in `src` (1-indexed).
    /// Used for human-readable error messages; O(n) in source length — not
    /// for hot paths.
    pub fn line_col(src: &str, byte_offset: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, ch) in src.char_indices() {
            if i >= byte_offset {
                return (line, col);
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_synthetic() {
            f.write_str("<synthetic>")
        } else {
            write!(f, "{}..{}", self.start, self.end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_real_spans() {
        let a = Span::new(5, 10);
        let b = Span::new(8, 20);
        assert_eq!(a.merge(b), Span::new(5, 20));
        assert_eq!(b.merge(a), Span::new(5, 20));
    }

    #[test]
    fn merge_with_synthetic_preserves_real() {
        let a = Span::new(5, 10);
        assert_eq!(a.merge(Span::synthetic()), a);
        assert_eq!(Span::synthetic().merge(a), a);
    }

    #[test]
    fn merge_two_synthetics_is_synthetic() {
        let s = Span::synthetic();
        assert!(s.merge(s).is_synthetic());
    }

    #[test]
    fn line_col_counts_newlines() {
        let src = "abc\nde\nfghi";
        assert_eq!(Span::line_col(src, 0), (1, 1));
        assert_eq!(Span::line_col(src, 2), (1, 3));
        assert_eq!(Span::line_col(src, 4), (2, 1));
        assert_eq!(Span::line_col(src, 7), (3, 1));
        assert_eq!(Span::line_col(src, 10), (3, 4));
    }

    #[test]
    fn display_synthetic() {
        assert_eq!(Span::synthetic().to_string(), "<synthetic>");
    }

    #[test]
    fn display_real() {
        assert_eq!(Span::new(4, 9).to_string(), "4..9");
    }
}
