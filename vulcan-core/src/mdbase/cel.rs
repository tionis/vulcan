use cel_interpreter::{Context, Program};
use cel_parser::ast::{EntryExpr, Expr, IdedEntryExpr, IdedExpr};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseCelLimits {
    pub max_source_bytes: usize,
    pub max_lexical_tokens: usize,
    pub max_ast_depth: usize,
    pub max_ast_nodes: usize,
    pub max_evaluation_work: usize,
    pub max_value_bytes: usize,
    pub max_value_nodes: usize,
    pub max_collection_items: usize,
    pub max_link_traversal: usize,
}

impl Default for MdbaseCelLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_lexical_tokens: 1_024,
            max_ast_depth: 100,
            max_ast_nodes: 1_024,
            max_evaluation_work: 100_000,
            max_value_bytes: 1024 * 1024,
            max_value_nodes: 50_000,
            max_collection_items: 2_048,
            max_link_traversal: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MdbaseCelProgramStats {
    pub ast_depth: usize,
    pub ast_nodes: usize,
    pub estimated_work: usize,
}

#[derive(Debug)]
pub struct MdbaseCelProgram {
    source: String,
    program: Program,
    stats: MdbaseCelProgramStats,
}

impl MdbaseCelProgram {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn stats(&self) -> MdbaseCelProgramStats {
        self.stats
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseCelError {
    pub code: &'static str,
    pub message: String,
}

impl MdbaseCelError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn limit(name: &str, actual: usize, maximum: usize) -> Self {
        Self::new(
            "expression_limit_exceeded",
            format!("CEL {name} is {actual}, exceeding the limit of {maximum}"),
        )
    }
}

impl Display for MdbaseCelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MdbaseCelError {}

#[derive(Debug, Clone)]
pub struct MdbaseCelEngine {
    limits: MdbaseCelLimits,
}

impl Default for MdbaseCelEngine {
    fn default() -> Self {
        Self::new(MdbaseCelLimits::default())
    }
}

impl MdbaseCelEngine {
    #[must_use]
    pub const fn new(limits: MdbaseCelLimits) -> Self {
        Self { limits }
    }

    #[must_use]
    pub const fn limits(&self) -> MdbaseCelLimits {
        self.limits
    }

    pub fn compile(&self, source: &str) -> Result<MdbaseCelProgram, MdbaseCelError> {
        if source.len() > self.limits.max_source_bytes {
            return Err(MdbaseCelError::limit(
                "source size",
                source.len(),
                self.limits.max_source_bytes,
            ));
        }
        let lexical = inspect_source(source)?;
        if lexical.tokens > self.limits.max_lexical_tokens {
            return Err(MdbaseCelError::limit(
                "lexical token count",
                lexical.tokens,
                self.limits.max_lexical_tokens,
            ));
        }
        if lexical.depth > self.limits.max_ast_depth {
            return Err(MdbaseCelError::limit(
                "syntactic depth",
                lexical.depth,
                self.limits.max_ast_depth,
            ));
        }

        let expression = cel_parser::Parser::new()
            .parse(source)
            .map_err(|error| MdbaseCelError::new("expression_compile_error", error.to_string()))?;
        let stats = inspect_ast(&expression, &self.limits)?;
        let program = Program::compile(source)
            .map_err(|error| MdbaseCelError::new("expression_compile_error", error.to_string()))?;
        Ok(MdbaseCelProgram {
            source: source.to_string(),
            program,
            stats,
        })
    }

    pub fn evaluate(
        &self,
        program: &MdbaseCelProgram,
        bindings: &BTreeMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, MdbaseCelError> {
        inspect_value(
            &serde_json::to_value(bindings).map_err(|error| {
                MdbaseCelError::new("expression_binding_error", error.to_string())
            })?,
            &self.limits,
            "input",
        )?;
        let mut context = Context::default();
        for (name, value) in bindings {
            context.add_variable(name, value).map_err(|error| {
                MdbaseCelError::new("expression_binding_error", error.to_string())
            })?;
        }
        let value = program.program.execute(&context).map_err(|error| {
            MdbaseCelError::new("expression_evaluation_error", error.to_string())
        })?;
        let value = value
            .json()
            .map_err(|error| MdbaseCelError::new("expression_result_error", error.to_string()))?;
        inspect_value(&value, &self.limits, "output")?;
        Ok(value)
    }

    #[must_use]
    pub const fn link_budget(&self) -> MdbaseCelLinkBudget {
        MdbaseCelLinkBudget {
            remaining: self.limits.max_link_traversal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdbaseCelLinkBudget {
    remaining: usize,
}

impl MdbaseCelLinkBudget {
    pub fn consume(&mut self) -> Result<(), MdbaseCelError> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            MdbaseCelError::new(
                "expression_link_traversal_limit",
                "CEL link traversal budget is exhausted",
            )
        })?;
        Ok(())
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining
    }
}

#[derive(Debug, Default)]
struct LexicalStats {
    tokens: usize,
    depth: usize,
}

fn inspect_source(source: &str) -> Result<LexicalStats, MdbaseCelError> {
    let mut stats = LexicalStats::default();
    let mut current_depth = 0_usize;
    let mut identifier = false;
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            stats.tokens = stats.tokens.saturating_add(1);
            identifier = false;
        } else if character.is_alphanumeric() || character == '_' {
            if !identifier {
                stats.tokens = stats.tokens.saturating_add(1);
                identifier = true;
            }
        } else {
            identifier = false;
            if matches!(character, '(' | '[' | '{') {
                current_depth = current_depth.saturating_add(1);
                stats.depth = stats.depth.max(current_depth);
                stats.tokens = stats.tokens.saturating_add(1);
            } else if matches!(character, ')' | ']' | '}') {
                current_depth = current_depth.saturating_sub(1);
                stats.tokens = stats.tokens.saturating_add(1);
            } else if !character.is_whitespace() {
                stats.tokens = stats.tokens.saturating_add(1);
            }
        }
    }
    if quote.is_some() {
        return Err(MdbaseCelError::new(
            "expression_compile_error",
            "unterminated string literal",
        ));
    }
    Ok(stats)
}

fn inspect_ast(
    expression: &IdedExpr,
    limits: &MdbaseCelLimits,
) -> Result<MdbaseCelProgramStats, MdbaseCelError> {
    let (nodes, depth, work) = ast_cost(expression, 1, limits)?;
    if nodes > limits.max_ast_nodes {
        return Err(MdbaseCelError::limit(
            "AST node count",
            nodes,
            limits.max_ast_nodes,
        ));
    }
    if depth > limits.max_ast_depth {
        return Err(MdbaseCelError::limit(
            "AST depth",
            depth,
            limits.max_ast_depth,
        ));
    }
    if work > limits.max_evaluation_work {
        return Err(MdbaseCelError::limit(
            "estimated evaluation work",
            work,
            limits.max_evaluation_work,
        ));
    }
    Ok(MdbaseCelProgramStats {
        ast_depth: depth,
        ast_nodes: nodes,
        estimated_work: work,
    })
}

fn ast_cost(
    expression: &IdedExpr,
    depth: usize,
    limits: &MdbaseCelLimits,
) -> Result<(usize, usize, usize), MdbaseCelError> {
    if depth > limits.max_ast_depth {
        return Err(MdbaseCelError::limit(
            "AST depth",
            depth,
            limits.max_ast_depth,
        ));
    }
    let children = match &expression.expr {
        Expr::Call(call) => call
            .target
            .iter()
            .map(AsRef::as_ref)
            .chain(call.args.iter())
            .collect(),
        Expr::Comprehension(comprehension) => {
            let range = ast_cost(&comprehension.iter_range, depth + 1, limits)?;
            let init = ast_cost(&comprehension.accu_init, depth + 1, limits)?;
            let condition = ast_cost(&comprehension.loop_cond, depth + 1, limits)?;
            let step = ast_cost(&comprehension.loop_step, depth + 1, limits)?;
            let result = ast_cost(&comprehension.result, depth + 1, limits)?;
            let nodes = 1_usize
                .saturating_add(range.0)
                .saturating_add(init.0)
                .saturating_add(condition.0)
                .saturating_add(step.0)
                .saturating_add(result.0);
            let iteration = condition.2.saturating_add(step.2);
            let work = 1_usize
                .saturating_add(range.2)
                .saturating_add(init.2)
                .saturating_add(iteration.saturating_mul(limits.max_collection_items))
                .saturating_add(result.2);
            return Ok((
                nodes,
                [range.1, init.1, condition.1, step.1, result.1]
                    .into_iter()
                    .max()
                    .unwrap_or(depth),
                work,
            ));
        }
        Expr::List(list) => list.elements.iter().collect(),
        Expr::Map(map) => entry_children(&map.entries),
        Expr::Select(select) => vec![select.operand.as_ref()],
        Expr::Struct(structure) => entry_children(&structure.entries),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => Vec::new(),
    };
    let mut nodes = 1_usize;
    let mut maximum_depth = depth;
    let mut work = 1_usize;
    for child in children {
        let child = ast_cost(child, depth + 1, limits)?;
        nodes = nodes.saturating_add(child.0);
        maximum_depth = maximum_depth.max(child.1);
        work = work.saturating_add(child.2);
    }
    Ok((nodes, maximum_depth, work))
}

fn entry_children(entries: &[IdedEntryExpr]) -> Vec<&IdedExpr> {
    entries
        .iter()
        .flat_map(|entry| match &entry.expr {
            EntryExpr::StructField(field) => vec![&field.value],
            EntryExpr::MapEntry(entry) => vec![&entry.key, &entry.value],
        })
        .collect()
}

fn inspect_value(
    value: &serde_json::Value,
    limits: &MdbaseCelLimits,
    label: &str,
) -> Result<(), MdbaseCelError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| MdbaseCelError::new("expression_value_error", error.to_string()))?
        .len();
    if bytes > limits.max_value_bytes {
        return Err(MdbaseCelError::limit(
            &format!("{label} value size"),
            bytes,
            limits.max_value_bytes,
        ));
    }
    let mut nodes = 0_usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > limits.max_value_nodes {
            return Err(MdbaseCelError::limit(
                &format!("{label} value node count"),
                nodes,
                limits.max_value_nodes,
            ));
        }
        match value {
            serde_json::Value::Array(values) => {
                if values.len() > limits.max_collection_items {
                    return Err(MdbaseCelError::limit(
                        &format!("{label} list iteration width"),
                        values.len(),
                        limits.max_collection_items,
                    ));
                }
                pending.extend(values);
            }
            serde_json::Value::Object(values) => {
                if values.len() > limits.max_collection_items {
                    return Err(MdbaseCelError::limit(
                        &format!("{label} map iteration width"),
                        values.len(),
                        limits.max_collection_items,
                    ));
                }
                pending.extend(values.values());
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine_with(mut update: impl FnMut(&mut MdbaseCelLimits)) -> MdbaseCelEngine {
        let mut limits = MdbaseCelLimits::default();
        update(&mut limits);
        MdbaseCelEngine::new(limits)
    }

    #[test]
    fn adapter_compiles_and_evaluates_without_exposing_engine_types() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile("status == 'open' && tags.exists(t, t == 'urgent')")
            .expect("expression compiles");
        let bindings = BTreeMap::from([
            ("status".to_string(), json!("open")),
            ("tags".to_string(), json!(["urgent", "project"])),
        ]);

        assert_eq!(
            engine.evaluate(&program, &bindings).expect("evaluates"),
            true
        );
        assert!(program.stats().ast_nodes > 1);
        assert!(program.stats().estimated_work > program.stats().ast_nodes);
    }

    #[test]
    fn default_limits_meet_the_portable_profile_minimums() {
        let limits = MdbaseCelLimits::default();

        assert!(limits.max_source_bytes >= 64 * 1024);
        assert!(limits.max_ast_depth >= 100);
        assert!(limits.max_link_traversal >= 10);
    }

    #[test]
    fn source_tokens_ast_and_work_are_bounded_before_evaluation() {
        let source = engine_with(|limits| limits.max_source_bytes = 4)
            .compile("true && true")
            .expect_err("source should be bounded");
        assert_eq!(source.code, "expression_limit_exceeded");

        let tokens = engine_with(|limits| limits.max_lexical_tokens = 2)
            .compile("a + b")
            .expect_err("tokens should be bounded");
        assert!(tokens.message.contains("token"));

        let depth = engine_with(|limits| limits.max_ast_depth = 2)
            .compile("(((true)))")
            .expect_err("depth should be bounded");
        assert!(depth.message.contains("depth"));

        let work = engine_with(|limits| limits.max_evaluation_work = 10)
            .compile("items.exists(item, item == 1)")
            .expect_err("comprehension work should be bounded");
        assert!(work.message.contains("work"));
    }

    #[test]
    fn input_output_memory_and_iteration_are_bounded() {
        let input_engine = engine_with(|limits| limits.max_collection_items = 1);
        let input_program = input_engine
            .compile("items.size()")
            .expect("expression compiles");
        let input = input_engine
            .evaluate(
                &input_program,
                &BTreeMap::from([("items".to_string(), json!([1, 2]))]),
            )
            .expect_err("input width should be bounded");
        assert!(input.message.contains("iteration width"));

        let output_engine = engine_with(|limits| limits.max_value_bytes = 8);
        let output_program = output_engine
            .compile("'long output'")
            .expect("expression compiles");
        let output = output_engine
            .evaluate(&output_program, &BTreeMap::new())
            .expect_err("output memory should be bounded");
        assert!(output.message.contains("output value size"));
    }

    #[test]
    fn link_traversal_budget_fails_closed() {
        let engine = engine_with(|limits| limits.max_link_traversal = 1);
        let mut budget = engine.link_budget();

        budget.consume().expect("first traversal is allowed");
        let error = budget.consume().expect_err("second traversal is denied");
        assert_eq!(error.code, "expression_link_traversal_limit");
        assert_eq!(budget.remaining(), 0);
    }
}
