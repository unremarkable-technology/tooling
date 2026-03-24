//! Rule execution engine with two-phase execution:
//! Phase 1: Derives run to fixed-point (model building)
//! Phase 2: Rules run in policy order (validation only, no model changes)

use std::collections::{HashMap, HashSet};

use crate::intents::kernel::ast::*;
use crate::intents::kernel::query::QueryEngine;
use crate::intents::model::{EntityId, Model, Value};

#[derive(Debug)]
pub struct RuleError {
	pub message: String,
}

/// Result of policy execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
	/// All must rules passed
	Pass,
	/// All must rules passed, but some should rules failed
	Degraded,
	/// At least one must rule failed (guard triggered)
	Fail,
}

/// A policy binding with its source policy name for tracing
#[derive(Debug, Clone)]
pub struct OrderedBinding {
	pub policy_name: String,
	pub modal: Modal,
	pub rule_name: String,
}

/// Record of a single rule execution for verbose output
#[derive(Debug, Clone)]
pub struct RuleExecution {
	pub policy_name: String,
	pub rule_name: String,
	pub modal: Modal,
	pub passed: bool,
	pub finding_count: usize,
}

/// Full result of policy execution including trace
#[derive(Debug)]
pub struct PolicyExecutionResult {
	pub outcome: PolicyOutcome,
	pub trace: Vec<RuleExecution>,
}

/// Result of executing a statement
enum StmtResult {
	Continue,
	/// Guard failed - skip rest of body
	Guard,
}

/// Reference binding: map of loop variable names to entity values
type ReferenceBinding = Vec<(String, EntityId)>;

pub struct RuleEngine {
	max_iterations: usize,
	/// Tracks (rule_name, reference_binding) combinations already processed
	processed: HashSet<(String, ReferenceBinding)>,
}

impl Default for RuleEngine {
	fn default() -> Self {
		Self::new()
	}
}

impl RuleEngine {
	pub fn new() -> Self {
		Self {
			max_iterations: 100,
			processed: HashSet::new(),
		}
	}

	/// Simple run for tests - no policy ordering
	pub fn run(&mut self, model: &mut Model, rules: &[Rule]) -> Result<(), RuleError> {
		let rules_map: HashMap<String, Rule> =
			rules.iter().map(|r| (r.name.clone(), r.clone())).collect();
		let bindings: Vec<OrderedBinding> = rules
			.iter()
			.map(|r| OrderedBinding {
				policy_name: String::new(),
				modal: Modal::Must,
				rule_name: r.name.clone(),
			})
			.collect();
		self.run_with_policy(model, &[], &rules_map, &bindings)?;
		Ok(())
	}

	/// Run with derives but no policy (for backwards compatibility)
	pub fn run_with_modals(
		&mut self,
		model: &mut Model,
		derives: &[Derive],
		rules: &[Rule],
		rule_modals: &HashMap<String, Modal>,
	) -> Result<(), RuleError> {
		let rules_map: HashMap<String, Rule> =
			rules.iter().map(|r| (r.name.clone(), r.clone())).collect();

		// Build bindings from rule_modals (arbitrary order for backwards compat)
		let bindings: Vec<OrderedBinding> = rules
			.iter()
			.filter_map(|r| {
				rule_modals.get(&r.name).map(|&modal| OrderedBinding {
					policy_name: String::new(),
					modal,
					rule_name: r.name.clone(),
				})
			})
			.collect();

		self.run_with_policy(model, derives, &rules_map, &bindings)?;
		Ok(())
	}

	/// Main entry point: two-phase execution with policy ordering
	pub fn run_with_policy(
		&mut self,
		model: &mut Model,
		derives: &[Derive],
		rules: &HashMap<String, Rule>,
		bindings: &[OrderedBinding],
	) -> Result<PolicyExecutionResult, RuleError> {
		// Phase 1: Run derives to fixed-point (model building)
		self.run_derives(model, derives)?;

		// Phase 2: Run rules in policy order (validation only)
		self.processed.clear();
		let mut policy_outcome = PolicyOutcome::Pass;
		let mut trace = Vec::new();

		for binding in bindings {
			let rule = match rules.get(&binding.rule_name) {
				Some(r) => r,
				None => {
					// Rule not in rules map - skip (might not be loaded)
					continue;
				}
			};

			let errors_before = self.count_error_findings(model);
			self.execute_rule(model, rule)?;
			let errors_after = self.count_error_findings(model);

			let new_errors = errors_after.saturating_sub(errors_before);
			let rule_failed = new_errors > 0;

			// Record execution for trace
			trace.push(RuleExecution {
				policy_name: binding.policy_name.clone(),
				rule_name: binding.rule_name.clone(),
				modal: binding.modal,
				passed: !rule_failed,
				finding_count: new_errors,
			});

			match (binding.modal, rule_failed) {
				(Modal::Must, true) => {
					// Guard: stop policy execution
					return Ok(PolicyExecutionResult {
						outcome: PolicyOutcome::Fail,
						trace,
					});
				}
				(Modal::Should, true) => {
					// Note degraded, continue
					policy_outcome = PolicyOutcome::Degraded;
				}
				(Modal::May, _) | (_, false) => {
					// Continue
				}
			}
		}

		Ok(PolicyExecutionResult {
			outcome: policy_outcome,
			trace,
		})
	}

	/// Count Error-severity findings in model
	fn count_error_findings(&self, model: &Model) -> usize {
		let failure_type = match model.resolve("core:AssertFailure") {
			Some(t) => t,
			None => return 0,
		};
		let severity_pred = match model.resolve("core:severity") {
			Some(p) => p,
			None => return 0,
		};
		let error_severity = match model.resolve("core:Error") {
			Some(e) => e,
			None => return 0,
		};

		(0..model.entity_count())
			.filter(|i| {
				let e = EntityId(*i as u32);
				if !model.has_type(e, failure_type) {
					return false;
				}
				model
					.get_all(e, severity_pred)
					.iter()
					.any(|v| matches!(v, Value::Entity(sev) if *sev == error_severity))
			})
			.count()
	}

	/// Run derives to fixed-point (model building phase)
	pub fn run_derives(&mut self, model: &mut Model, derives: &[Derive]) -> Result<(), RuleError> {
		self.processed.clear();

		for _ in 0..self.max_iterations {
			let initial_count = model.statement_count();

			for derive in derives {
				self.execute_derive(model, derive)?;
			}

			let final_count = model.statement_count();
			if final_count == initial_count {
				break;
			}
		}

		Ok(())
	}

	fn execute_derive(&mut self, model: &mut Model, derive: &Derive) -> Result<(), RuleError> {
		let mut env = Env::new();
		let binding = Vec::new();
		self.execute_statements(model, &derive.body, &mut env, &derive.name, &binding, true)
	}

	fn execute_rule(&mut self, model: &mut Model, rule: &Rule) -> Result<(), RuleError> {
		let mut env = Env::new();
		let binding = Vec::new();
		self.execute_statements(model, &rule.body, &mut env, &rule.name, &binding, false)
	}

	fn execute_statements(
		&mut self,
		model: &mut Model,
		stmts: &[Statement],
		env: &mut Env,
		rule_name: &str,
		reference_binding: &ReferenceBinding,
		is_derive: bool,
	) -> Result<(), RuleError> {
		for stmt in stmts {
			match self.execute_statement(
				model,
				stmt,
				env,
				rule_name,
				reference_binding,
				is_derive,
			)? {
				StmtResult::Continue => {}
				StmtResult::Guard => return Ok(()), // Stop processing this body
			}
		}
		Ok(())
	}

	fn execute_statement(
		&mut self,
		model: &mut Model,
		stmt: &Statement,
		env: &mut Env,
		rule_name: &str,
		reference_binding: &ReferenceBinding,
		is_derive: bool,
	) -> Result<StmtResult, RuleError> {
		match stmt {
			Statement::Let(let_stmt) => {
				let value = self.eval_expr(model, &let_stmt.value, env, is_derive)?;
				env.bind(let_stmt.name.clone(), value);
				Ok(StmtResult::Continue)
			}

			Statement::Add(add_stmt) => {
				if !is_derive {
					return Err(RuleError {
						message: "add statements not allowed in rules".to_string(),
					});
				}
				let subject = self.eval_expr_to_entity(model, &add_stmt.subject, env, is_derive)?;
				let pred_name = add_stmt.predicate.to_string();
				let object = self.eval_expr(model, &add_stmt.object, env, is_derive)?;

				match object {
					EvalResult::Entity(obj_id) => {
						model
							.apply_entity(subject, &pred_name, obj_id)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Literal(s) => {
						model
							.apply_literal(subject, &pred_name, &s)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Set(_) => {
						return Err(RuleError {
							message: "cannot use set as object".to_string(),
						});
					}
					EvalResult::Empty => {
						return Err(RuleError {
							message: "cannot use empty as object".to_string(),
						});
					}
				}
				Ok(StmtResult::Continue)
			}

			Statement::For(for_stmt) => {
				let collection = self.eval_expr(model, &for_stmt.collection, env, is_derive)?;
				let entities = match collection {
					EvalResult::Set(ids) => ids,
					EvalResult::Entity(id) => vec![id],
					_ => Vec::new(),
				};

				for entity in entities {
					let mut new_binding = reference_binding.clone();
					new_binding.push((for_stmt.var.clone(), entity));

					let key = (rule_name.to_string(), new_binding.clone());
					if self.processed.contains(&key) {
						continue;
					}
					self.processed.insert(key);

					let mut inner_env = env.clone();
					inner_env.bind(for_stmt.var.clone(), EvalResult::Entity(entity));
					self.execute_statements(
						model,
						&for_stmt.body,
						&mut inner_env,
						rule_name,
						&new_binding,
						is_derive,
					)?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::If(if_stmt) => {
				let cond = self.eval_expr(model, &if_stmt.condition, env, is_derive)?;
				if self.is_satisfied(&cond) {
					self.execute_statements(
						model,
						&if_stmt.then_body,
						env,
						rule_name,
						reference_binding,
						is_derive,
					)?;
				} else if let Some(ref else_body) = if_stmt.else_body {
					self.execute_statements(
						model,
						else_body,
						env,
						rule_name,
						reference_binding,
						is_derive,
					)?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::Assert(assert_stmt) => {
				let result = self.eval_expr(model, &assert_stmt.expr, env, is_derive)?;
				if !self.is_satisfied(&result) {
					self.create_failure(model, rule_name, "assertion failed")?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::Modal(modal_stmt) => {
				let result = self.eval_expr(model, &modal_stmt.expr, env, is_derive)?;
				if !self.is_satisfied(&result) {
					// Evaluate subject/area and create finding immediately
					let subject = if let Some(ref meta) = modal_stmt.metadata {
						if let Some(ref subj_expr) = meta.subject {
							Some(self.eval_expr_to_entity(model, subj_expr, env, is_derive)?)
						} else {
							self.get_context_entity(env)
						}
					} else {
						self.get_context_entity(env)
					};

					let area = if let Some(ref meta) = modal_stmt.metadata {
						if let Some(ref area_name) = meta.area {
							model.resolve(&area_name.to_string())
						} else {
							None
						}
					} else {
						None
					};

					let message = modal_stmt.metadata.as_ref().and_then(|m| m.message.clone());
					let modal = modal_stmt.modal;

					// Create finding immediately
					self.create_rich_failure(model, rule_name, subject, area, message, modal)?;

					// Guard behavior depends on modal:
					// - must/should: skip rest of body
					// - may: continue execution
					if modal == Modal::May {
						return Ok(StmtResult::Continue);
					} else {
						return Ok(StmtResult::Guard);
					}
				}
				Ok(StmtResult::Continue)
			}
		}
	}

	fn create_rich_failure(
		&self,
		model: &mut Model,
		rule_name: &str,
		subject: Option<EntityId>,
		area: Option<EntityId>,
		message: Option<String>,
		modal: Modal,
	) -> Result<(), RuleError> {
		let failure = model.blank();
		model
			.apply_to(failure, "wa2:type", "core:AssertFailure")
			.map_err(|e| RuleError {
				message: format!("failed to create failure node: {}", e),
			})?;

		// Assertion text (rule name)
		let modal_text = match modal {
			Modal::Must => "must",
			Modal::Should => "should",
			Modal::May => "may",
		};
		let assertion_text = format!("{}: {} obligation not satisfied", rule_name, modal_text);
		model
			.apply_literal(failure, "core:assertion", &assertion_text)
			.map_err(|e| RuleError {
				message: format!("failed to set assertion: {}", e),
			})?;

		// Subject (link to entity)
		if let Some(subj) = subject {
			model
				.apply_entity(failure, "core:subject", subj)
				.map_err(|e| RuleError {
					message: format!("failed to set subject: {}", e),
				})?;
		}

		// Area (link to entity for education content)
		if let Some(area_id) = area {
			model
				.apply_entity(failure, "core:area", area_id)
				.map_err(|e| RuleError {
					message: format!("failed to set area: {}", e),
				})?;
		}

		// Message (user action string)
		if let Some(msg) = message {
			model
				.apply_literal(failure, "core:message", &msg)
				.map_err(|e| RuleError {
					message: format!("failed to set message: {}", e),
				})?;
		}

		// Modal (as entity reference)
		model
			.apply_to(failure, "core:severity", Self::severity_for_modal(modal))
			.map_err(|e| RuleError {
				message: format!("failed to set severity: {}", e),
			})?;

		Ok(())
	}

	fn severity_for_modal(modal: Modal) -> &'static str {
		match modal {
			Modal::Must => "core:Error",
			Modal::Should => "core:Warning",
			Modal::May => "core:Info",
		}
	}

	/// Check if a result satisfies an obligation
	fn is_satisfied(&self, result: &EvalResult) -> bool {
		match result {
			EvalResult::Set(ids) => !ids.is_empty(),
			EvalResult::Entity(_) => true,
			EvalResult::Literal(s) => !s.is_empty() && s != "false",
			EvalResult::Empty => false,
		}
	}

	/// Get the current context entity (e.g., the loop variable)
	fn get_context_entity(&self, env: &Env) -> Option<EntityId> {
		// Return the most recently bound entity variable
		env.last_entity()
	}

	/// Create an assertion failure node
	fn create_failure(
		&mut self,
		model: &mut Model,
		rule_name: &str,
		message: &str,
	) -> Result<(), RuleError> {
		self.create_failure_with_context(model, rule_name, message, None)
	}

	/// Create an assertion failure node with context
	fn create_failure_with_context(
		&mut self,
		model: &mut Model,
		rule_name: &str,
		message: &str,
		context: Option<EntityId>,
	) -> Result<(), RuleError> {
		let failure = model.blank();
		model
			.apply_to(failure, "wa2:type", "core:AssertFailure")
			.map_err(|e| RuleError {
				message: format!("failed to create failure node: {}", e),
			})?;

		let full_message = format!("{}: {}", rule_name, message);
		model
			.apply_literal(failure, "core:assertion", &full_message)
			.map_err(|e| RuleError {
				message: format!("failed to set assertion message: {}", e),
			})?;

		// Link to context entity if available
		if let Some(entity) = context {
			model
				.apply_entity(failure, "core:subject", entity)
				.map_err(|e| RuleError {
					message: format!("failed to link failure to subject: {}", e),
				})?;
		}

		Ok(())
	}

	fn eval_expr(
		&self,
		model: &mut Model,
		expr: &Expr,
		env: &Env,
		is_derive: bool,
	) -> Result<EvalResult, RuleError> {
		match expr {
			Expr::Var(name, _) => env.get(name).cloned().ok_or_else(|| RuleError {
				message: format!("undefined variable: {}", name),
			}),

			Expr::Blank(_) => {
				if !is_derive {
					return Err(RuleError {
						message: "blank nodes not allowed in rules".to_string(),
					});
				}
				Err(RuleError {
					message: "blank node in eval context - should be handled in add".to_string(),
				})
			}

			Expr::Query(query) => {
				let engine = QueryEngine::new();

				// Check if the first step is a variable reference
				if let Some(first_step) = query.path.steps.first() {
					if let Some(ref node_test) = first_step.node_test {
						let var_name = &node_test.name;
						if node_test.namespace.is_none() {
							if let Some(result) = env.get(var_name) {
								let start_entities = match result {
									EvalResult::Entity(id) => vec![*id],
									EvalResult::Set(ids) => ids.clone(),
									_ => vec![],
								};

								if !start_entities.is_empty() {
									// Apply predicates from first step if any
									let filtered = if !first_step.predicates.is_empty() {
										let mut result = Vec::new();
										for &entity in &start_entities {
											if engine.check_predicates(
												model,
												entity,
												&first_step.predicates,
											)? {
												result.push(entity);
											}
										}
										result
									} else {
										start_entities
									};

									let remaining_path = QueryPath {
										steps: query.path.steps[1..].to_vec(),
										span: query.path.span.clone(),
									};

									if remaining_path.steps.is_empty() {
										return Ok(EvalResult::Set(filtered));
									}

									let results =
										engine.execute_from(model, &filtered, &remaining_path)?;
									return Ok(EvalResult::Set(results));
								}
							}
						}
					}
				}

				// No variable prefix, execute normally
				let results = engine.execute(model, &query.path)?;
				Ok(EvalResult::Set(results))
			}

			Expr::Add(add_expr) => {
				if !is_derive {
					return Err(RuleError {
						message: "add expressions not allowed in rules".to_string(),
					});
				}
				let subject = self.eval_expr_to_entity(model, &add_expr.subject, env, is_derive)?;
				let pred_name = add_expr.predicate.to_string();
				let object = self.eval_expr(model, &add_expr.object, env, is_derive)?;

				match object {
					EvalResult::Entity(obj_id) => {
						model
							.apply_entity(subject, &pred_name, obj_id)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Literal(s) => {
						model
							.apply_literal(subject, &pred_name, &s)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Set(_) => {
						return Err(RuleError {
							message: "cannot use set as object".to_string(),
						});
					}
					EvalResult::Empty => {
						return Err(RuleError {
							message: "cannot use empty as object".to_string(),
						});
					}
				}

				Ok(EvalResult::Entity(subject))
			}

			Expr::QName(qname) => {
				let name = qname.to_string();
				if let Some(id) = model.resolve(&name) {
					Ok(EvalResult::Entity(id))
				} else {
					Err(RuleError {
						message: format!("unresolved name: {}", name),
					})
				}
			}

			Expr::String(s, _) => Ok(EvalResult::Literal(s.clone())),

			Expr::Bool(b, _) => Ok(EvalResult::Literal(b.to_string())),

			Expr::Empty(inner, _) => {
				let value = self.eval_expr(model, inner, env, is_derive)?;
				Ok(if !self.is_satisfied(&value) {
					EvalResult::Literal("true".to_string())
				} else {
					EvalResult::Empty
				})
			}

			Expr::Match(match_expr) => {
				// Try to get a literal value for matching
				let value_str = if let Expr::Query(ref query) = match_expr.value {
					// Check if first step is a variable
					if let Some(first_step) = query.path.steps.first() {
						if let Some(ref node_test) = first_step.node_test {
							if node_test.namespace.is_none() {
								if let Some(result) = env.get(&node_test.name) {
									let start_entities = match result {
										EvalResult::Entity(id) => vec![*id],
										EvalResult::Set(ids) => ids.clone(),
										_ => vec![],
									};

									if !start_entities.is_empty() && query.path.steps.len() > 1 {
										let remaining_path = QueryPath {
											steps: query.path.steps[1..].to_vec(),
											span: query.path.span.clone(),
										};

										let engine = QueryEngine::new();
										let literals = engine.extract_literals(
											model,
											&start_entities,
											&remaining_path,
										)?;

										if literals.len() == 1 {
											literals[0].clone()
										} else if literals.is_empty() {
											return Ok(EvalResult::Literal("false".to_string()));
										} else {
											return Err(RuleError {
												message: format!(
													"match requires single value, got {}",
													literals.len()
												),
											});
										}
									} else {
										return Err(RuleError {
											message: "match on variable without path not supported"
												.to_string(),
										});
									}
								} else {
									return Err(RuleError {
										message: format!(
											"undefined variable in match: {}",
											node_test.name
										),
									});
								}
							} else {
								return Err(RuleError {
									message: "match on qualified name not supported".to_string(),
								});
							}
						} else {
							return Err(RuleError {
								message: "match query must start with variable".to_string(),
							});
						}
					} else {
						return Err(RuleError {
							message: "match query is empty".to_string(),
						});
					}
				} else {
					// Not a query - evaluate normally
					let value = self.eval_expr(model, &match_expr.value, env, is_derive)?;
					match value {
						EvalResult::Literal(s) => s,
						EvalResult::Empty => return Ok(EvalResult::Literal("false".to_string())),
						_ => {
							return Err(RuleError {
								message: "match value must be a literal".to_string(),
							});
						}
					}
				};

				// Strip quotes from value if present (model stores literals as "value")
				let value_str = if value_str.starts_with('"')
					&& value_str.ends_with('"')
					&& value_str.len() >= 2
				{
					value_str[1..value_str.len() - 1].to_string()
				} else {
					value_str
				};

				// Find matching arm
				for arm in &match_expr.arms {
					let matches = arm.patterns.iter().any(|pattern| match pattern {
						MatchPattern::Else => true,
						MatchPattern::Variant(name) => &value_str == name,
					});

					if matches {
						return self.eval_expr(model, &arm.result, env, is_derive);
					}
				}

				Err(RuleError {
					message: format!("no matching arm for value: {}", value_str),
				})
			}

			Expr::As(validation) => {
				// Extract literal value - handle query specially
				let value_str = if let Expr::Query(ref query) = validation.inner {
					// Check if first step is a variable
					if let Some(first_step) = query.path.steps.first() {
						if let Some(ref node_test) = first_step.node_test {
							if node_test.namespace.is_none() {
								if let Some(result) = env.get(&node_test.name) {
									let start_entities = match result {
										EvalResult::Entity(id) => vec![*id],
										EvalResult::Set(ids) => ids.clone(),
										_ => vec![],
									};

									if !start_entities.is_empty() && query.path.steps.len() > 1 {
										let remaining_path = QueryPath {
											steps: query.path.steps[1..].to_vec(),
											span: query.path.span.clone(),
										};

										let engine = QueryEngine::new();
										let literals = engine.extract_literals(
											model,
											&start_entities,
											&remaining_path,
										)?;

										if literals.len() == 1 {
											literals[0].clone()
										} else if literals.is_empty() {
											return Ok(EvalResult::Empty);
										} else {
											return Err(RuleError {
												message: format!(
													"as() requires single value, got {}",
													literals.len()
												),
											});
										}
									} else {
										return Ok(EvalResult::Empty);
									}
								} else {
									return Err(RuleError {
										message: format!(
											"undefined variable in as(): {}",
											node_test.name
										),
									});
								}
							} else {
								return Err(RuleError {
									message: "as() on qualified name not supported".to_string(),
								});
							}
						} else {
							return Err(RuleError {
								message: "as() query must start with variable".to_string(),
							});
						}
					} else {
						return Err(RuleError {
							message: "as() query is empty".to_string(),
						});
					}
				} else {
					// Not a query - evaluate normally
					let value = self.eval_expr(model, &validation.inner, env, is_derive)?;
					match value {
						EvalResult::Literal(s) => s,
						EvalResult::Empty => return Ok(EvalResult::Empty),
						_ => {
							return Err(RuleError {
								message: "as() requires literal value".to_string(),
							});
						}
					}
				};

				// Strip quotes if present
				let value_str = if value_str.starts_with('"')
					&& value_str.ends_with('"')
					&& value_str.len() >= 2
				{
					value_str[1..value_str.len() - 1].to_string()
				} else {
					value_str
				};

				let type_name = validation.target_type.to_string();
				let type_entity = model.resolve(&type_name).ok_or_else(|| RuleError {
					message: format!("type '{}' not found for as() conversion", type_name),
				})?;

				// Check if value matches a variant
				let mut is_valid = false;
				if let Some(sub_type_of) = model.resolve("wa2:subTypeOf") {
					for i in 0..model.entity_count() {
						let entity = EntityId(i as u32);
						if model.has(
							entity,
							sub_type_of,
							&crate::intents::model::Value::Entity(type_entity),
						) {
							let name = model.qualified_name(entity);
							let local = name.rsplit(':').next().unwrap_or(&name);
							if local == value_str {
								is_valid = true;
								break;
							}
						}
					}
				}

				if is_valid {
					Ok(EvalResult::Literal(value_str))
				} else {
					Ok(EvalResult::Empty)
				}
			}
		}
	}

	fn eval_expr_to_entity(
		&self,
		model: &mut Model,
		expr: &Expr,
		env: &Env,
		is_derive: bool,
	) -> Result<EntityId, RuleError> {
		match expr {
			Expr::Var(name, _) => match env.get(name) {
				Some(EvalResult::Entity(id)) => Ok(*id),
				Some(EvalResult::Set(ids)) if ids.len() == 1 => Ok(ids[0]),
				Some(EvalResult::Set(ids)) => Err(RuleError {
					message: format!(
						"variable '{}' is a set with {} elements, expected single entity",
						name,
						ids.len()
					),
				}),
				Some(_) => Err(RuleError {
					message: format!("variable '{}' is not an entity", name),
				}),
				None => Err(RuleError {
					message: format!("undefined variable: {}", name),
				}),
			},
			Expr::Blank(_) => {
				if !is_derive {
					return Err(RuleError {
						message: "blank nodes not allowed in rules".to_string(),
					});
				}
				let id = model.blank();
				Ok(id)
			}
			Expr::Query(query) => {
				let engine = QueryEngine::new();
				let results = engine.execute(model, &query.path)?;
				match results.len() {
					1 => Ok(results[0]),
					0 => Err(RuleError {
						message: "query returned no results, expected single entity".to_string(),
					}),
					n => Err(RuleError {
						message: format!("query returned {} results, expected single entity", n),
					}),
				}
			}
			Expr::QName(qname) => {
				let name = qname.to_string();
				model.resolve(&name).ok_or_else(|| RuleError {
					message: format!("unresolved name: {}", name),
				})
			}
			Expr::Add(add_expr) => {
				if !is_derive {
					return Err(RuleError {
						message: "add expressions not allowed in rules".to_string(),
					});
				}
				let subject = self.eval_expr_to_entity(model, &add_expr.subject, env, is_derive)?;
				let pred_name = add_expr.predicate.to_string();
				let object = self.eval_expr(model, &add_expr.object, env, is_derive)?;

				match object {
					EvalResult::Entity(obj_id) => {
						model
							.apply_entity(subject, &pred_name, obj_id)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Literal(s) => {
						model
							.apply_literal(subject, &pred_name, &s)
							.map_err(|e| RuleError {
								message: format!("failed to add statement: {}", e),
							})?;
					}
					EvalResult::Set(_) => {
						return Err(RuleError {
							message: "cannot use set as object".to_string(),
						});
					}
					EvalResult::Empty => {
						return Err(RuleError {
							message: "cannot use empty as object".to_string(),
						});
					}
				}

				Ok(subject)
			}
			Expr::As(_) => Err(RuleError {
				message: "as() expression cannot be converted to entity".to_string(),
			}),
			_ => Err(RuleError {
				message: format!("expression cannot be converted to entity: {:?}", expr),
			}),
		}
	}
}

/// Evaluation result
#[derive(Debug, Clone)]
pub enum EvalResult {
	Entity(EntityId),
	Set(Vec<EntityId>),
	Literal(String),
	Empty,
}

impl EvalResult {
	pub fn is_empty(&self) -> bool {
		match self {
			EvalResult::Set(v) => v.is_empty(),
			EvalResult::Literal(s) => s.is_empty(),
			EvalResult::Empty => true,
			EvalResult::Entity(_) => false,
		}
	}
}

/// Environment for variable bindings
#[derive(Debug, Clone, Default)]
pub struct Env {
	bindings: HashMap<String, EvalResult>,
}

impl Env {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn bind(&mut self, name: String, value: EvalResult) {
		self.bindings.insert(name, value);
	}

	pub fn get(&self, name: &str) -> Option<&EvalResult> {
		self.bindings.get(name)
	}

	pub fn last_entity(&self) -> Option<EntityId> {
		// Find any entity binding (most recent iteration variable)
		for value in self.bindings.values() {
			if let EvalResult::Entity(id) = value {
				return Some(*id);
			}
		}
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::intents::model::Model;

	// ==================== Truthiness ====================
	mod truthiness {
		use super::*;

		#[test]
		fn entity_always_true() {
			let engine = RuleEngine::new();
			let result = EvalResult::Entity(EntityId(42));
			assert!(
				engine.is_satisfied(&result),
				"Entity should always be truthy"
			);
		}

		#[test]
		fn set_non_empty_is_true() {
			let engine = RuleEngine::new();

			let result = EvalResult::Set(vec![EntityId(1), EntityId(2)]);
			assert!(
				engine.is_satisfied(&result),
				"Non-empty set should be truthy"
			);

			let result = EvalResult::Set(vec![EntityId(1)]);
			assert!(
				engine.is_satisfied(&result),
				"Single element set should be truthy"
			);
		}

		#[test]
		fn set_empty_is_false() {
			let engine = RuleEngine::new();
			let result = EvalResult::Set(vec![]);
			assert!(!engine.is_satisfied(&result), "Empty set should be falsy");
		}

		#[test]
		fn literal_non_empty_is_true() {
			let engine = RuleEngine::new();

			let result = EvalResult::Literal("hello".to_string());
			assert!(
				engine.is_satisfied(&result),
				"Non-empty literal should be truthy"
			);

			let result = EvalResult::Literal("true".to_string());
			assert!(
				engine.is_satisfied(&result),
				"Literal 'true' should be truthy"
			);

			let result = EvalResult::Literal("some value".to_string());
			assert!(
				engine.is_satisfied(&result),
				"Literal 'some value' should be truthy"
			);
		}

		#[test]
		fn literal_empty_is_false() {
			let engine = RuleEngine::new();
			let result = EvalResult::Literal("".to_string());
			assert!(
				!engine.is_satisfied(&result),
				"Empty literal should be falsy"
			);
		}

		#[test]
		fn literal_false_is_false() {
			let engine = RuleEngine::new();
			let result = EvalResult::Literal("false".to_string());
			assert!(
				!engine.is_satisfied(&result),
				"Literal 'false' should be falsy"
			);
		}

		#[test]
		fn empty_always_false() {
			let engine = RuleEngine::new();
			let result = EvalResult::Empty;
			assert!(
				!engine.is_satisfied(&result),
				"Empty should always be falsy"
			);
		}
	}

	// ==================== Empty Expression ====================
	mod empty_expr {
		use super::*;

		#[test]
		fn on_empty_set_returns_true() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			let env = Env::new();

			let inner = Expr::Query(QueryExpr {
				path: QueryPath {
					steps: vec![QueryStep {
						axis: Axis::Child,
						node_test: Some(QualifiedName {
							namespace: Some("core".to_string()),
							name: "NonExistent".to_string(),
							span: 0..0,
						}),
						predicates: vec![],
						span: 0..0,
					}],
					span: 0..0,
				},
				span: 0..0,
			});

			let expr = Expr::Empty(Box::new(inner), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Literal(s) if s == "true"),
				"empty(empty set) should return 'true'"
			);
		}

		#[test]
		fn on_non_empty_set_returns_empty() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();

			let thing_type = model.ensure_entity("core:Thing").unwrap();
			model.apply_to(thing_type, "wa2:type", "wa2:Type").unwrap();

			let entity = model.blank();
			model.apply_entity(entity, "wa2:type", thing_type).unwrap();

			let env = Env::new();

			let inner = Expr::Query(QueryExpr {
				path: QueryPath {
					steps: vec![QueryStep {
						axis: Axis::Child,
						node_test: Some(QualifiedName {
							namespace: Some("core".to_string()),
							name: "Thing".to_string(),
							span: 0..0,
						}),
						predicates: vec![],
						span: 0..0,
					}],
					span: 0..0,
				},
				span: 0..0,
			});

			let expr = Expr::Empty(Box::new(inner), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Empty),
				"empty(non-empty set) should return Empty"
			);
		}

		#[test]
		fn on_empty_literal_returns_true() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			let env = Env::new();

			let expr = Expr::Empty(Box::new(Expr::String("".to_string(), 0..0)), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Literal(s) if s == "true"),
				"empty('') should return 'true'"
			);
		}

		#[test]
		fn on_non_empty_literal_returns_empty() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			let env = Env::new();

			let expr = Expr::Empty(Box::new(Expr::String("hello".to_string(), 0..0)), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Empty),
				"empty('hello') should return Empty"
			);
		}

		#[test]
		fn on_false_literal_returns_true() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			let env = Env::new();

			let expr = Expr::Empty(Box::new(Expr::Bool(false, 0..0)), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Literal(s) if s == "true"),
				"empty(false) should return 'true' because false is falsy"
			);
		}

		#[test]
		fn on_entity_returns_empty() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();

			let entity = model.blank();
			let mut env = Env::new();
			env.bind("x".to_string(), EvalResult::Entity(entity));

			let expr = Expr::Empty(Box::new(Expr::Var("x".to_string(), 0..0)), 0..0);
			let result = engine.eval_expr(&mut model, &expr, &env, false).unwrap();

			assert!(
				matches!(result, EvalResult::Empty),
				"empty(entity) should return Empty"
			);
		}
	}

	// ==================== Policy Ordering ====================
	mod policy_ordering {
		use super::*;

		#[test]
		fn must_rule_failure_stops_policy() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			let item = model.blank();
			model.apply_entity(item, "wa2:type", item_type).unwrap();

			// First rule - will fail
			let rule1 = Rule {
				name: "first_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must,
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "NonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			// Second rule - should not run if first fails with must
			let rule2 = Rule {
				name: "second_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must,
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "AlsoNonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let rules_map: HashMap<String, Rule> = vec![
				("first_rule".to_string(), rule1),
				("second_rule".to_string(), rule2),
			]
			.into_iter()
			.collect();

			let bindings = vec![
				OrderedBinding {
					policy_name: "test_policy".to_string(),
					modal: Modal::Must,
					rule_name: "first_rule".to_string(),
				},
				OrderedBinding {
					policy_name: "test_policy".to_string(),
					modal: Modal::Must,
					rule_name: "second_rule".to_string(),
				},
			];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			assert_eq!(result.outcome, PolicyOutcome::Fail);

			// Should only have one failure (from first rule)
			let failure_type = model.resolve("core:AssertFailure").unwrap();
			let failures: Vec<_> = (0..model.entity_count())
				.filter(|i| {
					let e = EntityId(*i as u32);
					model.has_type(e, failure_type)
				})
				.collect();

			assert_eq!(
				failures.len(),
				1,
				"Only first rule should have run before guard stopped policy"
			);

			// Verify trace
			assert_eq!(result.trace.len(), 1, "Trace should have one entry");
			assert_eq!(result.trace[0].rule_name, "first_rule");
			assert!(!result.trace[0].passed);
		}

		#[test]
		fn should_rule_failure_continues_policy() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			let item = model.blank();
			model.apply_entity(item, "wa2:type", item_type).unwrap();

			// First rule - will fail but is should
			let rule1 = Rule {
				name: "first_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must, // Error-level finding
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "NonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			// Second rule - should run even though first failed
			let rule2 = Rule {
				name: "second_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must,
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "AlsoNonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let rules_map: HashMap<String, Rule> = vec![
				("first_rule".to_string(), rule1),
				("second_rule".to_string(), rule2),
			]
			.into_iter()
			.collect();

			let bindings = vec![
				OrderedBinding {
					policy_name: "test_policy".to_string(),
					modal: Modal::Should, // should at policy level means continue even on error
					rule_name: "first_rule".to_string(),
				},
				OrderedBinding {
					policy_name: "test_policy".to_string(),
					modal: Modal::Must,
					rule_name: "second_rule".to_string(),
				},
			];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			// Second rule also fails with must, so policy fails
			assert_eq!(result.outcome, PolicyOutcome::Fail);

			// Should have two failures (both rules ran)
			let failure_type = model.resolve("core:AssertFailure").unwrap();
			let failures: Vec<_> = (0..model.entity_count())
				.filter(|i| {
					let e = EntityId(*i as u32);
					model.has_type(e, failure_type)
				})
				.collect();

			assert_eq!(
				failures.len(),
				2,
				"Both rules should have run because first was 'should' at policy level"
			);

			// Verify trace has both rules
			assert_eq!(result.trace.len(), 2);
		}

		#[test]
		fn all_rules_pass_returns_pass() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			let item = model.blank();
			model.apply_entity(item, "wa2:type", item_type).unwrap();

			// Rule that passes
			let rule = Rule {
				name: "passing_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must,
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("test".to_string()),
										name: "Item".to_string(), // This type exists
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let rules_map: HashMap<String, Rule> = vec![("passing_rule".to_string(), rule)]
				.into_iter()
				.collect();

			let bindings = vec![OrderedBinding {
				policy_name: "test_policy".to_string(),
				modal: Modal::Must,
				rule_name: "passing_rule".to_string(),
			}];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			assert_eq!(result.outcome, PolicyOutcome::Pass);
			assert_eq!(result.trace.len(), 1);
			assert!(result.trace[0].passed);
		}

		#[test]
		fn degraded_when_should_fails_but_no_must_fails() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			let item = model.blank();
			model.apply_entity(item, "wa2:type", item_type).unwrap();

			// Rule that creates Error-level finding
			let failing_rule = Rule {
				name: "failing_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must, // Creates Error finding
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "NonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let rules_map: HashMap<String, Rule> = vec![("failing_rule".to_string(), failing_rule)]
				.into_iter()
				.collect();

			// Rule bound with should at policy level
			let bindings = vec![OrderedBinding {
				policy_name: "test_policy".to_string(),
				modal: Modal::Should,
				rule_name: "failing_rule".to_string(),
			}];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			assert_eq!(
				result.outcome,
				PolicyOutcome::Degraded,
				"Should return Degraded when should-bound rule fails"
			);
		}

		#[test]
		fn empty_bindings_returns_pass() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();

			let rules_map: HashMap<String, Rule> = HashMap::new();
			let bindings: Vec<OrderedBinding> = vec![];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			assert_eq!(
				result.outcome,
				PolicyOutcome::Pass,
				"Empty policy should pass"
			);
			assert!(result.trace.is_empty());
		}

		#[test]
		fn trace_captures_finding_count() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("core").unwrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			// Create multiple items
			for _ in 0..3 {
				let item = model.blank();
				model.apply_entity(item, "wa2:type", item_type).unwrap();
			}

			// Rule that fails on each item
			let rule = Rule {
				name: "multi_fail_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Modal(ModalStmt {
						modal: Modal::Must,
						expr: Expr::Query(QueryExpr {
							path: QueryPath {
								steps: vec![QueryStep {
									axis: Axis::Child,
									node_test: Some(QualifiedName {
										namespace: Some("core".to_string()),
										name: "NonExistent".to_string(),
										span: 0..0,
									}),
									predicates: vec![],
									span: 0..0,
								}],
								span: 0..0,
							},
							span: 0..0,
						}),
						metadata: None,
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let rules_map: HashMap<String, Rule> = vec![("multi_fail_rule".to_string(), rule)]
				.into_iter()
				.collect();

			// Use should so we don't stop early
			let bindings = vec![OrderedBinding {
				policy_name: "test_policy".to_string(),
				modal: Modal::Should,
				rule_name: "multi_fail_rule".to_string(),
			}];

			let mut engine = RuleEngine::new();
			let result = engine
				.run_with_policy(&mut model, &[], &rules_map, &bindings)
				.unwrap();

			assert_eq!(result.trace.len(), 1);
			// Should have 3 findings (one per item)
			assert_eq!(result.trace[0].finding_count, 3);
			assert!(!result.trace[0].passed);
		}
	}

	// ==================== Two-Phase Execution ====================
	mod two_phase_execution {
		use super::*;

		#[test]
		fn add_statement_rejected_in_rules() {
			let mut model = Model::bootstrap();
			model.ensure_namespace("test").unwrap();

			let item_type = model.ensure_entity("test:Item").unwrap();
			model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

			let item = model.blank();
			model.apply_entity(item, "wa2:type", item_type).unwrap();

			// Rule with add statement (should fail)
			let rule = Rule {
				name: "bad_rule".to_string(),
				body: vec![Statement::For(ForStmt {
					var: "x".to_string(),
					collection: Expr::Query(QueryExpr {
						path: QueryPath {
							steps: vec![QueryStep {
								axis: Axis::Child,
								node_test: Some(QualifiedName {
									namespace: Some("test".to_string()),
									name: "Item".to_string(),
									span: 0..0,
								}),
								predicates: vec![],
								span: 0..0,
							}],
							span: 0..0,
						},
						span: 0..0,
					}),
					body: vec![Statement::Add(AddStmt {
						subject: Expr::Var("x".to_string(), 0..0),
						predicate: QualifiedName {
							namespace: Some("test".to_string()),
							name: "tag".to_string(),
							span: 0..0,
						},
						object: Expr::String("value".to_string(), 0..0),
						span: 0..0,
					})],
					span: 0..0,
				})],
				span: 0..0,
			};

			let mut engine = RuleEngine::new();
			let result = engine.run(&mut model, &[rule]);

			assert!(result.is_err());
			assert!(result.unwrap_err().message.contains("not allowed in rules"));
		}

		#[test]
		fn blank_node_rejected_in_rules() {
			let engine = RuleEngine::new();
			let mut model = Model::bootstrap();
			model.ensure_namespace("test").unwrap();

			let env = Env::new();

			// Blank node expression should fail in rule context
			let result = engine.eval_expr_to_entity(&mut model, &Expr::Blank(0..0), &env, false);

			assert!(result.is_err());
			assert!(result.unwrap_err().message.contains("not allowed in rules"));
		}
	}
}
