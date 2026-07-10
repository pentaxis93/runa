//! The guarded required forge mutation set is consulted from the
//! methodology's procedure authority and the forge capability contract —
//! never enumerated in runa's own enforcement code. This check fails if any
//! canonical forge operation name appears as a string literal in the
//! non-test sources of the enforcing crates, which is what a hand-maintained
//! allowlist or per-site switch would need.

use std::fs;
use std::path::{Path, PathBuf};

use runa_forge_contract::Operation;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate sits inside the workspace")
        .to_path_buf()
}

fn rust_sources(dir: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory is readable") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, sources);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            sources.push(path);
        }
    }
}

#[test]
fn enforcement_crates_enumerate_no_guarded_operation_names() {
    let root = workspace_root();
    let mut sources = Vec::new();
    for crate_src in [
        "runa-mcp/src",
        "runa-cli/src",
        "libagent/src",
        "runa-forge-compose/src",
    ] {
        rust_sources(&root.join(crate_src), &mut sources);
    }
    assert!(
        !sources.is_empty(),
        "the enforcement crates' sources are present"
    );

    let mut violations = Vec::new();
    for path in sources {
        let content = fs::read_to_string(&path).unwrap();
        for operation in Operation::ALL {
            let literal = format!("\"{}\"", operation.canonical_name());
            for (index, line) in content.lines().enumerate() {
                if line.contains(&literal) {
                    violations.push(format!(
                        "{}:{}: string literal {literal}",
                        path.display(),
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "guarded forge operation names are consulted from their authority, \
         never enumerated in enforcement source:\n{}",
        violations.join("\n")
    );
}
