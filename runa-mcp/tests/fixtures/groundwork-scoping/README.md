# Groundwork scoping compatibility schemas

These immutable fixtures are exact copies of the Groundwork schemas consumed
while planning `tesserine/runa#252` on 2026-07-10:

- `contract.schema.json`
- `test-evidence.schema.json`
- `research-record.schema.json`

They protect Runa's served-tool and persistence compatibility without creating
a runtime or network dependency on a Groundwork checkout. Update them only
when deliberately reviewing compatibility against a newer Groundwork schema.
