// check.rs
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
use wa2lsp::intents::kernel::{AssertSeverity, Kernel};
use wa2lsp::intents::vendor::{DocumentFormat, Method, Vendor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
	Pass,
	Warn,
	Fail,
   
   #[allow(unused)]
	Blocked,
}

impl From<AssertSeverity> for Status {
	fn from(s: AssertSeverity) -> Self {
		match s {
			AssertSeverity::Error => Status::Fail,
			AssertSeverity::Warning => Status::Warn,
			AssertSeverity::Info => Status::Pass, // or a separate Info if you want later
		}
	}
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

	fn setup(&self, status: Status, label: &str, detail: Option<&str>) {
		self.node("", true, status, label, detail);
	}

	fn tree(&self, prefix: &str, is_last: bool, status: Status, label: &str, detail: Option<&str>) {
		self.node(prefix, is_last, status, label, detail);
	}

	fn node(&self, prefix: &str, is_last: bool, status: Status, label: &str, detail: Option<&str>) {
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

		let marker = self.marker(status);
		println!("{first_prefix}{marker} {label}");

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
		for para in text.split("\n\n") {
			let trimmed = para.trim();
			if trimmed.is_empty() {
				continue;
			}

			let available = self.width.saturating_sub(UnicodeWidthStr::width(prefix));

			let options = Options::new(available.max(20));
			for line in wrap(trimmed, options) {
				println!("{prefix}{}", line);
			}
			println!("{prefix}");
		}
	}

	fn marker(&self, status: Status) -> String {
		let s = match status {
			Status::Pass => "✓",
			Status::Warn => "!",
			Status::Fail => "✗",
			Status::Blocked => "•",
		};

		if !self.interactive {
			return s.to_string();
		}

		match status {
			Status::Pass => s.green().to_string(),
			Status::Warn => s.yellow().to_string(),
			Status::Fail => s.red().to_string(),
			Status::Blocked => s.dimmed().to_string(),
		}
	}
}

pub async fn run(profile: &str, target: &Path, entry: Option<&Path>) -> ExitCode {
	let out = Reporter::new();
	out.section("SETUP");

	if !target.exists() {
		out.setup(
			Status::Fail,
			&format!("Read target {}", target.display()),
			Some("Target file not found."),
		);
		return ExitCode::FAILURE;
	}

	let target_text = match fs::read_to_string(target) {
		Ok(t) => {
			out.setup(
				Status::Pass,
				&format!("Read target {}", target.display()),
				None,
			);
			t
		}
		Err(e) => {
			out.setup(
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

	let template = match format {
		DocumentFormat::Json => CfnTemplate::from_json(&target_text, &target_uri),
		DocumentFormat::Yaml => CfnTemplate::from_yaml(&target_text, &target_uri),
	};

	let template = match template {
		Ok(t) => {
			out.setup(Status::Pass, "Parse CloudFormation template", None);
			t
		}
		Err(diags) => {
			let detail = diags
				.into_iter()
				.map(|d| d.message)
				.collect::<Vec<_>>()
				.join("\n\n");
			out.setup(Status::Fail, "Parse CloudFormation template", Some(&detail));
			return ExitCode::FAILURE;
		}
	};

	match load_default_spec_store().await {
		Ok(spec_store) => {
			let spec_diags = template.validate_against_spec(&spec_store);
			if spec_diags.is_empty() {
				out.setup(
					Status::Pass,
					"Validate CloudFormation against specification",
					None,
				);
			} else {
				let detail = spec_diags
					.into_iter()
					.map(|d| d.message)
					.collect::<Vec<_>>()
					.join("\n\n");
				out.setup(
					Status::Fail,
					"Validate CloudFormation against specification",
					Some(&detail),
				);
				return ExitCode::FAILURE;
			}
		}
		Err(e) => {
			out.setup(
				Status::Warn,
				"Load CloudFormation spec",
				Some(&format!(
					"Could not load CloudFormation specification: {e}\n\nSkipping specification validation."
				)),
			);
		}
	}

	let skip_quickstart = entry.is_some();
	let mut kernel = Kernel::new(skip_quickstart);
	out.setup(Status::Pass, "Initialise kernel", None);

	if let Some(entry_path) = entry {
		if !entry_path.exists() {
			out.setup(
				Status::Fail,
				&format!("Read intent entry {}", entry_path.display()),
				Some("Entry file not found."),
			);
			return ExitCode::FAILURE;
		}

		match kernel.load_intent(entry_path) {
			Ok(_) => {
				out.setup(
					Status::Pass,
					&format!("Parse intent entry {}", entry_path.display()),
					None,
				);
			}
			Err(e) => {
				out.setup(
					Status::Fail,
					&format!("Parse intent entry {}", entry_path.display()),
					Some(&e.to_string()),
				);
				return ExitCode::FAILURE;
			}
		}
	}

	if let Err(e) = kernel.set_profile(profile.to_string()) {
		out.setup(
			Status::Fail,
			&format!("Select profile {profile}"),
			Some(&e.to_string()),
		);
		return ExitCode::FAILURE;
	}
	out.setup(Status::Pass, &format!("Select profile {profile}"), None);

	let result = kernel.analyse(
		&target_text,
		&target_uri,
		format,
		Vendor::Aws,
		Method::CloudFormation,
	);

	let analysis = match result {
		Ok(analysis) => {
			out.setup(Status::Pass, "Run analysis", None);
			analysis
		}
		Err(diags) => {
			let detail = diags
				.into_iter()
				.map(|d| d.message)
				.collect::<Vec<_>>()
				.join("\n\n");
			out.setup(Status::Fail, "Run analysis", Some(&detail));
			return ExitCode::FAILURE;
		}
	};

	out.section("RESULTS");

	if analysis.failures.is_empty() {
		out.tree(
			"",
			true,
			Status::Pass,
			&format!("Profile: {profile}"),
			Some("Target satisfies the selected intent profile."),
		);
		return ExitCode::SUCCESS;
	}

	out.tree("", true, Status::Fail, &format!("Profile: {profile}"), None);
	let profile_prefix = Reporter::child_prefix("", true);

	for (idx, failure) in analysis.failures.iter().enumerate() {
		let is_last = idx + 1 == analysis.failures.len();

		let mut detail_parts = Vec::new();

		if let Some(subject) = failure.subject {
			let name = analysis.model.qualified_name(subject);
			detail_parts.push(format!("Subject: {name}"));
		}

		if let Some(location) = analysis.resolve_failure_location(failure) {
         // make location url into file path - relative to current directory
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

		if let Some(msg) = &failure.message {
			detail_parts.push(format!("Message: {msg}"));
		}

		let detail = if detail_parts.is_empty() {
			None
		} else {
			Some(detail_parts.join("\n"))
		};

		let label = format!("{} ({})", failure.assertion, failure.severity.label());
		out.tree(
			&profile_prefix,
			is_last,
			Status::from(failure.severity),
			&label,
			detail.as_deref(),
		);
	}

	ExitCode::FAILURE
}
