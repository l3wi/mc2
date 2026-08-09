# Release automation: tags + releases on main, nightlies from dev

Status: **implemented** (see ADR-0003)
Branch: `dev` → will land on `main` via the release flow below.

## Context

MC2 already uses **cargo-dist** (`[workspace.metadata.dist]`, generated `release.yml`): a tag push like
`v0.1.0` builds installers/binaries and creates a GitHub Release. What's missing is the **front half**:
who decides the next version and creates the tag, and a nightly story for `dev`.

Stack: **release-plz** (version bumping + tagging, same axodotdev ecosystem as cargo-dist) +
**cargo-dist prerelease tags** for nightlies.

> Note: the crate name `mc2` is taken on crates.io by an unrelated project (nicolube/mc2). MC2 is never
> published (`publish = false`); verified locally that release-plz then skips registry-based version
> comparison.

## Version policy (final, ADR-0003)

- **`main` = the only release line.** release-plz on `main`:
  - Feature batches (merged via `dev` → `main`) bump **MINOR** —
    `custom_minor_increment_regex = "^feat"` makes `feat` → `x.Y.Z` even in 0.x.
  - Hotfixes merged directly to `main` bump **PATCH** (conventional `fix:`).
  - One release PR (version + CHANGELOG.md, managed by release-plz) per cycle; once merged, release-plz
    creates the single **`vX.Y.Z`** tag (`git_tag_name = "v{{ version }}"`, only the `mc2` package —
    internal crates are `release = false` and share the workspace version). cargo-dist builds the Release.
- **`dev` = nightlies.** A scheduled workflow cuts `vX.Y.Z-dev.<date>` **prerelease** tags from `dev`
  (throwaway branch, tag pushed only); cargo-dist publishes them as GitHub prereleases. The base version
  is the latest full release tag, so `dev`'s Cargo.toml never drifts and `-dev` tags never enter `main`'s
  history or the changelog (`tag_pattern` excludes them).
- **Changelog gate**: PRs into `dev`/`main` must modify `CHANGELOG.md` (automation branches exempt).

### Why not "bump minor on every dev merge" (as originally requested)

Verified against release-plz 0.3.160: release-plz **always increments the current version** from the
commit-derived bump. If `dev` bumped to `0.2.0` and `main` then ran release-plz, `main` would compute
`0.2.1` (or `0.3.0` with the feat regex) on top — a double bump — rather than "adopting" `0.2.0`.
Bumping on both branches also diverges the shared `Cargo.toml` version and causes merge conflicts.
So the minor bump for feature work happens **at release time on `main`** (from the `feat:` commits that
merged into `dev`), and hotfixes on `main` stay patch. This is the standard release-plz + cargo-dist
setup and keeps the automation reliable.

## Files

- `release-plz.toml` — main config (feat→minor regex, single v-tag for `mc2`, changelog managed, no
  publish, no git release; cargo-dist owns the Release).
- `.github/workflows/release-plz.yml` — on push to `main`: `release-pr` (opens version-bump PR) and
  `release` (tags the merged release PR).
- `.github/workflows/nightly.yml` — daily: cut `vX.Y.Z-dev.<date>` prerelease tag from `dev` → cargo-dist
  prerelease.
- `.github/workflows/changelog.yml` — gate: PRs into `dev`/`main` must modify `CHANGELOG.md`.
- `docs/decisions/ADR-0003-release-process.md`.

## Release flow (main)

```
PR (dev → main) merges  ── dev's version line is static; main keeps its higher version via 3-way merge
      │ push to main
      ▼
release-plz (main)
  ├─ release-pr job → opens/updates release PR (feat → minor, fix → patch; CHANGELOG.md regenerated)
  └─ release job   → no-op until that release PR merges
      │
      │ merge release PR → push to main
      ▼
release job → git tag vX.Y.Z
      │ tag push
      ▼
cargo-dist release.yml (existing) → installers + GitHub Release
```

Hotfix: `fix:` merged directly to `main` → release-plz bumps patch → `vX.Y.(z+1)`.

## Verification

- `release-plz update --config release-plz.toml` runs clean locally (clean tree; `publish = false` ⇒ no
  registry comparison) and reports `mc2: 0.1.0 → 0.2.0` with only the `mc2` package.
- Workflow YAML + TOML validated; CI additions don't affect `just check` / deny / audit.

