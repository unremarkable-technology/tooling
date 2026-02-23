//! Rule execution engine with fixed-point iteration

use std::collections::{HashMap, HashSet};

use crate::intents::kernel::ast::*;
use crate::intents::kernel::query::QueryEngine;
use crate::intents::model::{EntityId, Model};

#[derive(Debug)]
pub struct RuleError {
	pub message: String,
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

	/// Run rules to fixed-point
	pub fn run(&mut self, model: &mut Model, rules: &[Rule]) -> Result<(), RuleError> {
		// Clear processed set for fresh run
		self.processed.clear();

		for _ in 0..self.max_iterations {
			let initial_count = model.statement_count();

			for rule in rules {
				self.execute_rule(model, rule)?;
			}

			let final_count = model.statement_count();
			if final_count == initial_count {
				return Ok(());
			}
		}

		Err(RuleError {
			message: format!(
				"rules did not converge after {} iterations",
				self.max_iterations
			),
		})
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
			self.execute_statement(model, stmt, env, rule_name, reference_binding)?;
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
	) -> Result<(), RuleError> {
		match stmt {
			Statement::Let(let_stmt) => {
				let value = match &let_stmt.value {
					Expr::Add(_) => {
						let entity = self.eval_expr_to_entity(model, &let_stmt.value, env)?;
						EvalResult::Entity(entity)
					}
					_ => self.eval_expr(model, &let_stmt.value, env)?,
				};
				env.bind(let_stmt.name.clone(), value);
			}

			Statement::Add(add_stmt) => {
				let subject = self.eval_expr_to_entity(model, &add_stmt.subject, env)?;
				let predicate = &add_stmt.predicate.to_string();
				let object = self.eval_expr(model, &add_stmt.object, env)?;

				match object {
					EvalResult::Entity(obj_id) => {
						model
							.apply_entity(subject, predicate, obj_id)
							.map_err(|e| RuleError {
								message: format!("add failed: {:?}", e),
							})?;
					}
					EvalResult::Literal(s) => {
						model
							.apply_to(subject, predicate, &format!("\"{}\"", s))
							.map_err(|e| RuleError {
								message: format!("add failed: {:?}", e),
							})?;
					}
					EvalResult::Set(_) => {
						return Err(RuleError {
							message: "cannot use set as object in add statement".to_string(),
						});
					}
					EvalResult::Empty => {}
				}
			}

			Statement::Iterate(iter_stmt) => {
				let collection = self.eval_expr(model, &iter_stmt.collection, env)?;
				let entities = match collection {
					EvalResult::Set(ids) => ids,
					EvalResult::Entity(id) => vec![id],
					_ => Vec::new(),
				};

				for entity in entities {
					// Build new reference binding
					let mut new_binding = reference_binding.clone();
					new_binding.push((iter_stmt.var.clone(), entity));

					// Check if already processed
					let key = (rule_name.to_string(), new_binding.clone());
					if self.processed.contains(&key) {
						continue;
					}
					self.processed.insert(key);

					// Execute body
					let mut inner_env = env.clone();
					inner_env.bind(iter_stmt.var.clone(), EvalResult::Entity(entity));
					self.execute_statements(
						model,
						&iter_stmt.body,
						&mut inner_env,
						rule_name,
						&new_binding,
					)?;
				}
			}

			Statement::Assert(assert_stmt) => {
				let value = self.eval_expr(model, &assert_stmt.expr, env)?;
				if value.is_empty() {
					let failure = model.blank();
					model
						.apply_to(failure, "wa2:type", "core:AssertFailure")
						.map_err(|e| RuleError {
							message: format!("assert failure creation failed: {:?}", e),
						})?;
					if let Expr::Var(name, _) = &assert_stmt.expr {
						model
							.apply_to(failure, "core:assertion", &format!("\"{}\"", name))
							.ok();
					}
				}
			}
		}

		Ok(())
	}

	fn eval_expr(&self, model: &Model, expr: &Expr, env: &Env) -> Result<EvalResult, RuleError> {
		match expr {
			Expr::Var(name, _) => env.get(name),

			Expr::Blank(_) => {
				// Create a new blank node
				// Note: this requires mutable model, we'll handle this specially
				Err(RuleError {
					message: "blank node in eval context - should be handled in add".to_string(),
				})
			}

			Expr::Query(query) => {
				let engine = QueryEngine::new();
				let results = engine.execute(model, &query.path)?;
				Ok(EvalResult::Set(results))
			}

			Expr::Add(_) => {
				// Add expressions need mutable model, handled specially
				Err(RuleError {
					message: "add expression in non-statement context".to_string(),
				})
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

			Expr::Empty(inner, _) => {
				let value = self.eval_expr(model, inner, env)?;
				Ok(if value.is_empty() {
					EvalResult::Literal("true".to_string())
				} else {
					EvalResult::Empty
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
			Expr::Blank(_) => Ok(model.blank()),
			Expr::Var(name, _) => match env.get(name)? {
				EvalResult::Entity(id) => Ok(id),
				_ => Err(RuleError {
					message: format!("expected entity for {}", name),
				}),
			},
			Expr::QName(qname) => {
				let name = qname.to_string();
				model.ensure_entity(&name).map_err(|e| RuleError {
					message: format!("failed to resolve '{}': {}", name, e),
				})
			}
			Expr::Add(add_expr) => {
				// Execute the add and return the subject
				let subject = self.eval_expr_to_entity(model, &add_expr.subject, env)?;
				let predicate = &add_expr.predicate.to_string();
				let object = self.eval_expr(model, &add_expr.object, env)?;

				match object {
					EvalResult::Entity(obj_id) => {
						model
							.apply_entity(subject, predicate, obj_id)
							.map_err(|e| RuleError {
								message: format!("add failed: {:?}", e),
							})?;
					}
					EvalResult::Literal(s) => {
						model
							.apply_to(subject, predicate, &format!("\"{}\"", s))
							.map_err(|e| RuleError {
								message: format!("add failed: {:?}", e),
							})?;
					}
					EvalResult::Set(_) => {
						return Err(RuleError {
							message: "cannot use set as object in add".to_string(),
						});
					}
					EvalResult::Empty => {}
				}

				Ok(subject)
			}
			_ => Err(RuleError {
				message: "expected entity expression".to_string(),
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

	pub fn get(&self, name: &str) -> Result<EvalResult, RuleError> {
		self.bindings.get(name).cloned().ok_or_else(|| RuleError {
			message: format!("unbound variable: {}", name),
		})
	}
}
