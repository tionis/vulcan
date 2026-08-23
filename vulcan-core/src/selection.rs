//! Shared additive note selection for exports and publication workflows.

use crate::permissions::PermissionFilter;
use crate::{
    execute_query_report_with_filter, export_graph_with_filter, resolve_note_reference_with_filter,
    NoteRecord, QueryAst, QueryReport, VaultPaths,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};

pub const DEFAULT_SELECTION_MAX_NODES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GraphSelectionDirection {
    Outgoing,
    Incoming,
    #[default]
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelectionClause {
    Query {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_json: Option<String>,
    },
    Graph {
        seeds: Vec<String>,
        #[serde(default)]
        direction: GraphSelectionDirection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<usize>,
        #[serde(default = "default_true")]
        include_seeds: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_query_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        traverse_query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        traverse_query_json: Option<String>,
    },
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SelectionExclusions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_json: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SelectionPlan {
    #[serde(default)]
    pub clauses: Vec<SelectionClause>,
    #[serde(default, skip_serializing_if = "SelectionExclusions::is_empty")]
    pub exclusions: SelectionExclusions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<usize>,
}

impl SelectionExclusions {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.query.is_none() && self.query_json.is_none() && self.paths.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionProvenance {
    pub path: String,
    pub clause_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectionReport {
    pub selection: SelectionPlan,
    pub notes: Vec<NoteRecord>,
    pub provenance: Vec<SelectionProvenance>,
}

#[derive(Debug)]
pub struct SelectionError(String);

impl Display for SelectionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SelectionError {}

fn selection_error(error: impl Display) -> SelectionError {
    SelectionError(error.to_string())
}

fn validate_query_pair(
    query: Option<&str>,
    query_json: Option<&str>,
    label: &str,
) -> Result<(), SelectionError> {
    if query.is_some() && query_json.is_some() {
        return Err(selection_error(format!(
            "{label} must set only one of `query` or `query_json`"
        )));
    }
    Ok(())
}

pub fn validate_selection_plan(plan: &SelectionPlan) -> Result<(), SelectionError> {
    if plan.clauses.is_empty() {
        return Err(selection_error(
            "selection plan must contain at least one additive clause",
        ));
    }
    if plan.max_nodes == Some(0) {
        return Err(selection_error(
            "selection max_nodes must be greater than zero",
        ));
    }
    validate_query_pair(
        plan.exclusions.query.as_deref(),
        plan.exclusions.query_json.as_deref(),
        "selection exclusions",
    )?;
    for (index, clause) in plan.clauses.iter().enumerate() {
        match clause {
            SelectionClause::Query { query, query_json } => {
                validate_query_pair(
                    query.as_deref(),
                    query_json.as_deref(),
                    &format!("selection clause {index}"),
                )?;
                if query.is_none() && query_json.is_none() {
                    return Err(selection_error(format!(
                        "query selection clause {index} must set `query` or `query_json`"
                    )));
                }
            }
            SelectionClause::Graph {
                seeds,
                result_query,
                result_query_json,
                traverse_query,
                traverse_query_json,
                ..
            } => {
                if seeds.is_empty() || seeds.iter().any(|seed| seed.trim().is_empty()) {
                    return Err(selection_error(format!(
                        "graph selection clause {index} must contain at least one non-empty seed"
                    )));
                }
                validate_query_pair(
                    result_query.as_deref(),
                    result_query_json.as_deref(),
                    &format!("selection clause {index} result query"),
                )?;
                validate_query_pair(
                    traverse_query.as_deref(),
                    traverse_query_json.as_deref(),
                    &format!("selection clause {index} traverse query"),
                )?;
            }
        }
    }
    Ok(())
}

fn parse_query(query: Option<&str>, query_json: Option<&str>) -> Result<QueryAst, SelectionError> {
    match (query, query_json) {
        (Some(query), None) => QueryAst::from_dsl(query).map_err(selection_error),
        (None, Some(query_json)) => QueryAst::from_json(query_json).map_err(selection_error),
        (None, None) => QueryAst::from_dsl("from notes").map_err(selection_error),
        (Some(_), Some(_)) => Err(selection_error(
            "provide either a query or query_json, not both",
        )),
    }
}

fn query_path_set(
    paths: &VaultPaths,
    query: Option<&str>,
    query_json: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<HashSet<String>, SelectionError> {
    let ast = parse_query(query, query_json)?;
    let report = execute_query_report_with_filter(paths, ast, filter).map_err(selection_error)?;
    Ok(report
        .notes
        .into_iter()
        .map(|note| note.document_path)
        .collect())
}

struct SelectionAccumulator<'a> {
    paths: &'a VaultPaths,
    filter: Option<&'a PermissionFilter>,
    excluded: HashSet<String>,
    outgoing: HashMap<String, Vec<String>>,
    incoming: HashMap<String, Vec<String>>,
    max_nodes: usize,
    selected: BTreeSet<String>,
    provenance: Vec<SelectionProvenance>,
}

#[derive(Clone, Copy)]
struct GraphClause<'a> {
    seeds: &'a [String],
    direction: GraphSelectionDirection,
    depth: Option<usize>,
    include_seeds: bool,
    result_query: Option<&'a str>,
    result_query_json: Option<&'a str>,
    traverse_query: Option<&'a str>,
    traverse_query_json: Option<&'a str>,
}

impl SelectionAccumulator<'_> {
    fn add_clause(
        &mut self,
        clause_index: usize,
        clause: &SelectionClause,
    ) -> Result<(), SelectionError> {
        match clause {
            SelectionClause::Query { query, query_json } => {
                self.add_query(clause_index, query.as_deref(), query_json.as_deref())
            }
            SelectionClause::Graph {
                seeds,
                direction,
                depth,
                include_seeds,
                result_query,
                result_query_json,
                traverse_query,
                traverse_query_json,
            } => self.add_graph(
                clause_index,
                GraphClause {
                    seeds,
                    direction: *direction,
                    depth: *depth,
                    include_seeds: *include_seeds,
                    result_query: result_query.as_deref(),
                    result_query_json: result_query_json.as_deref(),
                    traverse_query: traverse_query.as_deref(),
                    traverse_query_json: traverse_query_json.as_deref(),
                },
            ),
        }
    }

    fn add_query(
        &mut self,
        clause_index: usize,
        query: Option<&str>,
        query_json: Option<&str>,
    ) -> Result<(), SelectionError> {
        let mut matches = query_path_set(self.paths, query, query_json, self.filter)?
            .into_iter()
            .collect::<Vec<_>>();
        matches.sort();
        for path in matches {
            if self.excluded.contains(&path) {
                continue;
            }
            self.selected.insert(path.clone());
            self.provenance.push(SelectionProvenance {
                path,
                clause_index,
                seed: None,
                depth: None,
            });
        }
        Ok(())
    }

    fn add_graph(
        &mut self,
        clause_index: usize,
        clause: GraphClause<'_>,
    ) -> Result<(), SelectionError> {
        let result_paths =
            self.optional_query_paths(clause.result_query, clause.result_query_json)?;
        let traverse_paths =
            self.optional_query_paths(clause.traverse_query, clause.traverse_query_json)?;
        for seed_identifier in clause.seeds {
            self.traverse_seed(
                clause_index,
                seed_identifier,
                &clause,
                result_paths.as_ref(),
                traverse_paths.as_ref(),
            )?;
        }
        Ok(())
    }

    fn optional_query_paths(
        &self,
        query: Option<&str>,
        query_json: Option<&str>,
    ) -> Result<Option<HashSet<String>>, SelectionError> {
        (query.is_some() || query_json.is_some())
            .then(|| query_path_set(self.paths, query, query_json, self.filter))
            .transpose()
    }

    fn traverse_seed(
        &mut self,
        clause_index: usize,
        seed_identifier: &str,
        clause: &GraphClause<'_>,
        result_paths: Option<&HashSet<String>>,
        traverse_paths: Option<&HashSet<String>>,
    ) -> Result<(), SelectionError> {
        let seed = resolve_note_reference_with_filter(self.paths, seed_identifier, self.filter)
            .map_err(selection_error)?
            .path;
        if self.excluded.contains(&seed) {
            return Ok(());
        }
        let mut visited = HashSet::from([seed.clone()]);
        let mut queue = VecDeque::from([(seed.clone(), 0_usize)]);
        while let Some((current, current_depth)) = queue.pop_front() {
            if (current_depth == 0 && clause.include_seeds)
                || (current_depth > 0
                    && result_paths.is_none_or(|matches| matches.contains(&current)))
            {
                self.selected.insert(current.clone());
                self.provenance.push(SelectionProvenance {
                    path: current.clone(),
                    clause_index,
                    seed: Some(seed.clone()),
                    depth: Some(current_depth),
                });
            }
            if clause.depth.is_some_and(|limit| current_depth >= limit)
                || (current_depth > 0
                    && traverse_paths.is_some_and(|matches| !matches.contains(&current)))
            {
                continue;
            }
            for neighbor in self.neighbors(&current, clause.direction) {
                if self.excluded.contains(&neighbor) || !visited.insert(neighbor.clone()) {
                    continue;
                }
                if visited.len() > self.max_nodes {
                    return Err(selection_error(format!(
                        "graph selection from seed `{seed_identifier}` exceeded max_nodes ({})",
                        self.max_nodes
                    )));
                }
                queue.push_back((neighbor, current_depth + 1));
            }
        }
        Ok(())
    }

    fn neighbors(&self, path: &str, direction: GraphSelectionDirection) -> Vec<String> {
        let mut neighbors = Vec::new();
        if matches!(
            direction,
            GraphSelectionDirection::Outgoing | GraphSelectionDirection::Both
        ) {
            neighbors.extend(self.outgoing.get(path).into_iter().flatten().cloned());
        }
        if matches!(
            direction,
            GraphSelectionDirection::Incoming | GraphSelectionDirection::Both
        ) {
            neighbors.extend(self.incoming.get(path).into_iter().flatten().cloned());
        }
        neighbors.sort();
        neighbors.dedup();
        neighbors
    }
}

type GraphNeighbors = HashMap<String, Vec<String>>;

fn load_selection_edges(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<(GraphNeighbors, GraphNeighbors), SelectionError> {
    let graph = export_graph_with_filter(paths, filter).map_err(selection_error)?;
    let mut outgoing = HashMap::<String, Vec<String>>::new();
    let mut incoming = HashMap::<String, Vec<String>>::new();
    for edge in graph.edges {
        outgoing
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
        incoming.entry(edge.target).or_default().push(edge.source);
    }
    for neighbors in outgoing.values_mut().chain(incoming.values_mut()) {
        neighbors.sort();
        neighbors.dedup();
    }
    Ok((outgoing, incoming))
}

fn load_selection_exclusions(
    paths: &VaultPaths,
    exclusions: &SelectionExclusions,
    filter: Option<&PermissionFilter>,
) -> Result<HashSet<String>, SelectionError> {
    let mut excluded = if exclusions.query.is_some() || exclusions.query_json.is_some() {
        query_path_set(
            paths,
            exclusions.query.as_deref(),
            exclusions.query_json.as_deref(),
            filter,
        )?
    } else {
        HashSet::new()
    };
    for identifier in &exclusions.paths {
        let resolved = resolve_note_reference_with_filter(paths, identifier, filter)
            .map_err(selection_error)?;
        excluded.insert(resolved.path);
    }
    Ok(excluded)
}

pub fn execute_selection_plan(
    paths: &VaultPaths,
    plan: &SelectionPlan,
    filter: Option<&PermissionFilter>,
) -> Result<SelectionReport, SelectionError> {
    validate_selection_plan(plan)?;
    let all_report = execute_query_report_with_filter(
        paths,
        QueryAst::from_dsl("from notes").map_err(selection_error)?,
        filter,
    )
    .map_err(selection_error)?;
    let note_by_path = all_report
        .notes
        .into_iter()
        .map(|note| (note.document_path.clone(), note))
        .collect::<BTreeMap<_, _>>();
    let excluded = load_selection_exclusions(paths, &plan.exclusions, filter)?;
    let (outgoing, incoming) = load_selection_edges(paths, filter)?;
    let mut accumulator = SelectionAccumulator {
        paths,
        filter,
        excluded,
        outgoing,
        incoming,
        max_nodes: plan.max_nodes.unwrap_or(DEFAULT_SELECTION_MAX_NODES),
        selected: BTreeSet::new(),
        provenance: Vec::new(),
    };
    for (index, clause) in plan.clauses.iter().enumerate() {
        accumulator.add_clause(index, clause)?;
    }
    accumulator.provenance.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.clause_index.cmp(&right.clause_index))
            .then(left.seed.cmp(&right.seed))
            .then(left.depth.cmp(&right.depth))
    });
    accumulator.provenance.dedup();
    let notes = accumulator
        .selected
        .into_iter()
        .filter_map(|path| note_by_path.get(&path).cloned())
        .collect();
    Ok(SelectionReport {
        selection: plan.clone(),
        notes,
        provenance: accumulator.provenance,
    })
}

pub fn selection_report_as_query_report(
    report: SelectionReport,
) -> Result<QueryReport, SelectionError> {
    Ok(QueryReport {
        query: QueryAst::from_dsl("from notes").map_err(selection_error)?,
        notes: report.notes,
        selection: Some(report.selection),
        selection_provenance: report.provenance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::fs;
    use tempfile::tempdir;

    fn graph_vault() -> (tempfile::TempDir, VaultPaths) {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("vault");
        fs::create_dir_all(root.join(".vulcan")).expect("vulcan directory");
        fs::write(
            root.join("A.md"),
            "---\nstatus: public\n---\n[[B]]\n[[Private]]\n",
        )
        .expect("A note");
        fs::write(root.join("B.md"), "---\nstatus: bridge\n---\n[[C]]\n").expect("B note");
        fs::write(root.join("C.md"), "---\nstatus: public\n---\n[[A]]\n").expect("C note");
        fs::write(
            root.join("Private.md"),
            "---\nstatus: private\n---\n[[Hidden]]\n",
        )
        .expect("private note");
        fs::write(root.join("Hidden.md"), "---\nstatus: public\n---\n").expect("hidden note");
        fs::write(root.join("Extra.md"), "---\nstatus: extra\n---\n[[B]]\n").expect("extra note");
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).expect("scan graph vault");
        (temp, paths)
    }

    #[test]
    fn additive_graph_clauses_union_multiple_seeds_and_keep_provenance() {
        let (_temp, paths) = graph_vault();
        let plan = SelectionPlan {
            clauses: vec![
                SelectionClause::Graph {
                    seeds: vec!["A".to_string()],
                    direction: GraphSelectionDirection::Outgoing,
                    depth: Some(2),
                    include_seeds: true,
                    result_query: None,
                    result_query_json: None,
                    traverse_query: None,
                    traverse_query_json: None,
                },
                SelectionClause::Graph {
                    seeds: vec!["Extra".to_string()],
                    direction: GraphSelectionDirection::Outgoing,
                    depth: Some(1),
                    include_seeds: true,
                    result_query: None,
                    result_query_json: None,
                    traverse_query: None,
                    traverse_query_json: None,
                },
            ],
            exclusions: SelectionExclusions::default(),
            max_nodes: None,
        };

        let report = execute_selection_plan(&paths, &plan, None).expect("select graph closure");
        assert_eq!(
            report
                .notes
                .iter()
                .map(|note| note.document_path.as_str())
                .collect::<Vec<_>>(),
            [
                "A.md",
                "B.md",
                "C.md",
                "Extra.md",
                "Hidden.md",
                "Private.md"
            ]
        );
        assert!(report.provenance.iter().any(|item| {
            item.path == "B.md"
                && item.clause_index == 0
                && item.seed.as_deref() == Some("A.md")
                && item.depth == Some(1)
        }));
        assert!(report.provenance.iter().any(|item| {
            item.path == "B.md"
                && item.clause_index == 1
                && item.seed.as_deref() == Some("Extra.md")
                && item.depth == Some(1)
        }));
    }

    #[test]
    fn result_and_traverse_queries_have_distinct_semantics() {
        let (_temp, paths) = graph_vault();
        let plan = SelectionPlan {
            clauses: vec![SelectionClause::Graph {
                seeds: vec!["A".to_string()],
                direction: GraphSelectionDirection::Outgoing,
                depth: None,
                include_seeds: true,
                result_query: Some("from notes where status = public".to_string()),
                result_query_json: None,
                traverse_query: Some(
                    "from notes where status matches \"^(public|bridge)$\"".to_string(),
                ),
                traverse_query_json: None,
            }],
            exclusions: SelectionExclusions::default(),
            max_nodes: None,
        };

        let report = execute_selection_plan(&paths, &plan, None).expect("filtered traversal");
        assert_eq!(
            report
                .notes
                .iter()
                .map(|note| note.document_path.as_str())
                .collect::<Vec<_>>(),
            ["A.md", "C.md"]
        );
        assert!(!report
            .provenance
            .iter()
            .any(|item| item.path == "Hidden.md"));
    }

    #[test]
    fn global_exclusions_are_hard_traversal_boundaries() {
        let (_temp, paths) = graph_vault();
        let plan = SelectionPlan {
            clauses: vec![SelectionClause::Graph {
                seeds: vec!["A".to_string()],
                direction: GraphSelectionDirection::Outgoing,
                depth: None,
                include_seeds: true,
                result_query: None,
                result_query_json: None,
                traverse_query: None,
                traverse_query_json: None,
            }],
            exclusions: SelectionExclusions {
                query: None,
                query_json: None,
                paths: vec!["B".to_string(), "Private".to_string()],
            },
            max_nodes: None,
        };

        let report = execute_selection_plan(&paths, &plan, None).expect("excluded traversal");
        assert_eq!(
            report
                .notes
                .iter()
                .map(|note| note.document_path.as_str())
                .collect::<Vec<_>>(),
            ["A.md"]
        );
    }

    #[test]
    fn incoming_direction_selects_backlinks_at_bounded_depth() {
        let (_temp, paths) = graph_vault();
        let plan = SelectionPlan {
            clauses: vec![SelectionClause::Graph {
                seeds: vec!["B".to_string()],
                direction: GraphSelectionDirection::Incoming,
                depth: Some(1),
                include_seeds: true,
                result_query: None,
                result_query_json: None,
                traverse_query: None,
                traverse_query_json: None,
            }],
            exclusions: SelectionExclusions::default(),
            max_nodes: None,
        };
        let report = execute_selection_plan(&paths, &plan, None).expect("incoming selection");
        assert_eq!(
            report
                .notes
                .iter()
                .map(|note| note.document_path.as_str())
                .collect::<Vec<_>>(),
            ["A.md", "B.md", "Extra.md"]
        );
    }

    #[test]
    fn recursive_traversal_fails_loudly_at_node_limit() {
        let (_temp, paths) = graph_vault();
        let plan = SelectionPlan {
            clauses: vec![SelectionClause::Graph {
                seeds: vec!["A".to_string()],
                direction: GraphSelectionDirection::Outgoing,
                depth: None,
                include_seeds: true,
                result_query: None,
                result_query_json: None,
                traverse_query: None,
                traverse_query_json: None,
            }],
            exclusions: SelectionExclusions::default(),
            max_nodes: Some(2),
        };
        let error = execute_selection_plan(&paths, &plan, None).expect_err("node limit");
        assert!(error.to_string().contains("exceeded max_nodes (2)"));
    }

    #[test]
    fn selection_plan_json_round_trips_tagged_clauses() {
        let json = r#"{
            "clauses": [
                {"type":"graph","seeds":["Home"],"direction":"both","depth":2},
                {"type":"query","query":"from notes where status = public"}
            ],
            "exclusions":{"paths":["Private"]},
            "max_nodes":500
        }"#;
        let plan: SelectionPlan = serde_json::from_str(json).expect("selection JSON");
        validate_selection_plan(&plan).expect("valid plan");
        let rendered = serde_json::to_string(&plan).expect("serialize plan");
        let reparsed: SelectionPlan = serde_json::from_str(&rendered).expect("reparse plan");
        assert_eq!(reparsed, plan);
        let SelectionClause::Graph { include_seeds, .. } = &plan.clauses[0] else {
            panic!("first clause should be graph selection");
        };
        assert!(*include_seeds);
        assert!(serde_json::from_str::<SelectionPlan>(
            r#"{"clauses":[{"type":"graph","seeds":["Home"],"direktion":"both"}]}"#
        )
        .is_err());
    }
}
