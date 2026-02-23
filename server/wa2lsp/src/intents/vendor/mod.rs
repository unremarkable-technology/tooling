pub mod aws;

use tower_lsp::lsp_types::Diagnostic;
use url::Url;

use crate::intents::model::Model;

/// Document format for IaC files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
	Yaml,
	Json,
}

impl DocumentFormat {
	pub fn from_language_id_or_path(language_id: Option<&str>, uri: &Url) -> Self {
		match language_id {
			Some("cloudformation-json") => DocumentFormat::Json,
			Some("cloudformation-yaml") => DocumentFormat::Yaml,
			_ => {
				// Fallback to extension
				let path = uri.path();
				if path.ends_with(".json") {
					DocumentFormat::Json
				} else {
					DocumentFormat::Yaml
				}
			}
		}
	}
}

// vendor/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
	Aws,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
	CloudFormation,
}

/// Result of projecting IaC into a Model
pub struct ProjectionResult {
    pub model: Model,
    pub diagnostics: Vec<Diagnostic>,
}

/// Trait for vendor-specific projectors
pub trait VendorProjector {
    /// Project infrastructure-as-code text into an existing Model
    fn project_into(
        &self,
        model: &mut Model,
        text: &str,
        uri: &Url,
        format: DocumentFormat,
    ) -> Result<(), Vec<Diagnostic>>;
}

/// Get the appropriate projector for a vendor/method combination
pub fn get_projector(vendor: Vendor, method: Method) -> Box<dyn VendorProjector> {
	match (vendor, method) {
		(Vendor::Aws, Method::CloudFormation) => Box::new(aws::AwsCfnProjector::new()),
	}
}
