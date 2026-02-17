//! AWS-specific type derivation and evidence detection

use crate::intents::model::{Axis, Cmp, EntityId, Model, ModelError, Query, Value};

/// Derive core:Node from cfn:Resource with appropriate core type
pub fn derive_wa2_type(
	model: &mut Model,
	cfn_entity: EntityId,
	workload: EntityId,
) -> Result<(), ModelError> {
	let Some(aws_type) = model.get_literal(cfn_entity, "aws:type") else {
		return Ok(());
	};

	let node_type = match aws_type.as_str() {
		"AWS::EC2::Instance" | "AWS::Lambda::Function" => Some("core:Run"),
		"AWS::S3::Bucket" | "AWS::EC2::Volume" | "AWS::EFS::FileSystem" => Some("core:Store"),
		"AWS::SQS::Queue" | "AWS::Kinesis::Stream" => Some("core:Move"),
		_ => None,
	};

	if let Some(as_type) = node_type {
		let node = model.blank();
		model.apply_to(node, "wa2:type", as_type)?;
		model.apply_entity(node, "core:source", cfn_entity)?;

		if let Some(range) = model.get_range(cfn_entity) {
			model.set_range(node, range);
		}

		// Add node to workload
		model.apply_entity(workload, "wa2:contains", node)?;
	}

	Ok(())
}

/// Derive evidence from resource configuration
pub fn derive_evidence(model: &mut Model, cfn_entity: EntityId) -> Result<(), ModelError> {
	// Check for S3 replication configuration
	let query = Query::follow("aws:ReplicationConfiguration")
		.then_follow("aws:Rules")
		.then(Axis::Child, None)
		.filter("aws:Status", Cmp::Eq, Value::Literal("Enabled".to_string()));

	if !model.query_from(&[cfn_entity], &query).is_empty() {
		// Find the core:Node that links to this cfn:Resource
		let core_source = model
			.resolve("core:source")
			.expect("core:source must exist");

		// Search for nodes that have core:source pointing to cfn_entity
		for i in 0..model.entity_count() {
			let node_id = EntityId(i as u32);
			let sources = model.get_all(node_id, core_source);
			if sources.iter().any(|v| v.as_entity() == Some(cfn_entity)) {
				// Found the core:Node - add evidence to it
				let evidence = model.blank();
				model.apply_to(evidence, "wa2:type", "core:Evidence")?;
				model.apply_to(evidence, "core:value", "\"DataResiliance\"")?;
				model.apply_entity(node_id, "wa2:contains", evidence)?;
				break;
			}
		}
	}

	Ok(())
}
