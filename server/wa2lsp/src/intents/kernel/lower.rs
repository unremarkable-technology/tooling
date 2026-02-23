//! Lower AST to Model statements

use crate::intents::kernel::ast::*;
use crate::intents::model::Model;

#[derive(Debug)]
pub struct LowerError {
	pub message: String,
}

pub struct Lower<'m> {
	model: &'m mut Model,
	namespace_stack: Vec<String>,
	default_namespace: String,
}

impl<'m> Lower<'m> {
	pub fn new(model: &'m mut Model, default_namespace: &str) -> Result<Self, LowerError> {
		// Ensure namespace entity exists and is typed
		model
			.ensure_namespace(default_namespace)
			.map_err(|e| LowerError {
				message: format!("failed to create namespace '{}': {}", default_namespace, e),
			})?;

		Ok(Self {
			model,
			namespace_stack: vec![default_namespace.to_string()],
			default_namespace: default_namespace.to_string(),
		})
	}

	/// Lower AST to model, returns rules for later execution
	pub fn lower(&mut self, ast: &Ast) -> Result<Vec<Rule>, LowerError> {
		let mut rules = Vec::new();

		for item in &ast.items {
			self.lower_item(item, &mut rules)?;
		}

		Ok(rules)
	}

	fn current_namespace(&self) -> Option<String> {
		if self.namespace_stack.is_empty() {
			None
		} else {
			Some(self.namespace_stack.join(":"))
		}
	}

	fn qualify(&self, name: &str) -> String {
		match self.current_namespace() {
			Some(ns) => format!("{}:{}", ns, name),
			None => name.to_string(),
		}
	}

	fn lower_item(&mut self, item: &Item, rules: &mut Vec<Rule>) -> Result<(), LowerError> {
		match item {
			Item::Namespace(ns) => {
				// Namespace names are always absolute (root-level), not qualified by context
				self.model
					.ensure_namespace(&ns.name)
					.map_err(|e| LowerError {
						message: format!("failed to create namespace '{}': {}", ns.name, e),
					})?;

				// Save current stack, replace with just this namespace
				let saved_stack =
					std::mem::replace(&mut self.namespace_stack, vec![ns.name.clone()]);

				for inner in &ns.items {
					self.lower_item(inner, rules)?;
				}

				// Restore previous stack
				self.namespace_stack = saved_stack;
			}

			Item::Struct(s) => {
				let qname = self.qualify(&s.name);
				// +(qname, wa2:type, wa2:Type)
				self.model
					.apply(&qname, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create struct type {}: {:?}", qname, e),
					})?;

				// TODO: lower fields as predicates with domain/range
			}

			Item::Enum(e) => {
				let base_qname = self.qualify(&e.name);
				// +(base, wa2:type, wa2:Type)
				self.model
					.apply(&base_qname, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create enum type {}: {:?}", base_qname, e),
					})?;

				// Each variant is a subtype
				for variant in &e.variants {
					let variant_qname = self.qualify(variant);
					self.model
						.apply(&variant_qname, "wa2:type", "wa2:Type")
						.map_err(|e| LowerError {
							message: format!("failed to create variant {}: {:?}", variant_qname, e),
						})?;
					self.model
						.apply(&variant_qname, "wa2:subTypeOf", &base_qname)
						.map_err(|e| LowerError {
							message: format!(
								"failed to set subtype for {}: {:?}",
								variant_qname, e
							),
						})?;
				}
			}

			Item::Type(t) => {
				let qname = self.qualify(&t.name);
				self.model
					.apply(&qname, "wa2:type", "wa2:Type")
					.map_err(|e| LowerError {
						message: format!("failed to create type {}: {:?}", qname, e),
					})?;
			}

			Item::Predicate(p) => {
				let qname = self.qualify(&p.name);
				self.model
					.apply(&qname, "wa2:type", "wa2:Predicate")
					.map_err(|e| LowerError {
						message: format!("failed to create predicate {}: {:?}", qname, e),
					})?;
			}

			Item::Instance(i) => {
				let name = i.name.to_string();
				let ty = i.ty.to_string();
				self.model
					.apply(&name, "wa2:type", &ty)
					.map_err(|e| LowerError {
						message: format!("failed to create instance {}: {:?}", name, e),
					})?;
			}

			Item::Rule(r) => {
				// Collect rules for later execution
				rules.push(r.clone());
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
		let rules = lower.lower(&ast).unwrap();

		// No rules in this snippet
		assert!(rules.is_empty());

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
}
