//! Symbol table for CloudFormation templates
//!
//! Tracks declarations (resources, parameters, conditions, etc.) for validation.
//! Does not resolve types - that's handled separately by the type resolver.

use std::collections::HashMap;
use tower_lsp::lsp_types::Range;

use crate::spec::cfn_ir::types::CfnTemplate;

/// Symbol table built from a CloudFormation template
#[derive(Debug, Clone)]
pub struct SymbolTable {
	pub resources: HashMap<String, ResourceEntry>,
	pub parameters: HashMap<String, ParameterEntry>,
	pub conditions: HashMap<String, ConditionEntry>,
	pub pseudo_parameters: HashMap<String, PseudoParameterEntry>,
}

/// Entry for a resource in the symbol table
#[derive(Debug, Clone)]
pub struct ResourceEntry {
	pub logical_id: String,
	pub resource_type: String,
	pub location: Range,
}

/// Entry for a parameter in the symbol table
#[derive(Debug, Clone)]
pub struct ParameterEntry {
	pub name: String,
	pub declared_type: String, // "String", "Number", "List<Number>", etc.
	pub location: Range,
}

/// Entry for a condition in the symbol table
#[derive(Debug, Clone)]
pub struct ConditionEntry {
	pub name: String,
	pub location: Range,
}

/// Entry for a pseudo-parameter in the symbol table
#[derive(Debug, Clone)]
pub struct PseudoParameterEntry {
	pub name: String,
	pub description: &'static str,
}

impl SymbolTable {
	/// Build a symbol table from a parsed CloudFormation template
	pub fn from_template(template: &CfnTemplate) -> Self {
		let mut resources = HashMap::new();
		let mut parameters = HashMap::new();
		let mut conditions = HashMap::new();

		// Add resources
		for (logical_id, resource) in &template.resources {
			resources.insert(
				logical_id.clone(),
				ResourceEntry {
					logical_id: logical_id.clone(),
					resource_type: resource.resource_type.clone(),
					location: resource.logical_id_range,
				},
			);
		}

		// Add parameters
		for (param_name, param) in &template.parameters {
			parameters.insert(
				param_name.clone(),
				ParameterEntry {
					name: param_name.clone(),
					declared_type: param.parameter_type.clone(),
					location: param.name_range,
				},
			);
		}

		// Add conditions
		for (cond_name, cond) in &template.conditions {
			conditions.insert(
				cond_name.clone(),
				ConditionEntry {
					name: cond_name.clone(),
					location: cond.name_range,
				},
			);
		}

		// Add pseudo-parameters (always available)
		let pseudo_parameters = Self::create_pseudo_parameters();

		SymbolTable {
			resources,
			parameters,
			conditions,
			pseudo_parameters,
		}
	}

	/// Create the standard AWS pseudo-parameters
	fn create_pseudo_parameters() -> HashMap<String, PseudoParameterEntry> {
		let mut map = HashMap::new();

		let pseudo_params = [
			("AWS::AccountId", "AWS account ID"),
			("AWS::NotificationARNs", "Notification ARNs"),
			("AWS::NoValue", "Removes resource property"),
			("AWS::Partition", "AWS partition (aws, aws-cn, aws-us-gov)"),
			("AWS::Region", "AWS region"),
			("AWS::StackId", "Stack ID"),
			("AWS::StackName", "Stack name"),
			(
				"AWS::URLSuffix",
				"Domain suffix (amazonaws.com, amazonaws.com.cn)",
			),
		];

		for (name, description) in pseudo_params {
			map.insert(
				name.to_string(),
				PseudoParameterEntry {
					name: name.to_string(),
					description,
				},
			);
		}

		map
	}

	/// Check if a resource with the given logical ID exists
	pub fn has_resource(&self, logical_id: &str) -> bool {
		self.resources.contains_key(logical_id)
	}

	/// Check if a parameter with the given name exists
	pub fn has_parameter(&self, name: &str) -> bool {
		self.parameters.contains_key(name)
	}

	/// Check if a condition with the given name exists
	pub fn has_condition(&self, name: &str) -> bool {
		self.conditions.contains_key(name)
	}

	/// Check if a pseudo-parameter with the given name exists
	pub fn has_pseudo_parameter(&self, name: &str) -> bool {
		self.pseudo_parameters.contains_key(name)
	}

	/// Check if a Ref target exists (resource, parameter, or pseudo-parameter)
	pub fn has_ref_target(&self, target: &str) -> bool {
		self.has_resource(target) || self.has_parameter(target) || self.has_pseudo_parameter(target)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tower_lsp::lsp_types::Url;

	fn test_uri() -> Url {
		Url::parse("file:///test.yaml").unwrap()
	}

	#[test]
	fn symbol_table_includes_resources() {
		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
  MyQueue:
    Type: AWS::SQS::Queue
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		assert_eq!(symbols.resources.len(), 2);
		assert!(symbols.has_resource("MyBucket"));
		assert!(symbols.has_resource("MyQueue"));
		assert!(!symbols.has_resource("MissingResource"));

		let bucket = &symbols.resources["MyBucket"];
		assert_eq!(bucket.resource_type, "AWS::S3::Bucket");
	}

	#[test]
	fn symbol_table_includes_parameters() {
		let text = r#"
Parameters:
  Environment:
    Type: String
  InstanceCount:
    Type: Number

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		assert_eq!(symbols.parameters.len(), 2);
		assert!(symbols.has_parameter("Environment"));
		assert!(symbols.has_parameter("InstanceCount"));
		assert!(!symbols.has_parameter("MissingParam"));

		let env = &symbols.parameters["Environment"];
		assert_eq!(env.declared_type, "String");
	}

	#[test]
	fn symbol_table_includes_conditions() {
		let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]
  IsDevelopment: !Equals [!Ref Environment, "dev"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		assert_eq!(symbols.conditions.len(), 2);
		assert!(symbols.has_condition("IsProduction"));
		assert!(symbols.has_condition("IsDevelopment"));
		assert!(!symbols.has_condition("MissingCondition"));

		let is_prod = &symbols.conditions["IsProduction"];
		assert_eq!(is_prod.name, "IsProduction");
	}

	#[test]
	fn symbol_table_includes_pseudo_parameters() {
		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		assert!(symbols.has_pseudo_parameter("AWS::Region"));
		assert!(symbols.has_pseudo_parameter("AWS::AccountId"));
		assert!(symbols.has_pseudo_parameter("AWS::StackName"));
		assert!(!symbols.has_pseudo_parameter("AWS::FakeParam"));
	}

	#[test]
	fn has_ref_target_checks_all_sources() {
		let text = r#"
Parameters:
  MyParam:
    Type: String

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		// Resources
		assert!(symbols.has_ref_target("MyBucket"));

		// Parameters
		assert!(symbols.has_ref_target("MyParam"));

		// Pseudo-parameters
		assert!(symbols.has_ref_target("AWS::Region"));

		// Missing
		assert!(!symbols.has_ref_target("MissingThing"));
	}
}
