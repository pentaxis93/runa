//! Per-run forge mutation ledger.
//!
//! A protocol reports success only when every forge mutation its declared
//! procedure requires has been performed. This module owns the run-local
//! state that makes the rule enforceable at the MCP delivery surface:
//!
//! - which forge mutations succeeded in the current protocol step, and what
//!   the connector returned for each (the provenance later deliveries are
//!   verified against);
//! - the first refusal of a *required* forge mutation, which fails the step:
//!   artifact delivery and session advance are blocked, and the refusal is
//!   receipted for the CLI's protocol classification.
//!
//! Which mutations a protocol requires is consulted from the methodology's
//! own procedure authority — the structured workflow contract when the
//! methodology publishes one, the protocol instructions otherwise — matched
//! against the canonical operation set at its single home in
//! `runa-forge-contract`. This crate enumerates no operation list of its own.

use std::collections::{BTreeMap, BTreeSet};

use libagent::ProtocolDeclaration;
use libagent::protocol_failure::ProtocolFailureReceipt;
use runa_forge_compose::RuntimeError;
use runa_forge_contract::Operation;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The step a ledger's state belongs to. Fixed-protocol handlers hold one
/// step for the process lifetime; session handlers move to a new step on a
/// successful advance, which resets the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepKey {
    pub protocol: String,
    pub work_unit: Option<String>,
}

/// A recorded refusal of a required forge mutation.
#[derive(Debug, Clone)]
pub struct RecordedRefusal {
    pub operation: Operation,
    pub tool: String,
    pub cause: String,
}

impl RecordedRefusal {
    /// Operator-facing description naming the failed operation and its cause.
    pub fn describe(&self) -> String {
        format!(
            "required forge mutation refused: {}: {}",
            self.operation, self.cause
        )
    }

    pub fn receipt(&self, step: &StepKey) -> ProtocolFailureReceipt {
        ProtocolFailureReceipt {
            protocol: step.protocol.clone(),
            work_unit: step.work_unit.clone(),
            operation: self.operation.canonical_name().to_string(),
            tool: self.tool.clone(),
            cause: self.cause.clone(),
        }
    }
}

/// Run-local forge mutation state for the current protocol step.
#[derive(Debug, Default)]
pub struct MutationLedger {
    step: Option<StepKey>,
    succeeded: BTreeMap<Operation, Vec<Value>>,
    refusal: Option<RecordedRefusal>,
}

impl MutationLedger {
    /// Bind the ledger to the given step, resetting recorded state when the
    /// step changed. A step change can only follow a successful advance, so
    /// a recorded refusal is never discarded by alignment within its step.
    pub fn align_to_step(&mut self, step: StepKey) {
        if self.step.as_ref() != Some(&step) {
            self.step = Some(step);
            self.succeeded.clear();
            self.refusal = None;
        }
    }

    /// Record a successful forge mutation and the payload it returned.
    pub fn record_success(&mut self, operation: Operation, payload: Value) {
        if operation.is_mutation() {
            self.succeeded.entry(operation).or_default().push(payload);
        }
    }

    /// Record the refusal of a required forge mutation. The first refusal
    /// wins; a later one within the same step is not recorded over it.
    pub fn record_refusal(&mut self, refusal: RecordedRefusal) -> &RecordedRefusal {
        if self.refusal.is_none() {
            self.refusal = Some(refusal);
        }
        self.refusal.as_ref().expect("refusal recorded above")
    }

    /// The step's recorded refusal, if a required forge mutation was refused.
    pub fn refusal(&self) -> Option<&RecordedRefusal> {
        self.refusal.as_ref()
    }

    /// Payloads returned by successful invocations of `operation` this step.
    pub fn successful_payloads(&self, operation: Operation) -> &[Value] {
        self.succeeded
            .get(&operation)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// The forge mutations the protocol's declared procedure requires.
///
/// Consulted, not modeled: when the methodology publishes a structured
/// workflow contract for the protocol, its node mechanics are the authority;
/// otherwise the protocol's instruction content is. In both cases the
/// operation identities come from matching the authority's text against the
/// canonical operation set in `runa-forge-contract` — a mutation added to
/// the declaring authority comes under enforcement with no change here.
pub fn required_forge_mutations(protocol: &ProtocolDeclaration) -> BTreeSet<Operation> {
    if let Some(mechanics) = &protocol.workflow_mechanics {
        return mechanics
            .iter()
            .filter_map(|mechanic| Operation::from_canonical_name(mechanic))
            .filter(|operation| operation.is_mutation())
            .collect();
    }
    let Some(instructions) = &protocol.instructions else {
        return BTreeSet::new();
    };
    Operation::ALL
        .into_iter()
        .filter(|operation| operation.is_mutation())
        .filter(|operation| instructions.contains(operation.canonical_name()))
        .collect()
}

/// Whether a forge dispatch error is a refusal of the operation — the forge
/// or its transport declined or could not perform the mutation — as opposed
/// to a malformed call the agent can correct and reissue.
pub fn is_refusal(error: &RuntimeError) -> bool {
    match error {
        RuntimeError::UnknownTool(_) => false,
        RuntimeError::InvalidInput(_) => false,
        RuntimeError::Composition(_) => true,
        RuntimeError::Forge(forge_error) => !matches!(
            forge_error,
            runa_forge_contract::ForgeError::InvalidInput(_)
        ),
    }
}

/// The stable instance id a first-delivery handle-carrying artifact must
/// use: `{artifact_type}-{sha256(handle.id)}` per the methodology's
/// delivery convention.
pub fn derived_instance_id(artifact_type: &str, handle_id: &str) -> String {
    let digest = Sha256::digest(handle_id.as_bytes());
    format!("{artifact_type}-{digest:x}")
}

/// The outcome of the provenance gate on a handle-carrying artifact delivery.
pub enum HandleProvenance {
    /// Delivery may persist.
    Accepted,
    /// Delivery is refused before persistence, for the stated reason.
    Refused(String),
}

/// Verify a handle-carrying artifact delivery against per-run provenance.
///
/// First delivery (no artifact recorded under this `instance_id`): the body's
/// `handle` must equal a handle returned by a successful
/// [`Operation::CreateWorkUnit`] in the same protocol run, and `instance_id`
/// must be the stable derivation
/// from that handle's `id`. Refinement (the `instance_id` is already
/// recorded): the body carries the previously delivered `handle` through
/// unchanged, and [`Operation::CreateWorkUnit`] is not consulted.
///
/// `entry_adoption` marks a run opened from a forge entry ticket — the
/// acquisition path, which adopts the ticket's existing tracker identity and
/// creates nothing. Its first delivery is governed by the runtime's
/// acquisition machinery (promise resolution and tracker-consistency
/// validation), not by create provenance, so this gate accepts it; the
/// refinement rule still applies to an already-recorded instance.
pub fn verify_handle_provenance(
    ledger: &MutationLedger,
    artifact_type: &str,
    instance_id: &str,
    body: &Value,
    existing_handle: Option<&Value>,
    entry_adoption: bool,
) -> HandleProvenance {
    let Some(body_handle) = body.get("handle") else {
        return HandleProvenance::Refused(format!(
            "{artifact_type} delivery requires a connector handle in the artifact body"
        ));
    };

    if let Some(existing) = existing_handle {
        if body_handle == existing {
            return HandleProvenance::Accepted;
        }
        return HandleProvenance::Refused(format!(
            "refinement of {artifact_type}/{instance_id} must carry the existing \
             connector handle through unchanged; the submitted handle differs \
             from the previously delivered one"
        ));
    }

    if entry_adoption {
        return HandleProvenance::Accepted;
    }

    let created = ledger.successful_payloads(Operation::CreateWorkUnit);
    let Some(matching) = created
        .iter()
        .find(|payload| payload.get("handle") == Some(body_handle))
    else {
        return HandleProvenance::Refused(format!(
            "first delivery of {artifact_type}/{instance_id} requires a handle \
             returned by a successful {} operation in this protocol run; the \
             submitted handle has no such provenance and delivery is refused \
             before persistence",
            Operation::CreateWorkUnit
        ));
    };

    let Some(handle_id) = matching
        .get("handle")
        .and_then(|handle| handle.get("id"))
        .and_then(Value::as_str)
    else {
        return HandleProvenance::Refused(format!(
            "the {} result for this handle carries no string id to derive an \
             instance id from",
            Operation::CreateWorkUnit
        ));
    };

    let expected = derived_instance_id(artifact_type, handle_id);
    if instance_id != expected {
        return HandleProvenance::Refused(format!(
            "first delivery of a {artifact_type} artifact must use the stable \
             instance id derived from the connector handle id (expected \
             '{expected}', got '{instance_id}')"
        ));
    }

    HandleProvenance::Accepted
}

/// Whether an artifact type's full schema declares a connector `handle`
/// property, which brings its delivery under the provenance gate.
pub fn schema_declares_handle(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(|properties| properties.get("handle"))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libagent::TriggerCondition;
    use serde_json::json;

    fn protocol(instructions: Option<&str>, mechanics: Option<Vec<&str>>) -> ProtocolDeclaration {
        ProtocolDeclaration {
            name: "example".into(),
            requires: Vec::new(),
            accepts: Vec::new(),
            produces: Vec::new(),
            may_produce: Vec::new(),
            required_output_choices: Vec::new(),
            scoped: false,
            trigger: TriggerCondition::OnArtifact {
                name: "example".into(),
            },
            instructions: instructions.map(str::to_string),
            workflow_mechanics: mechanics
                .map(|items| items.into_iter().map(str::to_string).collect()),
        }
    }

    #[test]
    fn required_set_reads_workflow_contract_mechanics_when_published() {
        let mutation = Operation::ALL
            .into_iter()
            .find(|operation| operation.is_mutation())
            .unwrap();
        let read = Operation::ALL
            .into_iter()
            .find(|operation| !operation.is_mutation())
            .unwrap();
        let declaration = protocol(
            Some("prose that never names an operation"),
            Some(vec![
                mutation.canonical_name(),
                read.canonical_name(),
                "revise",
            ]),
        );
        let required = required_forge_mutations(&declaration);
        assert!(required.contains(&mutation));
        assert!(
            !required.contains(&read),
            "read-only operations are never required mutations"
        );
        assert_eq!(required.len(), 1);
    }

    #[test]
    fn required_set_falls_back_to_instructions_prose() {
        let mut named = Operation::ALL.into_iter().filter(|op| op.is_mutation());
        let first = named.next().unwrap();
        let second = named.next().unwrap();
        let text =
            format!("First invoke the connector capability `{first}` operation, then `{second}`.");
        let declaration = protocol(Some(&text), None);
        let required = required_forge_mutations(&declaration);
        assert!(required.contains(&first));
        assert!(required.contains(&second));
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn empty_mechanics_contract_requires_nothing_even_when_prose_names_operations() {
        let mutation = Operation::ALL
            .into_iter()
            .find(|operation| operation.is_mutation())
            .unwrap();
        let text = format!("prose naming `{mutation}`");
        let declaration = protocol(Some(&text), Some(Vec::new()));
        assert!(required_forge_mutations(&declaration).is_empty());
    }

    #[test]
    fn ledger_resets_on_step_change_and_first_refusal_wins() {
        let mutation = Operation::ALL
            .into_iter()
            .find(|operation| operation.is_mutation())
            .unwrap();
        let mut ledger = MutationLedger::default();
        ledger.align_to_step(StepKey {
            protocol: "one".into(),
            work_unit: None,
        });
        ledger.record_success(mutation, json!({"handle": {"id": "a"}}));
        ledger.record_refusal(RecordedRefusal {
            operation: mutation,
            tool: mutation.canonical_name().into(),
            cause: "first".into(),
        });
        ledger.record_refusal(RecordedRefusal {
            operation: mutation,
            tool: mutation.canonical_name().into(),
            cause: "second".into(),
        });
        assert_eq!(ledger.refusal().unwrap().cause, "first");
        assert_eq!(ledger.successful_payloads(mutation).len(), 1);

        // Same step: state persists.
        ledger.align_to_step(StepKey {
            protocol: "one".into(),
            work_unit: None,
        });
        assert!(ledger.refusal().is_some());

        // Step change: state resets.
        ledger.align_to_step(StepKey {
            protocol: "two".into(),
            work_unit: None,
        });
        assert!(ledger.refusal().is_none());
        assert!(ledger.successful_payloads(mutation).is_empty());
    }

    #[test]
    fn refusal_classification_spares_correctable_calls() {
        assert!(!is_refusal(&RuntimeError::UnknownTool("x".into())));
        assert!(!is_refusal(&RuntimeError::InvalidInput("bad".into())));
        assert!(!is_refusal(&RuntimeError::Forge(
            runa_forge_contract::ForgeError::InvalidInput("bad".into())
        )));
        for refused in [
            runa_forge_contract::ForgeError::Transport("provider returned 403".into()),
            runa_forge_contract::ForgeError::Transport("provider returned 429".into()),
            runa_forge_contract::ForgeError::Transport("network unreachable".into()),
            runa_forge_contract::ForgeError::ProviderResponse("missing number".into()),
            runa_forge_contract::ForgeError::ForeignScope("other repo".into()),
            runa_forge_contract::ForgeError::Unsupported("no such capability".into()),
        ] {
            assert!(is_refusal(&RuntimeError::Forge(refused)));
        }
    }

    #[test]
    fn first_delivery_provenance_accepts_only_same_run_handle_and_derived_id() {
        let mut ledger = MutationLedger::default();
        ledger.align_to_step(StepKey {
            protocol: "example".into(),
            work_unit: None,
        });
        let handle = json!({"id": "forge:scope:work:7", "display": "scope#7"});
        let body = json!({"title": "t", "handle": handle});

        // No create-work-unit in the run: refused.
        let outcome =
            verify_handle_provenance(&ledger, "work-unit", "work-unit-x", &body, None, false);
        assert!(matches!(outcome, HandleProvenance::Refused(_)));

        ledger.record_success(
            Operation::CreateWorkUnit,
            json!({"handle": handle, "title": "t", "state": "open"}),
        );

        // Fabricated handle: refused.
        let fabricated =
            json!({"title": "t", "handle": {"id": "forge:scope:work:8", "display": "scope#8"}});
        let outcome = verify_handle_provenance(
            &ledger,
            "work-unit",
            "work-unit-x",
            &fabricated,
            None,
            false,
        );
        assert!(matches!(outcome, HandleProvenance::Refused(_)));

        // Matching handle, wrong instance id: refused, naming the derivation.
        let outcome =
            verify_handle_provenance(&ledger, "work-unit", "work-unit-x", &body, None, false);
        match outcome {
            HandleProvenance::Refused(reason) => {
                assert!(reason.contains(&derived_instance_id("work-unit", "forge:scope:work:7")));
            }
            HandleProvenance::Accepted => panic!("content-hash instance id must be refused"),
        }

        // Matching handle, derived instance id: accepted.
        let derived = derived_instance_id("work-unit", "forge:scope:work:7");
        let outcome = verify_handle_provenance(&ledger, "work-unit", &derived, &body, None, false);
        assert!(matches!(outcome, HandleProvenance::Accepted));
    }

    #[test]
    fn refinement_carries_the_existing_handle_through_unchanged() {
        let ledger = MutationLedger::default();
        let existing = json!({"id": "forge:scope:work:7", "display": "scope#7"});
        let unchanged = json!({"title": "t2", "handle": existing});
        let outcome = verify_handle_provenance(
            &ledger,
            "work-unit",
            "work-unit-existing",
            &unchanged,
            Some(&existing),
            false,
        );
        assert!(
            matches!(outcome, HandleProvenance::Accepted),
            "refinement is unaffected and never consults create-work-unit"
        );

        let rederived =
            json!({"title": "t2", "handle": {"id": "forge:scope:work:9", "display": "scope#9"}});
        let outcome = verify_handle_provenance(
            &ledger,
            "work-unit",
            "work-unit-existing",
            &rederived,
            Some(&existing),
            false,
        );
        assert!(matches!(outcome, HandleProvenance::Refused(_)));
    }

    #[test]
    fn entry_adoption_first_delivery_is_governed_by_acquisition_not_create_provenance() {
        let ledger = MutationLedger::default();
        let body = json!({
            "title": "t",
            "handle": {"id": "forge:scope:work:14", "display": "scope#14"}
        });
        let outcome = verify_handle_provenance(
            &ledger,
            "work-unit",
            "work-unit-14-cold-start",
            &body,
            None,
            true,
        );
        assert!(
            matches!(outcome, HandleProvenance::Accepted),
            "an entry-ticket run adopts; no create provenance is demanded"
        );

        // Refinement equality still holds on the adoption path.
        let existing = json!({"id": "forge:scope:work:14", "display": "scope#14"});
        let rederived = json!({
            "title": "t",
            "handle": {"id": "forge:scope:work:15", "display": "scope#15"}
        });
        let outcome = verify_handle_provenance(
            &ledger,
            "work-unit",
            "work-unit-14-cold-start",
            &rederived,
            Some(&existing),
            true,
        );
        assert!(matches!(outcome, HandleProvenance::Refused(_)));
    }
}
