//! AWS vendor projector

mod cfn_projector;
mod derivation;
mod tests;

use tower_lsp::lsp_types::{Diagnostic, Url};

use crate::intents::model::Model;
use crate::intents::vendor::{ProjectionResult, VendorProjector};
use crate::spec::cfn_ir::types::CfnTemplate;

/// AWS CloudFormation projector
pub struct AwsCfnProjector;

impl AwsCfnProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AwsCfnProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl VendorProjector for AwsCfnProjector {
    fn project(&self, text: &str, uri: &Url) -> Result<ProjectionResult, Vec<Diagnostic>> {
        // Parse CFN template
        let template = CfnTemplate::from_yaml(text, uri).map_err(|diags| diags)?;

        // Project into model
        let model = project_template(&template)?;

        Ok(ProjectionResult {
            model,
            diagnostics: vec![],
        })
    }
}

/// Project a parsed CFN template into a Model
pub fn project_template(template: &CfnTemplate) -> Result<Model, Vec<Diagnostic>> {
    let mut model = Model::bootstrap();
    model.ensure_entity("aws");
    cfn_projector::ensure_cfn_types(&mut model).map_err(model_error_to_diags)?;

    let root = model.ensure_entity("deployment");
    model
        .apply_to(root, "wa2:type", "wa2:Deployment")
        .map_err(model_error_to_diags)?;
    model.set_root(root);

    // Create template entity and link to deployment
    let template_entity = model.blank();
    model
        .apply_to(template_entity, "wa2:type", "cfn:Template")
        .map_err(model_error_to_diags)?;
    model
        .apply_entity(root, "wa2:source", template_entity)
        .map_err(model_error_to_diags)?;

    cfn_projector::project_outputs(&mut model, template_entity, &template.outputs)
        .map_err(model_error_to_diags)?;
    cfn_projector::project_parameters(&mut model, template_entity, &template.parameters)
        .map_err(model_error_to_diags)?;
    cfn_projector::project_pseudo_parameters(&mut model, template_entity)
        .map_err(model_error_to_diags)?;

    let mut entities = Vec::new();

    for resource in template.resources.values() {
        let entity = model.ensure_entity(&resource.logical_id);
        model
            .apply_to(
                entity,
                "aws:type",
                &format!("\"{}\"", resource.resource_type),
            )
            .map_err(model_error_to_diags)?;
        model
            .apply_to(
                entity,
                "aws:logicalId",
                &format!("\"{}\"", resource.logical_id),
            )
            .map_err(model_error_to_diags)?;
        model
            .apply_entity(root, "wa2:contains", entity)
            .map_err(model_error_to_diags)?;

        // Track source location
        model.set_range(entity, resource.logical_id_range);

        for (prop_name, (prop_value, _)) in &resource.properties {
            cfn_projector::project_value(&mut model, entity, prop_name, prop_value)
                .map_err(model_error_to_diags)?;
        }

        entities.push(entity);
    }

    // Derive phase - AWS-specific type classification and evidence
    for entity in entities {
        derivation::derive_wa2_type(&mut model, entity).map_err(model_error_to_diags)?;
        derivation::derive_evidence(&mut model, entity).map_err(model_error_to_diags)?;
    }

    Ok(model)
}

fn model_error_to_diags(err: crate::intents::model::ModelError) -> Vec<Diagnostic> {
    vec![Diagnostic {
        range: tower_lsp::lsp_types::Range::default(),
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        message: format!("Model error: {:?}", err),
        ..Default::default()
    }]
}