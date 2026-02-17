//! Type resolution for CloudFormation values and intrinsic functions
//!
//! Given a CfnValue, resolves what TypeInfo it produces at runtime.

use crate::iaac::cloudformation::cfn_ir::types::CfnValue;
use crate::iaac::cloudformation::spec_store::CollectionKind;

use super::spec_store::{
	AttributeName, PrimitiveType, ResourceTypeId, ShapeKind, SpecStore, TypeInfo,
};
use super::symbol_table::SymbolTable;

/// Resolve the type that a CfnValue will produce at runtime
pub fn resolve_type(value: &CfnValue, symbols: &SymbolTable, spec: &SpecStore) -> Option<TypeInfo> {
	match value {
		CfnValue::String(..) => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::Scalar,
		}),

		CfnValue::Number(..) => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::Double),
			collection: CollectionKind::Scalar,
		}),

		CfnValue::Bool(..) => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::Boolean),
			collection: CollectionKind::Scalar,
		}),

		CfnValue::Null(..) => None,

		CfnValue::Array(items, _) => {
			// For now, return List of first item's type
			// TODO: Handle heterogeneous arrays
			items
				.first()
				.and_then(|item| resolve_type(item, symbols, spec))
				.map(|mut item_type| {
					item_type.collection = CollectionKind::List;
					item_type
				})
		}

		CfnValue::Object(..) => {
			// Objects are complex - for now return Any
			Some(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::Ref { target, .. } => resolve_ref_type(target, symbols, spec),

		CfnValue::GetAtt {
			target, attribute, ..
		} => resolve_getatt_type(target, attribute, symbols, spec),

		CfnValue::Sub { .. } => {
			// Sub always returns String
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::GetAZs { .. } => {
			// GetAZs always returns List<String>
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::List,
			})
		}

		CfnValue::Join { .. } => {
			// Join always returns String
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::Select { list, .. } => {
			// Select returns the element type of the list
			// If we can resolve the list type, extract the element type
			if let Some(list_type) = resolve_type(list, symbols, spec) {
				match list_type.collection {
					CollectionKind::List => {
						// Return the element type as scalar
						Some(TypeInfo {
							kind: list_type.kind,
							collection: CollectionKind::Scalar,
						})
					}
					_ => {
						// List is not actually a list - return Any
						Some(TypeInfo {
							kind: ShapeKind::Any,
							collection: CollectionKind::Scalar,
						})
					}
				}
			} else {
				// Can't determine list type - return Any
				Some(TypeInfo {
					kind: ShapeKind::Any,
					collection: CollectionKind::Scalar,
				})
			}
		}

		CfnValue::If {
			value_if_true,
			value_if_false,
			..
		} => {
			// If returns the union of both branch types
			// For simplicity, if both branches have the same type, return that type
			// Otherwise return Any
			let true_type = resolve_type(value_if_true, symbols, spec);
			let false_type = resolve_type(value_if_false, symbols, spec);

			match (true_type, false_type) {
				(Some(t1), Some(t2)) if t1.kind == t2.kind && t1.collection == t2.collection => {
					Some(t1)
				}
				_ => Some(TypeInfo {
					kind: ShapeKind::Any,
					collection: CollectionKind::Scalar,
				}),
			}
		}

		CfnValue::Equals { .. } => {
			// Equals returns a condition (boolean), but CloudFormation doesn't have
			// a Boolean property type. These are only used in Conditions and !If.
			// Return None since they don't produce usable property values.
			None
		}

		CfnValue::Not { .. } => {
			// Not returns a condition (boolean)
			// Return None since they don't produce usable property values
			None
		}

		CfnValue::And { .. } | CfnValue::Or { .. } => {
			// And/Or return conditions (boolean)
			// Return None since they don't produce usable property values
			None
		}

		CfnValue::Condition { .. } => {
			// Condition returns a condition (boolean)
			// Return None since they don't produce usable property values
			None
		}

		CfnValue::Base64 { .. } => {
			// Base64 always returns String
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::Split { .. } => {
			// Split always returns List<String>
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::List,
			})
		}

		CfnValue::Cidr { .. } => {
			// Cidr always returns List<String> (list of CIDR blocks)
			Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::List,
			})
		}

		CfnValue::ImportValue { .. } => {
			// ImportValue can return any type depending on the export
			// Without knowing the export schema, return Any
			Some(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::FindInMap { .. } => {
			// FindInMap returns whatever type is stored in the mapping
			// Without analyzing the actual mapping values, return Any
			Some(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			})
		}

		CfnValue::ToJsonString { .. } => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::Scalar,
		}),

		CfnValue::Length { .. } => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::Integer),
			collection: CollectionKind::Scalar,
		}),

		CfnValue::Contains { .. } => {
			// Contains returns Boolean (condition)
			None // Like other condition functions, doesn't produce usable property values
		}

		CfnValue::Transform { .. } => {
			// Transform can return any type depending on the macro
			Some(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			})
		}
	}
}

/// Resolve the type returned by !Ref
fn resolve_ref_type(target: &str, symbols: &SymbolTable, spec: &SpecStore) -> Option<TypeInfo> {
	// Check if it's a resource
	if let Some(resource_entry) = symbols.resources.get(target) {
		// Look up the resource type in spec to find its Ref return type
		let type_id = ResourceTypeId(resource_entry.resource_type.clone());
		if let Some(resource_spec) = spec.resource_types.get(&type_id) {
			// Check if there's a special "Ref" attribute
			let ref_attr = AttributeName("Ref".to_string());
			if let Some(attr_shape) = resource_spec.attributes.get(&ref_attr) {
				return attr_shape.type_info.clone();
			}

			// Default: Ref returns the logical ID as a String
			return Some(TypeInfo {
				kind: ShapeKind::Primitive(PrimitiveType::String),
				collection: CollectionKind::Scalar,
			});
		}
	}

	// Check if it's a parameter
	if let Some(param_entry) = symbols.parameters.get(target) {
		// Parse the parameter type string to TypeInfo
		return parse_parameter_type(&param_entry.declared_type);
	}

	// Check if it's a pseudo-parameter
	if let Some(pseudo) = symbols.pseudo_parameters.get(target) {
		return pseudo_parameter_type(&pseudo.name);
	}

	None
}

/// Resolve the type returned by !GetAtt
fn resolve_getatt_type(
	target: &str,
	attribute: &str,
	symbols: &SymbolTable,
	spec: &SpecStore,
) -> Option<TypeInfo> {
	// Get the resource entry
	let resource_entry = symbols.resources.get(target)?;

	// Look up the resource type in spec
	let type_id = ResourceTypeId(resource_entry.resource_type.clone());
	let resource_spec = spec.resource_types.get(&type_id)?;

	// Look up the attribute
	let attr_name = AttributeName(attribute.to_string());
	let attr_shape = resource_spec.attributes.get(&attr_name)?;

	attr_shape.type_info.clone()
}

/// Parse a CloudFormation parameter type string into TypeInfo
fn parse_parameter_type(param_type: &str) -> Option<TypeInfo> {
	match param_type {
		"String" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::Scalar,
		}),
		"Number" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::Double),
			collection: CollectionKind::Scalar,
		}),
		"CommaDelimitedList" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::List,
		}),
		s if s.starts_with("List<") && s.ends_with('>') => {
			// Extract inner type, e.g., "List<Number>" -> "Number"
			let inner = &s[5..s.len() - 1];
			parse_parameter_type(inner).map(|mut inner_type| {
				inner_type.collection = CollectionKind::List;
				inner_type
			})
		}
		// AWS-specific parameter types (returns String)
		"AWS::EC2::AvailabilityZone::Name"
		| "AWS::EC2::Image::Id"
		| "AWS::EC2::Instance::Id"
		| "AWS::EC2::KeyPair::KeyName"
		| "AWS::EC2::SecurityGroup::Id"
		| "AWS::EC2::Subnet::Id"
		| "AWS::EC2::VPC::Id"
		| "AWS::Route53::HostedZone::Id" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::Scalar,
		}),
		_ => None,
	}
}

/// Get the type for a pseudo-parameter
fn pseudo_parameter_type(name: &str) -> Option<TypeInfo> {
	match name {
		"AWS::AccountId" | "AWS::Region" | "AWS::StackId" | "AWS::StackName" | "AWS::URLSuffix"
		| "AWS::Partition" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::Scalar,
		}),
		"AWS::NotificationARNs" => Some(TypeInfo {
			kind: ShapeKind::Primitive(PrimitiveType::String),
			collection: CollectionKind::List,
		}),
		"AWS::NoValue" => None, // Special - removes the property
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::iaac::cloudformation::cfn_ir::types::CfnTemplate;
	use url::Url;

	fn test_uri() -> Url {
		Url::parse("file:///test.yaml").unwrap()
	}

	#[test]
	fn resolve_scalar_types() {
		let spec = SpecStore {
			resource_types: Default::default(),
			property_types: Default::default(),
		};
		let symbols = SymbolTable {
			resources: Default::default(),
			parameters: Default::default(),
			conditions: Default::default(),
			pseudo_parameters: Default::default(),
		};

		// String
		let text = r#"
Resources:
  Test:
    Type: AWS::S3::Bucket
    Properties:
      Name: "test"
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let resource = &template.resources["Test"];
		let (name_value, _) = &resource.properties["Name"];

		let resolved = resolve_type(name_value, &symbols, &spec).unwrap();
		assert!(matches!(
			resolved.kind,
			ShapeKind::Primitive(PrimitiveType::String)
		));
	}

	#[test]
	fn resolve_ref_to_pseudo_parameter() {
		let spec = SpecStore {
			resource_types: Default::default(),
			property_types: Default::default(),
		};

		let text = r#"
Resources:
  Test:
    Type: AWS::S3::Bucket
    Properties:
      Name: !Ref AWS::Region
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		let resource = &template.resources["Test"];
		let (name_value, _) = &resource.properties["Name"];

		let resolved = resolve_type(name_value, &symbols, &spec).unwrap();
		assert!(matches!(
			resolved.kind,
			ShapeKind::Primitive(PrimitiveType::String)
		));
	}

	#[test]
	fn resolve_ref_to_parameter() {
		let spec = SpecStore {
			resource_types: Default::default(),
			property_types: Default::default(),
		};

		let text = r#"
Parameters:
  Environment:
    Type: String

Resources:
  Test:
    Type: AWS::S3::Bucket
    Properties:
      Name: !Ref Environment
"#;
		let template = CfnTemplate::from_yaml(text, &test_uri()).unwrap();
		let symbols = SymbolTable::from_template(&template);

		let resource = &template.resources["Test"];
		let (name_value, _) = &resource.properties["Name"];

		let resolved = resolve_type(name_value, &symbols, &spec).unwrap();
		assert!(matches!(
			resolved.kind,
			ShapeKind::Primitive(PrimitiveType::String)
		));
	}
}
