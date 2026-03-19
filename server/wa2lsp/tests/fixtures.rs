// tests/fixtures.rs
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};
use url::Url;
use wa2lsp::iaac::cloudformation::{
	cfn_ir::types::CfnTemplate, spec_cache::SpecCacheManager, spec_source::SpecSource,
	spec_store::SpecStore,
};

#[cfg(test)]
static CACHED_SPEC: OnceLock<Arc<SpecStore>> = OnceLock::new();

/// Loads the real CFN spec (blocking). Cached after first call.
/// Panics if spec can't be loaded.
pub fn use_latest_cfn_spec() -> Arc<SpecStore> {
	CACHED_SPEC
		.get_or_init(|| {
			let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
			rt.block_on(async {
				let source = SpecSource::for_region_schemas("us-east-1").expect("spec source");
				let cache_dir = dirs::cache_dir()
					.unwrap_or_else(std::env::temp_dir)
					.join("wa2")
					.join("cfn-spec");
				let manager = SpecCacheManager::new(source, &cache_dir);
				manager.load_registry_spec_store().await.expect("load spec")
			})
		})
		.clone()
}

#[allow(unused)]
#[allow(clippy::large_enum_variant)]
enum ParseResult {
	ParseError(Vec<Diagnostic>),
	Parsed {
		template: CfnTemplate,
		diags: Vec<Diagnostic>,
	},
}

fn parse_and_validate(path: &Path) -> datatest_stable::Result<ParseResult> {
	let content = std::fs::read_to_string(path)?;
	let abs_path = path.canonicalize()?;
	let uri =
		Url::from_file_path(&abs_path).map_err(|_| format!("Invalid path: {}", path.display()))?;

	match CfnTemplate::from_yaml(&content, &uri) {
		Err(parse_diags) => Ok(ParseResult::ParseError(parse_diags)),
		Ok(template) => {
			let spec = use_latest_cfn_spec();
			let diags = template.validate_against_spec(&spec);
			Ok(ParseResult::Parsed { template, diags })
		}
	}
}

fn test_good_template(path: &Path) -> datatest_stable::Result<()> {
	match parse_and_validate(path)? {
		ParseResult::ParseError(diags) => {
			Err(format!("Unexpected parse failure: {:?}", diags).into())
		}
		ParseResult::Parsed { diags, .. } => {
			// Allow warnings, but no errors
			let errors: Vec<_> = diags
				.iter()
				.filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
				.collect();

			if errors.is_empty() {
				Ok(())
			} else {
				Err(format!("Unexpected diagnostics: {:?}", diags).into())
			}
		}
	}
}

// fn test_bad_template(path: &Path) -> datatest_stable::Result<()> {
// 	match parse_and_validate(path)? {
// 		ParseResult::ParseError(_) => Ok(()), // Parse error = expected
// 		ParseResult::Parsed { diags, .. } => {
// 			if diags.is_empty() {
// 				Err("Expected diagnostics but got none".into())
// 			} else {
// 				Ok(())
// 			}
// 		}
// 	}
// }

datatest_stable::harness! {
	{ test = test_good_template, root = "tests/fixtures/cfn-lint/good", pattern = r"\.yaml$|\.json$" },
	//{ test = test_bad_template, root = "tests/fixtures/cfn-lint/bad", pattern = r"\.yaml$|\.json$" },
}
