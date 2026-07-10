# Origin-main scope compatibility fixture

These bytes were produced by an unmodified detached `tesserine/runa`
`origin/main` worktree at commit
`f327be9d51633a5b4a5bfac3a5d92b0187b998da`, before the #252 implementation.

The baseline binary was built with:

```text
CARGO_TARGET_DIR=/tmp/runa-252-origin-main-target cargo build --workspace
```

The methodology and initial workspace were placed at the stable generation
root `/tmp/runa-252-fixture-source`. The baseline runtime then performed:

```text
runa init --methodology /tmp/runa-252-fixture-source/methodology/manifest.toml
runa state --work-unit work-unit-a --json
runa step --work-unit work-unit-a
runa state --work-unit work-unit-a --json
runa state --work-unit work-unit-b --json
```

The live `step` produced `result/result-a` and the historical
`execution-records.json`; every file under the committed `workspace/` and
`store/` directories was copied byte-for-byte after those commands.

The fixture contains:

- `required-record/owner-a`: valid and bound to `work-unit-a`;
- `cross-record/shared`: valid and unscoped;
- `invalid-record/invalid-a`: bound to `work-unit-a` and invalid because its
  title is an integer;
- scoped `request` instances for A and B;
- A's valid `result/result-a`;
- the origin/main execution-record shape for `consume(work-unit-a)`, containing
  only `protocol`, `work_unit`, `input_modes`, and `inputs`.

The compatibility test rehydrates the fixture at its original stable root so
the absolute paths emitted by origin/main remain exact. It snapshots every
workspace and store byte before loading, then checks classification, bindings,
A/B context visibility, execution-record loading, freshness, readiness, scan
results, and the final byte-for-byte snapshot. Configuration and initialization
metadata are created outside the captured state and are not compatibility
claims.
