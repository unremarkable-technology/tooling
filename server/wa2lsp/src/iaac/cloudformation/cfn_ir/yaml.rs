use saphyr::{LoadableYamlNode, MarkedYaml, Scalar, ScanError, YamlData};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range};
use url::Url;

use crate::iaac::cloudformation::cfn_ir::types::CfnTemplate;
use crate::iaac::cloudformation::intrinsics::{self, IntrinsicKind};

use crate::iaac::cloudformation::cfn_ir::parser::{self, CfnParser, ObjectEntry};

impl CfnTemplate {
	/// Parse a CloudFormation template from YAML text
	pub fn from_yaml(text: &str, uri: &Url) -> Result<Self, Vec<Diagnostic>> {
		let docs = MarkedYaml::load_from_str(text)
			.map_err(|err| vec![yaml_error_to_diagnostic(err, uri)])?;

		if docs.is_empty() {
			return Ok(CfnTemplate::default());
		}

		// Use the unified parser
		let parser = YamlCfnParser::new(text);
		parser::parse_template(&parser, &docs[0], uri)
	}
}

/// Convert a saphyr scan error to an LSP diagnostic
fn yaml_error_to_diagnostic(err: ScanError, uri: &Url) -> Diagnostic {
	let marker = err.marker();
	let line = marker.line().saturating_sub(1);
	let col = marker.col();

	Diagnostic {
		range: Range {
			start: Position {
				line: line as u32,
				character: col as u32,
			},
			end: Position {
				line: line as u32,
				character: (col + 1) as u32,
			},
		},
		severity: Some(DiagnosticSeverity::ERROR),
		code: Some(NumberOrString::String("WA2_YAML_PARSE".into())),
		source: Some("wa2-lsp".into()),
		message: format!("YAML parse error in {uri}: {err}"),
		..Default::default()
	}
}

/// YAML parser implementation
pub struct YamlCfnParser<'a> {
	#[allow(dead_code)] // for lifetime
	text: &'a str,
}

impl<'a> YamlCfnParser<'a> {
	pub fn new(text: &'a str) -> Self {
		Self { text }
	}
}

impl<'a> CfnParser for YamlCfnParser<'a> {
	type Node = MarkedYaml<'a>;

	fn node_as_string(&self, node: &Self::Node) -> Option<String> {
		match &node.data {
			YamlData::Value(Scalar::String(s)) => Some(s.to_string()),
			_ => None,
		}
	}

	fn node_as_number(&self, node: &Self::Node) -> Option<f64> {
		match &node.data {
			YamlData::Value(Scalar::Integer(i)) => Some(*i as f64),
			YamlData::Value(Scalar::FloatingPoint(f)) => Some(f.into_inner()),
			_ => None,
		}
	}

	fn node_as_bool(&self, node: &Self::Node) -> Option<bool> {
		match &node.data {
			YamlData::Value(Scalar::Boolean(b)) => Some(*b),
			_ => None,
		}
	}

	fn node_is_null(&self, node: &Self::Node) -> bool {
		matches!(&node.data, YamlData::Value(Scalar::Null))
	}

	fn array_len(&self, node: &Self::Node) -> Option<usize> {
		match &node.data {
			YamlData::Sequence(seq) => Some(seq.len()),
			_ => None,
		}
	}

	fn array_get(&self, node: &Self::Node, index: usize) -> Option<Self::Node> {
		match &node.data {
			YamlData::Sequence(seq) => seq.get(index).cloned(), // ← Clone it!
			_ => None,
		}
	}

	fn object_entries(&self, node: &Self::Node) -> Option<Vec<(String, Self::Node)>> {
		match &node.data {
			YamlData::Mapping(map) => {
				let entries = map
					.iter()
					.filter_map(|(k, v)| {
						if let YamlData::Value(Scalar::String(key)) = &k.data {
							Some((key.to_string(), v.clone())) // ← Clone it!
						} else {
							None
						}
					})
					.collect();
				Some(entries)
			}
			_ => None,
		}
	}

	fn object_get(&self, node: &Self::Node, key: &str) -> Option<Self::Node> {
		match &node.data {
			YamlData::Mapping(map) => {
				map.iter()
					.find(
						|(k, _)| matches!(&k.data, YamlData::Value(Scalar::String(s)) if s == key),
					)
					.map(|(_, v)| v.clone()) // ← Clone it!
			}
			_ => None,
		}
	}

	fn node_range(&self, node: &Self::Node) -> Range {
		let start_marker = node.span.start;
		let end_marker = node.span.end;

		let start_line = start_marker.line().saturating_sub(1);
		let start_col = start_marker.col();

		let end_line = end_marker.line().saturating_sub(1);
		let end_col = end_marker.col();

		Range {
			start: tower_lsp::lsp_types::Position {
				line: start_line as u32,
				character: start_col as u32,
			},
			end: tower_lsp::lsp_types::Position {
				line: end_line as u32,
				character: end_col as u32,
			},
		}
	}

	fn object_entries_with_invalid_keys(
		&self,
		node: &Self::Node,
	) -> Option<Vec<(Option<String>, Self::Node)>> {
		match &node.data {
			YamlData::Mapping(map) => {
				let entries = map
					.iter()
					.map(|(k, v)| {
						let key = if let YamlData::Value(Scalar::String(s)) = &k.data {
							Some(s.to_string())
						} else {
							None // Non-string key
						};
						(key, v.clone())
					})
					.collect();
				Some(entries)
			}
			_ => None,
		}
	}

	fn detect_intrinsic(
		&self,
		node: &Self::Node,
	) -> Result<Option<(IntrinsicKind, Self::Node)>, Vec<Diagnostic>> {
		use saphyr::YamlData;

		// Check for YAML tags like !Ref, !GetAtt, !FindInMap
		if let YamlData::Tagged(tag, inner) = &node.data {
			let tag_name = &tag.suffix;

			if let Some(intrinsic) = intrinsics::get_intrinsic_by_tag(tag_name) {
				return Ok(Some((intrinsic.kind, (**inner).clone())));
			}

			// Unknown YAML tag - error!
			let range = self.node_range(node);
			return Err(vec![Diagnostic {
				range,
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_UNKNOWN_INTRINSIC".into())),
				source: Some("wa2-lsp".into()),
				message: format!("Unknown CloudFormation intrinsic function: !{}", tag_name),
				..Default::default()
			}]);
		}

		// Check for long-form like "Ref: MyBucket"
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
		match &node.data {
			YamlData::Mapping(map) => {
				let entries = map
					.iter()
					.map(|(k, v)| {
						let key = if let YamlData::Value(Scalar::String(s)) = &k.data {
							Some(s.to_string())
						} else {
							None
						};
						let key_range = self.node_range(k); // Get the KEY range
						(key, v.clone(), key_range)
					})
					.collect();
				Some(entries)
			}
			_ => None,
		}
	}

	fn word_at_position(&self, root: &Self::Node, position: Position) -> Option<String> {
		// Recursively search for a string node that contains this position
		fn search_node(node: &MarkedYaml, position: Position) -> Option<String> {
			// Check if position is within this node's range
			let start_marker = node.span.start;
			let end_marker = node.span.end;

			let start_line = start_marker.line().saturating_sub(1) as u32;
			let start_col = start_marker.col() as u32;
			let end_line = end_marker.line().saturating_sub(1) as u32;
			let end_col = end_marker.col() as u32;

			// Check if position is within range
			if position.line < start_line || position.line > end_line {
				return None;
			}
			if position.line == start_line && position.character < start_col {
				return None;
			}
			if position.line == end_line && position.character > end_col {
				return None;
			}

			// Position is within this node - check what type it is
			match &node.data {
				YamlData::Value(Scalar::String(s)) => {
					Some(s.to_string())
				}
				YamlData::Sequence(seq) => {
					for item in seq {
						if let Some(word) = search_node(item, position) {
							return Some(word);
						}
					}
					None
				}
				YamlData::Mapping(map) => {
					for (key, value) in map {
						if let Some(word) = search_node(key, position) {
							return Some(word);
						}
						if let Some(word) = search_node(value, position) {
							return Some(word);
						}
					}
					None
				}
				YamlData::Tagged(_, inner) => {
					// For tagged nodes (like !Ref), the inner value might have invalid range
					// So we extract it directly if it's a string
					match &inner.data {
						YamlData::Value(Scalar::String(s)) => {
							Some(s.to_string())
						}
						_ => search_node(inner, position),
					}
				}
				_ => {
					None
				}
			}
		}

		search_node(root, position)
	}
}
