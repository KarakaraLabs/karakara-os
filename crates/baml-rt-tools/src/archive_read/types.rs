// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Newtypes for the archive read interface.
//!
//! Parse boundaries: validation happens at deserialization, not at query time.

use serde::{Deserialize, Serialize};

/// Short archive ref: legacy flat `@N` (implicit prefix `1`) or composite `@prefix/local`
/// for multi-agent / multi-runtime archive namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShortRef {
    /// Agent-scoped archive namespace within a `context_id` (stable per `agent_id` in Surreal).
    pub prefix: u32,
    /// Monotonic index within `prefix`, allocated via the provenance archive store.
    pub local: u32,
}

/// Stable key for in-memory [`crate::archive_refs::RefTable`] archive rows.
#[inline]
pub fn archive_cell_key(prefix: u32, local: u32) -> u64 {
    ((prefix as u64) << 32) | (local as u64)
}

impl ShortRef {
    /// Legacy helper: prefix `1`, local `n` (flat `@n` on the wire).
    pub fn new(local: u32) -> Self {
        Self { prefix: 1, local }
    }

    pub fn new_prefixed(prefix: u32, local: u32) -> Self {
        Self { prefix, local }
    }

    #[inline]
    pub fn cell_key(self) -> u64 {
        archive_cell_key(self.prefix, self.local)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().strip_prefix('@')?;
        if let Some((p, rest)) = s.split_once('/') {
            let prefix = p.parse::<u32>().ok()?;
            let local = rest.parse::<u32>().ok()?;
            Some(Self { prefix, local })
        } else {
            let local = s.parse::<u32>().ok()?;
            Some(Self { prefix: 1, local })
        }
    }

    /// Parse `@N` or `@p/k`, or the last `@…` suffix (e.g. episode-prefixed `abcd@8` in display strings).
    pub fn parse_loose(s: &str) -> Option<Self> {
        Self::parse(s.trim()).or_else(|| {
            let t = s.trim();
            let i = t.rfind('@')?;
            Self::parse(&t[i..])
        })
    }

    /// Local index only — meaningful for legacy prefix-1 refs and citations.
    #[inline]
    pub fn as_u32(self) -> u32 {
        self.local
    }
}

impl std::fmt::Display for ShortRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.prefix == 1 {
            write!(f, "@{}", self.local)
        } else {
            write!(f, "@{}/{}", self.prefix, self.local)
        }
    }
}

impl Serialize for ShortRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ShortRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        ShortRef::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid archive ref: '{s}' (expected @N or @prefix/local)"
            ))
        })
    }
}

/// History ref (`#N`) for a message or tool-call description in the conversation.
/// Monotonic per conversation context, sharing the same `RefTable` counter as `ShortRef`.
/// Citation-only: cannot be Read. Used in `citations` fields on BAML wrapper types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HistoryRef(u32);

impl HistoryRef {
    pub fn new(n: u32) -> Self {
        Self(n)
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.strip_prefix('#')?.parse::<u32>().ok().map(Self)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for HistoryRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl Serialize for HistoryRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HistoryRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        HistoryRef::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid history ref: '{s}' (expected #N)"))
        })
    }
}

/// Zero-based pagination offset. Counts physical lines for PageRead and filtered result units for
/// SearchRead. The wire field remains `offset` in both operations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LineOffset(pub usize);

/// Maximum result units per page. Units are lines for PageRead and may be records for SearchRead.
#[derive(Debug, Clone, Copy)]
pub struct PageLimit(usize);

impl PageLimit {
    /// Default when the LLM omits limit — large enough to fit typical tool results
    /// without forcing pagination, but bounded to keep prompts manageable.
    pub const DEFAULT: usize = 200;
    pub const MAX: usize = 500;

    pub fn new(n: usize) -> Self {
        Self(n.min(Self::MAX))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for PageLimit {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl Serialize for PageLimit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0 as u64)
    }
}

impl<'de> Deserialize<'de> for PageLimit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = usize::deserialize(deserializer)?;
        Ok(Self::new(n))
    }
}

/// Grep pattern with optional CLI-style flag prefix.
///
/// Parses `-F` (fixed string, default), `-E` (extended regex), `-i` (case
/// insensitive). `-i` combines with either mode (`-iE`, `-iF`); `-F` and `-E`
/// are mutually exclusive and are rejected together. A bare `|` alternation is
/// auto-promoted to regex (e.g. `"task|in progress"`) so plain strings stay
/// literal while the model's natural alternation intent works without an
/// explicit `-E`, but only when every branch is non-empty (`|`, `foo|`, `|foo`,
/// `foo||bar` stay literal). `-F` forces literal matching even when `|` is
/// present. The BAML schema sees a single `string?` field; the flag
/// decomposition is an implementation detail.
///
/// Examples: `"deploy"`, `"-i deploy"`, `"-E error|warn"`, `"-iE \\bauth\\b"`,
/// `"task|in progress"`, `"-F a|b"`
#[derive(Debug, Clone)]
pub struct GrepPattern {
    /// Stripped pattern (flags removed) used for fixed/`-i` matching.
    pattern: String,
    /// Full trimmed input (flags + pattern). Returned by [`Self::pattern_text`] so callers
    /// can persist and later re-parse the grep value without losing its flags.
    input: String,
    /// Pre-lowercased pattern for the fixed+case-insensitive path — avoids
    /// one allocation per `matches()` call.
    pattern_lower: Option<String>,
    mode: GrepMode,
    case_insensitive: bool,
    compiled_regex: Option<regex::Regex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrepMode {
    Fixed,
    Regex,
}

impl GrepPattern {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("grep pattern is empty".to_string());
        }

        let (flags, pattern) = if input.starts_with('-') && input.len() > 1 {
            let space_pos = input.find(' ').unwrap_or(input.len());
            let flag_part = &input[1..space_pos];
            if flag_part.chars().all(|c| matches!(c, 'F' | 'E' | 'i')) {
                let pat = input[space_pos..].trim();
                if pat.is_empty() {
                    return Err("grep pattern is empty after flags".to_string());
                }
                (flag_part, pat)
            } else {
                ("", input)
            }
        } else {
            ("", input)
        };

        let case_insensitive = flags.contains('i');
        let explicit_regex = flags.contains('E');
        let explicit_fixed = flags.contains('F');

        if explicit_regex && explicit_fixed {
            return Err(
                "conflicting flags: -F (literal) and -E (regex) cannot be combined".to_string(),
            );
        }

        // Default is fixed (literal substring). Auto-promote a bare `|` alternation to
        // regex so the model's natural `a|b|c` intent works without requiring `-E`, while
        // plain strings stay literal-safe. `-F` forces literal; `-E` forces regex.
        let mut mode = if explicit_regex {
            GrepMode::Regex
        } else {
            GrepMode::Fixed
        };
        if mode == GrepMode::Fixed && !explicit_fixed && pattern.contains('|') {
            // A bare `|` with non-empty branches is treated as alternation. Empty branches
            // (`|`, `foo|`, `|foo`, `foo||bar`) compile to a regex whose empty alternative
            // matches every line, so they stay fixed; an invalid regex also stays fixed.
            let has_empty_branch = pattern.split('|').any(|branch| branch.is_empty());
            if !has_empty_branch && regex::Regex::new(pattern).is_ok() {
                mode = GrepMode::Regex;
            }
        }

        let compiled_regex = if mode == GrepMode::Regex {
            let re_pattern = if case_insensitive {
                format!("(?i){pattern}")
            } else {
                pattern.to_string()
            };
            Some(regex::Regex::new(&re_pattern).map_err(|e| format!("invalid regex: {e}"))?)
        } else {
            None
        };

        let pattern_lower = if mode == GrepMode::Fixed && case_insensitive {
            Some(pattern.to_lowercase())
        } else {
            None
        };

        Ok(Self {
            pattern: pattern.to_string(),
            input: input.to_string(),
            pattern_lower,
            mode,
            case_insensitive,
            compiled_regex,
        })
    }

    pub fn matches(&self, haystack: &str) -> bool {
        match (&self.mode, &self.compiled_regex) {
            (GrepMode::Regex, Some(re)) => re.is_match(haystack),
            _ if self.case_insensitive => haystack
                .to_lowercase()
                .contains(self.pattern_lower.as_deref().unwrap_or("")),
            _ => haystack.contains(self.pattern.as_str()),
        }
    }

    /// The full reparseable pattern including flags (`-F`, `-E`, `-i` when present).
    ///
    /// Round-trips through [`Self::parse`]: persisting this string and re-parsing it yields the
    /// same fixed/regex mode and case-insensitivity.
    pub fn pattern_text(&self) -> &str {
        &self.input
    }
}

/// A line with its original position in the full rendered content.
#[derive(Debug, Clone)]
pub struct LineWithPosition {
    /// 1-based position in the full `RenderedContent`.
    pub original_line_number: usize,
    pub text: String,
}

/// One paginated SearchRead result unit.
#[derive(Debug, Clone)]
pub enum GrepMatch {
    /// A matching line that is not owned by a renderer-identified record.
    Line(LineWithPosition),
    /// A complete renderer-identified record containing one or more matching lines.
    Record {
        /// 1-based inclusive first archive line.
        start_line_number: usize,
        /// 1-based inclusive last archive line.
        end_line_number: usize,
        /// Original 1-based archive lines that matched the grep pattern.
        matched_line_numbers: Vec<usize>,
        /// Full record lines, or a bounded head/matches/tail projection for an oversized record.
        lines: Vec<LineWithPosition>,
        truncated: bool,
    },
}

impl GrepMatch {
    pub fn lines(&self) -> &[LineWithPosition] {
        match self {
            Self::Line(line) => std::slice::from_ref(line),
            Self::Record { lines, .. } => lines,
        }
    }

    pub fn is_record(&self) -> bool {
        matches!(self, Self::Record { .. })
    }
}

/// Shape of the complete match set before pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepPageMode {
    Lines,
    Records,
    Mixed,
}

/// Result of grep + paginate.
#[derive(Debug, Clone)]
pub struct GrepPage {
    /// Flattened selected result lines for callers that only need `cat -n` output.
    pub lines: Vec<LineWithPosition>,
    /// Selected line or record units, retained for record-aware formatting.
    pub matches: Vec<GrepMatch>,
    /// Number of matching result units. A unit is a record when record metadata applies,
    /// otherwise one matching line.
    pub total_matched: usize,
    pub mode: GrepPageMode,
    pub has_more: bool,
    /// Offset to pass on the next Read call to get the following page.
    /// Equal to the number of result units consumed from the complete match set.
    pub next_offset: usize,
}

/// Default line cap for tool results in [`ProjectionRenderOptions`](crate::prompt_projection::ProjectionRenderOptions).
/// Episode transcript inline rendering uses the same cap so exports match LLM-visible history.
pub const DEFAULT_TOOL_RESULT_INLINE_LINES: usize = 40;

/// Legacy name: `SendDone` no longer inlines archive bodies in `conversation_history`. Kept in case
/// external code referenced this constant; use explicit `PageRead` / `SearchRead` for content.
pub const SEND_DONE_HISTORY_INLINE_LINES: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_read_types_matrix() {
        assert_eq!(ShortRef::parse("@3").unwrap().as_u32(), 3);
        assert_eq!(ShortRef::parse("@0").unwrap().as_u32(), 0);
        for bad in ["3", "@", "@abc", ""] {
            assert!(ShortRef::parse(bad).is_none(), "ShortRef parse {bad:?}");
        }
        assert_eq!(ShortRef::new(42).to_string(), "@42");
        assert_eq!(ShortRef::new_prefixed(2, 5).to_string(), "@2/5");
        let composite = ShortRef::parse("@2/7").expect("composite");
        assert_eq!(composite.prefix, 2);
        assert_eq!(composite.local, 7);
        assert_eq!(composite.as_u32(), 7);

        assert_eq!(HistoryRef::parse("#1").unwrap().as_u32(), 1);
        assert_eq!(HistoryRef::parse("#42").unwrap().as_u32(), 42);
        for bad in ["1", "#", "#abc", ""] {
            assert!(HistoryRef::parse(bad).is_none(), "HistoryRef parse {bad:?}");
        }
        assert_eq!(HistoryRef::new(7).to_string(), "#7");
        assert!(HistoryRef::parse("@3").is_none());
        assert!(ShortRef::parse("#3").is_none());

        assert_eq!(PageLimit::new(1000).get(), 500);
        assert_eq!(PageLimit::new(50).get(), 50);
        assert_eq!(PageLimit::default().get(), 200);

        struct GrepCase {
            label: &'static str,
            pattern: &'static str,
            matches: &'static [&'static str],
            misses: &'static [&'static str],
        }
        let grep_cases = [
            GrepCase {
                label: "fixed",
                pattern: "deploy",
                matches: &["the deploy pipeline"],
                misses: &["the DEPLOY pipeline"],
            },
            GrepCase {
                label: "case_insensitive",
                pattern: "-i deploy",
                matches: &["the DEPLOY pipeline", "deploy"],
                misses: &[],
            },
            GrepCase {
                label: "regex",
                pattern: "-E error|warn",
                matches: &["this is an error", "this is a warning"],
                misses: &["this is fine"],
            },
            GrepCase {
                label: "regex_ci",
                pattern: "-iE error|warn",
                matches: &["ERROR occurred", "Warning: low disk"],
                misses: &[],
            },
            GrepCase {
                label: "fixed_literal",
                pattern: "-F deploy.*pipeline",
                matches: &["the deploy.*pipeline thing"],
                misses: &["the deploy-pipeline thing"],
            },
            GrepCase {
                label: "bare_alternation_auto_regex",
                pattern: "task|in progress|progress",
                matches: &["a task item", "status is in progress", "making progress"],
                misses: &["nothing relevant", "TASK item"],
            },
            GrepCase {
                label: "forced_fixed_pipe",
                pattern: "-F a|b",
                matches: &["the a|b literal"],
                misses: &["a", "b"],
            },
            GrepCase {
                label: "empty_alternation_stays_fixed",
                pattern: "foo||bar",
                matches: &["the foo||bar literal"],
                misses: &["foo", "bar", "foo bar", "nothing"],
            },
            GrepCase {
                label: "leading_empty_branch_stays_fixed",
                pattern: "|foo",
                matches: &["the |foo literal"],
                misses: &["foo", "nothing"],
            },
            GrepCase {
                label: "trailing_empty_branch_stays_fixed",
                pattern: "foo|",
                matches: &["the foo| literal"],
                misses: &["foo", "nothing"],
            },
            GrepCase {
                label: "lone_pipe_stays_fixed",
                pattern: "|",
                matches: &["a | b"],
                misses: &["a", "b", "nothing"],
            },
        ];
        for case in grep_cases {
            let g = GrepPattern::parse(case.pattern).expect(case.label);
            for text in case.matches {
                assert!(g.matches(text), "{}: should match {text:?}", case.label);
            }
            for text in case.misses {
                assert!(
                    !g.matches(text),
                    "{}: should not match {text:?}",
                    case.label
                );
            }
        }
        assert_eq!(
            GrepPattern::parse("-something").unwrap().pattern_text(),
            "-something"
        );
        assert!(GrepPattern::parse("").is_err());
        assert!(GrepPattern::parse("-E ").is_err());
        assert!(GrepPattern::parse("-E [invalid").is_err());
        // `-F` and `-E` conflict: rejecting keeps precedence unambiguous.
        assert!(GrepPattern::parse("-FE a|b").is_err());
        assert!(GrepPattern::parse("-EF a|b").is_err());
        assert!(GrepPattern::parse("-iFE a|b").is_err());

        // `pattern_text` must round-trip flags so persisted/replayed grep values keep the
        // same semantics (no silent `-F`/`-i`/`-E` loss).
        for flagged in ["-F a|b", "-i deploy", "-E ^foo$", "task|in progress"] {
            let parsed = GrepPattern::parse(flagged).expect("parse flagged");
            let reparsed = GrepPattern::parse(parsed.pattern_text()).expect("reparse");
            assert_eq!(parsed.mode, reparsed.mode, "mode round-trip for {flagged}");
            assert_eq!(
                parsed.case_insensitive, reparsed.case_insensitive,
                "case-insensitivity round-trip for {flagged}"
            );
        }
    }
}
