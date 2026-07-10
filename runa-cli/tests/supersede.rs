//! End-to-end coverage for `runa supersede`: the planning case from
//! tesserine/runa#248 driven through the real CLI — execution and
//! regeneration flow only through the normal `runa run` path, and the
//! disposition's guards reject each mismatch with an accurate diagnostic.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn runa_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_runa"))
}

fn planning_manifest_toml() -> &'static str {
    r#"
name = "planning"

[[artifact_types]]
name = "intent"

[[artifact_types]]
name = "requirements"

[[artifact_types]]
name = "work-units"

[[protocols]]
name = "survey"
requires = ["intent"]
produces = ["requirements"]
trigger = { type = "on_artifact", name = "intent" }

[[protocols]]
name = "decompose"
requires = ["requirements"]
produces = ["work-units"]
trigger = { type = "on_artifact", name = "requirements" }
"#
}

fn planning_schemas() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "intent",
            r#"{"type":"object","required":["title"],"properties":{"title":{"type":"string"}}}"#,
        ),
        (
            "requirements",
            r#"{"type":"object","required":["title"],"properties":{"title":{"type":"string"}}}"#,
        ),
        (
            "work-units",
            r#"{"type":"object","required":["title"],"properties":{"title":{"type":"string"}}}"#,
        ),
    ]
}

fn init_project(project_dir: &Path, manifest_path: &Path) {
    let output = runa_bin()
        .arg("init")
        .arg("--methodology")
        .arg(manifest_path)
        .current_dir(project_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Fake agent: produces `requirements` from `survey` (content chosen by a
/// version file so regeneration can differ) and `work-units` from
/// `decompose`.
fn write_fake_agent(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("fake-agent.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         payload=$(cat)\n\
         case \"$payload\" in\n\
           *\"# Protocol: survey\"*)\n\
             version=$(cat requirements-version.txt)\n\
             mkdir -p .runa/workspace/requirements\n\
             printf '%s\\n' \"{\\\"title\\\":\\\"requirements $version\\\"}\" \
               > .runa/workspace/requirements/radicle-native.json\n\
             ;;\n\
           *\"# Protocol: decompose\"*)\n\
             mkdir -p .runa/workspace/work-units\n\
             printf '%s\\n' '{\"title\":\"decomposed\"}' \
               > .runa/workspace/work-units/plan.json\n\
             ;;\n\
           *)\n\
             exit 19\n\
             ;;\n\
         esac\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn run_cascade(project_dir: &Path, agent: &Path) -> std::process::Output {
    runa_bin()
        .arg("run")
        .arg("--agent-command")
        .arg("--")
        .arg("sh")
        .arg(agent)
        .current_dir(project_dir)
        .output()
        .unwrap()
}

fn recorded_content_hash(project_dir: &Path, artifact_type: &str, instance_id: &str) -> String {
    let state_path = project_dir
        .join(".runa/store")
        .join(artifact_type)
        .join(format!("{instance_id}.json"));
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(state_path).unwrap()).unwrap();
    state["content_hash"].as_str().unwrap().to_string()
}

#[test]
fn supersede_regenerates_the_planning_case_through_the_normal_run_path() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = common::write_methodology(
        dir.path(),
        planning_manifest_toml(),
        planning_schemas(),
        &["survey", "decompose"],
    );

    let project_dir = dir.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    init_project(&project_dir, &manifest_path);
    let agent = write_fake_agent(dir.path());

    // Seed intent; the fake agent's requirements content is versioned so the
    // regenerated revision differs while the intent stays byte-identical.
    let workspace = project_dir.join(".runa/workspace");
    fs::create_dir_all(workspace.join("intent")).unwrap();
    fs::write(
        workspace.join("intent/replace-forge-connectors.json"),
        r#"{"title":"replace forge connectors with native radicle"}"#,
    )
    .unwrap();
    fs::write(project_dir.join("requirements-version.txt"), "v1").unwrap();

    // The pipeline runs to quiescence through the normal path.
    let output = run_cascade(&project_dir, &agent);
    assert!(
        output.status.success(),
        "first cascade failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Suppressed: outputs are current, nothing is ready.
    let output = run_cascade(&project_dir, &agent);
    assert_eq!(
        output.status.code(),
        Some(4),
        "expected outputs-current quiescence: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let rejected_hash = recorded_content_hash(&project_dir, "requirements", "radicle-native");

    // Guard: a stale revision is rejected with a diagnostic naming the
    // current one, and the wrong protocol is rejected by name.
    let output = runa_bin()
        .args([
            "supersede",
            "--protocol",
            "survey",
            "--output",
            "requirements/radicle-native@sha256:stale",
            "--reason",
            "defective",
        ])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not the current revision") && stderr.contains(&rejected_hash),
        "stale-revision diagnostic must name the current revision: {stderr}"
    );

    let output = runa_bin()
        .args([
            "supersede",
            "--protocol",
            "missing",
            "--output",
            &format!("requirements/radicle-native@{rejected_hash}"),
            "--reason",
            "defective",
        ])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown protocol: 'missing'"),
        "unknown-protocol diagnostic: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = runa_bin()
        .args([
            "supersede",
            "--protocol",
            "survey",
            "--output",
            "requirements-radicle-native",
            "--reason",
            "defective",
        ])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "malformed token is usage");

    // The disposition itself, targeting the exact current revision.
    let output = runa_bin()
        .args([
            "supersede",
            "--protocol",
            "survey",
            "--output",
            &format!("requirements/radicle-native@{rejected_hash}"),
            "--reason",
            "governance found the requirements substantively defective",
        ])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "supersede failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The producer is READY against the unchanged intent, and downstream
    // state derived from the rejected output is no longer current.
    let output = runa_bin()
        .args(["state", "--json"])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let status_of = |name: &str| {
        state["protocols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status_of("survey"), "ready");
    assert_eq!(status_of("decompose"), "ready");

    // Regeneration flows only through the normal run path and records the
    // new execution; the corrected revision reopens and completes downstream.
    fs::write(project_dir.join("requirements-version.txt"), "v2").unwrap();
    let output = run_cascade(&project_dir, &agent);
    assert!(
        output.status.success(),
        "regeneration cascade failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let regenerated_hash = recorded_content_hash(&project_dir, "requirements", "radicle-native");
    assert_ne!(regenerated_hash, rejected_hash);

    let output = run_cascade(&project_dir, &agent);
    assert_eq!(output.status.code(), Some(4), "pipeline current again");

    // The rejected execution and output remain inspectable as lineage.
    let records: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_dir.join(".runa/store/execution-records.json")).unwrap(),
    )
    .unwrap();
    let supersessions = records["supersessions"].as_array().unwrap();
    assert_eq!(supersessions.len(), 1);
    assert_eq!(supersessions[0]["protocol"], "survey");
    assert_eq!(
        supersessions[0]["reason"],
        "governance found the requirements substantively defective"
    );
    assert_eq!(
        supersessions[0]["rejected_outputs"][0]["content_hash"],
        serde_json::Value::String(rejected_hash),
    );
    assert_eq!(
        supersessions[0]["rejected_outputs"][0]["content"]["title"],
        "requirements v1"
    );
}
