//! Bounded JSON Schema analysis for `work_unit` ownership.
//!
//! This module does not evaluate arbitrary schemas. It answers one question
//! for output artifacts: whether the runtime owns a required `work_unit`, the
//! payload owns an optional one, or the schema does not declare one.

use std::fmt;

use serde_json::Value;

/// The authority responsible for an output artifact's `work_unit` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkUnitOwnership {
    /// The effective schema requires the field; the runtime injects it.
    RuntimeRequired,
    /// The effective schema declares but does not require the field.
    PayloadOptional,
    /// The effective schema does not declare the field.
    Absent,
}

/// A schema form whose `work_unit` ownership cannot be resolved safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnitSchemaError {
    pub schema_path: String,
    pub detail: String,
}

impl fmt::Display for WorkUnitSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.schema_path, self.detail)
    }
}

impl std::error::Error for WorkUnitSchemaError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Viable(WorkUnitOwnership),
    NonViable,
}

/// Determine the single owner of `work_unit` for a served output schema.
pub fn analyze_work_unit_ownership(
    schema: &Value,
) -> Result<WorkUnitOwnership, WorkUnitSchemaError> {
    let mut references = Vec::new();
    match analyze_node(schema, schema, "", &mut references)? {
        Outcome::Viable(ownership) => Ok(ownership),
        Outcome::NonViable => Err(error("", "schema has no viable branch")),
    }
}

fn analyze_node(
    root: &Value,
    schema: &Value,
    path: &str,
    references: &mut Vec<String>,
) -> Result<Outcome, WorkUnitSchemaError> {
    let object = match schema {
        Value::Bool(false) => return Ok(Outcome::NonViable),
        Value::Bool(true) => return Ok(Outcome::Viable(WorkUnitOwnership::Absent)),
        Value::Object(object) => object,
        _ => return Err(error(path, "schema must be an object or boolean")),
    };

    let required = object
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|property| property.as_str() == Some("work_unit"))
        });
    if let Some((keyword, relevant_path)) =
        unsupported_work_unit_effect(root, object, path, references)?
    {
        return Err(error(
            &relevant_path,
            &format!("'{keyword}' can change work_unit ownership, admissibility, or constraints"),
        ));
    }
    if required {
        return Ok(Outcome::Viable(WorkUnitOwnership::RuntimeRequired));
    }

    let property = object
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("work_unit"));

    let mut ownership = if let Some(property_schema) = property {
        ensure_string_property(
            root,
            property_schema,
            &child_path(&child_path(path, "properties"), "work_unit"),
            references,
        )?;
        WorkUnitOwnership::PayloadOptional
    } else {
        WorkUnitOwnership::Absent
    };

    if let Some(reference) = object.get("$ref") {
        let reference = reference
            .as_str()
            .ok_or_else(|| error(&child_path(path, "$ref"), "'$ref' must be a string"))?;
        let target = resolve_local_ref(root, reference, &child_path(path, "$ref"), references)?;
        references.push(reference.to_string());
        let referenced = analyze_node(root, target, reference, references)?;
        references.pop();
        ownership = combine_conjunct(ownership, referenced)?;
    }

    if let Some(all_of) = object.get("allOf") {
        let all_of_path = child_path(path, "allOf");
        for (index, branch) in branches(all_of, &all_of_path)?.iter().enumerate() {
            ownership = combine_conjunct(
                ownership,
                analyze_node(
                    root,
                    branch,
                    &child_path(&all_of_path, &index.to_string()),
                    references,
                )?,
            )?;
        }
    }

    if let Some(any_of) = object.get("anyOf") {
        ownership = analyze_uniform_branches(root, any_of, path, "anyOf", ownership, references)?;
    }
    if let Some(one_of) = object.get("oneOf") {
        ownership = analyze_uniform_branches(root, one_of, path, "oneOf", ownership, references)?;
    }

    Ok(Outcome::Viable(ownership))
}

fn unsupported_work_unit_effect(
    root: &Value,
    object: &serde_json::Map<String, Value>,
    path: &str,
    references: &mut Vec<String>,
) -> Result<Option<(&'static str, String)>, WorkUnitSchemaError> {
    if object.contains_key("if") && (object.contains_key("then") || object.contains_key("else")) {
        for keyword in ["if", "then", "else"] {
            let Some(schema) = object.get(keyword) else {
                continue;
            };
            let keyword_path = child_path(path, keyword);
            if let Some(relevant_path) =
                work_unit_effect_location(root, schema, &keyword_path, references)?
            {
                return Ok(Some((keyword, relevant_path)));
            }
        }
    }

    if let Some(dependencies) = object.get("dependentRequired").and_then(Value::as_object) {
        let dependencies_path = child_path(path, "dependentRequired");
        for (trigger, required) in dependencies {
            let trigger_path = child_path(&dependencies_path, trigger);
            if trigger == "work_unit" {
                return Ok(Some(("dependentRequired", trigger_path)));
            }
            if let Some((index, _)) = required.as_array().and_then(|required| {
                required
                    .iter()
                    .enumerate()
                    .find(|(_, property)| property.as_str() == Some("work_unit"))
            }) {
                return Ok(Some((
                    "dependentRequired",
                    child_path(&trigger_path, &index.to_string()),
                )));
            }
        }
    }

    if let Some(dependencies) = object.get("dependentSchemas").and_then(Value::as_object) {
        let dependencies_path = child_path(path, "dependentSchemas");
        for (trigger, schema) in dependencies {
            let trigger_path = child_path(&dependencies_path, trigger);
            if trigger == "work_unit" {
                return Ok(Some(("dependentSchemas", trigger_path)));
            }
            if let Some(relevant_path) =
                work_unit_effect_location(root, schema, &trigger_path, references)?
            {
                return Ok(Some(("dependentSchemas", relevant_path)));
            }
        }
    }

    if let Some(schema) = object.get("not") {
        let keyword_path = child_path(path, "not");
        if let Some(relevant_path) =
            work_unit_effect_location(root, schema, &keyword_path, references)?
        {
            return Ok(Some(("not", relevant_path)));
        }
    }

    Ok(None)
}

fn work_unit_effect_location(
    root: &Value,
    schema: &Value,
    path: &str,
    references: &mut Vec<String>,
) -> Result<Option<String>, WorkUnitSchemaError> {
    let object = match schema {
        Value::Bool(_) => return Ok(None),
        Value::Object(object) => object,
        _ => return Err(error(path, "schema must be an object or boolean")),
    };

    if let Some((index, _)) =
        object
            .get("required")
            .and_then(Value::as_array)
            .and_then(|required| {
                required
                    .iter()
                    .enumerate()
                    .find(|(_, property)| property.as_str() == Some("work_unit"))
            })
    {
        return Ok(Some(child_path(
            &child_path(path, "required"),
            &index.to_string(),
        )));
    }
    if object
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key("work_unit"))
    {
        return Ok(Some(child_path(
            &child_path(path, "properties"),
            "work_unit",
        )));
    }

    if let Some(reference) = object.get("$ref") {
        let reference_path = child_path(path, "$ref");
        let reference = reference
            .as_str()
            .ok_or_else(|| error(&reference_path, "'$ref' must be a string"))?;
        let target = resolve_local_ref(root, reference, &reference_path, references)?;
        references.push(reference.to_string());
        let effect = work_unit_effect_location(root, target, reference, references);
        references.pop();
        if effect?.is_some() {
            return Ok(Some(reference_path));
        }
    }

    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(value) = object.get(keyword) {
            let composition_path = child_path(path, keyword);
            for (index, branch) in branches(value, &composition_path)?.iter().enumerate() {
                if let Some(relevant_path) = work_unit_effect_location(
                    root,
                    branch,
                    &child_path(&composition_path, &index.to_string()),
                    references,
                )? {
                    return Ok(Some(relevant_path));
                }
            }
        }
    }

    if let Some((_, relevant_path)) = unsupported_work_unit_effect(root, object, path, references)?
    {
        return Ok(Some(relevant_path));
    }

    Ok(None)
}

fn ensure_string_property(
    root: &Value,
    schema: &Value,
    path: &str,
    references: &mut Vec<String>,
) -> Result<(), WorkUnitSchemaError> {
    if property_guarantees_string(root, schema, path, references)? {
        Ok(())
    } else {
        Err(error(
            path,
            "work_unit property does not guarantee a string",
        ))
    }
}

fn property_guarantees_string(
    root: &Value,
    schema: &Value,
    path: &str,
    references: &mut Vec<String>,
) -> Result<bool, WorkUnitSchemaError> {
    let object = match schema {
        Value::Bool(false) => return Err(error(path, "work_unit property is not viable")),
        Value::Bool(true) => return Ok(false),
        Value::Object(object) => object,
        _ => return Err(error(path, "property schema must be an object or boolean")),
    };

    for keyword in ["if", "then", "else", "dependentSchemas", "not"] {
        if object.contains_key(keyword) {
            return Err(error(
                &child_path(path, keyword),
                &format!("'{keyword}' can make work_unit shape conditional"),
            ));
        }
    }

    let mut guarantees_string = object.get("type").is_some_and(|kind| match kind {
        Value::String(kind) => kind == "string",
        Value::Array(kinds) => {
            !kinds.is_empty() && kinds.iter().all(|kind| kind.as_str() == Some("string"))
        }
        _ => false,
    });

    if let Some(reference) = object.get("$ref") {
        let reference = reference
            .as_str()
            .ok_or_else(|| error(&child_path(path, "$ref"), "'$ref' must be a string"))?;
        let target = resolve_local_ref(root, reference, &child_path(path, "$ref"), references)?;
        references.push(reference.to_string());
        let result = property_guarantees_string(root, target, reference, references);
        references.pop();
        guarantees_string |= result?;
    }

    if let Some(all_of) = object.get("allOf") {
        let all_of_path = child_path(path, "allOf");
        for (index, branch) in branches(all_of, &all_of_path)?.iter().enumerate() {
            guarantees_string |= property_guarantees_string(
                root,
                branch,
                &child_path(&all_of_path, &index.to_string()),
                references,
            )?;
        }
    }
    if let Some(any_of) = object.get("anyOf") {
        guarantees_string |=
            uniform_branches_guarantee_string(root, any_of, path, "anyOf", references)?;
    }
    if let Some(one_of) = object.get("oneOf") {
        guarantees_string |=
            uniform_branches_guarantee_string(root, one_of, path, "oneOf", references)?;
    }

    Ok(guarantees_string)
}

fn uniform_branches_guarantee_string(
    root: &Value,
    value: &Value,
    path: &str,
    keyword: &str,
    references: &mut Vec<String>,
) -> Result<bool, WorkUnitSchemaError> {
    let composition_path = child_path(path, keyword);
    let mut viable = 0;
    for (index, branch) in branches(value, &composition_path)?.iter().enumerate() {
        if branch == &Value::Bool(false) {
            continue;
        }
        viable += 1;
        if !property_guarantees_string(
            root,
            branch,
            &child_path(&composition_path, &index.to_string()),
            references,
        )? {
            return Ok(false);
        }
    }
    if viable == 0 {
        return Err(error(&composition_path, "no viable branches"));
    }
    Ok(true)
}

fn resolve_local_ref<'a>(
    root: &'a Value,
    reference: &str,
    path: &str,
    references: &[String],
) -> Result<&'a Value, WorkUnitSchemaError> {
    if !reference.starts_with('#') {
        return Err(error(path, "external references are not supported"));
    }
    if references.iter().any(|seen| seen == reference) {
        return Err(error(
            path,
            &format!("reference cycle through '{reference}'"),
        ));
    }
    let pointer = reference.strip_prefix('#').unwrap_or_default();
    root.pointer(pointer)
        .ok_or_else(|| error(path, &format!("unresolved local reference '{reference}'")))
}

fn branches<'a>(value: &'a Value, path: &str) -> Result<&'a [Value], WorkUnitSchemaError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| error(path, "composition keyword must contain an array"))
}

fn analyze_uniform_branches(
    root: &Value,
    value: &Value,
    path: &str,
    keyword: &str,
    base: WorkUnitOwnership,
    references: &mut Vec<String>,
) -> Result<WorkUnitOwnership, WorkUnitSchemaError> {
    let composition_path = child_path(path, keyword);
    let mut resolved = None;
    for (index, branch) in branches(value, &composition_path)?.iter().enumerate() {
        let branch = analyze_node(
            root,
            branch,
            &child_path(&composition_path, &index.to_string()),
            references,
        )?;
        if branch == Outcome::NonViable {
            continue;
        }
        let branch_ownership = combine_conjunct(base, branch)?;
        match resolved {
            None => resolved = Some(branch_ownership),
            Some(existing) if existing == branch_ownership => {}
            Some(_) => {
                return Err(error(
                    &composition_path,
                    &format!("'{keyword}' branches disagree about work_unit ownership"),
                ));
            }
        }
    }
    resolved.ok_or_else(|| error(&composition_path, "no viable branches"))
}

fn combine_conjunct(
    current: WorkUnitOwnership,
    conjunct: Outcome,
) -> Result<WorkUnitOwnership, WorkUnitSchemaError> {
    let Outcome::Viable(conjunct) = conjunct else {
        return Err(error("", "schema has a false conjunct"));
    };
    Ok(match (current, conjunct) {
        (WorkUnitOwnership::RuntimeRequired, _) | (_, WorkUnitOwnership::RuntimeRequired) => {
            WorkUnitOwnership::RuntimeRequired
        }
        (WorkUnitOwnership::PayloadOptional, _) | (_, WorkUnitOwnership::PayloadOptional) => {
            WorkUnitOwnership::PayloadOptional
        }
        _ => WorkUnitOwnership::Absent,
    })
}

fn child_path(path: &str, token: &str) -> String {
    format!("{path}/{}", token.replace('~', "~0").replace('/', "~1"))
}

fn error(path: &str, detail: &str) -> WorkUnitSchemaError {
    WorkUnitSchemaError {
        schema_path: if path.is_empty() {
            "/".into()
        } else {
            path.into()
        },
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_of_is_runtime_owned_when_any_conjunct_requires_work_unit() {
        let schema = json!({
            "allOf": [
                {
                    "type": "object",
                    "properties": { "title": { "type": "string" } }
                },
                {
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } },
                    "required": ["work_unit"]
                }
            ]
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::RuntimeRequired
        );
    }

    #[test]
    fn any_of_uses_uniform_viable_branch_ownership() {
        let optional = json!({
            "type": "object",
            "properties": { "work_unit": { "type": "string" } }
        });
        let schema = json!({ "anyOf": [false, optional.clone(), optional] });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn one_of_uses_uniform_viable_branch_ownership() {
        let required = json!({
            "type": "object",
            "required": ["work_unit"]
        });
        let schema = json!({ "oneOf": [required.clone(), false, required] });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::RuntimeRequired
        );
    }

    #[test]
    fn direct_absent_schema_has_no_scope_owner() {
        let schema = json!({
            "type": "object",
            "properties": { "title": { "type": "string" } }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::Absent
        );
    }

    #[test]
    fn direct_requiredness_is_authoritative_over_conditional_siblings() {
        let schema = json!({
            "type": "object",
            "required": ["work_unit"],
            "if": { "properties": { "kind": { "const": "special" } } },
            "then": { "required": ["extra"] }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::RuntimeRequired
        );
    }

    #[test]
    fn local_ref_honors_required_sibling_semantics() {
        let schema = json!({
            "$defs": {
                "optional": {
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } }
                }
            },
            "$ref": "#/$defs/optional",
            "required": ["work_unit"]
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::RuntimeRequired
        );
    }

    #[test]
    fn optional_work_unit_may_use_a_local_string_ref() {
        let schema = json!({
            "$defs": { "scope": { "type": "string" } },
            "type": "object",
            "properties": { "work_unit": { "$ref": "#/$defs/scope" } }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn optional_work_unit_property_all_of_may_guarantee_string_shape() {
        let schema = json!({
            "type": "object",
            "properties": {
                "work_unit": {
                    "allOf": [
                        { "type": "string" },
                        { "pattern": "^[a-z]+$" }
                    ]
                }
            }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn optional_work_unit_property_any_of_requires_every_viable_branch_to_be_string() {
        let schema = json!({
            "type": "object",
            "properties": {
                "work_unit": {
                    "anyOf": [false, { "type": "string" }, { "type": "string" }]
                }
            }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn optional_work_unit_property_one_of_requires_every_viable_branch_to_be_string() {
        let schema = json!({
            "type": "object",
            "properties": {
                "work_unit": {
                    "oneOf": [{ "type": "string" }, false, { "type": "string" }]
                }
            }
        });

        assert_eq!(
            analyze_work_unit_ownership(&schema).unwrap(),
            WorkUnitOwnership::PayloadOptional
        );
    }

    #[test]
    fn mixed_any_of_ownership_fails_closed_at_the_composition() {
        let schema = json!({
            "anyOf": [
                { "type": "object" },
                {
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } }
                }
            ]
        });

        let error = analyze_work_unit_ownership(&schema).unwrap_err();
        assert_eq!(error.schema_path, "/anyOf");
        assert!(error.detail.contains("disagree"));
    }

    #[test]
    fn mixed_one_of_ownership_fails_closed_at_the_composition() {
        let schema = json!({
            "oneOf": [
                { "type": "object", "required": ["work_unit"] },
                {
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } }
                }
            ]
        });

        let error = analyze_work_unit_ownership(&schema).unwrap_err();
        assert_eq!(error.schema_path, "/oneOf");
        assert!(error.detail.contains("disagree"));
    }

    #[test]
    fn unresolved_external_and_cyclic_refs_fail_closed() {
        let cases = [
            (json!({ "$ref": "#/$defs/missing" }), "unresolved"),
            (
                json!({ "$ref": "https://example.invalid/scope" }),
                "external",
            ),
            (
                json!({
                    "$defs": { "cycle": { "$ref": "#/$defs/cycle" } },
                    "$ref": "#/$defs/cycle"
                }),
                "cycle",
            ),
        ];

        for (schema, expected) in cases {
            let error = analyze_work_unit_ownership(&schema).unwrap_err();
            assert!(error.detail.contains(expected), "{error}");
            assert!(error.schema_path.contains("$ref"));
        }
    }

    #[test]
    fn ownership_relevant_conditional_dependency_and_negation_forms_fail_closed() {
        let cases = [
            (
                "if",
                json!({
                    "if": { "properties": { "work_unit": { "const": "work-unit-a" } } },
                    "then": { "required": ["summary"] }
                }),
                "/if/properties/work_unit",
            ),
            (
                "then",
                json!({
                    "if": { "required": ["kind"] },
                    "then": { "required": ["work_unit"] }
                }),
                "/then/required/0",
            ),
            (
                "else",
                json!({
                    "if": { "required": ["kind"] },
                    "else": { "properties": { "work_unit": { "pattern": "^work-unit-" } } }
                }),
                "/else/properties/work_unit",
            ),
            (
                "dependentRequired",
                json!({ "dependentRequired": { "kind": ["work_unit"] } }),
                "/dependentRequired/kind/0",
            ),
            (
                "dependentRequired",
                json!({ "dependentRequired": { "work_unit": ["kind"] } }),
                "/dependentRequired/work_unit",
            ),
            (
                "dependentSchemas",
                json!({
                    "dependentSchemas": {
                        "kind": { "properties": { "work_unit": { "type": "string" } } }
                    }
                }),
                "/dependentSchemas/kind/properties/work_unit",
            ),
            (
                "dependentSchemas",
                json!({ "dependentSchemas": { "work_unit": { "required": ["kind"] } } }),
                "/dependentSchemas/work_unit",
            ),
            (
                "not",
                json!({ "not": { "properties": { "work_unit": { "const": "forbidden" } } } }),
                "/not/properties/work_unit",
            ),
        ];

        for (keyword, schema, expected_path) in cases {
            let error = analyze_work_unit_ownership(&schema).unwrap_err();
            assert_eq!(error.schema_path, expected_path, "{keyword}: {error}");
            assert!(error.detail.contains(keyword), "{error}");
        }
    }

    #[test]
    fn unresolved_or_non_string_optional_property_fails_closed() {
        let cases = [
            json!({
                "type": "object",
                "properties": { "work_unit": { "$ref": "#/$defs/missing" } }
            }),
            json!({
                "type": "object",
                "properties": { "work_unit": { "type": "integer" } }
            }),
            json!({
                "type": "object",
                "properties": { "work_unit": true }
            }),
        ];

        for schema in cases {
            let error = analyze_work_unit_ownership(&schema).unwrap_err();
            assert!(error.schema_path.contains("work_unit"));
        }
    }

    #[test]
    fn optional_property_composition_does_not_hide_an_external_reference() {
        let schema = json!({
            "type": "object",
            "properties": {
                "work_unit": {
                    "allOf": [
                        { "type": "string" },
                        { "$ref": "https://example.invalid/scope" }
                    ]
                }
            }
        });

        let error = analyze_work_unit_ownership(&schema).unwrap_err();
        assert!(error.detail.contains("external"), "{error}");
        assert!(error.schema_path.contains("$ref"), "{error}");
    }

    #[test]
    fn unrelated_conditional_and_dependency_constructs_preserve_direct_ownership() {
        let unrelated = [
            (
                "if/then/else",
                json!({
                    "if": { "properties": { "kind": { "const": "special" } } },
                    "then": { "required": ["summary"] },
                    "else": { "properties": { "summary": { "minLength": 1 } } }
                }),
            ),
            (
                "dependentRequired",
                json!({ "dependentRequired": { "title": ["summary"] } }),
            ),
            (
                "dependentSchemas",
                json!({ "dependentSchemas": { "title": { "required": ["summary"] } } }),
            ),
            ("not", json!({ "not": { "required": ["forbidden"] } })),
        ];
        let bases = [
            (
                json!({
                    "type": "object",
                    "properties": { "title": { "type": "string" } }
                }),
                WorkUnitOwnership::Absent,
            ),
            (
                json!({
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } }
                }),
                WorkUnitOwnership::PayloadOptional,
            ),
            (
                json!({
                    "type": "object",
                    "properties": { "work_unit": { "type": "string" } },
                    "required": ["work_unit"]
                }),
                WorkUnitOwnership::RuntimeRequired,
            ),
        ];

        for (base, expected) in bases {
            for (label, fragment) in &unrelated {
                let mut schema = base.clone();
                schema.as_object_mut().unwrap().extend(
                    fragment
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                assert_eq!(
                    analyze_work_unit_ownership(&schema).unwrap(),
                    expected,
                    "unrelated {label} changed ownership for {schema}"
                );
            }
        }
    }
}
