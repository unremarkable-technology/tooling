// check.rs
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use is_terminal::IsTerminal;
use owo_colors::OwoColorize;
use terminal_size::{Width, terminal_size};
use textwrap::{Options, wrap};
use unicode_width::UnicodeWidthStr;
use url::Url;

use wa2lsp::iaac::cloudformation::cfn_ir::types::CfnTemplate;
use wa2lsp::iaac::cloudformation::spec_cache::load_default_spec_store;
use wa2lsp::intents::kernel::{
	AnalysisResult, AssertFailure, AssertSeverity, Kernel, Modal, PolicyOutcome, RuleExecution,
};
use wa2lsp::intents::model::{EntityId, Model, Value};
use wa2lsp::intents::vendor::{DocumentFormat, Method, Vendor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
	Pass,
	Warn,
	Fail,
	Blocked,
	Info,
}

impl From<AssertSeverity> for Status {
	fn from(s: AssertSeverity) -> Self {
		match s {
			AssertSeverity::Error => Status::Fail,
			AssertSeverity::Warning => Status::Warn,
			AssertSeverity::Info => Status::Pass,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphNodeKind {
	Workload,
	Run,
	Store,
	Move,
	Evidence,

	Template,
	Resource,
	Parameter,
	PseudoParameter,
	Output,

	Ref,
	GetAtt,
	Sub,
	Intrinsic,

	Blank,
	Entity,
}

struct Reporter {
	interactive: bool,
	width: usize,
}

impl Reporter {
	fn new() -> Self {
		let interactive = std::io::stdout().is_terminal();
		let width = terminal_size()
			.map(|(Width(w), _)| w as usize)
			.unwrap_or(100);

		Self { interactive, width }
	}

	fn section(&self, title: &str) {
		println!();
		println!("{title}");
		println!("{}", "-".repeat(title.len()));
	}

	fn prepare(&self, status: Status, label: &str, detail: Option<&str>) {
		self.status_node("", true, status, label, detail);
	}

	fn tree(&self, prefix: &str, is_last: bool, status: Status, label: &str, detail: Option<&str>) {
		self.status_node(prefix, is_last, status, label, detail);
	}

	fn graph_tree(
		&self,
		prefix: &str,
		is_last: bool,
		kind: GraphNodeKind,
		label: &str,
		detail: Option<&str>,
	) {
		self.node(prefix, is_last, &self.graph_marker(kind), label, detail);
	}

	fn status_node(
		&self,
		prefix: &str,
		is_last: bool,
		status: Status,
		label: &str,
		detail: Option<&str>,
	) {
		self.node(prefix, is_last, &self.marker(status), label, detail);
	}

	fn node(&self, prefix: &str, is_last: bool, marker: &str, label: &str, detail: Option<&str>) {
		let branch = if prefix.is_empty() {
			""
		} else if is_last {
			"└─ "
		} else {
			"├─ "
		};

		let first_prefix = format!("{prefix}{branch}");
		let rest_prefix = if prefix.is_empty() {
			"   ".to_string()
		} else if is_last {
			format!("{prefix}   ")
		} else {
			format!("{prefix}│  ")
		};

		if marker.is_empty() {
			println!("{first_prefix}{label}");
		} else {
			println!("{first_prefix}{marker} {label}");
		}

		if let Some(detail) = detail {
			self.write_wrapped_block(&rest_prefix, detail);
		}
	}

	fn child_prefix(prefix: &str, is_last: bool) -> String {
		if prefix.is_empty() {
			if is_last {
				"   ".into()
			} else {
				"│  ".into()
			}
		} else if is_last {
			format!("{prefix}   ")
		} else {
			format!("{prefix}│  ")
		}
	}

	fn write_wrapped_block(&self, prefix: &str, text: &str) {
		let paragraphs: Vec<&str> = text
			.split("\n\n")
			.map(str::trim)
			.filter(|p| !p.is_empty())
			.collect();

		for (idx, para) in paragraphs.iter().enumerate() {
			let available = self.width.saturating_sub(UnicodeWidthStr::width(prefix));
			let options = Options::new(available.max(20));

			for line in wrap(para, options) {
				println!("{prefix}{}", line);
			}

			if idx + 1 != paragraphs.len() {
				println!("{prefix}");
			}
		}
	}

	fn marker(&self, status: Status) -> String {
		let s = match status {
			Status::Pass => "✓",
			Status::Warn => "⚠",
			Status::Fail => "✗",
			Status::Blocked => "•",
			Status::Info => "ℹ",
		};

		if !self.interactive {
			return s.to_string();
		}

		match status {
			Status::Pass => s.green().to_string(),
			Status::Warn => s.yellow().to_string(),
			Status::Fail => s.red().to_string(),
			Status::Blocked => s.dimmed().to_string(),
			Status::Info => s.blue().to_string(),
		}
	}

	fn graph_marker(&self, kind: GraphNodeKind) -> String {
		if !self.interactive {
			return String::new();
		}

		match kind {
			GraphNodeKind::Workload => "◎".bright_blue().to_string(),
			GraphNodeKind::Run => "●".bright_red().to_string(),
			GraphNodeKind::Store => "■".bright_blue().to_string(),
			GraphNodeKind::Move => "▶".bright_magenta().to_string(),
			GraphNodeKind::Evidence => "◌".bright_yellow().to_string(),

			GraphNodeKind::Template => "📦".to_string(),
			GraphNodeKind::Resource => "🧱".to_string(),
			GraphNodeKind::Parameter => "⚙".to_string(),
			GraphNodeKind::PseudoParameter => "🔧".to_string(),
			GraphNodeKind::Output => "📤".to_string(),

			GraphNodeKind::Ref => "🔗".to_string(),
			GraphNodeKind::GetAtt => "📎".to_string(),
			GraphNodeKind::Sub => "🧩".to_string(),
			GraphNodeKind::Intrinsic => "ƒ".bright_cyan().to_string(),

			GraphNodeKind::Blank => "•".dimmed().to_string(),
			GraphNodeKind::Entity => "·".dimmed().to_string(),
		}
	}
}

struct ValidationReport {
	status: Status,
	label: String,
	detail: Option<String>,
}

async fn run_validation(target_text: String, target_uri: Url) -> ValidationReport {
	let format = DocumentFormat::from_language_id_or_path(None, &target_uri);

	let template = match format {
		DocumentFormat::Json => CfnTemplate::from_json(&target_text, &target_uri),
		DocumentFormat::Yaml => CfnTemplate::from_yaml(&target_text, &target_uri),
	};

	let template = match template {
		Ok(t) => t,
		Err(diags) => {
			let detail = diags
				.into_iter()
				.map(|d| d.message)
				.collect::<Vec<_>>()
				.join("\n\n");

			return ValidationReport {
				status: Status::Fail,
				label: "Parse CloudFormation template".to_string(),
				detail: Some(detail),
			};
		}
	};

	let spec_store = match load_default_spec_store().await {
		Ok(store) => store,
		Err(e) => {
			return ValidationReport {
				status: Status::Fail,
				label: "Load CloudFormation specification".to_string(),
				detail: Some(e.to_string()),
			};
		}
	};

	let diags = template.validate_against_spec(&spec_store);
	if diags.is_empty() {
		ValidationReport {
			status: Status::Pass,
			label: "Validate CloudFormation against specification".to_string(),
			detail: None,
		}
	} else {
		let detail = diags
			.into_iter()
			.map(|d| d.message)
			.collect::<Vec<_>>()
			.join("\n\n");

		ValidationReport {
			status: Status::Fail,
			label: "Validate CloudFormation against specification".to_string(),
			detail: Some(detail),
		}
	}
}

pub async fn run(
	profile: &str,
	target: &Path,
	entry: Option<&Path>,
	graph: bool,
	validate: bool,
	verbose: bool,
) -> ExitCode {
	let out = Reporter::new();
	out.section("PREPARE");

	if !target.exists() {
		out.prepare(
			Status::Fail,
			&format!("Read target {}", target.display()),
			Some("Target file not found."),
		);
		return ExitCode::FAILURE;
	}

	let target_text = match fs::read_to_string(target) {
		Ok(t) => {
			out.prepare(
				Status::Pass,
				&format!("Read target {}", target.display()),
				None,
			);
			t
		}
		Err(e) => {
			out.prepare(
				Status::Fail,
				&format!("Read target {}", target.display()),
				Some(&e.to_string()),
			);
			return ExitCode::FAILURE;
		}
	};

	let target_uri = Url::from_file_path(
		target
			.canonicalize()
			.unwrap_or_else(|_| target.to_path_buf()),
	)
	.unwrap_or_else(|_| Url::parse("file:///unknown").unwrap());

	let format = DocumentFormat::from_language_id_or_path(None, &target_uri);

	let validation_task = if validate {
		out.prepare(
			Status::Blocked,
			"Schedule CloudFormation validation",
			Some("Validation will run concurrently and report after results."),
		);

		let validation_text = target_text.clone();
		let validation_uri = target_uri.clone();
		Some(tokio::spawn(async move {
			run_validation(validation_text, validation_uri).await
		}))
	} else {
		out.prepare(
			Status::Blocked,
			"CloudFormation validation",
			Some("Disabled with --novalidation."),
		);
		None
	};

	let skip_quickstart = entry.is_some();
	let mut kernel = Kernel::new(skip_quickstart);
	out.prepare(Status::Pass, "Initialise kernel", None);

	if let Some(entry_path) = entry {
		if !entry_path.exists() {
			out.prepare(
				Status::Fail,
				&format!("Read intent entry {}", entry_path.display()),
				Some("Entry file not found."),
			);
			return ExitCode::FAILURE;
		}

		match kernel.load_intent(entry_path) {
			Ok(_) => {
				out.prepare(
					Status::Pass,
					&format!("Parse intent entry {}", entry_path.display()),
					None,
				);
			}
			Err(e) => {
				out.prepare(
					Status::Fail,
					&format!("Parse intent entry {}", entry_path.display()),
					Some(&e.to_string()),
				);
				return ExitCode::FAILURE;
			}
		}
	}

	if let Err(e) = kernel.set_profile(profile.to_string()) {
		out.prepare(
			Status::Fail,
			&format!("Select profile {profile}"),
			Some(&e.to_string()),
		);
		return ExitCode::FAILURE;
	}
	out.prepare(Status::Pass, &format!("Select profile {profile}"), None);

	let result = kernel.analyse(
		&target_text,
		&target_uri,
		format,
		Vendor::Aws,
		Method::CloudFormation,
	);

	let analysis = match result {
		Ok(analysis) => {
			out.prepare(Status::Pass, "Run analysis", None);
			analysis
		}
		Err(diags) => {
			let detail = diags
				.into_iter()
				.map(|d| d.message)
				.collect::<Vec<_>>()
				.join("\n\n");
			out.prepare(Status::Fail, "Run analysis", Some(&detail));
			return ExitCode::FAILURE;
		}
	};

	out.section("RESULTS");

	let exit_code = print_results(&out, profile, &analysis, verbose);

	if graph {
		out.section("GRAPH");
		print_model_graph(&out, &analysis.model);
	}

	if let Some(task) = validation_task {
		out.section("VALIDATION");

		match task.await {
			Ok(report) => {
				out.tree(
					"",
					true,
					report.status,
					&report.label,
					report.detail.as_deref(),
				);
				if report.status == Status::Fail {
					return ExitCode::FAILURE;
				}
			}
			Err(e) => {
				out.tree(
					"",
					true,
					Status::Fail,
					"Run CloudFormation validation",
					Some(&e.to_string()),
				);
				return ExitCode::FAILURE;
			}
		}
	}

	exit_code
}

/// A policy with its rules grouped together
struct PolicyGroup<'a> {
	name: String,
	rules: Vec<&'a RuleExecution>,
}

/// Group trace entries by policy, preserving order
fn group_trace_by_policy(trace: &[RuleExecution]) -> Vec<PolicyGroup<'_>> {
	let mut groups: Vec<PolicyGroup<'_>> = Vec::new();
	let mut current_policy: Option<&str> = None;

	for exec in trace {
		let policy_name = if exec.policy_name.is_empty() {
			"(no policy)"
		} else {
			&exec.policy_name
		};

		if current_policy != Some(policy_name) {
			groups.push(PolicyGroup {
				name: policy_name.to_string(),
				rules: Vec::new(),
			});
			current_policy = Some(policy_name);
		}

		if let Some(group) = groups.last_mut() {
			group.rules.push(exec);
		}
	}

	groups
}

/// Find failures that belong to a specific rule
fn find_failures_for_rule<'a>(
	rule_name: &str,
	failures: &'a [AssertFailure],
) -> Vec<&'a AssertFailure> {
	failures
		.iter()
		.filter(|f| f.assertion.starts_with(rule_name))
		.collect()
}

/// Determine the status for a rule execution
fn rule_status(exec: &RuleExecution) -> Status {
	if exec.passed {
		Status::Pass
	} else {
		match exec.modal {
			Modal::Must => Status::Fail,
			Modal::Should => Status::Warn,
			Modal::May => Status::Info,
		}
	}
}

/// Determine if a policy has any failures (must failures = Fail, should failures = Warn)
fn policy_status(rules: &[&RuleExecution]) -> Status {
	let has_must_failure = rules.iter().any(|r| !r.passed && r.modal == Modal::Must);
	let has_should_failure = rules.iter().any(|r| !r.passed && r.modal == Modal::Should);

	if has_must_failure {
		Status::Fail
	} else if has_should_failure {
		Status::Warn
	} else {
		Status::Pass
	}
}

fn print_results(
	out: &Reporter,
	profile: &str,
	analysis: &AnalysisResult,
	verbose: bool,
) -> ExitCode {
	let policy_groups = group_trace_by_policy(&analysis.trace);

	// Calculate profile-level counts
	let total_policies = policy_groups.len();
	let passed_policies = policy_groups
		.iter()
		.filter(|g| policy_status(&g.rules) == Status::Pass)
		.count();

	let (profile_status, exit_code) = match analysis.outcome {
		PolicyOutcome::Pass => (Status::Pass, ExitCode::SUCCESS),
		PolicyOutcome::Degraded => (Status::Warn, ExitCode::SUCCESS),
		PolicyOutcome::Fail => (Status::Fail, ExitCode::FAILURE),
	};

	// Simple success case without verbose
	if !verbose && analysis.failures.is_empty() {
		out.tree(
			"",
			true,
			profile_status,
			&format!("Profile: {profile} [{passed_policies}/{total_policies}]"),
			None,
		);
		return exit_code;
	}

	// Need to show the tree
	// Filter to only policies/rules with failures if not verbose
	let policies_to_show: Vec<&PolicyGroup<'_>> = if verbose {
		policy_groups.iter().collect()
	} else {
		policy_groups
			.iter()
			.filter(|g| policy_status(&g.rules) != Status::Pass)
			.collect()
	};

	let has_children = !policies_to_show.is_empty();

	// Print profile header
	out.tree(
		"",
		true,
		profile_status,
		&format!("Profile: {profile} [{passed_policies}/{total_policies}]"),
		None,
	);

	if !has_children {
		return exit_code;
	}

	let profile_prefix = Reporter::child_prefix("", true);

	// Print each policy
	let total_policies_shown = policies_to_show.len();
	for (policy_idx, policy_group) in policies_to_show.iter().enumerate() {
		let is_last_policy = policy_idx + 1 == total_policies_shown;

		// Calculate rule counts for this policy
		let total_rules = policy_group.rules.len();
		let passed_rules = policy_group.rules.iter().filter(|r| r.passed).count();
		let status = policy_status(&policy_group.rules);

		out.tree(
			&profile_prefix,
			is_last_policy,
			status,
			&format!(
				"Policy: {} [{}/{}]",
				policy_group.name, passed_rules, total_rules
			),
			None,
		);

		let policy_prefix = Reporter::child_prefix(&profile_prefix, is_last_policy);

		// Filter rules to show
		let rules_to_show: Vec<&RuleExecution> = if verbose {
			policy_group.rules.clone()
		} else {
			policy_group
				.rules
				.iter()
				.filter(|r| !r.passed)
				.copied()
				.collect()
		};

		// Print each rule
		let total_rules_shown = rules_to_show.len();
		for (rule_idx, exec) in rules_to_show.iter().enumerate() {
			let is_last_rule = rule_idx + 1 == total_rules_shown;
			let status = rule_status(exec);

			let modal_str = match exec.modal {
				Modal::Must => "must",
				Modal::Should => "should",
				Modal::May => "may",
			};

			// Find failures for this rule
			let rule_failures = find_failures_for_rule(&exec.rule_name, &analysis.failures);
			let has_findings = !rule_failures.is_empty();

			let label = if has_findings {
				format!(
					"{} {} ({} finding{})",
					modal_str,
					exec.rule_name,
					rule_failures.len(),
					if rule_failures.len() == 1 { "" } else { "s" }
				)
			} else {
				format!("{} {}", modal_str, exec.rule_name)
			};

			out.tree(&policy_prefix, is_last_rule, status, &label, None);

			// Print failures nested under this rule
			if has_findings {
				let rule_prefix = Reporter::child_prefix(&policy_prefix, is_last_rule);
				print_failures_for_rule(out, &rule_prefix, &rule_failures, analysis);
			}
		}
	}

	exit_code
}

fn print_failures_for_rule(
	out: &Reporter,
	prefix: &str,
	failures: &[&AssertFailure],
	analysis: &AnalysisResult,
) {
	let total = failures.len();
	for (idx, failure) in failures.iter().enumerate() {
		let is_last = idx + 1 == total;
		let status = Status::from(failure.severity);

		// Build the label - use subject name if available
		let label = if let Some(subject) = failure.subject {
			analysis.model.qualified_name(subject)
		} else {
			"(unknown subject)".to_string()
		};

		// Build detail parts
		let mut detail_parts = Vec::new();

		if let Some(location) = analysis.resolve_failure_location(failure) {
			let display_path = location
				.uri
				.to_file_path()
				.ok()
				.and_then(|p| {
					std::env::current_dir()
						.ok()
						.and_then(|cwd| p.strip_prefix(&cwd).ok().map(|rel| rel.to_path_buf()))
						.or(Some(p))
				})
				.map(|p| p.display().to_string())
				.unwrap_or_else(|| location.uri.to_string());

			detail_parts.push(format!(
				"Location: {}: line {}",
				display_path,
				location.range.start.line + 1,
			));
		}

		if let Some(area_id) = failure.area {
			let area = analysis.model.qualified_name(area_id);
			detail_parts.push(format!("Area: {area}"));
		}

		if let Some(msg) = &failure.message {
			detail_parts.push(format!("Message: {msg}"));
		}

		let detail = if detail_parts.is_empty() {
			None
		} else {
			Some(detail_parts.join("\n"))
		};

		out.tree(prefix, is_last, status, &label, detail.as_deref());
	}
}

fn classify_graph_node(model: &Model, node: EntityId) -> GraphNodeKind {
	let type_names: Vec<String> = model
		.types(node)
		.iter()
		.map(|&t| model.qualified_name(t))
		.collect();

	let has = |name: &str| type_names.iter().any(|t| t == name);
	let has_prefix = |prefix: &str| type_names.iter().any(|t| t.starts_with(prefix));

	if has("core:Run") {
		return GraphNodeKind::Run;
	}
	if has("core:Store") {
		return GraphNodeKind::Store;
	}
	if has("core:Move") {
		return GraphNodeKind::Move;
	}
	if has("core:Workload") {
		return GraphNodeKind::Workload;
	}
	if has("core:Evidence") {
		return GraphNodeKind::Evidence;
	}

	if has("aws:cfn:Template") {
		return GraphNodeKind::Template;
	}
	if has("aws:cfn:Resource") {
		return GraphNodeKind::Resource;
	}
	if has("aws:cfn:Parameter") {
		return GraphNodeKind::Parameter;
	}
	if has("aws:cfn:PseudoParameter") {
		return GraphNodeKind::PseudoParameter;
	}
	if has("aws:cfn:Output") {
		return GraphNodeKind::Output;
	}
	if has("aws:cfn:Ref") {
		return GraphNodeKind::Ref;
	}
	if has("aws:cfn:GetAtt") {
		return GraphNodeKind::GetAtt;
	}
	if has("aws:cfn:Sub") {
		return GraphNodeKind::Sub;
	}
	if has_prefix("aws:cfn:") {
		return GraphNodeKind::Intrinsic;
	}

	if model.entity(node).localname.is_none() {
		GraphNodeKind::Blank
	} else {
		GraphNodeKind::Entity
	}
}

fn graph_kind_for_link_ref(model: &Model, child_id: EntityId) -> GraphNodeKind {
	classify_graph_node(model, child_id)
}

fn print_model_graph(out: &Reporter, model: &Model) {
	if let Some(root) = model.root() {
		let mut visited = HashSet::new();
		render_model_node(out, model, "", true, root, &mut visited);
	} else {
		out.graph_tree("", true, GraphNodeKind::Entity, "(no root set)", None);
	}
}

fn render_model_node(
	out: &Reporter,
	model: &Model,
	prefix: &str,
	is_last: bool,
	node: EntityId,
	visited: &mut HashSet<EntityId>,
) {
	let kind = classify_graph_node(model, node);

	let name = model.qualified_name(node);
	let types: Vec<String> = model
		.types(node)
		.iter()
		.map(|&t| model.qualified_name(t))
		.collect();

	let mut attrs = Vec::new();
	let mut linked_entities = Vec::new();
	let mut children = Vec::new();

	for stmt_id in model.outgoing(node) {
		let stmt = model.statement(stmt_id);
		let pred_name = model.qualified_name(stmt.predicate);

		if pred_name == "wa2:type" {
			continue;
		}

		match &stmt.object {
			Value::Literal(lit) => {
				attrs.push(format!("{pred_name}=\"{lit}\""));
			}
			Value::Number(n) => {
				attrs.push(format!("{pred_name}={n}"));
			}
			Value::Entity(child_id) => {
				if pred_name == "wa2:contains" {
					children.push(*child_id);
				} else {
					linked_entities.push((pred_name, *child_id));
				}
			}
		}
	}

	let mut label = name;
	if !types.is_empty() {
		label.push_str(" : ");
		label.push_str(&types.join(", "));
	}

	let detail = if attrs.is_empty() {
		None
	} else {
		Some(attrs.join("\n"))
	};

	if visited.contains(&node) {
		let label = format!("{label} (→)");
		out.graph_tree(prefix, is_last, kind, &label, detail.as_deref());
		return;
	}

	visited.insert(node);
	out.graph_tree(prefix, is_last, kind, &label, detail.as_deref());

	let child_prefix = Reporter::child_prefix(prefix, is_last);

	let mut items: Vec<GraphItem> = Vec::new();

	for (pred_name, child_id) in linked_entities {
		let child_entity = model.entity(child_id);
		let is_blank = child_entity.localname.is_none();

		if is_blank && !visited.contains(&child_id) {
			items.push(GraphItem::ExpandLinkedBlank {
				pred_name,
				child_id,
			});
		} else {
			items.push(GraphItem::LinkRef {
				pred_name,
				child_id,
			});
		}
	}

	for child in children {
		items.push(GraphItem::ContainsChild { child_id: child });
	}

	let total = items.len();
	for (idx, item) in items.into_iter().enumerate() {
		let item_is_last = idx + 1 == total;

		match item {
			GraphItem::ExpandLinkedBlank {
				pred_name,
				child_id,
			} => {
				out.graph_tree(
					&child_prefix,
					item_is_last,
					GraphNodeKind::Entity,
					&format!("-{pred_name}-"),
					None,
				);

				let nested_prefix = Reporter::child_prefix(&child_prefix, item_is_last);
				render_model_node(out, model, &nested_prefix, true, child_id, visited);
			}
			GraphItem::LinkRef {
				pred_name,
				child_id,
			} => {
				let child_kind = graph_kind_for_link_ref(model, child_id);

				let child_name = model.qualified_name(child_id);
				let child_types: Vec<String> = model
					.types(child_id)
					.iter()
					.map(|&t| model.qualified_name(t))
					.collect();

				let mut label = format!("-{pred_name}- {child_name}");
				if !child_types.is_empty() {
					label.push_str(" : ");
					label.push_str(&child_types.join(", "));
				}
				label.push_str(" (→)");

				out.graph_tree(&child_prefix, item_is_last, child_kind, &label, None);
			}
			GraphItem::ContainsChild { child_id } => {
				render_model_node(out, model, &child_prefix, item_is_last, child_id, visited);
			}
		}
	}
}

enum GraphItem {
	ExpandLinkedBlank {
		pred_name: String,
		child_id: EntityId,
	},
	LinkRef {
		pred_name: String,
		child_id: EntityId,
	},
	ContainsChild {
		child_id: EntityId,
	},
}
