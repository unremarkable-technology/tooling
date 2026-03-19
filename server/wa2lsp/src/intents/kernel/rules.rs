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
		self.run_with_modals(model, &[], rules, &HashMap::new())
	}

	pub fn run_with_modals(
		&mut self,
		model: &mut Model,
		derives: &[Derive],
		rules: &[Rule],
		rule_modals: &HashMap<String, Modal>,
	) -> Result<(), RuleError> {
		self.processed.clear();
		self.deferred_musts.clear();

		// Phase 1: Run derives to fixed-point (model building)
		self.run_derives(model, derives)?;

		// Phase 2: Run rules to fixed-point, defer must failures
		self.processed.clear(); // Reset for rules phase
		for _ in 0..self.max_iterations {
			let initial_count = model.statement_count();

			for rule in rules {
				// TODO: make into logging
				//eprintln!("\trunning rule {}", rule.name);
				self.current_modal = rule_modals.get(&rule.name).copied();
				self.execute_rule(model, rule)?;
			}

			let final_count = model.statement_count();
			if final_count == initial_count {
				break;
			}
		}

		// Phase 3: Re-evaluate deferred musts and create failures
		for deferred in std::mem::take(&mut self.deferred_musts) {
			// TODO: make into logging
			//eprintln!("\trunning rule (final) {}", deferred.rule_name);
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

	/// Run derives to fixed-point (model building phase)
	pub fn run_derives(&mut self, model: &mut Model, derives: &[Derive]) -> Result<(), RuleError> {
		self.processed.clear();

		for derive in derives {
			// TODO: make into logging
			//eprintln!("\trunning derive {}", derive.name);
		}

		for _ in 0..self.max_iterations {
			let initial_count = model.statement_count();

			for derive in derives {
				// Derives use should as default modal (not must)
				self.current_modal = Some(Modal::Should);
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
		self.execute_statements(model, &derive.body, &mut env, &derive.name, &binding)
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

			Statement::Modal(must_stmt) => {
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
					// Use modal from statement, fall back to current_modal (from policy), then Must
					let modal = must_stmt.modal;

					self.deferred_musts.push(DeferredMust {
						rule_name: rule_name.to_string(),
						expr: must_stmt.expr.clone(),
						env: env.clone(),
						subject,
						area,
						message,
						modal,
					});

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

				// Strip quotes from value if present (model stores literals as "value")
				let value_str = if value_str.starts_with('"')
					&& value_str.ends_with('"')
					&& value_str.len() >= 2
				{
					value_str[1..value_str.len() - 1].to_string()
				} else {
					value_str
				};

				// Check as_type conversion if specified
				if let Some(ref as_expr) = match_expr.as_type {
					let type_name = as_expr.target_type.to_string();
					if let Some(type_entity) = model.resolve(&type_name) {
						// Get all variants of this enum (entities with wa2:subTypeOf -> this enum)
						let mut valid_variants = Vec::new();
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
									valid_variants.push(local.to_string());
								}
							}
						}

						// Check if value matches a variant
						let is_valid = valid_variants.iter().any(|v| v == &value_str);

						if !is_valid && as_expr.mode != Modal::May {
							// Get subject from first variable in match query
							let subject = if let Expr::Query(ref query) = match_expr.value {
								if let Some(first_step) = query.path.steps.first() {
									if let Some(ref node_test) = first_step.node_test {
										if node_test.namespace.is_none() {
											if let Some(result) = env.get(&node_test.name) {
												match result {
													EvalResult::Entity(id) => Some(*id),
													EvalResult::Set(ids) if !ids.is_empty() => {
														Some(ids[0])
													}
													_ => None,
												}
											} else {
												None
											}
										} else {
											None
										}
									} else {
										None
									}
								} else {
									None
								}
							} else {
								None
							};

							// Create failure with appropriate severity
							let message =
								format!("Value '{}' is not a valid {}", value_str, type_name);
							self.create_rich_failure(
								model,
								"as_conversion",
								subject,
								Some(type_entity),
								Some(message),
								as_expr.mode,
							)?;

							// Guard: return Empty for must/should
							return Ok(EvalResult::Empty);
						}
					} else {
						// Type not found - always an error (framework bug, not data validation)
						return Err(RuleError {
							message: format!("type '{}' not found for as() conversion", type_name),
						});
					}
				}

				// Find matching arm
				for arm in &match_expr.arms {
					let matches = arm.patterns.iter().any(|pattern| match pattern {
						MatchPattern::Else => true,
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
					let value = self.eval_expr(model, &validation.inner, env)?;
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
			Expr::As(_) => Err(RuleError {
				message: "as() expression cannot be converted to entity".to_string(),
			}),
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
    use crate::intents::kernel::ast::*;
    use crate::intents::kernel::query::QueryEngine;
    use crate::intents::model::Model;

    // ==================== Truthiness ====================
    mod truthiness {
        use super::*;

        #[test]
        fn entity_always_true() {
            let engine = RuleEngine::new();
            let result = EvalResult::Entity(EntityId(42));
            assert!(engine.is_satisfied(&result), "Entity should always be truthy");
        }

        #[test]
        fn set_non_empty_is_true() {
            let engine = RuleEngine::new();

            let result = EvalResult::Set(vec![EntityId(1), EntityId(2)]);
            assert!(engine.is_satisfied(&result), "Non-empty set should be truthy");

            let result = EvalResult::Set(vec![EntityId(1)]);
            assert!(engine.is_satisfied(&result), "Single element set should be truthy");
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
            assert!(engine.is_satisfied(&result), "Non-empty literal should be truthy");

            let result = EvalResult::Literal("true".to_string());
            assert!(engine.is_satisfied(&result), "Literal 'true' should be truthy");

            let result = EvalResult::Literal("some value".to_string());
            assert!(engine.is_satisfied(&result), "Literal 'some value' should be truthy");
        }

        #[test]
        fn literal_empty_is_false() {
            let engine = RuleEngine::new();
            let result = EvalResult::Literal("".to_string());
            assert!(!engine.is_satisfied(&result), "Empty literal should be falsy");
        }

        #[test]
        fn literal_false_is_false() {
            let engine = RuleEngine::new();
            let result = EvalResult::Literal("false".to_string());
            assert!(!engine.is_satisfied(&result), "Literal 'false' should be falsy");
        }

        #[test]
        fn empty_always_false() {
            let engine = RuleEngine::new();
            let result = EvalResult::Empty;
            assert!(!engine.is_satisfied(&result), "Empty should always be falsy");
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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

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
            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            assert!(
                matches!(result, EvalResult::Empty),
                "empty(entity) should return Empty"
            );
        }
    }

    // ==================== Modal Guards ====================
    mod modal_guards {
        use super::*;

        #[test]
        fn must_skips_rest_of_block() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("core").unwrap();
            model.ensure_namespace("test").unwrap();

            let marker_type = model.ensure_entity("test:Marker").unwrap();
            model.apply_to(marker_type, "wa2:type", "wa2:Type").unwrap();

            let rule = Rule {
                name: "test_rule".to_string(),
                body: vec![
                    Statement::Modal(ModalStmt {
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
                    }),
                    Statement::Add(AddStmt {
                        subject: Expr::Blank(0..0),
                        predicate: QualifiedName {
                            namespace: Some("wa2".to_string()),
                            name: "type".to_string(),
                            span: 0..0,
                        },
                        object: Expr::QName(QualifiedName {
                            namespace: Some("test".to_string()),
                            name: "Marker".to_string(),
                            span: 0..0,
                        }),
                        span: 0..0,
                    }),
                ],
                span: 0..0,
            };

            let mut engine = RuleEngine::new();
            engine.run(&mut model, &[rule]).unwrap();

            let markers: Vec<_> = (0..model.entity_count())
                .filter(|i| {
                    let e = EntityId(*i as u32);
                    model.has_type(e, marker_type)
                })
                .collect();

            assert!(
                markers.is_empty(),
                "must guard should prevent subsequent statements from running"
            );
        }

        #[test]
        fn should_skips_rest_of_block() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("core").unwrap();
            model.ensure_namespace("test").unwrap();

            let marker_type = model.ensure_entity("test:Marker").unwrap();
            model.apply_to(marker_type, "wa2:type", "wa2:Type").unwrap();

            let rule = Rule {
                name: "test_rule".to_string(),
                body: vec![
                    Statement::Modal(ModalStmt {
                        modal: Modal::Should,
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
                    }),
                    Statement::Add(AddStmt {
                        subject: Expr::Blank(0..0),
                        predicate: QualifiedName {
                            namespace: Some("wa2".to_string()),
                            name: "type".to_string(),
                            span: 0..0,
                        },
                        object: Expr::QName(QualifiedName {
                            namespace: Some("test".to_string()),
                            name: "Marker".to_string(),
                            span: 0..0,
                        }),
                        span: 0..0,
                    }),
                ],
                span: 0..0,
            };

            let mut engine = RuleEngine::new();
            engine.run(&mut model, &[rule]).unwrap();

            let markers: Vec<_> = (0..model.entity_count())
                .filter(|i| {
                    let e = EntityId(*i as u32);
                    model.has_type(e, marker_type)
                })
                .collect();

            assert!(
                markers.is_empty(),
                "should guard should prevent subsequent statements from running"
            );
        }

        #[test]
        fn may_does_not_guard() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("core").unwrap();
            model.ensure_namespace("test").unwrap();

            let marker_type = model.ensure_entity("test:Marker").unwrap();
            model.apply_to(marker_type, "wa2:type", "wa2:Type").unwrap();

            let trigger_type = model.ensure_entity("test:Trigger").unwrap();
            model.apply_to(trigger_type, "wa2:type", "wa2:Type").unwrap();

            let trigger = model.blank();
            model.apply_entity(trigger, "wa2:type", trigger_type).unwrap();

            let rule = Rule {
                name: "test_rule".to_string(),
                body: vec![Statement::For(ForStmt {
                    var: "t".to_string(),
                    collection: Expr::Query(QueryExpr {
                        path: QueryPath {
                            steps: vec![QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: Some("test".to_string()),
                                    name: "Trigger".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            }],
                            span: 0..0,
                        },
                        span: 0..0,
                    }),
                    body: vec![
                        Statement::Modal(ModalStmt {
                            modal: Modal::May,
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
                        }),
                        Statement::Add(AddStmt {
                            subject: Expr::Blank(0..0),
                            predicate: QualifiedName {
                                namespace: Some("wa2".to_string()),
                                name: "type".to_string(),
                                span: 0..0,
                            },
                            object: Expr::QName(QualifiedName {
                                namespace: Some("test".to_string()),
                                name: "Marker".to_string(),
                                span: 0..0,
                            }),
                            span: 0..0,
                        }),
                    ],
                    span: 0..0,
                })],
                span: 0..0,
            };

            let mut engine = RuleEngine::new();
            engine.run(&mut model, &[rule]).unwrap();

            let markers: Vec<_> = (0..model.entity_count())
                .filter(|i| {
                    let e = EntityId(*i as u32);
                    model.has_type(e, marker_type)
                })
                .collect();

            assert_eq!(
                markers.len(),
                1,
                "may should NOT guard - subsequent statements should run"
            );
        }

        #[test]
        fn guard_only_affects_current_iteration() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("core").unwrap();
            model.ensure_namespace("test").unwrap();

            let item_type = model.ensure_entity("test:Item").unwrap();
            model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

            let processed_type = model.ensure_entity("test:Processed").unwrap();
            model.apply_to(processed_type, "wa2:type", "wa2:Type").unwrap();

            let field_marker_type = model.ensure_entity("test:FieldMarker").unwrap();
            model.apply_to(field_marker_type, "wa2:type", "wa2:Type").unwrap();

            model.ensure_entity("test:markedAs").unwrap();

            // Item 1 - has field
            let item1 = model.blank();
            model.apply_entity(item1, "wa2:type", item_type).unwrap();
            let field1 = model.blank();
            model.apply_entity(field1, "wa2:type", field_marker_type).unwrap();
            model.apply_entity(item1, "test:hasField", field1).unwrap();

            // Item 2 - no field (will fail should, guard)
            let item2 = model.blank();
            model.apply_entity(item2, "wa2:type", item_type).unwrap();

            // Item 3 - has field
            let item3 = model.blank();
            model.apply_entity(item3, "wa2:type", item_type).unwrap();
            let field3 = model.blank();
            model.apply_entity(field3, "wa2:type", field_marker_type).unwrap();
            model.apply_entity(item3, "test:hasField", field3).unwrap();

            let rule = Rule {
                name: "test_rule".to_string(),
                body: vec![Statement::For(ForStmt {
                    var: "item".to_string(),
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
                    body: vec![
                        Statement::Modal(ModalStmt {
                            modal: Modal::Should,
                            expr: Expr::Query(QueryExpr {
                                path: QueryPath {
                                    steps: vec![
                                        QueryStep {
                                            axis: Axis::Child,
                                            node_test: Some(QualifiedName {
                                                namespace: None,
                                                name: "item".to_string(),
                                                span: 0..0,
                                            }),
                                            predicates: vec![],
                                            span: 0..0,
                                        },
                                        QueryStep {
                                            axis: Axis::Child,
                                            node_test: Some(QualifiedName {
                                                namespace: Some("test".to_string()),
                                                name: "hasField".to_string(),
                                                span: 0..0,
                                            }),
                                            predicates: vec![],
                                            span: 0..0,
                                        },
                                    ],
                                    span: 0..0,
                                },
                                span: 0..0,
                            }),
                            metadata: None,
                            span: 0..0,
                        }),
                        Statement::Add(AddStmt {
                            subject: Expr::Var("item".to_string(), 0..0),
                            predicate: QualifiedName {
                                namespace: Some("test".to_string()),
                                name: "markedAs".to_string(),
                                span: 0..0,
                            },
                            object: Expr::QName(QualifiedName {
                                namespace: Some("test".to_string()),
                                name: "Processed".to_string(),
                                span: 0..0,
                            }),
                            span: 0..0,
                        }),
                    ],
                    span: 0..0,
                })],
                span: 0..0,
            };

            let mut engine = RuleEngine::new();
            engine.run(&mut model, &[rule]).unwrap();

            let marked_as_pred = model.resolve("test:markedAs").unwrap();
            let processed: Vec<_> = (0..model.entity_count())
                .filter(|i| {
                    let e = EntityId(*i as u32);
                    let values = model.get_all(e, marked_as_pred);
                    values.iter().any(|v| {
                        if let crate::intents::model::Value::Entity(target) = v {
                            *target == processed_type
                        } else {
                            false
                        }
                    })
                })
                .collect();

            assert_eq!(
                processed.len(),
                2,
                "Guard should only affect current iteration; item1 and item3 should be processed"
            );
        }
    }

    // ==================== Add Expression ====================
    mod add_expr {
        use super::*;

        #[test]
        fn blank_creates_new_entity() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();

            let marker_type = model.ensure_entity("test:Marker").unwrap();
            model.apply_to(marker_type, "wa2:type", "wa2:Type").unwrap();

            let initial_count = model.entity_count();

            let engine = RuleEngine::new();
            let env = Env::new();

            let expr = Expr::Add(Box::new(AddExpr {
                subject: Expr::Blank(0..0),
                predicate: QualifiedName {
                    namespace: Some("wa2".to_string()),
                    name: "type".to_string(),
                    span: 0..0,
                },
                object: Expr::QName(QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "Marker".to_string(),
                    span: 0..0,
                }),
                span: 0..0,
            }));

            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            assert!(model.entity_count() > initial_count, "Should create new entity");
            assert!(matches!(result, EvalResult::Entity(_)), "Add should return entity");
        }

        #[test]
        fn returns_subject_entity() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();

            let marker_type = model.ensure_entity("test:Marker").unwrap();
            model.apply_to(marker_type, "wa2:type", "wa2:Type").unwrap();

            let engine = RuleEngine::new();
            let env = Env::new();

            let expr = Expr::Add(Box::new(AddExpr {
                subject: Expr::Blank(0..0),
                predicate: QualifiedName {
                    namespace: Some("wa2".to_string()),
                    name: "type".to_string(),
                    span: 0..0,
                },
                object: Expr::QName(QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "Marker".to_string(),
                    span: 0..0,
                }),
                span: 0..0,
            }));

            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            if let EvalResult::Entity(id) = result {
                assert!(
                    model.has_type(id, marker_type),
                    "Returned entity should have the type we added"
                );
            } else {
                panic!("Add should return Entity, got {:?}", result);
            }
        }

        #[test]
        fn with_existing_entity_as_subject() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();

            let item_type = model.ensure_entity("test:Item").unwrap();
            model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

            let tag_type = model.ensure_entity("test:Tag").unwrap();
            model.apply_to(tag_type, "wa2:type", "wa2:Type").unwrap();

            let item = model.blank();
            model.apply_entity(item, "wa2:type", item_type).unwrap();

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("item".to_string(), EvalResult::Entity(item));

            let expr = Expr::Add(Box::new(AddExpr {
                subject: Expr::Var("item".to_string(), 0..0),
                predicate: QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "tag".to_string(),
                    span: 0..0,
                },
                object: Expr::QName(QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "Tag".to_string(),
                    span: 0..0,
                }),
                span: 0..0,
            }));

            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            if let EvalResult::Entity(id) = result {
                assert_eq!(id, item, "Add should return the subject entity");
            } else {
                panic!("Add should return Entity, got {:?}", result);
            }

            let tag_pred = model.resolve("test:tag").unwrap();
            let values = model.get_all(item, tag_pred);
            assert!(!values.is_empty(), "Item should have test:tag predicate");
        }

        #[test]
        fn with_literal_as_object() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();

            let item_type = model.ensure_entity("test:Item").unwrap();
            model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

            let item = model.blank();
            model.apply_entity(item, "wa2:type", item_type).unwrap();

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("item".to_string(), EvalResult::Entity(item));

            let expr = Expr::Add(Box::new(AddExpr {
                subject: Expr::Var("item".to_string(), 0..0),
                predicate: QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "name".to_string(),
                    span: 0..0,
                },
                object: Expr::String("hello".to_string(), 0..0),
                span: 0..0,
            }));

            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            assert!(matches!(result, EvalResult::Entity(id) if id == item));

            let name_pred = model.resolve("test:name").unwrap();
            let values = model.get_all(item, name_pred);
            assert_eq!(values.len(), 1, "Should have one value");
            assert!(
                matches!(&values[0], crate::intents::model::Value::Literal(s) if s == "hello"),
                "Value should be 'hello'"
            );
        }

        #[test]
        fn with_entity_as_object() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();

            let item_type = model.ensure_entity("test:Item").unwrap();
            model.apply_to(item_type, "wa2:type", "wa2:Type").unwrap();

            let target_type = model.ensure_entity("test:Target").unwrap();
            model.apply_to(target_type, "wa2:type", "wa2:Type").unwrap();

            let source = model.blank();
            model.apply_entity(source, "wa2:type", item_type).unwrap();

            let target = model.blank();
            model.apply_entity(target, "wa2:type", target_type).unwrap();

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("source".to_string(), EvalResult::Entity(source));
            env.bind("target".to_string(), EvalResult::Entity(target));

            let expr = Expr::Add(Box::new(AddExpr {
                subject: Expr::Var("source".to_string(), 0..0),
                predicate: QualifiedName {
                    namespace: Some("test".to_string()),
                    name: "pointsTo".to_string(),
                    span: 0..0,
                },
                object: Expr::Var("target".to_string(), 0..0),
                span: 0..0,
            }));

            let result = engine.eval_expr(&mut model, &expr, &env).unwrap();

            assert!(matches!(result, EvalResult::Entity(id) if id == source));

            let points_to_pred = model.resolve("test:pointsTo").unwrap();
            let values = model.get_all(source, points_to_pred);
            assert_eq!(values.len(), 1, "Should have one value");
            assert!(
                matches!(&values[0], crate::intents::model::Value::Entity(e) if *e == target),
                "Value should be target entity"
            );
        }

        #[test]
        fn chained_creates_linked_entities() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("test").unwrap();
            model.ensure_namespace("core").unwrap();

            let evidence_type = model.ensure_entity("core:Evidence").unwrap();
            model.apply_to(evidence_type, "wa2:type", "wa2:Type").unwrap();

            let store_type = model.ensure_entity("core:Store").unwrap();
            model.apply_to(store_type, "wa2:type", "wa2:Type").unwrap();

            let store = model.blank();
            model.apply_entity(store, "wa2:type", store_type).unwrap();

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("store".to_string(), EvalResult::Entity(store));

            // First add: create evidence
            let add_evidence = Expr::Add(Box::new(AddExpr {
                subject: Expr::Blank(0..0),
                predicate: QualifiedName {
                    namespace: Some("wa2".to_string()),
                    name: "type".to_string(),
                    span: 0..0,
                },
                object: Expr::QName(QualifiedName {
                    namespace: Some("core".to_string()),
                    name: "Evidence".to_string(),
                    span: 0..0,
                }),
                span: 0..0,
            }));

            let evidence_result = engine.eval_expr(&mut model, &add_evidence, &env).unwrap();
            let evidence = match evidence_result {
                EvalResult::Entity(id) => id,
                _ => panic!("Expected entity"),
            };

            env.bind("evidence".to_string(), EvalResult::Entity(evidence));

            // Second add: link store to evidence
            let add_link = Expr::Add(Box::new(AddExpr {
                subject: Expr::Var("store".to_string(), 0..0),
                predicate: QualifiedName {
                    namespace: Some("wa2".to_string()),
                    name: "contains".to_string(),
                    span: 0..0,
                },
                object: Expr::Var("evidence".to_string(), 0..0),
                span: 0..0,
            }));

            engine.eval_expr(&mut model, &add_link, &env).unwrap();

            assert!(
                model.has_type(evidence, evidence_type),
                "Evidence should have core:Evidence type"
            );

            let contains_pred = model.resolve("wa2:contains").unwrap();
            let values = model.get_all(store, contains_pred);
            assert!(
                values.iter().any(|v| matches!(v, crate::intents::model::Value::Entity(e) if *e == evidence)),
                "Store should contain evidence"
            );
        }
    }

    // ==================== Match/As Conversion ====================
    mod match_as_conversion {
        use super::*;

        #[test]
        fn should_valid_value() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("my").unwrap();

            let enum_type = model.ensure_entity("my:Criticality").unwrap();
            model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

            let variant_high = model.ensure_entity("my:High").unwrap();
            model.apply_entity(variant_high, "wa2:subTypeOf", enum_type).unwrap();

            let variant_low = model.ensure_entity("my:Low").unwrap();
            model.apply_entity(variant_low, "wa2:subTypeOf", enum_type).unwrap();

            let tag = model.blank();
            model.apply_to(tag, "aws:Value", "\"High\"").unwrap();

            let match_expr = MatchExpr {
                value: Expr::Query(QueryExpr {
                    path: QueryPath {
                        steps: vec![
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: None,
                                    name: "tag".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: Some("aws".to_string()),
                                    name: "Value".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                        ],
                        span: 0..0,
                    },
                    span: 0..0,
                }),
                as_type: Some(AsExpr {
                    target_type: QualifiedName {
                        namespace: Some("my".to_string()),
                        name: "Criticality".to_string(),
                        span: 0..0,
                    },
                    mode: Modal::Should,
                    span: 0..0,
                }),
                arms: vec![
                    MatchArm {
                        patterns: vec![MatchPattern::Variant("High".to_string())],
                        result: Expr::Bool(true, 0..0),
                        span: 0..0,
                    },
                    MatchArm {
                        patterns: vec![MatchPattern::Else],
                        result: Expr::Bool(false, 0..0),
                        span: 0..0,
                    },
                ],
                span: 0..0,
            };

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("tag".to_string(), EvalResult::Entity(tag));

            let result = engine
                .eval_expr(&mut model, &Expr::Match(Box::new(match_expr)), &env)
                .unwrap();

            assert!(matches!(result, EvalResult::Literal(s) if s == "true"));

            let failures: Vec<_> = (0..model.entity_count())
                .filter_map(|i| {
                    let e = EntityId(i as u32);
                    if model.has_type(e, model.resolve("core:AssertFailure").unwrap_or(EntityId(0))) {
                        Some(e)
                    } else {
                        None
                    }
                })
                .collect();
            assert!(failures.is_empty(), "Valid value should not create failures");
        }

        #[test]
        fn should_invalid_value_creates_warning() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("my").unwrap();
            model.ensure_namespace("core").unwrap();

            let enum_type = model.ensure_entity("my:Criticality").unwrap();
            model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

            let variant_high = model.ensure_entity("my:High").unwrap();
            model.apply_entity(variant_high, "wa2:subTypeOf", enum_type).unwrap();

            let tag = model.blank();
            model.apply_to(tag, "aws:Value", "\"InvalidValue\"").unwrap();

            let match_expr = MatchExpr {
                value: Expr::Query(QueryExpr {
                    path: QueryPath {
                        steps: vec![
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: None,
                                    name: "tag".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: Some("aws".to_string()),
                                    name: "Value".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                        ],
                        span: 0..0,
                    },
                    span: 0..0,
                }),
                as_type: Some(AsExpr {
                    target_type: QualifiedName {
                        namespace: Some("my".to_string()),
                        name: "Criticality".to_string(),
                        span: 0..0,
                    },
                    mode: Modal::Should,
                    span: 0..0,
                }),
                arms: vec![
                    MatchArm {
                        patterns: vec![MatchPattern::Variant("High".to_string())],
                        result: Expr::Bool(true, 0..0),
                        span: 0..0,
                    },
                    MatchArm {
                        patterns: vec![MatchPattern::Else],
                        result: Expr::Bool(false, 0..0),
                        span: 0..0,
                    },
                ],
                span: 0..0,
            };

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("tag".to_string(), EvalResult::Entity(tag));

            let result = engine
                .eval_expr(&mut model, &Expr::Match(Box::new(match_expr)), &env)
                .unwrap();

            assert!(matches!(result, EvalResult::Empty));

            if let Some(failure_type) = model.resolve("core:AssertFailure") {
                let failures: Vec<_> = (0..model.entity_count())
                    .filter(|i| {
                        let e = EntityId(*i as u32);
                        model.has_type(e, failure_type)
                    })
                    .collect();
                assert_eq!(failures.len(), 1, "Should create one failure");
            }
        }

        #[test]
        fn may_invalid_value_no_warning() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("my").unwrap();

            let enum_type = model.ensure_entity("my:Criticality").unwrap();
            model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

            let variant_high = model.ensure_entity("my:High").unwrap();
            model.apply_entity(variant_high, "wa2:subTypeOf", enum_type).unwrap();

            let tag = model.blank();
            model.apply_to(tag, "aws:Value", "\"InvalidValue\"").unwrap();

            let match_expr = MatchExpr {
                value: Expr::Query(QueryExpr {
                    path: QueryPath {
                        steps: vec![
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: None,
                                    name: "tag".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: Some("aws".to_string()),
                                    name: "Value".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                        ],
                        span: 0..0,
                    },
                    span: 0..0,
                }),
                as_type: Some(AsExpr {
                    target_type: QualifiedName {
                        namespace: Some("my".to_string()),
                        name: "Criticality".to_string(),
                        span: 0..0,
                    },
                    mode: Modal::May,
                    span: 0..0,
                }),
                arms: vec![
                    MatchArm {
                        patterns: vec![MatchPattern::Variant("High".to_string())],
                        result: Expr::Bool(true, 0..0),
                        span: 0..0,
                    },
                    MatchArm {
                        patterns: vec![MatchPattern::Else],
                        result: Expr::Bool(false, 0..0),
                        span: 0..0,
                    },
                ],
                span: 0..0,
            };

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("tag".to_string(), EvalResult::Entity(tag));

            let result = engine
                .eval_expr(&mut model, &Expr::Match(Box::new(match_expr)), &env)
                .unwrap();

            assert!(matches!(result, EvalResult::Literal(s) if s == "false"));

            if let Some(failure_type) = model.resolve("core:AssertFailure") {
                let failures: Vec<_> = (0..model.entity_count())
                    .filter(|i| {
                        let e = EntityId(*i as u32);
                        model.has_type(e, failure_type)
                    })
                    .collect();
                assert!(failures.is_empty(), "May mode should not create failures");
            }
        }

        #[test]
        fn type_not_found_always_errors() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("my").unwrap();
            model.ensure_namespace("core").unwrap();

            let tag = model.blank();
            model.apply_to(tag, "aws:Value", "\"SomeValue\"").unwrap();

            let make_match_expr = |mode: Modal| MatchExpr {
                value: Expr::Query(QueryExpr {
                    path: QueryPath {
                        steps: vec![
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: None,
                                    name: "tag".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                            QueryStep {
                                axis: Axis::Child,
                                node_test: Some(QualifiedName {
                                    namespace: Some("aws".to_string()),
                                    name: "Value".to_string(),
                                    span: 0..0,
                                }),
                                predicates: vec![],
                                span: 0..0,
                            },
                        ],
                        span: 0..0,
                    },
                    span: 0..0,
                }),
                as_type: Some(AsExpr {
                    target_type: QualifiedName {
                        namespace: Some("my".to_string()),
                        name: "NonExistentType".to_string(),
                        span: 0..0,
                    },
                    mode,
                    span: 0..0,
                }),
                arms: vec![
                    MatchArm {
                        patterns: vec![MatchPattern::Variant("A".to_string())],
                        result: Expr::Bool(true, 0..0),
                        span: 0..0,
                    },
                    MatchArm {
                        patterns: vec![MatchPattern::Else],
                        result: Expr::Bool(false, 0..0),
                        span: 0..0,
                    },
                ],
                span: 0..0,
            };

            let engine = RuleEngine::new();
            let mut env = Env::new();
            env.bind("tag".to_string(), EvalResult::Entity(tag));

            for mode in [Modal::Should, Modal::May, Modal::Must] {
                let result = engine.eval_expr(
                    &mut model,
                    &Expr::Match(Box::new(make_match_expr(mode))),
                    &env,
                );
                assert!(result.is_err(), "Type not found with {:?} should error", mode);
                assert!(
                    result.unwrap_err().message.contains("not found"),
                    "Error should mention type not found"
                );
            }
        }
    }

    // ==================== Query/Extraction ====================
    mod query_extraction {
        use super::*;

        #[test]
        fn extract_literal_from_tag_value() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();

            let tag_entity = model.blank();
            model.apply_to(tag_entity, "aws:Key", "\"DataCriticality\"").unwrap();
            model.apply_to(tag_entity, "aws:Value", "\"BusinessCritical\"").unwrap();

            let engine = QueryEngine::new();
            let path = QueryPath {
                steps: vec![QueryStep {
                    axis: Axis::Child,
                    node_test: Some(QualifiedName {
                        namespace: Some("aws".to_string()),
                        name: "Value".to_string(),
                        span: 0..0,
                    }),
                    predicates: vec![],
                    span: 0..0,
                }],
                span: 0..0,
            };

            let literals = engine.extract_literals(&model, &[tag_entity], &path).unwrap();

            assert!(!literals.is_empty(), "Should extract the value");
            assert!(literals[0].contains("BusinessCritical"), "Should contain BusinessCritical");
        }

        #[test]
        fn match_with_as_conversion() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("my").unwrap();

            let enum_type = model.ensure_entity("my:DataCriticality").unwrap();
            model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

            let variant_bc = model.ensure_entity("my:BusinessCritical").unwrap();
            model.apply_entity(variant_bc, "wa2:type", enum_type).unwrap();

            let variant_mc = model.ensure_entity("my:MissionCritical").unwrap();
            model.apply_entity(variant_mc, "wa2:type", enum_type).unwrap();

            let variant_nc = model.ensure_entity("my:NonCritical").unwrap();
            model.apply_entity(variant_nc, "wa2:type", enum_type).unwrap();

            let tag_entity = model.blank();
            model.apply_to(tag_entity, "aws:Key", "\"DataCriticality\"").unwrap();
            model.apply_to(tag_entity, "aws:Value", "\"BusinessCritical\"").unwrap();

            let engine = QueryEngine::new();
            let path = QueryPath {
                steps: vec![QueryStep {
                    axis: Axis::Child,
                    node_test: Some(QualifiedName {
                        namespace: Some("aws".to_string()),
                        name: "Value".to_string(),
                        span: 0..0,
                    }),
                    predicates: vec![],
                    span: 0..0,
                }],
                span: 0..0,
            };

            let literals = engine.extract_literals(&model, &[tag_entity], &path).unwrap();

            let value_str = &literals[0];
            let value_str = if value_str.starts_with('"') && value_str.ends_with('"') && value_str.len() >= 2 {
                &value_str[1..value_str.len() - 1]
            } else {
                value_str.as_str()
            };

            let enum_id = model.resolve("my:DataCriticality").unwrap();
            let mut valid_variants = Vec::new();
            for i in 0..model.entity_count() {
                let entity = EntityId(i as u32);
                if model.has_type(entity, enum_id) {
                    let name = model.qualified_name(entity);
                    let local = name.rsplit(':').next().unwrap_or(&name);
                    valid_variants.push(local.to_string());
                }
            }

            let is_valid = valid_variants.iter().any(|v| v == value_str);
            assert!(is_valid, "BusinessCritical should be a valid variant");
        }

        #[test]
        fn dc_tag_query_result() {
            let mut model = Model::bootstrap();
            model.ensure_namespace("aws").unwrap();
            model.ensure_namespace("aws:cfn").unwrap();
            model.ensure_namespace("core").unwrap();

            let resource = model.ensure_entity("DataBucket").unwrap();
            model.apply_to(resource, "wa2:type", "aws:cfn:Resource").unwrap();

            let tags_container = model.blank();
            model.apply_entity(resource, "aws:Tags", tags_container).unwrap();

            let tag1 = model.blank();
            model.apply_entity(tags_container, "wa2:contains", tag1).unwrap();
            model.apply_to(tag1, "aws:Key", "\"DataSensitivity\"").unwrap();
            model.apply_to(tag1, "aws:Value", "\"Confidential\"").unwrap();

            let tag2 = model.blank();
            model.apply_entity(tags_container, "wa2:contains", tag2).unwrap();
            model.apply_to(tag2, "aws:Key", "\"DataCriticality\"").unwrap();
            model.apply_to(tag2, "aws:Value", "\"BusinessCritical\"").unwrap();

            let engine = QueryEngine::new();

            let tags_path = QueryPath {
                steps: vec![QueryStep {
                    axis: Axis::Child,
                    node_test: Some(QualifiedName {
                        namespace: Some("aws".to_string()),
                        name: "Tags".to_string(),
                        span: 0..0,
                    }),
                    predicates: vec![],
                    span: 0..0,
                }],
                span: 0..0,
            };
            let tags_result = engine.execute_from(&model, &[resource], &tags_path).unwrap();

            let children = model.children(tags_result[0]);

            let mut dc_tags = Vec::new();
            for child in &children {
                let key_pred = model.resolve("aws:Key").unwrap();
                let values = model.get_all(*child, key_pred);
                for v in values {
                    if let crate::intents::model::Value::Literal(s) = v {
                        if s == "DataCriticality" || s == "\"DataCriticality\"" {
                            dc_tags.push(*child);
                        }
                    }
                }
            }

            assert!(!dc_tags.is_empty(), "dc_tag query should return results");

            let value_path = QueryPath {
                steps: vec![QueryStep {
                    axis: Axis::Child,
                    node_test: Some(QualifiedName {
                        namespace: Some("aws".to_string()),
                        name: "Value".to_string(),
                        span: 0..0,
                    }),
                    predicates: vec![],
                    span: 0..0,
                }],
                span: 0..0,
            };
            let literals = engine.extract_literals(&model, &dc_tags, &value_path).unwrap();
            assert!(!literals.is_empty(), "Should find the value");
        }
    }
}