use std::{collections::HashMap, sync::Arc};

use tower_lsp::lsp_types::{
	Diagnostic, DiagnosticSeverity, Location, NumberOrString, Position, Range, Url,
};

use crate::{
	intents::{
		guidance::{GuideLevel, guidance},
		model::Model,
		vendor::{Method, Vendor, get_projector},
	},
	spec::{cfn_ir::types::CfnTemplate, spec_store::SpecStore},
};

#[derive(Debug, Clone, Copy)]
enum DocumentFormat {
	Yaml,
	Json,
}

impl DocumentFormat {
	fn from_language_id_or_path(language_id: Option<&str>, uri: &Url) -> Self {
		match language_id {
			Some("cloudformation-json") => DocumentFormat::Json,
			Some("cloudformation-yaml") => DocumentFormat::Yaml,
			_ => {
				// Fallback to extension
				let path = uri.path();
				if path.ends_with(".json") {
					DocumentFormat::Json
				} else {
					DocumentFormat::Yaml
				}
			}
		}
	}
}

/// per-document state held by the core engine
struct DocumentState {
	text: String,
	format: DocumentFormat,
	cached_diagnostics: Vec<Diagnostic>,
	cached_model: Option<Model>,
}

/// core engine: owns all document state and analysis logic
pub struct CoreEngine {
	docs: HashMap<Url, DocumentState>,
	spec: Option<Arc<SpecStore>>,
}

impl Default for CoreEngine {
	fn default() -> Self {
		Self::new()
	}
}

impl CoreEngine {
	pub fn new() -> Self {
		Self {
			docs: HashMap::new(),
			spec: None,
		}
	}

	pub fn set_spec_store(&mut self, spec: Arc<SpecStore>) {
		self.spec = Some(spec);
	}

	pub fn spec_store(&self) -> Option<&SpecStore> {
		self.spec.as_deref()
	}

	pub fn on_open(&mut self, uri: Url, text: String, language_id: String) {
		let format = DocumentFormat::from_language_id_or_path(Some(&language_id), &uri);
		self.docs.insert(
			uri,
			DocumentState {
				text,
				format,
				cached_diagnostics: vec![],
				cached_model: None,
			},
		);
	}

	pub fn on_change(&mut self, uri: Url, new_text: String) {
		// For changes, we keep the existing format or detect from URI
		let format = self
			.docs
			.get(&uri)
			.map(|d| d.format)
			.unwrap_or_else(|| DocumentFormat::from_language_id_or_path(None, &uri));

		self.docs.insert(
			uri,
			DocumentState {
				text: new_text,
				format,
				cached_diagnostics: vec![],
				cached_model: None,
			},
		);
	}

	pub fn on_save(&mut self, _uri: &Url) {}

	pub fn analyse_document_fast(&mut self, uri: &Url) -> Result<CfnTemplate, Vec<Diagnostic>> {
		// always reset the cache so not stale
		if let Some(doc_state) = self.docs.get_mut(uri) {
			doc_state.cached_diagnostics.clear();
		}

		let doc = self.docs.get(uri).expect("document must have a uri");
		let text = &doc.text;

		let parse_result = match doc.format {
			DocumentFormat::Json => CfnTemplate::from_json(text, uri),
			DocumentFormat::Yaml => CfnTemplate::from_yaml(text, uri),
		};

		match parse_result {
			Ok(template) => {
				// Quick check - does this look like CloudFormation?
				if template.resources.is_empty() && !text.contains("AWSTemplateFormatVersion") {
					return Ok(template); // Not CloudFormation, ignore silently
				}

				let mut diagnostics = Vec::new();

				// Check for missing Resources section (warning only)
				if template.resources.is_empty() {
					diagnostics.push(Diagnostic {
						range: Range {
							start: Position {
								line: 0,
								character: 0,
							},
							end: Position {
								line: 0,
								character: 1,
							},
						},
						severity: Some(DiagnosticSeverity::WARNING),
						code: Some(NumberOrString::String("WA2_CFN_RESOURCES_MISSING".into())),
						source: Some("wa2-lsp".into()),
						message: "Template has no top-level `Resources` section; \
		           most CloudFormation templates define at least one resource."
							.to_string(),
						..Default::default()
					});
				}

				// Validate against spec if available
				if let Some(spec) = self.spec_store() {
					diagnostics.extend(template.validate_against_spec(spec));
				}

				if diagnostics.is_empty() {
					Ok(template)
				} else {
					Err(diagnostics)
				}
			}
			Err(diagnostics) => {
				if let Some(doc_state) = self.docs.get_mut(uri) {
					doc_state.cached_diagnostics = diagnostics.clone();
				}
				Err(diagnostics)
			}
		}
	}

	/// Convert WA2 guidance into LSP diagnostics
	pub fn analyse_document_slow(&mut self, uri: &Url) -> Vec<Diagnostic> {
		let doc = match self.docs.get(uri) {
			Some(d) => d,
			None => return vec![],
		};

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);

		let model = match projector.project(&doc.text, uri) {
			Ok(result) => result.model,
			Err(diags) => return diags,
		};

		let guides = guidance(&model);

		let diagnostics: Vec<Diagnostic> = guides
			.into_iter()
			.filter_map(|guide| {
				// We need the template to get ranges - but we can get them from the model now
				let range = model.get_range(guide.entity)?;

				let severity = match guide.level {
					GuideLevel::Required => DiagnosticSeverity::ERROR,
					GuideLevel::Action => DiagnosticSeverity::WARNING,
				};

				let data = serde_json::json!({
					"kind": "wa2_guide",
					"tldr": guide.tldr,
					"message": guide.message,
					"why": guide.why,
				});

				Some(Diagnostic {
					range,
					severity: Some(severity),
					code: Some(NumberOrString::String("WA2_GUIDE".into())),
					source: Some("wa2".into()),
					message: guide.tldr,
					data: Some(data),
					..Default::default()
				})
			})
			.collect();

		// Cache both diagnostics and model
		if let Some(doc_state) = self.docs.get_mut(uri) {
			doc_state.cached_diagnostics = diagnostics.clone();
			doc_state.cached_model = Some(model);
		}

		diagnostics
	}

	pub fn goto_definition(&self, uri: &Url, position: Position) -> Option<Location> {
		let doc = self.docs.get(uri)?;

		// Get cached model or project on demand
		let model = match &doc.cached_model {
			Some(m) => m.clone(),
			None => {
				let projector = get_projector(Vendor::Aws, Method::CloudFormation);
				projector.project(&doc.text, uri).ok()?.model
			}
		};

		// Find entity at cursor position
		let entity = model.entity_at_position(position)?;

		// Resolve cfn types
		let cfn_ref = model.resolve("cfn:Ref")?;
		let cfn_getatt = model.resolve("cfn:GetAtt")?;
		let cfn_sub = model.resolve("cfn:Sub")?;
		let cfn_sub_var_ref = model.resolve("cfn:SubVarRef"); // May not exist in older models
		let cfn_target = model.resolve("cfn:target")?;

		// If entity is a Ref, GetAtt, Sub, or SubVarRef, follow cfn:target to definition
		let is_navigable = model.has_type(entity, cfn_ref)
			|| model.has_type(entity, cfn_getatt)
			|| model.has_type(entity, cfn_sub)
			|| cfn_sub_var_ref.map_or(false, |t| model.has_type(entity, t));

		if is_navigable {
			let targets = model.get_all(entity, cfn_target);
			let target = targets.first()?.as_entity()?;
			let range = model.get_range(target)?;
			return Some(Location {
				uri: uri.clone(),
				range,
			});
		}

		// Entity itself has a definition location
		let range = model.get_range(entity)?;
		Some(Location {
			uri: uri.clone(),
			range,
		})
	}

	pub fn get_hover_content(&self, uri: &Url, position: Position) -> Option<String> {
		let doc = self.docs.get(uri)?;

		// Find diagnostic at this position
		for diag in &doc.cached_diagnostics {
			if position_in_range(position, diag.range) {
				// Extract guide data from diagnostic
				if let Some(data) = &diag.data
					&& let Ok(obj) = serde_json::from_value::<
						serde_json::Map<String, serde_json::Value>,
					>(data.clone()) && obj.get("kind").and_then(|v| v.as_str()) == Some("wa2_guide")
				{
					let tldr = obj.get("tldr").and_then(|v| v.as_str()).unwrap_or("");
					let message = obj.get("message").and_then(|v| v.as_str()).unwrap_or("");
					let why = obj.get("why").and_then(|v| v.as_str()).unwrap_or("");

					// Format as Markdown
					return Some(format!("## {}\n\n{}\n\n**Why?** {}", tldr, message, why));
				}
			}
		}

		None
	}
}

fn position_in_range(pos: Position, range: Range) -> bool {
	if pos.line < range.start.line || pos.line > range.end.line {
		return false;
	}
	if pos.line == range.start.line && pos.character < range.start.character {
		return false;
	}
	if pos.line == range.end.line && pos.character > range.end.character {
		return false;
	}
	true
}

#[cfg(test)]
mod spec_tests {

	use super::*;
	use crate::spec::spec_store::SpecStore;
	use std::sync::Arc;
	use tower_lsp::lsp_types::DiagnosticSeverity;

	fn uri(path: &str) -> Url {
		Url::parse(path).expect("valid URI")
	}

	/// Helper to create a minimal SpecStore for testing using JSON parsing
	fn create_test_spec() -> Arc<SpecStore> {
		let json = r#"{
        "ResourceTypes": {
            "AWS::S3::Bucket": {
                "Properties": {
                    "BucketName": {
                        "PrimitiveType": "String",
                        "Required": false
                    },
                    "BucketEncryption": {
                        "PrimitiveType": "String",
                        "Required": true
                    },
                    "Tags": {
                        "Type": "List",
                        "PrimitiveItemType": "String",
                        "Required": false
                    }
                }
            },
            "AWS::Lambda::Function": {
                "Properties": {
                    "FunctionName": {
                        "PrimitiveType": "String",
                        "Required": false
                    },
                    "Code": {
                        "PrimitiveType": "String",
                        "Required": true
                    },
                    "Runtime": {
                        "PrimitiveType": "String",
                        "Required": true
                    }
                }
            }
        }
    }"#;

		Arc::new(SpecStore::from_json_bytes(json.as_bytes()).unwrap())
	}

	#[test]
	fn spec_unknown_resource_type() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyResource:
    Type: AWS::FakeService::FakeResource
    Properties:
      SomeProp: value
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_UNKNOWN_RESOURCE_TYPE".into()
			))
		);
		assert!(d.message.contains("AWS::FakeService::FakeResource"));
		assert!(d.message.contains("MyResource"));
	}

	#[test]
	fn spec_unknown_property() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      InvalidProperty: value
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
		assert_eq!(
			d.code,
			Some(NumberOrString::String("WA2_CFN_UNKNOWN_PROPERTY".into()))
		);
		assert!(d.message.contains("InvalidProperty"));
		assert!(d.message.contains("MyBucket"));
		assert!(d.message.contains("AWS::S3::Bucket"));
	}

	#[test]
	fn spec_missing_required_property() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: my-bucket
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_REQUIRED_PROPERTY_MISSING".into()
			))
		);
		assert!(d.message.contains("BucketEncryption"));
		assert!(d.message.contains("MyBucket"));
	}

	#[test]
	fn spec_multiple_issues_one_resource() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      InvalidProperty: value
      AnotherBadProp: value2
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// Should have: missing BucketEncryption + 2 unknown properties = 3 diagnostics
		assert_eq!(diags.len(), 3);

		let unknown_count = diags
			.iter()
			.filter(|d| d.code == Some(NumberOrString::String("WA2_CFN_UNKNOWN_PROPERTY".into())))
			.count();
		assert_eq!(unknown_count, 2);

		let missing_count = diags
			.iter()
			.filter(|d| {
				d.code
					== Some(NumberOrString::String(
						"WA2_CFN_REQUIRED_PROPERTY_MISSING".into(),
					))
			})
			.count();
		assert_eq!(missing_count, 1);
	}

	#[test]
	fn spec_multiple_resources_different_issues() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      InvalidProperty: value
  MyFunction:
    Type: AWS::Lambda::Function
    Properties:
      FunctionName: my-func
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// MyBucket: missing BucketEncryption + unknown property = 2
		// MyFunction: missing Code + missing Runtime = 2
		// Total = 4
		assert_eq!(diags.len(), 4);

		let bucket_diags: Vec<_> = diags
			.iter()
			.filter(|d| d.message.contains("MyBucket"))
			.collect();
		assert_eq!(bucket_diags.len(), 2);

		let function_diags: Vec<_> = diags
			.iter()
			.filter(|d| d.message.contains("MyFunction"))
			.collect();
		assert_eq!(function_diags.len(), 2);
	}

	#[test]
	fn spec_valid_resource_all_required_properties() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let _template = engine.analyse_document_fast(&uri).unwrap();
	}

	#[test]
	fn spec_valid_resource_with_optional_properties() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      BucketName: my-bucket
      Tags:
        - Key: Environment
          Value: Production
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let _template = engine.analyse_document_fast(&uri).unwrap();
	}

	#[test]
	fn spec_resource_key_not_string() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		// YAML allows non-string keys, but CFN doesn't
		let text = r#"
Resources:
  123:
    Type: AWS::S3::Bucket
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_KEY_NOT_STRING".into()
			))
		);
	}

	#[test]
	fn spec_resource_not_mapping() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket: just-a-string
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_NOT_MAPPING".into()
			))
		);
		assert!(d.message.contains("MyBucket"));
	}

	#[test]
	fn spec_type_not_string() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: 123
    Properties:
      BucketName: test
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// IR conversion skips non-string Type, so we see missing Type
		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_TYPE_MISSING".into()
			))
		);
	}

	#[test]
	fn spec_type_missing() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Properties:
      BucketName: test
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_TYPE_MISSING".into()
			))
		);
		assert!(d.message.contains("MyBucket"));
	}

	#[test]
	fn spec_properties_not_mapping() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties: just-a-string
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// IR conversion skips non-mapping Properties, so we just see missing required property
		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_REQUIRED_PROPERTY_MISSING".into()
			))
		);
	}

	#[test]
	fn spec_property_key_not_string() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		// YAML technically allows this but it's invalid for CFN
		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      123: value
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let _template = engine.analyse_document_fast(&uri).unwrap();
	}

	#[test]
	fn spec_resource_with_no_properties_section() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		// Some resources might have all optional properties
		// but our S3::Bucket has required BucketEncryption
		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// Now correctly detects missing required property
		// (This is the correct behavior - the old code had a bug)
		assert_eq!(diags.len(), 1);
		assert!(diags[0].message.contains("missing required property"));
		assert!(diags[0].message.contains("BucketEncryption"));
	}

	#[test]
	fn spec_multiple_missing_required_properties() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyFunction:
    Type: AWS::Lambda::Function
    Properties:
      FunctionName: my-func
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// Missing Code and Runtime
		assert_eq!(diags.len(), 2);
		assert!(
			diags
				.iter()
				.all(|d| d.severity == Some(DiagnosticSeverity::ERROR))
		);
		assert!(diags.iter().all(|d| d.code
			== Some(NumberOrString::String(
				"WA2_CFN_REQUIRED_PROPERTY_MISSING".into()
			))));

		let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
		assert!(messages.iter().any(|m| m.contains("Code")));
		assert!(messages.iter().any(|m| m.contains("Runtime")));
	}

	#[test]
	fn spec_all_valid_resources_no_errors() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  MyBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
      BucketName: test-bucket
  MyFunction:
    Type: AWS::Lambda::Function
    Properties:
      Code: 
        ZipFile: "code"
      Runtime: python3.9
      FunctionName: test-function
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let _template = engine.analyse_document_fast(&uri).unwrap();
	}

	#[test]
	fn spec_mixed_valid_and_invalid_resources() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Resources:
  ValidBucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketEncryption: enabled
  InvalidFunction:
    Type: AWS::Lambda::Function
    Properties:
      FunctionName: test
      InvalidProp: value
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		// ValidBucket should be fine
		// InvalidFunction: missing Code, missing Runtime, unknown InvalidProp = 3 diagnostics
		assert_eq!(diags.len(), 3);

		let valid_bucket_diags: Vec<_> = diags
			.iter()
			.filter(|d| d.message.contains("ValidBucket"))
			.collect();
		assert_eq!(valid_bucket_diags.len(), 0);

		let invalid_function_diags: Vec<_> = diags
			.iter()
			.filter(|d| d.message.contains("InvalidFunction"))
			.collect();
		assert_eq!(invalid_function_diags.len(), 3);
	}

	#[test]
	fn span_points_to_resource_logical_id() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"Resources:
  MyBucket:
    Type: AWS::FakeType
    Properties:
      BucketName: test
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];

		// With saphyr markers, we now point to the Type VALUE, not the key
		// "AWS::FakeType" starts at column 10 on line 2
		assert_eq!(
			d.range.start.line, 2,
			"diagnostic points to Type value line"
		);
		assert_eq!(d.range.start.character, 10, "should start at Type value");
	}

	#[test]
	fn span_points_to_type_field() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"Resources:
  MyBucket:
    Type: AWS::FakeType
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];

		// Points to the Type value on line 2
		assert_eq!(d.range.start.line, 2, "diagnostic should be on line 2");
		assert_eq!(d.range.start.character, 10, "points to Type value");
	}

	#[test]
	fn span_disambiguates_similar_resource_names() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		// Test that "Bucket" and "BucketPolicy" don't confuse the span resolver
		let text = r#"Resources:
  Bucket:
    Type: AWS::FakeType1
  BucketPolicy:
    Type: AWS::FakeType2
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 2);

		// Both diagnostics point to Type values on lines 2 and 4
		let diag_lines: Vec<u32> = diags.iter().map(|d| d.range.start.line).collect();
		assert!(diag_lines.contains(&2), "Should have diagnostic on line 2");
		assert!(diag_lines.contains(&4), "Should have diagnostic on line 4");
	}

	#[test]
	fn span_handles_indented_yaml() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.yaml");

		// Varying indentation levels
		let text = r#"Resources:
    MyBucket:
        Type: AWS::FakeType
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];

		// Points to Type value (line 2) with extra indentation
		// "AWS::FakeType" starts at column 14 (8 spaces + "Type: " = 14)
		assert_eq!(d.range.start.line, 2);
		assert_eq!(
			d.range.start.character, 14,
			"should point to Type value with extra indentation"
		);
	}

	#[test]
	fn json_unknown_resource_type() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.json");

		let text = r#"{
  "Resources": {
    "MyResource": {
      "Type": "AWS::FakeService::FakeResource",
      "Properties": {
        "SomeProp": "value"
      }
    }
  }
}"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-json".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		let d = &diags[0];
		assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
		assert_eq!(
			d.code,
			Some(NumberOrString::String(
				"WA2_CFN_UNKNOWN_RESOURCE_TYPE".into()
			))
		);
		assert!(d.message.contains("AWS::FakeService::FakeResource"));
	}

	#[test]
	fn json_missing_required_property() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.json");

		let text = r#"{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "BucketName": "my-bucket"
      }
    }
  }
}"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-json".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		assert!(diags[0].message.contains("BucketEncryption"));
	}

	#[test]
	fn json_valid_resource() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.json");

		let text = r#"{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
        "BucketEncryption": "enabled"
      }
    }
  }
}"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-json".to_string(),
		);
		let _template = engine.analyse_document_fast(&uri).unwrap();
	}

	#[test]
	fn json_parse_error() {
		let mut engine = CoreEngine::new();
		engine.set_spec_store(create_test_spec());
		let uri = uri("file:///tmp/test.json");

		let text = r#"{
  "Resources": {
    "MyBucket": {
      "Type": "AWS::S3::Bucket",
      "Properties": {
    }
  }
}"#; // Unclosed brace - definitely invalid

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-json".to_string(),
		);
		let diags = engine.analyse_document_fast(&uri).unwrap_err();

		assert_eq!(diags.len(), 1);
		assert_eq!(
			diags[0].code,
			Some(NumberOrString::String("WA2_JSON_PARSE".into()))
		);
	}
}

#[cfg(test)]
mod goto_definition_tests {
	use super::*;

	fn uri(path: &str) -> Url {
		Url::parse(path).expect("valid URI")
	}

	fn test_uri() -> Url {
		Url::parse("file:///tmp/test.yaml").unwrap()
	}

	#[test]
	fn goto_def_ref_to_resource() {
		let mut engine = CoreEngine::new();
		let uri = uri("file:///tmp/test.yaml");

		// Line numbers (0-indexed):
		// 0: (empty)
		// 1: Resources:
		// 2:   DataBucket:
		// 3:     Type: AWS::S3::Bucket
		// 4:   Consumer:
		// 5:     Type: AWS::Lambda::Function
		// 6:     Properties:
		// 7:       BucketName: !Ref DataBucket
		let text = r#"
Resources:
  DataBucket:
    Type: AWS::S3::Bucket
  Consumer:
    Type: AWS::Lambda::Function
    Properties:
      BucketName: !Ref DataBucket
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);

		// Position cursor on "DataBucket" in the !Ref (line 7, somewhere in "DataBucket")
		let position = Position {
			line: 7,
			character: 25,
		};

		let result = engine.goto_definition(&uri, position);

		eprintln!("goto_definition result: {:?}", result);

		assert!(result.is_some(), "Should find definition");
		let location = result.unwrap();
		assert_eq!(location.uri, uri);
		// Should point to line 2 where DataBucket is defined
		assert_eq!(
			location.range.start.line, 2,
			"Should jump to DataBucket definition"
		);
	}

	#[test]
	fn goto_def_ref_to_parameter() {
		let mut engine = CoreEngine::new();
		let uri = uri("file:///tmp/test.yaml");

		let text = r#"
Parameters:
  Environment:
    Type: String

Resources:
  Bucket:
    Type: AWS::S3::Bucket
    Properties:
      BucketName: !Ref Environment
"#;

		engine.on_open(
			uri.clone(),
			text.to_string(),
			"cloudformation-yaml".to_string(),
		);

		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
			engine.analyse_document_slow(&uri);
		}

		// Position on "Environment" in !Ref
		// Line 9: `      BucketName: !Ref Environment`
		// "Environment" starts around char 28
		let position = Position {
			line: 9,
			character: 30,
		};

		let result = engine.goto_definition(&uri, position);

		eprintln!("goto_definition result: {:?}", result);

		assert!(result.is_some(), "Should find parameter definition");
		let location = result.unwrap();
		// Should point to line 2 where Environment parameter is defined
		assert_eq!(
			location.range.start.line, 2,
			"Should jump to Environment parameter"
		);
	}

	// 	#[test]
	// 	fn goto_def_debug_getatt_ranges() {
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"
	// Resources:
	//   MyRole:
	//     Type: AWS::IAM::Role
	//     Properties:
	//       AssumeRolePolicyDocument: {}
	//   Bucket:
	//     Type: AWS::S3::Bucket
	//     Properties:
	//       RoleArn: !GetAtt MyRole.Arn
	// "#;

	// 		let template = CfnTemplate::from_yaml(text, &uri).expect("parse");
	// 		let model = project_vendor_aws(&template).expect("project");

	// 		eprintln!("\n=== All entities with source ranges (GetAtt test) ===");
	// 		for i in 0..model.entity_count() {
	// 			let eid = crate::intents::model::EntityId(i as u32);
	// 			if let Some(range) = model.get_range(eid) {
	// 				let name = model.qualified_name(eid);
	// 				let types = model.types(eid);
	// 				let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 				eprintln!("  {} : {:?} @ {:?}", name, type_names, range);
	// 			}
	// 		}

	// 		// Check cfn:GetAtt type exists
	// 		if let Some(cfn_getatt) = model.resolve("cfn:GetAtt") {
	// 			eprintln!("\ncfn:GetAtt type: {:?}", cfn_getatt);
	// 		} else {
	// 			eprintln!("\ncfn:GetAtt: NOT FOUND");
	// 		}

	// 		// Print line 9 for reference
	// 		let lines: Vec<&str> = text.lines().collect();
	// 		eprintln!("\nLine 9: '{}'", lines.get(9).unwrap_or(&"<none>"));

	// 		// Try entity_at_position
	// 		let getatt_position = Position {
	// 			line: 9,
	// 			character: 20,
	// 		};
	// 		eprintln!("\n=== entity_at_position({:?}) ===", getatt_position);
	// 		if let Some(entity) = model.entity_at_position(getatt_position) {
	// 			let name = model.qualified_name(entity);
	// 			eprintln!("  Found: {}", name);
	// 		} else {
	// 			eprintln!("  No entity found at position");
	// 		}
	// 	}

	// 	#[test]
	// 	fn goto_def_getatt_to_resource() {
	// 		let mut engine = CoreEngine::new();
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"
	// Resources:
	//   MyRole:
	//     Type: AWS::IAM::Role
	//     Properties:
	//       AssumeRolePolicyDocument: {}
	//   Bucket:
	//     Type: AWS::S3::Bucket
	//     Properties:
	//       RoleArn: !GetAtt MyRole.Arn
	// "#;

	// 		engine.on_open(
	// 			uri.clone(),
	// 			text.to_string(),
	// 			"cloudformation-yaml".to_string(),
	// 		);

	// 		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
	// 			engine.analyse_document_slow(&template, &uri);
	// 		}

	// 		// Line 9: '      RoleArn: !GetAtt MyRole.Arn'
	// 		// GetAtt range is char 23-33 (just "MyRole.Arn" not "!GetAtt")
	// 		// Position must be INSIDE that range
	// 		let position = Position {
	// 			line: 9,
	// 			character: 25,
	// 		};

	// 		let result = engine.goto_definition(&uri, position);

	// 		eprintln!("goto_definition result: {:?}", result);

	// 		assert!(result.is_some(), "Should find resource definition");
	// 		let location = result.unwrap();
	// 		assert_eq!(
	// 			location.range.start.line, 2,
	// 			"Should jump to MyRole definition"
	// 		);
	// 	}

	// 	#[test]
	// 	fn goto_def_debug_parameter_ranges() {
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"
	// Parameters:
	//   Environment:
	//     Type: String

	// Resources:
	//   Bucket:
	//     Type: AWS::S3::Bucket
	//     Properties:
	//       BucketName: !Ref Environment
	// "#;

	// 		let template = CfnTemplate::from_yaml(text, &uri).expect("parse");

	// 		eprintln!("\n=== Parameters from parser ===");
	// 		for (name, param) in &template.parameters {
	// 			eprintln!("  {}: name_range={:?}", name, param.name_range);
	// 		}

	// 		let model = project_vendor_aws(&template).expect("project");

	// 		eprintln!("\n=== All entities with source ranges ===");
	// 		for i in 0..model.entity_count() {
	// 			let eid = crate::intents::model::EntityId(i as u32);
	// 			if let Some(range) = model.get_range(eid) {
	// 				let name = model.qualified_name(eid);
	// 				let types = model.types(eid);
	// 				let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 				eprintln!("  {} : {:?} @ {:?}", name, type_names, range);
	// 			}
	// 		}

	// 		// Check if Environment entity exists and what it's linked to
	// 		if let Some(env) = model.resolve("Environment") {
	// 			eprintln!("\nEnvironment entity: {:?}", env);
	// 			eprintln!(
	// 				"  types: {:?}",
	// 				model
	// 					.types(env)
	// 					.iter()
	// 					.map(|&t| model.qualified_name(t))
	// 					.collect::<Vec<_>>()
	// 			);
	// 			eprintln!("  range: {:?}", model.get_range(env));
	// 		} else {
	// 			eprintln!("\nEnvironment: NOT FOUND in model.resolve()");
	// 		}

	// 		// Find the Ref node and check its cfn:target
	// 		eprintln!("\n=== Checking cfn:Ref nodes ===");
	// 		if let Some(cfn_ref_type) = model.resolve("cfn:Ref") {
	// 			for i in 0..model.entity_count() {
	// 				let eid = crate::intents::model::EntityId(i as u32);
	// 				if model.has_type(eid, cfn_ref_type) {
	// 					eprintln!("  Found Ref node: {:?}", eid);
	// 					if let Some(cfn_target) = model.resolve("cfn:target") {
	// 						let targets = model.get_all(eid, cfn_target);
	// 						for target in &targets {
	// 							if let Some(target_id) = target.as_entity() {
	// 								eprintln!(
	// 									"    -> target: {} @ {:?}",
	// 									model.qualified_name(target_id),
	// 									model.get_range(target_id)
	// 								);
	// 							}
	// 						}
	// 					}
	// 				}
	// 			}
	// 		}
	// 	}

	// 	#[test]
	// 	fn goto_def_no_match_at_position() {
	// 		let mut engine = CoreEngine::new();
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"
	// Resources:
	//   Bucket:
	//     Type: AWS::S3::Bucket
	// "#;

	// 		engine.on_open(
	// 			uri.clone(),
	// 			text.to_string(),
	// 			"cloudformation-yaml".to_string(),
	// 		);

	// 		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
	// 			engine.analyse_document_slow(&template, &uri);
	// 		}

	// 		// Position on "Resources:" keyword - not an entity
	// 		let position = Position {
	// 			line: 1,
	// 			character: 5,
	// 		};

	// 		let result = engine.goto_definition(&uri, position);

	// 		eprintln!("goto_definition result: {:?}", result);

	// 		// Should return None - no navigable entity at this position
	// 		assert!(result.is_none(), "Should not find definition at keyword");
	// 	}

	// 	#[test]
	// 	fn goto_def_debug_model_ranges() {
	// 		// Debug test to see what ranges are actually stored
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"
	// Resources:
	//   DataBucket:
	//     Type: AWS::S3::Bucket
	//   Consumer:
	//     Type: AWS::Lambda::Function
	//     Properties:
	//       BucketName: !Ref DataBucket
	// "#;

	// 		let template = CfnTemplate::from_yaml(text, &uri).expect("parse");
	// 		let model = project_vendor_aws(&template).expect("project");

	// 		eprintln!("\n=== Model entities with ranges ===");

	// 		// Check what entities exist and their ranges
	// 		if let Some(data_bucket) = model.resolve("DataBucket") {
	// 			eprintln!("DataBucket entity: {:?}", data_bucket);
	// 			eprintln!("  range: {:?}", model.get_range(data_bucket));
	// 		} else {
	// 			eprintln!("DataBucket: NOT FOUND");
	// 		}

	// 		if let Some(consumer) = model.resolve("Consumer") {
	// 			eprintln!("Consumer entity: {:?}", consumer);
	// 			eprintln!("  range: {:?}", model.get_range(consumer));
	// 		} else {
	// 			eprintln!("Consumer: NOT FOUND");
	// 		}

	// 		// Check cfn:Ref type exists
	// 		if let Some(cfn_ref) = model.resolve("cfn:Ref") {
	// 			eprintln!("cfn:Ref type: {:?}", cfn_ref);
	// 		} else {
	// 			eprintln!("cfn:Ref: NOT FOUND - ensure_cfn_types not called?");
	// 		}

	// 		// Dump all source ranges
	// 		eprintln!("\n=== All entities with source ranges ===");
	// 		for i in 0..model.entity_count() {
	// 			let eid = crate::intents::model::EntityId(i as u32);
	// 			if let Some(range) = model.get_range(eid) {
	// 				let name = model.qualified_name(eid);
	// 				let types = model.types(eid);
	// 				let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 				eprintln!("  {} : {:?} @ {:?}", name, type_names, range);
	// 			}
	// 		}

	// 		// Try entity_at_position for the Ref location
	// 		let ref_position = Position {
	// 			line: 7,
	// 			character: 25,
	// 		};
	// 		eprintln!("\n=== entity_at_position({:?}) ===", ref_position);
	// 		if let Some(entity) = model.entity_at_position(ref_position) {
	// 			let name = model.qualified_name(entity);
	// 			let types = model.types(entity);
	// 			let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 			eprintln!("  Found: {} : {:?}", name, type_names);
	// 			eprintln!("  Range: {:?}", model.get_range(entity));
	// 		} else {
	// 			eprintln!("  No entity found at position");
	// 		}
	// 	}

	// 	#[test]
	// 	fn goto_def_ref_in_outputs() {
	// 		let mut engine = CoreEngine::new();
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"AWSTemplateFormatVersion: "2010-09-09"
	// Resources:
	//   DataBucket:
	//     Type: AWS::S3::Bucket
	// Outputs:
	//   DataBucketName:
	//     Value: !Ref DataBucket
	//   DataBucketArn:
	//     Value: !GetAtt DataBucket.Arn
	// "#;

	// 		engine.on_open(
	// 			uri.clone(),
	// 			text.to_string(),
	// 			"cloudformation-yaml".to_string(),
	// 		);

	// 		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
	// 			engine.analyse_document_slow(&template, &uri);
	// 		}

	// 		// Line 6: "    Value: !Ref DataBucket"
	// 		// !Ref starts around char 11, "DataBucket" around char 16
	// 		let position = Position {
	// 			line: 6,
	// 			character: 20,
	// 		};

	// 		let result = engine.goto_definition(&uri, position);
	// 		eprintln!("goto_definition result: {:?}", result);

	// 		assert!(result.is_some(), "Should find definition from Outputs Ref");
	// 	}

	// 	#[test]
	// 	fn goto_def_sub_variable_reference() {
	// 		let mut engine = CoreEngine::new();
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		// Line numbers (0-indexed):
	// 		// 0: Parameters:
	// 		// 1:   DataBucketName:
	// 		// 2:     Type: String
	// 		// 3: Resources:
	// 		// 4:   MyPolicy:
	// 		// 5:     Type: AWS::IAM::Policy
	// 		// 6:     Properties:
	// 		// 7:       Resource: !Sub "arn:${AWS::Partition}:s3:::${DataBucketName}/*"
	// 		let text = r#"Parameters:
	//   DataBucketName:
	//     Type: String
	// Resources:
	//   MyPolicy:
	//     Type: AWS::IAM::Policy
	//     Properties:
	//       Resource: !Sub "arn:${AWS::Partition}:s3:::${DataBucketName}/*"
	// "#;

	// 		engine.on_open(
	// 			uri.clone(),
	// 			text.to_string(),
	// 			"cloudformation-yaml".to_string(),
	// 		);

	// 		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
	// 			engine.analyse_document_slow(&template, &uri);
	// 		}

	// 		// Line 7: '      Resource: !Sub "arn:${AWS::Partition}:s3:::${DataBucketName}/*"'
	// 		// "DataBucketName" starts around character 51
	// 		let position = Position {
	// 			line: 7,
	// 			character: 53,
	// 		};

	// 		let result = engine.goto_definition(&uri, position);
	// 		eprintln!("goto_definition result: {:?}", result);

	// 		assert!(
	// 			result.is_some(),
	// 			"Should find definition for Sub variable reference"
	// 		);
	// 		let location = result.unwrap();
	// 		// Should point to line 1 where DataBucketName parameter is defined
	// 		assert_eq!(
	// 			location.range.start.line, 1,
	// 			"Should jump to DataBucketName parameter"
	// 		);
	// 	}

	// 	#[test]
	// 	fn goto_def_sub_pseudo_parameter() {
	// 		let mut engine = CoreEngine::new();
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"Resources:
	//   MyPolicy:
	//     Type: AWS::IAM::Policy
	//     Properties:
	//       Resource: !Sub "arn:${AWS::Partition}:s3:::mybucket/*"
	// "#;

	// 		engine.on_open(
	// 			uri.clone(),
	// 			text.to_string(),
	// 			"cloudformation-yaml".to_string(),
	// 		);

	// 		if let Ok(template) = CfnTemplate::from_yaml(text, &uri) {
	// 			engine.analyse_document_slow(&template, &uri);
	// 		}

	// 		// Line 4: '      Resource: !Sub "arn:${AWS::Partition}:s3:::mybucket/*"'
	// 		// "AWS::Partition" starts around character 27
	// 		let position = Position {
	// 			line: 4,
	// 			character: 30,
	// 		};

	// 		let result = engine.goto_definition(&uri, position);
	// 		eprintln!("goto_definition result: {:?}", result);

	// 		// Pseudo-parameters don't have source locations, so this might return None
	// 		// or could return the pseudo-parameter's synthetic location
	// 		// For now, just document the behavior
	// 		eprintln!("Note: Pseudo-parameters may not have goto_definition support");
	// 	}

	// 	#[test]
	// 	fn goto_def_debug_sub_var_refs() {
	// 		let uri = uri("file:///tmp/test.yaml");

	// 		let text = r#"Parameters:
	//   DataBucketName:
	//     Type: String
	// Resources:
	//   MyPolicy:
	//     Type: AWS::IAM::Policy
	//     Properties:
	//       Resource: !Sub "arn:${AWS::Partition}:s3:::${DataBucketName}/*"
	// "#;

	// 		// Print line 7 for reference
	// 		let lines: Vec<&str> = text.lines().collect();
	// 		eprintln!("\nLine 7: '{}'", lines.get(7).unwrap_or(&"<none>"));

	// 		// Print character positions
	// 		if let Some(line) = lines.get(7) {
	// 			eprintln!("Character positions:");
	// 			for (i, c) in line.chars().enumerate() {
	// 				if c == '$' || c == '{' || c == '}' {
	// 					eprintln!("  char {}: '{}'", i, c);
	// 				}
	// 			}
	// 		}

	// 		let template = CfnTemplate::from_yaml(text, &uri).expect("parse");
	// 		let model = project_vendor_aws(&template).expect("project");

	// 		eprintln!("\n=== All entities with source ranges ===");
	// 		for i in 0..model.entity_count() {
	// 			let eid = crate::intents::model::EntityId(i as u32);
	// 			if let Some(range) = model.get_range(eid) {
	// 				let name = model.qualified_name(eid);
	// 				let types = model.types(eid);
	// 				let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 				eprintln!("  {} : {:?} @ {:?}", name, type_names, range);
	// 			}
	// 		}

	// 		// Check for SubVarRef nodes specifically
	// 		eprintln!("\n=== SubVarRef nodes ===");
	// 		if let Some(sub_var_ref_type) = model.resolve("cfn:SubVarRef") {
	// 			for i in 0..model.entity_count() {
	// 				let eid = crate::intents::model::EntityId(i as u32);
	// 				if model.has_type(eid, sub_var_ref_type) {
	// 					eprintln!("  {:?} @ {:?}", eid, model.get_range(eid));
	// 					if let Some(cfn_target) = model.resolve("cfn:target") {
	// 						let targets = model.get_all(eid, cfn_target);
	// 						for target in &targets {
	// 							if let Some(target_id) = target.as_entity() {
	// 								eprintln!("    -> target: {}", model.qualified_name(target_id));
	// 							}
	// 						}
	// 					}
	// 				}
	// 			}
	// 		} else {
	// 			eprintln!("  cfn:SubVarRef type not found!");
	// 		}

	// 		// Test entity_at_position for the variable
	// 		let position = Position {
	// 			line: 7,
	// 			character: 53,
	// 		};
	// 		eprintln!("\n=== entity_at_position({:?}) ===", position);
	// 		if let Some(entity) = model.entity_at_position(position) {
	// 			let name = model.qualified_name(entity);
	// 			let types = model.types(entity);
	// 			let type_names: Vec<_> = types.iter().map(|&t| model.qualified_name(t)).collect();
	// 			eprintln!("  Found: {} : {:?}", name, type_names);
	// 			eprintln!("  Range: {:?}", model.get_range(entity));
	// 		} else {
	// 			eprintln!("  No entity found at position");
	// 		}
	// 	}
}
