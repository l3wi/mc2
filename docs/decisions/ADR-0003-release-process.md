# ADR-0003: Release process — release-plz on main, nightlies from dev

- Status: accepted
- Date: 2026-08-09
- Deciders: maintainers
- Context: [release-automation](../tasks/release-automation.md)

## Decision

Versioning and releases run on **`main`**; `dev` produces **nightly prereleases**.

- **release-plz** drives `main` (`.github/workflows/release-plz.yml`,
  `release-plz.toml`):
  - Feature batches (merged via `dev` → `main`) bump **MINOR**
    (`custom_minor_increment_regex = "^feat"`, so `feat` → `x.Y.Z` even in 0.x).
  - Hotfixes merged directly to `main` bump **PATCH** (conventional `fix:`).
  - release-plz opens a release PR (version + CHANGELOG.md, managed by
    release-plz) and, once merged, creates a single **`vX.Y.Z`** tag for the
    `mc2` package (internal crates share the workspace version and are excluded).
  - **cargo-dist** (`release.yml`, pre-existing) builds installers and publishes
    the GitHub Release from that tag.
- **Nightlies** (`.github/workflows/nightly.yml`): a scheduled job cuts
  `vX.Y.Z-dev.<date>` prerelease tags from `dev` on a throwaway branch; cargo-dist
  publishes them as GitHub **prereleases**. The base version is the latest full
  release tag, so nightlies never touch `dev`'s Cargo.toml and never appear in
  `main`'s history or the changelog (`tag_pattern` excludes `-dev`).
- **Changelog gate** (`.github/workflows/changelog.yml`): PRs into `main`/`dev`
  must modify `CHANGELOG.md` (automation branches exempt) so changes never land
  unexplained.

## Consequences

- `main` is the only place versions are bumped and tagged — no double-bump risk
  (release-plz always increments the current version, so bumping on both `dev`
  and `main` would over-increment).
- `dev`'s Cargo.toml version is static; `dev` → `main` merges preserve `main`'s
  higher version via 3-way merge, so no explicit sync-back is needed.
- The `mc2` crate name is taken on crates.io by an unrelated project; MC2 never
  publishes (`publish = false`), and release-plz skips registry comparison.
- `CHANGELOG.md` is regenerated from conventional commits at release time;
  contributors still add "## Unreleased" entries (gate requirement) which
  release-plz re-renders.
- First release: the existing `v0.1.0` tag is the base; the next release PR will
  be `v0.2.0` from the current `feat(cli)` commit.
