//! Kernel - loads core types and orchestrates projection + guidance

mod ast;
mod lexer;
mod lower;
mod parser;
mod query;
mod rules;

use std::path::Path;

use tower_lsp::lsp_types::Diagnostic;
use url::Url;

use crate::intents::model::Model;
use crate::intents::vendor::{DocumentFormat, Method, Vendor, get_projector};

use lexer::Wa2Source;
use lower::Lower;
use rules::RuleEngine;

/// Result of analyzing a document
pub struct AnalysisResult {
	pub model: Model,
	pub failures: Vec<AssertFailure>,
}

/// An assertion failure from rule execution
#[derive(Debug)]
pub struct AssertFailure {
	pub entity: crate::intents::model::EntityId,
	pub assertion: String,
}

/// Kernel - the WA2 analysis engine
pub struct Kernel {
	bootstrap_source: String,
}

impl Default for Kernel {
	fn default() -> Self {
		Self::new()
	}
}

impl Kernel {
	pub fn new() -> Self {
		// Load bootstrap.wa2 from embedded or file
		let bootstrap_source =
			include_str!("../../../../../wa2/core/v0.1/bootstrap.wa2").to_string();
		Self { bootstrap_source }
	}

	pub fn from_bootstrap_path(path: impl AsRef<Path>) -> std::io::Result<Self> {
		let bootstrap_source = std::fs::read_to_string(path)?;
		Ok(Self { bootstrap_source })
	}

	/// Analyze a document, returning model and failures
	pub fn analyse(
		&self,
		text: &str,
		uri: &Url,
		format: DocumentFormat,
		vendor: Vendor,
		method: Method,
	) -> Result<AnalysisResult, Vec<Diagnostic>> {
		// 1. Bootstrap model with minimal Rust primitives
		let mut model = Model::bootstrap();

		// 2. Parse bootstrap.wa2
		let source = Wa2Source::from_str(&self.bootstrap_source);
		let ast =
			parser::parse(source.lexer()).map_err(|e| vec![self.parse_error_to_diagnostic(&e)])?;

		// 3. Lower AST to model (types, predicates, instances)
		let mut lowerer =
			Lower::new(&mut model, "core").map_err(|e| vec![self.lower_error_to_diagnostic(&e)])?;
		let rules = lowerer
			.lower(&ast)
			.map_err(|e| vec![self.lower_error_to_diagnostic(&e)])?;

		// 4. Project vendor IaC into the same model
		let projector = get_projector(vendor, method);
		projector.project_into(&mut model, text, uri, format)?;

		// 5. Run rules to fixed-point
		let mut engine = RuleEngine::new();
		engine
			.run(&mut model, &rules)
			.map_err(|e| vec![self.rule_error_to_diagnostic(&e)])?;

		// 6. Query for assertion failures
		let failures = self.collect_failures(&model);

		Ok(AnalysisResult { model, failures })
	}

	fn collect_failures(&self, model: &Model) -> Vec<AssertFailure> {
		let mut failures = Vec::new();

		if let Some(failure_type) = model.resolve("core:AssertFailure") {
			for i in 0..model.entity_count() {
				let entity = crate::intents::model::EntityId(i as u32);
				if model.has_type(entity, failure_type) {
					let assertion = model
						.get_literal(entity, "core:assertion")
						.unwrap_or_default();
					failures.push(AssertFailure { entity, assertion });
				}
			}
		}

		failures
	}

	fn parse_error_to_diagnostic(&self, err: &parser::ParseError) -> Diagnostic {
		Diagnostic {
			range: tower_lsp::lsp_types::Range::default(),
			severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
			message: format!("Parse error: {}", err.message),
			..Default::default()
		}
	}

	fn lower_error_to_diagnostic(&self, err: &lower::LowerError) -> Diagnostic {
		Diagnostic {
			range: tower_lsp::lsp_types::Range::default(),
			severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
			message: format!("Lower error: {}", err.message),
			..Default::default()
		}
	}

	fn rule_error_to_diagnostic(&self, err: &rules::RuleError) -> Diagnostic {
		Diagnostic {
			range: tower_lsp::lsp_types::Range::default(),
			severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
			message: format!("Rule error: {}", err.message),
			..Default::default()
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::intents::kernel::lexer::Wa2Source;
	use crate::intents::kernel::lower::Lower;
	use crate::intents::kernel::parser::parse;
	use crate::intents::model::Model;

	#[test]
	fn test_derive_stores_rule() {
		// 1. Bootstrap and load DSL
		let bootstrap_src = include_str!("../../../../../wa2/core/v0.1/bootstrap.wa2");
		let source = Wa2Source::from_str(bootstrap_src);
		let ast = parse(source.lexer()).expect("parse bootstrap.wa2");

		let mut model = Model::bootstrap();
		let mut lower = Lower::new(&mut model, "core").expect("create lowerer");
		let rules = lower.lower(&ast).expect("lower bootstrap.wa2");

		eprintln!("Loaded {} rules", rules.len());
		for rule in &rules {
			eprintln!("  - {}", rule.name);
		}

		// 2. Manually create CFN structure (simulating projector without derive phase)
		model.ensure_namespace("cfn").unwrap();
		model.ensure_namespace("aws").unwrap();

		model.apply("cfn:Resource", "wa2:type", "wa2:Type").unwrap();
		model.apply("cfn:Template", "wa2:type", "wa2:Type").unwrap();

		let workload = model.ensure_entity("core:workload").unwrap();
		model
			.apply_to(workload, "wa2:type", "core:Workload")
			.unwrap();
		model.set_root(workload);

		let template = model.blank();
		model
			.apply_to(template, "wa2:type", "cfn:Template")
			.unwrap();
		model
			.apply_entity(workload, "core:source", template)
			.unwrap();

		let resources = model.blank();
		model
			.apply_entity(template, "cfn:resources", resources)
			.unwrap();

		let bucket = model.ensure_raw("MyBucket");
		model.apply_to(bucket, "wa2:type", "cfn:Resource").unwrap();
		model
			.apply_to(bucket, "aws:type", "\"AWS::S3::Bucket\"")
			.unwrap();
		model
			.apply_entity(resources, "wa2:contains", bucket)
			.unwrap();

		eprintln!("Before rules:\n{}", model);

		// 3. Run rules
		let mut engine = RuleEngine::new();
		engine.run(&mut model, &rules).expect("run rules");

		eprintln!("After rules:\n{}", model);

		// 4. Verify core:Store was created
		let store_type = model
			.resolve("core:Store")
			.expect("core:Store should exist");

		// Find entities with type core:Store
		let stores: Vec<_> = (0..model.entity_count())
			.map(|i| crate::intents::model::EntityId(i as u32))
			.filter(|&id| model.has_type(id, store_type))
			.collect();

		eprintln!("Found {} Store nodes", stores.len());
		assert_eq!(stores.len(), 1, "Should have created one Store node");

		// Verify it's attached to workload
		let children = model.children(workload);
		let store_in_children = children.iter().any(|&c| model.has_type(c, store_type));
		assert!(store_in_children, "Store should be child of workload");
      panic!();
	}
}
