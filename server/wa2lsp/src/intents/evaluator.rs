use std::fmt::Debug;

use crate::spec::cfn_ir::types::{CfnTemplate, CfnValue};

use super::{
	node::Annotation,
	system::{FocusTaxonomy, NodeError, System},
};

pub fn evaluate_system_ok(template: &CfnTemplate) -> Result<(), NodeError> {
	// PROJECT: vendor template into wa2 system
	let system = project_vendor_aws(template)?;

	eprintln!("\nSystem:\n===\n{}", super::system::PrettySystem { system: &system });

	// GUIDANCE: is guidance requuired?
	let guidance = system.guidance();

	eprintln!("Guidance:\n===\n{:?}", guidance);

	// For now: treat any required guidance as failing the check.
	if !guidance.is_empty() {
		return Err(NodeError::GuidanceRequired);
	}

	Ok(())
}

/// takes a Cfn template and create a system based on it
pub fn project_vendor_aws(template: &CfnTemplate) -> Result<System, NodeError> {
	let mut system = System::new().expect("created system graph");
	const VENDOR: &str = "AWS";

	// Create root deployment node FIRST — this is always node 1
	let deployment = system
		.add_node("Deployment", VENDOR, &[("name", "root")])
		.expect("insert deployment");

	for resource in template.resources.values() {
		// MAP: resources into primitives (s3/bucket -> store)
		let wa2_kind = match resource.resource_type.as_str() {
			"AWS::EC2::Instance" | "AWS::Lambda::Function" => "Run",
			"AWS::S3::Bucket" | "AWS::EC2::Volume" | "AWS::EFS::FileSystem" => "Store",
			"AWS::SQS::Queue" | "AWS::Kinesis::Stream" => "Move",
			_ => "",
		};

		let name = resource.logical_id.clone();
		//let node = system.add_node(&name, kind);
		let node = system
			.add_node(
				"Resource",
				VENDOR,
				&[
					("type", &resource.resource_type),
					("logical_id", &name),
					("wa2_kind", &wa2_kind),
				],
			)
			.expect("insert resource");
		// Attach resource to deployment
		system.add_edge("Contains", deployment, node).ok();

		// TAGS: extract any evidence held in tags
		// TODO: map tags into taxonomy via configuration not hard coded!
      if let Some((tag_val, _)) = resource.properties.get("Tags")
			&& let Some(tags) = tag_val.as_array()
		{
			let tags_property = system
				.add_node("Property", VENDOR, &[("name", "tags")])
				.expect("insert tags");
			system.add_edge("Contains", node, tags_property).ok();

			for tag in tags {
				if let Some(tag_obj) = tag.as_object() {
					// Safe access to Key and Value
					if let Some((key_val, _)) = tag_obj.get("Key")
						&& let Some((value_val, _)) = tag_obj.get("Value")
						&& let Some(key) = key_val.as_str()
						&& let Some(value) = value_val.as_str()
					{
						let tag = system
							.add_node("Value", VENDOR, &[("key", key), ("value", value)])
							.expect("insert tag");
						system.add_edge("Contains", tags_property, tag).ok();
					}
				}
			}
		}

		// Evidence: what resiliance is in place?
		// TODO: map methods resiliance
		// Evidence: what resilience is in place?
		if let Some((repl_val, _)) = resource.properties.get("ReplicationConfiguration")
			&& let Some(repl_obj) = repl_val.as_object()
			&& let Some((rules_field, _)) = repl_obj.get("Rules")
			&& let CfnValue::Array(rules, _) = rules_field
		{
         let wa2_property = system
				.add_node("meta", "wa2", &[("name", "derived")])
				.expect("insert tags");
			system.add_edge("Contains", node, wa2_property).ok();
			// can we find any rule that satisfies us
			for rule in rules {
				// need a rule that is at least enabled
				if let CfnValue::Object(rule_obj, _) = rule
					&& let Some((status_value, _)) = rule_obj.get("Status")
					&& let CfnValue::String(value, _) = status_value
					&& value == "Enabled"
				{
					//system.annotate(node, Annotation::Evidence(FocusTaxonomy::DataResiliance))?;
               let evidence = system
							.add_node("Evidence", "wa2", &[("key", "evidence"), ("value", "DataResiliance")])
							.expect("insert tag");
						system.add_edge("Contains", wa2_property, evidence).ok();
				}
			}
		}
	}

	Ok(system)
}

// impl FromStr for NodeKind {
// 	type Err = ();

// 	fn from_str(s: &str) -> Result<Self, Self::Err> {
// 		Ok(match s {
// 			"AWS::EC2::Instance" | "AWS::Lambda::Function" => NodeKind::Run,
// 			"AWS::S3::Bucket" | "AWS::EC2::Volume" | "AWS::EFS::FileSystem" => NodeKind::Store,
// 			"AWS::SQS::Queue" | "AWS::Kinesis::Stream" => NodeKind::Move,
// 			_ => return Err(()),
// 		})
// 	}
// }

#[cfg(test)]
mod tests {
	use crate::intents::test_support::use_latest_cfn_spec;

	use super::*;
	use std::{env, path::Path};
	use url::Url;

	#[allow(clippy::large_enum_variant)]
	#[allow(dead_code)]
	enum ParseResult {
		Error,
		Parsed { template: CfnTemplate },
	}

	fn parse_and_validate(path: &Path) -> Result<ParseResult, Box<dyn std::error::Error>> {
		let content = std::fs::read_to_string(path)?;
		let abs_path = path.canonicalize()?;
		let uri = Url::from_file_path(&abs_path)
			.map_err(|_| format!("Invalid path: {}", path.display()))?;

		match CfnTemplate::from_yaml(&content, &uri) {
			Err(parse_diags) => Ok(ParseResult::Error),
			Ok(template) => {
				let spec = use_latest_cfn_spec();
				let diags = template.validate_against_spec(&spec, &uri);
				Ok(ParseResult::Parsed { template })
			}
		}
	}

	#[test]
	fn tutorial_step_0() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/0.naive.yaml");
		let parse_result = parse_and_validate(path).expect("Failed to read/parse file");

		if let ParseResult::Parsed { template } = parse_result {
			evaluate_system_ok(&template).expect_err("should fail");
		} else {
			panic!("Expected parsed template");
		}
	}

	#[test]
	fn tutorial_step_1() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/1.calm.yaml");
		let parse_result = parse_and_validate(path).expect("Failed to read/parse file");

		if let ParseResult::Parsed { template } = parse_result {
			evaluate_system_ok(&template).expect_err("should fail: not tagged");
		} else {
			panic!("Expected parsed template");
		}
	}

	#[test]
	fn tutorial_step_2() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/2.wa2tags.yaml");
		let parse_result = parse_and_validate(path).expect("Failed to read/parse file");

		if let ParseResult::Parsed { template } = parse_result {
			evaluate_system_ok(&template).expect_err("should fail: not backed up");
		} else {
			panic!("Expected parsed template");
		}
	}

	#[test]
	fn tutorial_step_3() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/3.with-replication.yaml");
		let parse_result = parse_and_validate(path).expect("Failed to read/parse file");

		if let ParseResult::Parsed { template } = parse_result {
			evaluate_system_ok(&template).expect("all good");
		} else {
			panic!("Expected parsed template");
		}
	}
}
