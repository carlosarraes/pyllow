# pyllow

> Rust-native codebase intelligence for Python. Sub-second. Framework-aware. One tool replaces five.

[![release](https://img.shields.io/github/v/release/carlosarraes/pyllow)](https://github.com/carlosarraes/pyllow/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Current version:** `v0.0.6`

## Install

```bash
curl -fsSL https://github.com/carlosarraes/pyllow/releases/download/v0.0.6/install.sh | sh
```

Installs the latest binary for your platform (Linux/macOS x86_64 or aarch64) to `~/.local/bin/pyllow`. Pin a specific version with `PYLLOW_VERSION=v0.0.6`. Windows users can grab the `.zip` directly from the [latest release](https://github.com/carlosarraes/pyllow/releases/latest).

## Commands

```bash
pyllow check                # unused files / imports / deps + circular imports
pyllow dupes                # copy-paste detection (4 modes)
pyllow health               # complexity, maintainability, hotspots, refactor targets
pyllow smells               # 10 Python anti-patterns
pyllow flags                # feature-flag inventory
pyllow audit . --base main  # PR-scoped quality gate (PASS / WARN / FAIL)
pyllow fix --dry-run        # auto-remove unused imports
pyllow init                 # scaffold pyllow.toml
pyllow list                 # inspect detected entry points / files / plugins
pyllow llm                  # agent operating manual (markdown for AI agents)
```

`check`, `dupes`, `health`, `smells`, `flags`, `audit`, `watch` accept `--format {human,json,sarif,markdown}`. `list` accepts `--format {human,json}` (its inventory shape doesn't fit SARIF or markdown). The same six analysis commands support `--baseline` / `--save-baseline` / `--save-snapshot` / `--trend` / `--score` / `--ownership` for incremental adoption and CI dashboards.

```bash
# Common recipes
pyllow check . --circular-deps                  # only cycles
pyllow dupes . --mode semantic --skip-local     # find AI rename-paste clones
pyllow health . --top 10                        # 10 most complex functions
pyllow health . --targets --effort low          # quick-win refactor targets
pyllow audit . --base main --format sarif       # CI gate, GitHub Code Scanning
pyllow audit . --only smells --rule no-explicit-any --rule no-typing-cast
                                                # focused policy gate: one family, named rules
```

`audit --staged` analyzes the **exact staged Git index** — the pre-commit
view. The index is materialized into a temporary snapshot (removed on every
exit path), analysis and config loading read that snapshot, and scope is the
staged diff, so worktree-only edits are invisible and partial staging reports
the lines you will actually commit. Renamed files are analyzed at their
post-image path; staged deletions are skipped; no staged Python changes is an
immediate PASS with no analysis run. The index and worktree are never mutated.
Pair it with selection for a fast pre-commit hook:

```bash
pyllow audit . --staged --only smells
```

`audit --only <family>` (repeatable; `check`, `dupes`, `health`, `smells`) runs
only those families — unselected ones are skipped entirely, not filtered after
the fact. `--rule <key>` (repeatable, requires `--only`) narrows to named rules
within them; configured `[[smells.banned_api]]` IDs count as smell rules.
Unknown rules, rules outside the selected families, and rules the config has
disabled are rejected before scanning (exit 2). Parse errors are always
reported regardless of selection or diff scope. JSON records both what was
requested and what ran under `families` and `rules`.

## Plugins (12)

Framework awareness so route handlers, models, fixtures, and migrations aren't flagged as unused:

| Category | Plugins |
|---|---|
| **Web** | FastAPI, Django |
| **CLI** | Click (and Typer) |
| **Testing** | pytest |
| **Workflow** | Prefect |
| **Data / ORM** | Pydantic, SQLAlchemy, Beanie |
| **Tasks** | Celery |
| **Migrations** | Alembic |
| **Other** | FastMCP, script entry points |

Disable any plugin in `pyllow.toml`:

```toml
[plugins.django]
enabled = false
```

## Configuration

Optional `pyllow.toml` (or `[tool.pyllow]` in `pyproject.toml`):

```toml
entryPoints = ["src/main.py"]
ignorePatterns = ["scripts/**"]

[smells]
enabled = []                    # opt in to rules that ship disabled
disabled = ["raise-from-none"]  # FastAPI HTTPException idiom
todo_density_threshold = 5
```

Rules listed in `enabled` are turned on; rules listed in `disabled` are turned
off. **`disabled` wins** when a rule appears in both, so a shared config can
never force a rule a project cannot adopt. Unknown rule names are rejected when
the config loads, before any analysis runs — a typo fails the run (exit 2)
rather than silently selecting nothing.

Every rule shipped today is on by default except `banned-api`, so adding
`[smells]` to an existing project changes nothing until you name a rule.

### Explicit `Any` (`no-explicit-any`, opt-in)

```toml
[smells]
enabled = ["no-explicit-any"]
```

Flags `typing.Any` wherever an annotation can appear — parameters, returns,
variables, class attributes, `TypeAlias` values, the 3.12 `type X = ...`
statement, generic arguments, and `Callable` signatures — in direct (`Any`),
qualified (`typing.Any`), and aliased (`import typing as t`, `from typing
import Any as Dynamic`) forms. `object`, unions, `Optional`, protocols, and
type variables are never flagged.

**Limitations.** The check is syntactic, so it cannot see through things only a
type checker resolves: an alias chain that crosses modules (`from .types import
Json` where `Json = Any` elsewhere), string annotations (`x: "Any"`), `Any`
smuggled in through generated stubs, or values whose *inferred* type is `Any`.
Use it as a fast gate for the explicit form and keep a type checker for the
rest.

### Banned APIs (`banned-api`, opt-in)

Prohibit specific fully qualified Python APIs without writing a custom linter:

```toml
[smells]
enabled = ["banned-api"]

[[smells.banned_api]]
id = "no-typing-cast"
path = "typing.cast"
message = "Prefer parsing, narrowing, or a named contract."

[[smells.banned_api]]
id = "no-module-patch"
path = "unittest.mock.patch"
message = "Prefer dependency injection or a faithful fake."
```

Each `id` becomes the finding's rule key in JSON, SARIF, baselines, and
`[[suppress]]` entries, so `rules = ["no-typing-cast"]` suppresses exactly that
policy. Direct imports (`from typing import cast`), qualified access
(`typing.cast`), and aliases (`import typing as t`, `from typing import cast as c`)
are all resolved; a project-local `def cast()` or an attribute on an unrelated
object is not matched. Relative imports are left unresolved rather than guessed.
Duplicate IDs, IDs that shadow a built-in rule, malformed paths, and empty
messages are rejected when the config loads.

A `.pyllowignore` works alongside it for ignore globs only (one pattern per line, `#` for comments).

## Framework policy (FastAPI)

With `[plugins.fastapi]` enabled (the default), pyllow understands a few
official FastAPI idioms instead of flagging them:

- `raise HTTPException(...) from None` inside an `except` handler is the
  documented exception-translation idiom and is exempt from
  `raise-from-none`. The exemption is narrow — only an import-resolved
  `fastapi.HTTPException` / `starlette.exceptions.HTTPException`; any other
  `raise ... from None` in the same file is still reported.
- `Depends()` default arguments never trip `mutable-default`.
- Route handlers, `include_router` targets, and dependency functions reached
  only through `Depends(...)` stay reachable in `check`.

Exemptions are never silent: each one appears in `stats.exemptions` in JSON
output and as an `exempt:` line on stderr. Disabling the plugin
(`[plugins.fastapi] enabled = false`) restores framework-agnostic behavior.

## Count baselines (downward ratchet)

Fingerprint baselines (`--baseline`) *hide* known findings. Count baselines are
the strict alternative for cleanup programs — a committed per-rule allowance
that can only go down:

```bash
pyllow smells . --save-count-baseline counts.json   # write exact current counts
pyllow smells . --count-baseline counts.json        # gate against them
pyllow smells . --count-baseline counts.json --count-base main
                                                    # + refuse allowances raised vs merge-base
```

Per rule: more findings than the allowance is a **regression** (fail); fewer is
a **stale** allowance (fail, printing the exact lower value to commit); equal
passes. With `--count-base <ref>`, the file is also compared against its
committed version at `git merge-base HEAD <ref>` — a branch may lower an
allowance, never raise it. The file is versioned JSON
(`{"schemaVersion": 1, "counts": {"broad-except": 40}}`); unknown fields,
booleans, negative counts, and malformed JSON are rejected as operational
errors (exit 2), as are invalid base refs.

## Suppression

Pyllow honors existing Python lint conventions — no new dialect:

```python
foo == None        # noqa: E711
except Exception:  # noqa: BLE001
print("debug")     # noqa: T201
import os          # noqa: F401
```

Cross-tool codes mapped to pyllow rules: `B006`, `BLE001`, `E711`, `E712`, `E722`, `T201`, `T203`, `F401`, `S110`, `S112`. File-level `# ruff: noqa` and `# flake8: noqa` work too.

## For AI agents

Pyllow ships an operating manual designed to be piped into an agent's context:

```bash
pyllow llm > pyllow-guide.md
```

Covers what each command does, how to interpret JSON output, the seven framework-agnostic false-positive classes to verify before acting, and verification recipes per finding type.

## Credits

Pyllow is a parallel project to **[fallow](https://fallow.tools)** ([github.com/fallow-rs/fallow](https://github.com/fallow-rs/fallow)) — the TS/JS codebase-intelligence tool that proved this category of analysis is genuinely useful. Pyllow shares fallow's layered approach (dead code → duplication → health → audit) and adopts some of its UX (`--baseline`, `--ownership`, `--score`, ranked refactor `--targets`), but is built ground-up for Python's import system, framework conventions, and ecosystem.

Differences from fallow:

- **Always free.** No paid runtime tier.
- **Python module model.** PEP 420 namespace packages, dynamic imports, `__init__.py` re-exports.
- **Python-tailored plugin set.** Django / FastAPI / Beanie / SQLAlchemy / Celery / Pydantic etc.

## License

MIT.

## Status

`v0.0.6` — actively developed.
