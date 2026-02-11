//! AWS-specific type derivation and evidence detection

use crate::intents::model::{Axis, Cmp, EntityId, Model, ModelError, Query, Value};

/// Derive wa2 abstract types from AWS resource types
pub fn derive_wa2_type(model: &mut Model, entity: EntityId) -> Result<(), ModelError> {
    let Some(aws_type) = model.get_literal(entity, "aws:type") else {
        return Ok(());
    };

    let wa2_kind = match aws_type.as_str() {
        "AWS::EC2::Instance" | "AWS::Lambda::Function" => Some("wa2:Run"),
        "AWS::S3::Bucket" | "AWS::EC2::Volume" | "AWS::EFS::FileSystem" => Some("wa2:Store"),
        "AWS::SQS::Queue" | "AWS::Kinesis::Stream" => Some("wa2:Move"),
        _ => None,
    };

    if let Some(kind) = wa2_kind {
        model.apply_to(entity, "wa2:type", kind)?;
    }

    Ok(())
}

/// Derive evidence from resource configuration
pub fn derive_evidence(model: &mut Model, entity: EntityId) -> Result<(), ModelError> {
    // Check for S3 replication configuration
    let query = Query::follow("aws:ReplicationConfiguration")
        .then_follow("aws:Rules")
        .then(Axis::Child, None)
        .filter("aws:Status", Cmp::Eq, Value::Literal("Enabled".to_string()));

    if !model.query_from(&[entity], &query).is_empty() {
        let evidence = model.blank();
        model.apply_to(evidence, "wa2:type", "wa2:Evidence")?;
        model.apply_to(evidence, "wa2:value", "\"DataResiliance\"")?;
        model.apply_entity(entity, "wa2:contains", evidence)?;
    }

    Ok(())
}