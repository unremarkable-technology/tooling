use std::collections::HashMap;
use std::fmt;

use tower_lsp::lsp_types::Range;

// ─── Identifiers ───

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EntityId(pub u32);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct StatementId(pub u32);

impl EntityId {
	pub fn index(&self) -> usize {
		self.0 as usize
	}
}

impl StatementId {
	pub fn index(&self) -> usize {
		self.0 as usize
	}
}

// ─── Values ───

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
	Entity(EntityId),
	Literal(String),
	Number(i64),
}

impl Value {
	pub fn as_entity(&self) -> Option<EntityId> {
		match self {
			Value::Entity(id) => Some(*id),
			_ => None,
		}
	}

	pub fn as_literal(&self) -> Option<&str> {
		match self {
			Value::Literal(s) => Some(s),
			_ => None,
		}
	}

	pub fn as_number(&self) -> Option<i64> {
		match self {
			Value::Number(n) => Some(*n),
			_ => None,
		}
	}
}

// ─── Query ───

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Axis {
	Child,
	Descendant,
	Parent,
	Ancestor,
	Follow(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cmp {
	Eq,
	Ne,
}

#[derive(Clone, Debug)]
pub struct Filter {
	pub predicate: String,
	pub comparison: Cmp,
	pub value: Value,
}

#[derive(Clone, Debug)]
pub struct Step {
	pub axis: Axis,
	pub type_name: Option<String>, // None = wildcard
	pub filters: Vec<Filter>,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
	pub steps: Vec<Step>,
}

impl Query {
	pub fn new() -> Self {
		Self { steps: vec![] }
	}

	/// Start with descendant axis
	pub fn descendant(type_name: &str) -> Self {
		Self {
			steps: vec![Step {
				axis: Axis::Descendant,
				type_name: Some(type_name.to_string()),
				filters: vec![],
			}],
		}
	}

	/// Start with child axis
	pub fn child(type_name: &str) -> Self {
		Self {
			steps: vec![Step {
				axis: Axis::Child,
				type_name: Some(type_name.to_string()),
				filters: vec![],
			}],
		}
	}

	/// Chain another step
	pub fn then(mut self, axis: Axis, type_name: Option<&str>) -> Self {
		self.steps.push(Step {
			axis,
			type_name: type_name.map(|s| s.to_string()),
			filters: vec![],
		});
		self
	}

	/// Add filter to last step
	pub fn filter(mut self, predicate: &str, cmp: Cmp, value: Value) -> Self {
		if let Some(step) = self.steps.last_mut() {
			step.filters.push(Filter {
				predicate: predicate.to_string(),
				comparison: cmp,
				value,
			});
		}
		self
	}

	pub fn follow(predicate: &str) -> Self {
		Self {
			steps: vec![Step {
				axis: Axis::Follow(predicate.to_string()),
				type_name: None,
				filters: vec![],
			}],
		}
	}

	pub fn then_follow(mut self, predicate: &str) -> Self {
		self.steps.push(Step {
			axis: Axis::Follow(predicate.to_string()),
			type_name: None,
			filters: vec![],
		});
		self
	}
}

// ─── Errors ───

#[derive(Debug)]
pub enum ModelError {
	NotFound(String),
	CardinalityExceeded {
		predicate: EntityId,
		limit: usize,
	},
	Conflict {
		subject: EntityId,
		predicate: EntityId,
		existing: Value,
		proposed: Value,
	},
	InvalidStatement(String),
	GuidanceRequired,
}

impl fmt::Display for ModelError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			ModelError::NotFound(name) => write!(f, "Entity not found: {}", name),
			ModelError::CardinalityExceeded { predicate, limit } => {
				write!(
					f,
					"Cardinality exceeded for {:?}, limit {}",
					predicate, limit
				)
			}
			ModelError::Conflict {
				subject,
				predicate,
				existing,
				proposed,
			} => {
				write!(
					f,
					"Conflict at {:?}.{:?}: existing {:?}, proposed {:?}",
					subject, predicate, existing, proposed
				)
			}
			ModelError::InvalidStatement(msg) => write!(f, "Invalid statement: {}", msg),
			ModelError::GuidanceRequired => write!(f, "Guidance required"),
		}
	}
}

impl std::error::Error for ModelError {}

// ─── Data ───

#[derive(Clone, Debug)]
pub struct Entity {
	pub id: EntityId,
	pub namespace: Option<EntityId>,
	pub localname: Option<String>, // None = blank node
}

#[derive(Clone, Debug)]
pub struct Statement {
	pub id: StatementId,
	pub subject: EntityId,
	pub predicate: EntityId,
	pub object: Value,
}

// ─── Well-known entities ───
pub const NAMESPACE: EntityId = EntityId(0); // wa2
pub const TYPE: EntityId = EntityId(1); // wa2:Type
pub const NAMESPACE_TYPE: EntityId = EntityId(2); // wa2:Namespace
pub const PREDICATE: EntityId = EntityId(3); // wa2:Predicate
pub const TYPE_PRED: EntityId = EntityId(4); // wa2:type
pub const NAMESPACE_PRED: EntityId = EntityId(5); // wa2:namespace
pub const CARDINALITY: EntityId = EntityId(6); // wa2:cardinality
pub const CONTAINS: EntityId = EntityId(7); // wa2:contains
pub const DOMAIN: EntityId = EntityId(8); // wa2:domain
pub const RANGE: EntityId = EntityId(9); // wa2:range
pub const SUB_TYPE_OF: EntityId = EntityId(10); // wa2:subTypeOf
pub const BOOTSTRAP_COUNT: u32 = 11;

// ─── Model ───

#[derive(Clone, Debug)]
pub struct Model {
	// Storage
	entities: Vec<Entity>,
	statements: Vec<Statement>,

	// Name resolution indexes
	by_name: HashMap<String, EntityId>,                  // "aws" → id
	by_qualified: HashMap<(EntityId, String), EntityId>, // (aws_id, "Bucket") → id

	// Traversal indexes
	by_subject: HashMap<EntityId, Vec<StatementId>>, // outgoing
	by_object: HashMap<EntityId, Vec<StatementId>>,  // incoming (entities only)
	by_predicate: HashMap<EntityId, Vec<StatementId>>, // all uses of predicate

	// Root entity for queries
	root: Option<EntityId>,

	// Source location tracking (sidecar)
	source_ranges: HashMap<EntityId, Range>,
}

impl Model {
	// ─── Construction ───

	/// Empty model, no bootstrap
	pub fn empty() -> Self {
		Self {
			entities: Vec::new(),
			statements: Vec::new(),
			by_name: HashMap::new(),
			by_qualified: HashMap::new(),
			by_subject: HashMap::new(),
			by_object: HashMap::new(),
			by_predicate: HashMap::new(),
			root: None,
			source_ranges: HashMap::new(),
		}
	}

	/// Model with wa2 kernel loaded
	pub fn bootstrap() -> Self {
		let mut model = Self::empty();

		// Create bootstrap entities directly (avoiding apply for circular refs)
		// wa2 namespace - only meta-types
		model.add_entity_raw(Some("wa2"), None); // 0: wa2
		model.add_entity_raw(Some("Type"), Some(NAMESPACE)); // 1: wa2:Type
		model.add_entity_raw(Some("Namespace"), Some(NAMESPACE)); // 2: wa2:Namespace
		model.add_entity_raw(Some("Predicate"), Some(NAMESPACE)); // 3: wa2:Predicate
		model.add_entity_raw(Some("type"), Some(NAMESPACE)); // 4: wa2:type
		model.add_entity_raw(Some("namespace"), Some(NAMESPACE)); // 5: wa2:namespace
		model.add_entity_raw(Some("cardinality"), Some(NAMESPACE)); // 6: wa2:cardinality
		model.add_entity_raw(Some("contains"), Some(NAMESPACE)); // 7: wa2:contains
		model.add_entity_raw(Some("domain"), Some(NAMESPACE)); // 8: wa2:domain
		model.add_entity_raw(Some("range"), Some(NAMESPACE)); // 9: wa2:range
		model.add_entity_raw(Some("subTypeOf"), Some(NAMESPACE)); // 10: wa2:subTypeOf

		// Type statements for bootstrap entities
		model.add_statement_raw(NAMESPACE, TYPE_PRED, Value::Entity(NAMESPACE_TYPE));
		model.add_statement_raw(TYPE, TYPE_PRED, Value::Entity(TYPE));
		model.add_statement_raw(NAMESPACE_TYPE, TYPE_PRED, Value::Entity(TYPE));
		model.add_statement_raw(PREDICATE, TYPE_PRED, Value::Entity(TYPE));
		model.add_statement_raw(TYPE_PRED, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(NAMESPACE_PRED, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(CARDINALITY, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(CONTAINS, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(DOMAIN, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(RANGE, TYPE_PRED, Value::Entity(PREDICATE));
		model.add_statement_raw(SUB_TYPE_OF, TYPE_PRED, Value::Entity(PREDICATE));
      
		// Cardinality constraints
		model.add_statement_raw(TYPE_PRED, CARDINALITY, Value::Number(1));
		model.add_statement_raw(NAMESPACE_PRED, CARDINALITY, Value::Number(1));
		model.add_statement_raw(CARDINALITY, CARDINALITY, Value::Number(1));

		model
	}

	/// Internal: add entity without going through apply
	fn add_entity_raw(&mut self, localname: Option<&str>, namespace: Option<EntityId>) -> EntityId {
		let id = EntityId(self.entities.len() as u32);
		let entity = Entity {
			id,
			namespace,
			localname: localname.map(|s| s.to_string()),
		};
		self.entities.push(entity);

		// Update indexes
		if let Some(name) = localname {
			if let Some(ns) = namespace {
				self.by_qualified.insert((ns, name.to_string()), id);
			} else {
				self.by_name.insert(name.to_string(), id);
			}
		}

		id
	}

	/// Internal: add statement without cardinality checks
	fn add_statement_raw(
		&mut self,
		subject: EntityId,
		predicate: EntityId,
		object: Value,
	) -> StatementId {
		let id = StatementId(self.statements.len() as u32);
		let statement = Statement {
			id,
			subject,
			predicate,
			object: object.clone(),
		};
		self.statements.push(statement);

		// Update indexes
		self.by_subject.entry(subject).or_default().push(id);
		self.by_predicate.entry(predicate).or_default().push(id);
		if let Value::Entity(obj_id) = object {
			self.by_object.entry(obj_id).or_default().push(id);
		}

		id
	}

	// ─── Entity creation ───

	/// Create a blank (unnamed) node
	pub fn blank(&mut self) -> EntityId {
		self.add_entity_raw(None, None)
	}

	/// Ensure entity exists, creating if needed
	pub fn ensure_entity(&mut self, name: &str) -> EntityId {
		if let Some(id) = self.resolve(name) {
			return id;
		}

		// Parse namespace:local
		if let Some((ns_name, local)) = name.split_once(':') {
			let ns_id = self.ensure_entity(ns_name);
			self.add_entity_raw(Some(local), Some(ns_id))
		} else {
			self.add_entity_raw(Some(name), None)
		}
	}

	// ─── Statements ───

	/// Add statement with ensure semantics
	pub fn apply(
		&mut self,
		subject: &str,
		predicate: &str,
		object: &str,
	) -> Result<StatementId, ModelError> {
		let subj_id = self.ensure_entity(subject);
		let pred_id = self.ensure_entity(predicate);
		let obj_val = self.parse_object(object);

		self.apply_value(subj_id, pred_id, obj_val)
	}

	/// Add statement to blank node or known entity
	pub fn apply_to(
		&mut self,
		subject: EntityId,
		predicate: &str,
		object: &str,
	) -> Result<StatementId, ModelError> {
		let pred_id = self.ensure_entity(predicate);
		let obj_val = self.parse_object(object);

		self.apply_value(subject, pred_id, obj_val)
	}

	/// Add statement with entity ID as object
	pub fn apply_entity(
		&mut self,
		subject: EntityId,
		predicate: &str,
		object: EntityId,
	) -> Result<StatementId, ModelError> {
		let pred_id = self.ensure_entity(predicate);
		self.apply_value(subject, pred_id, Value::Entity(object))
	}

	/// Core apply logic with cardinality checking
	fn apply_value(
		&mut self,
		subject: EntityId,
		predicate: EntityId,
		object: Value,
	) -> Result<StatementId, ModelError> {
		// Check for existing statements with same subject+predicate
		let existing: Vec<_> = self
			.by_subject
			.get(&subject)
			.map(|stmts| {
				stmts
					.iter()
					.filter(|&&stmt_id| self.statements[stmt_id.index()].predicate == predicate)
					.copied()
					.collect()
			})
			.unwrap_or_default();

		// Check if already present (idempotent)
		for &stmt_id in &existing {
			if self.statements[stmt_id.index()].object == object {
				return Ok(stmt_id);
			}
		}

		// Check cardinality
		if let Some(limit) = self.cardinality(predicate) {
			if existing.len() >= limit {
				let existing_obj = self.statements[existing[0].index()].object.clone();
				return Err(ModelError::Conflict {
					subject,
					predicate,
					existing: existing_obj,
					proposed: object,
				});
			}
		}

		Ok(self.add_statement_raw(subject, predicate, object))
	}

	/// Parse object string into Value
	fn parse_object(&mut self, object: &str) -> Value {
		// Literal string in quotes
		if object.starts_with('"') && object.ends_with('"') {
			return Value::Literal(object[1..object.len() - 1].to_string());
		}

		// Try parsing as number
		if let Ok(n) = object.parse::<i64>() {
			return Value::Number(n);
		}

		// Otherwise it's an entity reference
		Value::Entity(self.ensure_entity(object))
	}

	/// Check if statement exists
	pub fn has(&self, subject: EntityId, predicate: EntityId, object: &Value) -> bool {
		self.by_subject
			.get(&subject)
			.map(|stmts| {
				stmts.iter().any(|&stmt_id| {
					let stmt = &self.statements[stmt_id.index()];
					stmt.predicate == predicate && &stmt.object == object
				})
			})
			.unwrap_or(false)
	}

	// ─── Resolution ───

	/// Resolve name to entity ("aws" or "aws:Bucket")
	pub fn resolve(&self, name: &str) -> Option<EntityId> {
		if let Some((ns_name, local)) = name.split_once(':') {
			let ns_id = self.resolve(ns_name)?;
			self.by_qualified.get(&(ns_id, local.to_string())).copied()
		} else {
			self.by_name.get(name).copied()
		}
	}

	/// Get entity's qualified name
	pub fn qualified_name(&self, id: EntityId) -> String {
		let entity = &self.entities[id.index()];
		match (&entity.namespace, &entity.localname) {
			(Some(ns_id), Some(local)) => {
				format!("{}:{}", self.qualified_name(*ns_id), local)
			}
			(None, Some(local)) => local.clone(),
			(_, None) => format!("_:{}", id.0),
		}
	}

	// ─── Traversal ───

	/// Statements where entity is subject
	pub fn outgoing(&self, id: EntityId) -> Vec<StatementId> {
		self.by_subject.get(&id).cloned().unwrap_or_default()
	}

	/// Statements where entity is object
	pub fn incoming(&self, id: EntityId) -> Vec<StatementId> {
		self.by_object.get(&id).cloned().unwrap_or_default()
	}

	/// Direct children via wa2:contains
	pub fn children(&self, id: EntityId) -> Vec<EntityId> {
		self.by_subject
			.get(&id)
			.map(|stmts| {
				stmts
					.iter()
					.filter_map(|&stmt_id| {
						let stmt = &self.statements[stmt_id.index()];
						if stmt.predicate == CONTAINS {
							stmt.object.as_entity()
						} else {
							None
						}
					})
					.collect()
			})
			.unwrap_or_default()
	}

	/// Direct parents (entities that contain this one)
	pub fn parents(&self, id: EntityId) -> Vec<EntityId> {
		self.by_object
			.get(&id)
			.map(|stmts| {
				stmts
					.iter()
					.filter_map(|&stmt_id| {
						let stmt = &self.statements[stmt_id.index()];
						if stmt.predicate == CONTAINS {
							Some(stmt.subject)
						} else {
							None
						}
					})
					.collect()
			})
			.unwrap_or_default()
	}

	/// Check if entity has given type
	pub fn has_type(&self, entity: EntityId, type_id: EntityId) -> bool {
		self.has(entity, TYPE_PRED, &Value::Entity(type_id))
	}

	/// Get all types for an entity
	pub fn types(&self, entity: EntityId) -> Vec<EntityId> {
		self.by_subject
			.get(&entity)
			.map(|stmts| {
				stmts
					.iter()
					.filter_map(|&stmt_id| {
						let stmt = &self.statements[stmt_id.index()];
						if stmt.predicate == TYPE_PRED {
							stmt.object.as_entity()
						} else {
							None
						}
					})
					.collect()
			})
			.unwrap_or_default()
	}

	// ─── Query execution ───

	/// Set the root for queries
	pub fn set_root(&mut self, id: EntityId) {
		self.root = Some(id);
	}

	/// Execute query from root
	pub fn query(&self, query: &Query) -> Vec<EntityId> {
		let start = self.root.map(|id| vec![id]).unwrap_or_default();
		self.query_from(&start, query)
	}

	/// Execute query from given starting set
	pub fn query_from(&self, start: &[EntityId], query: &Query) -> Vec<EntityId> {
		let mut current_set: Vec<EntityId> = start.to_vec();
		let mut current_namespace: Option<EntityId> = None;

		for step in &query.steps {
			let mut next_set = Vec::new();

			// Resolve type name with namespace inheritance
			let type_id = step.type_name.as_ref().and_then(|name| {
				if name.contains(':') {
					// Explicit namespace - resolve and update context
					let id = self.resolve(name)?;
					let entity = &self.entities[id.index()];
					current_namespace = entity.namespace;
					Some(id)
				} else if let Some(ns) = current_namespace {
					// Inherit namespace
					self.by_qualified.get(&(ns, name.clone())).copied()
				} else {
					// No namespace context, try bare name
					self.resolve(name)
				}
			});

			for &id in &current_set {
				let candidates = match &step.axis {
					Axis::Child => self.children(id),
					Axis::Descendant => self.descendants(id),
					Axis::Parent => self.parents(id),
					Axis::Ancestor => self.ancestors(id),
					Axis::Follow(pred_name) => {
						if let Some(pred_id) = self.resolve(&pred_name) {
							self.get_all(id, pred_id)
								.into_iter()
								.filter_map(|v| v.as_entity())
								.collect()
						} else {
							vec![]
						}
					}
				};

				for candidate in candidates {
					// Type filter
					if let Some(tid) = type_id {
						if !self.has_type(candidate, tid) {
							continue;
						}
					}

					// Predicate filters
					if !self.matches_filters(candidate, &step.filters, current_namespace) {
						continue;
					}

					if !next_set.contains(&candidate) {
						next_set.push(candidate);
					}
				}
			}

			current_set = next_set;
		}

		current_set
	}

	/// Get all descendants
	fn descendants(&self, id: EntityId) -> Vec<EntityId> {
		let mut result = Vec::new();
		let mut stack = vec![id];

		while let Some(current) = stack.pop() {
			for child in self.children(current) {
				if !result.contains(&child) {
					result.push(child);
					stack.push(child);
				}
			}
		}

		result
	}

	/// Get all ancestors
	fn ancestors(&self, id: EntityId) -> Vec<EntityId> {
		let mut result = Vec::new();
		let mut stack = vec![id];

		while let Some(current) = stack.pop() {
			for parent in self.parents(current) {
				if !result.contains(&parent) {
					result.push(parent);
					stack.push(parent);
				}
			}
		}

		result
	}

	/// Check if entity matches all filters
	fn matches_filters(
		&self,
		entity: EntityId,
		filters: &[Filter],
		ns_context: Option<EntityId>,
	) -> bool {
		for filter in filters {
			// Resolve predicate name
			let pred_id = if filter.predicate.contains(':') {
				self.resolve(&filter.predicate)
			} else if let Some(ns) = ns_context {
				self.by_qualified
					.get(&(ns, filter.predicate.clone()))
					.copied()
			} else {
				self.resolve(&filter.predicate)
			};

			let Some(pred_id) = pred_id else {
				return false;
			};

			let values = self.get_all(entity, pred_id);
			let matches = values.iter().any(|v| match filter.comparison {
				Cmp::Eq => v == &filter.value,
				Cmp::Ne => v != &filter.value,
			});

			if !matches {
				return false;
			}
		}

		true
	}

	// ─── Accessors ───

	/// Get statement by id
	pub fn statement(&self, id: StatementId) -> &Statement {
		&self.statements[id.index()]
	}

	/// Get entity by id
	pub fn entity(&self, id: EntityId) -> &Entity {
		&self.entities[id.index()]
	}

	/// Get single value for predicate (assumes cardinality 1)
	pub fn get(&self, subject: EntityId, predicate: EntityId) -> Option<Value> {
		self.by_subject.get(&subject).and_then(|stmts| {
			stmts
				.iter()
				.find(|&&stmt_id| self.statements[stmt_id.index()].predicate == predicate)
				.map(|&stmt_id| self.statements[stmt_id.index()].object.clone())
		})
	}

	/// Get all values for predicate
	pub fn get_all(&self, subject: EntityId, predicate: EntityId) -> Vec<Value> {
		self.by_subject
			.get(&subject)
			.map(|stmts| {
				stmts
					.iter()
					.filter(|&&stmt_id| self.statements[stmt_id.index()].predicate == predicate)
					.map(|&stmt_id| self.statements[stmt_id.index()].object.clone())
					.collect()
			})
			.unwrap_or_default()
	}

	/// Get literal value for a predicate by name
	pub fn get_literal(&self, subject: EntityId, predicate: &str) -> Option<String> {
		let pred_id = self.resolve(predicate)?;
		self.get(subject, pred_id)
			.and_then(|v| v.as_literal().map(|s| s.to_string()))
	}

	/// Check cardinality constraint for predicate
	pub fn cardinality(&self, predicate: EntityId) -> Option<usize> {
		self.get(predicate, CARDINALITY)
			.and_then(|v| v.as_number())
			.map(|n| n as usize)
	}

	// ─── Statistics ───

	pub fn entity_count(&self) -> usize {
		self.entities.len()
	}

	pub fn statement_count(&self) -> usize {
		self.statements.len()
	}

	// ─── Source Location Tracking ───

	/// Associate a source range with an entity
	pub fn set_range(&mut self, entity: EntityId, range: Range) {
		self.source_ranges.insert(entity, range);
	}

	/// Get the source range for an entity
	pub fn get_range(&self, entity: EntityId) -> Option<Range> {
		self.source_ranges.get(&entity).copied()
	}

	/// Find entity at a given position (for go-to-definition, hover)
	/// When multiple entities contain the position, returns the one with the smallest range
	pub fn entity_at_position(&self, position: tower_lsp::lsp_types::Position) -> Option<EntityId> {
		let mut best: Option<(EntityId, Range)> = None;

		for (&entity_id, &range) in &self.source_ranges {
			if Self::position_in_range(position, range) {
				let dominated = best.as_ref().map_or(true, |(_, best_range)| {
					Self::range_smaller(&range, best_range)
				});

				if dominated {
					best = Some((entity_id, range));
				}
			}
		}

		best.map(|(id, _)| id)
	}

	/// Returns true if `a` is strictly smaller than `b`
	fn range_smaller(a: &Range, b: &Range) -> bool {
		let a_lines = a.end.line - a.start.line;
		let b_lines = b.end.line - b.start.line;

		if a_lines != b_lines {
			return a_lines < b_lines;
		}

		// Same number of lines - compare character span
		let a_chars = if a_lines == 0 {
			a.end.character - a.start.character
		} else {
			a.end.character // approximate for multi-line
		};

		let b_chars = if b_lines == 0 {
			b.end.character - b.start.character
		} else {
			b.end.character
		};

		a_chars < b_chars
	}

	// Add helper function outside impl:
	fn position_in_range(pos: tower_lsp::lsp_types::Position, range: Range) -> bool {
		if pos.line < range.start.line || pos.line > range.end.line {
			return false;
		}
		if pos.line == range.start.line && pos.character < range.start.character {
			return false;
		}
		if pos.line == range.end.line && pos.character > range.end.character {
			return false;
		}
		true
	}
}

// ─── Display ───

impl fmt::Display for Model {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(
			f,
			"Model ({} entities, {} statements)",
			self.entities.len(),
			self.statements.len()
		)?;
		writeln!(f, "─────────────────────────────────────")?;

		for stmt in &self.statements {
			let subj = self.qualified_name(stmt.subject);
			let pred = self.qualified_name(stmt.predicate);
			let obj = match &stmt.object {
				Value::Entity(id) => self.qualified_name(*id),
				Value::Literal(s) => format!("\"{}\"", s),
				Value::Number(n) => n.to_string(),
			};
			writeln!(f, "{:20} {:20} {}", subj, pred, obj)?;
		}

		Ok(())
	}
}

pub fn print_model_as_tree(model: &Model) -> String {
	use std::fmt::Write;

	let mut out = String::new();

	if let Some(root) = model.root {
		let mut visited = std::collections::HashSet::new();
		print_node(&mut out, model, root, 0, &mut visited);
	} else {
		writeln!(out, "(no root set)").unwrap();
	}

	out
}

fn print_node(
	out: &mut String,
	model: &Model,
	node: EntityId,
	depth: usize,
	visited: &mut std::collections::HashSet<EntityId>,
) {
	use std::fmt::Write;
	let indent = "  ".repeat(depth);
	let name = model.qualified_name(node);
	let types: Vec<String> = model
		.types(node)
		.iter()
		.map(|&t| model.qualified_name(t))
		.collect();

	// If already visited, just print a reference
	if visited.contains(&node) {
		let type_str = if types.is_empty() {
			String::new()
		} else {
			format!(" : {}", types.join(", "))
		};
		writeln!(out, "{}{}{} (→)", indent, name, type_str).unwrap();
		return;
	}
	visited.insert(node);

	let mut attrs = Vec::new();
	let mut linked_entities = Vec::new();
	let mut children = Vec::new();

	// Collect all outgoing statements in order
	for stmt_id in model.outgoing(node) {
		let stmt = model.statement(stmt_id);
		let pred_name = model.qualified_name(stmt.predicate);

		if pred_name == "wa2:type" {
			continue;
		}

		match &stmt.object {
			Value::Literal(lit) => {
				attrs.push(format!("{}=\"{}\"", pred_name, lit));
			}
			Value::Entity(child_id) => {
				if pred_name == "wa2:contains" {
					children.push(*child_id);
				} else {
					linked_entities.push((pred_name, *child_id));
				}
			}
			Value::Number(n) => {
				attrs.push(format!("{}={}", pred_name, n));
			}
		}
	}

	let type_str = if types.is_empty() {
		String::new()
	} else {
		format!(" : {}", types.join(", "))
	};

	let attr_str = if attrs.is_empty() {
		String::new()
	} else {
		format!(" [{}]", attrs.join(", "))
	};

	writeln!(out, "{}{}{}{}", indent, name, type_str, attr_str).unwrap();

	// Print linked entities first (predicates like cfn:resources, cfn:outputs)
	for (pred_name, child_id) in linked_entities {
		let child_entity = model.entity(child_id);
		let is_blank = child_entity.localname.is_none();

		if is_blank && !visited.contains(&child_id) {
			// Blank node - expand inline under the predicate
			writeln!(out, "{}  -{}-", indent, pred_name).unwrap();
			print_node(out, model, child_id, depth + 2, visited);
		} else {
			// Named entity or already visited - show as reference
			let child_name = model.qualified_name(child_id);
			let child_types: Vec<String> = model
				.types(child_id)
				.iter()
				.map(|&t| model.qualified_name(t))
				.collect();
			let child_type_str = if child_types.is_empty() {
				String::new()
			} else {
				format!(" : {}", child_types.join(", "))
			};
			writeln!(
				out,
				"{}  -{}- {}{} (→)",
				indent, pred_name, child_name, child_type_str
			)
			.unwrap();
		}
	}

	// Then print wa2:contains children (in statement order)
	for child in children {
		print_node(out, model, child, depth + 1, visited);
	}
}

// ─── Tests ───

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_bootstrap() {
		let model = Model::bootstrap();

		// Check wa2 namespace exists
		let wa2_id = model.resolve("wa2").expect("wa2 should exist");
		assert_eq!(wa2_id, NAMESPACE);

		// Check wa2:Type exists
		let type_id = model.resolve("wa2:Type").expect("wa2:Type should exist");
		assert_eq!(type_id, TYPE);

		// Check wa2:Type has type wa2:Type (self-referential)
		assert!(model.has_type(TYPE, TYPE));

		// Check wa2:type has cardinality 1
		assert_eq!(model.cardinality(TYPE_PRED), Some(1));
	}

	#[test]
	fn test_ensure_entity() {
		let mut model = Model::bootstrap();

		// Create simple entity
		let foo = model.ensure_entity("foo");
		assert_eq!(model.resolve("foo"), Some(foo));

		// Create namespaced entity
		let bar_baz = model.ensure_entity("bar:baz");
		assert_eq!(model.resolve("bar:baz"), Some(bar_baz));

		// bar namespace should also exist
		let bar = model.resolve("bar").expect("bar namespace should exist");
		assert_eq!(model.entity(bar_baz).namespace, Some(bar));

		// Idempotent
		assert_eq!(model.ensure_entity("foo"), foo);
		assert_eq!(model.ensure_entity("bar:baz"), bar_baz);
	}

	#[test]
	fn test_apply_statements() {
		let mut model = Model::bootstrap();

		// First ensure core types exist
		model.apply("core:Store", "wa2:type", "wa2:Type").unwrap();
		model.apply("core:Run", "wa2:type", "wa2:Type").unwrap();

		// Create entity and add type
		model.apply("bucket", "wa2:type", "core:Store").unwrap();

		let bucket = model.resolve("bucket").expect("bucket should exist");
		let store_type = model
			.resolve("core:Store")
			.expect("core:Store should exist");
		assert!(model.has_type(bucket, store_type));

		// Idempotent - same statement again
		model.apply("bucket", "wa2:type", "core:Store").unwrap();

		// Conflict - different type (cardinality 1)
		let result = model.apply("bucket", "wa2:type", "core:Run");
		assert!(matches!(result, Err(ModelError::Conflict { .. })));
	}

	#[test]
	fn test_apply_literals() {
		let mut model = Model::bootstrap();

		model.apply("bucket", "aws:name", "\"my-bucket\"").unwrap();

		let bucket = model.resolve("bucket").expect("bucket should exist");
		let name = model
			.get_literal(bucket, "aws:name")
			.expect("should have name");
		assert_eq!(name, "my-bucket");
	}

	#[test]
	fn test_blank_nodes() {
		let mut model = Model::bootstrap();

		let bucket = model.ensure_entity("bucket");
		model.apply_to(bucket, "wa2:type", "wa2:Store").unwrap();

		// Create blank node for tags container
		let tags = model.blank();
		model.apply_entity(bucket, "wa2:contains", tags).unwrap();
		model.apply_to(tags, "wa2:type", "aws:Tags").unwrap();

		// Create blank nodes for individual tags
		let tag1 = model.blank();
		model.apply_entity(tags, "wa2:contains", tag1).unwrap();
		model.apply_to(tag1, "aws:key", "\"Environment\"").unwrap();
		model.apply_to(tag1, "aws:value", "\"Production\"").unwrap();

		// Check structure via traversal
		let children = model.children(bucket);
		assert_eq!(children.len(), 1);
		assert_eq!(children[0], tags);

		let tag_children = model.children(tags);
		assert_eq!(tag_children.len(), 1);
		assert_eq!(tag_children[0], tag1);
	}

	#[test]
	fn test_query_descendants() {
		let mut model = Model::bootstrap();

		// Ensure core types exist
		model
			.apply("core:Deployment", "wa2:type", "wa2:Type")
			.unwrap();
		model.apply("core:Store", "wa2:type", "wa2:Type").unwrap();
		model.apply("core:Run", "wa2:type", "wa2:Type").unwrap();

		// Create root
		let root = model.ensure_entity("root");
		model.apply_to(root, "wa2:type", "core:Deployment").unwrap();
		model.set_root(root);

		// Create stores
		let bucket1 = model.ensure_entity("bucket1");
		model.apply_to(bucket1, "wa2:type", "core:Store").unwrap();
		model.apply_entity(root, "wa2:contains", bucket1).unwrap();

		let bucket2 = model.ensure_entity("bucket2");
		model.apply_to(bucket2, "wa2:type", "core:Store").unwrap();
		model.apply_entity(root, "wa2:contains", bucket2).unwrap();

		// Create a non-store
		let lambda = model.ensure_entity("lambda");
		model.apply_to(lambda, "wa2:type", "core:Run").unwrap();
		model.apply_entity(root, "wa2:contains", lambda).unwrap();

		// Query all stores
		let stores = model.query(&Query::descendant("core:Store"));
		assert_eq!(stores.len(), 2);
		assert!(stores.contains(&bucket1));
		assert!(stores.contains(&bucket2));
		assert!(!stores.contains(&lambda));
	}

	#[test]
	fn test_query_with_filter() {
		let mut model = Model::bootstrap();

		// Create root
		let root = model.ensure_entity("root");
		model.set_root(root);

		// Create bucket with tags
		let bucket = model.ensure_entity("bucket");
		model.apply_to(bucket, "wa2:type", "wa2:Store").unwrap();
		model.apply_entity(root, "wa2:contains", bucket).unwrap();

		let tags = model.blank();
		model.apply_to(tags, "wa2:type", "aws:Tags").unwrap();
		model.apply_entity(bucket, "wa2:contains", tags).unwrap();

		let tag = model.blank();
		model.apply_to(tag, "wa2:type", "aws:Tag").unwrap();
		model
			.apply_to(tag, "aws:key", "\"DataSensitivity\"")
			.unwrap();
		model
			.apply_to(tag, "aws:value", "\"Confidential\"")
			.unwrap();
		model.apply_entity(tags, "wa2:contains", tag).unwrap();

		// Query: find tags with specific key
		model.ensure_entity("aws:Tags"); // ensure types exist
		model.ensure_entity("aws:Tag");

		let result = model.query(
			&Query::descendant("wa2:Store")
				.then(Axis::Descendant, Some("aws:Tag"))
				.filter(
					"aws:key",
					Cmp::Eq,
					Value::Literal("DataSensitivity".to_string()),
				),
		);

		assert_eq!(result.len(), 1);
		let tag_value = model
			.get_literal(result[0], "aws:value")
			.expect("should have value");
		assert_eq!(tag_value, "Confidential");
	}

	#[test]
	fn test_contains_relationship() {
		let mut model = Model::bootstrap();

		let parent = model.ensure_entity("parent");
		let child = model.ensure_entity("child");

		model.apply_entity(parent, "wa2:contains", child).unwrap();

		// Check traversal
		assert_eq!(model.children(parent), vec![child]);
		assert_eq!(model.parents(child), vec![parent]);
	}

	#[test]
	fn test_multiple_types() {
		let mut model = Model::bootstrap();

		// Ensure core:Store exists
		model.apply("core:Store", "wa2:type", "wa2:Type").unwrap();

		let bucket = model.ensure_entity("bucket");
		model.apply_to(bucket, "wa2:type", "core:Store").unwrap();
		model
			.apply_to(bucket, "aws:vendorType", "\"AWS::S3::Bucket\"")
			.unwrap();

		let store_type = model
			.resolve("core:Store")
			.expect("core:Store should exist");
		assert!(model.has_type(bucket, store_type));

		let vendor_type = model
			.get_literal(bucket, "aws:vendorType")
			.expect("vendor type");
		assert_eq!(vendor_type, "AWS::S3::Bucket");
	}
}
