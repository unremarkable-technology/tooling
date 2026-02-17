use tower_lsp::lsp_types::Diagnostic;
use url::Url;

use crate::intents::{guidance::{Guide, guidance}, model::Model, vendor::{DocumentFormat, Method, Vendor, get_projector}};

/// Result of analyzing a document
pub struct AnalysisResult {
	pub model: Model,
	pub guides: Vec<Guide>,
}

/// Kernel - the WA2 analysis engine
///
/// Loads core types (eventually from core.wa2 DSL) and orchestrates
/// projection and guidance generation.
pub struct Kernel;

impl Default for Kernel {
	fn default() -> Self {
		Self::new()
	}
}

impl Kernel {
	pub fn new() -> Self {
		Self
	}

	/// Analyze a document, returning model and guidance
	pub fn analyse(
		&self,
		text: &str,
		uri: &Url,
		format: DocumentFormat,
		vendor: Vendor,
		method: Method,
	) -> Result<AnalysisResult, Vec<Diagnostic>> {
		// Get projector for this vendor/method
		let projector = get_projector(vendor, method);

		// Project text into model (projector loads core types internally for now)
		let result = projector.project(text, uri, format)?;
		let model = result.model;

		// Run guidance
		let guides = guidance(&model);

		Ok(AnalysisResult { model, guides })
	}
}
