//! Rule execution engine with fixed-point iteration

use std::collections::{HashMap, HashSet};

use crate::intents::kernel::ast::*;
use crate::intents::kernel::query::QueryEngine;
use crate::intents::model::{EntityId, Model};

#[derive(Debug)]
pub struct RuleError {
	pub message: String,
}

/// Result of executing a statement
enum StmtResult {
	Continue,
	/// Guard failed - skip rest of body
	Guard,
}

/// Reference binding: map of loop variable names to entity values
type ReferenceBinding = Vec<(String, EntityId)>;

struct DeferredMust {
	rule_name: String,
	expr: Expr,
	env: Env,
	subject: Option<EntityId>,
	area: Option<EntityId>,
	message: Option<String>,
	modal: Modal,
}

pub struct RuleEngine {
	max_iterations: usize,
	/// Tracks (rule_name, reference_binding) combinations already processed
	processed: HashSet<(String, ReferenceBinding)>,
	current_modal: Option<Modal>,
	deferred_musts: Vec<DeferredMust>,
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
			current_modal: None,
			deferred_musts: Vec::new(),
		}
	}

	pub fn run(&mut self, model: &mut Model, rules: &[Rule]) -> Result<(), RuleError> {
		self.run_with_modals(model, rules, &HashMap::new())
	}

	pub fn run_with_modals(
		&mut self,
		model: &mut Model,
		rules: &[Rule],
		rule_modals: &HashMap<String, Modal>,
	) -> Result<(), RuleError> {
		self.processed.clear();
		self.deferred_musts.clear();

		// Phase 1: Run to fixed-point, defer must failures
		for _ in 0..self.max_iterations {
			let initial_count = model.statement_count();

			for rule in rules {
				self.current_modal = rule_modals.get(&rule.name).copied();
				self.execute_rule(model, rule)?;
			}

			let final_count = model.statement_count();
			if final_count == initial_count {
				break;
			}
		}

		// Phase 2: Re-evaluate deferred musts and create failures
		for deferred in std::mem::take(&mut self.deferred_musts) {
			let result = self.eval_expr(model, &deferred.expr, &deferred.env)?;
			if !self.is_satisfied(&result) {
				self.create_rich_failure(
					model,
					&deferred.rule_name,
					deferred.subject,
					deferred.area,
					deferred.message,
					deferred.modal,
				)?;
			}
		}

		Ok(())
	}

	fn execute_rule(&mut self, model: &mut Model, rule: &Rule) -> Result<(), RuleError> {
		let mut env = Env::new();
		let binding = Vec::new();
		self.execute_statements(model, &rule.body, &mut env, &rule.name, &binding)
	}

	fn execute_statements(
		&mut self,
		model: &mut Model,
		stmts: &[Statement],
		env: &mut Env,
		rule_name: &str,
		reference_binding: &ReferenceBinding,
	) -> Result<(), RuleError> {
		for stmt in stmts {
			match self.execute_statement(model, stmt, env, rule_name, reference_binding)? {
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
	) -> Result<StmtResult, RuleError> {
		match stmt {
			Statement::Let(let_stmt) => {
				let value = self.eval_expr(model, &let_stmt.value, env)?;
				env.bind(let_stmt.name.clone(), value);
				Ok(StmtResult::Continue)
			}

			Statement::Add(add_stmt) => {
				let subject = self.eval_expr_to_entity(model, &add_stmt.subject, env)?;
				let pred_name = add_stmt.predicate.to_string();
				let object = self.eval_expr(model, &add_stmt.object, env)?;

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
				let collection = self.eval_expr(model, &for_stmt.collection, env)?;
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
					)?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::If(if_stmt) => {
				let cond = self.eval_expr(model, &if_stmt.condition, env)?;
				if self.is_satisfied(&cond) {
					self.execute_statements(
						model,
						&if_stmt.then_body,
						env,
						rule_name,
						reference_binding,
					)?;
				} else if let Some(ref else_body) = if_stmt.else_body {
					self.execute_statements(model, else_body, env, rule_name, reference_binding)?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::Assert(assert_stmt) => {
				let result = self.eval_expr(model, &assert_stmt.expr, env)?;
				if !self.is_satisfied(&result) {
					self.create_failure(model, rule_name, "assertion failed")?;
				}
				Ok(StmtResult::Continue)
			}

			Statement::Must(must_stmt) => {
				let result = self.eval_expr(model, &must_stmt.expr, env)?;
				if !self.is_satisfied(&result) {
					// Evaluate subject/area now, defer failure creation
					let subject = if let Some(ref meta) = must_stmt.metadata {
						if let Some(ref subj_expr) = meta.subject {
							Some(self.eval_expr_to_entity(model, subj_expr, env)?)
						} else {
							self.get_context_entity(env)
						}
					} else {
						self.get_context_entity(env)
					};

					let area = if let Some(ref meta) = must_stmt.metadata {
						if let Some(ref area_name) = meta.area {
							model.resolve(&area_name.to_string())
						} else {
							None
						}
					} else {
						None
					};

					let message = must_stmt.metadata.as_ref().and_then(|m| m.message.clone());
					let modal = self.current_modal.unwrap_or(Modal::Must);

					self.deferred_musts.push(DeferredMust {
						rule_name: rule_name.to_string(),
						expr: must_stmt.expr.clone(),
						env: env.clone(),
						subject,
						area,
						message,
						modal,
					});

					// Guard: skip rest of body
					return Ok(StmtResult::Guard);
				}
				// If must passed, continue to next statements
				Ok(StmtResult::Continue)
			}
		}
	}

	fn create_rich_failure(
		&mut self,
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
		let assertion_text = format!("{}: must obligation not satisfied", rule_name);
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
		let modal_name = match modal {
			Modal::Must => "core:Error",
			Modal::Should => "core:Warning",
			Modal::May => "core:Info",
		};
		model
			.apply_to(failure, "core:severity", modal_name)
			.map_err(|e| RuleError {
				message: format!("failed to set severity: {}", e),
			})?;

		Ok(())
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
	) -> Result<EvalResult, RuleError> {
		match expr {
			Expr::Var(name, _) => env.get(name).cloned().ok_or_else(|| RuleError {
				message: format!("undefined variable: {}", name),
			}),

			Expr::Blank(_) => Err(RuleError {
				message: "blank node in eval context - should be handled in add".to_string(),
			}),

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
				let subject = self.eval_expr_to_entity(model, &add_expr.subject, env)?;
				let pred_name = add_expr.predicate.to_string();
				let object = self.eval_expr(model, &add_expr.object, env)?;

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
				let value = self.eval_expr(model, inner, env)?;
				Ok(if value.is_empty() {
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
					let value = self.eval_expr(model, &match_expr.value, env)?;
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

				// Find matching arm
				for arm in &match_expr.arms {
					let matches = arm.patterns.iter().any(|pattern| match pattern {
						MatchPattern::Wildcard => true,
						MatchPattern::Variant(name) => &value_str == name,
					});

					if matches {
						return self.eval_expr(model, &arm.result, env);
					}
				}

				Err(RuleError {
					message: format!("no matching arm for value: {}", value_str),
				})
			}
		}
	}

	fn eval_expr_to_entity(
		&self,
		model: &mut Model,
		expr: &Expr,
		env: &Env,
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
				let subject = self.eval_expr_to_entity(model, &add_expr.subject, env)?;
				let pred_name = add_expr.predicate.to_string();
				let object = self.eval_expr(model, &add_expr.object, env)?;

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
			_ => Err(RuleError {
				message: format!("expression cannot be converted to entity: {:?}", expr),
			}),
		}
	}

	fn modal_to_severity(modal: Modal) -> &'static str {
		match modal {
			Modal::Must => "error",
			Modal::Should => "warning",
			Modal::May => "info",
		}
	}

	fn create_failure_with_severity(
		&mut self,
		model: &mut Model,
		rule_name: &str,
		message: &str,
		context: Option<EntityId>,
		modal: Modal,
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

		// Set severity based on modal
		let severity = Self::modal_to_severity(modal);
		model
			.apply_literal(failure, "core:severity", severity)
			.map_err(|e| RuleError {
				message: format!("failed to set severity: {}", e),
			})?;

		if let Some(entity) = context {
			model
				.apply_entity(failure, "core:subject", entity)
				.map_err(|e| RuleError {
					message: format!("failed to link failure to subject: {}", e),
				})?;
		}

		Ok(())
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
