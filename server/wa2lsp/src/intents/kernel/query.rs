//! Query execution for XPath-like expressions

use crate::intents::kernel::ast::*;
use crate::intents::kernel::rules::RuleError;
use crate::intents::model::{EntityId, Model, Value};

pub struct QueryEngine;

impl QueryEngine {
	pub fn new() -> Self {
		Self
	}

	pub fn execute(&self, model: &Model, path: &QueryPath) -> Result<Vec<EntityId>, RuleError> {
		if path.steps.is_empty() {
			return Ok(Vec::new());
		}

		let first_step = &path.steps[0];

		// First step with Descendant axis = global scan
		let mut current = if matches!(first_step.axis, Axis::Descendant | Axis::DescendantOrSelf) {
			// All entities as candidates
			let all: Vec<EntityId> = (0..model.entity_count())
				.map(|i| EntityId(i as u32))
				.collect();
			// Apply type/predicate filters from first step
			self.apply_filters(model, all, first_step)?
		} else {
			let root = match model.root() {
				Some(r) => vec![r],
				None => return Ok(Vec::new()),
			};
			self.execute_step(model, &root, first_step)?
		};

		// Remaining steps
		for step in &path.steps[1..] {
			current = self.execute_step(model, &current, step)?;
		}

		Ok(current)
	}

	fn apply_filters(
		&self,
		model: &Model,
		candidates: Vec<EntityId>,
		step: &QueryStep,
	) -> Result<Vec<EntityId>, RuleError> {
		let mut results = Vec::new();

		for candidate in candidates {
			// Node test (type or name)
			if let Some(ref type_name) = step.node_test {
				let qname = type_name.to_string();
				if let Some(resolved) = model.resolve(&qname) {
					// Check if resolved is a type (has wa2:type = wa2:Type)
					let wa2_type = model.resolve("wa2:Type");
					if wa2_type.map_or(false, |t| model.has_type(resolved, t)) {
						// It's a type - filter by type
						if !model.has_type(candidate, resolved) {
							continue;
						}
					} else {
						// It's a name - exact match
						if candidate != resolved {
							continue;
						}
					}
				} else {
					continue; // Can't resolve, skip
				}
			}

			// Predicates
			if !self.check_predicates(model, candidate, &step.predicates)? {
				continue;
			}

			if !results.contains(&candidate) {
				results.push(candidate);
			}
		}

		Ok(results)
	}

	fn execute_step(
		&self,
		model: &Model,
		input: &[EntityId],
		step: &QueryStep,
	) -> Result<Vec<EntityId>, RuleError> {
		let mut results = Vec::new();

		for &node in input {
			let candidates = match step.axis {
				Axis::Child => model.children(node),
				Axis::Descendant | Axis::DescendantOrSelf => self.descendants(model, node),
			};

			for candidate in candidates {
				// Type test
				if let Some(ref type_name) = step.node_test {
					let qname = type_name.to_string();
					if let Some(type_id) = model.resolve(&qname) {
						if !model.has_type(candidate, type_id) {
							continue;
						}
					} else {
						continue;
					}
				}

				// Predicates
				if !self.check_predicates(model, candidate, &step.predicates)? {
					continue;
				}

				if !results.contains(&candidate) {
					results.push(candidate);
				}
			}
		}

		Ok(results)
	}

	fn descendants(&self, model: &Model, node: EntityId) -> Vec<EntityId> {
		let mut result = Vec::new();
		let mut stack = vec![node];

		while let Some(current) = stack.pop() {
			for child in model.children(current) {
				if !result.contains(&child) {
					result.push(child);
					stack.push(child);
				}
			}
		}

		result
	}

	fn check_predicates(
		&self,
		model: &Model,
		node: EntityId,
		predicates: &[QueryPredicate],
	) -> Result<bool, RuleError> {
		for pred in predicates {
			if !self.check_predicate(model, node, pred)? {
				return Ok(false);
			}
		}
		Ok(true)
	}

	fn check_predicate(
		&self,
		model: &Model,
		node: EntityId,
		predicate: &QueryPredicate,
	) -> Result<bool, RuleError> {
		match predicate {
			QueryPredicate::Eq(path, literal) => {
				let values = self.follow_path(model, node, path)?;
				let target = self.literal_to_string(literal);
				Ok(values.iter().any(|v| v == &target))
			}

			QueryPredicate::In(path, literals) => {
				let values = self.follow_path(model, node, path)?;
				let targets: Vec<String> =
					literals.iter().map(|l| self.literal_to_string(l)).collect();
				Ok(values.iter().any(|v| targets.contains(v)))
			}

			QueryPredicate::Exists(path) => {
				let values = self.follow_path(model, node, path)?;
				Ok(!values.is_empty())
			}
		}
	}

	fn follow_path(
		&self,
		model: &Model,
		start: EntityId,
		path: &QueryPath,
	) -> Result<Vec<String>, RuleError> {
		let mut current = vec![start];

		for step in &path.steps {
			let mut next = Vec::new();
			for node in current {
				if let Some(ref name) = step.node_test {
					let pred_name = name.to_string();
					if let Some(pred_id) = model.resolve(&pred_name) {
						for value in model.get_all(node, pred_id) {
							match value {
								Value::Entity(id) => next.push(id),
								_ => {}
							}
						}
					}
				}
			}
			current = next;
		}

		// Get literal values from final nodes
		let mut results = Vec::new();
		for node in current {
			// Try to get value as string
			for stmt_id in model.outgoing(node) {
				let stmt = model.statement(stmt_id);
				if let Value::Literal(s) = &stmt.object {
					results.push(s.clone());
				}
			}
		}

		// If no results from following, try direct literal access
		if results.is_empty() && path.steps.len() == 1 {
			if let Some(step) = path.steps.first() {
				if let Some(ref name) = step.node_test {
					let pred_name = name.to_string();
					if let Some(pred_id) = model.resolve(&pred_name) {
						for value in model.get_all(start, pred_id) {
							if let Value::Literal(s) = value {
								results.push(s);
							}
						}
					}
				}
			}
		}

		Ok(results)
	}

	fn literal_to_string(&self, literal: &Literal) -> String {
		match literal {
			Literal::String(s) => s.clone(),
			Literal::Number(n) => n.to_string(),
			Literal::Bool(b) => b.to_string(),
		}
	}
}
