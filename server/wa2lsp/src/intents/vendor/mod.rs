pub mod aws;

use tower_lsp::lsp_types::Diagnostic;
use url::Url;

use crate::intents::model::Model;

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
    /// Project infrastructure-as-code text into a Model
    fn project(&self, text: &str, uri: &Url) -> Result<ProjectionResult, Vec<Diagnostic>>;
}

/// Get the appropriate projector for a vendor/method combination
pub fn get_projector(vendor: Vendor, method: Method) -> Box<dyn VendorProjector> {
    match (vendor, method) {
        (Vendor::Aws, Method::CloudFormation) => Box::new(aws::AwsCfnProjector::new()),
    }
}