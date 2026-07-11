use std::cell::Cell;
use std::time::{Duration, Instant};

pub(crate) const MAX_QUERY_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_DOCUMENT_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_EXPRESSION_OUTPUT_CHARS: usize = 256 * 1024;
pub(crate) const MAX_PARSE_RECURSION_DEPTH: usize = 64;
pub(crate) const MAX_EVALUATION_OPERATIONS: usize = 100_000;
pub(crate) const MAX_EVALUATION_TIME: Duration = Duration::from_secs(1);

pub(crate) fn ensure_query_input(source: &str) -> Result<(), String> {
    if source.len() > MAX_QUERY_INPUT_BYTES {
        return Err(format!(
            "query input exceeds maximum size of {MAX_QUERY_INPUT_BYTES} bytes"
        ));
    }
    Ok(())
}

pub(crate) struct EvaluationBudget {
    depth: Cell<usize>,
    operations: Cell<usize>,
    deadline: Cell<Instant>,
    operation_limit: usize,
    time_limit: Duration,
}

impl Default for EvaluationBudget {
    fn default() -> Self {
        Self {
            depth: Cell::new(0),
            operations: Cell::new(0),
            deadline: Cell::new(Instant::now() + MAX_EVALUATION_TIME),
            operation_limit: MAX_EVALUATION_OPERATIONS,
            time_limit: MAX_EVALUATION_TIME,
        }
    }
}

impl EvaluationBudget {
    pub(crate) fn enter(&self) -> Result<EvaluationGuard<'_>, String> {
        let depth = self.depth.get();
        if depth == 0 {
            self.operations.set(0);
            self.deadline.set(Instant::now() + self.time_limit);
        }
        let operations = self.operations.get().saturating_add(1);
        if operations > self.operation_limit {
            return Err(format!(
                "expression evaluation exceeds maximum operation count of {}",
                self.operation_limit
            ));
        }
        if Instant::now() >= self.deadline.get() {
            return Err("expression evaluation exceeded its time budget".to_string());
        }
        self.operations.set(operations);
        self.depth.set(depth.saturating_add(1));
        Ok(EvaluationGuard { budget: self })
    }

    #[cfg(test)]
    pub(crate) fn with_limits(operation_limit: usize, time_limit: Duration) -> Self {
        Self {
            depth: Cell::new(0),
            operations: Cell::new(0),
            deadline: Cell::new(Instant::now() + time_limit),
            operation_limit,
            time_limit,
        }
    }
}

pub(crate) struct EvaluationGuard<'a> {
    budget: &'a EvaluationBudget,
}

impl Drop for EvaluationGuard<'_> {
    fn drop(&mut self) {
        self.budget
            .depth
            .set(self.budget.depth.get().saturating_sub(1));
    }
}

pub(crate) fn checked_repeat(text: &str, count: usize) -> Option<String> {
    let output_chars = text.chars().count().checked_mul(count)?;
    (output_chars <= MAX_EXPRESSION_OUTPUT_CHARS).then(|| text.repeat(count))
}

pub(crate) fn ensure_expression_output_chars(length: usize) -> Result<(), String> {
    if length > MAX_EXPRESSION_OUTPUT_CHARS {
        return Err(format!(
            "expression output exceeds maximum length of {MAX_EXPRESSION_OUTPUT_CHARS} characters"
        ));
    }
    Ok(())
}

pub(crate) fn ensure_collection_items(length: usize) -> Result<(), String> {
    if length > MAX_EVALUATION_OPERATIONS {
        return Err(format!(
            "expression collection exceeds maximum item count of {MAX_EVALUATION_OPERATIONS}"
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct WikilinkEndIndex {
    next_closing_bracket: Vec<usize>,
}

impl WikilinkEndIndex {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        let mut next_closing_bracket = vec![bytes.len(); bytes.len() + 1];
        let mut next = bytes.len();
        for index in (0..bytes.len()).rev() {
            if bytes[index] == b']' {
                next = index;
            }
            next_closing_bracket[index] = next;
        }
        Self {
            next_closing_bracket,
        }
    }

    pub(crate) fn end(&self, bytes: &[u8], start: usize) -> Option<usize> {
        let opening = start + usize::from(bytes.get(start) == Some(&b'!'));
        if bytes.get(opening..opening + 2) != Some(b"[[") {
            return None;
        }
        let closing = *self.next_closing_bracket.get(opening + 2)?;
        (bytes.get(closing..closing + 2) == Some(b"]]")).then_some(closing + 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_budget_rejects_oversized_vectorized_inputs() {
        assert!(ensure_collection_items(MAX_EVALUATION_OPERATIONS).is_ok());
        assert!(ensure_collection_items(MAX_EVALUATION_OPERATIONS + 1).is_err());
    }
}
