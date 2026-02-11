use tower_lsp::lsp_types::{Diagnostic, DiagnosticRelatedInformation, Location, Position, Range};

use crate::spec::{
	cfn_ir::types::{CfnAssertion, CfnMapping, CfnOutput, CfnRule},
	intrinsics::IntrinsicKind,
};
use std::collections::HashMap;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString};
use url::Url;

use crate::spec::cfn_ir::types::CfnValue;

use super::types::{CfnCondition, CfnParameter, CfnResource, CfnTemplate};

/// Result type for parsing operations
pub type ParseResult<T> = Result<T, Vec<Diagnostic>>;

/// Entry in an object with optional key string, value node, and key range
/// Used when parsing objects where keys might not be valid strings
pub type ObjectEntry<N> = (Option<String>, N, Range);

/// High-level operations needed to parse CloudFormation templates
/// Each format (JSON/YAML) implements this trait for their node type
pub trait CfnParser {
	/// The underlying node type (must be cloneable)
	type Node: Clone;

	// ===== Scalar operations =====

	/// Try to interpret this node as a string
	fn node_as_string(&self, node: &Self::Node) -> Option<String>;

	/// Try to interpret this node as a number
	fn node_as_number(&self, node: &Self::Node) -> Option<f64>;

	/// Try to interpret this node as a boolean
	fn node_as_bool(&self, node: &Self::Node) -> Option<bool>;

	/// Check if this node is null
	fn node_is_null(&self, node: &Self::Node) -> bool;

	// ===== Collection operations =====

	/// Get the length of an array node
	fn array_len(&self, node: &Self::Node) -> Option<usize>;

	/// Get an element from an array by index (returns owned copy)
	fn array_get(&self, node: &Self::Node, index: usize) -> Option<Self::Node>;

	/// Get all key-value pairs from an object/mapping (returns owned copies)
	fn object_entries(&self, node: &Self::Node) -> Option<Vec<(String, Self::Node)>>;

	/// Get a specific property from an object by key (returns owned copy)
	fn object_get(&self, node: &Self::Node, key: &str) -> Option<Self::Node>;

	// ===== Range operations =====

	/// Get the LSP range for this node
	fn node_range(&self, node: &Self::Node) -> Range;

	// ===== High-level helpers with default implementations =====

	/// Get a top-level section from the root node
	fn get_section(&self, root: &Self::Node, section_name: &str) -> Option<Self::Node> {
		self.object_get(root, section_name)
	}

	/// Get all entries from an object, including those with non-string keys
	/// Returns (Option<key_as_string>, value_node)
	/// If key is None, it means the key wasn't a valid string
	fn object_entries_with_invalid_keys(
		&self,
		node: &Self::Node,
	) -> Option<Vec<(Option<String>, Self::Node)>>;

	/// Check if this node represents an intrinsic function
	/// Returns Ok(Some((kind, inner))) for known intrinsics
	/// Returns Ok(None) if not an intrinsic
	/// Returns Err(...) for unknown intrinsics
	fn detect_intrinsic(
		&self,
		node: &Self::Node,
	) -> Result<Option<(IntrinsicKind, Self::Node)>, Vec<Diagnostic>>;

	/// Get all entries from an object with both key and value ranges
	/// Returns (key_string, value_node, key_range)
	fn object_entries_with_ranges(&self, node: &Self::Node)
	-> Option<Vec<ObjectEntry<Self::Node>>>;

	/// Find the word/identifier at the given position
	fn word_at_position(&self, root: &Self::Node, position: Position) -> Option<String>;
}

/// Parse a CloudFormation template from a root node using a generic parser
/// This works for both JSON and YAML
pub fn parse_template<P: CfnParser>(
	parser: &P,
	root: &P::Node,
	uri: &Url,
) -> ParseResult<CfnTemplate> {
	let resources = parser
		.get_section(root, "Resources")
		.map(|node| parse_resources(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	let parameters = parser
		.get_section(root, "Parameters")
		.map(|node| parse_parameters(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	let conditions = parser
		.get_section(root, "Conditions")
		.map(|node| parse_conditions(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	let mappings = parser
		.get_section(root, "Mappings")
		.map(|node| parse_mappings(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	let rules = parser
		.get_section(root, "Rules")
		.map(|node| parse_rules(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	let outputs = parser
		.get_section(root, "Outputs")
		.map(|node| parse_outputs(parser, &node, uri))
		.transpose()?
		.unwrap_or_default();

	// Parse Transform (can be string or array of strings)
	let transform = parser.get_section(root, "Transform").and_then(|node| {
		if let Some(s) = parser.node_as_string(&node) {
			// Single transform
			Some(vec![s])
		} else if let Some(len) = parser.array_len(&node) {
			// Array of transforms
			let mut transforms = Vec::new();
			for i in 0..len {
				if let Some(item) = parser.array_get(&node, i)
					&& let Some(s) = parser.node_as_string(&item)
				{
					transforms.push(s);
				}
			}
			Some(transforms)
		} else {
			None
		}
	});

	Ok(CfnTemplate {
		resources,
		parameters,
		conditions,
		mappings,
		rules,
		outputs,
		transform,
	})
}

fn parse_resources<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnResource>> {
	let mut resources: HashMap<String, CfnResource> = HashMap::new();
	let mut errors = Vec::new();

	// Get all resources with key ranges
	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_RESOURCES".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Resources section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	// Parse each resource
	for (logical_id_opt, resource_node, key_range) in entries {
		let logical_id = match logical_id_opt {
			Some(id) => id,
			None => {
				errors.push(Diagnostic {
                    range: key_range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: Some(NumberOrString::String("WA2_CFN_RESOURCE_KEY_NOT_STRING".into())),
                    source: Some("wa2-lsp".into()),
                    message: "Template: resource key is not a string; CloudFormation logical IDs must be strings.".to_string(),
                    ..Default::default()
                });
				continue;
			}
		};

		// Validate logical ID format
		if let Some(diag) = validate_logical_id(&logical_id, "resource", key_range, uri) {
			errors.push(diag);
			continue; // Skip parsing this resource
		}

		// Check if this is a ForEach construct
		if logical_id.starts_with("Fn::ForEach::") {
			// Parse as ForEach, not as regular resource
			match parse_for_each_resource(parser, &logical_id, &resource_node, key_range, uri) {
				Ok(for_each_resource) => {
					// Add as a special resource type
					resources.insert(logical_id.clone(), for_each_resource);
				}
				Err(mut diags) => {
					errors.append(&mut diags);
				}
			}
			continue; // Skip normal resource parsing
		}

		// Try to insert - if key exists, it's a duplicate
		match resources.entry(logical_id.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				// Duplicate key found
				let first_range = entry.get().logical_id_range;
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate resource key `{}`. Previous definition will be overwritten.",
						logical_id
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				// Parse the resource anyway (overwriting the previous one)
				match parse_resource(parser, &logical_id, &resource_node, key_range, uri) {
					Ok(resource) => {
						*entry.into_mut() = resource; // Overwrite
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				// First occurrence
				match parse_resource(parser, &logical_id, &resource_node, key_range, uri) {
					Ok(resource) => {
						entry.insert(resource);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(resources)
}

fn parse_for_each_resource<P: CfnParser>(
	parser: &P,
	logical_id: &str,
	node: &P::Node,
	name_range: Range,
	uri: &Url,
) -> ParseResult<CfnResource> {
	// ForEach structure: [Identifier, Collection, TemplateFragment]
	let len = parser.array_len(node);

	if len != Some(3) {
		return Err(vec![Diagnostic {
			range: parser.node_range(node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: format!(
				"Malformed Fn::ForEach `{}`: expected array [Identifier, Collection, TemplateFragment]",
				logical_id
			),
			..Default::default()
		}]);
	}

	let identifier_node = parser.array_get(node, 0).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: "ForEach: missing Identifier".to_string(),
			..Default::default()
		}]
	})?;

	let collection_node = parser.array_get(node, 1).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: "ForEach: missing Collection".to_string(),
			..Default::default()
		}]
	})?;

	let fragment_node = parser.array_get(node, 2).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: "ForEach: missing TemplateFragment".to_string(),
			..Default::default()
		}]
	})?;

	// Parse identifier (must be string)
	let _identifier = parser.node_as_string(&identifier_node).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(&identifier_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: "ForEach Identifier must be a string".to_string(),
			..Default::default()
		}]
	})?;

	// Parse collection (can be array or intrinsic) - validates it's a valid value
	let _collection = parse_value(parser, &collection_node)?;

	// Parse template fragment (must be an object with one entry)
	let fragment_entries = parser.object_entries(&fragment_node).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(&fragment_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: "ForEach TemplateFragment must be an object".to_string(),
			..Default::default()
		}]
	})?;

	if fragment_entries.len() != 1 {
		return Err(vec![Diagnostic {
			range: parser.node_range(&fragment_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_MALFORMED_FOREACH".into())),
			source: Some("wa2-lsp".into()),
			message: format!(
				"ForEach TemplateFragment must contain exactly one resource definition, found {}",
				fragment_entries.len()
			),
			..Default::default()
		}]);
	}

	// Parse the resource inside the fragment
	let (resource_key, resource_value_node) = &fragment_entries[0];
	let mut resource = parse_resource(
		parser,
		resource_key,
		resource_value_node,
		parser.node_range(resource_value_node),
		uri,
	)?;

	// Override the logical ID and type to indicate this is a ForEach
	resource.logical_id = logical_id.to_string();
	resource.resource_type = format!(
		"AWS::LanguageExtensions::ForEach<{}>",
		resource.resource_type
	);
	resource.logical_id_range = name_range;

	Ok(resource)
}

fn parse_resource<P: CfnParser>(
	parser: &P,
	logical_id: &str,
	node: &P::Node,
	logical_id_range: Range,
	_uri: &Url,
) -> ParseResult<CfnResource> {
	// Check if node is an object first
	if parser.object_entries(node).is_none() {
		return Err(vec![Diagnostic {
			range: parser.node_range(node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_NOT_MAPPING".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!(
				"Template: resource `{}` is not an object/mapping; \
                 CloudFormation resources must be objects with `Type` and `Properties`.",
				logical_id
			),
			..Default::default()
		}]);
	}

	// Extract Type property
	let type_node = parser.object_get(node, "Type").ok_or_else(|| {
		vec![Diagnostic {
			range: logical_id_range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_TYPE_MISSING".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!(
				"Template: resource `{}` is missing required `Type`.",
				logical_id
			),
			..Default::default()
		}]
	})?;

	let type_str = parser.node_as_string(&type_node).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(&type_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_RESOURCE_TYPE_MISSING".into(),
			)), // ← Changed from TYPE_INVALID
			source: Some("wa2-lsp".into()),
			message: format!(
				"Template: resource `{}` is missing required `Type`.",
				logical_id
			), // ← Changed message
			..Default::default()
		}]
	})?;

	let type_range = parser.node_range(&type_node);

	// Extract Properties (optional)
	let properties = if let Some(props_node) = parser.object_get(node, "Properties") {
		parse_properties(parser, &props_node)?
	} else {
		HashMap::new()
	};

	Ok(CfnResource {
		logical_id: logical_id.to_string(),
		resource_type: type_str,
		properties,
		logical_id_range,
		type_range,
	})
}

fn parse_properties<P: CfnParser>(
	parser: &P,
	node: &P::Node,
) -> ParseResult<HashMap<String, (CfnValue, Range)>> {
	let mut properties = HashMap::new();

	// Get all properties as key-value pairs
	let entries = match parser.object_entries(node) {
		Some(entries) => entries,
		None => {
			// If Properties isn't an object, return empty map and let validation handle it
			return Ok(HashMap::new());
		}
	};

	// Parse each property
	for (key, value_node) in entries {
		let value = parse_value(parser, &value_node)?;
		let key_range = parser.node_range(&value_node);
		properties.insert(key, (value, key_range));
	}

	Ok(properties)
}

fn parse_parameters<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnParameter>> {
	let mut parameters: HashMap<String, CfnParameter> = HashMap::new();
	let mut errors = Vec::new();

	// Use object_entries_with_ranges to get key ranges
	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_PARAMETERS".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Parameters section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	// Parse each parameter
	for (param_name_opt, param_node, key_range) in entries {
		let param_name = match param_name_opt {
			Some(name) => name,
			None => {
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String(
						"WA2_CFN_PARAMETER_KEY_NOT_STRING".into(),
					)),
					source: Some("wa2-lsp".into()),
					message: "Template: parameter key is not a string".to_string(),
					..Default::default()
				});
				continue;
			}
		};

		let name_range = key_range;

		// Validate logical ID format
		if let Some(diag) = validate_logical_id(&param_name, "parameter", name_range, uri) {
			errors.push(diag);
			continue;
		}

		// Try to insert - if key exists, it's a duplicate
		match parameters.entry(param_name.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				let first_range = entry.get().name_range;
				errors.push(Diagnostic {
					range: name_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate parameter key `{}`. Previous definition will be overwritten.",
						param_name
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				match parse_parameter(parser, &param_name, &param_node, name_range, uri) {
					Ok(param) => {
						*entry.into_mut() = param;
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				match parse_parameter(parser, &param_name, &param_node, name_range, uri) {
					Ok(param) => {
						entry.insert(param);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(parameters)
}
fn parse_parameter<P: CfnParser>(
	parser: &P,
	name: &str,
	node: &P::Node,
	name_range: Range,
	_uri: &Url,
) -> ParseResult<CfnParameter> {
	// Extract Type (required)
	let type_node = parser.object_get(node, "Type").ok_or_else(|| {
		vec![Diagnostic {
			range: name_range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_PARAMETER_TYPE_MISSING".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!("Template: parameter `{}` is missing required `Type`.", name),
			..Default::default()
		}]
	})?;

	let type_str = parser.node_as_string(&type_node).ok_or_else(|| {
		vec![Diagnostic {
			range: parser.node_range(&type_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_PARAMETER_TYPE_INVALID".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!("Template: parameter `{}` Type must be a string.", name),
			..Default::default()
		}]
	})?;

	let type_range = parser.node_range(&type_node);

	// Extract Default (optional)
	let default_value = if let Some(default_node) = parser.object_get(node, "Default") {
		Some(parse_value(parser, &default_node)?)
	} else {
		None
	};

	// Extract Description (optional)
	let description = parser
		.object_get(node, "Description")
		.and_then(|n| parser.node_as_string(&n));

	Ok(CfnParameter {
		name: name.to_string(),
		parameter_type: type_str,
		default_value,
		description,
		name_range,
		type_range,
	})
}

fn parse_conditions<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnCondition>> {
	let mut conditions: HashMap<String, CfnCondition> = HashMap::new();
	let mut errors = Vec::new();

	// Use object_entries_with_ranges to get key ranges
	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_CONDITIONS".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Conditions section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	// Parse each condition
	for (condition_name_opt, condition_node, key_range) in entries {
		let condition_name = match condition_name_opt {
			Some(name) => name,
			None => {
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String(
						"WA2_CFN_CONDITION_KEY_NOT_STRING".into(),
					)),
					source: Some("wa2-lsp".into()),
					message: "Template: condition key is not a string".to_string(),
					..Default::default()
				});
				continue;
			}
		};

		let name_range = key_range;

		// Validate logical ID format
		if let Some(diag) = validate_logical_id(&condition_name, "condition", name_range, uri) {
			errors.push(diag);
			continue;
		}

		// Try to insert - if key exists, it's a duplicate
		match conditions.entry(condition_name.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				let first_range = entry.get().name_range;
				errors.push(Diagnostic {
					range: name_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate condition key `{}`. Previous definition will be overwritten.",
						condition_name
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				match parse_condition(parser, &condition_name, &condition_node, name_range, uri) {
					Ok(condition) => {
						*entry.into_mut() = condition;
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				match parse_condition(parser, &condition_name, &condition_node, name_range, uri) {
					Ok(condition) => {
						entry.insert(condition);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(conditions)
}

fn parse_condition<P: CfnParser>(
	parser: &P,
	name: &str,
	node: &P::Node,
	name_range: Range,
	_uri: &Url,
) -> ParseResult<CfnCondition> {
	// A condition is just a value expression (usually an intrinsic function)
	let expression = parse_value(parser, node)?;

	Ok(CfnCondition {
		name: name.to_string(),
		expression,
		name_range,
	})
}

fn parse_mappings<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnMapping>> {
	let mut mappings: HashMap<String, CfnMapping> = HashMap::new();
	let mut errors = Vec::new();

	// Get all mappings including invalid keys
	// Use object_entries_with_ranges to get key ranges
	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_MAPPINGS".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Mappings section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	// Parse each mapping
	for (mapping_name_opt, mapping_node, key_range) in entries {
		let mapping_name = match mapping_name_opt {
			Some(name) => name,
			None => {
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String(
						"WA2_CFN_MAPPING_KEY_NOT_STRING".into(),
					)),
					source: Some("wa2-lsp".into()),
					message: "Template: mapping key is not a string".to_string(),
					..Default::default()
				});
				continue;
			}
		};

		let name_range = key_range;

		// Validate logical ID format
		if let Some(diag) = validate_logical_id(&mapping_name, "mapping", name_range, uri) {
			errors.push(diag);
			continue;
		}

		// Try to insert - if key exists, it's a duplicate
		match mappings.entry(mapping_name.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				let first_range = entry.get().name_range;
				errors.push(Diagnostic {
					range: name_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate mapping key `{}`. Previous definition will be overwritten.",
						mapping_name
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				match parse_mapping(parser, &mapping_name, &mapping_node, name_range, uri) {
					Ok(mapping) => {
						*entry.into_mut() = mapping;
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				match parse_mapping(parser, &mapping_name, &mapping_node, name_range, uri) {
					Ok(mapping) => {
						entry.insert(mapping);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(mappings)
}

fn parse_mapping<P: CfnParser>(
	parser: &P,
	name: &str,
	node: &P::Node,
	name_range: Range,
	_uri: &Url,
) -> ParseResult<CfnMapping> {
	// A mapping is a two-level nested object:
	// MapName:
	//   TopLevelKey:
	//     SecondLevelKey: Value

	let top_level_entries = match parser.object_entries(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_MAPPING".into())),
				source: Some("wa2-lsp".into()),
				message: format!("Mapping `{}` must be an object", name),
				..Default::default()
			}]);
		}
	};

	let mut map = HashMap::new();

	for (top_key, second_level_node) in top_level_entries {
		let second_level_entries = match parser.object_entries(&second_level_node) {
			Some(entries) => entries,
			None => {
				return Err(vec![Diagnostic {
					range: parser.node_range(&second_level_node),
					severity: Some(DiagnosticSeverity::ERROR),
					code: Some(NumberOrString::String("WA2_CFN_INVALID_MAPPING".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Mapping `{}` top-level key `{}` must be an object",
						name, top_key
					),
					..Default::default()
				}]);
			}
		};

		let mut second_level_map = HashMap::new();
		for (second_key, value_node) in second_level_entries {
			let value = parse_value(parser, &value_node)?;
			second_level_map.insert(second_key, value);
		}

		map.insert(top_key, second_level_map);
	}

	Ok(CfnMapping {
		name: name.to_string(),
		map,
		name_range,
	})
}

fn parse_rules<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnRule>> {
	let mut rules: HashMap<String, CfnRule> = HashMap::new();
	let mut errors = Vec::new();

	// Get all rules including invalid keys
	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_RULES".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Rules section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	// Parse each rule
	for (rule_name_opt, rule_node, key_range) in entries {
		let rule_name = match rule_name_opt {
			Some(name) => name,
			None => {
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_RULE_KEY_NOT_STRING".into())),
					source: Some("wa2-lsp".into()),
					message: "Template: rule key is not a string".to_string(),
					..Default::default()
				});
				continue;
			}
		};

		let name_range = key_range;

		// Validate logical ID format
		if let Some(diag) = validate_logical_id(&rule_name, "rule", name_range, uri) {
			errors.push(diag);
			continue;
		}

		// Try to insert - if key exists, it's a duplicate
		match rules.entry(rule_name.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				let first_range = entry.get().name_range;
				errors.push(Diagnostic {
					range: name_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate rule key `{}`. Previous definition will be overwritten.",
						rule_name
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				match parse_rule(parser, &rule_name, &rule_node, name_range, uri) {
					Ok(rule) => {
						*entry.into_mut() = rule;
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				match parse_rule(parser, &rule_name, &rule_node, name_range, uri) {
					Ok(rule) => {
						entry.insert(rule);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(rules)
}

fn parse_rule<P: CfnParser>(
	parser: &P,
	name: &str,
	node: &P::Node,
	name_range: Range,
	_uri: &Url,
) -> ParseResult<CfnRule> {
	// A rule contains an Assertions array
	let assertions_node = parser.object_get(node, "Assertions").ok_or_else(|| {
		vec![Diagnostic {
			range: name_range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_RULE_MISSING_ASSERTIONS".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!("Rule `{}` must have an Assertions array", name),
			..Default::default()
		}]
	})?;

	// Parse assertions array
	let mut assertions = Vec::new();

	if let Some(len) = parser.array_len(&assertions_node) {
		for i in 0..len {
			if let Some(assertion_node) = parser.array_get(&assertions_node, i) {
				let assertion = parse_assertion(parser, &assertion_node)?;
				assertions.push(assertion);
			}
		}
	} else {
		return Err(vec![Diagnostic {
			range: parser.node_range(&assertions_node),
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_RULE_INVALID_ASSERTIONS".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!("Rule `{}` Assertions must be an array", name),
			..Default::default()
		}]);
	}

	Ok(CfnRule {
		name: name.to_string(),
		assertions,
		name_range,
	})
}

fn parse_outputs<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	uri: &Url,
) -> ParseResult<HashMap<String, CfnOutput>> {
	let mut outputs: HashMap<String, CfnOutput> = HashMap::new();
	let mut errors = Vec::new();

	let entries = match parser.object_entries_with_ranges(node) {
		Some(entries) => entries,
		None => {
			return Err(vec![Diagnostic {
				range: parser.node_range(node),
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_INVALID_OUTPUTS".into())),
				source: Some("wa2-lsp".into()),
				message: "Template Outputs section must be an object/mapping".to_string(),
				..Default::default()
			}]);
		}
	};

	for (output_name_opt, output_node, key_range) in entries {
		let output_name = match output_name_opt {
			Some(name) => name,
			None => {
				errors.push(Diagnostic {
					range: key_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String(
						"WA2_CFN_OUTPUT_KEY_NOT_STRING".into(),
					)),
					source: Some("wa2-lsp".into()),
					message: "Template: output key is not a string".to_string(),
					..Default::default()
				});
				continue;
			}
		};

		let name_range = key_range;

		if let Some(diag) = validate_logical_id(&output_name, "output", name_range, uri) {
			errors.push(diag);
			continue;
		}

		match outputs.entry(output_name.clone()) {
			std::collections::hash_map::Entry::Occupied(entry) => {
				let first_range = entry.get().name_range;
				errors.push(Diagnostic {
					range: name_range,
					severity: Some(DiagnosticSeverity::WARNING),
					code: Some(NumberOrString::String("WA2_CFN_DUPLICATE_KEY".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Duplicate output key `{}`. Previous definition will be overwritten.",
						output_name
					),
					related_information: Some(vec![DiagnosticRelatedInformation {
						location: Location {
							uri: uri.clone(),
							range: first_range,
						},
						message: "First definition here".to_string(),
					}]),
					..Default::default()
				});

				match parse_output(parser, &output_name, &output_node, name_range) {
					Ok(output) => {
						*entry.into_mut() = output;
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				match parse_output(parser, &output_name, &output_node, name_range) {
					Ok(output) => {
						entry.insert(output);
					}
					Err(mut diags) => {
						errors.append(&mut diags);
					}
				}
			}
		}
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	Ok(outputs)
}

fn parse_output<P: CfnParser>(
	parser: &P,
	name: &str,
	node: &P::Node,
	name_range: Range,
) -> ParseResult<CfnOutput> {
	// Extract Value (required)
	let value_node = parser.object_get(node, "Value").ok_or_else(|| {
		vec![Diagnostic {
			range: name_range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_OUTPUT_VALUE_MISSING".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: format!("Template: output `{}` is missing required `Value`.", name),
			..Default::default()
		}]
	})?;

	let value = parse_value(parser, &value_node)?;

	// Extract Description (optional)
	let description = parser
		.object_get(node, "Description")
		.and_then(|n| parser.node_as_string(&n));

	// Extract Export.Name (optional)
	let export_name = if let Some(export_node) = parser.object_get(node, "Export") {
		if let Some(name_node) = parser.object_get(&export_node, "Name") {
			Some(parse_value(parser, &name_node)?)
		} else {
			None
		}
	} else {
		None
	};

	// Extract Condition (optional)
	let condition = parser
		.object_get(node, "Condition")
		.and_then(|n| parser.node_as_string(&n));

	Ok(CfnOutput {
		name: name.to_string(),
		value,
		description,
		export_name,
		condition,
		name_range,
	})
}

fn parse_assertion<P: CfnParser>(parser: &P, node: &P::Node) -> ParseResult<CfnAssertion> {
	let range = parser.node_range(node);

	// Extract Assert (required)
	let assert_node = parser.object_get(node, "Assert").ok_or_else(|| {
		vec![Diagnostic {
			range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String(
				"WA2_CFN_ASSERTION_MISSING_ASSERT".into(),
			)),
			source: Some("wa2-lsp".into()),
			message: "Assertion must have an Assert condition".to_string(),
			..Default::default()
		}]
	})?;

	let assert_condition = parse_value(parser, &assert_node)?;

	// Extract AssertDescription (optional)
	let assert_description = parser
		.object_get(node, "AssertDescription")
		.and_then(|n| parser.node_as_string(&n));

	Ok(CfnAssertion {
		assert_condition,
		assert_description,
		range,
	})
}

fn parse_value<P: CfnParser>(parser: &P, node: &P::Node) -> ParseResult<CfnValue> {
	let range = parser.node_range(node);

	// Try scalars first
	if let Some(s) = parser.node_as_string(node) {
		return Ok(CfnValue::String(s, range));
	}

	if let Some(n) = parser.node_as_number(node) {
		return Ok(CfnValue::Number(n, range));
	}

	if let Some(b) = parser.node_as_bool(node) {
		return Ok(CfnValue::Bool(b, range));
	}

	if parser.node_is_null(node) {
		return Ok(CfnValue::Null(range));
	}

	// Check for intrinsics BEFORE checking arrays/objects
	match parser.detect_intrinsic(node)? {
		// ← Handle Result with ?
		Some((kind, inner_node)) => {
			return parse_intrinsic(parser, kind, &inner_node, range);
		}
		None => {
			// Not an intrinsic, continue
		}
	}

	// Try array
	if let Some(len) = parser.array_len(node) {
		let mut items = Vec::new();
		for i in 0..len {
			if let Some(element) = parser.array_get(node, i) {
				items.push(parse_value(parser, &element)?);
			}
		}
		return Ok(CfnValue::Array(items, range));
	}

	// Try object (regular object, not intrinsic)
	if let Some(entries) = parser.object_entries(node) {
		let mut map = HashMap::new();
		for (key, value_node) in entries {
			let value = parse_value(parser, &value_node)?;
			let key_range = parser.node_range(&value_node);
			map.insert(key, (value, key_range));
		}
		return Ok(CfnValue::Object(map, range));
	}

	// Fallback to null
	Ok(CfnValue::Null(range))
}

fn parse_intrinsic<P: CfnParser>(
	parser: &P,
	kind: IntrinsicKind,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	match kind {
		IntrinsicKind::Ref => parse_ref(parser, node, range),
		IntrinsicKind::GetAtt => parse_getatt(parser, node, range),
		IntrinsicKind::Sub => parse_sub(parser, node, range),
		IntrinsicKind::GetAZs => parse_getazs(parser, node, range),
		IntrinsicKind::Join => parse_join(parser, node, range),
		IntrinsicKind::Select => parse_select(parser, node, range),
		IntrinsicKind::If => parse_if(parser, node, range),
		IntrinsicKind::Equals => parse_equals(parser, node, range),
		IntrinsicKind::Not => parse_not(parser, node, range),
		IntrinsicKind::And => parse_and(parser, node, range),
		IntrinsicKind::Or => parse_or(parser, node, range),
		IntrinsicKind::Condition => parse_condition_intrinsic(parser, node, range),
		IntrinsicKind::Base64 => parse_base64(parser, node, range),
		IntrinsicKind::Split => parse_split(parser, node, range),
		IntrinsicKind::Cidr => parse_cidr(parser, node, range),
		IntrinsicKind::ImportValue => parse_import_value(parser, node, range),
		IntrinsicKind::FindInMap => parse_find_in_map(parser, node, range),
		IntrinsicKind::ToJsonString => parse_to_json_string(parser, node, range),
		IntrinsicKind::Length => parse_length(parser, node, range),
		IntrinsicKind::Contains => parse_contains(parser, node, range),
		IntrinsicKind::Transform => parse_transform(parser, node, range),
	}
}

// Helper to create diagnostic
fn make_diagnostic(range: Range, code: &str, message: String) -> Vec<Diagnostic> {
	vec![Diagnostic {
		range,
		severity: Some(DiagnosticSeverity::ERROR),
		code: Some(NumberOrString::String(code.to_string())),
		source: Some("wa2-lsp".into()),
		message,
		..Default::default()
	}]
}

// Now implement each intrinsic - starting with Ref
fn parse_ref<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	if let Some(target) = parser.node_as_string(node) {
		return Ok(CfnValue::Ref { target, range });
	}
	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_REF",
		"Malformed !Ref: expected a string value".to_string(),
	))
}

fn parse_getatt<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Array form: ["ResourceName", "AttributeName"]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(target_node), Some(attr_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
		&& let (Some(target), Some(attribute)) = (
			parser.node_as_string(&target_node),
			parser.node_as_string(&attr_node),
		) {
		return Ok(CfnValue::GetAtt {
			target,
			attribute,
			range,
		});
	}

	// String form: "ResourceName.AttributeName"
	if let Some(s) = parser.node_as_string(node) {
		if let Some((target, attribute)) = s.split_once('.') {
			return Ok(CfnValue::GetAtt {
				target: target.to_string(),
				attribute: attribute.to_string(),
				range,
			});
		} else {
			return Err(make_diagnostic(
				range,
				"WA2_CFN_MALFORMED_GETATT",
				format!(
					"Malformed !GetAtt: expected 'ResourceName.AttributeName', got '{}'",
					s
				),
			));
		}
	}

	Err(make_diagnostic(
        range,
        "WA2_CFN_MALFORMED_GETATT",
        "Malformed !GetAtt: expected 'ResourceName.AttributeName' or array [ResourceName, AttributeName]".to_string(),
    ))
}

fn parse_getazs<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	let region_value = parse_value(parser, node)?;
	Ok(CfnValue::GetAZs {
		region: Box::new(region_value),
		range,
	})
}

fn parse_condition_intrinsic<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	if let Some(condition_name) = parser.node_as_string(node) {
		return Ok(CfnValue::Condition {
			condition_name,
			range,
		});
	}
	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_CONDITION",
		"Malformed !Condition: expected condition name string".to_string(),
	))
}

fn parse_base64<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	let value = parse_value(parser, node)?;
	Ok(CfnValue::Base64 {
		value: Box::new(value),
		range,
	})
}

fn parse_split<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Split: [delimiter, source_string]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(delim_node), Some(source_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
		&& let Some(delimiter) = parser.node_as_string(&delim_node)
	{
		let source = parse_value(parser, &source_node)?;
		return Ok(CfnValue::Split {
			delimiter,
			source: Box::new(source),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_SPLIT",
		"Malformed !Split: expected array [delimiter, source_string]".to_string(),
	))
}

fn parse_cidr<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Cidr: [ipBlock, count, cidrBits]
	if let Some(len) = parser.array_len(node)
		&& len == 3
		&& let (Some(ip_node), Some(count_node), Some(bits_node)) = (
			parser.array_get(node, 0),
			parser.array_get(node, 1),
			parser.array_get(node, 2),
		) {
		let ip_block = parse_value(parser, &ip_node)?;
		let count = parse_value(parser, &count_node)?;
		let cidr_bits = parse_value(parser, &bits_node)?;

		return Ok(CfnValue::Cidr {
			ip_block: Box::new(ip_block),
			count: Box::new(count),
			cidr_bits: Box::new(cidr_bits),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_CIDR",
		"Malformed !Cidr: expected array [ipBlock, count, cidrBits]".to_string(),
	))
}

fn parse_import_value<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	// ImportValue expects a string or intrinsic that returns a string
	// NOT an array or another ImportValue

	// Check if it's an array (invalid)
	if parser.array_len(node).is_some() {
		return Err(make_diagnostic(
			range,
			"WA2_CFN_MALFORMED_IMPORT_VALUE",
			"Malformed !ImportValue: expected string or intrinsic, not array".to_string(),
		));
	}

	let name = parse_value(parser, node)?;

	// Check if nested ImportValue (invalid)
	if matches!(name, CfnValue::ImportValue { .. }) {
		return Err(make_diagnostic(
			range,
			"WA2_CFN_MALFORMED_IMPORT_VALUE",
			"Malformed !ImportValue: cannot nest ImportValue inside another ImportValue"
				.to_string(),
		));
	}

	Ok(CfnValue::ImportValue {
		name: Box::new(name),
		range,
	})
}

fn parse_find_in_map<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	// FindInMap: [MapName, TopLevelKey, SecondLevelKey] or
	//            [MapName, TopLevelKey, SecondLevelKey, {DefaultValue: ...}]

	let len = parser.array_len(node);

	if (len == Some(3) || len == Some(4))
		&& let (Some(map_node), Some(top_node), Some(second_node)) = (
			parser.array_get(node, 0),
			parser.array_get(node, 1),
			parser.array_get(node, 2),
		) {
		let map_name = parse_value(parser, &map_node)?;
		let top_key = parse_value(parser, &top_node)?;
		let second_key = parse_value(parser, &second_node)?;

		// Parse optional 4th parameter (DefaultValue)
		let default_value = if len == Some(4) {
			if let Some(default_node) = parser.array_get(node, 3) {
				Some(Box::new(parse_value(parser, &default_node)?))
			} else {
				None
			}
		} else {
			None
		};

		return Ok(CfnValue::FindInMap {
			map_name: Box::new(map_name),
			top_key: Box::new(top_key),
			second_key: Box::new(second_key),
			default_value,
			range,
		});
	}

	Err(make_diagnostic(
        range,
        "WA2_CFN_MALFORMED_FINDINMAP",
        "Malformed !FindInMap: expected array [MapName, TopLevelKey, SecondLevelKey] or [MapName, TopLevelKey, SecondLevelKey, DefaultValue]".to_string(),
    ))
}

fn parse_to_json_string<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	let value = parse_value(parser, node)?;
	Ok(CfnValue::ToJsonString {
		value: Box::new(value),
		range,
	})
}

fn parse_length<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	let array = parse_value(parser, node)?;
	Ok(CfnValue::Length {
		array: Box::new(array),
		range,
	})
}

fn parse_contains<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Contains: [values, value] - checks if value is in values array
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(values_node), Some(value_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
	{
		let values = parse_value(parser, &values_node)?;
		let value = parse_value(parser, &value_node)?;

		return Ok(CfnValue::Contains {
			values: Box::new(values),
			value: Box::new(value),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_CONTAINS",
		"Malformed !Contains: expected array [values, value]".to_string(),
	))
}

fn parse_transform<P: CfnParser>(
	parser: &P,
	node: &P::Node,
	range: Range,
) -> ParseResult<CfnValue> {
	// Transform expects an object with Name and Parameters
	// { Name: "MacroName", Parameters: {...} }
	if let Some(_entries) = parser.object_entries(node) {
		let name_node = parser.object_get(node, "Name");
		let params_node = parser.object_get(node, "Parameters");

		let name = if let Some(n) = name_node {
			parse_value(parser, &n)?
		} else {
			return Err(make_diagnostic(
				range,
				"WA2_CFN_MALFORMED_TRANSFORM",
				"Malformed !Transform: missing required 'Name' field".to_string(),
			));
		};

		let parameters = if let Some(p) = params_node {
			parse_value(parser, &p)?
		} else {
			// Parameters is optional, default to empty object
			CfnValue::Object(HashMap::new(), range)
		};

		return Ok(CfnValue::Transform {
			name: Box::new(name),
			parameters: Box::new(parameters),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_TRANSFORM",
		"Malformed !Transform: expected object with Name and Parameters".to_string(),
	))
}

fn parse_join<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Join: [delimiter, values]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(delim_node), Some(values_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
		&& let Some(delimiter) = parser.node_as_string(&delim_node)
	{
		let values = parse_value(parser, &values_node)?;

		return Ok(CfnValue::Join {
			delimiter,
			values: Box::new(values),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_JOIN",
		"Malformed !Join: expected array [delimiter, values]".to_string(),
	))
}

fn parse_select<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Select: [index, list]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(index_node), Some(list_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
	{
		let index_value = parse_value(parser, &index_node)?;
		let list_value = parse_value(parser, &list_node)?;

		return Ok(CfnValue::Select {
			index: Box::new(index_value),
			list: Box::new(list_value),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_SELECT",
		"Malformed !Select: expected array [index, list]".to_string(),
	))
}

fn parse_if<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// If: [condition_name, value_if_true, value_if_false]
	if let Some(len) = parser.array_len(node)
		&& len == 3
		&& let (Some(cond_node), Some(true_node), Some(false_node)) = (
			parser.array_get(node, 0),
			parser.array_get(node, 1),
			parser.array_get(node, 2),
		) && let Some(condition_name) = parser.node_as_string(&cond_node)
	{
		let value_if_true = parse_value(parser, &true_node)?;
		let value_if_false = parse_value(parser, &false_node)?;

		return Ok(CfnValue::If {
			condition_name,
			value_if_true: Box::new(value_if_true),
			value_if_false: Box::new(value_if_false),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_IF",
		"Malformed !If: expected array [condition_name, value_if_true, value_if_false]".to_string(),
	))
}

fn parse_equals<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Equals: [value1, value2]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(left_node), Some(right_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
	{
		let left = parse_value(parser, &left_node)?;
		let right = parse_value(parser, &right_node)?;

		return Ok(CfnValue::Equals {
			left: Box::new(left),
			right: Box::new(right),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_EQUALS",
		"Malformed !Equals: expected array [value1, value2]".to_string(),
	))
}

fn parse_not<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Not: [condition]
	if let Some(len) = parser.array_len(node)
		&& len == 1
		&& let Some(cond_node) = parser.array_get(node, 0)
	{
		let condition = parse_value(parser, &cond_node)?;

		return Ok(CfnValue::Not {
			condition: Box::new(condition),
			range,
		});
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_NOT",
		"Malformed !Not: expected array with single condition [condition]".to_string(),
	))
}

fn parse_and<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// And: [cond1, cond2, ...] (2-10 conditions)
	if let Some(len) = parser.array_len(node) {
		if !(2..=10).contains(&len) {
			return Err(make_diagnostic(
				range,
				"WA2_CFN_MALFORMED_AND",
				format!("Malformed !And: expected 2-10 conditions, got {}", len),
			));
		}

		let mut conditions = Vec::new();
		for i in 0..len {
			if let Some(cond_node) = parser.array_get(node, i) {
				conditions.push(parse_value(parser, &cond_node)?);
			}
		}

		return Ok(CfnValue::And { conditions, range });
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_AND",
		"Malformed !And: expected array of conditions".to_string(),
	))
}

fn parse_or<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// Or: [cond1, cond2, ...] (2-10 conditions)
	if let Some(len) = parser.array_len(node) {
		if !(2..=10).contains(&len) {
			return Err(make_diagnostic(
				range,
				"WA2_CFN_MALFORMED_OR",
				format!("Malformed !Or: expected 2-10 conditions, got {}", len),
			));
		}

		let mut conditions = Vec::new();
		for i in 0..len {
			if let Some(cond_node) = parser.array_get(node, i) {
				conditions.push(parse_value(parser, &cond_node)?);
			}
		}

		return Ok(CfnValue::Or { conditions, range });
	}

	Err(make_diagnostic(
		range,
		"WA2_CFN_MALFORMED_OR",
		"Malformed !Or: expected array of conditions".to_string(),
	))
}

fn parse_sub<P: CfnParser>(parser: &P, node: &P::Node, range: Range) -> ParseResult<CfnValue> {
	// String form: "template with ${Var}"
	if let Some(template_str) = parser.node_as_string(node) {
		return Ok(CfnValue::Sub {
			template: Box::new(CfnValue::String(template_str, parser.node_range(node))),
			variables: None,
			range,
		});
	}

	// Array form: ["template", {Var1: val1, Var2: val2}]
	if let Some(len) = parser.array_len(node)
		&& len == 2
		&& let (Some(template_node), Some(vars_node)) =
			(parser.array_get(node, 0), parser.array_get(node, 1))
	{
		let template_value = parse_value(parser, &template_node)?;

		// Variables must be an object
		let variables = if parser.object_entries(&vars_node).is_some() {
			let vars_value = parse_value(parser, &vars_node)?;
			vars_value.as_object_values()
		} else {
			None
		};

		return Ok(CfnValue::Sub {
			template: Box::new(template_value),
			variables,
			range,
		});
	}

	// Intrinsic/other value form: !Sub wrapping another value that resolves to string
	let template_value = parse_value(parser, node)?;
	Ok(CfnValue::Sub {
		template: Box::new(template_value),
		variables: None,
		range,
	})
}

/// Validate CloudFormation logical ID format
/// Must be alphanumeric (a-zA-Z0-9)
fn validate_logical_id(id: &str, id_type: &str, range: Range, _uri: &Url) -> Option<Diagnostic> {
	// Allow Fn::ForEach::* pattern (AWS::LanguageExtensions)
	if id.starts_with("Fn::ForEach::") {
		return None;
	}

	// CloudFormation logical IDs must be alphanumeric
	if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
		return Some(Diagnostic {
			range,
			severity: Some(DiagnosticSeverity::ERROR),
			code: Some(NumberOrString::String("WA2_CFN_INVALID_LOGICAL_ID".into())),
			source: Some("wa2-lsp".into()),
			message: format!(
				"Invalid {} name `{}`. CloudFormation logical IDs must contain only alphanumeric characters (a-zA-Z0-9).",
				id_type, id
			),
			..Default::default()
		});
	}
	None
}
