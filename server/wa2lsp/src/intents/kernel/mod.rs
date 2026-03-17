//! Kernel - loads core types and orchestrates projection + guidance

mod ast;
mod lexer;
mod loader;
mod lower;
mod parser;
mod query;
mod resolver;
mod rules;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tower_lsp::lsp_types::Diagnostic;
use url::Url;

use crate::intents::kernel::ast::{Derive, Modal, Policy, QualifiedName, Rule};
use crate::intents::kernel::loader::Loader;
use crate::intents::model::{Model, NAMESPACE_TYPE};
use crate::intents::vendor::{DocumentFormat, Method, Vendor, get_projector};
use crate::wa2_config::Wa2Config;

use lexer::Wa2Source;
use lower::Lower;
use parser::Resolver;
use rules::RuleEngine;

pub use loader::{LoadError, LoadedFile};
pub use resolver::FileResolver;

macro_rules! include_wa2 {
	($file:literal) => {
		include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wa2/", $file))
	};
}

const EMBEDDED_BOOTSTRAP: &str = include_wa2!("bootstrap.wa2");
const EMBEDDED_CORE: &str = include_wa2!("core/core.wa2");
const EMBEDDED_AWS: &str = include_wa2!("aws/aws.wa2");
const EMBEDDED_AWS_CFN: &str = include_wa2!("aws/cfn/cfn.wa2");
const EMBEDDED_DATA: &str = include_wa2!("data/data.wa2");
const EMBEDDED_QUICKSTART: &str = include_wa2!("examples/quickstart.wa2");

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
	pub severity: AssertSeverity,
	pub subject: Option<crate::intents::model::EntityId>,
	pub area: Option<crate::intents::model::EntityId>,
	pub message: Option<String>,
}

/// Shared severity for WA2 findings.
/// This is the canonical severity used by kernel, CLI, and adapters like LSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertSeverity {
	Error,
	Warning,
	Info,
}

impl AssertSeverity {
	/// Parse from model qualified-name values like `core:Error`.
	pub fn from_model_qname(name: &str) -> Option<Self> {
		match name {
			"core:Error" => Some(Self::Error),
			"core:Warning" => Some(Self::Warning),
			"core:Info" => Some(Self::Info),
			_ => None,
		}
	}

	pub fn as_model_qname(self) -> &'static str {
		match self {
			Self::Error => "core:Error",
			Self::Warning => "core:Warning",
			Self::Info => "core:Info",
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Error => "error",
			Self::Warning => "warning",
			Self::Info => "info",
		}
	}
}

pub struct FailureLocation {
	pub uri: Url,
	pub range: tower_lsp::lsp_types::Range,
}

impl AnalysisResult {
	pub fn resolve_failure_location(&self, failure: &AssertFailure) -> Option<FailureLocation> {
		let range = failure
			.subject
			.and_then(|s| self.model.get_range(s))
			.or_else(|| {
				failure.subject.and_then(|s| {
					let core_source = self.model.resolve("core:source")?;
					self.model
						.get_all(s, core_source)
						.first()
						.and_then(|v| v.as_entity())
						.and_then(|e| self.model.get_range(e))
				})
			})
			.or_else(|| self.model.get_range(failure.entity))?;

		let uri = failure
			.subject
			.and_then(|s| self.model.get_uri(s))
			.or_else(|| {
				failure.subject.and_then(|s| {
					let core_source = self.model.resolve("core:source")?;
					self.model
						.get_all(s, core_source)
						.first()
						.and_then(|v| v.as_entity())
						.and_then(|e| self.model.get_uri(e))
				})
			})
			.or_else(|| self.model.get_uri(failure.entity))?;

		Some(FailureLocation { uri: uri.clone(), range })
	}
}

/// Kernel - the WA2 analysis engine
pub struct Kernel {
	source_path: Option<std::path::PathBuf>,
	model: Model,
	rules: Vec<Rule>,
	derives: Vec<Derive>,
	policies: Vec<Policy>,
	profiles: HashMap<String, Vec<QualifiedName>>,
	selected_profile: Option<String>,
}

impl Default for Kernel {
	fn default() -> Self {
		let skip_quickstart = false;
		Self::new(skip_quickstart)
	}
}

/// Create a resolver from a model that checks if a name is a namespace
fn model_resolver(model: &Model) -> Resolver<'_> {
	Box::new(move |name: &str| {
		if let Some(id) = model.resolve(name) {
			model.has_type(id, NAMESPACE_TYPE)
		} else {
			false
		}
	})
}

impl Kernel {
	pub fn new(skip_quickstart: bool) -> Self {
		//eprintln!("WA2: Kernel starting");
		// Check current directory for wa2.toml
		if let Ok(cwd) = std::env::current_dir() {
			//eprintln!("WA2: Kernel checking for config in {:?}", cwd);
			if let Some(config) = Wa2Config::load(&cwd) {
				//eprintln!("WA2: Kernel found config");

				// Determine framework root
				let framework_root = config
					.framework_path(&cwd)
					.and_then(|p| p.canonicalize().ok());

				// Case 1: entry specified - load user's file
				if let Some(entry_path) = config.entry_path(&cwd) {
					if entry_path.exists() {
						//eprintln!("WA2: Kernel loading from {:?}", entry_path);
						match Self::from_file_with_framework(&entry_path, framework_root.as_deref())
						{
							Ok(kernel) => return kernel,
							Err(e) => {
								eprintln!("WA2: Failed to load {:?}: {}", entry_path, e);
								eprintln!("WA2: Falling back to embedded bootstrap");
							}
						}
					} else {
						eprintln!(
							"WA2: Entry file {:?} not found, using embedded bootstrap",
							entry_path
						);
					}
				} else {
					// Case 2: no entry - use framework with profile from config (or default)
					let profile = config.profile().unwrap_or("default").to_string();
					//eprintln!("WA2: No entry specified, using profile '{}'", profile);
					return Self::bootstrap_with_profile(
						framework_root.as_deref(),
						Some(profile),
						skip_quickstart,
					);
				}
			}
		}
		// Fall back to embedded bootstrap
		Self::bootstrap_embedded(skip_quickstart)
	}

	/// Log the selected profile
	fn log_selected_profile(&self) {
		// match &self.selected_profile {
		// 	Some(profile) => eprintln!("WA2: Selected profile '{}'", profile),
		// 	None => eprintln!("WA2: No profile selected"),
		// }
	}

	/// Override the selected profile
	pub fn set_profile(&mut self, profile: String) -> Result<(), String> {
		// Exact match first
		if self.profiles.contains_key(&profile) {
			self.selected_profile = Some(profile);
			self.validate()?;
			self.log_selected_profile();
			return Ok(());
		}

		// Suffix match: find profiles ending with :$profile
		let suffix = format!(":{}", profile);
		let matches: Vec<_> = self
			.profiles
			.keys()
			.filter(|k| k.ends_with(&suffix))
			.collect();

		match matches.len() {
			0 => Err(format!(
				"profile '{}' not found; available profiles: {:?}",
				profile,
				self.profiles.keys().collect::<Vec<_>>()
			)),
			1 => {
				self.selected_profile = Some(matches[0].clone());
				self.validate()?;
				self.log_selected_profile();
				Ok(())
			}
			_ => Err(format!(
				"profile '{}' is ambiguous; matches: {:?}",
				profile, matches
			)),
		}
	}

	/// Load kernel from a wa2 file (simple case, no framework)
	pub fn from_file(path: &Path) -> Result<Self, String> {
		Self::from_file_with_framework(path, None)
	}

	/// Load kernel from a wa2 file with optional framework path
	pub fn from_file_with_framework(
		entry_path: &Path,
		framework_root: Option<&Path>,
	) -> Result<Self, String> {
		// Bootstrap model with minimal Rust primitives
		let mut model = Model::bootstrap();
		let mut all_rules = Vec::new();
		let mut all_derives = Vec::new();
		let mut all_policies = Vec::new();
		let mut all_profiles: HashMap<String, Vec<QualifiedName>> = HashMap::new();
		let mut selected_profile = None;

		// Load minimal bootstrap first (just _internal namespace)
		Self::load_source_into(
			&mut model,
			EMBEDDED_BOOTSTRAP,
			"_internal",
			&mut all_rules,
			&mut all_derives,
			&mut all_policies,
			&mut all_profiles,
			&mut selected_profile,
		)?;

		// Determine loader root (framework root or entry's parent)
		let loader_root = framework_root
			.map(|p| p.to_path_buf())
			.unwrap_or_else(|| entry_path.parent().unwrap_or(Path::new(".")).to_path_buf());

		// Load entry and all dependencies
		let mut loader = Loader::new(&loader_root);
		let files = loader
			.load_entry(entry_path, &model)
			.map_err(|e| format!("Load error: {}", e))?;

		// Lower each file in dependency order
		for file in files {
			let namespace_context = file.inferred_namespace.as_deref().unwrap_or("core");

			let mut lowerer = Lower::new(&mut model, namespace_context)
				.map_err(|e| format!("Lower error: {}", e.message))?;

			let result = lowerer
				.lower(&file.ast)
				.map_err(|e| format!("Lower error in {:?}: {}", file.path, e.message))?;

			all_rules.extend(result.rules);
			all_derives.extend(result.derives);
			all_policies.extend(result.policies);

			// Merge profiles additively
			for profile in result.profiles {
				let profile_name = profile.name.to_string();
				all_profiles
					.entry(profile_name)
					.or_default()
					.extend(profile.policies);
			}

			// Last selection wins (entry file should be last)
			if let Some(selection) = result.selected_profile {
				selected_profile = Some(selection.to_string());
			}
		}

		let kernel = Self {
			source_path: Some(entry_path.to_path_buf()),
			model,
			rules: all_rules,
			derives: all_derives,
			policies: all_policies,
			profiles: all_profiles,
			selected_profile,
		};

		// Validate profile and policy references
		kernel.validate()?;
		kernel.log_selected_profile();

		Ok(kernel)
	}

	/// Load an intent file on top of the current kernel
	pub fn load_intent(&mut self, path: &Path) -> Result<(), String> {
		let source = std::fs::read_to_string(path)
			.map_err(|e| format!("Could not read intent file: {}", e))?;

		// Infer namespace from filename
		let namespace = path.file_stem().and_then(|s| s.to_str()).unwrap_or("user");

		Self::load_source_into(
			&mut self.model,
			&source,
			namespace,
			&mut self.rules,
			&mut self.derives,
			&mut self.policies,
			&mut self.profiles,
			&mut self.selected_profile,
		)
		.map_err(|e| format!("parse error in {:?}: {}", path, e))?;

		self.validate()?;
		self.log_selected_profile();

		Ok(())
	}

	/// Bootstrap using embedded framework files
	fn bootstrap_embedded(skip_quickstart: bool) -> Self {
		//eprintln!("WA2: Using embedded framework");

		let mut model = Model::bootstrap();
		let mut all_rules = Vec::new();
		let mut all_derives = Vec::new();
		let mut all_policies = Vec::new();
		let mut all_profiles: HashMap<String, Vec<QualifiedName>> = HashMap::new();
		let mut selected_profile = None;

		// Load in dependency order
		let mut sources: Vec<(&str, &str)> = vec![
			(EMBEDDED_BOOTSTRAP, "_internal"),
			(EMBEDDED_CORE, "core"),
			(EMBEDDED_AWS, "aws"),
			(EMBEDDED_AWS_CFN, "aws:cfn"),
			(EMBEDDED_DATA, "data"),
		];

		if !skip_quickstart {
			sources.push((EMBEDDED_QUICKSTART, "my"));
		}

		for (source, namespace) in sources {
			if let Err(e) = Self::load_source_into(
				&mut model,
				source,
				namespace,
				&mut all_rules,
				&mut all_derives,
				&mut all_policies,
				&mut all_profiles,
				&mut selected_profile,
			) {
				eprintln!("WA2: Failed to load embedded {}: {}", namespace, e);
				panic!("Embedded framework should always load");
			}
			//eprintln!("WA2: Loaded embedded namespace '{}'", namespace);
		}

		let kernel = Self {
			source_path: None,
			model,
			rules: all_rules,
			derives: all_derives,
			policies: all_policies,
			profiles: all_profiles,
			selected_profile,
		};

		// Validate - panic on embedded since it should always be valid
		if let Err(e) = kernel.validate() {
			panic!("Embedded framework validation failed: {}", e);
		}
		kernel.log_selected_profile();

		kernel
	}

	/// Bootstrap with framework and explicit profile selection
	fn bootstrap_with_profile(
		framework_root: Option<&Path>,
		profile: Option<String>,
		skip_quickstart: bool,
	) -> Self {
		// For now, use embedded framework
		// TODO: Load from framework_root if provided
		let mut kernel = Self::bootstrap_embedded(skip_quickstart);

		// Override profile selection
		if let Some(p) = profile {
			kernel.selected_profile = Some(p);
		}

		// Re-validate with new profile selection
		if let Err(e) = kernel.validate() {
			eprintln!("WA2: Profile validation failed: {}", e);
			// Fall back to no profile selection
			kernel.selected_profile = None;
		}
		kernel.log_selected_profile();

		kernel
	}

	/// Load a source string into the model
	fn load_source_into(
		model: &mut Model,
		source: &str,
		namespace_context: &str,
		rules: &mut Vec<Rule>,
		derives: &mut Vec<Derive>,
		policies: &mut Vec<Policy>,
		profiles: &mut HashMap<String, Vec<QualifiedName>>,
		selected_profile: &mut Option<String>,
	) -> Result<(), String> {
		// Ensure namespace exists BEFORE parsing so resolver knows about it
		model
			.ensure_namespace(namespace_context)
			.map_err(|e| format!("Failed to create namespace {}: {}", namespace_context, e))?;

		let wa2_source = Wa2Source::from_str(source);
		let resolver = model_resolver(model);
		let ast = parser::parse_with_resolver(wa2_source.lexer(), resolver)
			.map_err(|e| format!("Parse error at {:?}: {}", e.span, e.message))?;

		let mut lowerer = Lower::new(model, namespace_context)
			.map_err(|e| format!("Lower error: {}", e.message))?;
		let result = lowerer
			.lower(&ast)
			.map_err(|e| format!("Lower error: {}", e.message))?;

		rules.extend(result.rules);
		derives.extend(result.derives);
		policies.extend(result.policies);

		// Merge profiles additively
		for profile in result.profiles {
			let profile_name = profile.name.to_string();
			profiles
				.entry(profile_name)
				.or_default()
				.extend(profile.policies);
		}

		// Update selection if present
		if let Some(selection) = result.selected_profile {
			*selected_profile = Some(selection.to_string());
		}

		Ok(())
	}

	/// Build a map from rule name to its modal (from selected profile's policies only)
	fn build_rule_modals(&self) -> HashMap<String, Modal> {
		let mut modals = HashMap::new();

		// Get policies from selected profile only
		let active_policy_names: HashSet<String> =
			if let Some(ref profile_name) = self.selected_profile {
				self.profiles
					.get(profile_name)
					.map(|policies| policies.iter().map(|p| p.to_string()).collect())
					.unwrap_or_default()
			} else {
				// No profile selected - no policies active
				HashSet::new()
			};
		// TODO: make into logging
		//eprintln!("active_policy_names: {active_policy_names:?}");

		for policy in &self.policies {
			// Skip policies not in the selected profile
			if !active_policy_names.contains(&policy.name) {
				continue;
			}
			// TODO: make into logging
			//eprintln!("\trunning policy {}", policy.name);

			for binding in &policy.bindings {
				let rule_name = binding.rule_name.to_string();
				let new_modal = binding.modal;
				modals
					.entry(rule_name)
					.and_modify(|existing| {
						*existing = Self::stricter_modal(*existing, new_modal);
					})
					.or_insert(new_modal);
			}
		}
		modals
	}

	fn stricter_modal(a: Modal, b: Modal) -> Modal {
		match (a, b) {
			(Modal::Must, _) | (_, Modal::Must) => Modal::Must,
			(Modal::Should, _) | (_, Modal::Should) => Modal::Should,
			_ => Modal::May,
		}
	}

	pub fn analyse(
		&self,
		text: &str,
		uri: &Url,
		format: DocumentFormat,
		vendor: Vendor,
		method: Method,
	) -> Result<AnalysisResult, Vec<Diagnostic>> {
		let mut model = self.model.clone();
		let derives = self.derives.clone();

		let projector = get_projector(vendor, method);
		projector.project_into(&mut model, text, uri, format)?;

		// Build rule→modal map from selected profile's policies
		let rule_modals = self.build_rule_modals();

		// Only run rules that are in the selected profile
		let active_rules: Vec<Rule> = self
			.rules
			.iter()
			.filter(|r| rule_modals.contains_key(&r.name))
			.cloned()
			.collect();

		let mut engine = RuleEngine::new();
		engine
			.run_with_modals(&mut model, &derives, &active_rules, &rule_modals)
			.map_err(|e| vec![Kernel::rule_error_to_diagnostic(&e)])?;

		//eprintln!("model: {}", print_model_as_tree(&model));
		let failures = self.collect_failures(&model);

		Ok(AnalysisResult { model, failures })
	}

	fn collect_failures(&self, model: &Model) -> Vec<AssertFailure> {
		let mut failures = Vec::new();

		if let Some(failure_type) = model.resolve("core:AssertFailure") {
			let subject_pred = model.resolve("core:subject");
			let area_pred = model.resolve("core:area");
			let severity_pred = model.resolve("core:severity");

			for i in 0..model.entity_count() {
				let entity = crate::intents::model::EntityId(i as u32);
				if model.has_type(entity, failure_type) {
					let assertion = model
						.get_literal(entity, "core:assertion")
						.unwrap_or_default();

					let severity = severity_pred
						.and_then(|p| model.get(entity, p))
						.and_then(|v| v.as_entity())
						.map(|e| model.qualified_name(e))
						.as_deref()
						.and_then(AssertSeverity::from_model_qname)
						.unwrap_or(AssertSeverity::Error);

					let subject = subject_pred
						.and_then(|p| model.get(entity, p))
						.and_then(|v| v.as_entity());

					let area = area_pred
						.and_then(|p| model.get(entity, p))
						.and_then(|v| v.as_entity());

					let message = model.get_literal(entity, "core:message");

					failures.push(AssertFailure {
						entity,
						assertion,
						severity,
						subject,
						area,
						message,
					});
				}
			}
		}

		failures
	}

	fn rule_error_to_diagnostic(err: &rules::RuleError) -> Diagnostic {
		Diagnostic {
			range: tower_lsp::lsp_types::Range::default(),
			severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
			message: format!("Rule error: {}", err.message),
			..Default::default()
		}
	}

	/// Validate profile selections and policy references
	fn validate(&self) -> Result<(), String> {
		// Validate selected profile exists
		if let Some(ref profile_name) = self.selected_profile {
			if !self.profiles.contains_key(profile_name) {
				return Err(format!(
					"selected profile '{}' is not declared; available profiles: {:?}",
					profile_name,
					self.profiles.keys().collect::<Vec<_>>()
				));
			}
		}

		// Validate policy references in profiles
		let policy_names: std::collections::HashSet<_> =
			self.policies.iter().map(|p| p.name.clone()).collect();

		for (profile_name, policy_refs) in &self.profiles {
			for policy_ref in policy_refs {
				let policy_name = policy_ref.to_string();
				if !policy_names.contains(&policy_name) {
					return Err(format!(
						"profile '{}' references unknown policy '{}'; available policies: {:?}",
						profile_name, policy_name, policy_names
					));
				}
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use crate::intents::kernel::{Kernel, rules::RuleEngine};

	#[test]
	fn test_embedded_bootstrap_loads() {
		let kernel = Kernel::bootstrap_embedded(false);

		// Should have loaded core types
		assert!(kernel.model.resolve("core:Store").is_some());
		assert!(kernel.model.resolve("core:Workload").is_some());
		assert!(kernel.model.resolve("aws:cfn:Resource").is_some());
		assert!(kernel.model.resolve("data:Criticality").is_some());

		// Should have rules
		assert!(!kernel.rules.is_empty());

		// Should have policies
		assert!(!kernel.policies.is_empty());
	}

	#[test]
	fn test_derive_stores_rule() {
		let kernel = Kernel::bootstrap_embedded(false);
		let mut model = kernel.model.clone();
		let derives = kernel.derives.clone();
		let rules = kernel.rules.clone();

		eprintln!("Loaded {} derives", derives.len());
		for derive in &derives {
			eprintln!("  - {}", derive.name);
		}
		eprintln!("Loaded {} rules", rules.len());
		for rule in &rules {
			eprintln!("  - {}", rule.name);
		}

		let workload = model.ensure_entity("core:workload").unwrap();
		model
			.apply_to(workload, "wa2:type", "core:Workload")
			.unwrap();
		model.set_root(workload);

		let template = model.blank();
		model
			.apply_to(template, "wa2:type", "aws:cfn:Template")
			.unwrap();
		model
			.apply_entity(workload, "core:source", template)
			.unwrap();

		let resources = model.blank();
		model
			.apply_entity(template, "aws:cfn:resources", resources)
			.unwrap();

		let bucket = model.ensure_raw("MyBucket");
		model
			.apply_to(bucket, "wa2:type", "aws:cfn:Resource")
			.unwrap();
		model
			.apply_to(bucket, "aws:type", "\"AWS::S3::Bucket\"")
			.unwrap();
		model
			.apply_entity(resources, "wa2:contains", bucket)
			.unwrap();

		let mut engine = RuleEngine::new();
		engine
			.run_with_modals(&mut model, &derives, &rules, &HashMap::new())
			.expect("run rules");

		let store_type = model
			.resolve("core:Store")
			.expect("core:Store should exist");

		let stores: Vec<_> = (0..model.entity_count())
			.map(|i| crate::intents::model::EntityId(i as u32))
			.filter(|&id| model.has_type(id, store_type))
			.collect();

		eprintln!("Found {} Store nodes", stores.len());
		assert_eq!(stores.len(), 1, "Should have created one Store node");

		let children = model.children(workload);
		let store_in_children = children.iter().any(|&c| model.has_type(c, store_type));
		assert!(store_in_children, "Store should be child of workload");
	}

	#[test]
	fn test_profile_merging() {
		let kernel = Kernel::bootstrap_embedded(false);

		// core:default profile should exist
		assert!(kernel.profiles.contains_key("core:default"));

		// Should contain data policies
		let default_policies = &kernel.profiles["core:default"];
		let policy_names: Vec<_> = default_policies.iter().map(|p| p.to_string()).collect();
		assert!(policy_names.contains(&"data:protect_critical_data".to_string()));
	}

	#[test]
	fn test_selected_profile() {
		let kernel = Kernel::bootstrap_embedded(false);

		// quickstart selects core:default
		assert_eq!(kernel.selected_profile, Some("core:default".to_string()));
	}

	#[test]
	fn test_derives_loaded() {
		let kernel = Kernel::bootstrap_embedded(false);

		// Should have derives from cfn.wa2 and quickstart.wa2
		let derive_names: Vec<_> = kernel.derives.iter().map(|d| d.name.as_str()).collect();

		assert!(derive_names.contains(&"aws:cfn:stores"));
		assert!(derive_names.contains(&"aws:cfn:runs"));
		assert!(derive_names.contains(&"aws:cfn:moves"));
	}
}
