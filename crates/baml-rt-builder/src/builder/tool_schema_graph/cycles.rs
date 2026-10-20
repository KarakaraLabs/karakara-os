// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Recursion analysis for the BAML renderer.
//!
//! The graph records every `SchemaTypeId` edge. This module decides which cycles the BAML target
//! can represent and produces a deterministic, dependency-first emission order.
//!
//! BAML accepts two kinds of recursion:
//!
//! - class recursion that passes through an optional field, a list, a map, or a union with a
//!   non-recursive arm;
//! - alias recursion that passes through a list or a map.
//!
//! Required class dependencies and direct alias references cannot form a cycle. The checks below
//! mirror `baml-core`'s `validate/cycle.rs` so unsupported schemas fail here, with graph symbols
//! and source locations, instead of failing later inside the compiler.

use std::collections::{BTreeMap, BTreeSet};

use super::{SchemaDeclarationKind, SchemaTypeId, SchemaTypeRef, ToolSchemaGraph};
use crate::builder::error::{BamlBuilderError, Result};

/// Validate BAML recursion support and return declarations in dependency-first order.
///
/// Each declaration appears exactly once. Accepted strongly connected components (for example a
/// class that refers to itself through an optional field or a list) are emitted contiguously in
/// their historical discovery order.
pub(crate) fn baml_emission_order(graph: &ToolSchemaGraph) -> Result<Vec<SchemaTypeId>> {
    validate_baml_cycles(graph)?;
    Ok(dependency_order(graph))
}

fn validate_baml_cycles(graph: &ToolSchemaGraph) -> Result<()> {
    validate_alias_cycles(graph)?;
    validate_class_cycles(graph)
}

/// Reject alias cycles that BAML cannot unwind.
///
/// BAML only allows alias cycles that pass through a list or a map. A union, optional wrapper, or
/// direct reference does not introduce the indirection needed to terminate the cycle.
fn validate_alias_cycles(graph: &ToolSchemaGraph) -> Result<()> {
    let nodes: Vec<SchemaTypeId> = graph
        .declarations
        .iter()
        .filter_map(|(id, declaration)| {
            matches!(&declaration.kind, SchemaDeclarationKind::Alias(_)).then_some(*id)
        })
        .collect();
    if nodes.is_empty() {
        return Ok(());
    }

    let mut edges = BTreeMap::new();
    for id in &nodes {
        let SchemaDeclarationKind::Alias(ty) = &graph.declarations[id].kind else {
            continue;
        };
        let mut dependencies = BTreeSet::new();
        collect_alias_dependencies(graph, ty, &mut dependencies);
        edges.insert(*id, dependencies);
    }

    for component in strongly_connected_components(&nodes, &edges) {
        if let Some(cycle) = unsupported_cycle(&component, &edges) {
            return Err(cycle_error(
                graph,
                "type aliases",
                &component,
                &cycle,
                "Recursive aliases must pass through an array or a map.",
            ));
        }
    }
    Ok(())
}

/// Reject required class dependency cycles.
///
/// BAML treats optional fields, lists, maps, and unions with a non-recursive arm as cycle
/// terminators. Required fields that directly reference classes (or aliases resolving to classes)
/// cannot form a cycle.
fn validate_class_cycles(graph: &ToolSchemaGraph) -> Result<()> {
    let nodes: Vec<SchemaTypeId> = graph
        .declarations
        .iter()
        .filter_map(|(id, declaration)| {
            matches!(&declaration.kind, SchemaDeclarationKind::Object(_)).then_some(*id)
        })
        .collect();
    if nodes.is_empty() {
        return Ok(());
    }

    let mut edges = BTreeMap::new();
    for id in &nodes {
        let SchemaDeclarationKind::Object(object) = &graph.declarations[id].kind else {
            continue;
        };
        let mut dependencies = BTreeSet::new();
        for field in &object.fields {
            if !field.required {
                continue;
            }
            let mut alias_stack = BTreeSet::new();
            dependencies.extend(required_class_dependencies(
                graph,
                *id,
                &field.ty,
                &mut alias_stack,
            ));
        }
        edges.insert(*id, dependencies);
    }

    for component in strongly_connected_components(&nodes, &edges) {
        if let Some(cycle) = unsupported_cycle(&component, &edges) {
            return Err(cycle_error(
                graph,
                "classes",
                &component,
                &cycle,
                "Required class recursion must pass through an optional field, an array, a map, or a union with a non-recursive arm.",
            ));
        }
    }
    Ok(())
}

/// Collect direct alias references for BAML's alias dependency graph.
///
/// Lists and maps are deliberately not traversed: they are the only constructs that make an alias
/// cycle finite in BAML. `VecOrOne` keeps its scalar arm because the rendered `T | T[]` union has a
/// direct `T` arm, which BAML's alias validator follows.
fn collect_alias_dependencies(
    graph: &ToolSchemaGraph,
    ty: &SchemaTypeRef,
    dependencies: &mut BTreeSet<SchemaTypeId>,
) {
    match ty {
        SchemaTypeRef::Named(id) => {
            if graph.declarations.get(id).is_some_and(|declaration| {
                matches!(&declaration.kind, SchemaDeclarationKind::Alias(_))
            }) {
                dependencies.insert(*id);
            }
        }
        SchemaTypeRef::Union(arms) => {
            for arm in arms {
                collect_alias_dependencies(graph, arm, dependencies);
            }
        }
        SchemaTypeRef::VecOrOne(inner) => collect_alias_dependencies(graph, inner, dependencies),
        SchemaTypeRef::Array(_) | SchemaTypeRef::Map(_) => {}
        SchemaTypeRef::String
        | SchemaTypeRef::Integer
        | SchemaTypeRef::Number
        | SchemaTypeRef::Boolean
        | SchemaTypeRef::Null
        | SchemaTypeRef::JsonValue
        | SchemaTypeRef::Literal(_) => {}
    }
}

/// Collect the class ids a required field keeps alive, following BAML's dependency rules.
///
/// Alias targets are resolved the way the compiler resolves them. `owner` is used only for the
/// union rule that drops a class's self-reference when the union has another dependency.
fn required_class_dependencies(
    graph: &ToolSchemaGraph,
    owner: SchemaTypeId,
    ty: &SchemaTypeRef,
    alias_stack: &mut BTreeSet<SchemaTypeId>,
) -> BTreeSet<SchemaTypeId> {
    match ty {
        SchemaTypeRef::Named(id) => match graph.declarations.get(id).map(|d| &d.kind) {
            Some(SchemaDeclarationKind::Object(_)) => BTreeSet::from([*id]),
            Some(SchemaDeclarationKind::Enum(_)) => BTreeSet::new(),
            Some(SchemaDeclarationKind::Alias(inner)) => {
                // A structurally recursive alias terminates at its list/map edge. This guard keeps
                // the traversal finite even if a malformed graph contains a direct alias cycle.
                if !alias_stack.insert(*id) {
                    return BTreeSet::new();
                }
                let dependencies = required_class_dependencies(graph, owner, inner, alias_stack);
                alias_stack.remove(id);
                dependencies
            }
            None => BTreeSet::new(),
        },
        SchemaTypeRef::Union(arms) => {
            let mut dependencies = BTreeSet::new();
            for arm in arms {
                let arm_dependencies = required_class_dependencies(graph, owner, arm, alias_stack);
                // A union can stop at any arm, so a non-recursive arm ends the dependency chain.
                if arm_dependencies.is_empty() {
                    return BTreeSet::new();
                }
                dependencies.extend(arm_dependencies);
            }
            if dependencies.len() > 1 {
                dependencies.remove(&owner);
            }
            dependencies
        }
        // A list or map introduces indirection; the compiler does not add a class edge for it.
        SchemaTypeRef::Array(_) | SchemaTypeRef::Map(_) => BTreeSet::new(),
        // `T | T[]`: the list arm is always available, so the union is finite.
        SchemaTypeRef::VecOrOne(_) => BTreeSet::new(),
        SchemaTypeRef::String
        | SchemaTypeRef::Integer
        | SchemaTypeRef::Number
        | SchemaTypeRef::Boolean
        | SchemaTypeRef::Null
        | SchemaTypeRef::JsonValue
        | SchemaTypeRef::Literal(_) => BTreeSet::new(),
    }
}

/// Return a representative cycle if the component is recursive, otherwise `None`.
fn unsupported_cycle(
    component: &[SchemaTypeId],
    edges: &BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>>,
) -> Option<Vec<SchemaTypeId>> {
    match component {
        [] => None,
        [only] => edges
            .get(only)
            .is_some_and(|dependencies| dependencies.contains(only))
            .then(|| vec![*only, *only]),
        _ => Some(find_cycle_path(component, edges)),
    }
}

/// Find one simple cycle inside a strongly connected component.
///
/// Neighbours are visited in stable id order so diagnostics are deterministic across runs.
fn find_cycle_path(
    component: &[SchemaTypeId],
    edges: &BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>>,
) -> Vec<SchemaTypeId> {
    let members: BTreeSet<SchemaTypeId> = component.iter().copied().collect();
    let start = *component.iter().min().expect("non-empty component");

    let mut path = vec![start];
    let mut on_path = BTreeSet::from([start]);
    let mut stack = vec![CycleFrame::new(start, &members, edges)];

    while let Some(frame) = stack.last_mut() {
        let Some(next) = frame.next_neighbor() else {
            on_path.remove(&frame.node);
            path.pop();
            stack.pop();
            continue;
        };
        if next == start || on_path.contains(&next) {
            let position = path
                .iter()
                .position(|id| *id == next)
                .expect("cycle target is on the current path");
            let mut cycle = path[position..].to_vec();
            cycle.push(next);
            return cycle;
        }
        path.push(next);
        on_path.insert(next);
        stack.push(CycleFrame::new(next, &members, edges));
    }

    // SCCs always contain a cycle; fall back to the member list if the traversal cannot find it.
    let mut fallback = component.to_vec();
    if let Some(first) = fallback.first().copied() {
        fallback.push(first);
    }
    fallback
}

struct CycleFrame {
    node: SchemaTypeId,
    neighbors: Vec<SchemaTypeId>,
    next: usize,
}

impl CycleFrame {
    fn new(
        node: SchemaTypeId,
        members: &BTreeSet<SchemaTypeId>,
        edges: &BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>>,
    ) -> Self {
        let mut neighbors: Vec<SchemaTypeId> = edges
            .get(&node)
            .into_iter()
            .flatten()
            .filter(|dependency| members.contains(dependency))
            .copied()
            .collect();
        neighbors.sort_unstable();
        Self {
            node,
            neighbors,
            next: 0,
        }
    }

    fn next_neighbor(&mut self) -> Option<SchemaTypeId> {
        let neighbor = self.neighbors.get(self.next).copied()?;
        self.next += 1;
        Some(neighbor)
    }
}

fn cycle_error(
    graph: &ToolSchemaGraph,
    kind: &str,
    component: &[SchemaTypeId],
    cycle: &[SchemaTypeId],
    guidance: &str,
) -> BamlBuilderError {
    let path = cycle
        .iter()
        .map(|id| symbol(graph, *id))
        .collect::<Vec<_>>()
        .join(" -> ");

    let mut participants: Vec<String> = component
        .iter()
        .map(|id| {
            let declaration = &graph.declarations[id];
            format!("{} ({})", declaration.symbol, declaration.source)
        })
        .collect();
    participants.sort();

    BamlBuilderError::InvalidArgument(format!(
        "unsupported recursive {kind} for generated BAML: dependency cycle {path}; participating symbols: {}; {guidance}",
        participants.join(", ")
    ))
}

fn symbol(graph: &ToolSchemaGraph, id: SchemaTypeId) -> &str {
    graph
        .declarations
        .get(&id)
        .map_or("<unresolved>", |declaration| declaration.symbol.as_str())
}

/// Emit dependencies before dependents, with the historical discovery order as the tie-breaker.
fn dependency_order(graph: &ToolSchemaGraph) -> Vec<SchemaTypeId> {
    let nodes: Vec<SchemaTypeId> = graph.declarations.keys().copied().collect();
    let edges: BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>> = graph
        .declarations
        .iter()
        .map(|(id, declaration)| (*id, declaration.dependencies.clone()))
        .collect();

    let mut components = strongly_connected_components(&nodes, &edges);
    for component in &mut components {
        component.sort_by_key(|id| graph.declarations[id].order);
    }
    components.sort_by_key(|component| {
        (
            component_min_order(graph, component),
            component.first().copied().map(SchemaTypeId::as_u32),
        )
    });

    let mut component_of = BTreeMap::new();
    for (index, component) in components.iter().enumerate() {
        for id in component {
            component_of.insert(*id, index);
        }
    }

    let mut dependents: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); components.len()];
    let mut unresolved: Vec<usize> = vec![0; components.len()];
    for (index, component) in components.iter().enumerate() {
        for id in component {
            for dependency in &graph.declarations[id].dependencies {
                if let Some(&dependency_index) = component_of.get(dependency)
                    && dependency_index != index
                    && dependents[dependency_index].insert(index)
                {
                    unresolved[index] += 1;
                }
            }
        }
    }

    let mut ready: BTreeSet<(u32, usize)> = components
        .iter()
        .enumerate()
        .filter(|(index, _)| unresolved[*index] == 0)
        .map(|(index, component)| (component_min_order(graph, component), index))
        .collect();

    let mut order = Vec::with_capacity(nodes.len());
    while let Some((_, index)) = ready.pop_first() {
        order.extend(components[index].iter().copied());
        for dependent in &dependents[index] {
            unresolved[*dependent] -= 1;
            if unresolved[*dependent] == 0 {
                ready.insert((
                    component_min_order(graph, &components[*dependent]),
                    *dependent,
                ));
            }
        }
    }

    debug_assert_eq!(
        order.len(),
        nodes.len(),
        "every declaration must be emitted once"
    );
    order
}

fn component_min_order(graph: &ToolSchemaGraph, component: &[SchemaTypeId]) -> u32 {
    component
        .iter()
        .map(|id| graph.declarations[id].order)
        .min()
        .unwrap_or(u32::MAX)
}

/// Kosaraju's algorithm, implemented iteratively so deeply nested schemas cannot overflow the
/// stack. Nodes and edges are `BTreeMap`/`BTreeSet`, so the result is deterministic.
fn strongly_connected_components(
    nodes: &[SchemaTypeId],
    edges: &BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>>,
) -> Vec<Vec<SchemaTypeId>> {
    let members: BTreeSet<SchemaTypeId> = nodes.iter().copied().collect();
    let mut reverse: BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>> = BTreeMap::new();
    for node in nodes {
        reverse.entry(*node).or_default();
    }
    for (from, dependencies) in edges {
        for dependency in dependencies {
            if members.contains(dependency) {
                reverse.entry(*dependency).or_default().insert(*from);
            }
        }
    }

    let mut visited = BTreeSet::new();
    let mut finish = Vec::with_capacity(nodes.len());
    for node in nodes {
        if !visited.contains(node) {
            finish_order(*node, edges, &members, &mut visited, &mut finish);
        }
    }

    let mut assigned = BTreeSet::new();
    let mut components = Vec::new();
    for node in finish.iter().rev() {
        if !assigned.insert(*node) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![*node];
        while let Some(current) = stack.pop() {
            component.push(current);
            if let Some(predecessors) = reverse.get(&current) {
                for predecessor in predecessors {
                    if assigned.insert(*predecessor) {
                        stack.push(*predecessor);
                    }
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }
    components
}

fn finish_order(
    start: SchemaTypeId,
    edges: &BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>>,
    members: &BTreeSet<SchemaTypeId>,
    visited: &mut BTreeSet<SchemaTypeId>,
    finish: &mut Vec<SchemaTypeId>,
) {
    let mut stack = vec![(start, false)];
    while let Some((node, expanded)) = stack.pop() {
        if expanded {
            finish.push(node);
            continue;
        }
        if !visited.insert(node) {
            continue;
        }
        stack.push((node, true));
        if let Some(dependencies) = edges.get(&node) {
            for dependency in dependencies.iter().rev() {
                if members.contains(dependency) && !visited.contains(dependency) {
                    stack.push((*dependency, false));
                }
            }
        }
    }
}
