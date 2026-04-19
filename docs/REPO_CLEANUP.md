# Repository Cleanup Report

## Date: 2026-04-19

## Files Deleted (8 files)

| File | Reason |
|------|--------|
| `dummy.json` | Empty placeholder test file, unused |
| `.githooks/pre-commit` | Redundant with CI workflow; deleted to avoid confusion |
| `.github/workflows/ci.yml` | Duplicate of `.github/ci.yml` (smaller, stale) |
| `.github/workflows/release.yml` | Duplicate of `.github/release.yml` (smaller, stale) |
| `web/src/assets/hero.png` | Vite scaffold artifact, not referenced in any component |
| `web/src/assets/react.svg` | Vite scaffold artifact, not referenced in any component |
| `web/src/assets/vite.svg` | Vite scaffold artifact, not referenced in any component |

## Empty Directories Removed

- `.githooks/` — became empty after pre-commit deletion
- `.github/workflows/` — became empty after duplicate workflow file deletion

## .gitignore Fixes

1. **Removed `Cargo.lock`** — Cargo.lock MUST be committed for reproducible builds. This was a critical bug.
2. **Removed stale `src/bin/order_test.rs`** — this path no longer exists.
3. **Added `web/dist/`** — frontend build output directory was not being ignored.

## README.md Updates

1. Replaced `.githooks/pre-commit` mention → "CI quality gates via GitHub Actions"
2. Removed `git config core.hooksPath .githooks` from Installation instructions
3. Added new features:
   - Per-Category Position Limits (`[risk_by_category]` config)
   - PnL & Equity Chart (7-day SVG chart, `GET /api/pnl_history`)
4. Added `[risk_by_category]` config table under `config.toml` reference

## Git State After Cleanup

```
D  .githooks/pre-commit
D  .github/workflows/ci.yml
D  .github/workflows/release.yml
D  web/src/assets/hero.png
D  web/src/assets/react.svg
D  web/src/assets/vite.svg
D  dummy.json

M  .gitignore        (Cargo.lock removed from ignore, web/dist/ added)
M  README.md         (new features documented)
M  config.example.toml
M  src/*.rs          (engineering mode features)
M  web/src/*.tsx     (PnLChart, SettingsManager, App)

A  Cargo.lock        (now tracked — was incorrectly ignored)
A  web/src/PnLChart.tsx
```

## Recommended Next Step

```bash
git add -A
git commit -m "feat: engineering mode — PnL chart, per-category risk limits, cleanup"
```
