use super::node::Annotation;
use agdb::{Comparison, DbElement, DbId, DbMemory, DbType, QueryBuilder};

/// A declared node
#[derive(Debug)]
pub struct System {
	// graph db for working
	db: DbMemory,
}

#[derive(Debug)]
pub enum NodeError {
	InvalidNode,
	GuidanceRequired,
}

pub enum NodeKind {
	Deployment,
	Resource,
	Property,
	Value,
	Export,
}

pub enum Provider {
	AWS,
	Azure,
	GCP,
}

impl Provider {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::AWS => "AWS",
			Self::Azure => "Azure",
			Self::GCP => "GCP",
		}
	}
}

impl NodeKind {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Deployment => "Deployment",
			Self::Resource => "Resource",
			Self::Property => "Property",
			Self::Value => "Value",
			Self::Export => "Export",
		}
	}
}

#[derive(Debug, Clone, DbType)]
pub struct Node {
	pub db_id: DbId,
	pub kind: String,     // "Deployment", "Resource", "Property", "Value", "Export"
	pub provider: String, // "AWS", "Azure", "GCP", "Generic"
}

#[derive(Debug, Clone, DbType)]
pub struct Edge {
	pub db_id: Option<DbId>,
	pub kind: String, // "Contains", "References", "DependsOn", "Exports", "Imports"
}

impl System {
	pub fn new() -> Result<Self, agdb::DbError> {
		let db = DbMemory::new("")?;
		Ok(Self {
			db,
			//		nodes: Arena::default(),
		})
	}

	pub fn add_node(
		&mut self,
		kind: &str,
		provider: &str,
		attrs: &[(&str, &str)],
	) -> Result<DbId, agdb::DbError> {
		let mut values: Vec<agdb::DbKeyValue> =
			vec![("kind", kind).into(), ("provider", provider).into()];
		for (k, v) in attrs {
			values.push((*k, *v).into());
		}

		let result = self
			.db
			.exec_mut(QueryBuilder::insert().nodes().values([values]).query())?;

		Ok(result.ids()[0])
	}

	pub fn add_edge(&mut self, kind: &str, from: DbId, to: DbId) -> Result<DbId, agdb::DbError> {
		let result = self.db.exec_mut(
			QueryBuilder::insert()
				.edges()
				.from(from)
				.to(to)
				.values([[("kind", kind).into()]])
				.query(),
		)?;
		Ok(result.ids()[0])
	}

	pub fn get_attr(&self, id: DbId, key: &str) -> Result<Option<String>, agdb::DbError> {
		let result = self
			.db
			.exec(QueryBuilder::select().values([key]).ids(id).query())?;

		Ok(result
			.elements
			.first()
			.and_then(|e| e.values.first())
			.map(|kv| kv.value.to_string()))
	}

	pub fn get_logical_id(&self, id: DbId) -> Result<Option<String>, agdb::DbError> {
		self.get_attr(id, "logical_id")
	}

	pub fn get_tag(
		&self,
		resource_id: DbId,
		tag_key: &str,
	) -> Result<Option<String>, agdb::DbError> {
		// Step 1: Find the "Tags" property node under this resource
		let result = self.db.exec(
			QueryBuilder::select()
				.search() // search mode (not direct select)
				.depth_first() // traverse children recursively
				.from(resource_id) // start from this resource
				.where_() // filter condition
				.key("name") // look at the "name" attribute
				.value(Comparison::Equal("tags".into())) // must equal "Tags"
				.query(),
		)?;

		// Get the IDs from result (need binding for lifetime)
		let ids = result.ids();

		// If no Tags property found, return None
		let Some(tags_id) = ids.first().copied() else {
			return Ok(None);
		};

		// Step 2: Find the Value node with matching tag key under Tags
		let result = self.db.exec(
			QueryBuilder::select()
				.search()
				.depth_first()
				.from(tags_id) // start from the Tags property
				.where_()
				.key("key") // look at the "key" attribute (tag key name)
				.value(Comparison::Equal(tag_key.into())) // must match requested key
				.query(),
		)?;

		// Step 3: Extract the tag value from the result
		Ok(result
			.elements // get full elements (not just IDs)
			.first() // first matching element
			.and_then(|e| {
				e.values // get its key-value pairs
					.iter()
					.find(|kv| kv.key.to_string() == "value")
			}) // find the "value" attr
			.map(|kv| kv.value.to_string())) // convert to String
	}

	pub fn has_evidence(
		&self,
		resource_id: DbId,
		evidence_name: &str,
	) -> Result<bool, agdb::DbError> {
		let result = self.db.exec(
			QueryBuilder::select()
				.search()
				.depth_first()
				.from(resource_id)
				.where_()
				.key("provider")
				.value(Comparison::Equal("wa2".into()))
				.and()
				.key("kind")
				.value(Comparison::Equal("Evidence".into()))
				.and()
				.key("value")
				.value(Comparison::Equal(evidence_name.into()))
				.query(),
		)?;

		Ok(!result.elements.is_empty())
	}

	pub fn find_nodes(
		&self,
		kind: &str,
		attr_key: &str,
		attr_value: &str,
	) -> Result<Vec<DbId>, agdb::DbError> {
		let result = self.db.exec(
			QueryBuilder::select()
				.search()
				.depth_first()
				.from(1) // Now reliably the Deployment root
				.where_()
				.key("kind")
				.value(Comparison::Equal(kind.into()))
				.and()
				.key(attr_key)
				.value(Comparison::Equal(attr_value.into()))
				.query(),
		)?;
		Ok(result.ids())
	}

	//pub fn node(&self, id: NodeId) -> Result<&Node, NodeError> {
	//self.nodes.get(id).ok_or(NodeError::InvalidNode)
	//}

	// pub fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, NodeError> {
	// 	//self.nodes.get_mut(id).ok_or(NodeError::InvalidNode)
	// }

	pub fn annotate(&mut self, id: DbId, annotation: Annotation) -> Result<(), NodeError> {
		//self.node_mut(id)?.annotations.push(annotation);
		Ok(())
	}

	pub fn guidance(&self) -> Vec<Guide> {
		let mut guides = Vec::<Guide>::default();

		let stores = self
			.find_nodes("Resource", "wa2_kind", "Store")
			.expect("query store nodes");
		for id in stores {
			// check for mandatory tags
			if self.get_tag(id, "DataSensitivity").expect("msg").is_none() {
				let logical_id = self
					.get_logical_id(id)
					.unwrap()
					.expect("logical_id must exist");

				guides.push(Guide {
					node: id,
					logical_id,
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

			if self.get_tag(id, "DataCriticality").expect("msg").is_none() {
				let logical_id = self
					.get_logical_id(id)
					.unwrap()
					.expect("logical_id must exist");

				guides.push(Guide {
					node: id,
					logical_id,
					tldr: "Tag this resource for DataCriticality".to_string(),
					message: "All stores of information should have a data criticality."
						.to_string(),
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
			if let Some(criticality) = self.get_tag(id, "DataCriticality").expect("msg") {
				if (criticality == "MissionCritical" || criticality == "BusinessCritical")
					&& !self
						.has_evidence(id, "DataResiliance")
						.expect("evidence check")
				{
					let logical_id = self
						.get_logical_id(id)
						.unwrap()
						.expect("logical_id must exist");

					guides.push(Guide {
						node: id,
						logical_id,
						tldr: "Backup this resource".to_string(),
						message: "All critical stores of information should be backed up."
							.to_string(),
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
}

#[derive(Debug)]
pub struct Guide {
	pub node: DbId,
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

use std::{collections::HashMap, fmt};

pub struct PrettySystem<'a> {
	pub system: &'a System,
}

fn get_value(elem: &DbElement, key: &str) -> Option<String> {
	elem.values
		.iter()
		.find(|kv| kv.key.to_string() == key)
		.map(|kv| kv.value.to_string())
}

fn print_graph(
	f: &mut fmt::Formatter<'_>,
	nodes: &HashMap<DbId, String>,
	edges: &HashMap<DbId, Vec<(String, DbId)>>,
	target: DbId,
	depth: usize,
	prefix: char,
) -> fmt::Result {
	// writeln!(f, "nodes {nodes:?}");
	// writeln!(f, "edges {edges:?}");

	if let Some(line) = nodes.get(&target) {
		let indent = " ".repeat(depth);
		writeln!(f, "{indent}{prefix}{}", line)?;

		if let Some(out_edges) = edges.get(&target) {
			for edge in out_edges {
				write!(f, "{:2}->{:2}", target.as_index(), edge.1.as_index())?;
				print_graph(f, &nodes, &edges, edge.1, depth + 1, '+')?;
			}
		}
	}

	Ok(())
}

impl<'a> fmt::Display for PrettySystem<'a> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		const ROOT_ID: DbId = DbId(1);
		let result = self
			.system
			.db
			.exec(
				QueryBuilder::select()
					.ids(QueryBuilder::search().from(ROOT_ID).query())
					.query(),
			)
			.ok()
			.expect("query");

		let mut nodes: HashMap<DbId, String> = HashMap::new();
		let mut edges: HashMap<DbId, Vec<(String, DbId)>> = HashMap::new();
		for elem in &result.elements {
			if elem.id.0 > 0 {
				let provider = get_value(&elem, "provider").unwrap_or_default();
				let kind = get_value(&elem, "kind").unwrap_or_default();
				let typed = get_value(&elem, "type").unwrap_or_default();
				let logical_id = get_value(&elem, "logical_id")
					.or_else(|| get_value(&elem, "name"))
					.unwrap_or_default();
				let key = get_value(&elem, "key").unwrap_or_default();
				let value = get_value(&elem, "value").unwrap_or_default();

				let tag = if key.is_empty() {
					"(none)"
				} else {
					&format!("#{}={}", key, value)
				};

				let wa2_kind = get_value(&elem, "wa2_kind").unwrap_or(tag.to_string());

				let line = format!(
					"{logical_id} = {provider}:{kind}(#{}): {typed}, WA2 -> {}",
					elem.id.0, wa2_kind
				);
				nodes.insert(elem.id, line);
			} else {
				let from = elem.from.unwrap();
				let kind = get_value(&elem, "kind").unwrap_or_default();
				let to = elem.to.unwrap();
				edges.entry(from).or_default().push((kind, to));
			}
		}

		write!(f, "{:2}->{:2}", 0, ROOT_ID.as_index())?;
		print_graph(f, &nodes, &edges, ROOT_ID, 0, '\\')?;

		Ok(())
	}
}
