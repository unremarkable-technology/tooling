use jsonc_parser::ParseOptions;
use jsonc_parser::ast::Value;
use jsonc_parser::common::Ranged;
use jsonc_parser::errors::ParseError;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range};
use url::Url;

use crate::iaac::cloudformation::cfn_ir::parser::{self, CfnParser, ObjectEntry};
use crate::iaac::cloudformation::cfn_ir::types::CfnTemplate;
use crate::iaac::cloudformation::intrinsics::{self, IntrinsicKind};

impl CfnTemplate {
	/// Parse a CloudFormation template from JSON text
	pub fn from_json(text: &str, uri: &Url) -> Result<Self, Vec<Diagnostic>> {
		let parse_options = ParseOptions {
			allow_comments: true,
			allow_trailing_commas: true,
			allow_loose_object_property_names: false,
			allow_hexadecimal_numbers: false,
			allow_single_quoted_strings: false,
			allow_unary_plus_numbers: false,
		};

		let parse_result = jsonc_parser::parse_to_ast(text, &Default::default(), &parse_options)
			.map_err(|err| vec![json_error_to_diagnostic(err, uri)])?;

		let root = parse_result.value.ok_or_else(|| {
			vec![Diagnostic {
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
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_JSON_EMPTY".into())),
				source: Some("wa2-lsp".into()),
				message: "Template is empty".to_string(),
				..Default::default()
			}]
		})?;

		// Use the unified parser
		let parser = JsonCfnParser::new(text);
		parser::parse_template(&parser, &root, uri)
	}
}

/// Convert a jsonc-parser error to an LSP diagnostic
fn json_error_to_diagnostic(err: ParseError, uri: &Url) -> Diagnostic {
	// ParseError doesn't have text, so use simple range
	let json_range = err.range();

	let range = Range {
		start: Position {
			line: 0,
			character: json_range.start as u32,
		},
		end: Position {
			line: 0,
			character: json_range.end as u32,
		},
	};

	Diagnostic {
		range,
		severity: Some(DiagnosticSeverity::ERROR),
		code: Some(NumberOrString::String("WA2_JSON_PARSE".into())),
		source: Some("wa2-lsp".into()),
		message: format!("JSON parse error in {uri}: {}", err),
		..Default::default()
	}
}

/// JSON parser implementation
pub struct JsonCfnParser<'a> {
	text: &'a str,
}

impl<'a> JsonCfnParser<'a> {
	pub fn new(text: &'a str) -> Self {
		Self { text }
	}
}

impl<'a> CfnParser for JsonCfnParser<'a> {
	type Node = Value<'a>;

	fn node_as_string(&self, node: &Self::Node) -> Option<String> {
		node.as_string_lit().map(|s| s.value.to_string())
	}

	fn node_as_number(&self, node: &Self::Node) -> Option<f64> {
		node.as_number_lit()
			.and_then(|n| n.value.parse::<f64>().ok())
	}

	fn node_as_bool(&self, node: &Self::Node) -> Option<bool> {
		node.as_boolean_lit().map(|b| b.value)
	}

	fn node_is_null(&self, node: &Self::Node) -> bool {
		// JSON null: check if it's not string/number/bool/array/object
		node.as_string_lit().is_none()
			&& node.as_number_lit().is_none()
			&& node.as_boolean_lit().is_none()
			&& node.as_array().is_none()
			&& node.as_object().is_none()
	}

	fn array_len(&self, node: &Self::Node) -> Option<usize> {
		node.as_array().map(|arr| arr.elements.len())
	}

	fn array_get(&self, node: &Self::Node, index: usize) -> Option<Self::Node> {
		node.as_array()
			.and_then(|arr| arr.elements.get(index))
			.cloned()
	}

	fn object_entries(&self, node: &Self::Node) -> Option<Vec<(String, Self::Node)>> {
		node.as_object().map(|obj| {
			obj.properties
				.iter()
				.map(|prop| {
					let key = prop.name.as_str().to_string();
					let value = prop.value.clone();
					(key, value)
				})
				.collect()
		})
	}

	fn object_get(&self, node: &Self::Node, key: &str) -> Option<Self::Node> {
		node.as_object()
			.and_then(|obj| obj.get(key))
			.map(|prop| prop.value.clone())
	}

	fn node_range(&self, node: &Self::Node) -> Range {
		let json_range = node.range();

		Range {
			start: self.byte_offset_to_position(json_range.start),
			end: self.byte_offset_to_position(json_range.end),
		}
	}

	fn object_entries_with_invalid_keys(
		&self,
		node: &Self::Node,
	) -> Option<Vec<(Option<String>, Self::Node)>> {
		// JSON keys are always strings, so this is the same as object_entries
		self.object_entries(node).map(|entries| {
			entries
				.into_iter()
				.map(|(key, node)| (Some(key), node))
				.collect()
		})
	}

	fn detect_intrinsic(
		&self,
		node: &Self::Node,
	) -> Result<Option<(IntrinsicKind, Self::Node)>, Vec<Diagnostic>> {
		// JSON only has long-form like {"Ref": "MyBucket"}
		if let Some(entries) = self.object_entries(node)
			&& entries.len() == 1
		{
			let (key, value_node) = &entries[0];
			if let Some(intrinsic) = intrinsics::get_intrinsic_by_json_key(key) {
				return Ok(Some((intrinsic.kind, value_node.clone())));
			}

			// Check if it's an unknown Fn::* intrinsic
			if key.starts_with("Fn::") {
				let range = self.node_range(node);
				return Err(vec![Diagnostic {
					range,
					severity: Some(DiagnosticSeverity::ERROR),
					code: Some(NumberOrString::String("WA2_CFN_UNKNOWN_INTRINSIC".into())),
					source: Some("wa2-lsp".into()),
					message: format!("Unknown CloudFormation intrinsic function: {}", key),
					..Default::default()
				}]);
			}
		}

		Ok(None)
	}

	fn object_entries_with_ranges(
		&self,
		node: &Self::Node,
	) -> Option<Vec<ObjectEntry<Self::Node>>> {
		node.as_object().map(|obj| {
			obj.properties
				.iter()
				.map(|prop| {
					let key = Some(prop.name.as_str().to_string());
					let value = prop.value.clone();

					// Get key range using ranged_to_range on the name
					let key_range = self.ranged_to_range(&prop.name);
					(key, value, key_range)
				})
				.collect()
		})
	}

	fn get_section(&self, root: &Self::Node, section_name: &str) -> Option<Self::Node> {
		self.object_get(root, section_name)
	}

	fn word_at_position(&self, root: &Self::Node, position: Position) -> Option<String> {
		// Recursively search for a string node that contains this position
		fn search_node<'a>(
			parser: &JsonCfnParser<'a>,
			node: &Value<'a>,
			position: Position,
		) -> Option<String> {
			let range = parser.node_range(node);

			// Check if position is within this node's range
			if position.line < range.start.line || position.line > range.end.line {
				return None;
			}
			if position.line == range.start.line && position.character < range.start.character {
				return None;
			}
			if position.line == range.end.line && position.character > range.end.character {
				return None;
			}

			// Position is within this node
			match node {
				Value::StringLit(s) => Some(s.value.to_string()),
				Value::Array(arr) => {
					for elem in &arr.elements {
						if let Some(word) = search_node(parser, elem, position) {
							return Some(word);
						}
					}
					None
				}
				Value::Object(obj) => {
					for prop in &obj.properties {
						if let Some(word) = search_node(parser, &prop.value, position) {
							return Some(word);
						}
					}
					None
				}
				_ => None,
			}
		}

		search_node(self, root, position)
	}
}

impl<'a> JsonCfnParser<'a> {
	/// Convert byte offset to line/column position
	fn byte_offset_to_position(&self, offset: usize) -> tower_lsp::lsp_types::Position {
		let mut line = 0u32;
		let mut character = 0u32;

		for (byte_idx, ch) in self.text.char_indices() {
			if byte_idx >= offset {
				break;
			}
			if ch == '\n' {
				line += 1;
				character = 0;
			} else {
				character += 1;
			}
		}

		tower_lsp::lsp_types::Position { line, character }
	}

	/// Helper to convert anything with a range to an LSP Range
	fn ranged_to_range<T: Ranged>(&self, item: &T) -> Range {
		let json_range = item.range();
		Range {
			start: self.byte_offset_to_position(json_range.start),
			end: self.byte_offset_to_position(json_range.end),
		}
	}
}
