use indexmap::IndexMap;

use tower_lsp::lsp_types::Range;

/// CloudFormation template intermediate representation
// Update CfnTemplate struct
#[derive(Debug, Clone, Default)]
pub struct CfnTemplate {
	pub resources: IndexMap<String, CfnResource>,
	pub parameters: IndexMap<String, CfnParameter>,
	pub conditions: IndexMap<String, CfnCondition>,
	pub mappings: IndexMap<String, CfnMapping>,
	pub rules: IndexMap<String, CfnRule>,
   pub outputs: IndexMap<String, CfnOutput>,
	pub transform: Option<Vec<String>>,
}

/// A CloudFormation resource with position tracking
#[derive(Debug, Clone)]
pub struct CfnResource {
	pub logical_id: String,
	pub resource_type: String,
	pub properties: IndexMap<String, (CfnValue, Range)>,

	// Position tracking for diagnostics
	pub logical_id_range: Range,
	pub type_range: Range,
}

/// A CloudFormation parameter declaration
#[derive(Debug, Clone)]
pub struct CfnParameter {
	pub name: String,
	pub parameter_type: String, // "String", "Number", "List<Number>", etc.
	pub default_value: Option<CfnValue>,
	pub description: Option<String>,

	// Position tracking
	pub name_range: Range,
	pub type_range: Range,
}

/// A CloudFormation condition declaration
#[derive(Debug, Clone)]
pub struct CfnCondition {
	pub name: String,
	pub expression: CfnValue, // The condition expression (e.g., !Equals [...])

	// Position tracking
	pub name_range: Range,
}

/// A CloudFormation mapping declaration
#[derive(Debug, Clone)]
pub struct CfnMapping {
	pub name: String,
	pub map: IndexMap<String, IndexMap<String, CfnValue>>, // Top-level key -> Second-level key -> Value

	// Position tracking
	pub name_range: Range,
}

/// A CloudFormation rule declaration (for parameter validation)
#[derive(Debug, Clone)]
pub struct CfnRule {
	pub name: String,
	pub assertions: Vec<CfnAssertion>,

	// Position tracking
	pub name_range: Range,
}

/// An assertion within a rule
#[derive(Debug, Clone)]
pub struct CfnAssertion {
	pub assert_condition: CfnValue,
	pub assert_description: Option<String>,
	pub range: Range,
}

/// A CloudFormation output declaration
#[derive(Debug, Clone)]
pub struct CfnOutput {
    pub name: String,
    pub value: CfnValue,
    pub description: Option<String>,
    pub export_name: Option<CfnValue>,
    pub condition: Option<String>,

    // Position tracking
    pub name_range: Range,
}

/// All CFN intrinsic type names (for iteration)
pub const CFN_INTRINSICS: &[&str] = &[
	"Ref",
	"GetAtt",
	"Sub",
	"Join",
	"If",
	"Select",
	"GetAZs",
	"Base64",
	"Split",
	"Cidr",
	"ImportValue",
	"FindInMap",
	"ToJsonString",
	"Length",
	"Contains",
	"Transform",
	"Equals",
	"Not",
	"And",
	"Or",
	"Condition",
];

/// A value in a CloudFormation template with position tracking
#[derive(Debug, Clone)]
pub enum CfnValue {
	String(String, Range),
	Number(f64, Range),
	Bool(bool, Range),
	Null(Range),
	Array(Vec<CfnValue>, Range),
	Object(IndexMap<String, (CfnValue, Range)>, Range),

	/// !Ref / { "Ref": "LogicalId" }
	Ref {
		target: String,
		range: Range,
	},

	/// !GetAtt / { "Fn::GetAtt": ["LogicalId", "Attribute"] } / "LogicalId.Attribute"
	GetAtt {
		target: String,
		attribute: String,
		range: Range,
	},

	// !Sub / { "Fn::Sub": "template string" } or { "Fn::Sub": ["template", {vars}] }
	Sub {
		template: Box<CfnValue>,
		variables: Option<IndexMap<String, CfnValue>>, // For long form with explicit variables
		range: Range,
	},

	/// !GetAZs / { "Fn::GetAZs": "region" }
	GetAZs {
		region: Box<CfnValue>, // Usually empty string "" or !Ref AWS::Region
		range: Range,
	},

	/// !Join / { "Fn::Join": [delimiter, values] }
	Join {
		delimiter: String,
		values: Box<CfnValue>,
		range: Range,
	},

	/// !Select / { "Fn::Select": [index, list] }
	Select {
		index: Box<CfnValue>, // Usually a number or !Ref to parameter
		list: Box<CfnValue>,  // Usually array or !GetAZs
		range: Range,
	},

	/// !If / { "Fn::If": [condition_name, value_if_true, value_if_false] }
	If {
		condition_name: String,
		value_if_true: Box<CfnValue>,
		value_if_false: Box<CfnValue>,
		range: Range,
	},

	/// !Equals / { "Fn::Equals": [value1, value2] }
	Equals {
		left: Box<CfnValue>,
		right: Box<CfnValue>,
		range: Range,
	},

	/// !Not / { "Fn::Not": [condition, range] }
	Not {
		condition: Box<CfnValue>,
		range: Range,
	},

	/// !And / { "Fn::And": [condition1, condition2, ...] }
	/// Takes 2-10 conditions
	And {
		conditions: Vec<CfnValue>,
		range: Range,
	},

	/// !Or / { "Fn::Or": [condition1, condition2, ...] }
	/// Takes 2-10 conditions
	Or {
		conditions: Vec<CfnValue>,
		range: Range,
	},

	/// !Condition / { "Condition": "ConditionName" }
	/// References another condition by name
	Condition {
		condition_name: String,
		range: Range,
	},

	Base64 {
		value: Box<CfnValue>,
		range: Range,
	},
	Split {
		delimiter: String,
		source: Box<CfnValue>,
		range: Range,
	},
	Cidr {
		ip_block: Box<CfnValue>,
		count: Box<CfnValue>,
		cidr_bits: Box<CfnValue>,
		range: Range,
	},
	ImportValue {
		name: Box<CfnValue>,
		range: Range,
	},

	FindInMap {
		map_name: Box<CfnValue>,
		top_key: Box<CfnValue>,
		second_key: Box<CfnValue>,
		default_value: Option<Box<CfnValue>>,
		range: Range,
	},

	ToJsonString {
		value: Box<CfnValue>,
		range: Range,
	},
	Length {
		array: Box<CfnValue>,
		range: Range,
	},

	Contains {
		values: Box<CfnValue>, // Array to search in
		value: Box<CfnValue>,  // Value to search for
		range: Range,
	},

	Transform {
		name: Box<CfnValue>,
		parameters: Box<CfnValue>,
		range: Range,
	},
}

impl CfnValue {
	/// Get the position range of this value
	pub fn range(&self) -> Range {
		match self {
			CfnValue::String(_, r) => *r,
			CfnValue::Number(_, r) => *r,
			CfnValue::Bool(_, r) => *r,
			CfnValue::Null(r) => *r,
			CfnValue::Array(_, r) => *r,
			CfnValue::Object(_, r) => *r,
			CfnValue::Ref { range, .. } => *range,
			CfnValue::GetAtt { range, .. } => *range,
			CfnValue::Sub { range, .. } => *range,
			CfnValue::GetAZs { range, .. } => *range,
			CfnValue::Join { range, .. } => *range,
			CfnValue::Select { range, .. } => *range,
			CfnValue::If { range, .. } => *range,
			CfnValue::Equals { range, .. } => *range,
			CfnValue::Not { range, .. } => *range,
			CfnValue::And { range, .. } => *range,
			CfnValue::Or { range, .. } => *range,
			CfnValue::Condition { range, .. } => *range,
			CfnValue::Base64 { range, .. } => *range,
			CfnValue::Split { range, .. } => *range,
			CfnValue::Cidr { range, .. } => *range,
			CfnValue::ImportValue { range, .. } => *range,
			CfnValue::FindInMap { range, .. } => *range,
			CfnValue::ToJsonString { range, .. } => *range,
			CfnValue::Length { range, .. } => *range,
			CfnValue::Contains { range, .. } => *range,
			CfnValue::Transform { range, .. } => *range,
		}
	}

	/// Get the CFN intrinsic function name, if this is an intrinsic
	pub fn intrinsic_name(&self) -> Option<&'static str> {
		match self {
			CfnValue::Ref { .. } => Some(CFN_INTRINSICS[0]),
			CfnValue::GetAtt { .. } => Some(CFN_INTRINSICS[1]),
			CfnValue::Sub { .. } => Some(CFN_INTRINSICS[2]),
			CfnValue::Join { .. } => Some(CFN_INTRINSICS[3]),
			CfnValue::If { .. } => Some(CFN_INTRINSICS[4]),
			CfnValue::Select { .. } => Some(CFN_INTRINSICS[5]),
			CfnValue::GetAZs { .. } => Some(CFN_INTRINSICS[6]),
			CfnValue::Base64 { .. } => Some(CFN_INTRINSICS[7]),
			CfnValue::Split { .. } => Some(CFN_INTRINSICS[8]),
			CfnValue::Cidr { .. } => Some(CFN_INTRINSICS[9]),
			CfnValue::ImportValue { .. } => Some(CFN_INTRINSICS[10]),
			CfnValue::FindInMap { .. } => Some(CFN_INTRINSICS[11]),
			CfnValue::ToJsonString { .. } => Some(CFN_INTRINSICS[12]),
			CfnValue::Length { .. } => Some(CFN_INTRINSICS[13]),
			CfnValue::Contains { .. } => Some(CFN_INTRINSICS[14]),
			CfnValue::Transform { .. } => Some(CFN_INTRINSICS[15]),
			CfnValue::Equals { .. } => Some(CFN_INTRINSICS[16]),
			CfnValue::Not { .. } => Some(CFN_INTRINSICS[17]),
			CfnValue::And { .. } => Some(CFN_INTRINSICS[18]),
			CfnValue::Or { .. } => Some(CFN_INTRINSICS[19]),
			CfnValue::Condition { .. } => Some(CFN_INTRINSICS[20]),
			_ => None,
		}
	}

	/// Try to get this value as a string
	pub fn as_str(&self) -> Option<&str> {
		match self {
			CfnValue::String(s, _) => Some(s.as_str()),
			_ => None,
		}
	}

	// Return the map with (value, key_range) tuples
	pub fn as_object(&self) -> Option<&IndexMap<String, (CfnValue, Range)>> {
		match self {
			CfnValue::Object(map, _) => Some(map),
			_ => None,
		}
	}

	// Helper to get just values (for backward compatibility where ranges aren't needed)
	pub fn as_object_values(&self) -> Option<IndexMap<String, CfnValue>> {
		match self {
			CfnValue::Object(map, _) => Some(
				map.iter()
					.map(|(k, (v, _))| (k.clone(), v.clone()))
					.collect(),
			),
			_ => None,
		}
	}

	/// Try to get this value as an array
	pub fn as_array(&self) -> Option<&[CfnValue]> {
		match self {
			CfnValue::Array(items, _) => Some(items),
			_ => None,
		}
	}
}
