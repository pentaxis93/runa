//! `runa supersede` — apply a supersession disposition to a recorded
//! protocol execution.
//!
//! The disposition marks a conforming-but-rejected execution result as
//! superseded so the producing protocol regenerates against its unchanged
//! inputs. It is guarded against current reality: the targeted protocol,
//! output identity, and revision must all match what is recorded now, and
//! the judgment must carry an auditable reason. The rejected revision is
//! preserved in the supersession lineage; regeneration itself flows only
//! through the normal `step`/`run`/`go`/session paths.

use std::fmt;
use std::path::Path;

use super::CommandError;

#[derive(Debug)]
pub enum SupersedeCommandError {
    Command(CommandError),
    /// An `--output` token does not parse as `<type>/<instance>@<revision>`.
    MalformedOutput(String),
    /// The named protocol is not declared by the methodology.
    UnknownProtocol(String),
    /// The store rejected the disposition.
    Disposition(libagent::SupersedeError),
}

impl fmt::Display for SupersedeCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SupersedeCommandError::Command(err) => write!(f, "{err}"),
            SupersedeCommandError::MalformedOutput(token) => write!(
                f,
                "malformed --output '{token}': expected <type>/<instance>@<revision>, \
                 for example 'requirements/radicle-native-collaboration@sha256:abc123'"
            ),
            SupersedeCommandError::UnknownProtocol(name) => {
                write!(f, "unknown protocol: '{name}'")
            }
            SupersedeCommandError::Disposition(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SupersedeCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SupersedeCommandError::Command(err) => Some(err),
            SupersedeCommandError::Disposition(err) => Some(err),
            _ => None,
        }
    }
}

impl From<CommandError> for SupersedeCommandError {
    fn from(err: CommandError) -> Self {
        SupersedeCommandError::Command(err)
    }
}

impl SupersedeCommandError {
    /// Exit-code mapping: malformed invocation is a usage error; a
    /// disposition rejected against current reality is a generic failure;
    /// project-load, scan, and store I/O failures are infrastructure.
    pub fn exit_code(&self) -> crate::exit_codes::ExitCode {
        match self {
            SupersedeCommandError::MalformedOutput(_) => crate::exit_codes::ExitCode::UsageError,
            SupersedeCommandError::UnknownProtocol(_) => {
                crate::exit_codes::ExitCode::GenericFailure
            }
            SupersedeCommandError::Disposition(libagent::SupersedeError::Store(_)) => {
                crate::exit_codes::ExitCode::InfrastructureFailure
            }
            SupersedeCommandError::Disposition(_) => crate::exit_codes::ExitCode::GenericFailure,
            SupersedeCommandError::Command(_) => crate::exit_codes::ExitCode::InfrastructureFailure,
        }
    }
}

/// Parse one `--output` token of the form `<type>/<instance>@<revision>`
/// into its `(artifact_type, instance_id, content_hash)` identity.
fn parse_output_token(token: &str) -> Option<(String, String, String)> {
    let (identity, revision) = token.split_once('@')?;
    let (artifact_type, instance_id) = identity.split_once('/')?;
    if artifact_type.is_empty() || instance_id.is_empty() || revision.is_empty() {
        return None;
    }
    Some((
        artifact_type.to_string(),
        instance_id.to_string(),
        revision.to_string(),
    ))
}

pub fn run(
    working_dir: &Path,
    config_override: Option<&Path>,
    protocol: &str,
    work_unit: Option<&str>,
    outputs: &[String],
    reason: &str,
) -> Result<(), SupersedeCommandError> {
    let parsed: Vec<(String, String, String)> = outputs
        .iter()
        .map(|token| {
            parse_output_token(token)
                .ok_or_else(|| SupersedeCommandError::MalformedOutput(token.clone()))
        })
        .collect::<Result<_, _>>()?;

    // Scan first so the revision guard checks current reality, and validate
    // scoped identity exactly as the other scope-taking commands do.
    let (mut loaded, _scan_result) = super::load_and_scan(working_dir, config_override)?;
    super::validate_scoped_work_unit(&loaded, work_unit)?;

    let declaration = loaded
        .manifest
        .protocols
        .iter()
        .find(|candidate| candidate.name == protocol)
        .cloned()
        .ok_or_else(|| SupersedeCommandError::UnknownProtocol(protocol.to_string()))?;

    loaded
        .store
        .supersede_execution(&declaration, work_unit, &parsed, reason)
        .map_err(SupersedeCommandError::Disposition)?;

    println!(
        "Superseded execution of '{protocol}'{}; the protocol regenerates against its unchanged inputs.",
        match work_unit {
            Some(id) => format!(" for work unit '{id}'"),
            None => String::new(),
        }
    );
    for (artifact_type, instance_id, revision) in &parsed {
        println!("  rejected: {artifact_type}/{instance_id}@{revision}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_output_token;

    #[test]
    fn parses_well_formed_output_token() {
        let parsed = parse_output_token("requirements/radicle-native@sha256:abc123").unwrap();
        assert_eq!(
            parsed,
            (
                "requirements".to_string(),
                "radicle-native".to_string(),
                "sha256:abc123".to_string()
            )
        );
    }

    #[test]
    fn rejects_tokens_missing_a_component() {
        for token in [
            "requirements/radicle-native",
            "requirements@sha256:abc",
            "/inst@sha256:abc",
            "type/@sha256:abc",
            "type/inst@",
        ] {
            assert!(parse_output_token(token).is_none(), "accepted '{token}'");
        }
    }

    #[test]
    fn revision_hash_may_contain_further_separators() {
        // `@` splits on the first occurrence; the instance side keeps `/`
        // splitting on the first occurrence so nested-looking instance ids
        // still parse deterministically.
        let parsed = parse_output_token("a/b/c@sha256:x").unwrap();
        assert_eq!(
            parsed,
            ("a".to_string(), "b/c".to_string(), "sha256:x".to_string())
        );
    }
}
