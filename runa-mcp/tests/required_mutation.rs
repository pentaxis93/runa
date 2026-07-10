//! Required forge mutation enforcement at the MCP surface.
//!
//! A protocol whose declared procedure requires a forge mutation reports
//! success only if that mutation succeeded. These tests drive real
//! `runa-mcp` processes against local HTTP stubs standing in for the forge:
//! the stub refuses (or serves) the mutation, and the assertions read the
//! enforcement from the tool results, the artifact store, and the
//! protocol-failure receipt — no live forge, no deployment gate.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::ServiceExt;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use runa_forge_contract::Operation;
use sha2::{Digest, Sha256};
use tokio::process::Command;

fn write_methodology(
    dir: &Path,
    manifest_toml: &str,
    schemas: &[(&str, &str)],
    protocols: &[(&str, &str)],
) -> PathBuf {
    let manifest_path = dir.join("manifest.toml");
    fs::write(&manifest_path, manifest_toml).unwrap();

    let schemas_dir = dir.join("schemas");
    fs::create_dir_all(&schemas_dir).unwrap();
    for (name, content) in schemas {
        fs::write(schemas_dir.join(format!("{name}.schema.json")), content).unwrap();
    }

    for (protocol_name, instructions) in protocols {
        let protocol_dir = dir.join("protocols").join(protocol_name);
        fs::create_dir_all(&protocol_dir).unwrap();
        fs::write(protocol_dir.join("PROTOCOL.md"), instructions).unwrap();
    }

    manifest_path
}

fn init_project(project_dir: &Path, manifest_path: &Path) {
    let runa_dir = project_dir.join(".runa");
    fs::create_dir_all(&runa_dir).unwrap();

    let manifest_path = fs::canonicalize(manifest_path).unwrap();
    fs::write(
        runa_dir.join("config.toml"),
        format!(
            "methodology_path = {:?}\n",
            manifest_path.display().to_string()
        ),
    )
    .unwrap();
    fs::write(
        runa_dir.join("state.toml"),
        "initialized_at = \"2026-03-25T00:00:00Z\"\nruna_version = \"0.1.0\"\n",
    )
    .unwrap();
}

fn append_github_forge_config(project_dir: &Path, api_base: &str) {
    let config_path = project_dir.join(".runa/config.toml");
    let existing = fs::read_to_string(&config_path).unwrap();
    fs::write(
        config_path,
        format!(
            "{existing}\n[forge]\ntype = \"github\"\nowner = \"tesserine\"\nname = \"example\"\nassignee = \"operator\"\napi_base = \"{api_base}\"\n",
        ),
    )
    .unwrap();
}

/// A stub forge serving one scripted `(status_line, body)` response per
/// accepted connection, in order.
fn scripted_forge(
    responses: &'static [(&'static str, &'static str)],
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for (status_line, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    (format!("http://{address}"), server)
}

fn tool_call(name: &str, arguments: serde_json::Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string()).with_arguments(
        arguments
            .as_object()
            .expect("tool arguments must be an object")
            .clone(),
    )
}

fn tool_result_text(result: &CallToolResult) -> String {
    result.content[0]
        .as_text()
        .expect("tool result should be text")
        .text
        .clone()
}

async fn call(
    service: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: serde_json::Value,
) -> CallToolResult {
    tokio::time::timeout(
        Duration::from_secs(10),
        service.call_tool(tool_call(name, arguments)),
    )
    .await
    .expect("MCP call should return before the timeout")
    .unwrap()
}

fn report_manifest_toml() -> &'static str {
    r#"
name = "groundwork"

[[artifact_types]]
name = "report"

[[protocols]]
name = "publish"
produces = ["report"]
trigger = { type = "on_change", name = "report" }
"#
}

fn report_schema() -> (&'static str, &'static str) {
    (
        "report",
        r#"{"type":"object","required":["title"],"properties":{"title":{"type":"string"}}}"#,
    )
}

/// Instructions naming the given operations as procedure mechanics; the
/// protocol's required forge mutation set derives from this authority.
fn instructions_naming(operations: &[Operation]) -> String {
    let mut text = String::from("# protocol\n\nProcedure:\n");
    for operation in operations {
        text.push_str(&format!(
            "- invoke the connector capability `{operation}` operation\n"
        ));
    }
    text
}

struct FixedProtocolRun {
    _dir: tempfile::TempDir,
    project_dir: PathBuf,
    receipt_path: PathBuf,
    service: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

async fn spawn_publish_protocol(required: &[Operation], api_base: &str) -> FixedProtocolRun {
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = write_methodology(
        dir.path(),
        report_manifest_toml(),
        &[report_schema()],
        &[("publish", &instructions_naming(required))],
    );
    let project_dir = dir.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    init_project(&project_dir, &manifest_path);
    append_github_forge_config(&project_dir, api_base);

    let receipt_path = dir.path().join("receipt/protocol-failure.json");
    let receipt_env = receipt_path.to_string_lossy().into_owned();

    let service = ()
        .serve(
            TokioChildProcess::new(
                Command::new(env!("CARGO_BIN_EXE_runa-mcp")).configure(|cmd| {
                    cmd.arg("--protocol")
                        .arg("publish")
                        .env_remove("RUNA_FORGE_TYPE")
                        .env_remove("RUNA_FORGE_OWNER")
                        .env_remove("RUNA_FORGE_NAME")
                        .env_remove("RUNA_FORGE_TRACKER_ID")
                        .env(libagent::PROTOCOL_FAILURE_RECEIPT_ENV, &receipt_env)
                        .current_dir(&project_dir);
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    FixedProtocolRun {
        _dir: dir,
        project_dir,
        receipt_path,
        service,
    }
}

fn record_progress_input() -> serde_json::Value {
    serde_json::json!({
        "handle": { "id": "github:tesserine/example:issue:9", "display": "tesserine/example#9" },
        "body": "progress note"
    })
}

fn close_out_input() -> serde_json::Value {
    serde_json::json!({
        "work_unit": { "id": "github:tesserine/example:issue:9", "display": "tesserine/example#9" },
        "completion": {
            "criterion_summary": "done",
            "gaps": [],
            "change_reference": "abc123",
            "documentation_status": "updated"
        },
        "body": "completion record"
    })
}

#[tokio::test]
async fn refused_required_mutation_fails_protocol_names_cause_and_blocks_artifact_delivery() {
    let record_progress = Operation::RecordProgress;
    let (api_base, server) = scripted_forge(&[("403 Forbidden", r#"{"message":"Forbidden"}"#)]);
    let run = spawn_publish_protocol(&[record_progress], &api_base).await;

    let refusal = call(
        &run.service,
        record_progress.canonical_name(),
        record_progress_input(),
    )
    .await;
    assert_eq!(refusal.is_error, Some(true));
    let text = tool_result_text(&refusal);
    assert!(
        text.contains("required forge mutation refused"),
        "refusal names the enforcement: {text}"
    );
    assert!(
        text.contains(record_progress.canonical_name()),
        "refusal names the operation: {text}"
    );
    assert!(
        text.contains("403"),
        "refusal names the transport cause: {text}"
    );

    // The refusal is sticky: the protocol's artifact output is blocked.
    let blocked = call(
        &run.service,
        "report",
        serde_json::json!({ "instance_id": "report-1", "title": "t" }),
    )
    .await;
    assert_eq!(blocked.is_error, Some(true));
    let text = tool_result_text(&blocked);
    assert!(
        text.contains("artifact delivery is blocked"),
        "artifact delivery is blocked after the refusal: {text}"
    );
    assert!(
        !run.project_dir
            .join(".runa/workspace/report/report-1.json")
            .exists(),
        "no artifact is persisted past a refused required mutation"
    );

    // The refusal is receipted for the CLI's protocol classification.
    let receipt = libagent::protocol_failure::read_receipt(&run.receipt_path)
        .unwrap()
        .expect("a protocol-failure receipt is written");
    assert_eq!(receipt.protocol, "publish");
    assert_eq!(receipt.operation, record_progress.canonical_name());
    assert!(receipt.cause.contains("403"), "{}", receipt.cause);

    run.service.cancel().await.unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn out_of_set_mutation_and_non_403_refusal_fail_by_the_same_mechanism() {
    // An operation outside those observed at the originating defect sites,
    // refused with a non-403 (rate limit): same enforcement, no site-local
    // special case.
    let close_out = Operation::CloseOut;
    let (api_base, server) = scripted_forge(&[(
        "429 Too Many Requests",
        r#"{"message":"API rate limit exceeded"}"#,
    )]);
    let run = spawn_publish_protocol(&[close_out], &api_base).await;

    let refusal = call(&run.service, close_out.canonical_name(), close_out_input()).await;
    assert_eq!(refusal.is_error, Some(true));
    let text = tool_result_text(&refusal);
    assert!(text.contains("required forge mutation refused"), "{text}");
    assert!(text.contains(close_out.canonical_name()), "{text}");
    assert!(text.contains("429"), "{text}");

    let blocked = call(
        &run.service,
        "report",
        serde_json::json!({ "instance_id": "report-1", "title": "t" }),
    )
    .await;
    assert_eq!(blocked.is_error, Some(true));
    assert!(tool_result_text(&blocked).contains("artifact delivery is blocked"));

    let receipt = libagent::protocol_failure::read_receipt(&run.receipt_path)
        .unwrap()
        .expect("a protocol-failure receipt is written");
    assert_eq!(receipt.operation, close_out.canonical_name());
    assert!(receipt.cause.contains("429"), "{}", receipt.cause);

    run.service.cancel().await.unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn refused_mutation_outside_the_required_set_does_not_fail_the_protocol() {
    // The protocol's declared procedure requires only record-progress; a
    // refused close-out is an error result but not a protocol failure.
    let (api_base, server) = scripted_forge(&[("403 Forbidden", r#"{"message":"Forbidden"}"#)]);
    let run = spawn_publish_protocol(&[Operation::RecordProgress], &api_base).await;

    let refusal = call(
        &run.service,
        Operation::CloseOut.canonical_name(),
        close_out_input(),
    )
    .await;
    assert_eq!(refusal.is_error, Some(true));
    let text = tool_result_text(&refusal);
    assert!(
        !text.contains("required forge mutation refused"),
        "an unrequired mutation's refusal is not a protocol failure: {text}"
    );

    let delivered = call(
        &run.service,
        "report",
        serde_json::json!({ "instance_id": "report-1", "title": "t" }),
    )
    .await;
    assert_ne!(delivered.is_error, Some(true), "{delivered:?}");
    assert_eq!(
        libagent::protocol_failure::read_receipt(&run.receipt_path).unwrap(),
        None,
        "no receipt without a refused required mutation"
    );

    run.service.cancel().await.unwrap();
    server.join().unwrap();
}

fn decompose_manifest_toml() -> &'static str {
    r#"
name = "groundwork"

[[artifact_types]]
name = "work-unit"

[[protocols]]
name = "decompose"
produces = ["work-unit"]
trigger = { type = "on_change", name = "work-unit" }
"#
}

fn work_unit_schema() -> (&'static str, &'static str) {
    (
        "work-unit",
        r#"{"type":"object","required":["title","description","acceptance_criteria","handle"],"additionalProperties":false,"properties":{"title":{"type":"string"},"description":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}},"handle":{"type":"object","required":["id","display"],"properties":{"id":{"type":"string"},"display":{"type":"string"}}}}}"#,
    )
}

fn work_unit_body(handle: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "title": "unit title",
        "description": "unit description",
        "acceptance_criteria": ["criterion"],
        "handle": handle
    })
}

fn derived_id(artifact_type: &str, handle_id: &str) -> String {
    let digest = Sha256::digest(handle_id.as_bytes());
    format!("{artifact_type}-{digest:x}")
}

#[tokio::test]
async fn first_delivery_provenance_and_refinement_are_enforced_at_the_delivery_surface() {
    let create = Operation::CreateWorkUnit;
    // One scripted create response for the whole test: refinement never
    // invoking create-work-unit is proven by the stub serving exactly one.
    let (api_base, server) = scripted_forge(&[(
        "200 OK",
        r#"{"number":7,"title":"unit title","body":"unit description","state":"open"}"#,
    )]);

    let dir = tempfile::tempdir().unwrap();
    let manifest_path = write_methodology(
        dir.path(),
        decompose_manifest_toml(),
        &[work_unit_schema()],
        &[("decompose", &instructions_naming(&[create]))],
    );
    let project_dir = dir.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    init_project(&project_dir, &manifest_path);
    append_github_forge_config(&project_dir, &api_base);

    let service = ()
        .serve(
            TokioChildProcess::new(
                Command::new(env!("CARGO_BIN_EXE_runa-mcp")).configure(|cmd| {
                    cmd.arg("--protocol")
                        .arg("decompose")
                        .env_remove("RUNA_FORGE_TYPE")
                        .env_remove("RUNA_FORGE_OWNER")
                        .env_remove("RUNA_FORGE_NAME")
                        .env_remove("RUNA_FORGE_TRACKER_ID")
                        .current_dir(&project_dir);
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let issued_handle = serde_json::json!({
        "id": "github:tesserine/example:issue:7",
        "display": "tesserine/example#7"
    });
    let derived = derived_id("work-unit", "github:tesserine/example:issue:7");

    // First delivery before any create-work-unit: refused.
    let premature = call(&service, "work-unit", {
        let mut input = work_unit_body(issued_handle.clone());
        input["instance_id"] = serde_json::json!(derived.clone());
        input
    })
    .await;
    assert_eq!(premature.is_error, Some(true));
    assert!(
        tool_result_text(&premature).contains("refused before persistence"),
        "{premature:?}"
    );

    // The sanctioned create.
    let created = call(
        &service,
        create.canonical_name(),
        serde_json::json!({ "title": "unit title", "body": "unit description" }),
    )
    .await;
    assert_ne!(created.is_error, Some(true), "{created:?}");
    let payload: serde_json::Value = serde_json::from_str(&tool_result_text(&created)).unwrap();
    assert_eq!(payload["handle"], issued_handle);

    // Fabricated handle: refused.
    let fabricated_handle = serde_json::json!({
        "id": "github:tesserine/example:issue:8",
        "display": "tesserine/example#8"
    });
    let fabricated = call(&service, "work-unit", {
        let mut input = work_unit_body(fabricated_handle);
        input["instance_id"] =
            serde_json::json!(derived_id("work-unit", "github:tesserine/example:issue:8"));
        input
    })
    .await;
    assert_eq!(fabricated.is_error, Some(true));
    assert!(
        tool_result_text(&fabricated).contains("no such provenance"),
        "{fabricated:?}"
    );

    // Sanctioned handle, content-hash instance id: refused, naming the derivation.
    let content_hash_id = call(&service, "work-unit", {
        let mut input = work_unit_body(issued_handle.clone());
        input["instance_id"] = serde_json::json!("work-unit-b650a7358d63b733");
        input
    })
    .await;
    assert_eq!(content_hash_id.is_error, Some(true));
    assert!(
        tool_result_text(&content_hash_id).contains(&derived),
        "the refusal names the expected derivation: {content_hash_id:?}"
    );

    // Sanctioned handle, derived instance id: persisted.
    let accepted = call(&service, "work-unit", {
        let mut input = work_unit_body(issued_handle.clone());
        input["instance_id"] = serde_json::json!(derived.clone());
        input
    })
    .await;
    assert_ne!(accepted.is_error, Some(true), "{accepted:?}");
    assert!(
        project_dir
            .join(format!(".runa/workspace/work-unit/{derived}.json"))
            .exists()
    );

    // Refinement: existing instance id, handle carried through unchanged —
    // accepted, and create-work-unit is not invoked (the stub served its one
    // scripted response already).
    let refined = call(&service, "work-unit", {
        let mut input = work_unit_body(issued_handle.clone());
        input["title"] = serde_json::json!("refined title");
        input["instance_id"] = serde_json::json!(derived.clone());
        input
    })
    .await;
    assert_ne!(refined.is_error, Some(true), "{refined:?}");
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_dir.join(format!(".runa/workspace/work-unit/{derived}.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["title"], "refined title");

    // Refinement that re-derives the handle: refused.
    let rederived = call(&service, "work-unit", {
        let mut input = work_unit_body(serde_json::json!({
            "id": "github:tesserine/example:issue:99",
            "display": "tesserine/example#99"
        }));
        input["instance_id"] = serde_json::json!(derived.clone());
        input
    })
    .await;
    assert_eq!(rederived.is_error, Some(true));
    assert!(
        tool_result_text(&rederived).contains("through unchanged"),
        "{rederived:?}"
    );

    service.cancel().await.unwrap();
    server.join().unwrap();
}

fn define_session_manifest_toml() -> &'static str {
    r#"
name = "groundwork"

[[artifact_types]]
name = "work-unit"

[[artifact_types]]
name = "contract"

[[artifact_types]]
name = "implementation-plan"

[[protocols]]
name = "define"
requires = ["work-unit"]
produces = ["contract"]
scoped = true
trigger = { type = "on_artifact", name = "work-unit" }

[[protocols]]
name = "plan"
requires = ["contract"]
produces = ["implementation-plan"]
scoped = true
trigger = { type = "on_artifact", name = "contract" }
"#
}

fn define_session_schemas() -> Vec<(&'static str, &'static str)> {
    vec![
        work_unit_schema(),
        (
            "contract",
            r#"{"type":"object","required":["work_unit","criteria"],"properties":{"work_unit":{"type":"string"},"criteria":{"type":"array","items":{"type":"string"}}}}"#,
        ),
        (
            "implementation-plan",
            r#"{"type":"object","required":["work_unit","steps"],"properties":{"work_unit":{"type":"string"},"steps":{"type":"array","items":{"type":"string"}}}}"#,
        ),
    ]
}

/// A define-shaped scoped session step whose declared procedure requires the
/// given forge mutation; the stub refuses it. Asserts the shared enforcement:
/// the refusal names the operation, the contract artifact is not deliverable,
/// advance is blocked, and the receipt records the refusal.
async fn assert_define_fails_closed_on_refused(operation: Operation, input: serde_json::Value) {
    let (api_base, server) =
        scripted_forge(&[("403 Forbidden", r#"{"message":"Resource not accessible"}"#)]);

    let dir = tempfile::tempdir().unwrap();
    let instructions = instructions_naming(&[operation]);
    let manifest_path = write_methodology(
        dir.path(),
        define_session_manifest_toml(),
        &define_session_schemas(),
        &[("define", &instructions), ("plan", "# plan\n")],
    );
    let project_dir = dir.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    init_project(&project_dir, &manifest_path);
    append_github_forge_config(&project_dir, &api_base);

    let work_unit_id = derived_id("work-unit", "github:tesserine/example:issue:9");
    let workspace = project_dir.join(".runa/workspace");
    fs::create_dir_all(workspace.join("work-unit")).unwrap();
    fs::write(
        workspace.join(format!("work-unit/{work_unit_id}.json")),
        serde_json::to_string_pretty(&work_unit_body(serde_json::json!({
            "id": "github:tesserine/example:issue:9",
            "display": "tesserine/example#9"
        })))
        .unwrap(),
    )
    .unwrap();

    let receipt_path = dir.path().join("receipt/protocol-failure.json");
    let receipt_env = receipt_path.to_string_lossy().into_owned();

    let service = ()
        .serve(
            TokioChildProcess::new(
                Command::new(env!("CARGO_BIN_EXE_runa-mcp")).configure(|cmd| {
                    cmd.arg("--session")
                        .arg("--work-unit")
                        .arg(&work_unit_id)
                        .env_remove("RUNA_FORGE_TYPE")
                        .env_remove("RUNA_FORGE_OWNER")
                        .env_remove("RUNA_FORGE_NAME")
                        .env_remove("RUNA_FORGE_TRACKER_ID")
                        .env(libagent::PROTOCOL_FAILURE_RECEIPT_ENV, &receipt_env)
                        .current_dir(&project_dir);
                }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let refusal = call(&service, operation.canonical_name(), input).await;
    assert_eq!(refusal.is_error, Some(true));
    let text = tool_result_text(&refusal);
    assert!(text.contains("required forge mutation refused"), "{text}");
    assert!(text.contains(operation.canonical_name()), "{text}");

    // The contract artifact is not enough: its delivery is blocked.
    let contract = call(
        &service,
        "contract",
        serde_json::json!({ "instance_id": "contract-9", "criteria": ["c1"] }),
    )
    .await;
    assert_eq!(contract.is_error, Some(true));
    assert!(
        tool_result_text(&contract).contains("artifact delivery is blocked"),
        "{contract:?}"
    );

    // Define cannot advance; plan does not become the next trusted stage.
    let advance = call(&service, "advance", serde_json::json!({})).await;
    assert_eq!(advance.is_error, Some(true));
    let text = tool_result_text(&advance);
    assert!(
        text.contains("cannot advance past a refused required forge mutation"),
        "{text}"
    );

    let receipt = libagent::protocol_failure::read_receipt(&receipt_path)
        .unwrap()
        .expect("a protocol-failure receipt is written");
    assert_eq!(receipt.protocol, "define");
    assert_eq!(receipt.work_unit.as_deref(), Some(work_unit_id.as_str()));
    assert_eq!(receipt.operation, operation.canonical_name());

    service.cancel().await.unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn define_fails_closed_when_its_claim_mutation_is_refused() {
    assert_define_fails_closed_on_refused(
        Operation::ClaimWorkUnit,
        serde_json::json!({
            "handle": { "id": "github:tesserine/example:issue:9", "display": "tesserine/example#9" }
        }),
    )
    .await;
}

#[tokio::test]
async fn define_fails_closed_when_its_progress_record_mutation_is_refused() {
    assert_define_fails_closed_on_refused(Operation::RecordProgress, record_progress_input()).await;
}
