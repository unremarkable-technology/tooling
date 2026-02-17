// server/wa2lsp/src/spec_store.rs

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

/// Public, read-only store for the parsed CloudFormation
/// Resource Specification.
#[derive(Debug)]
pub struct SpecStore {
	pub resource_types: HashMap<ResourceTypeId, ResourceTypeDescriptor>,
	pub property_types: HashMap<PropertyTypeId, PropertyTypeDescriptor>,
}

impl SpecStore {
	/// Build a SpecStore from the raw JSON bytes of the AWS
	/// CloudFormation Resource Specification.
	pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, SpecStoreError> {
		let raw: RawSpec = serde_json::from_slice(bytes)?;

		let mut resource_types = HashMap::new();
		let mut property_types = HashMap::new();

		// Convert PropertyTypes first so ResourceTypes can refer to them.
		for (name, raw_pt) in raw.property_types.unwrap_or_default() {
			let type_id = PropertyTypeId(name.clone());

			let mut properties = HashMap::new();
			if let Some(raw_props) = raw_pt.properties {
				for (prop_name, raw_prop) in raw_props {
					let type_info = TypeInfo::from_raw_property(&raw_prop).map_err(|reason| {
						SpecStoreError::InvalidPropertyType {
							resource_type: name.clone(),
							property_name: prop_name.clone(),
							reason,
						}
					})?;

					let shape = PropertyShape {
						name: PropertyName(prop_name.clone()),
						type_info,
						required: raw_prop.required.unwrap_or(false),
						documentation_url: raw_prop.documentation,
						update_behavior: raw_prop
							.update_type
							.as_deref()
							.map(UpdateBehavior::from_str),
						duplicates_allowed: raw_prop.duplicates_allowed.unwrap_or(false),

						// legacy spec doesn't have these
						one_of_required: None,
						any_of_required: None,
						all_of_required: None,
					};

					properties.insert(PropertyName(prop_name), shape);
				}
			}

			let descriptor = PropertyTypeDescriptor {
				type_id,
				properties,
			};
			property_types.insert(descriptor.type_id.clone(), descriptor);
		}

		// Now convert ResourceTypes.
		for (name, raw_rt) in raw.resource_types.unwrap_or_default() {
			let type_id = ResourceTypeId(name.clone());

			// Properties
			let mut properties = HashMap::new();
			if let Some(raw_props) = raw_rt.properties {
				for (prop_name, raw_prop) in raw_props {
					let type_info = TypeInfo::from_raw_property(&raw_prop).map_err(|reason| {
						SpecStoreError::InvalidPropertyType {
							resource_type: name.clone(),
							property_name: prop_name.clone(),
							reason,
						}
					})?;

					let shape = PropertyShape {
						name: PropertyName(prop_name.clone()),
						type_info,
						required: raw_prop.required.unwrap_or(false),
						documentation_url: raw_prop.documentation,
						update_behavior: raw_prop
							.update_type
							.as_deref()
							.map(UpdateBehavior::from_str),
						duplicates_allowed: raw_prop.duplicates_allowed.unwrap_or(false),

						// legacy spec doesn't have these
						one_of_required: None,
						any_of_required: None,
						all_of_required: None,
					};

					properties.insert(PropertyName(prop_name), shape);
				}
			}

			// Attributes
			let mut attributes = HashMap::new();
			if let Some(raw_attrs) = raw_rt.attributes {
				for (attr_name, raw_attr) in raw_attrs {
					let type_info = TypeInfo::from_raw_attribute(&raw_attr);
					let shape = AttributeShape {
						name: AttributeName(attr_name.clone()),
						type_info,
						documentation_url: raw_attr.documentation,
					};
					attributes.insert(AttributeName(attr_name), shape);
				}
			}

			let descriptor = ResourceTypeDescriptor {
				type_id,
				properties,
				attributes,
				documentation_url: raw_rt.documentation,
			};

			resource_types.insert(descriptor.type_id.clone(), descriptor);
		}

		Ok(SpecStore {
			resource_types,
			property_types,
		})
	}
}

/// Errors that can occur while building a SpecStore from JSON.
#[derive(Debug, Error)]
pub enum SpecStoreError {
	#[error("failed to parse CloudFormation spec JSON: {0}")]
	Json(#[from] serde_json::Error),

	#[error("invalid property type for {resource_type}.{property_name}: {reason}")]
	InvalidPropertyType {
		resource_type: String,
		property_name: String,
		reason: String,
	},
}

/// Strongly-typed IDs / names
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceTypeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyTypeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyName(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttributeName(pub String);

/// Descriptor for a top-level CloudFormation Resource Type
/// (e.g. AWS::S3::Bucket).
#[derive(Debug)]
pub struct ResourceTypeDescriptor {
	pub type_id: ResourceTypeId,
	pub properties: HashMap<PropertyName, PropertyShape>,
	pub attributes: HashMap<AttributeName, AttributeShape>,
	pub documentation_url: Option<String>,
}

/// Descriptor for a complex Property Type
/// (e.g. AWS::S3::Bucket.LoggingConfiguration).
#[derive(Debug)]
pub struct PropertyTypeDescriptor {
	pub type_id: PropertyTypeId,
	pub properties: HashMap<PropertyName, PropertyShape>,
}

/// Shape of a single property.
#[derive(Debug, Clone)]
pub struct PropertyShape {
	pub name: PropertyName,
	pub type_info: TypeInfo,
	pub required: bool,
	pub documentation_url: Option<String>,
	pub update_behavior: Option<UpdateBehavior>,
	pub duplicates_allowed: bool,

	// Schema validation constraints
	pub one_of_required: Option<Vec<Vec<String>>>, // Each Vec<String> is one valid combination
	pub any_of_required: Option<Vec<Vec<String>>>,
	pub all_of_required: Option<Vec<Vec<String>>>,
}
/// Shape of a single attribute on a resource.
#[derive(Debug, Clone)]
pub struct AttributeShape {
	pub name: AttributeName,
	pub type_info: Option<TypeInfo>,
	pub documentation_url: Option<String>,
}

/// Type information for a property or attribute.
///
/// This is intentionally simple for now: it knows whether the value is
/// scalar/list/map, and whether the underlying element is a primitive
/// or a complex property type.
#[derive(Debug, Clone)]
pub struct TypeInfo {
	pub kind: ShapeKind,
	pub collection: CollectionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShapeKind {
	Primitive(PrimitiveType),
	Complex(PropertyTypeId),
	Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
	Scalar,
	List,
	Map,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrimitiveType {
	String,
	Long,
	Integer,
	Double,
	Boolean,
	Timestamp,
	Json,
	// Fallback for unknown primitives (for forward-compat).
	Other(String),
}

impl PrimitiveType {
	fn from_str(s: &str) -> Self {
		match s {
			"String" => PrimitiveType::String,
			"Long" => PrimitiveType::Long,
			"Integer" => PrimitiveType::Integer,
			"Double" => PrimitiveType::Double,
			"Boolean" => PrimitiveType::Boolean,
			"Timestamp" => PrimitiveType::Timestamp,
			"Json" => PrimitiveType::Json,
			other => PrimitiveType::Other(other.to_string()),
		}
	}
}

#[derive(Debug, Clone)]
pub enum UpdateBehavior {
	Mutable,
	Immutable,
	Conditional,
	Other(String),
}

impl UpdateBehavior {
	fn from_str(s: &str) -> Self {
		match s {
			"Mutable" => UpdateBehavior::Mutable,
			"Immutable" => UpdateBehavior::Immutable,
			"Conditional" => UpdateBehavior::Conditional,
			other => UpdateBehavior::Other(other.to_string()),
		}
	}
}

impl TypeInfo {
	fn from_raw_property(raw: &RawProperty) -> Result<Self, String> {
		// Determine collection kind first.
		let collection = match raw.r#type.as_deref() {
			Some("List") => CollectionKind::List,
			Some("Map") => CollectionKind::Map,
			_ => CollectionKind::Scalar,
		};

		// Determine underlying kind.
		// Priority order follows the CFN spec patterns.
		if let Some(ref p) = raw.primitive_type {
			let prim = PrimitiveType::from_str(p);
			return Ok(TypeInfo {
				kind: ShapeKind::Primitive(prim),
				collection,
			});
		}

		if let Some(ref pit) = raw.primitive_item_type {
			let prim = PrimitiveType::from_str(pit);
			return Ok(TypeInfo {
				kind: ShapeKind::Primitive(prim),
				collection,
			});
		}

		if let Some(ref item_type) = raw.item_type {
			let pt_id = PropertyTypeId(item_type.clone());
			return Ok(TypeInfo {
				kind: ShapeKind::Complex(pt_id),
				collection,
			});
		}

		if let Some(ref t) = raw.r#type {
			// If Type is not List/Map and there is no PrimitiveType,
			// interpret it as a complex type reference.
			if t != "List" && t != "Map" {
				let pt_id = PropertyTypeId(t.clone());
				return Ok(TypeInfo {
					kind: ShapeKind::Complex(pt_id),
					collection,
				});
			}
		}

		// If we get here, the combination is unexpected. Keep it
		// forward-compatible by treating it as "Any".
		Ok(TypeInfo {
			kind: ShapeKind::Any,
			collection,
		})
	}

	fn from_raw_attribute(raw: &RawAttribute) -> Option<Self> {
		// Attributes use similar patterns but are often looser.
		let collection = match raw.r#type.as_deref() {
			Some("List") => CollectionKind::List,
			Some("Map") => CollectionKind::Map,
			_ => CollectionKind::Scalar,
		};

		if let Some(ref p) = raw.primitive_type {
			let prim = PrimitiveType::from_str(p);
			return Some(TypeInfo {
				kind: ShapeKind::Primitive(prim),
				collection,
			});
		}

		if let Some(ref pit) = raw.primitive_item_type {
			let prim = PrimitiveType::from_str(pit);
			return Some(TypeInfo {
				kind: ShapeKind::Primitive(prim),
				collection,
			});
		}

		if let Some(ref item_type) = raw.item_type {
			let pt_id = PropertyTypeId(item_type.clone());
			return Some(TypeInfo {
				kind: ShapeKind::Complex(pt_id),
				collection,
			});
		}

		if let Some(ref t) = raw.r#type
			&& t != "List"
			&& t != "Map"
		{
			let pt_id = PropertyTypeId(t.clone());
			return Some(TypeInfo {
				kind: ShapeKind::Complex(pt_id),
				collection,
			});
		}

		None
	}
}

//
// Raw serde layer matching the AWS CloudFormation spec JSON
//

#[derive(Debug, Deserialize)]
struct RawSpec {
	#[serde(rename = "ResourceTypes")]
	resource_types: Option<HashMap<String, RawResourceType>>,

	#[serde(rename = "PropertyTypes")]
	property_types: Option<HashMap<String, RawPropertyType>>,
}

#[derive(Debug, Deserialize)]
struct RawResourceType {
	#[serde(rename = "Documentation")]
	documentation: Option<String>,

	#[serde(rename = "Properties")]
	properties: Option<HashMap<String, RawProperty>>,

	#[serde(rename = "Attributes")]
	attributes: Option<HashMap<String, RawAttribute>>,
}

#[derive(Debug, Deserialize)]
struct RawPropertyType {
	#[serde(rename = "Properties")]
	properties: Option<HashMap<String, RawProperty>>,
}

#[derive(Debug, Deserialize)]
struct RawProperty {
	#[serde(rename = "Required")]
	required: Option<bool>,

	#[serde(rename = "PrimitiveType")]
	primitive_type: Option<String>,

	#[serde(rename = "Type")]
	r#type: Option<String>,

	#[serde(rename = "ItemType")]
	item_type: Option<String>,

	#[serde(rename = "PrimitiveItemType")]
	primitive_item_type: Option<String>,

	#[serde(rename = "Documentation")]
	documentation: Option<String>,

	#[serde(rename = "UpdateType")]
	update_type: Option<String>,

	#[serde(rename = "DuplicatesAllowed")]
	duplicates_allowed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawAttribute {
	#[serde(rename = "PrimitiveType")]
	primitive_type: Option<String>,

	#[serde(rename = "Type")]
	r#type: Option<String>,

	#[serde(rename = "ItemType")]
	item_type: Option<String>,

	#[serde(rename = "PrimitiveItemType")]
	primitive_item_type: Option<String>,

	#[serde(rename = "Documentation")]
	documentation: Option<String>,
}
