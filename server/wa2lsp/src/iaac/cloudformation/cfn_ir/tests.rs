use url::Url;

use crate::iaac::cloudformation::cfn_ir::types::CfnValue;
use crate::iaac::cloudformation::spec_store::{
	CollectionKind, PrimitiveType, PropertyName, PropertyShape, ResourceTypeDescriptor,
	ResourceTypeId, ShapeKind, SpecStore, TypeInfo,
};
use crate::iaac::cloudformation::{
	cfn_ir::types::CfnTemplate,
	spec_store::{AttributeName, AttributeShape},
};
use tower_lsp::lsp_types::NumberOrString;

fn test_uri() -> Url {
	Url::parse("file:///tmp/test.yaml").unwrap()
}

#[test]
fn yaml_converts_simple_resource() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: test-bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	assert_eq!(template.resources.len(), 1);
	assert!(template.resources.contains_key("MyBucket"));

	let bucket = &template.resources["MyBucket"];
	assert_eq!(bucket.logical_id, "MyBucket");
	assert_eq!(bucket.resource_type, "AWS::S3::Bucket");
	assert_eq!(bucket.properties.len(), 1);

	let bucket_name = bucket.properties.get("BucketName").unwrap();
	assert_eq!(bucket_name.0.as_str(), Some("test-bucket"));
}

#[test]
fn yaml_converts_multiple_resources() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: test
  MyFunction:
    Type: AWS::Lambda::Function
    Properties:
      FunctionName: test-fn
      Runtime: python3.9
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	assert_eq!(template.resources.len(), 2);
	assert!(template.resources.contains_key("MyBucket"));
	assert!(template.resources.contains_key("MyFunction"));

	let function = &template.resources["MyFunction"];
	assert_eq!(function.resource_type, "AWS::Lambda::Function");
	assert_eq!(function.properties.len(), 2);
}

#[test]
fn yaml_resource_without_properties() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	assert_eq!(bucket.properties.len(), 0);
}

#[test]
fn yaml_nested_properties() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption:
        ServerSideEncryptionConfiguration:
          - ServerSideEncryptionByDefault:
              SSEAlgorithm: AES256
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let encryption = bucket.properties.get("BucketEncryption").unwrap();

	// Check it's an object
	assert!(encryption.0.as_object().is_some());

	let encryption_obj = encryption.0.as_object().unwrap();
	assert!(encryption_obj.contains_key("ServerSideEncryptionConfiguration"));
}

#[test]
fn yaml_property_with_array() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: Environment
          Value: Production
        - Key: Owner
          Value: Team
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let tags = bucket.properties.get("Tags").unwrap();

	let tags_array = tags.0.as_array().unwrap();
	assert_eq!(tags_array.len(), 2);
}

#[test]
fn yaml_missing_type_returns_error() {
	let text = r#"
Resources:
  MyBucket:
    Properties:
      BucketName: test
"#;

	let result = CfnTemplate::from_yaml(text, &test_uri());

	assert!(result.is_err());
	let diags = result.unwrap_err();
	assert_eq!(diags.len(), 1);
	assert!(diags[0].message.contains("missing required `Type`"));
}

#[test]
fn yaml_invalid_yaml_returns_parse_error() {
	let text = r#"
Resources:
  MyBucket:
    Type: "unclosed string
"#;

	let result = CfnTemplate::from_yaml(text, &test_uri());

	assert!(result.is_err());
	let diags = result.unwrap_err();
	assert_eq!(diags.len(), 1);
	assert!(diags[0].message.contains("YAML parse error"));
}

#[test]
fn yaml_empty_resources_section() {
	let text = r#"
Resources: {}
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.resources.len(), 0);
}

#[test]
fn yaml_no_resources_section() {
	let text = r#"
Parameters:
  Env:
    Type: String
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.resources.len(), 0);
}

#[test]
fn yaml_preserves_position_info() {
	let text = r#"Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];

	// MyBucket is on line 1 (0-indexed)
	assert_eq!(bucket.logical_id_range.start.line, 1);

	// Type value is on line 2
	assert_eq!(bucket.type_range.start.line, 2);
}

fn create_test_spec() -> SpecStore {
	let mut resource_types = std::collections::HashMap::new();

	// AWS::S3::Bucket with BucketName and BucketEncryption as simple strings (not complex)
	let mut s3_bucket_props = std::collections::HashMap::new();
	s3_bucket_props.insert(
		PropertyName("BucketName".into()),
		PropertyShape {
			name: PropertyName("BucketName".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			},
			required: false,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);
	s3_bucket_props.insert(
		PropertyName("BucketEncryption".into()),
		PropertyShape {
			name: PropertyName("BucketEncryption".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String), // Changed from Complex to String
				collection: CollectionKind::Scalar,
			},
			required: true,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);
	s3_bucket_props.insert(
		PropertyName("Tags".into()),
		PropertyShape {
			name: PropertyName("Tags".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::List, // List of strings for simplicity
			},
			required: false,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);

	resource_types.insert(
		ResourceTypeId("AWS::S3::Bucket".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::S3::Bucket".into()),
			properties: s3_bucket_props,
			attributes: std::collections::HashMap::new(),
			documentation_url: None,
		},
	);

	// AWS::Lambda::Function with required properties as strings
	let mut lambda_props = std::collections::HashMap::new();
	lambda_props.insert(
		PropertyName("FunctionName".into()),
		PropertyShape {
			name: PropertyName("FunctionName".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			},
			required: false,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);
	lambda_props.insert(
		PropertyName("Code".into()),
		PropertyShape {
			name: PropertyName("Code".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String), // Simplified for testing
				collection: CollectionKind::Scalar,
			},
			required: true,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);
	lambda_props.insert(
		PropertyName("Runtime".into()),
		PropertyShape {
			name: PropertyName("Runtime".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			},
			required: true,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);
	lambda_props.insert(
		PropertyName("RoleArn".into()),
		PropertyShape {
			name: PropertyName("RoleArn".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			},
			required: false,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,

			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);

	resource_types.insert(
		ResourceTypeId("AWS::Lambda::Function".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::Lambda::Function".into()),
			properties: lambda_props,
			attributes: std::collections::HashMap::new(),
			documentation_url: None,
		},
	);

	SpecStore {
		resource_types,
		property_types: std::collections::HashMap::new(),
	}
}

#[test]
fn validation_detects_unknown_resource_type() {
	let text = r#"
Resources:
  MyResource:
    Type: AWS::FakeService::FakeResource
    Properties:
      SomeProp: value
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let spec = create_test_spec();
	let diags = template.validate_against_spec(&spec);

	assert_eq!(diags.len(), 1);
	assert!(
		diags[0]
			.message
			.contains("Unknown CloudFormation resource type")
	); // Changed capitalization
	assert!(diags[0].message.contains("AWS::FakeService::FakeResource"));
}

#[test]
fn validation_detects_unknown_property() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      InvalidProperty: value
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let spec = create_test_spec();
	let diags = template.validate_against_spec(&spec);

	assert_eq!(diags.len(), 1);
	assert!(diags[0].message.contains("Unknown property")); // Changed capitalization
	assert!(diags[0].message.contains("InvalidProperty"));
}

#[test]
fn validation_detects_missing_required_property() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: test
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let spec = create_test_spec();
	let diags = template.validate_against_spec(&spec);

	assert_eq!(diags.len(), 1);
	assert!(diags[0].message.contains("missing required property"));
	assert!(diags[0].message.contains("BucketEncryption"));
}

#[test]
fn validation_passes_for_valid_resource() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      BucketName: test
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let spec = create_test_spec();
	let diags = template.validate_against_spec(&spec);

	assert_eq!(diags.len(), 0);
}

#[test]
fn yaml_intrinsic_ref_long_form_converts_to_ref_value() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName:
        Ref: OtherBucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket
		.properties
		.get("BucketName")
		.expect("BucketName property must exist");

	match bucket_name {
		(CfnValue::Ref { target, .. }, _) => {
			assert_eq!(target, "OtherBucket");
		}
		other => panic!("expected Ref CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_intrinsic_getatt_array_form_converts_to_getatt_value() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Arn:
        Fn::GetAtt: [MyBucket, Arn]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let arn = bucket
		.properties
		.get("Arn")
		.expect("Arn property must exist");

	match arn {
		(
			CfnValue::GetAtt {
				target, attribute, ..
			},
			_,
		) => {
			assert_eq!(target, "MyBucket");
			assert_eq!(attribute, "Arn");
		}
		other => panic!("expected GetAtt CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_intrinsic_getatt_string_form_converts_to_getatt_value() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Arn:
        Fn::GetAtt: MyBucket.Arn
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let arn = bucket
		.properties
		.get("Arn")
		.expect("Arn property must exist");

	match arn {
		(
			CfnValue::GetAtt {
				target, attribute, ..
			},
			_,
		) => {
			assert_eq!(target, "MyBucket");
			assert_eq!(attribute, "Arn");
		}
		other => panic!("expected GetAtt CfnValue, got {:?}", other),
	}
}

#[test]
fn json_intrinsic_ref_object_converts_to_ref_value() {
	let text = r#"
{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "BucketName": { "Ref": "OtherBucket" }
      }
    }
  }
}
"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket
		.properties
		.get("BucketName")
		.expect("BucketName property must exist");

	match bucket_name {
		(CfnValue::Ref { target, .. }, _) => {
			assert_eq!(target, "OtherBucket");
		}
		other => panic!("expected Ref CfnValue, got {:?}", other),
	}
}

#[test]
fn json_intrinsic_getatt_forms_convert_to_getatt_value() {
	// Array form
	let text_array = r#"
{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "Arn": { "Fn::GetAtt": ["MyBucket", "Arn"] }
      }
    }
  }
}
"#;

	let template_array = CfnTemplate::from_json(text_array, &test_uri()).unwrap();
	let bucket_array = &template_array.resources["MyBucket"];
	let arn_array = bucket_array
		.properties
		.get("Arn")
		.expect("Arn property must exist");

	match arn_array {
		(
			CfnValue::GetAtt {
				target, attribute, ..
			},
			_,
		) => {
			assert_eq!(target, "MyBucket");
			assert_eq!(attribute, "Arn");
		}
		other => panic!("expected GetAtt CfnValue (array form), got {:?}", other),
	}

	// String form
	let text_string = r#"
{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "Arn": { "Fn::GetAtt": "MyBucket.Arn" }
      }
    }
  }
}
"#;

	let template_string = CfnTemplate::from_json(text_string, &test_uri()).unwrap();
	let bucket_string = &template_string.resources["MyBucket"];
	let arn_string = bucket_string
		.properties
		.get("Arn")
		.expect("Arn property must exist");

	match arn_string {
		(
			CfnValue::GetAtt {
				target, attribute, ..
			},
			_,
		) => {
			assert_eq!(target, "MyBucket");
			assert_eq!(attribute, "Arn");
		}
		other => panic!("expected GetAtt CfnValue (string form), got {:?}", other),
	}
}

#[test]
fn yaml_short_form_ref_converts_to_ref_value() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref OtherBucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket
		.properties
		.get("BucketName")
		.expect("BucketName property must exist");

	match bucket_name {
		(CfnValue::Ref { target, .. }, _) => {
			assert_eq!(target, "OtherBucket");
		}
		other => panic!("expected Ref CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_short_form_getatt_converts_to_getatt_value() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Arn: !GetAtt MyBucket.Arn
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	let bucket = &template.resources["MyBucket"];
	let arn = bucket
		.properties
		.get("Arn")
		.expect("Arn property must exist");

	match arn {
		(
			CfnValue::GetAtt {
				target, attribute, ..
			},
			_,
		) => {
			assert_eq!(target, "MyBucket");
			assert_eq!(attribute, "Arn");
		}
		other => panic!("expected GetAtt CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_short_form_intrinsics_complete_check() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref OtherBucket
      Arn: !GetAtt MyBucket.Arn
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let output = template.to_string();

	let expected = r#"Template:
  Resources:
    MyBucket:
      Type: AWS::S3::Bucket
      Properties:
        Arn: !GetAtt MyBucket.Arn
        BucketName: !Ref OtherBucket
"#;

	assert_eq!(output, expected);
}

#[test]
fn yaml_parses_parameters_section() {
	let text = r#"
Parameters:
  Environment:
    Type: String
    Default: dev
    Description: Deployment environment
  InstanceCount:
    Type: Number
    Default: 2

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	assert_eq!(template.parameters.len(), 2);
	assert!(template.parameters.contains_key("Environment"));
	assert!(template.parameters.contains_key("InstanceCount"));

	let env_param = &template.parameters["Environment"];
	assert_eq!(env_param.parameter_type, "String");
	assert_eq!(
		env_param.description,
		Some("Deployment environment".to_string())
	);
	assert!(env_param.default_value.is_some());

	let count_param = &template.parameters["InstanceCount"];
	assert_eq!(count_param.parameter_type, "Number");
}

#[test]
fn json_parses_parameters_section() {
	let text = r#"{
  "Parameters": {
    "Environment": {
      "Type": "String",
      "Default": "dev",
      "Description": "Deployment environment"
    },
    "InstanceCount": {
      "Type": "Number",
      "Default": 2
    }
  },
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket"
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();

	assert_eq!(template.parameters.len(), 2);
	assert!(template.parameters.contains_key("Environment"));
	assert!(template.parameters.contains_key("InstanceCount"));

	let env_param = &template.parameters["Environment"];
	assert_eq!(env_param.parameter_type, "String");
	assert_eq!(
		env_param.description,
		Some("Deployment environment".to_string())
	);
}

#[test]
fn test_invalid_ref_target() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref NonExistentResource
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec(); // Changed from new_empty()
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code == Some(NumberOrString::String("WA2_CFN_INVALID_REF".into()))
			&& d.message.contains("NonExistentResource")
	}));
}

#[test]
fn test_valid_ref_to_resource() {
	let text = r#"
Resources:
  SourceBucket:
    Type: AWS::S3::Bucket
  DestBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref SourceBucket
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec(); // Changed
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(
		!diagnostics
			.iter()
			.any(|d| { d.code == Some(NumberOrString::String("WA2_CFN_INVALID_REF".into())) })
	);
}

#[test]
fn test_valid_ref_to_parameter() {
	let text = r#"
Parameters:
  Environment:
    Type: String

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref Environment
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec(); // Changed
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(
		!diagnostics
			.iter()
			.any(|d| { d.code == Some(NumberOrString::String("WA2_CFN_INVALID_REF".into())) })
	);
}

#[test]
fn test_valid_ref_to_pseudo_parameter() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref AWS::Region
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec(); // Changed
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(
		!diagnostics
			.iter()
			.any(|d| { d.code == Some(NumberOrString::String("WA2_CFN_INVALID_REF".into())) })
	);
}

#[test]
fn test_invalid_getatt_resource() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Arn: !GetAtt NonExistentBucket.Arn
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec(); // Changed
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code
			== Some(NumberOrString::String(
				"WA2_CFN_INVALID_GETATT_RESOURCE".into(),
			)) && d.message.contains("NonExistentBucket")
	}));
}

#[test]
fn test_type_mismatch_ref_returns_string() {
	// Create spec with S3::Bucket and Lambda::Function
	let mut resource_types = std::collections::HashMap::new();

	// Add S3::Bucket (empty properties, just needs to exist)
	resource_types.insert(
		ResourceTypeId("AWS::S3::Bucket".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::S3::Bucket".into()),
			properties: std::collections::HashMap::new(),
			attributes: std::collections::HashMap::new(),
			documentation_url: None,
		},
	);

	// Add Lambda::Function with Timeout property that expects Integer
	let mut lambda_props = std::collections::HashMap::new();
	lambda_props.insert(
		PropertyName("Timeout".into()),
		PropertyShape {
			name: PropertyName("Timeout".into()),
			type_info: TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::Integer),
				collection: CollectionKind::Scalar,
			},
			required: false,
			documentation_url: None,
			update_behavior: None,
			duplicates_allowed: false,
			// legacy spec doesn't have these
			one_of_required: None,
			any_of_required: None,
			all_of_required: None,
		},
	);

	resource_types.insert(
		ResourceTypeId("AWS::Lambda::Function".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::Lambda::Function".into()),
			properties: lambda_props,
			attributes: std::collections::HashMap::new(),
			documentation_url: None,
		},
	);

	let spec = SpecStore {
		resource_types,
		property_types: std::collections::HashMap::new(),
	};

	// Ref to a resource returns String, but Timeout needs Integer
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
  MyFunction:
    Type: AWS::Lambda::Function
    Properties:
      Timeout: !Ref MyBucket
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let diagnostics = template.validate_against_spec(&spec);

	assert!(
		diagnostics.iter().any(|d| {
			d.code == Some(NumberOrString::String("WA2_CFN_TYPE_MISMATCH".into()))
				&& d.message.contains("expected Integer")
				&& d.message.contains("got String")
		}),
		"Expected type mismatch diagnostic, got: {:?}",
		diagnostics
	);
}

#[test]
fn test_type_match_string_property() {
	let text = r#"
Parameters:
  BucketName:
    Type: String

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref BucketName
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec);

	// Should not have type mismatch - String parameter to String property is valid
	assert!(
		!diagnostics
			.iter()
			.any(|d| { d.code == Some(NumberOrString::String("WA2_CFN_TYPE_MISMATCH".into())) }),
		"Should not have type mismatch, got: {:?}",
		diagnostics
	);
}

#[test]
fn test_invalid_getatt_attribute() {
	// Create spec with S3::Bucket that has only "Arn" attribute
	let mut resource_types = std::collections::HashMap::new();
	let mut s3_attributes = std::collections::HashMap::new();

	s3_attributes.insert(
		AttributeName("Arn".into()),
		AttributeShape {
			name: AttributeName("Arn".into()),
			type_info: Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			}),
			documentation_url: None,
		},
	);

	resource_types.insert(
		ResourceTypeId("AWS::S3::Bucket".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::S3::Bucket".into()),
			properties: std::collections::HashMap::new(),
			attributes: s3_attributes,
			documentation_url: None,
		},
	);

	let spec = SpecStore {
		resource_types,
		property_types: std::collections::HashMap::new(),
	};

	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
  AnotherBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !GetAtt MyBucket.InvalidAttribute
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let diagnostics = template.validate_against_spec(&spec);

	assert!(
		diagnostics.iter().any(|d| {
			d.code
				== Some(NumberOrString::String(
					"WA2_CFN_INVALID_GETATT_ATTRIBUTE".into(),
				)) && d.message.contains("InvalidAttribute")
		}),
		"Expected invalid attribute diagnostic, got: {:?}",
		diagnostics
	);
}

#[test]
fn test_valid_getatt_attribute() {
	// Create spec with S3::Bucket that has "Arn" attribute
	let mut resource_types = std::collections::HashMap::new();
	let mut s3_attributes = std::collections::HashMap::new();

	s3_attributes.insert(
		AttributeName("Arn".into()),
		AttributeShape {
			name: AttributeName("Arn".into()),
			type_info: Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			}),
			documentation_url: None,
		},
	);

	resource_types.insert(
		ResourceTypeId("AWS::S3::Bucket".into()),
		ResourceTypeDescriptor {
			type_id: ResourceTypeId("AWS::S3::Bucket".into()),
			properties: std::collections::HashMap::new(),
			attributes: s3_attributes,
			documentation_url: None,
		},
	);

	let spec = SpecStore {
		resource_types,
		property_types: std::collections::HashMap::new(),
	};

	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
  AnotherBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !GetAtt MyBucket.Arn
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let diagnostics = template.validate_against_spec(&spec);

	// Should not have invalid attribute error
	assert!(
		!diagnostics.iter().any(|d| {
			d.code
				== Some(NumberOrString::String(
					"WA2_CFN_INVALID_GETATT_ATTRIBUTE".into(),
				))
		}),
		"Should not have invalid attribute error, got: {:?}",
		diagnostics
	);
}

#[test]
fn yaml_sub_string_form() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Sub "my-bucket-${AWS::Region}"
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(
			CfnValue::Sub {
				template,
				variables,
				..
			},
			_,
		) => {
			// template is now Box<CfnValue>, check if it's a String
			if let CfnValue::String(s, _) = &**template {
				assert_eq!(s, "my-bucket-${AWS::Region}");
			} else {
				panic!("Expected template to be a String, got {:?}", template);
			}
			assert!(variables.is_none());
		}
		other => panic!("expected Sub CfnValue, got {:?}", other),
	}
}

#[test]
fn test_invalid_sub_variable() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Sub "my-bucket-${NonExistent}"
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code
			== Some(NumberOrString::String(
				"WA2_CFN_INVALID_SUB_VARIABLE".into(),
			)) && d.message.contains("NonExistent")
	}));
}

#[test]
fn yaml_getazs() {
	let text = r#"
Resources:
  MyAutoScaling:
    Type: AWS::AutoScaling::AutoScalingGroup
    Properties:
      AvailabilityZones: !GetAZs
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let resource = &template.resources["MyAutoScaling"];
	let azs = resource.properties.get("AvailabilityZones").unwrap();

	match azs {
		(CfnValue::GetAZs { region, .. }, _) => {
			assert!(matches!(**region, CfnValue::String(ref s, _) if s.is_empty()));
		}
		other => panic!("expected GetAZs CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_join() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Join ["-", ["dev", !Ref AWS::Region, "bucket"]]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(
			CfnValue::Join {
				delimiter, values, ..
			},
			_,
		) => {
			assert_eq!(delimiter, "-");
			// values is now Box<CfnValue>, should be an Array
			if let CfnValue::Array(items, _) = &**values {
				assert_eq!(items.len(), 3);

				// Check first value is "dev"
				assert!(matches!(&items[0], CfnValue::String(s, _) if s == "dev"));

				// Check second value is !Ref AWS::Region
				assert!(
					matches!(&items[1], CfnValue::Ref { target, .. } if target == "AWS::Region")
				);

				// Check third value is "bucket"
				assert!(matches!(&items[2], CfnValue::String(s, _) if s == "bucket"));
			} else {
				panic!("Expected Join values to be an Array, got {:?}", values);
			}
		}
		other => panic!("expected Join CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_select() {
	let text = r#"
Resources:
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      AvailabilityZone: !Select [0, !GetAZs ""]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let instance = &template.resources["MyInstance"];
	let az = instance.properties.get("AvailabilityZone").unwrap();

	match az {
		(CfnValue::Select { index, list, .. }, _) => {
			// Check index is 0
			assert!(matches!(**index, CfnValue::Number(n, _) if n == 0.0));

			// Check list is !GetAZs ""
			assert!(matches!(**list, CfnValue::GetAZs { .. }));
		}
		other => panic!("expected Select CfnValue, got {:?}", other),
	}
}

#[test]
fn test_select_with_valid_numeric_index() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Select [0, !GetAZs ""]
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	// Should NOT have any Select index errors
	assert!(
		!diagnostics.iter().any(|d| {
			d.code
				== Some(NumberOrString::String(
					"WA2_CFN_INVALID_SELECT_INDEX".into(),
				))
		}),
		"Valid numeric index should not produce errors, got: {:?}",
		diagnostics
	);
}

#[test]
fn test_select_with_non_numeric_index() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Select ["invalid", !GetAZs ""]
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code
			== Some(NumberOrString::String(
				"WA2_CFN_INVALID_SELECT_INDEX".into(),
			))
	}));
}

#[test]
fn yaml_parses_conditions_section() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]
  IsDevelopment: !Equals [!Ref Environment, "dev"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();

	assert_eq!(template.conditions.len(), 2);
	assert!(template.conditions.contains_key("IsProduction"));
	assert!(template.conditions.contains_key("IsDevelopment"));
}

#[test]
fn yaml_if_intrinsic() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !If [IsProduction, "prod-bucket", "dev-bucket"]
      BucketEncryption: enabled
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(
			CfnValue::If {
				condition_name,
				value_if_true,
				value_if_false,
				..
			},
			_,
		) => {
			assert_eq!(condition_name, "IsProduction");
			assert!(matches!(**value_if_true, CfnValue::String(ref s, _) if s == "prod-bucket"));
			assert!(matches!(**value_if_false, CfnValue::String(ref s, _) if s == "dev-bucket"));
		}
		other => panic!("expected If CfnValue, got {:?}", other),
	}
}

#[test]
fn test_if_with_invalid_condition() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !If [NonExistentCondition, "prod", "dev"]
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code
			== Some(NumberOrString::String(
				"WA2_CFN_INVALID_IF_CONDITION".into(),
			)) && d.message.contains("NonExistentCondition")
	}));
}

#[test]
fn test_if_with_valid_condition() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !If [IsProduction, "prod-bucket", "dev-bucket"]
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	// Should NOT have any If condition errors
	assert!(
		!diagnostics.iter().any(|d| {
			d.code
				== Some(NumberOrString::String(
					"WA2_CFN_INVALID_IF_CONDITION".into(),
				))
		}),
		"Valid condition should not produce errors, got: {:?}",
		diagnostics
	);
}

#[test]
fn yaml_equals_intrinsic() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 1);

	let condition = &template.conditions["IsProduction"];
	match &condition.expression {
		CfnValue::Equals { left, right, .. } => {
			// Left should be !Ref Environment
			assert!(matches!(**left, CfnValue::Ref { ref target, .. } if target == "Environment"));
			// Right should be "prod"
			assert!(matches!(**right, CfnValue::String(ref s, _) if s == "prod"));
		}
		other => panic!("expected Equals CfnValue, got {:?}", other),
	}
}

#[test]
fn json_equals_intrinsic() {
	let text = r#"{
  "Conditions": {
    "IsProduction": {
      "Fn::Equals": [
        {"Ref": "Environment"},
        "prod"
      ]
    }
  },
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket"
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 1);

	let condition = &template.conditions["IsProduction"];
	match &condition.expression {
		CfnValue::Equals { left, right, .. } => {
			assert!(matches!(**left, CfnValue::Ref { ref target, .. } if target == "Environment"));
			assert!(matches!(**right, CfnValue::String(ref s, _) if s == "prod"));
		}
		other => panic!("expected Equals CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_not_intrinsic() {
	let text = r#"
Conditions:
  IsNotProduction: !Not [!Equals [!Ref Environment, "prod"]]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 1);

	let condition = &template.conditions["IsNotProduction"];
	match &condition.expression {
		CfnValue::Not { condition, .. } => {
			// Inside should be !Equals
			assert!(matches!(**condition, CfnValue::Equals { .. }));
		}
		other => panic!("expected Not CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_and_intrinsic() {
	let text = r#"
Conditions:
  IsProdAndUSEast: !And
    - !Equals [!Ref Environment, "prod"]
    - !Equals [!Ref AWS::Region, "us-east-1"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 1);

	let condition = &template.conditions["IsProdAndUSEast"];
	match &condition.expression {
		CfnValue::And { conditions, .. } => {
			assert_eq!(conditions.len(), 2);
			// Both should be Equals
			assert!(matches!(&conditions[0], CfnValue::Equals { .. }));
			assert!(matches!(&conditions[1], CfnValue::Equals { .. }));
		}
		other => panic!("expected And CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_or_intrinsic() {
	let text = r#"
Conditions:
  IsProdOrStaging: !Or
    - !Equals [!Ref Environment, "prod"]
    - !Equals [!Ref Environment, "staging"]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 1);

	let condition = &template.conditions["IsProdOrStaging"];
	match &condition.expression {
		CfnValue::Or { conditions, .. } => {
			assert_eq!(conditions.len(), 2);
			assert!(matches!(&conditions[0], CfnValue::Equals { .. }));
			assert!(matches!(&conditions[1], CfnValue::Equals { .. }));
		}
		other => panic!("expected Or CfnValue, got {:?}", other),
	}
}

#[test]
fn test_and_with_too_few_conditions() {
	let text = r#"
Conditions:
  Invalid: !And [!Equals [!Ref Env, "prod"]]

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let result = CfnTemplate::from_yaml(text, &test_uri());
	assert!(result.is_err());
	let diags = result.unwrap_err();
	assert!(diags.iter().any(|d| d.message.contains("2-10 conditions")));
}

#[test]
fn yaml_condition_intrinsic() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]
  ShouldCreateBucket: !Condition IsProduction

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	assert_eq!(template.conditions.len(), 2);

	let condition = &template.conditions["ShouldCreateBucket"];
	match &condition.expression {
		CfnValue::Condition { condition_name, .. } => {
			assert_eq!(condition_name, "IsProduction");
		}
		other => panic!("expected Condition CfnValue, got {:?}", other),
	}
}

#[test]
fn test_condition_with_invalid_reference() {
	let text = r#"
Conditions:
  Invalid: !Condition NonExistentCondition

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	assert!(diagnostics.iter().any(|d| {
		d.code
			== Some(NumberOrString::String(
				"WA2_CFN_INVALID_CONDITION_REF".into(),
			)) && d.message.contains("NonExistentCondition")
	}));
}

#[test]
fn test_condition_with_valid_reference() {
	let text = r#"
Conditions:
  IsProduction: !Equals [!Ref Environment, "prod"]
  ShouldCreate: !Condition IsProduction

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
"#;

	let uri = test_uri();
	let template = CfnTemplate::from_yaml(text, &uri).unwrap();
	let spec_store = create_test_spec();
	let diagnostics = template.validate_against_spec(&spec_store);

	// Should NOT have any Condition reference errors
	assert!(
		!diagnostics.iter().any(|d| {
			d.code
				== Some(NumberOrString::String(
					"WA2_CFN_INVALID_CONDITION_REF".into(),
				))
		}),
		"Valid condition reference should not produce errors, got: {:?}",
		diagnostics
	);
}

// ===== Base64 Tests =====

#[test]
fn yaml_base64_string_form() {
	let text = r#"
Resources:
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      UserData: !Base64 "hello world"
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let instance = &template.resources["MyInstance"];
	let user_data = instance.properties.get("UserData").unwrap();

	match user_data {
		(CfnValue::Base64 { value, .. }, _) => {
			assert!(matches!(**value, CfnValue::String(ref s, _) if s == "hello world"));
		}
		other => panic!("expected Base64 CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_base64_with_sub() {
	let text = r#"
Resources:
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      UserData: !Base64
        Fn::Sub: |
          #!/bin/bash
          echo Region: ${AWS::Region}
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let instance = &template.resources["MyInstance"];
	let user_data = instance.properties.get("UserData").unwrap();

	match user_data {
		(CfnValue::Base64 { value, .. }, _) => {
			assert!(matches!(**value, CfnValue::Sub { .. }));
		}
		other => panic!("expected Base64 with Sub, got {:?}", other),
	}
}

#[test]
fn json_base64() {
	let text = r#"{
  "Resources": {
    "MyInstance": {
      "Type": "AWS::EC2::Instance",
      "Properties": {
        "UserData": {
          "Fn::Base64": "echo hello"
        }
      }
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();
	let instance = &template.resources["MyInstance"];
	let user_data = instance.properties.get("UserData").unwrap();

	match user_data {
		(CfnValue::Base64 { value, .. }, _) => {
			assert!(matches!(**value, CfnValue::String(ref s, _) if s == "echo hello"));
		}
		other => panic!("expected Base64 CfnValue, got {:?}", other),
	}
}

// ===== Split Tests =====

#[test]
fn yaml_split_basic() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags: !Split [",", "Key1,Key2,Key3"]
      BucketEncryption: enabled
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let tags = bucket.properties.get("Tags").unwrap();

	match tags {
		(
			CfnValue::Split {
				delimiter, source, ..
			},
			_,
		) => {
			assert_eq!(delimiter, ",");
			assert!(matches!(**source, CfnValue::String(ref s, _) if s == "Key1,Key2,Key3"));
		}
		other => panic!("expected Split CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_split_with_ref() {
	let text = r#"
Parameters:
  CommaSeparatedList:
    Type: String

Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags: !Split [",", !Ref CommaSeparatedList]
      BucketEncryption: enabled
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let tags = bucket.properties.get("Tags").unwrap();

	match tags {
		(
			CfnValue::Split {
				delimiter, source, ..
			},
			_,
		) => {
			assert_eq!(delimiter, ",");
			assert!(
				matches!(&**source, CfnValue::Ref { target, .. } if target == "CommaSeparatedList")
			);
		}
		other => panic!("expected Split with Ref, got {:?}", other),
	}
}

#[test]
fn json_split() {
	let text = r#"{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "Tags": {
          "Fn::Split": [",", "Key1,Key2,Key3"]
        }
      }
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let tags = bucket.properties.get("Tags").unwrap();

	match tags {
		(
			CfnValue::Split {
				delimiter, source, ..
			},
			_,
		) => {
			assert_eq!(delimiter, ",");
			assert!(matches!(**source, CfnValue::String(ref s, _) if s == "Key1,Key2,Key3"));
		}
		other => panic!("expected Split CfnValue, got {:?}", other),
	}
}

#[test]
fn test_split_malformed_wrong_arg_count() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags: !Split [","]
      BucketEncryption: enabled
"#;

	let result = CfnTemplate::from_yaml(text, &test_uri());
	assert!(result.is_err());
	let diags = result.unwrap_err();
	assert!(diags.iter().any(|d| d.message.contains("Malformed !Split")));
}

// ===== Cidr Tests =====

#[test]
fn yaml_cidr_basic() {
	let text = r#"
Resources:
  MyVPC:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: !Select [0, !Cidr ["10.0.0.0/16", 6, 8]]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let vpc = &template.resources["MyVPC"];
	let cidr = vpc.properties.get("CidrBlock").unwrap();

	match cidr {
		(CfnValue::Select { list, .. }, _) => {
			if let CfnValue::Cidr {
				ip_block,
				count,
				cidr_bits,
				..
			} = &**list
			{
				assert!(matches!(**ip_block, CfnValue::String(ref s, _) if s == "10.0.0.0/16"));
				assert!(matches!(**count, CfnValue::Number(n, _) if n == 6.0));
				assert!(matches!(**cidr_bits, CfnValue::Number(n, _) if n == 8.0));
			} else {
				panic!("expected Cidr inside Select");
			}
		}
		other => panic!("expected Select with Cidr, got {:?}", other),
	}
}

#[test]
fn yaml_cidr_with_getatt() {
	let text = r#"
Resources:
  MyVPC:
    Type: AWS::EC2::VPC
  MySubnet:
    Type: AWS::EC2::Subnet
    Properties:
      CidrBlock: !Select [0, !Cidr [!GetAtt MyVPC.CidrBlock, 4, 8]]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let subnet = &template.resources["MySubnet"];
	let cidr = subnet.properties.get("CidrBlock").unwrap();

	match cidr {
		(CfnValue::Select { list, .. }, _) => {
			if let CfnValue::Cidr { ip_block, .. } = &**list {
				assert!(matches!(**ip_block, CfnValue::GetAtt { .. }));
			} else {
				panic!("expected Cidr inside Select");
			}
		}
		other => panic!("expected Select with Cidr, got {:?}", other),
	}
}

#[test]
fn json_cidr() {
	let text = r#"{
  "Resources": {
    "MyVPC": {
      "Type": "AWS::EC2::VPC",
      "Properties": {
        "CidrBlock": {
          "Fn::Select": [
            0,
            {"Fn::Cidr": ["10.0.0.0/16", 6, 8]}
          ]
        }
      }
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();
	let vpc = &template.resources["MyVPC"];
	let cidr = vpc.properties.get("CidrBlock").unwrap();

	match cidr {
		(CfnValue::Select { list, .. }, _) => {
			assert!(matches!(**list, CfnValue::Cidr { .. }));
		}
		other => panic!("expected Select with Cidr, got {:?}", other),
	}
}

#[test]
fn test_cidr_malformed_wrong_arg_count() {
	let text = r#"
Resources:
  MyVPC:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: !Cidr ["10.0.0.0/16", 6]
"#;

	let result = CfnTemplate::from_yaml(text, &test_uri());
	assert!(result.is_err());
	let diags = result.unwrap_err();
	assert!(diags.iter().any(|d| d.message.contains("Malformed !Cidr")));
}

// ===== ImportValue Tests =====

#[test]
fn yaml_import_value_string() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !ImportValue "shared-bucket-name"
      BucketEncryption: enabled
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(CfnValue::ImportValue { name, .. }, _) => {
			assert!(matches!(**name, CfnValue::String(ref s, _) if s == "shared-bucket-name"));
		}
		other => panic!("expected ImportValue CfnValue, got {:?}", other),
	}
}

#[test]
fn yaml_import_value_with_sub() {
	let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !ImportValue
        Fn::Sub: "${NetworkStackName}-BucketName"
      BucketEncryption: enabled
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(CfnValue::ImportValue { name, .. }, _) => {
			assert!(matches!(**name, CfnValue::Sub { .. }));
		}
		other => panic!("expected ImportValue with Sub, got {:?}", other),
	}
}

#[test]
fn json_import_value() {
	let text = r#"{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "BucketName": {
          "Fn::ImportValue": "shared-bucket-name"
        }
      }
    }
  }
}"#;

	let template = CfnTemplate::from_json(text, &test_uri()).unwrap();
	let bucket = &template.resources["MyBucket"];
	let bucket_name = bucket.properties.get("BucketName").unwrap();

	match bucket_name {
		(CfnValue::ImportValue { name, .. }, _) => {
			assert!(matches!(**name, CfnValue::String(ref s, _) if s == "shared-bucket-name"));
		}
		other => panic!("expected ImportValue CfnValue, got {:?}", other),
	}
}

// ===== Combined/Integration Tests =====

#[test]
fn test_complex_nesting_with_new_intrinsics() {
	let text = r#"
Resources:
  MyVPC:
    Type: AWS::EC2::VPC
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      # Base64 wrapping Sub
      UserData: !Base64
        Fn::Sub: |
          #!/bin/bash
          REGION=${AWS::Region}
      # Split a comma-delimited list
      SecurityGroups: !Split [",", !ImportValue "shared-security-groups"]
      # Cidr for subnet calculation
      SubnetId: !Select [0, !Cidr [!GetAtt MyVPC.CidrBlock, 3, 8]]
"#;

	let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
	let instance = &template.resources["MyInstance"];

	// Verify all three new intrinsics are parsed correctly
	assert!(instance.properties.contains_key("UserData"));
	assert!(instance.properties.contains_key("SecurityGroups"));
	assert!(instance.properties.contains_key("SubnetId"));

	// UserData should be Base64
	match &instance.properties["UserData"] {
		(CfnValue::Base64 { .. }, _) => {}
		other => panic!("expected Base64, got {:?}", other),
	}

	// SecurityGroups should be Split with ImportValue
	match &instance.properties["SecurityGroups"] {
		(CfnValue::Split { source, .. }, _) => {
			assert!(matches!(**source, CfnValue::ImportValue { .. }));
		}
		other => panic!("expected Split with ImportValue, got {:?}", other),
	}

	// SubnetId should be Select with Cidr
	match &instance.properties["SubnetId"] {
		(CfnValue::Select { list, .. }, _) => {
			assert!(matches!(**list, CfnValue::Cidr { .. }));
		}
		other => panic!("expected Select with Cidr, got {:?}", other),
	}
}
