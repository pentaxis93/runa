//! Core data types for methodology manifests.
//!
//! Defines the four types that represent a parsed methodology: [`Manifest`],
//! [`ArtifactType`], [`ProtocolDeclaration`], and [`TriggerCondition`]. These
//! types are the shared vocabulary consumed by every other module in libagent.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::schema_scope::{WorkUnitOwnership, analyze_work_unit_ownership};

/// A methodology's complete registration with the runa runtime.
///
/// The manifest is the methodology's only interface with the runtime.
/// It declares the methodology's artifact types and protocol declarations.
/// runa reads it, builds the dependency graph, and begins monitoring.
///
/// Format: TOML. See `manifest::parse` for reading from files.
/// TOML serialization of this runtime model is not a supported operation:
/// `manifest::parse` populates schema content from the methodology layout
/// convention, and that derived state is intentionally not accepted back
/// through the TOML manifest parser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Methodology name.
    pub name: String,
    /// Artifact types declared by this methodology.
    pub artifact_types: Vec<ArtifactType>,
    /// Protocols declared by this methodology.
    pub protocols: Vec<ProtocolDeclaration>,
}

/// A named category of work product with a machine-checkable schema contract.
///
/// Methodologies define artifact types. The runtime validates instances
/// against their schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactType {
    /// Unique identifier within the methodology.
    pub name: String,
    /// JSON Schema defining what a valid instance contains.
    pub schema: serde_json::Value,
}

impl ArtifactType {
    /// Determine whether `work_unit` is runtime-owned, payload-owned, or absent.
    pub fn work_unit_ownership(
        &self,
    ) -> Result<WorkUnitOwnership, crate::schema_scope::WorkUnitSchemaError> {
        analyze_work_unit_ownership(&self.schema)
    }
}

/// An unscoped protocol declares an output schema that requires `work_unit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnscopedOutputRequiresWorkUnitError {
    pub protocol: String,
    pub artifact_type: String,
    pub schema_path: Option<String>,
    pub detail: Option<String>,
}

impl fmt::Display for UnscopedOutputRequiresWorkUnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(schema_path), Some(detail)) = (&self.schema_path, &self.detail) {
            return write!(
                f,
                "output artifact type '{}' has unresolved work_unit ownership at {}: {}",
                self.artifact_type, schema_path, detail
            );
        }
        write!(
            f,
            "protocol '{}' is declared unscoped but output artifact type '{}' requires 'work_unit'",
            self.protocol, self.artifact_type
        )
    }
}

impl std::error::Error for UnscopedOutputRequiresWorkUnitError {}

/// Reject the accepted manifest/output mismatch: unscoped protocols cannot
/// declare outputs whose schemas require `work_unit`.
pub fn validate_output_scope(
    protocol: &ProtocolDeclaration,
    artifact_type: &ArtifactType,
) -> Result<(), UnscopedOutputRequiresWorkUnitError> {
    let ownership = artifact_type.work_unit_ownership().map_err(|error| {
        UnscopedOutputRequiresWorkUnitError {
            protocol: protocol.name.clone(),
            artifact_type: artifact_type.name.clone(),
            schema_path: Some(error.schema_path),
            detail: Some(error.detail),
        }
    })?;
    if !protocol.scoped && ownership == WorkUnitOwnership::RuntimeRequired {
        return Err(UnscopedOutputRequiresWorkUnitError {
            protocol: protocol.name.clone(),
            artifact_type: artifact_type.name.clone(),
            schema_path: None,
            detail: None,
        });
    }

    Ok(())
}

/// A named group of output artifact types where exactly one member is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredOutputChoice {
    /// Unique group name within the declaring protocol.
    pub name: String,
    /// Artifact type names that satisfy this choice. Exactly one must exist
    /// and validate after protocol execution.
    pub members: Vec<String>,
}

/// A protocol's declared relationship to artifacts and its activation condition.
///
/// Protocols declare what they require, accept, produce, and may produce.
/// Topology emerges from the graph of these relationships across protocols.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolDeclaration {
    /// Unique identifier for the protocol.
    pub name: String,
    /// Artifact types that must exist and validate before execution.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Artifact types consumed if available; protocol operates without them.
    #[serde(default)]
    pub accepts: Vec<String>,
    /// Artifact types that must exist and validate after execution.
    #[serde(default)]
    pub produces: Vec<String>,
    /// Artifact types that may be produced; validated if present.
    #[serde(default)]
    pub may_produce: Vec<String>,
    /// Named output groups where exactly one member type must be produced.
    #[serde(default)]
    pub required_output_choices: Vec<RequiredOutputChoice>,
    /// Whether this protocol must be evaluated within a delegated work-unit scope.
    #[serde(default)]
    pub scoped: bool,
    /// Condition that activates this protocol.
    pub trigger: TriggerCondition,
    /// Protocol instruction content loaded from the methodology layout
    /// convention by `manifest::parse`.
    /// `None` when produced by `manifest::from_str` (no filesystem access).
    #[serde(skip)]
    pub instructions: Option<String>,
    /// Mechanic handles declared by the protocol's structured workflow
    /// contract (`workflow-contracts/{protocol_name}.toml` beside the
    /// manifest), when the methodology publishes one. Loaded by
    /// `manifest::parse`; `None` when the file is absent or when produced
    /// by `manifest::from_str` (no filesystem access). When present, this
    /// is the structured procedure authority; when absent, the protocol's
    /// `instructions` prose remains the declaring authority.
    #[serde(skip)]
    pub workflow_mechanics: Option<Vec<String>>,
}

impl ProtocolDeclaration {
    /// Required-choice member artifact types, in declaration order.
    pub fn required_choice_members(&self) -> impl Iterator<Item = &String> {
        self.required_output_choices
            .iter()
            .flat_map(|choice| choice.members.iter())
    }

    /// All declared output artifact types that can be produced by this protocol.
    pub fn output_artifact_types(&self) -> impl Iterator<Item = &String> {
        self.produces
            .iter()
            .chain(&self.may_produce)
            .chain(self.required_choice_members())
    }
}

/// Defines when the runtime should activate a protocol.
///
/// Primitive conditions test artifact state or external events.
/// Composite conditions combine primitives with `AllOf` and `AnyOf`.
/// Nesting is permitted to arbitrary depth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerCondition {
    /// The named artifact exists and satisfies its schema.
    OnArtifact { name: String },
    /// The named artifact is newer than the protocol's current output artifacts.
    OnChange { name: String },
    /// The named artifact exists but fails schema validation.
    OnInvalid { name: String },
    /// All conditions must be satisfied.
    AllOf { conditions: Vec<TriggerCondition> },
    /// At least one condition must be satisfied.
    AnyOf { conditions: Vec<TriggerCondition> },
}

impl fmt::Display for TriggerCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriggerCondition::OnArtifact { name } => write!(f, "on_artifact({name})"),
            TriggerCondition::OnChange { name } => write!(f, "on_change({name})"),
            TriggerCondition::OnInvalid { name } => write!(f, "on_invalid({name})"),
            TriggerCondition::AllOf { conditions } => {
                write!(f, "all_of(")?;
                for (i, c) in conditions.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{c}")?;
                }
                write!(f, ")")
            }
            TriggerCondition::AnyOf { conditions } => {
                write!(f, "any_of(")?;
                for (i, c) in conditions.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{c}")?;
                }
                write!(f, ")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_type_round_trip() {
        let at = ArtifactType {
            name: "constraints".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string" }
                },
                "required": ["description"]
            }),
        };
        let json = serde_json::to_string(&at).unwrap();
        let deserialized: ArtifactType = serde_json::from_str(&json).unwrap();
        assert_eq!(at, deserialized);
    }

    #[test]
    fn protocol_declaration_round_trip() {
        let protocol = ProtocolDeclaration {
            name: "design".into(),
            requires: vec!["constraints".into()],
            accepts: vec!["prior-design".into()],
            produces: vec!["design-doc".into()],
            may_produce: vec!["design-notes".into()],
            required_output_choices: Vec::new(),
            scoped: true,
            trigger: TriggerCondition::OnArtifact {
                name: "constraints".into(),
            },
            instructions: None,
            workflow_mechanics: None,
        };
        let json = serde_json::to_string(&protocol).unwrap();
        let deserialized: ProtocolDeclaration = serde_json::from_str(&json).unwrap();
        assert_eq!(protocol, deserialized);
    }

    #[test]
    fn trigger_condition_simple_round_trip() {
        let cases = vec![
            TriggerCondition::OnArtifact {
                name: "constraints".into(),
            },
            TriggerCondition::OnChange {
                name: "design-doc".into(),
            },
            TriggerCondition::OnInvalid {
                name: "test-evidence".into(),
            },
        ];
        for tc in cases {
            let json = serde_json::to_string(&tc).unwrap();
            let deserialized: TriggerCondition = serde_json::from_str(&json).unwrap();
            assert_eq!(tc, deserialized);
        }
    }

    #[test]
    fn trigger_condition_nested_round_trip() {
        let tc = TriggerCondition::AllOf {
            conditions: vec![
                TriggerCondition::OnArtifact {
                    name: "constraints".into(),
                },
                TriggerCondition::AnyOf {
                    conditions: vec![
                        TriggerCondition::OnInvalid {
                            name: "draft".into(),
                        },
                        TriggerCondition::OnArtifact {
                            name: "auto-approve".into(),
                        },
                    ],
                },
            ],
        };
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: TriggerCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, deserialized);
    }

    #[test]
    fn trigger_condition_json_shape() {
        let tc = TriggerCondition::OnArtifact {
            name: "constraints".into(),
        };
        let value = serde_json::to_value(&tc).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "on_artifact",
                "name": "constraints"
            })
        );

        let tc = TriggerCondition::AllOf {
            conditions: vec![TriggerCondition::OnInvalid {
                name: "report".into(),
            }],
        };
        let value = serde_json::to_value(&tc).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "all_of",
                "conditions": [
                    { "type": "on_invalid", "name": "report" }
                ]
            })
        );
    }

    #[test]
    fn protocol_declaration_empty_vecs() {
        let protocol = ProtocolDeclaration {
            name: "artifact-only".into(),
            requires: vec![],
            accepts: vec![],
            produces: vec![],
            may_produce: vec![],
            required_output_choices: Vec::new(),
            scoped: false,
            trigger: TriggerCondition::OnArtifact {
                name: "constraints".into(),
            },
            instructions: None,
            workflow_mechanics: None,
        };
        let json = serde_json::to_string(&protocol).unwrap();
        let deserialized: ProtocolDeclaration = serde_json::from_str(&json).unwrap();
        assert_eq!(protocol, deserialized);
    }

    #[test]
    fn protocol_declaration_defaults_scoped_to_false() {
        let json = serde_json::json!({
            "name": "artifact-only",
            "requires": [],
            "accepts": [],
            "produces": [],
            "may_produce": [],
            "trigger": {
                "type": "on_artifact",
                "name": "constraints"
            }
        });

        let deserialized: ProtocolDeclaration = serde_json::from_value(json).unwrap();
        assert!(!deserialized.scoped);
    }

    #[test]
    fn display_on_artifact() {
        let tc = TriggerCondition::OnArtifact {
            name: "constraints".into(),
        };
        assert_eq!(tc.to_string(), "on_artifact(constraints)");
    }

    #[test]
    fn display_on_change() {
        let tc = TriggerCondition::OnChange {
            name: "design-doc".into(),
        };
        assert_eq!(tc.to_string(), "on_change(design-doc)");
    }

    #[test]
    fn display_on_invalid() {
        let tc = TriggerCondition::OnInvalid {
            name: "test-evidence".into(),
        };
        assert_eq!(tc.to_string(), "on_invalid(test-evidence)");
    }

    #[test]
    fn display_nested_composite() {
        let tc = TriggerCondition::AllOf {
            conditions: vec![
                TriggerCondition::OnArtifact { name: "X".into() },
                TriggerCondition::AnyOf {
                    conditions: vec![
                        TriggerCondition::OnInvalid { name: "Z".into() },
                        TriggerCondition::OnArtifact { name: "Y".into() },
                    ],
                },
            ],
        };
        assert_eq!(
            tc.to_string(),
            "all_of(on_artifact(X), any_of(on_invalid(Z), on_artifact(Y)))"
        );
    }

    #[test]
    fn validate_output_scope_rejects_unscoped_output_requiring_work_unit() {
        let protocol = ProtocolDeclaration {
            name: "implement".into(),
            requires: vec![],
            accepts: vec![],
            produces: vec!["implementation".into()],
            may_produce: vec![],
            required_output_choices: Vec::new(),
            scoped: false,
            trigger: TriggerCondition::OnArtifact {
                name: "draft".into(),
            },
            instructions: None,
            workflow_mechanics: None,
        };
        let artifact_type = ArtifactType {
            name: "implementation".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "work_unit": { "type": "string" }
                },
                "required": ["title", "work_unit"]
            }),
        };

        let error = validate_output_scope(&protocol, &artifact_type).unwrap_err();
        assert_eq!(error.protocol, "implement");
        assert_eq!(error.artifact_type, "implementation");
    }

    #[test]
    fn validate_output_scope_allows_scoped_output_without_required_work_unit() {
        let protocol = ProtocolDeclaration {
            name: "summarize".into(),
            requires: vec![],
            accepts: vec![],
            produces: vec!["summary".into()],
            may_produce: vec![],
            required_output_choices: Vec::new(),
            scoped: true,
            trigger: TriggerCondition::OnArtifact {
                name: "draft".into(),
            },
            instructions: None,
            workflow_mechanics: None,
        };
        let artifact_type = ArtifactType {
            name: "summary".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" }
                },
                "required": ["title"]
            }),
        };

        assert!(validate_output_scope(&protocol, &artifact_type).is_ok());
    }

    #[test]
    fn validate_output_scope_rejects_work_unit_required_through_local_ref() {
        let protocol = ProtocolDeclaration {
            name: "summarize".into(),
            requires: vec![],
            accepts: vec![],
            produces: vec!["summary".into()],
            may_produce: vec![],
            required_output_choices: Vec::new(),
            scoped: false,
            trigger: TriggerCondition::OnArtifact {
                name: "draft".into(),
            },
            instructions: None,
            workflow_mechanics: None,
        };
        let artifact_type = ArtifactType {
            name: "summary".into(),
            schema: serde_json::json!({
                "$defs": {
                    "scoped": {
                        "type": "object",
                        "properties": { "work_unit": { "type": "string" } },
                        "required": ["work_unit"]
                    }
                },
                "$ref": "#/$defs/scoped"
            }),
        };

        assert!(validate_output_scope(&protocol, &artifact_type).is_err());
    }

    #[test]
    fn schema_classifies_optional_work_unit_as_payload_owned() {
        let artifact_type = ArtifactType {
            name: "summary".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "work_unit": { "type": "string" }
                },
                "required": ["title"]
            }),
        };

        assert_eq!(
            artifact_type.work_unit_ownership().unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn schema_classifies_required_work_unit_as_runtime_owned() {
        let artifact_type = ArtifactType {
            name: "implementation".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["title", "work_unit"]
            }),
        };

        assert_eq!(
            artifact_type.work_unit_ownership().unwrap(),
            WorkUnitOwnership::RuntimeRequired
        );
    }
}
