# pyllow

> Rust-native codebase intelligence for Python. Sub-second. Framework-aware. One tool replaces five.

**Status:** v0.0.1 in development — FastAPI-first MVP. The roadmap below tracks the full vision through v1.0+ feature parity with [fallow](https://fallow.tools) (the TS/JS tool that inspired pyllow).

---

## Installation

```bash
curl -fsSL https://github.com/carlosarraes/pyllow/releases/download/v0.0.1/install.sh | sh
```

Installs the latest binary for your platform (Linux/macOS x86_64 or aarch64) to `~/.local/bin/pyllow`. Pin a specific version with `PYLLOW_VERSION=v0.0.1`. Windows users can grab the `.zip` directly from the [latest release](https://github.com/carlosarraes/pyllow/releases/latest).

```bash
pyllow check                  # human table; exits 1 if anything is flagged
pyllow check --format json    # machine-readable
```

Drop a `.pyllowignore` at your project root to skip directories pyllow doesn't yet have plugin coverage for (one glob pattern per line, `#` for comments):

```
# .pyllowignore
scripts/**
tests/**
docs/**
```

Patterns are appended to the built-in ignore list (`.venv/`, `__pycache__/`, etc.) and combine with `ignorePatterns` from `pyllow.toml` if present.

---

## Why pyllow exists

Python's static-analysis ecosystem is fragmented. To approximate what fallow gives a TypeScript codebase in one command, a Python team today stitches together:

| Tool | Covers | Misses |
|------|--------|--------|
| `vulture` | Dead code (functions, classes, vars) | Framework conventions; high false-positive rate; no cross-module export graph |
| `ruff` (F-rules) | Single-file unused imports/vars | Cross-module dead exports; framework awareness |
| `radon` / `xenon` / `mccabe` | Cyclomatic complexity | No cognitive complexity, no CRAP score, no maintainability index |
| `pylint` (R0801) | Weak duplication detection | Slow; coarse token-level dedup; no semantic mode |
| `import-linter` | Architecture boundaries | Static imports only; framework-blind |
| `pydeps` | Dependency graphs | No dead-code inference |

Five tools, five configs, five output formats, no shared graph. **No tool today combines cross-module dead-code + framework-aware entry points + duplication + architecture + complexity in one fast pass.** That is pyllow.

---

## MVP — v0.0.1 focus

To prove the thesis quickly, v0.0.1 ships a sharp, FastAPI-first wedge. Anything not on this list is **not** in v0.0.1.

### v0.0.1 includes
- **One command**: `pyllow check`
- **Four analyses**:
  - Unused files
  - Unused top-level names (functions, classes, constants)
  - Unused imports (single-file, ruff F401-style)
  - Unused dependencies (vs `pyproject.toml`)
- **One framework plugin**: FastAPI
  - Route decorators (`@app.{get,post,put,patch,delete,head,options,websocket}`)
  - Routers + `app.include_router(...)` chains
  - `Depends(func)` dependency injection
  - Pydantic body/response models referenced in path-op annotations
  - `app.add_middleware(...)`, `lifespan`, `@app.exception_handler(...)`, `BackgroundTasks`
- **Two output formats**: human (colored table) and JSON
- **Config**: minimal `pyllow.toml`
- **Distribution**: `cargo install pyllow`, `pip install pyllow` (maturin wheel), GitHub release binaries

### v0.0.1 explicitly excludes
Dupes, health, flags, audit, fix, watch, baseline, migrate, CI templates, list, LSP, MCP, SARIF, editor extensions, all framework plugins except FastAPI, runtime coverage, external plugins, ownership grouping.

### v0.0.1 ship criteria
1. Runs against `tiangolo/full-stack-fastapi-template` with **zero false positives on route handlers** (where `vulture` flags many)
2. Sub-2s analysis on a 10k-line FastAPI codebase
3. Cross-module unused-export detection works through `__init__.py` re-export chains
4. Wins at least 3 categories of findings against `vulture` on real-world repos

---

## Full vision (v1.0+)

Everything below is the destination, not the MVP. Each piece is added in a labelled phase (see [Roadmap](#roadmap)).

### Analyses
- **Dead code**: unused files, top-level names, class methods, class attributes, enum members, imports, dependencies, unresolved imports
- **Architecture**: circular imports, boundary violations between configurable zones, stale suppressions
- **Health**: cyclomatic complexity, cognitive complexity, CRAP score (when coverage is available), maintainability index, file-level scores, function hotspots (complexity × git churn), coverage gaps, refactoring targets ranked by impact
- **Duplication**: token-based clone detection in 4 modes — strict (exact), mild (whitespace-normalized), weak (comment-stripped), semantic (type-stripped)
- **Feature flags**: detect `os.environ.get("FEATURE_*")`, Django `settings.FEATURES`, LaunchDarkly / Statsig / Unleash / GrowthBook SDK calls; cross-reference with dead-code findings to surface stale flags
- **Audit mode**: PR-scoped check (changed files since base branch) with pass/warn/fail verdicts and per-analysis baselines

### Framework plugins (target ~30 at v1.0)
- **Web**: Django, Flask, FastAPI, Starlette, Pyramid, Tornado, Sanic, Quart, Litestar
- **Testing**: pytest (fixtures, conftest, plugin entry points), unittest, hypothesis, tox
- **ORM / Migrations**: SQLAlchemy, Django ORM, Tortoise, Peewee, SQLModel, Alembic, Django migrations
- **Validation / Settings**: Pydantic, msgspec, attrs, dataclasses, marshmallow, pydantic-settings, dynaconf, hydra
- **Task queues / Schedulers**: Celery, RQ, Dramatiq, Huey, ARQ, APScheduler
- **Workflow / Pipeline**: Airflow, Prefect, Dagster, Luigi
- **CLI / TUI**: Click, Typer, fire, argparse, Textual
- **Web UI**: Streamlit, Gradio, Dash, Reflex, NiceGUI
- **Data / ML**: Pandas, Polars, scikit-learn, PyTorch (Lightning), Hugging Face transformers
- **GraphQL**: Strawberry, Graphene, Ariadne
- **Build / Packaging**: setuptools, hatchling, poetry, uv, flit, pdm, scikit-build
- **Documentation**: Sphinx, MkDocs (autodoc references)

Each plugin teaches the analyzer about implicit usage patterns the import graph would otherwise miss — decorator routes, string-referenced views, autodiscovered tasks, dependency-injection containers, fixture name resolution, model auto-registration.

### CLI commands (v1.0 surface)
| Command | Purpose |
|---------|---------|
| `pyllow check` | Combined dead-code + circular imports + boundaries |
| `pyllow dupes` | Duplication detection (4 modes) |
| `pyllow health` | Complexity + maintainability + hotspots |
| `pyllow flags` | Feature-flag inventory and stale-flag detection |
| `pyllow audit` | PR quality gate (pass/warn/fail on changed files) |
| `pyllow fix` | Auto-remove unused imports, deps, enum members, class attrs |
| `pyllow watch` | Continuous re-analysis on file change |
| `pyllow init` | Scaffold `pyllow.toml` (or `[tool.pyllow]` in `pyproject.toml`); optional pre-commit hook |
| `pyllow config` | Print resolved config (with `extends` chains) |
| `pyllow list` | Inspect detected entry points, files, plugins, boundaries |
| `pyllow baseline` | Save/compare against baselines; regression detection with tolerance |
| `pyllow migrate` | Convert from `vulture` whitelist / `import-linter` config |
| `pyllow ci-template` | Print or vendor GitHub Actions / GitLab CI templates |
| `pyllow schema` / `pyllow config-schema` | Machine-readable JSON schemas for CLI and config |

### Output formats
Human (colored table), JSON, SARIF, compact, Markdown (PR comments), CodeClimate (GitLab), SVG/text health badges.

### Integrations
- **LSP server** — real-time diagnostics in VS Code, PyCharm, Neovim, Zed
- **MCP server** — `pyllow analyze`, `pyllow audit`, `pyllow trace_*` tools exposed to Claude Code, Cursor, and other MCP clients
- **GitHub Action** — distributed action with built-in SARIF upload
- **GitLab CI templates** — MR comment + Code Quality reporter
- **Pre-commit hooks** — both `.pre-commit-config.yaml` integration and Claude Code `PreToolUse` gate
- **Editor extensions** — VS Code, JetBrains/PyCharm, Zed

### Configuration
`pyllow.toml` standalone or `[tool.pyllow]` inside `pyproject.toml` (Python convention). Supports `extends` for shared config inheritance, per-analysis suppressions, baseline files, ignore patterns, custom entry points, and external user-defined plugins via JSON manifests.

### Auto-fix
Lossless removals only — unused imports, unused dependencies (from `pyproject.toml`), unused enum members, unused class attributes. `--dry-run` previews; `--yes` applies. Never modifies semantics.

---

## Roadmap

| Phase | Adds | Approx. solo timeline |
|-------|------|----------------------|
| **v0.0.1** *(current)* | `check` + FastAPI plugin (see [MVP](#mvp--v001-focus)) | 2–3 months |
| **v0.2** | `dupes` (suffix-array, mild mode); Flask + Django + pytest plugins; baselines | +2 months |
| **v0.3** | `health` (cyclomatic, cognitive, maintainability); auto-`fix` for unused imports/deps | +2 months |
| **v0.4** | LSP server; MCP server | +2 months |
| **v1.0** | Celery, SQLAlchemy, Pydantic, Click/Typer, Pyramid, Tornado, Streamlit plugins; architecture boundaries; SARIF; GitHub Action; GitLab CI templates | +3 months |
| **post-1.0** | Runtime coverage layer (free — pyllow gates nothing); editor extensions; Sphinx/MkDocs awareness |

---

## Architecture

- **Parser**: [`enderpy_python_parser`](https://github.com/Glyphack/enderpy) — a Rust-native Python parser published to crates.io as part of the `enderpy` type checker. Chosen for clean dependency management (no vendoring, no upstream-fork tracking). pyllow builds module-graph and scope analysis on top of enderpy's AST. Astral's unpublished `ruff_python_parser` + `ruff_python_semantic` remain a backup option if enderpy's semantic surface proves insufficient at scale, but vendoring an unpublished crate set is a maintenance cost we'd rather avoid.
- **Workspace**: Cargo workspace with `crates/cli`, `crates/analyzer`, and one crate per framework plugin (`crates/plugin-fastapi`, `crates/plugin-django`, …). The `cli`/`analyzer` split mirrors fallow's so the LSP and MCP servers can embed the analyzer without dragging clap.
- **Module resolution (v0.0.1 assumptions)**:
  - Trust `pyproject.toml` package roots
  - Honor PEP 420 namespace packages inside those roots
  - Resolve literal-string `importlib.import_module(...)` and `__import__(...)`
  - Treat both branches of `try/except ImportError` and `if TYPE_CHECKING:` as live
  - Mark non-literal dynamic imports as opaque — false negatives are preferred over false positives

---

## Inspiration

pyllow exists because [**fallow**](https://fallow.tools) ([github.com/fallow-rs/fallow](https://github.com/fallow-rs/fallow)) proved this category of tool is genuinely useful — and because fallow is TypeScript/JavaScript only. pyllow is a *parallel project*, not a port: Python's import system, framework conventions, and ecosystem differ enough that the analyzer cores cannot be shared. Credit and design inspiration belong to fallow's authors.

Differences from fallow:
- **Always free.** No paid runtime tier; pyllow's runtime coverage layer (when it lands post-1.0) is open
- **Python module model.** PEP 420 namespace packages, dynamic imports, `__init__.py` re-exports, type-stub awareness
- **Python-tailored plugin set.** Django / Flask / FastAPI / Celery / SQLAlchemy / etc., not Next.js / Nuxt / Remix

---

## License

MIT.

## Contributing

v0.0.1 is in solo development. Issues, suggestions, and framework-pattern reports welcome once the first pre-release is tagged.
