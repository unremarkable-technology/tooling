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
					Ok(EvalResult::Literal("true".to_string()))
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

#[test]
fn test_extract_literal_from_tag_value() {
	use crate::intents::kernel::ast::*;
	use crate::intents::kernel::query::QueryEngine;
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();

	// Create the aws namespace first
	model.ensure_namespace("aws").unwrap();

	// Simulate what CFN projector does for a tag
	let tag_entity = model.blank();

	// This is how cfn_projector stores string values
	model
		.apply_to(tag_entity, "aws:Key", "\"DataCriticality\"")
		.unwrap();
	model
		.apply_to(tag_entity, "aws:Value", "\"BusinessCritical\"")
		.unwrap();

	eprintln!("Tag entity: {:?}", tag_entity);
	eprintln!("Model:\n{}", model);

	// Check what get_all returns for aws:Value
	let aws_value_pred = model.resolve("aws:Value").expect("aws:Value should exist");
	let values = model.get_all(tag_entity, aws_value_pred);
	eprintln!("get_all(tag, aws:Value) = {:?}", values);

	for v in &values {
		match v {
			crate::intents::model::Value::Entity(e) => {
				eprintln!("  Entity: {:?}, name: {}", e, model.qualified_name(*e));
			}
			crate::intents::model::Value::Literal(s) => {
				eprintln!("  Literal: {}", s);
			}
			crate::intents::model::Value::Number(n) => {
				eprintln!("  Number: {}", n);
			}
		}
	}

	// Now test extract_literals
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

	let literals = engine
		.extract_literals(&model, &[tag_entity], &path)
		.unwrap();
	eprintln!("extract_literals result: {:?}", literals);

	assert!(!literals.is_empty(), "Should extract the value");
	assert!(
		literals[0].contains("BusinessCritical"),
		"Should contain BusinessCritical"
	);
}

#[test]
fn test_match_with_as_conversion() {
	use crate::intents::kernel::ast::*;
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();

	// Create namespaces
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("my").unwrap();

	// Create enum type and variants (mimicking what Lower does for enums)
	let enum_type = model.ensure_entity("my:DataCriticality").unwrap();
	model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

	// Create variants as entities with type = the enum
	let variant_bc = model.ensure_entity("my:BusinessCritical").unwrap();
	model
		.apply_entity(variant_bc, "wa2:type", enum_type)
		.unwrap();

	let variant_mc = model.ensure_entity("my:MissionCritical").unwrap();
	model
		.apply_entity(variant_mc, "wa2:type", enum_type)
		.unwrap();

	let variant_nc = model.ensure_entity("my:NonCritical").unwrap();
	model
		.apply_entity(variant_nc, "wa2:type", enum_type)
		.unwrap();

	// Create a tag entity
	let tag_entity = model.blank();
	model
		.apply_to(tag_entity, "aws:Key", "\"DataCriticality\"")
		.unwrap();
	model
		.apply_to(tag_entity, "aws:Value", "\"BusinessCritical\"")
		.unwrap();

	eprintln!("Model:\n{}", model);

	// Check enum variants
	let enum_id = model.resolve("my:DataCriticality").unwrap();
	eprintln!("Enum type: {:?}", enum_id);

	let mut valid_variants = Vec::new();
	for i in 0..model.entity_count() {
		let entity = crate::intents::model::EntityId(i as u32);
		if model.has_type(entity, enum_id) {
			let name = model.qualified_name(entity);
			let local = name.rsplit(':').next().unwrap_or(&name);
			valid_variants.push(local.to_string());
			eprintln!("  Variant: {} (local: {})", name, local);
		}
	}
	eprintln!("Valid variants: {:?}", valid_variants);

	// Now simulate what the derive does:
	// let is_critical = match query(dc_tag/aws:Value) as(my:DataCriticality, strict) {
	//     MissionCritical, BusinessCritical => true,
	//     else => false
	// }

	// First extract the value
	let engine = crate::intents::kernel::query::QueryEngine::new();
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

	let literals = engine
		.extract_literals(&model, &[tag_entity], &path)
		.unwrap();
	eprintln!("Extracted literals: {:?}", literals);

	// Check if value matches a variant
	let value_str = &literals[0];
	let value_str =
		if value_str.starts_with('"') && value_str.ends_with('"') && value_str.len() >= 2 {
			&value_str[1..value_str.len() - 1]
		} else {
			value_str.as_str()
		};
	eprintln!("Value after stripping quotes: {}", value_str);

	let is_valid = valid_variants.iter().any(|v| v == value_str);
	eprintln!("Is valid variant: {}", is_valid);

	assert!(is_valid, "BusinessCritical should be a valid variant");
}

#[test]
fn test_dc_tag_query_result() {
	use crate::intents::kernel::ast::*;
	use crate::intents::kernel::query::QueryEngine;
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();

	// Create namespaces
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("aws:cfn").unwrap();
	model.ensure_namespace("core").unwrap();

	// Create a cfn:Resource with tags (mimicking the projector)
	let resource = model.ensure_entity("DataBucket").unwrap();
	model
		.apply_to(resource, "wa2:type", "aws:cfn:Resource")
		.unwrap();

	// Create tags container
	let tags_container = model.blank();
	model
		.apply_entity(resource, "aws:Tags", tags_container)
		.unwrap();

	// Create tag entities as children of the container
	let tag1 = model.blank();
	model
		.apply_entity(tags_container, "wa2:contains", tag1)
		.unwrap();
	model
		.apply_to(tag1, "aws:Key", "\"DataSensitivity\"")
		.unwrap();
	model
		.apply_to(tag1, "aws:Value", "\"Confidential\"")
		.unwrap();

	let tag2 = model.blank();
	model
		.apply_entity(tags_container, "wa2:contains", tag2)
		.unwrap();
	model
		.apply_to(tag2, "aws:Key", "\"DataCriticality\"")
		.unwrap();
	model
		.apply_to(tag2, "aws:Value", "\"BusinessCritical\"")
		.unwrap();

	eprintln!("Model:\n{}", model);
	eprintln!("Tags container: {:?}", tags_container);
	eprintln!("Tag2 (DataCriticality): {:?}", tag2);

	// Now simulate the query: source/aws:Tags/*[aws:Key = "DataCriticality"]
	let engine = QueryEngine::new();

	// Step 1: resource/aws:Tags
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
	let tags_result = engine
		.execute_from(&model, &[resource], &tags_path)
		.unwrap();
	eprintln!("resource/aws:Tags = {:?}", tags_result);

	// Step 2: tags/*
	let children = model.children(tags_result[0]);
	eprintln!("tags/* (children) = {:?}", children);

	// Step 3: filter by aws:Key = "DataCriticality"
	let mut dc_tags = Vec::new();
	for child in &children {
		let key_pred = model.resolve("aws:Key").unwrap();
		let values = model.get_all(*child, key_pred);
		eprintln!("  Child {:?} aws:Key values: {:?}", child, values);
		for v in values {
			if let crate::intents::model::Value::Literal(s) = v {
				if s == "DataCriticality" || s == "\"DataCriticality\"" {
					dc_tags.push(*child);
				}
			}
		}
	}
	eprintln!("dc_tag candidates: {:?}", dc_tags);

	// Step 4: extract aws:Value from dc_tag
	if !dc_tags.is_empty() {
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
		let literals = engine
			.extract_literals(&model, &dc_tags, &value_path)
			.unwrap();
		eprintln!("dc_tag/aws:Value literals: {:?}", literals);

		assert!(!literals.is_empty(), "Should find the value");
	} else {
		panic!("dc_tag query returned no results!");
	}
}

#[test]
fn test_match_as_should_valid_value() {
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("my").unwrap();

	// Create enum type and variants
	let enum_type = model.ensure_entity("my:Criticality").unwrap();
	model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

	let variant_high = model.ensure_entity("my:High").unwrap();
	model
		.apply_entity(variant_high, "wa2:subTypeOf", enum_type)
		.unwrap();

	let variant_low = model.ensure_entity("my:Low").unwrap();
	model
		.apply_entity(variant_low, "wa2:subTypeOf", enum_type)
		.unwrap();

	// Create a tag with valid value
	let tag = model.blank();
	model.apply_to(tag, "aws:Value", "\"High\"").unwrap();

	// Build match expression: match query(tag/aws:Value) as(my:Criticality, should)
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

	// Should return true (matched High)
	assert!(matches!(result, EvalResult::Literal(s) if s == "true"));

	// Should NOT have created any failures
	let failures: Vec<_> = (0..model.entity_count())
		.filter_map(|i| {
			let e = EntityId(i as u32);
			if model.has_type(
				e,
				model.resolve("core:AssertFailure").unwrap_or(EntityId(0)),
			) {
				Some(e)
			} else {
				None
			}
		})
		.collect();
	assert!(
		failures.is_empty(),
		"Valid value should not create failures"
	);
}

#[test]
fn test_match_as_should_invalid_value() {
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("my").unwrap();
	model.ensure_namespace("core").unwrap();

	// Create enum type and variants
	let enum_type = model.ensure_entity("my:Criticality").unwrap();
	model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

	let variant_high = model.ensure_entity("my:High").unwrap();
	model
		.apply_entity(variant_high, "wa2:subTypeOf", enum_type)
		.unwrap();

	// Create a tag with INVALID value
	let tag = model.blank();
	model
		.apply_to(tag, "aws:Value", "\"InvalidValue\"")
		.unwrap();

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

	// Should return Empty (conversion failed)
	assert!(matches!(result, EvalResult::Empty));

	// Should have created a warning failure
	if let Some(failure_type) = model.resolve("core:AssertFailure") {
		let failures: Vec<_> = (0..model.entity_count())
			.filter(|i| {
				let e = EntityId(*i as u32);
				model.has_type(e, failure_type)
			})
			.collect();
		assert_eq!(failures.len(), 1, "Should create one failure");

		// Check severity is Warning
		let failure = EntityId(failures[0] as u32);
		if let Some(severity_pred) = model.resolve("core:severity") {
			let severities = model.get_all(failure, severity_pred);
			assert!(!severities.is_empty(), "Failure should have severity");
		}
	}
}

#[test]
fn test_match_as_may_invalid_value_no_warning() {
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("my").unwrap();

	// Create enum type and variants
	let enum_type = model.ensure_entity("my:Criticality").unwrap();
	model.apply_to(enum_type, "wa2:type", "wa2:Type").unwrap();

	let variant_high = model.ensure_entity("my:High").unwrap();
	model
		.apply_entity(variant_high, "wa2:subTypeOf", enum_type)
		.unwrap();

	// Create a tag with INVALID value
	let tag = model.blank();
	model
		.apply_to(tag, "aws:Value", "\"InvalidValue\"")
		.unwrap();

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
			mode: Modal::May, // May mode - should be silent
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

	// May mode should continue to match arms, hitting else => false
	assert!(matches!(result, EvalResult::Literal(s) if s == "false"));

	// Should NOT have created any failures
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
fn test_match_as_type_not_found_always_errors() {
	use crate::intents::model::Model;

	let mut model = Model::bootstrap();
	model.ensure_namespace("aws").unwrap();
	model.ensure_namespace("my").unwrap();
	model.ensure_namespace("core").unwrap();

	// Create a tag - but NO enum type
	let tag = model.blank();
	model.apply_to(tag, "aws:Value", "\"SomeValue\"").unwrap();

	// Test with Modal::Should - should still error because type doesn't exist
	let match_expr_should = MatchExpr {
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
			mode: Modal::Should,
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

	// Should error - type not found is always an error regardless of modal
	let result = engine.eval_expr(&mut model, &Expr::Match(Box::new(match_expr_should)), &env);
	assert!(
		result.is_err(),
		"Type not found with 'should' should still error"
	);
	assert!(
		result.unwrap_err().message.contains("not found"),
		"Error should mention type not found"
	);

	// Test with Modal::May - should also error
	let match_expr_may = MatchExpr {
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
			mode: Modal::May,
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

	let result = engine.eval_expr(&mut model, &Expr::Match(Box::new(match_expr_may)), &env);
	assert!(
		result.is_err(),
		"Type not found with 'may' should still error"
	);

	// Test with Modal::Must - should also error
	let match_expr_must = MatchExpr {
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
			mode: Modal::Must,
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

	let result = engine.eval_expr(&mut model, &Expr::Match(Box::new(match_expr_must)), &env);
	assert!(result.is_err(), "Type not found with 'must' should error");
}
