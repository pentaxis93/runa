use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use libagent::context::build_context;
use libagent::{
    EvaluationScope, ProtocolStatus, ValidationStatus, collect_scan_findings, evaluate_protocols,
    protocol_relevant_inputs_changed, scan,
};

static FIXTURE_LOCK: Mutex<()> = Mutex::new(());
const RUNTIME_ROOT: &str = "/tmp/runa-252-fixture-source";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/origin-main-scope-state")
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, snapshot);
            } else {
                snapshot.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn context_inputs(
    protocol: &libagent::ProtocolDeclaration,
    store: &libagent::ArtifactStore,
    work_unit: &str,
) -> BTreeSet<(String, String)> {
    build_context(protocol, store, Some(work_unit))
        .inputs
        .into_iter()
        .map(|input| (input.artifact_type, input.instance_id))
        .collect()
}

#[test]
fn origin_main_scope_state_loads_evaluates_and_scans_without_migration() {
    let _guard = FIXTURE_LOCK.lock().unwrap();
    let provenance = std::fs::read_to_string(fixture_root().join("README.md")).unwrap();
    assert!(
        provenance.contains("f327be9d51633a5b4a5bfac3a5d92b0187b998da"),
        "fixture must identify the exact unmodified origin/main producer"
    );
    let runtime_root = Path::new(RUNTIME_ROOT);
    if runtime_root.exists() {
        std::fs::remove_dir_all(runtime_root).unwrap();
    }
    copy_tree(&fixture_root(), runtime_root);

    let project = runtime_root.join("project");
    let runa_dir = project.join(".runa");
    std::fs::write(
        runa_dir.join("config.toml"),
        format!("methodology_path = \"{RUNTIME_ROOT}/methodology/manifest.toml\"\n"),
    )
    .unwrap();
    std::fs::write(
        runa_dir.join("state.toml"),
        "initialized_at = \"2026-07-10T18:57:41Z\"\nruna_version = \"0.2.0-rc.1\"\n",
    )
    .unwrap();

    let persisted_root = runa_dir.clone();
    let before = snapshot_tree(&persisted_root);
    let mut loaded = libagent::project::load(&project, None).unwrap();
    let protocol = loaded
        .manifest
        .protocols
        .iter()
        .find(|protocol| protocol.name == "consume")
        .unwrap()
        .clone();

    let owner = loaded.store.get("required-record", "owner-a").unwrap();
    assert!(matches!(owner.status, ValidationStatus::Valid));
    assert_eq!(owner.work_unit.as_deref(), Some("work-unit-a"));
    let cross = loaded.store.get("cross-record", "shared").unwrap();
    assert!(matches!(cross.status, ValidationStatus::Valid));
    assert_eq!(cross.work_unit, None);
    let invalid = loaded.store.get("invalid-record", "invalid-a").unwrap();
    assert!(matches!(invalid.status, ValidationStatus::Invalid(_)));
    assert_eq!(invalid.work_unit.as_deref(), Some("work-unit-a"));

    let owner_context = context_inputs(&protocol, &loaded.store, "work-unit-a");
    assert!(owner_context.contains(&("required-record".into(), "owner-a".into())));
    assert!(owner_context.contains(&("cross-record".into(), "shared".into())));
    assert!(!owner_context.contains(&("invalid-record".into(), "invalid-a".into())));
    let foreign_context = context_inputs(&protocol, &loaded.store, "work-unit-b");
    assert!(!foreign_context.contains(&("required-record".into(), "owner-a".into())));
    assert!(foreign_context.contains(&("cross-record".into(), "shared".into())));
    assert!(!foreign_context.contains(&("invalid-record".into(), "invalid-a".into())));

    let raw_records: serde_json::Value = serde_json::from_slice(
        before
            .get(Path::new("store/execution-records.json"))
            .expect("origin/main execution record fixture"),
    )
    .unwrap();
    assert_eq!(
        raw_records["records"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["input_modes", "inputs", "protocol", "work_unit"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let execution = loaded
        .store
        .execution_record("consume", Some("work-unit-a"))
        .expect("origin/main execution record must load");
    assert_eq!(
        execution.inputs.artifact_types["request"][0].instance_id,
        "request-a"
    );

    let no_scan_findings = libagent::status::ScanFindings {
        affected_types: Default::default(),
        warnings: Vec::new(),
    };
    let owner_state = evaluate_protocols(
        &loaded,
        &project,
        &no_scan_findings,
        EvaluationScope::Scoped("work-unit-a"),
    );
    assert!(owner_state.ready.is_empty());
    assert_eq!(owner_state.waiting.len(), 1);
    assert!(owner_state.waiting[0].status == ProtocolStatus::Waiting);
    assert!(
        owner_state.waiting[0]
            .unsatisfied_conditions
            .iter()
            .any(|condition| condition == "outputs are current")
    );
    let foreign_state = evaluate_protocols(
        &loaded,
        &project,
        &no_scan_findings,
        EvaluationScope::Scoped("work-unit-b"),
    );
    assert_eq!(foreign_state.ready.len(), 1);
    assert!(foreign_state.ready[0].status == ProtocolStatus::Ready);
    assert!(foreign_state.waiting.is_empty());

    let scan_result = scan(&loaded.workspace_dir, &mut loaded.store).unwrap();
    assert!(scan_result.new.is_empty());
    assert!(scan_result.modified.is_empty());
    assert!(scan_result.revalidated.is_empty());
    assert!(scan_result.removed.is_empty());
    assert_eq!(scan_result.invalid.len(), 1);
    assert_eq!(scan_result.invalid[0].artifact_type, "invalid-record");
    assert!(!protocol_relevant_inputs_changed(
        &protocol,
        Some("work-unit-a"),
        &scan_result
    ));
    let findings = collect_scan_findings(&scan_result, &loaded.workspace_dir);
    let owner_after_scan = evaluate_protocols(
        &loaded,
        &project,
        &findings,
        EvaluationScope::Scoped("work-unit-a"),
    );
    assert!(owner_after_scan.ready.is_empty());
    assert_eq!(owner_after_scan.waiting.len(), 1);

    assert_eq!(
        snapshot_tree(&persisted_root),
        before,
        "loading, context construction, readiness/freshness evaluation, execution-record reads, and scan must not normalize or rewrite origin/main bytes"
    );

    std::fs::remove_dir_all(runtime_root).unwrap();
}
