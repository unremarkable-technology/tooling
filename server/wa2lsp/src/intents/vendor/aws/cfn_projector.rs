//! CloudFormation IR to Model projection

use std::collections::HashMap;

use tower_lsp::lsp_types::{Position, Range};

use crate::intents::model::{EntityId, Model, ModelError};
use crate::spec::cfn_ir::types::{CFN_INTRINSICS, CfnOutput, CfnParameter, CfnValue};

/// Extract Sub template variables with their positions within the template string
/// Returns Vec<(variable_name, start_offset, end_offset)> where offsets are byte positions
pub fn extract_sub_variable_positions(template: &str) -> Vec<(String, usize, usize)> {
    let mut results = Vec::new();
    let mut i = 0;
    let bytes = template.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i;
            let var_start = i + 2;
            // Find closing brace
            if let Some(end_offset) = template[var_start..].find('}') {
                let var_end = var_start + end_offset;
                let var_name = &template[var_start..var_end];
                // Strip .Attribute for GetAtt-style references
                let base_name = var_name.split('.').next().unwrap_or(var_name);
                results.push((base_name.to_string(), start, var_end + 1)); // +1 to include }
                i = var_end + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    results
}

const CFN_PREDICATES: &[&str] = &[
    "target",
    "attribute",
    "template",
    "parameters",
    "pseudoParameters",
    "type",
    "default",
    "description",
    "delimiter",
    "condition",
    "conditionName",
];

/// Ensure CFN-specific types and predicates exist in the model
pub fn ensure_cfn_types(model: &mut Model) -> Result<(), ModelError> {
    model.ensure_entity("cfn");
    model.ensure_entity("cfn:Output");
    model.ensure_entity("cfn:outputs");
    model.ensure_entity("cfn:value");
    model.ensure_entity("cfn:exportName");
    model.ensure_entity("cfn:SubVarRef");
    model.ensure_entity("cfn:varRef");

    model.apply("cfn:Template", "wa2:type", "wa2:Type")?;
    model.apply("cfn:Parameter", "wa2:type", "wa2:Type")?;
    model.apply("cfn:PseudoParameter", "wa2:type", "wa2:Type")?;
    for name in CFN_INTRINSICS {
        model.apply(&format!("cfn:{}", name), "wa2:type", "wa2:Type")?;
    }

    for name in CFN_PREDICATES {
        model.apply(&format!("cfn:{}", name), "wa2:type", "wa2:Predicate")?;
    }

    model.apply("wa2:source", "wa2:type", "wa2:Predicate")?;

    Ok(())
}

/// Create typed intrinsic node and link to parent
fn project_intrinsic(
    model: &mut Model,
    parent: EntityId,
    pred: &str,
    value: &CfnValue,
) -> Result<EntityId, ModelError> {
    let name = value.intrinsic_name().expect("must be intrinsic");
    let node = model.blank();
    model.apply_to(node, "wa2:type", &format!("cfn:{}", name))?;
    model.set_range(node, value.range());
    model.apply_entity(parent, pred, node)?;
    Ok(node)
}

pub fn project_value(
    model: &mut Model,
    parent: EntityId,
    property_name: &str,
    value: &CfnValue,
) -> Result<(), ModelError> {
    let pred = format!("aws:{}", property_name);

    match value {
        CfnValue::String(s, _) => {
            model.apply_to(parent, &pred, &format!("\"{}\"", s))?;
        }
        CfnValue::Number(n, _) => {
            model.apply_to(parent, &pred, &format!("\"{}\"", n))?;
        }
        CfnValue::Bool(b, _) => {
            model.apply_to(parent, &pred, &format!("\"{}\"", b))?;
        }
        CfnValue::Null(_) => {
            // Skip nulls
        }
        CfnValue::Array(items, _) => {
            let container = model.blank();
            model.apply_entity(parent, &pred, container)?;
            for item in items {
                project_array_item(model, container, item)?;
            }
        }
        CfnValue::Object(map, _) => {
            let node = model.blank();
            model.apply_entity(parent, &pred, node)?;
            for (key, (val, _)) in map {
                project_value(model, node, key, val)?;
            }
        }

        // Intrinsic functions as typed nodes
        CfnValue::Ref { target, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            let target_entity = model.ensure_entity(target);
            model.apply_entity(node, "cfn:target", target_entity)?;
        }

        CfnValue::GetAtt {
            target, attribute, ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            let target_entity = model.ensure_entity(target);
            model.apply_entity(node, "cfn:target", target_entity)?;
            model.apply_to(node, "cfn:attribute", &format!("\"{}\"", attribute))?;
        }

        CfnValue::Sub {
            template,
            variables,
            range,
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;

            // Get template string content
            if let CfnValue::String(template_str, _) = template.as_ref() {
                model.apply_to(node, "cfn:template", template_str)?;

                // Create per-variable reference nodes with source ranges
                let var_positions = extract_sub_variable_positions(template_str);
                let cfn_sub_var_ref = model
                    .resolve("cfn:SubVarRef")
                    .expect("cfn:SubVarRef must exist");

                for (var_name, start_offset, end_offset) in var_positions {
                    // Use the Sub's range as the base
                    let var_range = Range {
                        start: Position {
                            line: range.start.line,
                            character: range.start.character + 1 + start_offset as u32,
                        },
                        end: Position {
                            line: range.start.line,
                            character: range.start.character + 1 + end_offset as u32,
                        },
                    };

                    // Create reference node
                    let ref_node = model.blank();
                    model.apply_entity(ref_node, "wa2:type", cfn_sub_var_ref)?;
                    model.set_range(ref_node, var_range);
                    model.apply_entity(node, "cfn:varRef", ref_node)?;

                    // Link to target
                    if let Some(target) = model.resolve(&var_name) {
                        model.apply_entity(ref_node, "cfn:target", target)?;
                    }
                }
            }

            // Handle explicit variables map if present
            if let Some(vars) = variables {
                for (var_name, var_value) in vars {
                    project_value(model, node, &format!("cfn:var:{}", var_name), var_value)?;
                }
            }
        }

        CfnValue::Join {
            delimiter, values, ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            model.apply_to(node, "cfn:delimiter", &format!("\"{}\"", delimiter))?;
            project_value(model, node, "values", values)?;
        }

        CfnValue::If {
            condition_name,
            value_if_true,
            value_if_false,
            ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            model.apply_to(node, "cfn:condition", &format!("\"{}\"", condition_name))?;
            project_value(model, node, "then", value_if_true)?;
            project_value(model, node, "else", value_if_false)?;
        }

        CfnValue::Select { index, list, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "index", index)?;
            project_value(model, node, "list", list)?;
        }

        CfnValue::GetAZs { region, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "region", region)?;
        }

        CfnValue::Base64 { value: inner, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "value", inner)?;
        }

        CfnValue::Split {
            delimiter, source, ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            model.apply_to(node, "cfn:delimiter", &format!("\"{}\"", delimiter))?;
            project_value(model, node, "source", source)?;
        }

        CfnValue::ImportValue { name, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "name", name)?;
        }

        CfnValue::FindInMap {
            map_name,
            top_key,
            second_key,
            default_value,
            ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "mapName", map_name)?;
            project_value(model, node, "topKey", top_key)?;
            project_value(model, node, "secondKey", second_key)?;
            if let Some(default) = default_value {
                project_value(model, node, "default", default)?;
            }
        }

        CfnValue::Cidr {
            ip_block,
            count,
            cidr_bits,
            ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "ipBlock", ip_block)?;
            project_value(model, node, "count", count)?;
            project_value(model, node, "cidrBits", cidr_bits)?;
        }

        CfnValue::ToJsonString { value: inner, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "value", inner)?;
        }

        CfnValue::Length { array, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "array", array)?;
        }

        CfnValue::Contains {
            values: vals,
            value: val,
            ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "values", vals)?;
            project_value(model, node, "value", val)?;
        }

        CfnValue::Transform {
            name, parameters, ..
        } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "name", name)?;
            project_value(model, node, "parameters", parameters)?;
        }

        CfnValue::Equals { left, right, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "left", left)?;
            project_value(model, node, "right", right)?;
        }

        CfnValue::Not { condition, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            project_value(model, node, "condition", condition)?;
        }

        CfnValue::And { conditions, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            let container = model.blank();
            model.apply_entity(node, "cfn:conditions", container)?;
            for cond in conditions {
                project_array_item(model, container, cond)?;
            }
        }

        CfnValue::Or { conditions, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            let container = model.blank();
            model.apply_entity(node, "cfn:conditions", container)?;
            for cond in conditions {
                project_array_item(model, container, cond)?;
            }
        }

        CfnValue::Condition { condition_name, .. } => {
            let node = project_intrinsic(model, parent, &pred, value)?;
            model.apply_to(
                node,
                "cfn:conditionName",
                &format!("\"{}\"", condition_name),
            )?;
        }
    }

    Ok(())
}

fn project_array_item(
    model: &mut Model,
    container: EntityId,
    value: &CfnValue,
) -> Result<(), ModelError> {
    match value {
        CfnValue::Object(map, _) => {
            let item = model.blank();
            model.apply_entity(container, "wa2:contains", item)?;
            for (key, (val, _)) in map {
                project_value(model, item, key, val)?;
            }
        }
        CfnValue::String(s, _) => {
            let item = model.blank();
            model.apply_entity(container, "wa2:contains", item)?;
            model.apply_to(item, "aws:value", &format!("\"{}\"", s))?;
        }
        CfnValue::Number(n, _) => {
            let item = model.blank();
            model.apply_entity(container, "wa2:contains", item)?;
            model.apply_to(item, "aws:value", &format!("\"{}\"", n))?;
        }
        CfnValue::Bool(b, _) => {
            let item = model.blank();
            model.apply_entity(container, "wa2:contains", item)?;
            model.apply_to(item, "aws:value", &format!("\"{}\"", b))?;
        }
        CfnValue::Array(items, _) => {
            let nested = model.blank();
            model.apply_entity(container, "wa2:contains", nested)?;
            for sub_item in items {
                project_array_item(model, nested, sub_item)?;
            }
        }
        CfnValue::Null(_) => {
            // Skip nulls in arrays
        }
        // All intrinsics
        other => {
            let item = model.blank();
            model.apply_entity(container, "wa2:contains", item)?;
            if let Some(name) = other.intrinsic_name() {
                model.apply_to(item, "wa2:type", &format!("cfn:{}", name))?;
                model.set_range(item, other.range());

                match other {
                    CfnValue::Ref { target, .. } => {
                        let target_entity = model.ensure_entity(target);
                        model.apply_entity(item, "cfn:target", target_entity)?;
                    }
                    CfnValue::GetAtt {
                        target, attribute, ..
                    } => {
                        let target_entity = model.ensure_entity(target);
                        model.apply_entity(item, "cfn:target", target_entity)?;
                        model.apply_to(item, "cfn:attribute", &format!("\"{}\"", attribute))?;
                    }
                    CfnValue::Sub {
                        template,
                        variables,
                        ..
                    } => {
                        if let Some(s) = template.as_str() {
                            model.apply_to(item, "cfn:template", &format!("\"{}\"", s))?;
                            for (var_name, _, _) in extract_sub_variable_positions(s) {
                                let target_entity = model.ensure_entity(&var_name);
                                model.apply_entity(item, "cfn:target", target_entity)?;
                            }
                        }
                        if let Some(vars) = variables {
                            for (var_name, var_value) in vars {
                                project_value(model, item, var_name, var_value)?;
                            }
                        }
                    }
                    CfnValue::Join {
                        delimiter, values, ..
                    } => {
                        model.apply_to(item, "cfn:delimiter", &format!("\"{}\"", delimiter))?;
                        project_value(model, item, "values", values)?;
                    }
                    CfnValue::If {
                        condition_name,
                        value_if_true,
                        value_if_false,
                        ..
                    } => {
                        model.apply_to(
                            item,
                            "cfn:condition",
                            &format!("\"{}\"", condition_name),
                        )?;
                        project_value(model, item, "then", value_if_true)?;
                        project_value(model, item, "else", value_if_false)?;
                    }
                    CfnValue::Select { index, list, .. } => {
                        project_value(model, item, "index", index)?;
                        project_value(model, item, "list", list)?;
                    }
                    CfnValue::GetAZs { region, .. } => {
                        project_value(model, item, "region", region)?;
                    }
                    CfnValue::Base64 { value: inner, .. } => {
                        project_value(model, item, "value", inner)?;
                    }
                    CfnValue::Split {
                        delimiter, source, ..
                    } => {
                        model.apply_to(item, "cfn:delimiter", &format!("\"{}\"", delimiter))?;
                        project_value(model, item, "source", source)?;
                    }
                    CfnValue::ImportValue { name, .. } => {
                        project_value(model, item, "name", name)?;
                    }
                    CfnValue::FindInMap {
                        map_name,
                        top_key,
                        second_key,
                        default_value,
                        ..
                    } => {
                        project_value(model, item, "mapName", map_name)?;
                        project_value(model, item, "topKey", top_key)?;
                        project_value(model, item, "secondKey", second_key)?;
                        if let Some(default) = default_value {
                            project_value(model, item, "default", default)?;
                        }
                    }
                    CfnValue::Cidr {
                        ip_block,
                        count,
                        cidr_bits,
                        ..
                    } => {
                        project_value(model, item, "ipBlock", ip_block)?;
                        project_value(model, item, "count", count)?;
                        project_value(model, item, "cidrBits", cidr_bits)?;
                    }
                    CfnValue::ToJsonString { value: inner, .. } => {
                        project_value(model, item, "value", inner)?;
                    }
                    CfnValue::Length { array, .. } => {
                        project_value(model, item, "array", array)?;
                    }
                    CfnValue::Contains {
                        values: vals,
                        value: val,
                        ..
                    } => {
                        project_value(model, item, "values", vals)?;
                        project_value(model, item, "value", val)?;
                    }
                    CfnValue::Transform {
                        name, parameters, ..
                    } => {
                        project_value(model, item, "name", name)?;
                        project_value(model, item, "parameters", parameters)?;
                    }
                    CfnValue::Equals { left, right, .. } => {
                        project_value(model, item, "left", left)?;
                        project_value(model, item, "right", right)?;
                    }
                    CfnValue::Not { condition, .. } => {
                        project_value(model, item, "condition", condition)?;
                    }
                    CfnValue::And { conditions, .. } | CfnValue::Or { conditions, .. } => {
                        let cond_container = model.blank();
                        model.apply_entity(item, "cfn:conditions", cond_container)?;
                        for cond in conditions {
                            project_array_item(model, cond_container, cond)?;
                        }
                    }
                    CfnValue::Condition { condition_name, .. } => {
                        model.apply_to(
                            item,
                            "cfn:conditionName",
                            &format!("\"{}\"", condition_name),
                        )?;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Project CFN parameters into the model
pub fn project_parameters(
    model: &mut Model,
    template_entity: EntityId,
    parameters: &HashMap<String, CfnParameter>,
) -> Result<(), ModelError> {
    if parameters.is_empty() {
        return Ok(());
    }

    let params_container = model.blank();
    model.apply_entity(template_entity, "cfn:parameters", params_container)?;

    for (name, param) in parameters {
        let param_entity = model.ensure_entity(name);
        model.apply_to(param_entity, "wa2:type", "cfn:Parameter")?;
        model.apply_to(
            param_entity,
            "cfn:type",
            &format!("\"{}\"", param.parameter_type),
        )?;
        model.apply_entity(params_container, "wa2:contains", param_entity)?;

        model.set_range(param_entity, param.name_range);

        if let Some(ref desc) = param.description {
            model.apply_to(param_entity, "cfn:description", &format!("\"{}\"", desc))?;
        }

        if let Some(ref default_val) = param.default_value {
            if let Some(s) = default_val.as_str() {
                model.apply_to(param_entity, "cfn:default", &format!("\"{}\"", s))?;
            }
        }
    }

    Ok(())
}

/// Project pseudo-parameters into the model
pub fn project_pseudo_parameters(
    model: &mut Model,
    template_entity: EntityId,
) -> Result<(), ModelError> {
    let pseudo_container = model.blank();
    model.apply_entity(template_entity, "cfn:pseudoParameters", pseudo_container)?;

    let pseudo_params = [
        "AWS::AccountId",
        "AWS::NotificationARNs",
        "AWS::NoValue",
        "AWS::Partition",
        "AWS::Region",
        "AWS::StackId",
        "AWS::StackName",
        "AWS::URLSuffix",
    ];

    for name in pseudo_params {
        let pseudo_entity = model.ensure_entity(name);
        model.apply_to(pseudo_entity, "wa2:type", "cfn:PseudoParameter")?;
        model.apply_entity(pseudo_container, "wa2:contains", pseudo_entity)?;
    }

    Ok(())
}

pub fn project_outputs(
    model: &mut Model,
    template_entity: EntityId,
    outputs: &HashMap<String, CfnOutput>,
) -> Result<(), ModelError> {
    if outputs.is_empty() {
        return Ok(());
    }

    let cfn_output = model.resolve("cfn:Output").expect("cfn:Output must exist");

    let outputs_container = model.blank();
    model.apply_entity(template_entity, "cfn:outputs", outputs_container)?;

    for output in outputs.values() {
        let output_entity = model.blank();
        model.apply_to(output_entity, "cfn:name", &output.name)?;
        model.apply_entity(output_entity, "wa2:type", cfn_output)?;
        model.set_range(output_entity, output.name_range);
        model.apply_entity(outputs_container, "wa2:contains", output_entity)?;

        project_value(model, output_entity, "cfn:value", &output.value)?;

        if let Some(ref export_name) = output.export_name {
            project_value(model, output_entity, "cfn:exportName", export_name)?;
        }
    }

    Ok(())
}