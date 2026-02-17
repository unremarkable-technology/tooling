use std::collections::HashMap;

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

use crate::iaac::cloudformation::{
	cfn_ir::types::{CfnTemplate, CfnValue},
	code_utils::names,
	spec_store::{
		AttributeName, CollectionKind, PrimitiveType, PropertyName, ResourceTypeId, ShapeKind,
		SpecStore, TypeInfo,
	},
	symbol_table::SymbolTable,
	type_resolver,
};

// In helper to build diagnostics with suggestions
fn create_diagnostic_with_suggestion(
	range: Range,
	severity: DiagnosticSeverity,
	code: &str,
	message: String,
	suggestion: Option<String>,
) -> Diagnostic {
	let mut diag = Diagnostic {
		range,
		severity: Some(severity),
		code: Some(NumberOrString::String(code.into())),
		source: Some("wa2-lsp".into()),
		message,
		..Default::default()
	};

	// Store suggestion in data field as JSON
	if let Some(suggestion) = suggestion
		&& let Ok(json) = serde_json::to_value(serde_json::json!({
			"suggestion": suggestion
		})) {
		diag.data = Some(json);
	}

	diag
}

impl CfnTemplate {
	/// Validate this template against the CloudFormation spec
	pub fn validate_against_spec(&self, spec_store: &SpecStore) -> Vec<Diagnostic> {
		let mut diagnostics = Vec::new();

		// Build symbol table for intrinsic validation
		let symbols = SymbolTable::from_template(self);

		// Validate each resource
		for (logical_id, resource) in &self.resources {
			let type_id = ResourceTypeId(resource.resource_type.clone());
			let type_str = &resource.resource_type;

			// Skip ForEach constructs - but validate they have the required transform
			if type_str.starts_with("AWS::LanguageExtensions::ForEach") {
				// Check if AWS::LanguageExtensions transform is present
				let has_language_extensions = self
					.transform
					.as_ref()
					.map(|transforms| transforms.iter().any(|t| t == "AWS::LanguageExtensions"))
					.unwrap_or(false);

				if !has_language_extensions {
					diagnostics.push(Diagnostic {
						range: resource.logical_id_range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String(
							"WA2_CFN_FOREACH_REQUIRES_TRANSFORM".into(),
						)),
						source: Some("wa2-lsp".into()),
						message: format!(
							"Fn::ForEach `{}` requires Transform: AWS::LanguageExtensions",
							logical_id
						),
						..Default::default()
					});
				}
				continue; // Don't validate as regular resource
			}

			// Skip validation for SAM/Serverless resources (transform-based)
			if type_str.starts_with("AWS::Serverless::") || type_str.starts_with("AWS::SAM::") {
				continue;
			}

			// Check if resource type exists in spec
			if !spec_store.resource_types.contains_key(&type_id) {
				// Determine severity based on resource type pattern
				let (severity, message) = if type_str.starts_with("Custom::") {
					(
						DiagnosticSeverity::WARNING,
						format!(
							"Custom resource type `{}` cannot be validated (in resource `{}`)",
							type_str, logical_id
						),
					)
				} else if type_str.ends_with("::MODULE") {
					(
						DiagnosticSeverity::WARNING,
						format!(
							"Module resource type `{}` cannot be validated (in resource `{}`)",
							type_str, logical_id
						),
					)
				} else if type_str.contains("::") && !type_str.starts_with("AWS::") {
					// Third-party resource type (e.g., Initech::TPS::Report, Datadog::*, etc.)
					(
						DiagnosticSeverity::WARNING,
						format!(
							"Third-party resource type `{}` cannot be validated (in resource `{}`)",
							type_str, logical_id
						),
					)
				} else {
					(
						DiagnosticSeverity::ERROR,
						format!(
							"Unknown CloudFormation resource type: {} (in resource `{}`)",
							type_str, logical_id
						),
					)
				};

				diagnostics.push(Diagnostic {
					range: resource.type_range,
					severity: Some(severity),
					code: Some(NumberOrString::String(
						"WA2_CFN_UNKNOWN_RESOURCE_TYPE".into(),
					)),
					source: Some("wa2-lsp".into()),
					message,
					..Default::default()
				});
				continue;
			}

			// Get resource spec for property validation
			let resource_spec = match spec_store.resource_types.get(&type_id) {
				Some(spec) => spec,
				None => continue,
			};

			//println!("\n{} ({})\n======\n{:?}", logical_id, type_str, resource_spec);

			// Check for required properties
			for (prop_name, prop_spec) in &resource_spec.properties {
				if prop_spec.required && !resource.properties.contains_key(&prop_name.0) {
					// TECHDEBT: Skip known exceptions where AWS specs / cfn-lint tests disagree
					if Self::is_known_optional(&resource.resource_type, &prop_name.0) {
						continue;
					}

					diagnostics.push(Diagnostic {
						range: resource.logical_id_range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String(
							"WA2_CFN_REQUIRED_PROPERTY_MISSING".into(),
						)),
						source: Some("wa2-lsp".into()),
						message: format!(
							"Resource `{}` is missing required property `{}`",
							logical_id, prop_name.0
						),
						..Default::default()
					});
				}
			}

			// Check oneOf/anyOf/allOf constraints for complex properties
			for (prop_name, prop_spec) in &resource_spec.properties {
				// Skip if property not present - we already checked required
				if !resource.properties.contains_key(&prop_name.0) {
					continue;
				}

				let (prop_value, prop_key_range) = &resource.properties[&prop_name.0]; // ← Extract both

				// Validate oneOf constraints
				if let Some(ref one_of_sets) = prop_spec.one_of_required {
					Self::validate_one_of_constraint(
						one_of_sets,
						prop_value,      // ← Pass value
						*prop_key_range, // ← Pass key range
						logical_id,
						&prop_name.0,
						&mut diagnostics,
					);
				}

				// Validate anyOf constraints
				if let Some(ref any_of_sets) = prop_spec.any_of_required {
					Self::validate_any_of_constraint(
						any_of_sets,
						prop_value,
						*prop_key_range,
						logical_id,
						&prop_name.0,
						&mut diagnostics,
					);
				}
			}
			// Check for unknown properties and validate types
			for (prop_name, prop_value) in &resource.properties {
				let prop_id = PropertyName(prop_name.clone());

				// Extract value and key range from tuple
				let (value, key_range) = prop_value;

				//println!("\n\t{}\n\t====\n\t{:?}", prop_name, value);

				match resource_spec.properties.get(&prop_id) {
					Some(prop_spec) => {
						// Property exists - validate type
						Self::validate_property_type(
							value,
							&prop_spec.type_info,
							&symbols,
							spec_store,
							logical_id,
							prop_name,
							&mut diagnostics,
						);
					}
					// For unknown properties
					None => {
						let candidates = resource_spec.properties.keys().map(|k| k.0.as_str());
						let suggestion_data = names::find_closest(prop_name, candidates);

						let (message, suggestion) = if let Some((suggested, _)) = suggestion_data {
							(
								format!(
									"Unknown property `{}` for resource type `{}` (in resource `{}`). Did you mean `{}`?",
									prop_name, resource.resource_type, logical_id, suggested
								),
								Some(suggested.to_string()),
							)
						} else {
							(
								format!(
									"Unknown property `{}` for resource type `{}` (in resource `{}`)",
									prop_name, resource.resource_type, logical_id
								),
								None,
							)
						};

						diagnostics.push(create_diagnostic_with_suggestion(
							*key_range,
							DiagnosticSeverity::WARNING,
							"WA2_CFN_UNKNOWN_PROPERTY",
							message,
							suggestion,
						));
					}
				}

				// Validate intrinsic functions (existence checks)
				Self::validate_intrinsics(value, &symbols, &mut diagnostics, spec_store); // Pass spec_store
			}
		}

		// Validate conditions
		for condition in self.conditions.values() {
			// Validate the condition expression
			Self::validate_intrinsics(
				&condition.expression,
				&symbols,
				&mut diagnostics,
				spec_store,
			);
		}

		diagnostics
	}

	/// Check if a property is known to be incorrectly marked as required in AWS specs
	fn is_known_optional(resource_type: &str, property_name: &str) -> bool {
		matches!(
			(resource_type, property_name),
			// AWS::CloudFormation::Stack - StackName is auto-generated if not provided
			("AWS::CloudFormation::Stack", "StackName") // Add more exceptions here as we discover them
		)
	}

	/// Validate oneOf constraint: exactly one of the required property sets must be satisfied
	fn validate_one_of_constraint(
		one_of_sets: &[Vec<String>],
		prop_value: &CfnValue,
		prop_key_range: Range,
		resource_id: &str,
		prop_name: &str,
		diagnostics: &mut Vec<Diagnostic>,
	) {
		// Extract the properties present in this value (if it's an object)
		let present_props = match prop_value {
			CfnValue::Object(map, _) => map
				.keys()
				.cloned()
				.collect::<std::collections::HashSet<String>>(),
			_ => return, // oneOf only applies to object properties
		};

		// Find which constraint sets are satisfied
		let satisfied_sets: Vec<&Vec<String>> = one_of_sets
			.iter()
			.filter(|required_set| required_set.iter().all(|prop| present_props.contains(prop)))
			.collect();

		let satisfied_count = satisfied_sets.len();

		match satisfied_count {
			0 => {
				// None satisfied - show what's required
				let options = one_of_sets
					.iter()
					.map(|set| format!("[{}]", set.join(", ")))
					.collect::<Vec<_>>()
					.join(" OR ");

				diagnostics.push(Diagnostic {
					range: prop_key_range,
					severity: Some(DiagnosticSeverity::ERROR),
					code: Some(NumberOrString::String("WA2_CFN_ONEOF_VIOLATION".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Property `{}` in resource `{}` must satisfy exactly one of these requirements: {}",
						prop_name, resource_id, options
					),
					..Default::default()
				});
			}
			1 => {
				// Exactly one satisfied - valid!
			}
			_ => {
				// Multiple satisfied - show which ones AND what properties caused it
				let satisfied_list = satisfied_sets
					.iter()
					.map(|set| format!("[{}]", set.join(", ")))
					.collect::<Vec<_>>()
					.join(" AND ");

				// Find the conflicting properties (properties that appear in multiple sets)
				let mut prop_counts: HashMap<&str, usize> = HashMap::new();
				for set in &satisfied_sets {
					for prop in *set {
						*prop_counts.entry(prop.as_str()).or_insert(0) += 1;
					}
				}
				let conflicting: Vec<&str> = prop_counts
					.into_iter()
					.filter(|(_, count)| *count > 1)
					.map(|(prop, _)| prop)
					.collect();

				let conflict_msg = if !conflicting.is_empty() {
					format!(" Remove one of: {}", conflicting.join(", "))
				} else {
					String::new()
				};

				diagnostics.push(Diagnostic {
					range: prop_key_range,
					severity: Some(DiagnosticSeverity::ERROR),
					code: Some(NumberOrString::String("WA2_CFN_ONEOF_MULTIPLE".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Property `{}` in resource `{}` has conflicting properties. It satisfies: {}.{}",
						prop_name, resource_id, satisfied_list, conflict_msg
					),
					..Default::default()
				});
			}
		}
	}

	/// Validate anyOf constraint: at least one of the required property sets must be satisfied
	fn validate_any_of_constraint(
		any_of_sets: &[Vec<String>],
		prop_value: &CfnValue,
		prop_key_range: Range,
		resource_id: &str,
		prop_name: &str,
		diagnostics: &mut Vec<Diagnostic>,
	) {
		let present_props = match prop_value {
			CfnValue::Object(map, _) => map
				.keys()
				.cloned()
				.collect::<std::collections::HashSet<String>>(),
			_ => return,
		};

		// Check if at least one set is satisfied
		let any_satisfied = any_of_sets
			.iter()
			.any(|required_set| required_set.iter().all(|prop| present_props.contains(prop)));

		if !any_satisfied {
			let options = any_of_sets
				.iter()
				.map(|set| format!("[{}]", set.join(", ")))
				.collect::<Vec<_>>()
				.join(" OR ");

			diagnostics.push(Diagnostic {
				range: prop_key_range,
				severity: Some(DiagnosticSeverity::ERROR),
				code: Some(NumberOrString::String("WA2_CFN_ANYOF_VIOLATION".into())),
				source: Some("wa2-lsp".into()),
				message: format!(
					"Property `{}` in resource `{}` must satisfy at least one of these requirements: {}",
					prop_name, resource_id, options
				),
				..Default::default()
			});
		}
	}

	// Validate that a property value's type matches what the spec expects
	fn validate_property_type(
		value: &CfnValue,
		expected: &TypeInfo,
		symbols: &SymbolTable,
		spec: &SpecStore,
		resource_name: &str,
		property_name: &str,
		diagnostics: &mut Vec<Diagnostic>,
	) {
		let actual = type_resolver::resolve_type(value, symbols, spec);

		if let Some(actual_type) = actual {
			// Check if types match (with coercion rules)
			if !Self::types_compatible(expected, &actual_type, value) {
				diagnostics.push(Diagnostic {
					range: value.range(),
					severity: Some(DiagnosticSeverity::ERROR),
					code: Some(NumberOrString::String("WA2_CFN_TYPE_MISMATCH".into())),
					source: Some("wa2-lsp".into()),
					message: format!(
						"Type mismatch in resource `{}` property `{}`: expected {}, got {}",
						resource_name,
						property_name,
						Self::format_type(expected),
						Self::format_type(&actual_type)
					),
					..Default::default()
				});
			}
		}
	}

	fn types_compatible(expected: &TypeInfo, actual: &TypeInfo, value: &CfnValue) -> bool {
		// Check collection compatibility first
		match (&actual.collection, &expected.collection) {
			(CollectionKind::Scalar, CollectionKind::Scalar) => {}
			(CollectionKind::List, CollectionKind::List) => {}
			(CollectionKind::Map, CollectionKind::Map) => {}
			_ => return false, // Collection mismatch
		}

		// Check shape compatibility
		match (&actual.kind, &expected.kind) {
			// Exact match
			(ShapeKind::Primitive(a), ShapeKind::Primitive(e)) if a == e => true,
			// Any accepts anything
			(_, ShapeKind::Any) => true,
			(ShapeKind::Any, _) => true,

			// CloudFormation string → number coercion (for literal strings)
			(
				ShapeKind::Primitive(PrimitiveType::String),
				ShapeKind::Primitive(
					PrimitiveType::Integer | PrimitiveType::Double | PrimitiveType::Long,
				),
			) => {
				matches!(value, CfnValue::String(..))
			}

			// CloudFormation string → boolean coercion (for literal strings)
			(
				ShapeKind::Primitive(PrimitiveType::String),
				ShapeKind::Primitive(PrimitiveType::Boolean),
			) => {
				matches!(value, CfnValue::String(..))
			}

			// CloudFormation number → string coercion
			(
				ShapeKind::Primitive(
					PrimitiveType::Integer | PrimitiveType::Double | PrimitiveType::Long,
				),
				ShapeKind::Primitive(PrimitiveType::String),
			) => true,

			// Be lenient with Ref/GetAtt
			(ShapeKind::Primitive(PrimitiveType::String), ShapeKind::Primitive(_)) => {
				matches!(value, CfnValue::Ref { .. } | CfnValue::GetAtt { .. })
			}

			// Number type compatibility
			(
				ShapeKind::Primitive(PrimitiveType::Integer),
				ShapeKind::Primitive(PrimitiveType::Double),
			) => true,
			(
				ShapeKind::Primitive(PrimitiveType::Integer),
				ShapeKind::Primitive(PrimitiveType::Long),
			) => true,
			(
				ShapeKind::Primitive(PrimitiveType::Long),
				ShapeKind::Primitive(PrimitiveType::Double),
			) => true,
			(
				ShapeKind::Primitive(PrimitiveType::Long),
				ShapeKind::Primitive(PrimitiveType::Integer),
			) => true,
			(
				ShapeKind::Primitive(PrimitiveType::Double),
				ShapeKind::Primitive(PrimitiveType::Integer),
			) => true,
			(
				ShapeKind::Primitive(PrimitiveType::Double),
				ShapeKind::Primitive(PrimitiveType::Long),
			) => true,

			// Complex types must match exactly
			(ShapeKind::Complex(a), ShapeKind::Complex(e)) => a == e,
			_ => false,
		}
	}

	// Format a type for display in error messages
	fn format_type(type_info: &TypeInfo) -> String {
		let base = match &type_info.kind {
			ShapeKind::Primitive(p) => match p {
				PrimitiveType::String => "String",
				PrimitiveType::Integer => "Integer",
				PrimitiveType::Long => "Long",
				PrimitiveType::Double => "Double",
				PrimitiveType::Boolean => "Boolean",
				PrimitiveType::Timestamp => "Timestamp",
				PrimitiveType::Json => "Json",
				PrimitiveType::Other(s) => s.as_str(),
			},
			ShapeKind::Complex(id) => &id.0,
			ShapeKind::Any => "Any",
		};

		match type_info.collection {
			CollectionKind::Scalar => base.to_string(),
			CollectionKind::List => format!("List<{}>", base),
			CollectionKind::Map => format!("Map<{}>", base),
		}
	}

	/// Extract ${Variable} references from a Sub template string
	pub fn extract_sub_variables(template: &str) -> Vec<String> {
		let mut variables = Vec::new();
		let mut chars = template.chars().peekable();

		while let Some(ch) = chars.next() {
			if ch == '$' && chars.peek() == Some(&'{') {
				chars.next(); // consume '{'
				let mut var_name = String::new();

				while let Some(&ch) = chars.peek() {
					if ch == '}' {
						chars.next(); // consume '}'
						let trimmed = var_name.trim(); // ← Trim whitespace
						if !trimmed.is_empty() {
							variables.push(trimmed.to_string()); // ← Push trimmed version
						}
						break;
					}
					var_name.push(ch);
					chars.next();
				}
			}
		}

		variables
	}

	fn validate_intrinsics(
		value: &CfnValue,
		symbols: &SymbolTable,
		diagnostics: &mut Vec<Diagnostic>,
		spec: &SpecStore,
	) {
		match value {
			CfnValue::Ref { target, range } => {
				// For invalid Ref
				if !symbols.has_ref_target(target) {
					let mut candidates: Vec<&str> = Vec::new();
					candidates.extend(symbols.resources.keys().map(|s| s.as_str()));
					candidates.extend(symbols.parameters.keys().map(|s| s.as_str()));
					candidates.extend(symbols.pseudo_parameters.keys().map(|s| s.as_str()));

					let suggestion_data = names::find_closest(target, candidates.into_iter());

					let (message, suggestion) = if let Some((suggested, _)) = suggestion_data {
						(
							format!(
								"!Ref target `{}` does not exist. Did you mean `{}`?",
								target, suggested
							),
							Some(suggested.to_string()),
						)
					} else {
						(
							format!(
								"!Ref target `{}` does not exist. Must reference a resource, parameter, or pseudo-parameter.",
								target
							),
							None,
						)
					};

					diagnostics.push(create_diagnostic_with_suggestion(
						*range,
						DiagnosticSeverity::ERROR,
						"WA2_CFN_INVALID_REF",
						message,
						suggestion,
					));
				}
			}
			CfnValue::GetAtt {
				target,
				attribute,
				range,
			} => {
				// Check if the resource exists
				if !symbols.has_resource(target) {
					diagnostics.push(Diagnostic {
						range: *range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String(
							"WA2_CFN_INVALID_GETATT_RESOURCE".into(),
						)),
						source: Some("wa2-lsp".into()),
						message: format!("!GetAtt target resource `{}` does not exist.", target),
						..Default::default()
					});
					return; // Can't validate attribute if resource doesn't exist
				}
				// Validate attribute exists for the resource type
				if let Some(resource_entry) = symbols.resources.get(target) {
					let type_id = ResourceTypeId(resource_entry.resource_type.clone());
					if let Some(resource_spec) = spec.resource_types.get(&type_id) {
						// Special case: AWS::CloudFormation::Stack allows Outputs.* attributes
						let is_stack_output = resource_entry.resource_type
							== "AWS::CloudFormation::Stack"
							&& attribute.starts_with("Outputs.");

						// Special case: Custom resources allow any attribute
						let is_custom_resource = resource_entry.resource_type
							== "AWS::CloudFormation::CustomResource"
							|| resource_entry.resource_type.starts_with("Custom::");

						if !is_stack_output && !is_custom_resource {
							let attr_name = AttributeName(attribute.clone());
							// For GetAtt attributes
							if !resource_spec.attributes.contains_key(&attr_name) {
								let candidates =
									resource_spec.attributes.keys().map(|k| k.0.as_str());
								let suggestion_data = names::find_closest(attribute, candidates);
								let (message, suggestion) = if let Some((suggested, _)) =
									suggestion_data
								{
									(
										format!(
											"!GetAtt attribute `{}` does not exist on resource type `{}`. Did you mean `{}`?",
											attribute, resource_entry.resource_type, suggested
										),
										Some(serde_json::json!({
											"kind": "getatt",
											"target": target,
											"attribute": suggested
										})),
									)
								} else {
									(
										format!(
											"!GetAtt attribute `{}` does not exist on resource type `{}`. This resource type has no attributes.",
											attribute, resource_entry.resource_type
										),
										None,
									)
								};
								let mut diag = Diagnostic {
									range: *range,
									severity: Some(DiagnosticSeverity::ERROR),
									code: Some(NumberOrString::String(
										"WA2_CFN_INVALID_GETATT_ATTRIBUTE".into(),
									)),
									source: Some("wa2-lsp".into()),
									message,
									..Default::default()
								};
								if let Some(sugg) = suggestion {
									diag.data = Some(sugg);
								}
								diagnostics.push(diag);
							}
						}
					}
				}
			}
			CfnValue::Sub {
				template,
				variables,
				range,
			} => {
				// Validate the template value itself (could be an intrinsic)
				Self::validate_intrinsics(template, symbols, diagnostics, spec);

				// If template is a string, extract and validate ${Variable} references
				if let CfnValue::String(template_str, _) = &**template {
					let var_refs = Self::extract_sub_variables(template_str);
					for var_ref in var_refs {
						// Check if variable exists in explicit variables map
						if let Some(vars) = variables
							&& vars.contains_key(&var_ref)
						{
							continue; // Found in explicit variables
						}
						// Check if it's a valid Ref target (resource/parameter/pseudo-param)
						// OR a GetAtt-style reference with dot notation (ResourceName.AttributeName)
						let is_valid = if var_ref.contains('.') {
							// Dot notation: ${Resource.Attribute}
							let parts: Vec<&str> = var_ref.splitn(2, '.').collect();
							if parts.len() == 2 {
								let resource_name = parts[0];
								let attribute_name = parts[1];

								// Check resource exists
								if !symbols.has_resource(resource_name) {
									false
								} else {
									// Resource exists - validate attribute if we have the spec
									if let Some(resource_entry) =
										symbols.resources.get(resource_name)
									{
										let type_id =
											ResourceTypeId(resource_entry.resource_type.clone());

										if let Some(resource_spec) =
											spec.resource_types.get(&type_id)
										{
											// Special cases where any attribute is allowed
											let is_stack_output = resource_entry.resource_type
												== "AWS::CloudFormation::Stack"
												&& attribute_name.starts_with("Outputs.");
											let is_custom_resource = resource_entry.resource_type
												== "AWS::CloudFormation::CustomResource"
												|| resource_entry
													.resource_type
													.starts_with("Custom::");

											if is_stack_output || is_custom_resource {
												true
											} else {
												// Validate attribute exists
												let attr_name =
													AttributeName(attribute_name.to_string());
												resource_spec.attributes.contains_key(&attr_name)
											}
										} else {
											// No spec for resource type - assume valid
											true
										}
									} else {
										// Shouldn't happen since we checked has_resource
										false
									}
								}
							} else {
								false
							}
						} else {
							// Simple reference: ${Resource} or ${Parameter}
							symbols.has_ref_target(&var_ref)
						};

						if !is_valid {
							let mut candidates: Vec<&str> = Vec::new();
							candidates.extend(symbols.resources.keys().map(|s| s.as_str()));
							candidates.extend(symbols.parameters.keys().map(|s| s.as_str()));
							candidates.extend(symbols.pseudo_parameters.keys().map(|s| s.as_str()));
							let suggestion_data = crate::iaac::cloudformation::code_utils::names::find_closest(
								&var_ref,
								candidates.into_iter(),
							);
							let message = if let Some((suggested, _)) = suggestion_data {
								format!(
									"!Sub variable `${{{}}}` does not exist. Did you mean `{}`?",
									var_ref, suggested
								)
							} else {
								format!(
									"!Sub variable `${{{}}}` does not exist. Must reference a resource, parameter, or pseudo-parameter.",
									var_ref
								)
							};
							diagnostics.push(Diagnostic {
								range: *range,
								severity: Some(DiagnosticSeverity::ERROR),
								code: Some(NumberOrString::String(
									"WA2_CFN_INVALID_SUB_VARIABLE".into(),
								)),
								source: Some("wa2-lsp".into()),
								message,
								..Default::default()
							});
						}
					}
				}

				// Recursively validate variable values if present
				if let Some(vars) = variables {
					for val in vars.values() {
						Self::validate_intrinsics(val, symbols, diagnostics, spec);
					}
				}
			}
			CfnValue::GetAZs { region, .. } => {
				// Recursively validate the region value
				Self::validate_intrinsics(region, symbols, diagnostics, spec);
			}
			CfnValue::Join { values, .. } => {
				// Recursively validate all values in the array
				Self::validate_intrinsics(values, symbols, diagnostics, spec);
			}
			CfnValue::Select { index, list, range } => {
				// Validate index (should be a number or resolve to a number)
				if let Some(index_type) = type_resolver::resolve_type(index, symbols, spec) {
					// Check if index is numeric
					let is_numeric = matches!(
						index_type.kind,
						ShapeKind::Primitive(PrimitiveType::Integer)
							| ShapeKind::Primitive(PrimitiveType::Long)
							| ShapeKind::Primitive(PrimitiveType::Double)
					);

					if !is_numeric {
						diagnostics.push(Diagnostic {
							range: *range,
							severity: Some(DiagnosticSeverity::ERROR),
							code: Some(NumberOrString::String(
								"WA2_CFN_INVALID_SELECT_INDEX".into(),
							)),
							source: Some("wa2-lsp".into()),
							message: "!Select index must be a number".to_string(),
							..Default::default()
						});
					}
				}

				// Validate list (should be a list or resolve to a list)
				if let Some(list_type) = type_resolver::resolve_type(list, symbols, spec)
					&& list_type.collection != CollectionKind::List
				{
					diagnostics.push(Diagnostic {
						range: *range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String("WA2_CFN_INVALID_SELECT_LIST".into())),
						source: Some("wa2-lsp".into()),
						message: "!Select second argument must be a list".to_string(),
						..Default::default()
					});
				}

				// Recursively validate index and list
				Self::validate_intrinsics(index, symbols, diagnostics, spec);
				Self::validate_intrinsics(list, symbols, diagnostics, spec);
			}
			CfnValue::If {
				condition_name,
				value_if_true,
				value_if_false,
				range,
			} => {
				// Check if the condition exists
				if !symbols.has_condition(condition_name) {
					let candidates = symbols.conditions.keys().map(|s| s.as_str());
					let suggestion_data =
						crate::iaac::cloudformation::code_utils::names::find_closest(condition_name, candidates);

					let message = if let Some((suggested, _)) = suggestion_data {
						format!(
							"!If condition `{}` does not exist. Did you mean `{}`?",
							condition_name, suggested
						)
					} else {
						format!(
							"!If condition `{}` does not exist. Must reference a condition defined in Conditions section.",
							condition_name
						)
					};

					diagnostics.push(Diagnostic {
						range: *range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String(
							"WA2_CFN_INVALID_IF_CONDITION".into(),
						)),
						source: Some("wa2-lsp".into()),
						message,
						..Default::default()
					});
				}

				// Recursively validate both branches
				Self::validate_intrinsics(value_if_true, symbols, diagnostics, spec);
				Self::validate_intrinsics(value_if_false, symbols, diagnostics, spec);
			}
			CfnValue::Equals { left, right, .. } => {
				// Recursively validate both operands
				Self::validate_intrinsics(left, symbols, diagnostics, spec);
				Self::validate_intrinsics(right, symbols, diagnostics, spec);
			}
			CfnValue::Not { condition, .. } => {
				// Recursively validate the condition
				Self::validate_intrinsics(condition, symbols, diagnostics, spec);
			}
			CfnValue::And { conditions, .. } | CfnValue::Or { conditions, .. } => {
				// Recursively validate all conditions
				for condition in conditions {
					Self::validate_intrinsics(condition, symbols, diagnostics, spec);
				}
			}
			CfnValue::Condition {
				condition_name,
				range,
			} => {
				// Validate that the referenced condition exists
				if !symbols.has_condition(condition_name) {
					let candidates = symbols.conditions.keys().map(|s| s.as_str());
					let suggestion_data =
						crate::iaac::cloudformation::code_utils::names::find_closest(condition_name, candidates);

					let message = if let Some((suggested, _)) = suggestion_data {
						format!(
							"!Condition references `{}` which does not exist. Did you mean `{}`?",
							condition_name, suggested
						)
					} else {
						format!(
							"!Condition references `{}` which does not exist. Must reference a condition defined in Conditions section.",
							condition_name
						)
					};

					diagnostics.push(Diagnostic {
						range: *range,
						severity: Some(DiagnosticSeverity::ERROR),
						code: Some(NumberOrString::String(
							"WA2_CFN_INVALID_CONDITION_REF".into(),
						)),
						source: Some("wa2-lsp".into()),
						message,
						..Default::default()
					});
				}
			}
			CfnValue::Base64 { value, .. } => {
				Self::validate_intrinsics(value, symbols, diagnostics, spec);
			}
			CfnValue::Split { source, .. } => {
				Self::validate_intrinsics(source, symbols, diagnostics, spec);
			}
			CfnValue::Cidr {
				ip_block,
				count,
				cidr_bits,
				..
			} => {
				Self::validate_intrinsics(ip_block, symbols, diagnostics, spec);
				Self::validate_intrinsics(count, symbols, diagnostics, spec);
				Self::validate_intrinsics(cidr_bits, symbols, diagnostics, spec);
			}
			CfnValue::ImportValue { name, .. } => {
				Self::validate_intrinsics(name, symbols, diagnostics, spec);
			}
			CfnValue::FindInMap {
				map_name,
				top_key,
				second_key,
				default_value,
				..
			} => {
				Self::validate_intrinsics(map_name, symbols, diagnostics, spec);
				Self::validate_intrinsics(top_key, symbols, diagnostics, spec);
				Self::validate_intrinsics(second_key, symbols, diagnostics, spec);
				if let Some(default) = default_value {
					Self::validate_intrinsics(default, symbols, diagnostics, spec);
				}
			}
			CfnValue::ToJsonString { value, .. } => {
				Self::validate_intrinsics(value, symbols, diagnostics, spec);
			}
			CfnValue::Length { array, .. } => {
				Self::validate_intrinsics(array, symbols, diagnostics, spec);
			}
			CfnValue::Contains { values, value, .. } => {
				Self::validate_intrinsics(values, symbols, diagnostics, spec);
				Self::validate_intrinsics(value, symbols, diagnostics, spec);
			}
			CfnValue::Transform {
				name, parameters, ..
			} => {
				Self::validate_intrinsics(name, symbols, diagnostics, spec);
				Self::validate_intrinsics(parameters, symbols, diagnostics, spec);
			}
			CfnValue::Array(items, _) => {
				for item in items {
					Self::validate_intrinsics(item, symbols, diagnostics, spec); // Pass spec
				}
			}
			CfnValue::Object(map, _) => {
				for (val, _key_range) in map.values() {
					// ← Destructure the tuple
					Self::validate_intrinsics(val, symbols, diagnostics, spec);
				}
			}
			CfnValue::String(..)
			| CfnValue::Number(..)
			| CfnValue::Bool(..)
			| CfnValue::Null(..) => {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_oneof_error_spans() {
		let yaml = r#"
AWSTemplateFormatVersion: '2010-09-09'
Resources:
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      ImageId: ami-12345678
      InstanceType: t2.micro
      LaunchTemplate:
        LaunchTemplateName: "my-template"
        LaunchTemplateId: "lt-12345"
        Version: "1"
"#;

		eprintln!("\n=== Testing OneOf Error Spans ===");
		eprintln!("YAML content:");
		for (i, line) in yaml.lines().enumerate() {
			eprintln!("{:3}: {}", i, line);
		}

		// Parse template
		let uri = url::Url::parse("file:///test.yaml").unwrap();
		let template = CfnTemplate::from_yaml(yaml, &uri).expect("Should parse");

		eprintln!("\nParsed resources:");
		for (name, resource) in &template.resources {
			eprintln!("  {}: type={}", name, resource.resource_type);

			if let Some(lt) = resource.properties.get("LaunchTemplate") {
				let (lt_value, lt_key_range) = lt;
				eprintln!("    LaunchTemplate value range: {:?}", lt_value.range());
				eprintln!("    LaunchTemplate key range: {:?}", lt_key_range);

				if let CfnValue::Object(map, _) = lt_value {
					for (key, (val, val_key_range)) in map {
						// ← Changed: map contains (CfnValue, Range)
						eprintln!(
							"      {}: value range={:?}, key range={:?}",
							key,
							val.range(),
							val_key_range
						);
					}
				}
			}
		}

		eprintln!("\n=== Analysis ===");
		eprintln!("Expected error should highlight:");
		eprintln!("  From line 9 (LaunchTemplate:)");
		eprintln!("  To line 12 (Version: \"1\")");
		eprintln!("\nActual LaunchTemplate object range shown above.");
	}
}

#[test]
fn test_oneof_validation_line_numbers() {
	use std::fs;

	let yaml = r#"AWSTemplateFormatVersion: '2010-09-09'
Resources:
  MyInstance:
    Type: AWS::EC2::Instance
    Properties:
      ImageId: ami-12345678
      InstanceType: t2.micro
      LaunchTemplate:
        LaunchTemplateName: "my-template"
        LaunchTemplateId: "lt-12345"
        Version: "1"
"#;

	// Parse and validate
	let uri = url::Url::parse("file:///test.yaml").unwrap();
	let template = CfnTemplate::from_yaml(yaml, &uri).expect("Should parse");

	// Need spec store - load from cache
	let cache_dir = dirs::cache_dir().unwrap().join("wa2/cfn-spec");

	// Load spec - we'll just check one file
	let ec2_schema = cache_dir.join("aws-ec2-instance.json");
	eprintln!("file: {:?}", ec2_schema);
	let content = fs::read_to_string(&ec2_schema).expect("Schema exists");

	let parsed =
		crate::iaac::cloudformation::registry_store::parse_resource_schema("aws-ec2-instance.json", &content)
			.expect("Parse")
			.expect("Descriptor");

	// Create a minimal spec store
	use crate::iaac::cloudformation::spec_store::{ResourceTypeId, SpecStore};
	use std::collections::HashMap;

	let mut resources = HashMap::new();
	resources.insert(ResourceTypeId("AWS::EC2::Instance".to_string()), parsed);

	let spec = SpecStore {
		resource_types: resources,
		property_types: HashMap::new(),
	};

	// Validate
	let diagnostics = template.validate_against_spec(&spec);

	eprintln!("\n=== Diagnostics ===");
	for diag in &diagnostics {
		eprintln!("Code: {:?}", diag.code);
		eprintln!("Message: {}", diag.message);
		eprintln!("Range: {:?}", diag.range);
		eprintln!(
			"  Start line: {} (0-indexed) = {} (1-indexed in VSCode)",
			diag.range.start.line,
			diag.range.start.line + 1
		);
		eprintln!(
			"  End line: {} (0-indexed) = {} (1-indexed in VSCode)",
			diag.range.end.line,
			diag.range.end.line + 1
		);
		eprintln!();
	}
}
