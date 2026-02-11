use super::model::{Axis, Cmp, EntityId, Model, Query, Value};

// ─── Guidance Types ───

#[derive(Debug)]
pub struct Guide {
	pub entity: EntityId,
	pub logical_id: String,
	pub level: GuideLevel,
	pub focus: FocusTaxonomy,
	pub tldr: String,
	pub message: String,
	pub why: String,
}

#[derive(Debug)]
pub enum GuideLevel {
	Required,
	Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTaxonomy {
	DataSensitivity,
	DataCriticality,
	DataResiliance,
}

// ─── Query Helpers ───

/// Check if a resource has a tag with the given key
fn has_tag(model: &Model, resource: EntityId, tag_key: &str) -> bool {
	get_tag_value(model, resource, tag_key).is_some()
}

/// Get the value of a tag on a resource
fn get_tag_value(model: &Model, resource: EntityId, tag_key: &str) -> Option<String> {
	let query = Query::follow("aws:Tags").then(Axis::Child, None).filter(
		"aws:Key",
		Cmp::Eq,
		Value::Literal(tag_key.to_string()),
	);

	let matches = model.query_from(&[resource], &query);

	matches
		.first()
		.and_then(|&tag| model.get_literal(tag, "aws:Value"))
}

/// Check if a resource has evidence of a given type
pub fn has_evidence(model: &Model, resource: EntityId, evidence_name: &str) -> bool {
	let query = Query::descendant("wa2:Evidence").filter(
		"wa2:value",
		Cmp::Eq,
		Value::Literal(evidence_name.to_string()),
	);

	!model.query_from(&[resource], &query).is_empty()
}

// ─── Guidance ───

/// Compute guidance for a model
pub fn guidance(model: &Model) -> Vec<Guide> {
	let mut guides = Vec::new();

	// Find all stores
	let stores = model.query(&Query::descendant("wa2:Store"));

	for id in stores {
		let logical_id = model
			.get_literal(id, "aws:logicalId")
			.unwrap_or_else(|| model.qualified_name(id));

		// Check for mandatory tags
		if !has_tag(model, id, "DataSensitivity") {
			guides.push(Guide {
				entity: id,
				logical_id: logical_id.clone(),
				tldr: "Tag this resource for DataSensitivity".to_string(),
				message: "All stores of information should have data sensitivity.".to_string(),
				why: "Tagging store with sensitivity speeds up design decisions, \
                    we can apply the same designs for all data of the same class. \
                    For example website assets don't need encryption, whilst \
                    a healthcare record needs encryption and access restricted."
					.to_string(),
				level: GuideLevel::Required,
				focus: FocusTaxonomy::DataSensitivity,
			});
		}

		if !has_tag(model, id, "DataCriticality") {
			guides.push(Guide {
				entity: id,
				logical_id: logical_id.clone(),
				tldr: "Tag this resource for DataCriticality".to_string(),
				message: "All stores of information should have a data criticality.".to_string(),
				why: "Tagging store with criticality speeds up design decisions, \
                    we can apply the same designs for all data of the same class. \
                    For example easily recreated data does not need backing up, whilst \
                    employment records need protection against loss."
					.to_string(),
				level: GuideLevel::Required,
				focus: FocusTaxonomy::DataCriticality,
			});
		}

		// Check critical data is backed up
		if let Some(criticality) = get_tag_value(model, id, "DataCriticality") {
			if (criticality == "MissionCritical" || criticality == "BusinessCritical")
				&& !has_evidence(model, id, "DataResiliance")
			{
				guides.push(Guide {
					entity: id,
					logical_id,
					tldr: "Backup this resource".to_string(),
					message: "All critical stores of information should be backed up.".to_string(),
					why: "This data's criticality indicates we don't want to lose it, \
                        so we need to apply a mechanism to ensure its backed up. \
                        Backup, Snapshots or Replication are common solutions."
						.to_string(),
					level: GuideLevel::Action,
					focus: FocusTaxonomy::DataResiliance,
				});
			}
		}
	}

	guides
}
