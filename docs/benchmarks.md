# Benchmarks

Measured on the 0.0.7 release build against [Netflix/dispatch](https://github.com/Netflix/dispatch),
a production FastAPI application (655 Python files, ~72385 lines), on Linux x86_64.
One small change staged for the staged-mode runs.

| Run | Wall time |
| --- | --- |
| `pyllow check .` (full tree) | 73 ms |
| `pyllow smells .` (full tree) | 61 ms |
| `pyllow audit .` (all families) | 1.10 s |
| `pyllow audit . --staged --only smells` | 194 ms |
| `pyllow audit . --staged` (all families) | 1.11 s |

Family selection is where the pre-commit budget comes from: `--only smells`
skips the duplicate-detection and health passes entirely, cutting a staged
audit from ~1.1 s to under 200 ms. The staged snapshot itself
(`checkout-index` into a temp dir) costs roughly 120 ms of that.

Reproduce with any large repo:

```bash
git clone --depth 1 https://github.com/Netflix/dispatch
cd dispatch && time pyllow audit . --staged --only smells
```
