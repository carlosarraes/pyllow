# Machine output and exit codes

Pyllow's `--format json` and `--format sarif` documents, and its exit codes,
are a stable contract for CI integrations. This page defines what you may
depend on and how it changes.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Analysis completed; no blocking findings. |
| `1` | Analysis completed; blocking findings were reported. |
| `2` | Analysis did **not** complete: configuration, git, parsing, I/O, or internal failure. |

A run that could not complete never reports `0` or `1`. This is the property a
policy gate depends on: without it, a broken tool is indistinguishable from a
clean codebase. Usage errors from argument parsing also exit `2`.

## Streams

`--format json` and `--format sarif` write their document to **stdout and
nothing else**. Progress, warnings, verdicts, and other human-facing output go
to **stderr**. Piping stdout into a parser is always safe.

## JSON envelope

```json
{
  "schemaVersion": 1,
  "tool": "pyllow",
  "rules": { "executed": ["unused-file", "mutable-default"], "requested": [] },
  "families": { "executed": ["check", "smells"], "requested": [] },
  "diagnostics": [
    {
      "path": "src/example.py",
      "startLine": 12,
      "endLine": 14,
      "rule": "no-explicit-any",
      "message": "Explicit Any discards type evidence"
    }
  ],
  "issues": [ { "type": "smell", "path": "src/example.py", "line": 12, "…": "…" } ],
  "stats": { "files_scanned": 1, "entry_points": 0, "plugins_run": [], "elapsed_ms": 3 }
}
```

| Field | Guarantee |
| --- | --- |
| `schemaVersion` | Integer. Bumped only for a breaking change (below). |
| `tool` | Always `"pyllow"`. |
| `rules.executed` | Stable rule keys that **ran**, whether or not they produced findings. A rule that ran and found nothing still appears — otherwise "clean" and "disabled" are indistinguishable. |
| `rules.requested` | Rule keys named with `--rule`. Empty when none were. |
| `families` | Present for commands with family selection (`audit`). `families.requested` is what `--only` named (empty = everything); `families.executed` is what ran. |
| `diagnostics[]` | The uniform view of every finding. Prefer this. |
| `issues[]` | The richer variant-tagged view, keyed by `type`. Same findings, more per-family detail. |
| `stats` | Run metadata. Informational; fields may be added. |

> **Casing note.** Envelope-level keys (`schemaVersion`) and `diagnostics`
> fields use camelCase. The `issues[]` and `stats` views predate this contract
> and use snake_case (`files_scanned`, `start_line`). This split is deliberate:
> normalizing them would break existing consumers for no functional gain.
> Within a schema version the casing of any given field never changes.

### Diagnostic fields

- `path` — repository-relative POSIX path, always forward slashes, never absolute.
- `startLine` / `endLine` — **one-based and inclusive**. `null` for file-level
  findings, which own no specific range; they are never faked to line 1.
  Function-scoped findings (`complexity`, `refactor-target`) span the whole
  analyzed body, not just the `def` line.
- `rule` — stable kebab-case rule key. The same key is used by `[[suppress]]`
  entries, baselines, SARIF `ruleId`, and rule selection.
- `message` — human-readable, names the specific symbol or function involved.
  Wording is **not** stable; do not match on it.

## SARIF

SARIF output reports the same findings under the same names at the same
locations as the JSON:

- `ruleId` equals the JSON `rule`.
- `artifactLocation.uri` equals the JSON `path`.
- `region.startLine` / `region.endLine` equal the JSON `startLine` / `endLine`.

SARIF `message` text may differ from the JSON `message`; neither is stable.

## Compatibility policy

**Additive changes do not bump `schemaVersion`.** Consumers must ignore
unknown fields. The following may happen within a schema version:

- new fields on the envelope, a diagnostic, an issue, or `stats`
- new rule keys in `rules.executed` and new `type` values in `issues[]`
- new entries in the SARIF rule catalog
- changes to any `message` wording

**Breaking changes bump `schemaVersion`.** These require a bump:

- removing or renaming a documented field
- changing a field's type or cardinality
- changing the meaning of `startLine`/`endLine` (base or inclusivity)
- changing path resolution away from repository-relative POSIX
- renaming an existing rule key
- changing the meaning of an exit code

Rule keys are part of the contract because suppressions and baselines are
written against them; renaming one is a breaking change even though adding one
is not.
