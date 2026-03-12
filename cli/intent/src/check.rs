use std::fs;
use std::path::Path;
use std::process;

use url::Url;
use wa2lsp::iaac::cloudformation::cfn_ir::types::CfnTemplate;
use wa2lsp::iaac::cloudformation::spec_cache::load_default_spec_store;
use wa2lsp::intents::kernel::Kernel;
use wa2lsp::intents::vendor::{DocumentFormat, Method, Vendor};

pub async fn run(profile: &str, target: &Path, entry: Option<&Path>) {
	// Validate target exists
	if !target.exists() {
		eprintln!("Error: target file not found: {}", target.display());
		process::exit(1);
	}

	// Load target text
	let target_text = match fs::read_to_string(target) {
		Ok(t) => t,
		Err(e) => {
			eprintln!("Error: Could not read target file: {}", e);
			process::exit(1);
		}
	};

	let target_uri =
		Url::from_file_path(target.canonicalize().unwrap_or_else(|_| target.to_path_buf()))
			.unwrap_or_else(|_| Url::parse("file:///unknown").unwrap());

	// Determine format from path
	let format = DocumentFormat::from_language_id_or_path(None, &target_uri);

	// Fast path: parse target
	let template = match format {
		DocumentFormat::Json => CfnTemplate::from_json(&target_text, &target_uri),
		DocumentFormat::Yaml => CfnTemplate::from_yaml(&target_text, &target_uri),
	};

	let template = match template {
		Ok(t) => t,
		Err(diags) => {
			eprintln!("target file {}: syntax errors", target.display());
			for d in diags {
				eprintln!("  - {}", d.message);
			}
			process::exit(1);
		}
	};

	// Validate against CloudFormation spec
	match load_default_spec_store().await {
		Ok(spec_store) => {
			let spec_diags = template.validate_against_spec(&spec_store);
			if !spec_diags.is_empty() {
				eprintln!("target {}: CloudFormation specification errors", target.display());
				for d in spec_diags {
					eprintln!("  - {}", d.message);
				}
				process::exit(1);
			}
		}
		Err(e) => {
			eprintln!("Warning: Could not load CloudFormation spec: {}", e);
			eprintln!("         Skipping spec validation.");
		}
	}

	println!(
		"Target file {}: parsed and validated successfully.",
		target.display()
	);

	// Load kernel (uses wa2.toml if present, else embedded)
   let skip_quickstart = entry.is_some();
   let mut kernel = Kernel::new(skip_quickstart);

	// Layer intent file if provided
	if let Some(entry_path) = entry {
		if !entry_path.exists() {
			eprintln!("Entry file {}: not found", entry_path.display());
			process::exit(1);
		}

		if let Err(e) = kernel.load_intent(entry_path) {
			eprintln!("Entry file {}: intent error", entry_path.display());
			eprintln!("  {}", e);
			process::exit(1);
		}

		println!(
			"Entry file {}: intent parsed and validated successfully.",
			entry_path.display()
		);
	}

	// Override profile selection
	if let Err(e) = kernel.set_profile(profile.to_string()) {
		eprintln!("Profile error: {}", e);
		process::exit(1);
	}

	// Run analysis
	let result = kernel.analyse(
		&target_text,
		&target_uri,
		format,
		Vendor::Aws,
		Method::CloudFormation,
	);

	match result {
		Ok(analysis) => {
			if analysis.failures.is_empty() {
				println!("\nSuccess: target satisfies intent of profile {}.", profile);
			} else {
				println!(
					"\nFailed: target does not satisfy intent of profile {}",
					profile
				);
				println!("\nCauses:");
				for failure in &analysis.failures {
					println!("✖ {}", failure.assertion);
					if let Some(subject) = failure.subject {
						let name = analysis.model.qualified_name(subject);
						println!("  - Subject: {}", name);
					}
					if let Some(ref msg) = failure.message {
						println!("  - Message: {}", msg);
					}
				}
				process::exit(1);
			}
		}
		Err(diags) => {
			eprintln!("target {}: analysis errors", target.display());
			for d in diags {
				eprintln!("  - {}", d.message);
			}
			process::exit(1);
		}
	}
}
