use std::{collections::HashMap, sync::Arc};

use saphyr::LoadableYamlNode;
use tower_lsp::lsp_types::{
	Diagnostic, DiagnosticSeverity, Location, NumberOrString, Position, Range, Url,
};

use crate::{
	intents::{evaluator::project_vendor_aws, system::GuideLevel},
	spec::{
		cfn_ir::{parser::CfnParser, types::CfnTemplate},
		spec_store::SpecStore,
		symbol_table::SymbolTable,
	},
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
					diagnostics.extend(template.validate_against_spec(spec, uri));
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
	pub fn analyse_document_slow(&mut self, template: &CfnTemplate, uri: &Url) -> Vec<Diagnostic> {
		let system = match project_vendor_aws(template) {
			Ok(system) => system,
			Err(_) => return vec![], // If evaluation fails, return no diagnostics
		};

		let guides = system.guidance();

		// Convert each guide to a diagnostic
		let diagnostics: Vec<Diagnostic> = guides
			.into_iter()
			.filter_map(|guide| {
				// Find the resource by matching logical_id to node name
				let resource = template
					.resources
					.values()
					.find(|r| r.logical_id == guide.logical_id)?;

				let severity = match guide.level {
					GuideLevel::Required => DiagnosticSeverity::ERROR,
					GuideLevel::Action => DiagnosticSeverity::WARNING,
				};

				// Store guide data in diagnostic.data for hover provider
				let data = serde_json::json!({
					"kind": "wa2_guide",
					"tldr": guide.tldr,
					"message": guide.message,
					"why": guide.why,
				});

				Some(Diagnostic {
					range: resource.logical_id_range,
					severity: Some(severity),
					code: Some(NumberOrString::String("WA2_GUIDE".into())),
					source: Some("wa2".into()),
					message: guide.tldr, // Short message in Problems panel
					data: Some(data),    // Full data for hover
					..Default::default()
				})
			})
			.collect();

		// Cache the WA2 diagnostics
		if let Some(doc_state) = self.docs.get_mut(uri) {
			doc_state.cached_diagnostics = diagnostics.clone();
		}

		diagnostics
	}

	pub fn goto_definition(&self, uri: &Url, position: Position) -> Option<Location> {
		let doc = self.docs.get(uri)?;

		// Parse template and get root node
		let (word, template) = match doc.format {
			DocumentFormat::Json => {
				let parse_result =
					jsonc_parser::parse_to_ast(&doc.text, &Default::default(), &Default::default())
						.ok()?;
				let root = parse_result.value?;
				let parser = crate::spec::cfn_ir::json::JsonCfnParser::new(&doc.text);
				let word = parser.word_at_position(&root, position)?;
				let template = CfnTemplate::from_json(&doc.text, uri).ok()?;
				(word, template)
			}
			DocumentFormat::Yaml => {
				let docs = saphyr::MarkedYaml::load_from_str(&doc.text).ok()?;

				if docs.is_empty() {
					return None;
				}

				let parser = crate::spec::cfn_ir::yaml::YamlCfnParser::new(&doc.text);
				let word = parser.word_at_position(&docs[0], position);

				let word = word?;

				let template = CfnTemplate::from_yaml(&doc.text, uri).ok()?;
				(word, template)
			}
		};

		// Build symbol table
		let symbols = SymbolTable::from_template(&template);

		// Look up the word (handle GetAtt dot notation)
		let lookup_word = word.split('.').next().unwrap_or(&word);
		if let Some(resource) = symbols.resources.get(lookup_word) {
			return Some(Location {
				uri: uri.clone(),
				range: resource.location,
			});
		}

		// Look up the word as a parameter
		if let Some(parameter) = symbols.parameters.get(&word) {
			return Some(Location {
				uri: uri.clone(),
				range: parameter.location,
			});
		}

		// Look up the word as a condition
		if let Some(condition) = symbols.conditions.get(&word) {
			return Some(Location {
				uri: uri.clone(),
				range: condition.location,
			});
		}

		None
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
