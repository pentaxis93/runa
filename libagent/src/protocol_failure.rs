//! Protocol-failure receipt: the seam between the MCP delivery surface and
//! the CLI's protocol classification.
//!
//! When a protocol's required forge mutation is refused, `runa-mcp` records
//! the refusal here (at the path named by [`PROTOCOL_FAILURE_RECEIPT_ENV`]),
//! and the CLI that spawned the agent reads it after the agent exits to
//! classify the protocol run as failed — regardless of the agent process's
//! own exit status, which is recorded as it occurred.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment variable naming the file where a protocol-failure receipt is
/// written. Injected by the CLI into the MCP server environment for each
/// agent run; absent in ad-hoc invocations, where the in-process enforcement
/// still blocks artifact delivery and session advance.
pub const PROTOCOL_FAILURE_RECEIPT_ENV: &str = "RUNA_PROTOCOL_FAILURE_RECEIPT";

/// The recorded refusal of a required forge mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolFailureReceipt {
    /// Protocol whose declared procedure required the refused mutation.
    pub protocol: String,
    /// Work unit in scope, when the protocol ran scoped.
    pub work_unit: Option<String>,
    /// Canonical forge operation name that was refused.
    pub operation: String,
    /// Exposed MCP tool name through which the operation was invoked.
    pub tool: String,
    /// The connector or transport error, verbatim.
    pub cause: String,
}

impl ProtocolFailureReceipt {
    /// One-line operator-facing description naming the operation and cause.
    pub fn describe(&self) -> String {
        format!(
            "required forge mutation refused: {}: {}",
            self.operation, self.cause
        )
    }
}

/// Write the receipt to the path named by [`PROTOCOL_FAILURE_RECEIPT_ENV`],
/// when set. The first refusal wins: an existing receipt is left in place.
pub fn write_receipt_from_env(receipt: &ProtocolFailureReceipt) -> std::io::Result<()> {
    let Some(path) = std::env::var_os(PROTOCOL_FAILURE_RECEIPT_ENV) else {
        return Ok(());
    };
    write_receipt(Path::new(&path), receipt)
}

/// Write the receipt to `path`. The first refusal wins: an existing receipt
/// is left in place.
pub fn write_receipt(path: &Path, receipt: &ProtocolFailureReceipt) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(receipt).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Read a receipt if one was written at `path`; `Ok(None)` when absent.
pub fn read_receipt(path: &Path) -> std::io::Result<Option<ProtocolFailureReceipt>> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let receipt = serde_json::from_slice(&content).map_err(std::io::Error::other)?;
    Ok(Some(receipt))
}

/// The conventional receipt file location for an agent run rooted at `dir`.
pub fn receipt_path_in(dir: &Path) -> PathBuf {
    dir.join("protocol-failure.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> ProtocolFailureReceipt {
        ProtocolFailureReceipt {
            protocol: "decompose".into(),
            work_unit: None,
            operation: "op-under-test".into(),
            tool: "op-under-test".into(),
            cause: "transport error: provider returned 403".into(),
        }
    }

    #[test]
    fn receipt_round_trips_and_first_refusal_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = receipt_path_in(dir.path());
        assert_eq!(read_receipt(&path).unwrap(), None);

        let first = receipt();
        write_receipt(&path, &first).unwrap();
        let mut second = receipt();
        second.cause = "a later refusal".into();
        write_receipt(&path, &second).unwrap();

        assert_eq!(read_receipt(&path).unwrap(), Some(first));
    }

    #[test]
    fn describe_names_operation_and_cause() {
        let text = receipt().describe();
        assert!(text.contains("op-under-test"), "{text}");
        assert!(text.contains("403"), "{text}");
    }
}
