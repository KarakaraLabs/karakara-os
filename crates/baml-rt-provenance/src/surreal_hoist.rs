// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Hoist uncorrelated `IN (SELECT …)` subqueries into `LET` statements.
//!
//! SurrealDB 3.0.x plans `WHERE node_id IN (SELECT … FROM prov_edge …)` as a scan of the
//! outer table (for `prov_node`, usually every node with the filtered label across **all**
//! contexts) and re-evaluates the subquery inside the per-row filter. Read cost therefore grows
//! with the size of the whole store times the size of the context: a 100-row context read took
//! 5.6 s against ~3.4k unrelated nodes and the same read with the subqueries hoisted took 65 ms.
//!
//! [`hoist_in_subqueries`] rewrites a single statement so each subquery runs once. The result
//! is still **one** statement (a block whose value is its last expression), so callers keep
//! reading result `0`:
//!
//! ```text
//! SELECT … FROM prov_node WHERE node_id IN (SELECT VALUE from_id FROM prov_edge WHERE …)
//! ⇒
//! { LET $__hoist_0 = (SELECT VALUE from_id FROM prov_edge WHERE …);
//!   SELECT … FROM prov_node WHERE node_id IN $__hoist_0 }
//! ```
//!
//! Nested subqueries are hoisted innermost-first. The rewrite is only valid for uncorrelated
//! subqueries; provenance queries never reference the outer row (`$parent`), and
//! [`hoist_in_subqueries`] leaves any statement that does unchanged.
//!
//! Hoisting alone still leaves reads proportional to the whole store: the planner prefers any
//! other index (`label = 'X'` scans every node with that label, `rel_type = 'R'` every edge of
//! that type) and turns an `IN` list that is empty or longer than 32 ids into a table scan.
//! Each id lookup is therefore also rewritten to fetch the ids in chunks of 32 point lookups
//! before applying its other filters (see [`chunk_id_lookups`]), with or without a subquery.

const HOIST_PREFIX: &str = "__hoist_";

/// Rewrite every `IN (SELECT …)` in `sql` into a `LET`-bound array inside a single block.
///
/// Also rewrites id lookups ([`chunk_id_lookups`]), with or without a subquery.
/// Returns the input unchanged when it already contains more than one statement or references
/// `$parent`.
pub(crate) fn hoist_in_subqueries(sql: &str) -> String {
    if sql.contains("$parent") || has_statement_separator(sql) {
        return sql.to_string();
    }
    let mut lets = Vec::new();
    let body = hoist_into(sql, &mut lets);
    if lets.is_empty() {
        return body;
    }
    let mut out =
        String::with_capacity(body.len() + lets.iter().map(String::len).sum::<usize>() + 8);
    out.push_str("{ ");
    for stmt in &lets {
        out.push_str(stmt);
        out.push_str("; ");
    }
    out.push_str(body.trim_end().trim_end_matches(';'));
    out.push_str(" }");
    out
}

fn hoist_into(sql: &str, lets: &mut Vec<String>) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut copied = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                i = skip_quoted(bytes, i);
                continue;
            }
            _ => {}
        }
        if let Some((open, select_at)) = in_subquery_at(bytes, i) {
            let Some(close) = matching_paren(bytes, open) else {
                break;
            };
            let inner = &sql[select_at..close];
            let inner = hoist_into(inner.trim_end(), lets);
            let var = format!("${HOIST_PREFIX}{}", lets.len());
            lets.push(format!("LET {var} = ({inner})"));
            out.push_str(&sql[copied..open]);
            out.push_str(&var);
            i = close + 1;
            copied = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&sql[copied..]);
    chunk_id_lookups(&out).unwrap_or(out)
}

/// Largest `IN` array SurrealDB 3.x expands into index point lookups
/// (`MAX_IN_EXPANSION_SIZE` in `surrealdb-core` `exec/index/analysis.rs`). Larger arrays, and
/// empty ones, are planned as a full table scan.
const IN_EXPANSION_LIMIT: usize = 32;

/// Id columns whose lookups are rewritten, per table.
///
/// `prov_node.node_id` is fetched with a closure over chunks, which handles hoisted lists and
/// bound arrays whose length is only known at run time. Every other column is only rewritten
/// for literal lists, unrolled into one subquery per chunk: on SurrealDB 3.0.4 a closure-bound
/// `prov_edge.from_id IN $chunk` returns no rows.
fn id_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "prov_node" => Some(&["node_id"]),
        "prov_edge" => Some(&["from_id", "to_id"]),
        crate::surreal_tables::TBL_PAYLOAD => Some(&["payload_id"]),
        crate::surreal_tables::TBL_PAYLOAD_BLOB => Some(&["content_hash"]),
        _ => None,
    }
}

/// One `field IN …` conjunct that can drive the fetch.
enum IdDriver<'a> {
    /// `$param`: a hoisted list or a bound array (`prov_node` only).
    Param(&'a str),
    /// `['a', 'b', …]`: its elements as written.
    Literal(Vec<&'a str>),
}

/// Rewrite `SELECT … FROM <table> WHERE … <id> IN <ids> …` so the fetch is driven only by the
/// id list and the other conditions apply to the fetched rows.
///
/// Even with subqueries hoisted, the planner does not drive the scan from the id list:
/// - any other indexed predicate wins (`label = 'X'` reads every node with that label in the
///   store, `rel_type = 'R'` every edge of that type);
/// - an `IN` array longer than [`IN_EXPANSION_LIMIT`] or empty becomes a table scan.
///
/// The ids are fetched in chunks of at most [`IN_EXPANSION_LIMIT`], each of which plans as
/// index point lookups:
///
/// ```text
/// SELECT p FROM prov_node WHERE node_id IN $ids AND label = 'X' ORDER BY o
/// ⇒
/// SELECT p FROM array::flatten(array::map(array::clump(array::distinct($ids), 32),
///     |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label = 'X' ORDER BY o
///
/// SELECT from_id FROM prov_edge WHERE rel_type = 'R' AND from_id IN ['a', 'b', …]
/// ⇒
/// SELECT from_id FROM array::flatten([(SELECT * FROM prov_edge WHERE from_id IN ['a', …]),
///     (SELECT * FROM prov_edge WHERE from_id IN [ … ])]) WHERE rel_type = 'R'
/// ```
///
/// `prov_node` accepts hoisted lists, bound arrays and literals (intersected when several).
/// `prov_edge` also accepts hoisted lists and bound arrays, fetched by one equality lookup per
/// id with any literal `rel_type` kept in the lookup. Literal lists of two or more ids on the
/// other columns are unrolled into chunks; a single literal id is usually a hub (an agent, a
/// context) whose full fan-out is larger than the indexed read it replaces.
///
/// Returns `None` (statement left as is) unless the statement is a single-table select over
/// one of those tables whose `WHERE` is a plain `AND` chain containing such a lookup.
fn chunk_id_lookups(stmt: &str) -> Option<String> {
    let words = top_level_words(stmt)?;
    let word = |k: usize| &stmt[words[k].0..words[k].1];
    if !word(0).eq_ignore_ascii_case("SELECT") {
        return None;
    }
    let from = (1..words.len()).find(|&k| word(k).eq_ignore_ascii_case("FROM"))?;
    if words.get(from + 2).is_none() || !word(from + 2).eq_ignore_ascii_case("WHERE") {
        return None;
    }
    let table = word(from + 1);
    let columns = id_columns(table)?;
    let table_end = words[from + 1].1;
    let where_at = words[from + 2];
    // Only whitespace may sit between the table name and WHERE (no alias, WITH, second table).
    if !stmt[table_end..where_at.0].trim().is_empty() {
        return None;
    }
    let cond_start = where_at.1;
    let cond_end = words[from + 3..]
        .iter()
        .find(|(s, e)| is_clause_keyword(&stmt[*s..*e]))
        .map_or(stmt.len(), |(s, _)| *s);
    let mut conjuncts = Vec::new();
    let mut piece_start = cond_start;
    for &(s, e) in &words[from + 3..] {
        if s >= cond_end {
            break;
        }
        let w = &stmt[s..e];
        if w.eq_ignore_ascii_case("OR") {
            return None;
        }
        if w.eq_ignore_ascii_case("AND") {
            conjuncts.push(stmt[piece_start..s].trim());
            piece_start = e;
        }
    }
    conjuncts.push(stmt[piece_start..cond_end].trim());

    let is_node = table == "prov_node";
    let mut drivers: Vec<IdDriver<'_>> = Vec::new();
    let mut driver_column: Option<&str> = None;
    let mut rest: Vec<&str> = Vec::new();
    for c in conjuncts {
        let accepted = columns.iter().find_map(|col| {
            if driver_column.is_some_and(|d| d != *col) {
                return None;
            }
            let driver = id_driver(c, col)?;
            let usable = match &driver {
                IdDriver::Param(_) => is_node || table == "prov_edge",
                IdDriver::Literal(items) => is_node || items.len() >= 2,
            };
            // Edges, payloads and blobs are fetched by one list; the rest stay filters.
            (usable && (is_node || drivers.is_empty())).then_some((*col, driver))
        });
        match accepted {
            Some((col, driver)) => {
                driver_column = Some(col);
                drivers.push(driver);
            }
            None => rest.push(c),
        }
    }
    let column = driver_column?;
    let source = if is_node {
        let operand = |d: &IdDriver<'_>| match d {
            IdDriver::Param(var) => (*var).to_string(),
            IdDriver::Literal(items) => format!("[{}]", items.join(", ")),
        };
        let mut ids = operand(&drivers[0]);
        for d in &drivers[1..] {
            ids = format!("array::intersect({ids}, {})", operand(d));
        }
        format!(
            "array::flatten(array::map(array::clump(array::distinct({ids}), {IN_EXPANSION_LIMIT}), \
             |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c)))"
        )
    } else if let IdDriver::Param(var) = &drivers[0] {
        // A run-time list on prov_edge: one equality lookup per id. (`from_id IN $chunk`
        // inside a closure returns no rows on SurrealDB 3.0.4; equality is planned
        // correctly.) A literal `rel_type` moves into the lookup so it uses the compound
        // `(id, rel_type)` index instead of every edge on a hub node.
        let rel = rest
            .iter()
            .position(|c| literal_rel_type(c))
            .map(|k| rest.remove(k));
        let rel = rel.map_or(String::new(), |c| format!(" AND {c}"));
        format!(
            "array::flatten(array::map(array::distinct({var}), \
             |$__i| (SELECT * FROM {table} WHERE {column} = $__i{rel})))"
        )
    } else {
        let IdDriver::Literal(items) = &drivers[0] else {
            return None;
        };
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&str> = items.iter().copied().filter(|i| seen.insert(*i)).collect();
        let parts: Vec<String> = unique
            .chunks(IN_EXPANSION_LIMIT)
            .map(|chunk| {
                format!(
                    "(SELECT * FROM {table} WHERE {column} IN [{}])",
                    chunk.join(", ")
                )
            })
            .collect();
        format!("array::flatten([{}])", parts.join(", "))
    };
    let mut out = String::with_capacity(stmt.len() + source.len() + 32);
    out.push_str(stmt[..words[from].0].trim_end());
    out.push_str(" FROM ");
    out.push_str(&source);
    if !rest.is_empty() {
        out.push_str(" WHERE ");
        out.push_str(&rest.join(" AND "));
    }
    let (tail, limit, start) = split_limit_start(stmt[cond_end..].trim())?;
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(&tail);
    }
    // SurrealDB 3.0.4 returns the wrong rows for `ORDER BY … LIMIT` over an array source
    // (top-k is not the sorted prefix); sorting alone is correct, so page after sorting.
    Some(match (limit, start) {
        (None, None) => out,
        (Some(limit), None) => format!("array::slice(({out}), 0, {limit})"),
        (None, Some(start)) => format!("array::slice(({out}), {start})"),
        (Some(limit), Some(start)) => {
            format!("array::slice(({out}), {start}, {start} + {limit})")
        }
    })
}

/// `rel_type = 'R'` with a single string literal.
fn literal_rel_type(conjunct: &str) -> bool {
    conjunct
        .strip_prefix("rel_type")
        .map(str::trim_start)
        .and_then(|r| r.strip_prefix('='))
        .map(str::trim)
        .and_then(string_literal)
        .is_some()
}

/// The literal itself when `s` is exactly one quoted string.
fn string_literal(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let first = *bytes.first()?;
    if !matches!(first, b'\'' | b'"') || skip_quoted(bytes, 0) != bytes.len() || bytes.len() < 2 {
        return None;
    }
    (bytes[bytes.len() - 1] == first).then_some(s)
}

/// `<column> IN $ids` or `<column> IN ['a', 'b', …]`.
///
/// Every provenance call site binds `<column> IN $param` to a JSON array of ids.
fn id_driver<'a>(conjunct: &'a str, column: &str) -> Option<IdDriver<'a>> {
    let rest = conjunct.strip_prefix(column)?;
    if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("IN")
        .or_else(|| rest.strip_prefix("in"))?;
    if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    let operand = rest.trim();
    if let Some(name) = operand.strip_prefix('$') {
        return (!name.is_empty() && name.bytes().all(is_ident))
            .then_some(IdDriver::Param(operand));
    }
    string_literal_list(operand).map(IdDriver::Literal)
}

/// Elements of `['a', "b", …]` when every element is a single string literal.
fn string_literal_list(operand: &str) -> Option<Vec<&str>> {
    let inner = operand.strip_prefix('[')?.strip_suffix(']')?;
    let bytes = inner.as_bytes();
    let mut items = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if !matches!(bytes[i], b'\'' | b'"') {
            return None;
        }
        let end = skip_quoted(bytes, i);
        if end > bytes.len() || bytes.get(end - 1) != Some(&bytes[i]) {
            return None;
        }
        items.push(&inner[i..end]);
        i = end;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => break,
            Some(b',') => i += 1,
            Some(_) => return None,
        }
    }
    Some(items)
}

/// Rows of `table` whose `column` is one of `ids`, as a SurrealQL source expression plus one
/// bound chunk of at most [`IN_EXPANSION_LIMIT`] ids per subquery (`$<bind>_0`, `$<bind>_1`, …).
///
/// Use it as `SELECT … FROM <source> WHERE <other filters>`. Each chunk plans as index point
/// lookups on `column`, whereas `column IN $ids AND rel_type = 'R'` scans every `R` edge, or
/// the whole table once the list exceeds 32 ids. Duplicate ids are dropped; no ids yields `[]`.
pub(crate) fn chunked_id_source(
    table: &str,
    column: &str,
    bind: &str,
    ids: &[String],
) -> (String, Vec<(String, serde_json::Value)>) {
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<&String> = ids.iter().filter(|id| seen.insert(id.as_str())).collect();
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    for (k, chunk) in unique.chunks(IN_EXPANSION_LIMIT).enumerate() {
        let name = format!("{bind}_{k}");
        parts.push(format!("(SELECT * FROM {table} WHERE {column} IN ${name})"));
        binds.push((
            name,
            serde_json::Value::Array(
                chunk
                    .iter()
                    .map(|id| serde_json::Value::String((*id).clone()))
                    .collect(),
            ),
        ));
    }
    (format!("array::flatten([{}])", parts.join(", ")), binds)
}

/// Splits `LIMIT [BY] n` / `START [AT] n` out of a clause tail. `None` when either takes
/// anything other than a single literal or `$param`.
fn split_limit_start(tail: &str) -> Option<(String, Option<String>, Option<String>)> {
    let words = top_level_words(tail)?;
    let mut keep = String::with_capacity(tail.len());
    let mut copied = 0;
    let mut limit = None;
    let mut start = None;
    let mut k = 0;
    while k < words.len() {
        let w = &tail[words[k].0..words[k].1];
        let (slot, filler) = if w.eq_ignore_ascii_case("LIMIT") {
            (&mut limit, "BY")
        } else if w.eq_ignore_ascii_case("START") {
            (&mut start, "AT")
        } else {
            k += 1;
            continue;
        };
        let mut v = k + 1;
        if words
            .get(v)
            .is_some_and(|&(s, e)| tail[s..e].eq_ignore_ascii_case(filler))
        {
            v += 1;
        }
        let &(vs, ve) = words.get(v)?;
        let gap_ok = tail[words[v - 1].1..vs].trim().is_empty();
        let next = words.get(v + 1).map_or(tail.len(), |&(s, _)| s);
        if !gap_ok || !tail[ve..next].trim().is_empty() {
            return None;
        }
        *slot = Some(tail[vs..ve].to_string());
        keep.push_str(&tail[copied..words[k].0]);
        copied = ve;
        k = v + 1;
    }
    keep.push_str(&tail[copied..]);
    Some((
        keep.split_whitespace().collect::<Vec<_>>().join(" "),
        limit,
        start,
    ))
}

fn is_clause_keyword(w: &str) -> bool {
    [
        "ORDER",
        "GROUP",
        "LIMIT",
        "START",
        "SPLIT",
        "FETCH",
        "TIMEOUT",
        "PARALLEL",
        "EXPLAIN",
        "TEMPFILES",
        "WITH",
    ]
    .iter()
    .any(|k| w.eq_ignore_ascii_case(k))
}

/// Spans of identifier words outside quotes and brackets. `None` when the statement uses
/// `&&` / `||` at the top level (left alone rather than parsed).
fn top_level_words(sql: &str) -> Option<Vec<(usize, usize)>> {
    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                i = skip_quoted(bytes, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'&' | b'|' if depth == 0 && bytes.get(i + 1) == Some(&bytes[i]) => return None,
            b if depth == 0
                && (is_ident(b) || b == b'$')
                && (i == 0 || !is_word_byte(bytes[i - 1])) =>
            {
                let start = i;
                while i < bytes.len() && is_word_byte(bytes[i]) {
                    i += 1;
                }
                words.push((start, i));
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    Some(words)
}

/// Identifier bytes plus `$`, `.` and `:` so `$__hoist_0`, `props.x` and `math::sum` stay one word.
fn is_word_byte(b: u8) -> bool {
    is_ident(b) || matches!(b, b'$' | b'.' | b':')
}

/// When `bytes[i..]` starts a whole-word `IN`, followed by `(` and then `SELECT`, returns the
/// `(` offset and the `SELECT` offset.
fn in_subquery_at(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if !bytes[i..].starts_with(b"IN") {
        return None;
    }
    if i > 0 && is_ident(bytes[i - 1]) {
        return None;
    }
    let mut j = i + 2;
    if j < bytes.len() && is_ident(bytes[j]) {
        return None;
    }
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'(' {
        return None;
    }
    let open = j;
    j += 1;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let rest = &bytes[j..];
    let is_select = rest.len() >= 6
        && rest[..6].eq_ignore_ascii_case(b"SELECT")
        && rest.get(6).is_none_or(|b| !is_ident(*b));
    is_select.then_some((open, j))
}

fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                i = skip_quoted(bytes, i);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Returns the offset just past the closing quote of the literal starting at `start`.
fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn has_statement_separator(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                i = skip_quoted(bytes, i);
                continue;
            }
            b';' => return bytes[i + 1..].iter().any(|b| !b.is_ascii_whitespace()),
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hoist_matrix() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "no subquery",
                "SELECT * FROM prov_node WHERE label = 'Message'",
                "SELECT * FROM prov_node WHERE label = 'Message'",
            ),
            (
                "prov_node: bound id array without a subquery is chunked",
                "SELECT node_id, label FROM prov_node WHERE node_id IN $node_ids AND label != 'X' ORDER BY node_id",
                "SELECT node_id, label FROM array::flatten(array::map(array::clump(array::distinct($node_ids), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label != 'X' ORDER BY node_id",
            ),
            (
                "prov_node: LIMIT / START are applied after sorting",
                "SELECT node_id FROM prov_node WHERE node_id IN $ids ORDER BY o DESC LIMIT $limit START AT 10 FETCH x",
                "array::slice((SELECT node_id FROM array::flatten(array::map(array::clump(array::distinct($ids), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) ORDER BY o DESC FETCH x), 10, 10 + $limit)",
            ),
            (
                "prov_node: START alone",
                "SELECT node_id FROM prov_node WHERE node_id IN $ids ORDER BY o START 3",
                "array::slice((SELECT node_id FROM array::flatten(array::map(array::clump(array::distinct($ids), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) ORDER BY o), 3)",
            ),
            (
                "prov_node: LIMIT with an expression is left alone",
                "SELECT node_id FROM prov_node WHERE node_id IN $ids ORDER BY o LIMIT ($n + 1)",
                "SELECT node_id FROM prov_node WHERE node_id IN $ids ORDER BY o LIMIT ($n + 1)",
            ),
            (
                "prov_edge literal list: fetch by ids, filter after",
                "SELECT from_id, props.raw AS raw FROM prov_edge WHERE rel_type = 'CITED' AND from_label = 'Message' AND from_id IN ['a', 'b', 'a']",
                "SELECT from_id, props.raw AS raw FROM array::flatten([(SELECT * FROM prov_edge WHERE from_id IN ['a', 'b'])]) WHERE rel_type = 'CITED' AND from_label = 'Message'",
            ),
            (
                "prov_edge literal list over 32 ids is unrolled into chunks",
                "SELECT VALUE to_id FROM prov_edge WHERE from_id IN ['n0', 'n1', 'n2', 'n3', 'n4', 'n5', 'n6', 'n7', 'n8', 'n9', 'n10', 'n11', 'n12', 'n13', 'n14', 'n15', 'n16', 'n17', 'n18', 'n19', 'n20', 'n21', 'n22', 'n23', 'n24', 'n25', 'n26', 'n27', 'n28', 'n29', 'n30', 'n31', 'n32'] AND rel_type = 'R'",
                "SELECT VALUE to_id FROM array::flatten([(SELECT * FROM prov_edge WHERE from_id IN ['n0', 'n1', 'n2', 'n3', 'n4', 'n5', 'n6', 'n7', 'n8', 'n9', 'n10', 'n11', 'n12', 'n13', 'n14', 'n15', 'n16', 'n17', 'n18', 'n19', 'n20', 'n21', 'n22', 'n23', 'n24', 'n25', 'n26', 'n27', 'n28', 'n29', 'n30', 'n31']), (SELECT * FROM prov_edge WHERE from_id IN ['n32'])]) WHERE rel_type = 'R'",
            ),
            (
                "prov_edge single-id literal (hub) is left alone",
                "SELECT VALUE from_id FROM prov_edge WHERE rel_type = 'R' AND to_id IN ['agent:x']",
                "SELECT VALUE from_id FROM prov_edge WHERE rel_type = 'R' AND to_id IN ['agent:x']",
            ),
            (
                "prov_edge: only the first id list drives, later ones stay filters",
                "SELECT * FROM prov_edge WHERE to_id IN ['p', 'q'] AND from_id IN ['a', 'b']",
                "SELECT * FROM array::flatten([(SELECT * FROM prov_edge WHERE to_id IN ['p', 'q'])]) WHERE from_id IN ['a', 'b']",
            ),
            (
                "prov_edge subquery inside a node read: both levels chunked",
                "SELECT props FROM prov_node WHERE node_id IN (SELECT VALUE to_id FROM prov_edge WHERE from_id IN ['t1', 't2'] AND rel_type = 'HAS_PLAN') ORDER BY o DESC",
                "{ LET $__hoist_0 = (SELECT VALUE to_id FROM array::flatten([(SELECT * FROM prov_edge WHERE from_id IN ['t1', 't2'])]) WHERE rel_type = 'HAS_PLAN'); SELECT props FROM array::flatten(array::map(array::clump(array::distinct($__hoist_0), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) ORDER BY o DESC }",
            ),
            (
                "payload literal list",
                "SELECT payload_id FROM provenance_payload WHERE payload_id IN ['p1', \"p2\"]",
                "SELECT payload_id FROM array::flatten([(SELECT * FROM provenance_payload WHERE payload_id IN ['p1', \"p2\"])])",
            ),
            (
                "prov_node literal list uses the closure fetch",
                "SELECT node_id FROM prov_node WHERE label = 'Agent' AND node_id IN ['a']",
                "SELECT node_id FROM array::flatten(array::map(array::clump(array::distinct(['a']), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label = 'Agent'",
            ),
            (
                "list with a non-literal element is left alone",
                "SELECT * FROM prov_edge WHERE from_id IN ['a', $b]",
                "SELECT * FROM prov_edge WHERE from_id IN ['a', $b]",
            ),
            (
                "prov_edge bound id array: per-id equality lookup with the rel_type inside",
                "SELECT from_id, to_id FROM prov_edge WHERE from_id IN $ids AND rel_type = 'R' AND to_label = 'T'",
                "SELECT from_id, to_id FROM array::flatten(array::map(array::distinct($ids), |$__i| (SELECT * FROM prov_edge WHERE from_id = $__i AND rel_type = 'R'))) WHERE to_label = 'T'",
            ),
            (
                "prov_edge hoisted list with a bound rel_type keeps it outside",
                "SELECT VALUE to_id FROM prov_edge WHERE rel_type = $r AND to_id IN (SELECT VALUE x FROM e)",
                "{ LET $__hoist_0 = (SELECT VALUE x FROM e); SELECT VALUE to_id FROM array::flatten(array::map(array::distinct($__hoist_0), |$__i| (SELECT * FROM prov_edge WHERE to_id = $__i))) WHERE rel_type = $r }",
            ),
            (
                "prov_node: a field path is not an id-array driver",
                "SELECT * FROM prov_node WHERE node_id IN $row.ids",
                "SELECT * FROM prov_node WHERE node_id IN $row.ids",
            ),
            (
                "array literal IN is untouched",
                "SELECT * FROM prov_edge WHERE from_label IN ['Message', 'ToolCall']",
                "SELECT * FROM prov_edge WHERE from_label IN ['Message', 'ToolCall']",
            ),
            (
                "single subquery",
                "SELECT node_id FROM prov_node WHERE node_id IN (SELECT VALUE from_id FROM prov_edge WHERE to_id = $c) ORDER BY node_id",
                "{ LET $__hoist_0 = (SELECT VALUE from_id FROM prov_edge WHERE to_id = $c); SELECT node_id FROM array::flatten(array::map(array::clump(array::distinct($__hoist_0), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) ORDER BY node_id }",
            ),
            (
                "prov_node: label and other filters move after the id-driven fetch",
                "SELECT node_id, props FROM prov_node WHERE label = 'Message' AND node_id IN (SELECT VALUE from_id FROM prov_edge WHERE to_id = $c) AND (label != 'ToolCall' OR x IN ['a']) ORDER BY o DESC LIMIT 5",
                "{ LET $__hoist_0 = (SELECT VALUE from_id FROM prov_edge WHERE to_id = $c); array::slice((SELECT node_id, props FROM array::flatten(array::map(array::clump(array::distinct($__hoist_0), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label = 'Message' AND (label != 'ToolCall' OR x IN ['a']) ORDER BY o DESC), 0, 5) }",
            ),
            (
                "prov_node: several id lists are intersected",
                "SELECT count() AS n FROM prov_node WHERE node_id IN (SELECT VALUE a FROM e) AND label = 'LlmCall' AND node_id IN (SELECT VALUE b FROM e) GROUP ALL",
                "{ LET $__hoist_0 = (SELECT VALUE a FROM e); LET $__hoist_1 = (SELECT VALUE b FROM e); SELECT count() AS n FROM array::flatten(array::map(array::clump(array::distinct(array::intersect($__hoist_0, $__hoist_1)), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label = 'LlmCall' GROUP ALL }",
            ),
            (
                "prov_node: nested prov_node subquery is rewritten too",
                "SELECT * FROM prov_edge WHERE from_id IN (SELECT VALUE node_id FROM prov_node WHERE node_id IN (SELECT VALUE to_id FROM prov_edge WHERE r = 'x') AND label = 'T')",
                "{ LET $__hoist_0 = (SELECT VALUE to_id FROM prov_edge WHERE r = 'x'); LET $__hoist_1 = (SELECT VALUE node_id FROM array::flatten(array::map(array::clump(array::distinct($__hoist_0), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label = 'T'); SELECT * FROM array::flatten(array::map(array::distinct($__hoist_1), |$__i| (SELECT * FROM prov_edge WHERE from_id = $__i))) }",
            ),
            (
                "prov_node: top-level OR keeps the plain hoisted form",
                "SELECT * FROM prov_node WHERE node_id IN (SELECT VALUE a FROM e) OR label = 'T'",
                "{ LET $__hoist_0 = (SELECT VALUE a FROM e); SELECT * FROM prov_node WHERE node_id IN $__hoist_0 OR label = 'T' }",
            ),
            (
                "prov_node: || at top level keeps the plain hoisted form",
                "SELECT * FROM prov_node WHERE node_id IN (SELECT VALUE a FROM e) || label = 'T'",
                "{ LET $__hoist_0 = (SELECT VALUE a FROM e); SELECT * FROM prov_node WHERE node_id IN $__hoist_0 || label = 'T' }",
            ),
            (
                "prov_node: NOT IN is not a driver",
                "SELECT * FROM prov_node WHERE label = 'T' AND node_id NOT IN (SELECT VALUE a FROM e)",
                "{ LET $__hoist_0 = (SELECT VALUE a FROM e); SELECT * FROM prov_node WHERE label = 'T' AND node_id NOT IN $__hoist_0 }",
            ),
            (
                "prov_node: a second FROM source is left alone",
                "SELECT * FROM prov_node, prov_edge WHERE node_id IN (SELECT VALUE a FROM e)",
                "{ LET $__hoist_0 = (SELECT VALUE a FROM e); SELECT * FROM prov_node, prov_edge WHERE node_id IN $__hoist_0 }",
            ),
            (
                "prov_node: OMIT projection and keywords inside strings",
                "SELECT node_id, label, props OMIT id FROM prov_node WHERE node_id IN (SELECT VALUE from_id FROM prov_edge WHERE to_id IN $ids AND rel_type = $r) AND label != 'ORDER BY OR'",
                "{ LET $__hoist_0 = (SELECT VALUE from_id FROM array::flatten(array::map(array::distinct($ids), |$__i| (SELECT * FROM prov_edge WHERE to_id = $__i))) WHERE rel_type = $r); SELECT node_id, label, props OMIT id FROM array::flatten(array::map(array::clump(array::distinct($__hoist_0), 32), |$__c| (SELECT * FROM prov_node WHERE node_id IN $__c))) WHERE label != 'ORDER BY OR' }",
            ),
            (
                "two sibling subqueries keep order",
                "SELECT * FROM n WHERE a IN (SELECT VALUE x FROM e) AND b IN (\n SELECT VALUE y FROM e WHERE k IN ['p', 'q']\n)",
                "{ LET $__hoist_0 = (SELECT VALUE x FROM e); LET $__hoist_1 = (SELECT VALUE y FROM e WHERE k IN ['p', 'q']); SELECT * FROM n WHERE a IN $__hoist_0 AND b IN $__hoist_1 }",
            ),
            (
                "nested subqueries hoist innermost first",
                "SELECT * FROM n WHERE node_id IN (SELECT VALUE to_id FROM e WHERE from_id IN (SELECT VALUE from_id FROM e WHERE r = 'x'))",
                "{ LET $__hoist_0 = (SELECT VALUE from_id FROM e WHERE r = 'x'); LET $__hoist_1 = (SELECT VALUE to_id FROM e WHERE from_id IN $__hoist_0); SELECT * FROM n WHERE node_id IN $__hoist_1 }",
            ),
            (
                "parens and IN inside string literals are ignored",
                "SELECT * FROM n WHERE t = 'IN (SELECT nope)' AND a IN (SELECT VALUE x FROM e WHERE s = ')')",
                "{ LET $__hoist_0 = (SELECT VALUE x FROM e WHERE s = ')'); SELECT * FROM n WHERE t = 'IN (SELECT nope)' AND a IN $__hoist_0 }",
            ),
            (
                "identifier ending in IN is not the IN operator",
                "SELECT * FROM n WHERE JOIN (SELECT 1)",
                "SELECT * FROM n WHERE JOIN (SELECT 1)",
            ),
            (
                "multi-statement input is left alone",
                "LET $a = 1; SELECT * FROM n WHERE a IN (SELECT VALUE x FROM e)",
                "LET $a = 1; SELECT * FROM n WHERE a IN (SELECT VALUE x FROM e)",
            ),
            (
                "correlated subquery is left alone",
                "SELECT * FROM n WHERE a IN (SELECT VALUE x FROM e WHERE y = $parent.id)",
                "SELECT * FROM n WHERE a IN (SELECT VALUE x FROM e WHERE y = $parent.id)",
            ),
        ];
        for (name, input, want) in cases {
            assert_eq!(hoist_in_subqueries(input), *want, "{name}");
        }
    }

    #[test]
    fn chunked_id_source_binds_deduplicated_chunks() {
        let ids: Vec<String> = (0..33)
            .map(|i| format!("n{i}"))
            .chain(["n0".into()])
            .collect();
        let (source, binds) = chunked_id_source("prov_edge", "from_id", "ids", &ids);
        assert_eq!(
            source,
            "array::flatten([(SELECT * FROM prov_edge WHERE from_id IN $ids_0), \
             (SELECT * FROM prov_edge WHERE from_id IN $ids_1)])"
        );
        let lens: Vec<usize> = binds
            .iter()
            .map(|(_, v)| v.as_array().map_or(0, Vec::len))
            .collect();
        assert_eq!(lens, [32, 1]);
        assert_eq!(binds[0].0, "ids_0");
        let (empty, none) = chunked_id_source("prov_edge", "from_id", "ids", &[]);
        assert_eq!((empty.as_str(), none.len()), ("array::flatten([])", 0));
    }
}
