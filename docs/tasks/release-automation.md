# Release automation: tags + releases on main, nightlies from dev

Status: **approved with refinements** (implementing)
Branch: `dev` → will land on `main` via the release flow below.

## Context

MC2 already uses **cargo-dist** (`[workspace.metadata.dist]`, generated `release.yml`): a tag push like
`v0.1.0` builds installers/binaries and creates a GitHub Release. What's missing is the **front half**:
who decides the next version and creates the tag, and a nightly story for `dev`.

Stack: **release-plz** (version bumping + tagging, same axodotdev ecosystem as cargo-dist) +
**cargo-dist prerelease tags** for nightlies.

> Note: the crate name `mc2` is taken on crates.io by an unrelated project (nicolube/mc2). MC2 is never
> published (`publish = false`), but release-plz compares local crates against the registry, so its
> registry lookups must not drive versioning. Verified: with `publish = false` release-plz skips
> registry-based version comparison.

## Version policy (branch-based)

- **`dev` = next-minor line.** Every merge into `dev` bumps **MINOR** (`x.Y.Z` → `x.(Y+1).0`) via a
  release-plz release PR (`custom_minor_increment_regex = ".*"` forces minor). No release tags on `dev`.
- **`main` = release + patch line.** The dev → main release PR adopts dev's already-bumped version
  (release-plz takes max(current, computed) — never double-bumps) and tags it; a hotfix merged directly
  to `main` bumps **PATCH** (`x.Y.z` → `x.Y.(z+1)`, conventional `fix:`).
- **Nightlies = cargo-dist prereleases.** A scheduled workflow cuts `vX.Y.Z-dev.<date>` prerelease tags
  from `dev` on a temp branch; the existing `release.yml` builds them as GitHub **prereleases**. Nightly
  tags never appear in the changelog (`tag_pattern`).

## Files

- `release-plz.toml` (main) — conventional-commit bumps, changelog managed by release-plz, no publish,
  no git release (cargo-dist owns the Release), `semver_check = false`.
- `release-plz-dev.toml` (dev) — same + `custom_minor_increment_regex = ".*"` (force minor).
- `.github/workflows/release-plz.yml` — on push to `main`: `release-pr` (opens version-bump PR) and
  `release` (tags the merged release PR → cargo-dist builds the Release).
- `.github/workflows/release-plz-dev.yml` — on push to `dev`: `release-pr` only (bump minor + changelog
  via PR into dev; no tag).
- `.github/workflows/nightly.yml` — daily: bump `dev` HEAD to `X.Y.Z-dev.<date>`, tag `vX.Y.Z-dev.<date>`
  on a temp branch, push the tag → cargo-dist prerelease.
- `.github/workflows/changelog.yml` — **gate**: PRs into `dev`/`main` must modify `CHANGELOG.md`
  (exempt: release-plz / dependabot / nightly branches) so changes never land unexplained.
- `docs/decisions/ADR-0003-release-process.md`.

## Release flow (main)

```
PR (dev → main) merges  ── carries dev's already-bumped minor version
      │ push to main
      ▼
release-plz (main)
  ├─ release-pr job → opens/updates release PR (version + CHANGELOG)
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

- `release-plz update --config <file>` runs clean locally (clean tree, `publish = false` ⇒ no registry
  comparison) and prints the computed next version.
- CI additions don't affect `just check` / deny / audit.
