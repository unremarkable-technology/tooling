// server/wa2lsp/src/spec/intrinsics.rs
//! Declarative registry of CloudFormation intrinsic functions

// ordered same as docs for ease of comparison
// https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/intrinsic-function-reference.html
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicKind {
	Base64,
	Cidr,

	// Condition functions
	Condition,
	And,
	Equals,
	If,
	Not,
	Or,

	FindInMap,
	//ForEach,
	GetAtt,
	GetAZs,
	ImportValue,
	Join,
	Length,
	Select,
	Split,
	Sub,
	ToJsonString,
	Transform,
	Ref,

	// Rule functions (condition functions+)
	Contains,
	// EachMemberEquals,
	// EachMemberIn,
	// RefAll,
	// ValueOf,
	// ValueOfAll,
}

/// Definition of an intrinsic function
#[derive(Debug)]
pub struct IntrinsicDef {
	pub kind: IntrinsicKind,
	pub yaml_tag: &'static str,
	pub json_key: &'static str,
}

/// Global registry of all intrinsics
pub static INTRINSICS: &[IntrinsicDef] = &[
	IntrinsicDef {
		kind: IntrinsicKind::Ref,
		yaml_tag: "Ref",
		json_key: "Ref",
	},
	IntrinsicDef {
		kind: IntrinsicKind::GetAtt,
		yaml_tag: "GetAtt",
		json_key: "Fn::GetAtt",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Sub,
		yaml_tag: "Sub",
		json_key: "Fn::Sub",
	},
	IntrinsicDef {
		kind: IntrinsicKind::GetAZs,
		yaml_tag: "GetAZs",
		json_key: "Fn::GetAZs",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Join,
		yaml_tag: "Join",
		json_key: "Fn::Join",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Select,
		yaml_tag: "Select",
		json_key: "Fn::Select",
	},
	IntrinsicDef {
		kind: IntrinsicKind::If,
		yaml_tag: "If",
		json_key: "Fn::If",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Equals,
		yaml_tag: "Equals",
		json_key: "Fn::Equals",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Not,
		yaml_tag: "Not",
		json_key: "Fn::Not",
	},
	IntrinsicDef {
		kind: IntrinsicKind::And,
		yaml_tag: "And",
		json_key: "Fn::And",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Or,
		yaml_tag: "Or",
		json_key: "Fn::Or",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Condition,
		yaml_tag: "Condition",
		json_key: "Condition",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Base64,
		json_key: "Fn::Base64",
		yaml_tag: "Base64",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Split,
		json_key: "Fn::Split",
		yaml_tag: "Split",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Cidr,
		json_key: "Fn::Cidr",
		yaml_tag: "Cidr",
	},
	IntrinsicDef {
		kind: IntrinsicKind::ImportValue,
		json_key: "Fn::ImportValue",
		yaml_tag: "ImportValue",
	},
	IntrinsicDef {
		kind: IntrinsicKind::FindInMap,
		json_key: "Fn::FindInMap",
		yaml_tag: "FindInMap",
	},
	IntrinsicDef {
		kind: IntrinsicKind::ToJsonString,
		json_key: "Fn::ToJsonString",
		yaml_tag: "ToJsonString",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Length,
		json_key: "Fn::Length",
		yaml_tag: "Length",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Contains,
		json_key: "Fn::Contains",
		yaml_tag: "Contains",
	},
	IntrinsicDef {
		kind: IntrinsicKind::Transform,
		json_key: "Fn::Transform",
		yaml_tag: "Transform",
	},
];

/// Look up intrinsic by YAML tag
pub fn get_intrinsic_by_tag(tag: &str) -> Option<&'static IntrinsicDef> {
	INTRINSICS.iter().find(|i| i.yaml_tag == tag)
}

/// Look up intrinsic by JSON key
pub fn get_intrinsic_by_json_key(key: &str) -> Option<&'static IntrinsicDef> {
	INTRINSICS.iter().find(|i| i.json_key == key)
}
