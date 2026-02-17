use std::fmt;

use crate::iaac::cloudformation::cfn_ir::types::{CfnParameter, CfnResource, CfnTemplate, CfnValue};

impl fmt::Display for CfnTemplate {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "Template:")?;

		// Display Parameters section
		if !self.parameters.is_empty() {
			writeln!(f, "{}Parameters:", " ".repeat(2))?;
			let mut params: Vec<_> = self.parameters.iter().collect();
			params.sort_by_key(|(name, _)| *name);
			for (_, param) in params {
				for line in param.to_string().lines() {
					writeln!(f, "{}{}", " ".repeat(4), line)?;
				}
			}
		}

		// Display Resources section
		if !self.resources.is_empty() {
			writeln!(f, "{}Resources:", " ".repeat(2))?;
			for (logical_id, resource) in &self.resources {
				writeln!(f, "{}{}:", " ".repeat(4), logical_id)?;
				for line in resource.to_string().lines() {
					writeln!(f, "{}{}", " ".repeat(2), line)?;
				}
			}
		}

		Ok(())
	}
}

impl fmt::Display for CfnResource {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "{}Type: {}", " ".repeat(4), self.resource_type)?;
		if !self.properties.is_empty() {
			writeln!(f, "    Properties:")?;
			// Sort properties by name for deterministic output
			let mut props: Vec<_> = self.properties.iter().collect();
			props.sort_by_key(|(name, _)| *name);

			for (name, value) in props {
				writeln!(f, "{}{}: {}", " ".repeat(6), name, value.0)?;
			}
		}
		Ok(())
	}
}

impl fmt::Display for CfnValue {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			CfnValue::String(s, _) => write!(f, "\"{}\"", s),
			CfnValue::Number(n, _) => write!(f, "{}", n),
			CfnValue::Bool(b, _) => write!(f, "{}", b),
			CfnValue::Null(_) => write!(f, "null"),
			CfnValue::Ref { target, .. } => write!(f, "!Ref {}", target),
			CfnValue::GetAtt {
				target, attribute, ..
			} => {
				write!(f, "!GetAtt {}.{}", target, attribute)
			}
			CfnValue::Sub {
				template,
				variables,
				..
			} => {
				if let Some(_vars) = variables {
					write!(f, "!Sub [{}, {{...}}]", template)
				} else {
					write!(f, "!Sub {}", template)
				}
			}
			CfnValue::GetAZs { region, .. } => {
				write!(f, "!GetAZs {}", region)
			}
			CfnValue::Join {
				delimiter, values, ..
			} => {
				write!(f, "!Join [\"{}\", {}]", delimiter, values)
			}
			CfnValue::Select { index, list, .. } => {
				write!(f, "!Select [{}, {}]", index, list)
			}
			CfnValue::If {
				condition_name,
				value_if_true,
				value_if_false,
				..
			} => {
				write!(
					f,
					"!If [{}, {}, {}]",
					condition_name, value_if_true, value_if_false
				)
			}
			CfnValue::Equals { left, right, .. } => {
				write!(f, "!Equals [{}, {}]", left, right)
			}
			CfnValue::Not { condition, .. } => {
				write!(f, "!Not [{}]", condition)
			}
			CfnValue::And { conditions, .. } => {
				write!(f, "!And [")?;
				for (i, cond) in conditions.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", cond)?;
				}
				write!(f, "]")
			}
			CfnValue::Or { conditions, .. } => {
				write!(f, "!Or [")?;
				for (i, cond) in conditions.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", cond)?;
				}
				write!(f, "]")
			}
			CfnValue::Condition { condition_name, .. } => {
				write!(f, "!Condition {}", condition_name)
			}
			CfnValue::Base64 { value, .. } => {
				write!(f, "!Base64 {}", value)
			}
			CfnValue::Split {
				delimiter, source, ..
			} => {
				write!(f, "!Split [\"{}\", {}]", delimiter, source)
			}
			CfnValue::Cidr {
				ip_block,
				count,
				cidr_bits,
				..
			} => {
				write!(f, "!Cidr [{}, {}, {}]", ip_block, count, cidr_bits)
			}
			CfnValue::ImportValue { name, .. } => {
				write!(f, "!ImportValue {}", name)
			}
			CfnValue::Array(items, _) => {
				write!(f, "[")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", item)?;
				}
				write!(f, "]")
			}
			CfnValue::Object(map, _) => {
				write!(f, "{{")?;
				for (i, (k, v)) in map.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}: {}", k, v.0)?;
				}
				write!(f, "}}")
			}
			CfnValue::FindInMap {
				map_name,
				top_key,
				second_key,
				default_value,
				..
			} => {
				if let Some(default) = default_value {
					write!(
						f,
						"!FindInMap [{}, {}, {}, {}]",
						map_name, top_key, second_key, default
					)
				} else {
					write!(f, "!FindInMap [{}, {}, {}]", map_name, top_key, second_key)
				}
			}
			CfnValue::ToJsonString { value, .. } => {
				write!(f, "!ToJsonString {}", value)
			}
			CfnValue::Length { array, .. } => {
				write!(f, "!Length {}", array)
			}
			CfnValue::Contains { values, value, .. } => {
				write!(f, "!Contains [{}, {}]", values, value)
			}
			CfnValue::Transform { name, .. } => {
				write!(f, "!Transform {}", name)
			}
		}
	}
}

impl fmt::Display for CfnParameter {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "{}:", self.name)?;
		writeln!(f, "  Type: {}", self.parameter_type)?;
		if let Some(ref desc) = self.description {
			writeln!(f, "  Description: {}", desc)?;
		}
		if let Some(ref default) = self.default_value {
			writeln!(f, "  Default: {}", default)?;
		}
		Ok(())
	}
}
