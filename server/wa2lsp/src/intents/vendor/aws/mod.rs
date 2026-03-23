//! AWS vendor projector

mod cfn_projector;
mod tests;

use tower_lsp::lsp_types::{Diagnostic, Url};

use crate::iaac::cloudformation::cfn_ir::types::{CFN_INTRINSICS, CfnTemplate};
use crate::intents::model::{Model, ModelError};
use crate::intents::vendor::{DocumentFormat, VendorProjector};

/// AWS CloudFormation projector
pub struct AwsCfnProjector;

impl AwsCfnProjector {
	pub fn new() -> Self {
		Self
	}
}

impl Default for AwsCfnProjector {
	fn default() -> Self {
		Self::new()
	}
}

impl VendorProjector for AwsCfnProjector {
	fn project_into(
		&self,
		model: &mut Model,
		text: &str,
		uri: &Url,
		format: DocumentFormat,
	) -> Result<(), Vec<Diagnostic>> {
		let template = match format {
			DocumentFormat::Json => CfnTemplate::from_json(text, uri),
			DocumentFormat::Yaml => CfnTemplate::from_yaml(text, uri),
		}?;

		project_template_into(model, &template, uri).map_err(model_error_to_diags)
	}
}

fn project_template_into(
	model: &mut Model,
	template: &CfnTemplate,
	uri: &Url,
) -> Result<(), ModelError> {
	// Create template entity
	let template_entity = model.blank();
	model.apply_to(template_entity, "wa2:type", "cfn:Template")?;

	// Create or get workload, set as root
	let workload = model.ensure_entity("core:workload")?;
	model.apply_to(workload, "wa2:type", "core:Workload")?;
	model.apply_entity(workload, "core:source", template_entity)?;
	model.set_root(workload);

	// Project template sections
	cfn_projector::project_outputs(model, template_entity, &template.outputs, uri)?;
	cfn_projector::project_parameters(model, template_entity, &template.parameters, uri)?;
	cfn_projector::project_pseudo_parameters(model, template_entity)?;

	let entities =
		cfn_projector::project_resources(model, template_entity, &template.resources, uri)?;

	Ok(())
}

fn model_error_to_diags(err: crate::intents::model::ModelError) -> Vec<Diagnostic> {
	vec![Diagnostic {
		range: tower_lsp::lsp_types::Range::default(),
		severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
		message: format!("Model error: {:?}", err),
		..Default::default()
	}]
}

const CFN_PREDICATES: &[&str] = &[
	"target",
	"attribute",
	"template",
	"parameters",
	"pseudoParameters",
	"type",
	"default",
	"description",
	"delimiter",
	"condition",
	"conditionName",
];

/// Ensure core architectural types exist in the model
fn ensure_core_types(model: &mut Model) -> Result<(), ModelError> {
	model.ensure_namespace("core")?;

	// DSL: enum Node { Run, Store, Move }
	model.apply("core:Node", "wa2:type", "wa2:Type")?;
	model.apply("core:Store", "wa2:type", "wa2:Type")?;
	model.apply("core:Store", "wa2:subTypeOf", "core:Node")?;
	model.apply("core:Run", "wa2:type", "wa2:Type")?;
	model.apply("core:Run", "wa2:subTypeOf", "core:Node")?;
	model.apply("core:Move", "wa2:type", "wa2:Type")?;
	model.apply("core:Move", "wa2:subTypeOf", "core:Node")?;

	// DSL: struct Workload { nodes: Node[] }
	model.apply("core:Workload", "wa2:type", "wa2:Type")?;
	model.apply("core:nodes", "wa2:type", "wa2:Predicate")?;
	model.apply("core:nodes", "wa2:domain", "core:Workload")?;
	model.apply("core:nodes", "wa2:range", "core:Node")?;

	// DSL: struct Evidence { value: String }
	model.apply("core:Evidence", "wa2:type", "wa2:Type")?;
	model.apply("core:value", "wa2:type", "wa2:Predicate")?;
	model.apply("core:value", "wa2:domain", "core:Evidence")?;

	// Predicates
	model.apply("core:source", "wa2:type", "wa2:Predicate")?;
	model.apply("core:source", "wa2:domain", "core:Node")?;
	model.apply("core:source", "wa2:range", "aws:cfn:Resource")?;

	Ok(())
}

/// Ensure CFN-specific types and predicates exist in the model
fn ensure_aws_types(model: &mut Model) -> Result<(), ModelError> {
	model.ensure_namespace("aws")?;

	Ok(())
}

/// Ensure CFN-specific types and predicates exist in the model
fn ensure_cfn_types(model: &mut Model) -> Result<(), ModelError> {
	model.ensure_namespace("aws")?; // Parent first
	model.ensure_namespace("aws:cfn")?; // Nested namespace

	model.ensure_entity("aws:cfn:Output")?;
	model.ensure_entity("aws:cfn:Resource")?;
	model.ensure_entity("aws:cfn:outputs")?;
	model.ensure_entity("aws:cfn:resources")?;
	model.ensure_entity("aws:cfn:value")?;
	model.ensure_entity("aws:cfn:exportName")?;
	model.ensure_entity("aws:cfn:SubVarRef")?;
	model.ensure_entity("aws:cfn:varRef")?;

	model.apply("aws:cfn:Template", "wa2:type", "wa2:Type")?;
	model.apply("aws:cfn:Parameter", "wa2:type", "wa2:Type")?;
	model.apply("aws:cfn:PseudoParameter", "wa2:type", "wa2:Type")?;
	model.apply("aws:cfn:Resource", "wa2:type", "wa2:Type")?;
	for name in CFN_INTRINSICS {
		model.apply(&format!("aws:cfn:{}", name), "wa2:type", "wa2:Type")?;
	}

	for name in CFN_PREDICATES {
		model.apply(&format!("aws:cfn:{}", name), "wa2:type", "wa2:Predicate")?;
	}

	Ok(())
}

// In vendor/aws/mod.rs
#[cfg(test)]
mod projection_tests {
	use super::*;
	use crate::intents::model::EntityId;

	fn test_uri() -> Url {
		Url::parse("file:///tmp/test.yaml").unwrap()
	}

	fn test_setup(cfn_text: &str) -> Model {
		let projector = AwsCfnProjector::new();
		let mut model = Model::bootstrap();
		// Ensure types exist (idempotent if already loaded from bootstrap.wa2)
		ensure_cfn_types(&mut model).unwrap();
		ensure_aws_types(&mut model).unwrap();
		ensure_core_types(&mut model).unwrap();

		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		model
	}

	#[test]
	fn debug_getatt_ranges() {
		let cfn_text = r#"
Resources:
  MyRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument: {}
  Bucket:
    Type: AWS::S3::Bucket
    Properties:
      RoleArn: !GetAtt MyRole.Arn
"#;

		let model = test_setup(cfn_text);

		eprintln!("\n=== All entities with source ranges (GetAtt test) ===");
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if let Some(range) = model.get_range(eid) {
				let name = model.qualified_name(eid);
				let types = model.types(eid);
				let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
				eprintln!("  {} : {:?} @ {:?}", name, type_names, range);
			}
		}

		// Check cfn:GetAtt type exists
		assert!(
			model.resolve("cfn:GetAtt").is_some(),
			"cfn:GetAtt should exist"
		);
	}

	#[test]
	fn debug_sub_var_refs() {
		let cfn_text = r#"Parameters:
  DataBucketName:
    Type: String
Resources:
  MyPolicy:
    Type: AWS::IAM::Policy
    Properties:
      Resource: !Sub "arn:${AWS::Partition}:s3:::${DataBucketName}/*"
"#;

		let model = test_setup(cfn_text);

		// Check for SubVarRef nodes
		let sub_var_ref_type = model
			.resolve("cfn:SubVarRef")
			.expect("cfn:SubVarRef should exist");

		let mut found_refs = 0;
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if model.has_type(eid, sub_var_ref_type) {
				found_refs += 1;
				eprintln!("  SubVarRef {:?} @ {:?}", eid, model.get_range(eid));
			}
		}

		assert_eq!(
			found_refs, 2,
			"Should find 2 SubVarRef nodes (AWS::Partition and DataBucketName)"
		);
	}

	#[test]
	fn project_json_basic() {
		let cfn_text = r#"{
  "Resources": {
	 "MyBucket": {
		"Type": "AWS::S3::Bucket",
		"Properties": {
		  "BucketName": "test-bucket"
		}
	 }
  }
}"#;

		let model = test_setup(cfn_text);

		// Verify resource was projected
		let bucket = model.resolve("MyBucket").expect("MyBucket should exist");
		let aws_type = model
			.get_literal(bucket, "aws:type")
			.expect("should have aws:type");
		assert_eq!(aws_type, "AWS::S3::Bucket");
	}

	#[test]
	fn project_json_ref() {
		let cfn_text = r#"{
  "Parameters": {
	 "Environment": {
		"Type": "String"
	 }
  },
  "Resources": {
	 "Bucket": {
		"Type": "AWS::S3::Bucket",
		"Properties": {
		  "BucketName": { "Ref": "Environment" }
		}
	 }
  }
}"#;

		let model = test_setup(cfn_text);

		// Verify Ref node exists and has correct type
		let cfn_ref_type = model.resolve("cfn:Ref").expect("cfn:Ref should exist");

		let mut found_ref = false;
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if model.has_type(eid, cfn_ref_type) {
				found_ref = true;
				// Verify it has a target
				let cfn_target = model
					.resolve("cfn:target")
					.expect("cfn:target should exist");
				let targets = model.get_all(eid, cfn_target);
				assert!(!targets.is_empty(), "Ref should have a target");
				break;
			}
		}
		assert!(found_ref, "Should find a Ref node");
	}

	#[test]
	fn project_json_getatt() {
		let cfn_text = r#"{
  "Resources": {
	 "MyRole": {
		"Type": "AWS::IAM::Role",
		"Properties": {
		  "AssumeRolePolicyDocument": {}
		}
	 },
	 "Bucket": {
		"Type": "AWS::S3::Bucket",
		"Properties": {
		  "RoleArn": { "Fn::GetAtt": ["MyRole", "Arn"] }
		}
	 }
  }
}"#;

		let model = test_setup(cfn_text);

		// Verify GetAtt node exists
		let cfn_getatt_type = model
			.resolve("cfn:GetAtt")
			.expect("cfn:GetAtt should exist");

		let mut found_getatt = false;
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if model.has_type(eid, cfn_getatt_type) {
				found_getatt = true;

				// Verify target points to MyRole
				let cfn_target = model
					.resolve("cfn:target")
					.expect("cfn:target should exist");
				let targets = model.get_all(eid, cfn_target);
				assert!(!targets.is_empty(), "GetAtt should have a target");

				let target_id = targets[0].as_entity().expect("target should be entity");
				let target_name = model.qualified_name(target_id);
				assert_eq!(target_name, "MyRole", "GetAtt should target MyRole");
				break;
			}
		}
		assert!(found_getatt, "Should find a GetAtt node");
	}

	#[test]
	fn project_json_sub() {
		let cfn_text = r#"{
  "Parameters": {
	 "BucketName": {
		"Type": "String"
	 }
  },
  "Resources": {
	 "Policy": {
		"Type": "AWS::IAM::Policy",
		"Properties": {
		  "Resource": { "Fn::Sub": "arn:${AWS::Partition}:s3:::${BucketName}/*" }
		}
	 }
  }
}"#;

		let model = test_setup(cfn_text);

		// Check for SubVarRef nodes
		let sub_var_ref_type = model
			.resolve("cfn:SubVarRef")
			.expect("cfn:SubVarRef should exist");

		let mut found_refs = 0;
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if model.has_type(eid, sub_var_ref_type) {
				found_refs += 1;
			}
		}

		assert_eq!(
			found_refs, 2,
			"Should find 2 SubVarRef nodes (AWS::Partition and BucketName)"
		);
	}

	#[test]
	fn project_json_getatt_dotted_string() {
		// JSON also supports the dotted string form for GetAtt
		let cfn_text = r#"{
  "Resources": {
	 "MyRole": {
		"Type": "AWS::IAM::Role",
		"Properties": {
		  "AssumeRolePolicyDocument": {}
		}
	 },
	 "Bucket": {
		"Type": "AWS::S3::Bucket",
		"Properties": {
		  "RoleArn": { "Fn::GetAtt": "MyRole.Arn" }
		}
	 }
  }
}"#;

		let model = test_setup(cfn_text);

		// Verify GetAtt node exists and targets MyRole
		let cfn_getatt_type = model
			.resolve("cfn:GetAtt")
			.expect("cfn:GetAtt should exist");

		let mut found_getatt = false;
		for i in 0..model.entity_count() {
			let eid = EntityId(i as u32);
			if model.has_type(eid, cfn_getatt_type) {
				found_getatt = true;
				break;
			}
		}
		assert!(
			found_getatt,
			"Should find a GetAtt node from dotted string form"
		);
	}

	#[test]
	fn project_tags_are_queryable() {
		let cfn_text = r#"
AWSTemplateFormatVersion: "2010-09-09"
Resources:
  DataBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: DataCriticality
          Value: Important
"#;

		let model = test_setup(cfn_text);

		// Debug: print model structure
		eprintln!(
			"Model:\n{}",
			crate::intents::model::print_model_as_tree(&model)
		);

		// Check DataBucket exists
		let bucket = model
			.resolve("DataBucket")
			.expect("DataBucket should exist");

		// Check aws:Tags predicate exists and points somewhere
		let tags_pred = model
			.resolve("aws:Tags")
			.expect("aws:Tags predicate should exist");
		let tags_container = model.get_all(bucket, tags_pred);
		eprintln!("Tags container: {:?}", tags_container);
		assert!(
			!tags_container.is_empty(),
			"DataBucket should have aws:Tags"
		);

		// Check the tag items have aws:Key
		let container_id = tags_container[0].as_entity().expect("should be entity");
		let children = model.children(container_id);
		eprintln!("Tag children: {:?}", children);
		assert!(!children.is_empty(), "Tags container should have children");

		// Check aws:Key on the tag item
		let key_pred = model.resolve("aws:Key").expect("aws:Key should exist");
		for child in &children {
			let keys = model.get_all(*child, key_pred);
			eprintln!("  Child {:?} has aws:Key: {:?}", child, keys);
		}
	}
}
