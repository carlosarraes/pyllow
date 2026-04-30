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

All commands accept `--format {human,json,sarif,markdown}`. `check`, `dupes`, `health`, `smells`, `flags`, `audit` support `--baseline` / `--save-baseline` / `--save-snapshot` / `--trend` / `--score` / `--ownership` for incremental adoption and CI dashboards.

```bash
# Common recipes
pyllow check . --circular-deps                  # only cycles
pyllow dupes . --mode semantic --skip-local     # find AI rename-paste clones
pyllow health . --top 10                        # 10 most complex functions
pyllow health . --targets --effort low          # quick-win refactor targets
pyllow audit . --base main --format sarif       # CI gate, GitHub Code Scanning
```

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
disabled = ["raise-from-none"]  # FastAPI HTTPException idiom
todo_density_threshold = 5
```

A `.pyllowignore` works alongside it for ignore globs only (one pattern per line, `#` for comments).

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
