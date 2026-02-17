//! CloudFormation Registry Schema Store
//!
//! Parses CloudFormation resource provider schemas (JSON Schema format)
//! instead of the older resource specification format.

use bytes::Bytes;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use thiserror::Error;
use zip::ZipArchive;

use super::spec_store::{
	AttributeName, AttributeShape, CollectionKind, PrimitiveType, PropertyName, PropertyShape,
	ResourceTypeDescriptor, ResourceTypeId, ShapeKind, SpecStore, TypeInfo,
};

#[derive(Debug, Error)]
pub enum RegistryStoreError {
	#[error("failed to extract ZIP: {0}")]
	Zip(#[from] zip::result::ZipError),

	#[error("failed to parse schema JSON for {resource}: {source}")]
	Json {
		resource: String,
		source: serde_json::Error,
	},

	#[error("I/O error reading schema: {0}")]
	Io(#[from] std::io::Error),
}

/// Build a SpecStore from CloudFormation Registry schemas (ZIP format)
pub fn build_from_zip(zip_bytes: &Bytes) -> Result<SpecStore, RegistryStoreError> {
	let cursor = Cursor::new(zip_bytes.as_ref());
	let mut archive = ZipArchive::new(cursor)?;

	let mut resource_types = HashMap::new();

	// Iterate through all files in the ZIP
	for i in 0..archive.len() {
		let mut file = archive.by_index(i)?;
		let filename = file.name().to_string();

		// Only process JSON files that look like resource schemas
		// Format: aws-s3-bucket.json, aws-ec2-instance.json, etc.
		if !filename.ends_with(".json") {
			continue;
		}

		// Read the schema JSON
		let mut contents = String::new();
		file.read_to_string(&mut contents)?;

		// Parse the schema
		match parse_resource_schema(&filename, &contents) {
			Ok(Some(descriptor)) => {
				resource_types.insert(descriptor.type_id.clone(), descriptor);
			}
			Ok(None) => {
				// Not a resource schema (might be a property type), skip
			}
			Err(e) => {
				// Log error but continue processing other schemas
				eprintln!("Warning: failed to parse {}: {}", filename, e);
			}
		}
	}

	Ok(SpecStore {
		resource_types,
		property_types: HashMap::new(), // Schemas handle this differently
	})
}

/// Resolve a $ref path and return the referenced definition's oneOf constraints
fn resolve_oneof_from_ref(
	ref_path: &str,
	definitions: &HashMap<String, PropertySchema>,
) -> Option<Vec<Vec<String>>> {
	// ref_path format: "#/definitions/LaunchTemplateSpecification"
	if !ref_path.starts_with("#/definitions/") {
		return None;
	}

	let def_name = ref_path.trim_start_matches("#/definitions/");
	let definition = definitions.get(def_name)?;

	// Extract oneOf required sets from the definition
	definition.one_of.as_ref().map(|constraints| {
		constraints
			.iter()
			.filter_map(|c| c.required.clone())
			.collect()
	})
}

/// Similarly for anyOf
fn resolve_anyof_from_ref(
	ref_path: &str,
	definitions: &HashMap<String, PropertySchema>,
) -> Option<Vec<Vec<String>>> {
	if !ref_path.starts_with("#/definitions/") {
		return None;
	}

	let def_name = ref_path.trim_start_matches("#/definitions/");
	let definition = definitions.get(def_name)?;

	definition.any_of.as_ref().map(|constraints| {
		constraints
			.iter()
			.filter_map(|c| c.required.clone())
			.collect()
	})
}

/// Parse a single resource provider schema JSON file
pub fn parse_resource_schema(
	filename: &str,
	json: &str,
) -> Result<Option<ResourceTypeDescriptor>, RegistryStoreError> {
	let schema: ResourceSchema =
		serde_json::from_str(json).map_err(|e| RegistryStoreError::Json {
			resource: filename.to_string(),
			source: e,
		})?;

	let type_id = ResourceTypeId(schema.type_name.clone());

	let mut properties = HashMap::new();
	let mut attributes = HashMap::new(); // NEW

	// Parse properties
	if let Some(props) = schema.properties {
		for (prop_name, prop_schema) in props {
			let type_info = parse_property_type(&prop_schema)?;

			let required = schema
				.required
				.as_ref()
				.map(|req| req.contains(&prop_name))
				.unwrap_or(false);

			// Check if this property is read-only (an attribute)
			let is_read_only = schema
				.read_only_properties
				.as_ref()
				.map(|ro| {
					ro.iter().any(|path| {
						// Paths are like "/properties/Arn"
						path == &format!("/properties/{}", prop_name)
					})
				})
				.unwrap_or(false);

			if is_read_only {
				// This is an attribute (read-only)
				attributes.insert(
					AttributeName(prop_name.clone()),
					AttributeShape {
						name: AttributeName(prop_name.clone()),
						type_info: Some(type_info),
						documentation_url: prop_schema.description.map(|_| {
							format!(
								"https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/{}.html",
								schema.type_name.replace("::", "-")
							)
						}),
					},
				);
			} else {
				// This is a regular property
				let shape = PropertyShape {
					name: PropertyName(prop_name.clone()),
					type_info,
					required,
					documentation_url: prop_schema.description.map(|_| {
						format!(
							"https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/{}.html",
							schema.type_name.replace("::", "-")
						)
					}),
					update_behavior: None,
					duplicates_allowed: false,

					// Check for oneOf - either directly on property or via $ref
					one_of_required: {
						// First check direct oneOf
						if let Some(constraints) = &prop_schema.one_of {
							Some(
								constraints
									.iter()
									.filter_map(|c| c.required.clone())
									.collect(),
							)
						}
						// Then check if property has $ref to a definition with oneOf
						else if let Some(ref_path) = &prop_schema.ref_path {
							if let Some(defs) = &schema.definitions {
								resolve_oneof_from_ref(ref_path, defs)
							} else {
								None
							}
						} else {
							None
						}
					},

					// Same for anyOf
					any_of_required: {
						if let Some(constraints) = &prop_schema.any_of {
							Some(
								constraints
									.iter()
									.filter_map(|c| c.required.clone())
									.collect(),
							)
						} else if let Some(ref_path) = &prop_schema.ref_path {
							if let Some(defs) = &schema.definitions {
								resolve_anyof_from_ref(ref_path, defs)
							} else {
								None
							}
						} else {
							None
						}
					},

					all_of_required: prop_schema.all_of.as_ref().map(|constraints| {
						constraints
							.iter()
							.filter_map(|c| c.required.clone())
							.collect()
					}),
				};

				properties.insert(PropertyName(prop_name), shape);
			}
		}
	}

	Ok(Some(ResourceTypeDescriptor {
		type_id,
		properties,
		attributes,
		documentation_url: Some(format!(
			"https://docs.aws.amazon.com/AWSCloudFormation/latest/UserGuide/{}.html",
			schema.type_name.replace("::", "-")
		)),
	}))
}

/// Parse type information from a property schema
fn parse_property_type(prop: &PropertySchema) -> Result<TypeInfo, RegistryStoreError> {
	// If no type field is present, default to Any
	// (These are often complex types with $ref, oneOf, etc.)
	let type_str = match &prop.r#type {
		Some(t) => t,
		None => {
			return Ok(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			});
		}
	};

	// Handle array types
	if type_str == "array"
		&& let Some(ref items) = prop.items
	{
		let item_type = parse_property_type(items)?;
		return Ok(TypeInfo {
			kind: item_type.kind,
			collection: CollectionKind::List,
		});
	}

	// Handle array types
	if let Some(ref type_str) = prop.r#type {
		if type_str == "array" {
			// List type
			if let Some(ref items) = prop.items {
				let item_type = parse_property_type(items)?;
				return Ok(TypeInfo {
					kind: item_type.kind,
					collection: CollectionKind::List,
				});
			}
		}

		if type_str == "object" {
			// Map type or complex object
			// For now, treat as Any
			return Ok(TypeInfo {
				kind: ShapeKind::Any,
				collection: CollectionKind::Scalar,
			});
		}

		// Primitive types
		let primitive = match type_str.as_str() {
			"string" => PrimitiveType::String,
			"integer" => PrimitiveType::Integer,
			"number" => PrimitiveType::Double,
			"boolean" => PrimitiveType::Boolean,
			_ => PrimitiveType::Other(type_str.clone()),
		};

		return Ok(TypeInfo {
			kind: ShapeKind::Primitive(primitive),
			collection: CollectionKind::Scalar,
		});
	}

	// Fallback to Any
	Ok(TypeInfo {
		kind: ShapeKind::Any,
		collection: CollectionKind::Scalar,
	})
}

//
// JSON Schema structures for CloudFormation resource provider schemas
//

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceSchema {
	type_name: String,
	_description: Option<String>,
	properties: Option<HashMap<String, PropertySchema>>,
	required: Option<Vec<String>>,
	read_only_properties: Option<Vec<String>>,

	// Capture definitions section
	definitions: Option<HashMap<String, PropertySchema>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PropertySchema {
	#[serde(default, deserialize_with = "deserialize_type_field")]
	r#type: Option<String>,
	description: Option<String>,
	items: Option<Box<PropertySchema>>,

	// JSON Schema validation keywords
	one_of: Option<Vec<SchemaConstraint>>,
	any_of: Option<Vec<SchemaConstraint>>,
	all_of: Option<Vec<SchemaConstraint>>,
	_required: Option<Vec<String>>,

	#[serde(rename = "$ref")]
	ref_path: Option<String>, // e.g., "#/definitions/LaunchTemplateSpecification"
}

/// A schema constraint (for oneOf, anyOf, allOf)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaConstraint {
	required: Option<Vec<String>>,
	_properties: Option<HashMap<String, PropertySchema>>,
}

/// Handle type field that can be either a string or array of strings
fn deserialize_type_field<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::de::Deserialize;

	#[derive(Deserialize)]
	#[serde(untagged)]
	enum TypeField {
		Single(String),
		Multiple(Vec<String>),
	}

	match Option::<TypeField>::deserialize(deserializer)? {
		None => Ok(None),
		Some(TypeField::Single(s)) => Ok(Some(s)),
		Some(TypeField::Multiple(types)) => {
			// Take the first type, or prefer "string" if present
			if types.contains(&"string".to_string()) {
				Ok(Some("string".to_string()))
			} else {
				Ok(types.first().cloned())
			}
		}
	}
}

#[test]
fn verify_ec2_instance_oneof_captured() {
	use std::fs;

	let cache_dir = dirs::cache_dir().unwrap().join("wa2/cfn-spec");
	let ec2_schema = cache_dir.join("aws-ec2-instance.json");
	eprintln!("file: {:?}", ec2_schema);
	let content = fs::read_to_string(&ec2_schema).expect("Can read EC2 schema");
	// the cfn_spec_*.json is actually a zip file
	// for this test to work we need to extract (normal use does not require that)
	// unzip cfn_spec_*.json

	// Parse the schema
	let parsed = parse_resource_schema("aws-ec2-instance.json", &content)
		.expect("Should parse")
		.expect("Should return descriptor");

	eprintln!("\n=== EC2::Instance Resource ===");

	// Check LaunchTemplate property
	let lt_prop_name = crate::iaac::cloudformation::spec_store::PropertyName("LaunchTemplate".to_string());
	if let Some(lt_prop) = parsed.properties.get(&lt_prop_name) {
		eprintln!("\nLaunchTemplate property found:");
		eprintln!("  Type: {:?}", lt_prop.type_info);
		eprintln!("  OneOf: {:?}", lt_prop.one_of_required);
		eprintln!("  AnyOf: {:?}", lt_prop.any_of_required);

		// Verify oneOf was captured
		assert!(
			lt_prop.one_of_required.is_some(),
			"LaunchTemplate should have oneOf constraint"
		);

		let one_of = lt_prop.one_of_required.as_ref().unwrap();
		eprintln!("\n  OneOf constraints:");
		for (i, constraint_set) in one_of.iter().enumerate() {
			eprintln!("    Option {}: {:?}", i + 1, constraint_set);
		}

		// Should have 2 options
		assert_eq!(one_of.len(), 2, "Should have 2 oneOf options");
	} else {
		panic!("LaunchTemplate property not found!");
	}
}
