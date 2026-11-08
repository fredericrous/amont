---
canonical: fredericrous/adr-lint docs/plans/2026-11-08-ci-fetches-release-assets-api.md
phases: [2]
status: done
branch: fix/gh-api-downloads
repos: [amont]
---

# CI fetches GitHub release assets through the API, fleet-wide (pointer)

This repository's slice of the fleet plan: Phase 2. The plan, its review
and its recipe are canonical in adr-lint (pilot: fredericrous/adr-lint#53).

## Code review

- amont: backend, platform

## Outcome

- input: "Compare MANIFEST.sha256 with amont-agent's" (now `gh api … Accept: application/vnd.github.raw`) → expected: the files match → actual: "excerpt fixtures match amont-agent's", exit 0.
- input: "Install the k8s tools (Linux)" (`gh_dl`, including the slash tag `kustomize/v5.4.3`) → expected: kube-linter, kubeconform and kustomize in `$HOME/.local/bin` → actual: as expected, exit 0.
- input: release.yaml "git-attest 1.7.0" → expected: fetched through gh, `sha256sum -c` OK → actual: as expected; `GH_TOKEN` is set on that step itself (yq).
- input: hadolint v2.12.0 `hadolint-Linux-x86_64`, yq v4.44.6 `yq_linux_amd64` → expected: ELF executables through gh → actual: as expected.
- input: "Ensure aval" 1.10.0 and mdBook 0.4.40 → expected: fetched and extracted → actual: as expected.
- input: the PowerShell `releases/latest` step → not run locally (no pwsh here); the PR's Windows leg is its check.
- These runs happened while the web route was down (a 60 s timeout and a 503 at 12:34Z); the API served every file.
- To observe after the merge: `docs.yaml` dispatch; `release.yaml` on the next tag.

## Implementation review

- amont implementation-review: approve-with-changes, low (record outcome, done here; Windows leg is the PowerShell check); 53k, 24 s (opus).
- amont backend: approve-with-changes, low; deliberate: the PowerShell lookup fails once rather than retrying (honest failure, a re-run covers a blip), and the five inline loops are the fleet-identical block; 67k, 66 s (opus).
- amont platform: approve, no findings; 58k, 54 s (opus).
