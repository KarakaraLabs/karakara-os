// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Record-aware grep + pagination.
//!
//! PageRead keeps physical-line pagination. SearchRead returns complete renderer-identified records
//! when a matching line belongs to one, and falls back to individual matching lines elsewhere.
//! Original archive line numbers are preserved in both modes.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    rendered::{RecordSpan, RenderedContent},
    types::{
        GrepMatch, GrepPage, GrepPageMode, GrepPattern, LineOffset, LineWithPosition, PageLimit,
    },
};

const MAX_EXPANDED_LINES: usize = PageLimit::MAX;
const OVERSIZED_RECORD_EDGE_LINES: usize = 32;

enum MatchDescriptor {
    Line(usize),
    Record {
        span: RecordSpan,
        matched_line_numbers: Vec<usize>,
    },
}

impl MatchDescriptor {
    fn start(&self) -> usize {
        match self {
            Self::Line(index) => *index,
            Self::Record { span, .. } => span.start,
        }
    }
}

/// Filter and paginate rendered content.
///
/// With no grep, `offset` and `limit` count physical lines. With grep, they count result units.
/// A result unit is a complete record when renderer metadata owns the matching line, otherwise it
/// is one matching line. Multiple matching lines in one record produce one result unit.
pub fn grep_paginate(
    content: &RenderedContent,
    grep: Option<&GrepPattern>,
    offset: LineOffset,
    limit: PageLimit,
) -> GrepPage {
    let Some(grep) = grep else {
        return paginate_lines(content, offset, limit);
    };

    let descriptors = collect_match_descriptors(content, grep);
    let total_matched = descriptors.len();
    let mode = match (
        descriptors
            .iter()
            .any(|item| matches!(item, MatchDescriptor::Record { .. })),
        descriptors
            .iter()
            .any(|item| matches!(item, MatchDescriptor::Line(_))),
    ) {
        (true, true) => GrepPageMode::Mixed,
        (true, false) => GrepPageMode::Records,
        _ => GrepPageMode::Lines,
    };

    let start = offset.0.min(total_matched);
    let requested_end = (start + limit.get()).min(total_matched);
    let mut matches = Vec::new();
    let mut expanded_line_count = 0;

    for descriptor in &descriptors[start..requested_end] {
        let item = materialize_match(content, descriptor);
        let item_line_count = item.lines().len();
        if !matches.is_empty() && expanded_line_count + item_line_count > MAX_EXPANDED_LINES {
            break;
        }
        expanded_line_count += item_line_count;
        matches.push(item);
    }

    let next_offset = start + matches.len();
    let has_more = next_offset < total_matched;
    let lines = matches
        .iter()
        .flat_map(|item| item.lines().iter().cloned())
        .collect();

    GrepPage {
        lines,
        matches,
        total_matched,
        mode,
        has_more,
        next_offset,
    }
}

fn paginate_lines(content: &RenderedContent, offset: LineOffset, limit: PageLimit) -> GrepPage {
    let total_matched = content.line_count();
    let start = offset.0.min(total_matched);
    let end = (start + limit.get()).min(total_matched);
    let matches: Vec<GrepMatch> = (start..end)
        .filter_map(|index| positioned_line(content, index).map(GrepMatch::Line))
        .collect();
    let lines = matches
        .iter()
        .flat_map(|item| item.lines().iter().cloned())
        .collect();

    GrepPage {
        lines,
        matches,
        total_matched,
        mode: GrepPageMode::Lines,
        has_more: end < total_matched,
        next_offset: end,
    }
}

fn collect_match_descriptors(
    content: &RenderedContent,
    grep: &GrepPattern,
) -> Vec<MatchDescriptor> {
    let spans = content.record_spans();
    if spans.is_empty() {
        return content
            .lines()
            .enumerate()
            .filter(|(_, line)| grep.matches(line))
            .map(|(index, _)| MatchDescriptor::Line(index))
            .collect();
    }

    let mut record_matches: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut line_matches = Vec::new();
    // Spans are ordered and non-overlapping, so a single moving cursor finds the owning
    // span for each line in amortized O(1) instead of rescanning every span per match.
    let mut span_cursor = 0usize;
    for (line_index, line) in content.lines().enumerate() {
        if !grep.matches(line) {
            continue;
        }
        while span_cursor < spans.len() && spans[span_cursor].end <= line_index {
            span_cursor += 1;
        }
        if span_cursor < spans.len()
            && spans[span_cursor].start <= line_index
            && line_index < spans[span_cursor].end
        {
            record_matches
                .entry(span_cursor)
                .or_default()
                .push(line_index + 1);
        } else {
            line_matches.push(line_index);
        }
    }

    let mut descriptors: Vec<MatchDescriptor> = record_matches
        .into_iter()
        .map(
            |(span_index, matched_line_numbers)| MatchDescriptor::Record {
                span: spans[span_index],
                matched_line_numbers,
            },
        )
        .chain(line_matches.into_iter().map(MatchDescriptor::Line))
        .collect();
    descriptors.sort_by_key(MatchDescriptor::start);
    descriptors
}

fn materialize_match(content: &RenderedContent, descriptor: &MatchDescriptor) -> GrepMatch {
    match descriptor {
        MatchDescriptor::Line(index) => GrepMatch::Line(
            positioned_line(content, *index).expect("match indexes originate from rendered lines"),
        ),
        MatchDescriptor::Record {
            span,
            matched_line_numbers,
        } => {
            let (lines, truncated) = bounded_record_lines(content, *span, matched_line_numbers);
            GrepMatch::Record {
                start_line_number: span.start + 1,
                end_line_number: span.end,
                matched_line_numbers: matched_line_numbers.clone(),
                lines,
                truncated,
            }
        }
    }
}

fn positioned_line(content: &RenderedContent, index: usize) -> Option<LineWithPosition> {
    content.get_line(index).map(|line| LineWithPosition {
        original_line_number: index + 1,
        text: line.to_string(),
    })
}

fn bounded_record_lines(
    content: &RenderedContent,
    span: RecordSpan,
    matched_line_numbers: &[usize],
) -> (Vec<LineWithPosition>, bool) {
    let record_len = span.end - span.start;
    if record_len <= MAX_EXPANDED_LINES {
        return (
            (span.start..span.end)
                .filter_map(|index| positioned_line(content, index))
                .collect(),
            false,
        );
    }

    // Oversized records retain their identity fields at the head, matching lines, and trailing
    // fields such as URLs. The formatter reports the original full span and marks the block as
    // truncated so callers can use PageRead for any omitted range.
    let mut selected = BTreeSet::new();
    for index in span.start..(span.start + OVERSIZED_RECORD_EDGE_LINES).min(span.end) {
        selected.insert(index);
    }
    for index in span.end.saturating_sub(OVERSIZED_RECORD_EDGE_LINES)..span.end {
        selected.insert(index);
    }
    for line_number in matched_line_numbers {
        if selected.len() >= MAX_EXPANDED_LINES {
            break;
        }
        selected.insert(line_number - 1);
    }
    for index in span.start..span.end {
        if selected.len() >= MAX_EXPANDED_LINES {
            break;
        }
        selected.insert(index);
    }

    (
        selected
            .into_iter()
            .filter_map(|index| positioned_line(content, index))
            .collect(),
        true,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::archive_read::render_to_lines;

    fn sample_content() -> RenderedContent {
        RenderedContent::from_lines(vec![
            "- user: alice".to_string(),
            "  text: the deploy pipeline is broken".to_string(),
            "- user: bob".to_string(),
            "  text: I fixed the auth issue".to_string(),
            "- user: alice".to_string(),
            "  text: deploy v2.1 is green on staging".to_string(),
        ])
    }

    #[test]
    fn no_grep_returns_all_lines() {
        let content = sample_content();
        let page = grep_paginate(&content, None, LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 6);
        assert_eq!(page.lines.len(), 6);
        assert_eq!(page.mode, GrepPageMode::Lines);
        assert!(!page.has_more);
    }

    #[test]
    fn legacy_content_filters_lines() {
        let content = sample_content();
        let pattern = GrepPattern::parse("deploy").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 2);
        assert_eq!(page.lines[0].original_line_number, 2);
        assert_eq!(page.lines[1].original_line_number, 6);
        assert_eq!(page.mode, GrepPageMode::Lines);
    }

    #[test]
    fn record_search_returns_complete_deduplicated_records() {
        let content = render_to_lines(&json!({
            "tasks": [
                {"id": "one", "name": "First", "status": "in progress", "note": "in progress again"},
                {"id": "two", "name": "Second", "status": "to do"},
                {"id": "three", "name": "Third", "status": "in progress", "url": "https://example.test/three"}
            ],
            "message": "Found three tasks"
        }));
        let pattern = GrepPattern::parse("in progress").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));

        assert_eq!(page.total_matched, 2);
        assert_eq!(page.matches.len(), 2);
        assert_eq!(page.mode, GrepPageMode::Records);
        assert!(page.lines.iter().any(|line| line.text.contains("id: one")));
        assert!(
            page.lines
                .iter()
                .any(|line| line.text.contains("name: First"))
        );
        assert!(
            page.lines
                .iter()
                .any(|line| line.text.contains("id: three"))
        );
        assert!(
            page.lines
                .iter()
                .any(|line| line.text.contains("example.test"))
        );
        assert!(!page.lines.iter().any(|line| line.text.contains("id: two")));
        match &page.matches[0] {
            GrepMatch::Record {
                matched_line_numbers,
                ..
            } => assert_eq!(matched_line_numbers.len(), 2),
            GrepMatch::Line(_) => panic!("expected record match"),
        }
    }

    #[test]
    fn sibling_matches_remain_line_results() {
        let content = render_to_lines(&json!({
            "tasks": [{"id": "one", "status": "open"}],
            "message": "Found one task"
        }));
        let pattern = GrepPattern::parse("Found one").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        assert_eq!(page.mode, GrepPageMode::Lines);
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.lines[0].text, "message: Found one task");
    }

    #[test]
    fn record_offset_and_limit_count_records() {
        let content = render_to_lines(&json!([
            {"id": 1, "status": "open"},
            {"id": 2, "status": "open"},
            {"id": 3, "status": "open"}
        ]));
        let pattern = GrepPattern::parse("status: open").unwrap();
        let first = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(2));
        assert_eq!(first.matches.len(), 2);
        assert_eq!(first.next_offset, 2);
        assert!(first.has_more);

        let second = grep_paginate(&content, Some(&pattern), LineOffset(2), PageLimit::new(2));
        assert_eq!(second.matches.len(), 1);
        assert_eq!(second.next_offset, 3);
        assert!(!second.has_more);
        assert!(second.lines.iter().any(|line| line.text == "- id: 3"));
    }

    #[test]
    fn oversized_record_is_bounded_and_marked_truncated() {
        let mut lines: Vec<String> = (0..600)
            .map(|index| format!("  field_{index}: value"))
            .collect();
        lines[550] = "  status: in progress".to_string();
        let content = RenderedContent::try_from_lines_with_record_spans(
            lines,
            vec![RecordSpan { start: 0, end: 600 }],
        )
        .expect("record content");
        let pattern = GrepPattern::parse("in progress").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(1));

        assert_eq!(page.lines.len(), MAX_EXPANDED_LINES);
        assert!(
            page.lines
                .iter()
                .any(|line| line.original_line_number == 551)
        );
        match &page.matches[0] {
            GrepMatch::Record { truncated, .. } => assert!(*truncated),
            GrepMatch::Line(_) => panic!("expected record match"),
        }
    }

    #[test]
    fn page_read_offset_and_limit_still_count_lines() {
        let content = render_to_lines(&json!([
            {"id": 1, "status": "open"},
            {"id": 2, "status": "open"}
        ]));
        let page = grep_paginate(&content, None, LineOffset(1), PageLimit::new(2));
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].original_line_number, 2);
        assert_eq!(page.lines[1].original_line_number, 3);
        assert!(page.has_more);
    }
}
