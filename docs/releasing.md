# Release Pipeline

Aizu's release workflow is `.github/workflows/release.yml`. It does not run on
pull requests and therefore does not add to the normal two-runner PR budget.

## Rehearsal

Run **Release** from the Actions page with:

- `mode`: `rehearsal`
- `version`: the exact version currently present in Cargo, Tauri, and the
  desktop package

A rehearsal uses no protected signing secrets. It runs source checks, builds
the standalone CLI for Linux and macOS on both supported architectures, builds
unsigned macOS application bundles, constructs development DMGs, generates an
SPDX SBOM and `SHA256SUMS`, verifies the downloaded artifact set on a clean
runner, and retains one assembled Actions artifact for 14 days. It does not
create a tag or GitHub Release.

To limit Actions consumption, the rehearsal uses four jobs: preflight, one
Linux cross-build job, one Apple Silicon macOS cross-build job, and one clean
assembly/verification job. Architectures are built sequentially inside their
platform job instead of multiplying runners with a matrix.

## Public Release

Public publishing is intentionally unavailable until all of these are true:

1. Cargo workspace, Tauri, and desktop package versions are identical.
2. `assets/branding/icon-manifest.json` has `branding_status: "approved"` and
   `release_approved: true` after explicit brand review.
3. The checked-in Tauri release configuration enables updater artifacts, and
   the main Tauri config contains the approved updater public key.
4. GitHub environments `release-signing` and `release-publish` exist with
   required reviewers and tag-only deployment rules.
5. `release-signing` contains the following secrets:

   - `APPLE_CERTIFICATE`
   - `APPLE_CERTIFICATE_PASSWORD`
   - `APPLE_SIGNING_IDENTITY`
   - `APPLE_TEAM_ID`
   - `APPLE_API_ISSUER`
   - `APPLE_API_KEY`
   - `APPLE_API_KEY_CONTENT`
   - `TAURI_SIGNING_PRIVATE_KEY`
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` when the updater key is encrypted

6. A protected `vX.Y.Z` tag points to a commit contained in `main`. Its merged
   pull request head has a successful pull-request CI run, and the PR's merge
   commit is the tagged squash commit.

Creating the tag starts the publish path. Signing/notarization and GitHub
Release publication are separate protected-environment jobs. Artifacts are
attested before publication, and publication creates a protected **draft**
release. A repository owner must inspect the draft assets, notarization,
attestations, `latest.json`, checksums, and release notes before making it
public.

Never rerun a public version to replace its artifacts. Delete a bad draft
before publication, or release a new patch version after publication.

## Local Contract Checks

```bash
mise run ci:check
actionlint .github/workflows/release.yml
shellcheck scripts/release/*.sh
```

`mise run ci:check` mutation-tests the preflight, asset inventory, and checksum
verification. It does not attempt Apple signing or notarization locally.
