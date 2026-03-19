//! Lower AST to Model statements

use crate::intents::kernel::ast::*;
use crate::intents::model::{EntityId, Model};

#[derive(Debug)]
pub struct LowerError {
	pub message: String,
}

pub struct Lower<'m> {
	model: &'m mut Model,
	namespace_stack: Vec<String>,
	default_namespace: String,
}

/// Result of lowering AST
#[derive(Debug)]
pub struct LowerResult {
	pub rules: Vec<Rule>,
	pub derives: Vec<Derive>,
	pub policies: Vec<Policy>,
	pub profiles: Vec<Profile>,
	pub selected_profile: Option<QualifiedName>,
}

impl<'m> Lower<'m> {
	pub fn new(model: &'m mut Model, default_namespace: &str) -> Result<Self, LowerError> {
		if !default_namespace.is_empty() {
			model
				.ensure_namespace(default_namespace)
				.map_err(|e| LowerError {
					message: format!(
						"failed to create default namespace '{}': {}",
						default_namespace, e
					),
				})?;
		}
		Ok(Self {
			model,
			namespace_stack: Vec::new(),
			default_namespace: default_namespace.to_string(),
		})
	}

	fn current_namespace(&self) -> String {
		if self.namespace_stack.is_empty() {
			self.default_namespace.clone()
		} else {
			self.namespace_stack.join(":")
		}
	}

	fn qualify(&self, name: &str) -> String {
		let ns = self.current_namespace();
		if ns.is_empty() {
			name.to_string()
		} else {
			format!("{}:{}", ns, name)
		}
	}

	/// Qualify type name in AsExpr if unqualified
	fn qualify_as_expr(&self, as_expr: &mut AsExpr) {
		if as_expr.target_type.namespace.is_none() {
			as_expr.target_type.namespace = Some(self.current_namespace());
		}
	}

	/// Walk statements and qualify type names in as() expressions
	fn qualify_statements(&self, stmts: &mut [Statement]) {
		for stmt in stmts {
			self.qualify_statement(stmt);
		}
	}

	fn qualify_statement(&self, stmt: &mut Statement) {
		match stmt {
			Statement::Let(let_stmt) => {
				self.qualify_expr(&mut let_stmt.value);
			}
			Statement::For(for_stmt) => {
				self.qualify_expr(&mut for_stmt.collection);
				self.qualify_statements(&mut for_stmt.body);
			}
			Statement::If(if_stmt) => {
				self.qualify_expr(&mut if_stmt.condition);
				self.qualify_statements(&mut if_stmt.then_body);
				if let Some(ref mut else_body) = if_stmt.else_body {
					self.qualify_statements(else_body);
				}
			}
			Statement::Add(add_stmt) => {
				self.qualify_expr(&mut add_stmt.subject);
				self.qualify_expr(&mut add_stmt.object);
			}
			Statement::Assert(assert_stmt) => {
				self.qualify_expr(&mut assert_stmt.expr);
			}
			Statement::Modal(modal_stmt) => {
				self.qualify_expr(&mut modal_stmt.expr);
			}
		}
	}

	fn qualify_expr(&self, expr: &mut Expr) {
		match expr {
			Expr::Match(match_expr) => {
				self.qualify_expr(&mut match_expr.value);
				if let Some(ref mut as_type) = match_expr.as_type {
					self.qualify_as_expr(as_type);
				}
				for arm in &mut match_expr.arms {
					self.qualify_expr(&mut arm.result);
				}
			}
			Expr::As(as_validation) => {
				self.qualify_expr(&mut as_validation.inner);
				if as_validation.target_type.namespace.is_none() {
					as_validation.target_type.namespace = Some(self.current_namespace());
				}
			}
			Expr::Add(add_expr) => {
				self.qualify_expr(&mut add_expr.subject);
				self.qualify_expr(&mut add_expr.object);
			}
			Expr::Empty(inner, _) => {
				self.qualify_expr(inner);
			}
			// These don't contain type references that need qualifying
			Expr::Query(_) => {}
			Expr::Var(_, _) => {}
			Expr::Blank(_) => {}
			Expr::QName(_) => {}
			Expr::String(_, _) => {}
			Expr::Bool(_, _) => {}
		}
	}

	/// Lower AST to model, returns rules for later execution
	pub fn lower(&mut self, ast: &Ast) -> Result<LowerResult, LowerError> {
		let mut rules = Vec::new();
		let mut derives = Vec::new();
		let mut policies = Vec::new();
		let mut profiles = Vec::new();
		let mut selected_profile = None;

		for item in &ast.items {
			self.lower_item(
				item,
				&mut rules,
				&mut derives,
				&mut policies,
				&mut profiles,
				&mut selected_profile,
			)?;
		}

		Ok(LowerResult {
			rules,
			derives,
			policies,
			profiles,
			selected_profile,
		})
	}

	fn lower_item(
		&mut self,
		item: &Item,
		rules: &mut Vec<Rule>,
		derives: &mut Vec<Derive>,
		policies: &mut Vec<Policy>,
		profiles: &mut Vec<Profile>,
		selected_profile: &mut Option<QualifiedName>,
	) -> Result<(), LowerError> {
		match item {
			Item::Namespace(ns) => {
				self.namespace_stack.push(ns.name.clone());

				let fqn = self.namespace_stack.join(":");
				self.model.ensure_namespace(&fqn).map_err(|e| LowerError {
					message: format!("failed to create namespace '{}': {}", fqn, e),
				})?;

				for inner in &ns.items {
					self.lower_item(inner, rules, derives, policies, profiles, selected_profile)?;
				}

				self.namespace_stack.pop();
			}

			Item::Use(_) => {
				// Use statements are handled by the multi-file loader
			}

			Item::Type(type_decl) => {
				let fqn = self.qualify(&type_decl.name);
				self.model
					.apply(&fqn, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create type '{}': {}", fqn, e),
					})?;

				let entity_id = self.model.resolve(&fqn).ok_or_else(|| LowerError {
					message: format!("failed to resolve type '{}' after creation", fqn),
				})?;

				self.lower_doc_annotations(entity_id, &type_decl.annotations)?;
			}

			Item::Struct(struct_decl) => {
				let fqn = self.qualify(&struct_decl.name);
				self.model
					.apply(&fqn, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create struct '{}': {}", fqn, e),
					})?;

				let entity_id = self.model.resolve(&fqn).ok_or_else(|| LowerError {
					message: format!("failed to resolve struct '{}' after creation", fqn),
				})?;

				self.lower_doc_annotations(entity_id, &struct_decl.annotations)?;

				for field in &struct_decl.fields {
					let field_fqn = self.qualify(&field.name);
					self.model
						.apply(&field_fqn, "wa2:type", "wa2:Predicate")
						.map_err(|e| LowerError {
							message: format!("failed to create field '{}': {}", field_fqn, e),
						})?;
				}
			}

			Item::Enum(enum_decl) => {
				let fqn = self.qualify(&enum_decl.name);
				self.model
					.apply(&fqn, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create enum '{}': {}", fqn, e),
					})?;

				let entity_id = self.model.resolve(&fqn).ok_or_else(|| LowerError {
					message: format!("failed to resolve enum '{}' after creation", fqn),
				})?;

				self.lower_doc_annotations(entity_id, &enum_decl.annotations)?;

				for variant in &enum_decl.variants {
					let variant_fqn = self.qualify(variant);
					self.model
						.apply(&variant_fqn, "wa2:type", "wa2:Type")
						.map_err(|e| LowerError {
							message: format!("failed to create variant '{}': {}", variant_fqn, e),
						})?;
					self.model
						.apply(&variant_fqn, "wa2:subTypeOf", &fqn)
						.map_err(|e| LowerError {
							message: format!("failed to set subtype for '{}': {}", variant_fqn, e),
						})?;
				}
			}

			Item::Predicate(pred_decl) => {
				let fqn = self.qualify(&pred_decl.name);
				self.model
					.apply(&fqn, "wa2:type", "wa2:Predicate")
					.map_err(|e| LowerError {
						message: format!("failed to create predicate '{}': {}", fqn, e),
					})?;
			}

			Item::Instance(inst) => {
				let entity_name = &inst.name.to_string();
				let type_name = &inst.ty.to_string();
				self.model
					.apply(entity_name, "wa2:type", type_name)
					.map_err(|e| LowerError {
						message: format!("failed to create instance '{}': {}", entity_name, e),
					})?;
			}

			Item::Rule(rule) => {
				let mut qualified_rule = rule.clone();
				let qualified_name = self.qualify(&rule.name);
				qualified_rule.name = qualified_name;
				self.qualify_statements(&mut qualified_rule.body);
				rules.push(qualified_rule);
			}

			Item::Derive(derive) => {
				// Validate: no must allowed in derive blocks
				Self::validate_derive_body(&derive.body, &derive.name)?;

				let mut qualified_derive = derive.clone();
				let qualified_name = self.qualify(&derive.name);
				qualified_derive.name = qualified_name;
				self.qualify_statements(&mut qualified_derive.body);
				derives.push(qualified_derive);
			}

			Item::Policy(policy) => {
				let qualified_name = self.qualify(&policy.name);

				// Qualify rule names in bindings that don't have a namespace
				let qualified_bindings: Vec<PolicyBinding> = policy
					.bindings
					.iter()
					.map(|b| {
						let qualified_rule_name = if b.rule_name.namespace.is_some() {
							b.rule_name.clone()
						} else {
							QualifiedName {
								namespace: Some(self.current_namespace()),
								name: b.rule_name.name.clone(),
								span: b.rule_name.span.clone(),
							}
						};
						PolicyBinding {
							modal: b.modal,
							rule_name: qualified_rule_name,
							span: b.span.clone(),
						}
					})
					.collect();

				policies.push(Policy {
					name: qualified_name,
					bindings: qualified_bindings,
					span: policy.span.clone(),
				});
			}

			Item::Profile(profile) => {
				// Qualify profile name if not already qualified
				let qualified_profile_name = if profile.name.namespace.is_some() {
					profile.name.clone()
				} else {
					QualifiedName {
						namespace: Some(self.current_namespace()),
						name: profile.name.name.clone(),
						span: profile.name.span.clone(),
					}
				};

				// Qualify policy names that don't have a namespace
				let qualified_policies: Vec<QualifiedName> = profile
					.policies
					.iter()
					.map(|p| {
						if p.namespace.is_some() {
							p.clone()
						} else {
							QualifiedName {
								namespace: Some(self.current_namespace()),
								name: p.name.clone(),
								span: p.span.clone(),
							}
						}
					})
					.collect();

				profiles.push(Profile {
					name: qualified_profile_name,
					policies: qualified_policies,
					span: profile.span.clone(),
				});
			}

			Item::ProfileSelection(selection) => {
				// Qualify profile selection name if not already qualified
				let qualified_name = if selection.name.namespace.is_some() {
					selection.name.clone()
				} else {
					QualifiedName {
						namespace: Some(self.current_namespace()),
						name: selection.name.name.clone(),
						span: selection.name.span.clone(),
					}
				};
				*selected_profile = Some(qualified_name);
			}
		}

		Ok(())
	}

	/// Validate that derive body contains no must statements (only should/may)
	fn validate_derive_body(stmts: &[Statement], derive_name: &str) -> Result<(), LowerError> {
		for stmt in stmts {
			match stmt {
				Statement::Modal(must_stmt) => {
					if must_stmt.modal == Modal::Must {
						return Err(LowerError {
							message: format!(
								"derive '{}' contains 'must' statement at {:?}; use 'should' or 'may' instead",
								derive_name, must_stmt.span
							),
						});
					}
				}
				Statement::For(for_stmt) => {
					Self::validate_derive_body(&for_stmt.body, derive_name)?;
				}
				Statement::If(if_stmt) => {
					Self::validate_derive_body(&if_stmt.then_body, derive_name)?;
					if let Some(ref else_body) = if_stmt.else_body {
						Self::validate_derive_body(else_body, derive_name)?;
					}
				}
				_ => {}
			}
		}
		Ok(())
	}

	/// Lower @#doc annotations to wa2:tldr, wa2:why, wa2:summary predicates
	fn lower_doc_annotations(
		&mut self,
		entity_id: EntityId,
		annotations: &[Annotation],
	) -> Result<(), LowerError> {
		for annotation in annotations {
			// Check if this is a @#doc annotation
			let is_doc = annotation.path.as_ref().map_or(false, |p| p.name == "doc");
			if !is_doc {
				continue;
			}

			for arg in &annotation.args {
				let predicate = match arg.name.as_str() {
					"tldr" => "wa2:tldr",
					"why" => "wa2:why",
					"summary" => "wa2:summary",
					_ => continue, // ignore unknown doc fields
				};

				if let Literal::String(value) = &arg.value {
					self.model
						.apply_literal(entity_id, predicate, value)
						.map_err(|e| LowerError {
							message: format!("failed to set {} annotation: {}", predicate, e),
						})?;
				}
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::intents::kernel::lexer::Wa2Source;
	use crate::intents::kernel::parser::parse;
	use crate::intents::model::Value;

	#[test]
	fn lower_struct() {
		let src = r#"
        struct Workload {}
    "#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		let mut lower = Lower::new(&mut model, "core").unwrap();
		lower.lower(&ast).unwrap();

		assert!(model.resolve("core:Workload").is_some());
		let workload = model.resolve("core:Workload").unwrap();
		let wa2_type = model.resolve("wa2:Type").unwrap();
		assert!(model.has_type(workload, wa2_type));
	}

	#[test]
	fn lower_enum() {
		let src = r#"
        enum Node { Store, Run, Move }
    "#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		let mut lower = Lower::new(&mut model, "core").unwrap();
		lower.lower(&ast).unwrap();

		assert!(model.resolve("core:Node").is_some());
		assert!(model.resolve("core:Store").is_some());
		assert!(model.resolve("core:Run").is_some());
		assert!(model.resolve("core:Move").is_some());

		let sub_type_of = model.resolve("wa2:subTypeOf").unwrap();
		let store = model.resolve("core:Store").unwrap();
		let node = model.resolve("core:Node").unwrap();
		assert!(model.has(store, sub_type_of, &Value::Entity(node)));
	}

	#[test]
	fn lower_instance() {
		let src = r#"
        struct Workload {}
        instance core:workload: core:Workload
    "#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		let mut lower = Lower::new(&mut model, "core").unwrap();
		lower.lower(&ast).unwrap();

		let workload_instance = model.resolve("core:workload").unwrap();
		let workload_type = model.resolve("core:Workload").unwrap();
		assert!(model.has_type(workload_instance, workload_type));
	}

	#[test]
	fn lower_bootstrap_subset() {
		let src = r#"
        enum Node { Store, Run, Move }
        
        struct Workload {
            nodes: Node[]
        }
        
        struct Evidence {
            value: String
        }
        
        predicate source
        predicate value
        
        instance core:workload: core:Workload
    "#;

		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();

		eprintln!("{}", &model);

		// Verify these DON'T exist before lowering
		assert!(model.resolve("core:Node").is_none());
		assert!(model.resolve("core:Store").is_none());
		assert!(model.resolve("core:workload").is_none());

		let mut lower = Lower::new(&mut model, "core").unwrap();
		let result = lower.lower(&ast).unwrap();

		// No rules in this snippet
		assert!(result.rules.is_empty());

		eprintln!("{}", &model);
		//panic!();

		// Check namespace exists
		let core_ns = model.resolve("core").expect("core namespace should exist");
		let wa2_namespace = model.resolve("wa2:Namespace").unwrap();
		assert!(model.has_type(core_ns, wa2_namespace));

		// Check enum base type
		let node = model.resolve("core:Node").expect("core:Node should exist");
		let wa2_type = model.resolve("wa2:Type").unwrap();
		assert!(model.has_type(node, wa2_type));

		// Check enum variants
		let store = model
			.resolve("core:Store")
			.expect("core:Store should exist");
		let run = model.resolve("core:Run").expect("core:Run should exist");
		let mov = model.resolve("core:Move").expect("core:Move should exist");

		assert!(model.has_type(store, wa2_type));
		assert!(model.has_type(run, wa2_type));
		assert!(model.has_type(mov, wa2_type));

		// Check subtype relationships
		let sub_type_of = model.resolve("wa2:subTypeOf").unwrap();
		assert!(model.has(store, sub_type_of, &Value::Entity(node)));
		assert!(model.has(run, sub_type_of, &Value::Entity(node)));
		assert!(model.has(mov, sub_type_of, &Value::Entity(node)));

		// Check structs
		let workload = model
			.resolve("core:Workload")
			.expect("core:Workload should exist");
		let evidence = model
			.resolve("core:Evidence")
			.expect("core:Evidence should exist");
		assert!(model.has_type(workload, wa2_type));
		assert!(model.has_type(evidence, wa2_type));

		// Check predicates
		let source_pred = model
			.resolve("core:source")
			.expect("core:source should exist");
		let value_pred = model
			.resolve("core:value")
			.expect("core:value should exist");
		let wa2_predicate = model.resolve("wa2:Predicate").unwrap();
		assert!(model.has_type(source_pred, wa2_predicate));
		assert!(model.has_type(value_pred, wa2_predicate));

		// Check instance
		let workload_instance = model
			.resolve("core:workload")
			.expect("core:workload should exist");
		assert!(model.has_type(workload_instance, workload));
	}

	#[test]
	fn reject_must_in_derive() {
		let src = r#"
derive bad {
	must query(core:Store)
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		model.ensure_namespace("test").unwrap();
		let mut lower = Lower::new(&mut model, "test").unwrap();

		let result = lower.lower(&ast);
		assert!(result.is_err());
		assert!(result.unwrap_err().message.contains("must"));
	}

	#[test]
	fn allow_should_in_derive() {
		let src = r#"
derive good {
	should query(core:Store)
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		model.ensure_namespace("core").unwrap();
		model.ensure_namespace("test").unwrap();
		let mut lower = Lower::new(&mut model, "test").unwrap();

		let result = lower.lower(&ast);
		assert!(result.is_ok());
		assert_eq!(result.unwrap().derives.len(), 1);
	}

	#[test]
	fn qualify_as_type_in_derive_inside_namespace() {
		let src = r#"
namespace my {
    enum DataCriticality {
        Low,
        High
    }
    
    derive test_derive {
        let x = query(tag/value) as(DataCriticality)
    }
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		let mut model = Model::bootstrap();
		let mut lower = Lower::new(&mut model, "").unwrap();
		let result = lower.lower(&ast).unwrap();

		// The derive's as(DataCriticality) should be qualified to as(my:DataCriticality)
		assert_eq!(result.derives.len(), 1);
		let derive = &result.derives[0];

		// Find the let statement with as() expression
		match &derive.body[0] {
			Statement::Let(let_stmt) => match &let_stmt.value {
				Expr::As(as_validation) => {
					assert_eq!(
						as_validation.target_type.namespace,
						Some("my".to_string()),
						"as(DataCriticality) should be qualified to my:DataCriticality"
					);
					assert_eq!(as_validation.target_type.name, "DataCriticality");
				}
				_ => panic!("expected As expression, got {:?}", let_stmt.value),
			},
			_ => panic!("expected Let statement"),
		}
	}
}
