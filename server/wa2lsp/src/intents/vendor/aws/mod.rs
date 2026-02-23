//! AWS vendor projector

mod cfn_projector;
mod derivation;
mod tests;

use tower_lsp::lsp_types::{Diagnostic, Url};

use crate::iaac::cloudformation::cfn_ir::types::CfnTemplate;
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

		project_template_into(model, &template).map_err(model_error_to_diags)
	}
}

pub fn project_template_into(model: &mut Model, template: &CfnTemplate) -> Result<(), ModelError> {
	// Ensure types exist (idempotent if already loaded from bootstrap.wa2)
	cfn_projector::ensure_cfn_types(model)?;
	cfn_projector::ensure_aws_types(model)?;
	cfn_projector::ensure_core_types(model)?;

	// Create template entity
	let template_entity = model.blank();
	model.apply_to(template_entity, "wa2:type", "cfn:Template")?;

	// Create or get workload, set as root
	let workload = model.ensure_entity("core:workload")?;
	model.apply_to(workload, "wa2:type", "core:Workload")?;
	model.apply_entity(workload, "core:source", template_entity)?;
	model.set_root(workload);

	// Project template sections
	cfn_projector::project_outputs(model, template_entity, &template.outputs)?;
	cfn_projector::project_parameters(model, template_entity, &template.parameters)?;
	cfn_projector::project_pseudo_parameters(model, template_entity)?;

	let entities = cfn_projector::project_resources(model, template_entity, &template.resources)?;

	// Derive phase
	for entity in entities {
		derivation::derive_wa2_type(model, entity, workload)?;
		derivation::derive_evidence(model, entity)?;
	}

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

// In vendor/aws/mod.rs
#[cfg(test)]
mod projection_tests {
	use super::*;
	use crate::intents::model::EntityId;

	fn test_uri() -> Url {
		Url::parse("file:///tmp/test.yaml").unwrap()
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

		let projector = AwsCfnProjector::new();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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

		let projector = AwsCfnProjector::new();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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

		let projector = AwsCfnProjector::new();
		let uri = Url::parse("file:///tmp/test.json").unwrap();
      let mut model = Model::bootstrap();
		let result = projector
			.project_into(&mut model, cfn_text, &uri, DocumentFormat::Json)
			.expect("project");
		

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

		let projector = AwsCfnProjector::new();
		let uri = Url::parse("file:///tmp/test.json").unwrap();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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

		let projector = AwsCfnProjector::new();
		let uri = Url::parse("file:///tmp/test.json").unwrap();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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

		let projector = AwsCfnProjector::new();
		let uri = Url::parse("file:///tmp/test.json").unwrap();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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

		let projector = AwsCfnProjector::new();
		let uri = Url::parse("file:///tmp/test.json").unwrap();
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		

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
}
