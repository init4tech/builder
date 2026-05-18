# Gouda builder deps bump + image cut — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `init4tech/builder` into the gouda rollup cohort: dep pins moved to git-rev pins against the cohort SHAs, code compiles against the 0.16 → 0.19 signet-sdk jump, a verified `linux/arm64` ECR image is pushed for node-ops to deploy.

**Architecture:** Single feature branch `feat/gouda-deps` (worktree already created), one draft PR targeting `main`, one ECR image cut tagged `gouda-<sha>` + `gouda-latest`. Mirrors the path the signet binary followed across its 4 cuts.

**Tech Stack:** Rust 1.88 / Edition 2024, cargo, `signet-*` SDK (git-rev pinned), `init4-bin-base` (git-rev pinned), trevm, tokio, docker buildx (arm64), AWS ECR.

**Spec:** `docs/superpowers/specs/2026-05-18-gouda-builder-deps-bump-design.md` (committed as `6d2ce19` on this branch).

**Worktree:** `/Users/swanpro/git/builder/.claude/worktrees/gouda-deps` on branch `feat/gouda-deps`.

---

## Cohort pin SHAs (reference)

| Source PR | Repo | HEAD SHA | Crates pinned |
|---|---|---|---|
| init4tech/signet-sdk#236 | signet-sdk | `ecce6a4` | signet-constants, signet-sim, signet-tx-cache, signet-types, signet-zenith, signet-bundle |
| init4tech/bin-base#149 | bin-base | `b2f92e4` | init4-bin-base |
| init4tech/node-components#144 | node-components | `752fa4a` | signet-block-processor, signet-genesis |

---

## Pre-flight assumptions

The executor of this plan must have:
- Working directory `/Users/swanpro/git/builder/.claude/worktrees/gouda-deps` on branch `feat/gouda-deps`.
- An SSH agent forwarding GitHub-authenticated keys for the private `init4tech/*` repos (`echo $SSH_AUTH_SOCK` non-empty; `ssh -T git@github.com` succeeds).
- `aws` CLI configured with credentials for account `637423570300` (region `us-east-1`) capable of `ecr:GetAuthorizationToken`, `ecr:DescribeRepositories`, and pushing to the builder image repository.
- `docker buildx` available with QEMU support for `linux/arm64` emulation (`docker buildx ls` lists a builder that supports arm64; if not, `docker buildx create --use --bootstrap`).
- `gh` CLI authenticated for `init4tech/builder`.

If any of these is missing, stop and surface — they're not in this plan's scope to provision.

---

## Phase A — Verify starting state

### Task A1: Confirm worktree and spec commit

**Files:** none modified.

- [ ] **Step 1: Confirm cwd / branch / spec commit**

Run:
```bash
git -C /Users/swanpro/git/builder/.claude/worktrees/gouda-deps status
git -C /Users/swanpro/git/builder/.claude/worktrees/gouda-deps log --oneline -3
```

Expected output:
- `On branch feat/gouda-deps`
- Working tree clean
- Topmost commit: `docs: add gouda deps bump + image cut design spec` (6d2ce19)
- Second commit: `6782ecf ignore new audit warnings (#271)` (main HEAD at branch creation)

If anything else, stop and report.

---

## Phase B — Dependency bumps

### Task B1: Replace Cargo.toml pins with cohort git-revs

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Edit Cargo.toml dependency pins**

Replace each line in the `[dependencies]` section. Use git-rev pins (no `tag = …`, no `branch = …`, no `version = …` constraint alongside). Exact replacements:

```toml
[dependencies]
init4-bin-base = { git = "https://github.com/init4tech/bin-base", rev = "b2f92e4", features = ["perms", "aws", "pylon"] }

signet-constants = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
signet-sim = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
signet-tx-cache = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
signet-types = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
signet-zenith = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
signet-block-processor = { git = "https://github.com/init4tech/node-components", rev = "752fa4ad0956cf3a0b0d040402f40fc9b41b0fe8" }
signet-genesis = { git = "https://github.com/init4tech/node-components", rev = "752fa4ad0956cf3a0b0d040402f40fc9b41b0fe8" }
```

And in the `[dev-dependencies]` section, replace the `signet-bundle` line:

```toml
signet-bundle = { git = "https://github.com/init4tech/signet-sdk", rev = "ecce6a4" }
```

Leave every other line (trevm, alloy, reth-chainspec, axum, etc.) untouched.

Note: the node-components rev is shown in full (`752fa4ad…`) to match the signet PR's pin verbatim and avoid abbreviation ambiguity. The 7-char `752fa4a` is the same commit.

- [ ] **Step 2: Confirm the diff is what was intended**

Run:
```bash
git -C /Users/swanpro/git/builder/.claude/worktrees/gouda-deps diff Cargo.toml
```

Expected: only the 9 dependency lines changed (8 in `[dependencies]`, 1 in `[dev-dependencies]`). No other edits. No removed `[patch.crates-io]` section.

- [ ] **Step 3: Regenerate Cargo.lock by fetching the new deps**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && cargo update --workspace
```

Expected: cargo resolves all 9 git-rev deps. No errors. Cargo.lock is rewritten.

If a transitive dep conflict appears (e.g. reth version clash), capture the full error, **do not attempt to fix yet** — stop and report. This is the diamond-dep risk noted in the spec.

- [ ] **Step 4: Commit the deps bump (pre-build)**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add Cargo.toml Cargo.lock && \
  git commit -m "$(cat <<'EOF'
chore(deps): pin signet-sdk family to gouda cohort revs

- init4-bin-base @ b2f92e4 (bin-base#149)
- signet-{constants,sim,tx-cache,types,zenith} @ ecce6a4 (signet-sdk#236)
- signet-{block-processor,genesis} @ 752fa4a (node-components#144)
- signet-bundle (dev-dep) @ ecce6a4

Matches the lockstep pin set used by signet#105 cut 4. Build/test fixes
follow in subsequent commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds. (No push yet — pre-push hook would fail because compile errors are likely.)

---

## Phase C — Compile clean against the new SDK

This phase iterates until `cargo build --release --bin zenith-builder-example` succeeds. The exact code changes can't be enumerated in advance because they depend on which API surfaces actually changed. The process is fixed; the fixes are discovered.

### Task C1: First build attempt and breakage triage

**Files:** none modified yet — investigation only.

- [ ] **Step 1: Attempt the release build**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  cargo build --release --bin zenith-builder-example 2>&1 | tee /tmp/builder-gouda-build-1.log
```

- [ ] **Step 2: Triage**

If `cargo build` exits 0, skip to **Phase D**. Otherwise, scan `/tmp/builder-gouda-build-1.log` and produce a list:

For each unique `error[E…]` or `error:` block:
- File path and line number from the error
- One-line summary of what changed (renamed item, changed signature, removed item, etc.)
- Likely fix category: (1) mechanical rename, (2) signature update, (3) replaced API, (4) requires design re-think

If **every** error falls into categories (1)–(3), proceed to C2. If **any** error is category (4) — i.e. requires a non-trivial rewrite beyond what was anticipated in the spec's "anticipated surface area" — **stop and surface to the user** before making code changes. The spec mandates this gate.

### Task C2: Iterate fixes file-by-file

Iterate this task once per logical fix area until `cargo build --release` succeeds. Group fixes by the file they live in (use the spec's "anticipated surface area" list as a starting partition: `src/config.rs`, then `src/tasks/block/`, then `src/tasks/cache/`, then `src/tasks/submit/`, then `src/tasks/env.rs`, then `src/test_utils/mod.rs`, then `bin/builder.rs`, then `benches/`).

**Files (per iteration):**
- Modify: one or a small group of `src/**/*.rs` files
- Read: the relevant cohort PR (e.g. signet-sdk@ecce6a4) to see the new API shape

- [ ] **Step 1: Pick the next file to fix**

Choose the file with the most errors, or the one earliest in the dep chain (config.rs first, since other files import from it).

- [ ] **Step 2: Resolve the API change**

For each error in that file:
1. Read the relevant module in the cohort PR. Useful commands:
   - `gh api repos/init4tech/signet-sdk/contents/crates/<crate>/src/<file>.rs?ref=ecce6a4 --jq .content | base64 -d`
   - `gh api repos/init4tech/bin-base/contents/<path>?ref=b2f92e4 --jq .content | base64 -d`
   - `gh api repos/init4tech/node-components/contents/<path>?ref=752fa4ad0956cf3a0b0d040402f40fc9b41b0fe8 --jq .content | base64 -d`
   - Or browse the PR diff on github.com if easier.
2. Apply the minimal change to call the new API. **No refactoring beyond the rename/signature update.**
3. Preserve all surrounding code, comments, log messages.

- [ ] **Step 3: Re-run cargo build to verify the file is clean**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  cargo build --release --bin zenith-builder-example 2>&1 | tee /tmp/builder-gouda-build-iter.log
```

Expected: errors in the file just edited are gone (others may remain). If new errors appeared in the *same* file, fix them in the same commit.

- [ ] **Step 4: Commit this fix**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add <files-touched> && \
  git commit -m "$(cat <<'EOF'
fix(<area>): port <module> to gouda <crate> API

<One-line description of the API change. e.g.
"SlotCalculator::new now takes (chain_id, start_ts, duration, offset) instead of …">

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Replace `<area>` (e.g. `config`, `sim`, `cache`, `submit`, `env`, `test-utils`, `bin`, `bench`), `<module>` and the description with real content.

- [ ] **Step 5: Repeat until cargo build exits 0**

Re-run Step 1 for the next file. Stop iterating when `cargo build --release --bin zenith-builder-example` exits 0.

---

## Phase D — Lint / format / doc / test gates

These are the spec's verification gates 2–6. Run them in order; any failure means fix-then-recheck before proceeding.

### Task D1: Format check

**Files:** any `*.rs` whose formatting drifted.

- [ ] **Step 1: Apply cargo fmt**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && cargo fmt --all
```

- [ ] **Step 2: Confirm no diff**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && git diff --stat
```

Expected: empty output (no diff). If non-empty, the API-port commits left unformatted code. Inspect with `git diff`, then:

```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add -u && \
  git commit -m "style: cargo fmt after gouda API port

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task D2: Clippy

**Files:** any `*.rs` flagged by clippy.

- [ ] **Step 1: Run clippy with -D warnings**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && make clippy
```

(This runs `cargo clippy --all-targets --all-features --workspace --profile dev -- -D warnings`.)

Expected: exit 0.

- [ ] **Step 2: Fix any lints**

If clippy fails:
1. Read each lint at the cited file/line.
2. Apply the suggested fix (or rewrite to the simplest equivalent).
3. **Do not** suppress with `#[allow(...)]` unless the lint is genuinely wrong for the context; if so, add a one-line `// reason: …` comment.
4. Re-run `make clippy` until green.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add -u && \
  git commit -m "chore(clippy): satisfy lints after gouda API port

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

(Skip the commit if there were no clippy fixes needed.)

### Task D3: Doc build with -D warnings

**Files:** any rustdoc comments referencing renamed/removed items.

- [ ] **Step 1: Build docs with warnings denied**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Expected: exit 0.

- [ ] **Step 2: Fix any broken intra-doc links / missing-doc warnings**

For each warning:
1. Locate the offending doc comment.
2. Update the link target or replace with a plain markdown reference.
3. Re-run until green.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add -u && \
  git commit -m "docs: fix rustdoc references after gouda API port

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

(Skip if no doc fixes were needed.)

### Task D4: Unit tests

**Files:** any `mod tests` blocks broken by the API change.

- [ ] **Step 1: Run the unit test suite**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && make test
```

(This runs `cargo test`, which compiles tests but skips integration tests in `tests/` since they require the `test-utils` feature.)

Expected: all unit tests pass.

- [ ] **Step 2: Fix any failures**

For each failing test:
1. If it's a compile-time failure (mismatched type, removed item), fix the test's usage of the new API.
2. If it's a runtime assertion failure, **stop and surface** — that means the API port changed behavior, which is out of scope for a dep bump.

- [ ] **Step 3: Commit test fixes**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git add -u && \
  git commit -m "test: update unit tests for gouda API

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

(Skip if no test edits were needed.)

### Task D5: Integration tests (best-effort)

**Files:** any `tests/*.rs` broken by the API change.

- [ ] **Step 1: Build the integration test binaries**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  cargo test --features test-utils --no-run 2>&1 | tee /tmp/builder-gouda-itest-build.log
```

Expected: exit 0 (binaries compile).

If compile fails, fix the tests under `tests/` the same way as Task D4 Step 2, commit with `test(integration): …` message, re-run.

- [ ] **Step 2: Run integration tests**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  cargo test --features test-utils 2>&1 | tee /tmp/builder-gouda-itest-run.log
```

The CLAUDE.md note for this crate says "requires network". Expected outcomes:

- **All pass:** great, proceed.
- **Some/all fail with network errors** (DNS, connection refused, 401/403 from RPC): note this explicitly in the PR description ("integration tests skipped: require live RPC; built locally"). Do **not** treat as a plan failure.
- **Failures unrelated to network** (assertion failures, panics on local logic): investigate. May indicate an API behavior change. Stop and surface.

---

## Phase E — Local arm64 image build

The spec's verification gates 7–9. The signet PR's cut-3 incident (buildx cache poisoning producing an aarch64 ELF inside an amd64 manifest) is the primary risk here; mitigated by `buildx prune --all` and dual arch verification.

### Task E1: Clear buildx cache

**Files:** none.

- [ ] **Step 1: Prune buildx cache**

Run:
```bash
docker buildx prune --all --force
```

Expected: cache freed. (Output line `Total: …MB` or similar.)

### Task E2: Build the arm64 image locally

**Files:** none modified — produces local image tagged `builder:gouda-local`.

- [ ] **Step 1: Build with explicit arm64 platform and SSH mount**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  DOCKER_BUILDKIT=1 docker buildx build \
    --platform linux/arm64 \
    --ssh default=$SSH_AUTH_SOCK \
    --load \
    -t builder:gouda-local \
    .
```

Expected: build completes. The cargo-chef stages (planner, cook, builder) all complete; the final `debian:bookworm-slim` stage produces the runtime image.

If build fails:
- **`failed to fetch` from a git dep:** the SSH agent isn't being forwarded into the build. Confirm `$SSH_AUTH_SOCK` is set on the host before re-running. If the agent isn't running, start one (`ssh-add`) and re-run.
- **`exec /usr/bin/cargo: exec format error`:** QEMU emulation isn't installed for arm64. Run `docker run --privileged --rm tonistiigi/binfmt --install arm64` and retry.

### Task E3: Verify image is genuinely arm64 (dual check)

**Files:** none.

- [ ] **Step 1: Inspect manifest architecture**

Run:
```bash
docker image inspect builder:gouda-local --format '{{ .Os }}/{{ .Architecture }}'
```

Expected output: `linux/arm64`.

- [ ] **Step 2: Inspect actual binary ELF header**

Run:
```bash
docker run --rm --platform linux/arm64 --entrypoint /bin/sh builder:gouda-local \
  -c 'head -c 20 /usr/local/bin/zenith-builder-example | od -An -tx1'
```

Expected output: a line ending with the byte sequence `… b7 00` (where `b7` is the `e_machine` field for AArch64). If the value is `3e 00` (amd64) or anything else, the build was poisoned — purge buildx (`docker buildx prune --all --force`), purge images (`docker image rm builder:gouda-local`), then re-run Task E2. Do not push.

---

## Phase F — Push to ECR

The spec's gates 10–11.

### Task F1: Discover the ECR repo name and authenticate

**Files:** none.

- [ ] **Step 1: Find the builder ECR repo name**

The progress doc flags this as not previously confirmed. The running pod for builder is `quincey-builder-blobs`. ECR repo name candidates: `builder`, `zenith-builder`, `zenith-builder-example`, `quincey-builder`, `signet-builder`. Discover by listing all repos and matching tag history:

```bash
aws ecr describe-repositories --region us-east-1 \
  --query 'repositories[].repositoryName' --output text | tr '\t' '\n' | \
  grep -iE 'build|quincey'
```

For each candidate returned, list its existing tags to find the one already serving builder images in parmigiana/pecorino:

```bash
for r in <candidate-1> <candidate-2> ...; do
  echo "=== $r ==="
  aws ecr describe-images --region us-east-1 --repository-name "$r" \
    --query 'reverse(sort_by(imageDetails, &imagePushedAt))[:5].[imageTags[0], imagePushedAt]' \
    --output table
done
```

Pick the repo whose recent tags look like builder builds (e.g. existing `parmigiana-*` or `pecorino-*` tags, or `latest` that was recently pushed by the existing `builder-ecr-cd.yml` workflow). Record as `ECR_REPO`.

If no candidate has matching tag history, stop and surface — confirm with node-ops before pushing to an unfamiliar repo.

- [ ] **Step 2: Authenticate docker to ECR**

Run:
```bash
aws ecr get-login-password --region us-east-1 | \
  docker login --username AWS --password-stdin 637423570300.dkr.ecr.us-east-1.amazonaws.com
```

Expected: `Login Succeeded`.

### Task F2: Tag and push the image

**Files:** none.

- [ ] **Step 1: Capture the short SHA**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git rev-parse --short HEAD
```

Record the output as `SHORT_SHA` (will be e.g. `a1b2c3d`).

- [ ] **Step 2: Tag the local image with both ECR tags**

Run (substituting `<ECR_REPO>` from Task F1 and `<SHORT_SHA>` from Step 1):
```bash
docker tag builder:gouda-local 637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-<SHORT_SHA>
docker tag builder:gouda-local 637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-latest
```

- [ ] **Step 3: Push both tags**

Run:
```bash
docker push 637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-<SHORT_SHA>
docker push 637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-latest
```

Expected: both pushes succeed, registry returns a `sha256:…` digest for each. Record both digests.

### Task F3: Verify the remote image is arm64

**Files:** none.

- [ ] **Step 1: Inspect the pushed manifest**

Run:
```bash
docker buildx imagetools inspect \
  637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-latest \
  --format '{{ .Image.OS }}/{{ .Image.Architecture }}'
```

Expected: `linux/arm64`.

If anything else, **do not let node-ops deploy this image** — the cut is poisoned. Delete the remote tags:

```bash
aws ecr batch-delete-image --region us-east-1 --repository-name <ECR_REPO> \
  --image-ids imageTag=gouda-latest imageTag=gouda-<SHORT_SHA>
```

Re-run Task E1 (`prune`) and E2 (`build`), and re-investigate why the manifest was wrong.

---

## Phase G — Open the PR and update the progress doc

### Task G1: Push branch and create draft PR

**Files:** none in repo — creates the GitHub PR.

- [ ] **Step 1: Push the branch**

Run:
```bash
cd /Users/swanpro/git/builder/.claude/worktrees/gouda-deps && \
  git push -u origin feat/gouda-deps
```

The repo's pre-push hook will run `make fmt` (must be no-op), `make clippy`, and `cargo doc -D warnings`. All three were green in Phase D; expect the hook to pass and the push to succeed.

- [ ] **Step 2: Create a draft PR**

Run (still in the worktree):
```bash
gh pr create --draft --base main --title "chore(deps): bump signet-sdk family to gouda cohort" --body "$(cat <<'EOF'
## Summary

Brings `init4tech/builder` into the gouda rollup cohort:

- Pins `signet-{constants,sim,tx-cache,types,zenith,bundle}` to `init4tech/signet-sdk@ecce6a4` (sdk#236).
- Pins `init4-bin-base` to `init4tech/bin-base@b2f92e4` (bin-base#149).
- Pins `signet-{block-processor,genesis}` to `init4tech/node-components@752fa4a` (node-components#144).
- Ports any API surfaces that changed in the 0.16 → 0.19 jump (see commits).

Lockstep with `init4tech/signet#105` cut 4. Companion to the parmigiana
gouda rollout (see `/tmp/parmigiana-deploy-progress.md`).

## Image

Built locally with `docker buildx --platform linux/arm64`. Pushed to ECR as:
- `637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-<SHORT_SHA>`
- `637423570300.dkr.ecr.us-east-1.amazonaws.com/<ECR_REPO>:gouda-latest`

Digests captured in the cohort progress doc.

Reason for local build (vs. CI): CI workflow `init4tech/actions/.github/workflows/ecr-build-and-push.yml@main` arch is the open question flagged in the parmigiana progress doc. CI audit is a follow-up.

## Test plan

- [x] `cargo build --release --bin zenith-builder-example`
- [x] `make fmt` (no diff)
- [x] `make clippy` (`-D warnings`)
- [x] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- [x] `make test` (unit)
- [x] `cargo test --features test-utils` *(integration; if skipped due to network, note here)*
- [x] arm64 image: manifest = `linux/arm64`, ELF `e_machine = 0xb7`
- [x] Remote ECR manifest re-verified arm64
- [ ] Deployment into `parmigiana` namespace (owned by node-ops; out of scope for this PR)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Substitute the actual `<ECR_REPO>` and `<SHORT_SHA>` placeholders before running.

If any integration test was skipped due to network, update the test-plan checkbox to `[~]` and add a one-line note above it explaining which.

- [ ] **Step 3: Record the PR number**

The `gh pr create` output ends with a URL like `https://github.com/init4tech/builder/pull/272`. Record the number (e.g. `272`).

### Task G2: Update the parmigiana progress doc

**Files:**
- Modify: `/tmp/parmigiana-deploy-progress.md` (not in this repo — it's a session artifact)

- [ ] **Step 1: Add the builder row to the Open PRs table**

In `/tmp/parmigiana-deploy-progress.md`, locate the "Open PRs (all drafts, target `main`)" table and append a row:

```markdown
| builder | [#<PR_NUMBER>](https://github.com/init4tech/builder/pull/<PR_NUMBER>) | `feat/gouda-deps` | `<SHORT_SHA>` — cohort dep bump (signet-sdk + bin-base + node-components git-revs), API port for 0.16→0.19 jump, arm64 ECR image cut |
```

- [ ] **Step 2: Add the builder image digests under "ECR images"**

Locate the existing "ECR images" / "Cut 4" section. Append a new section header and a row in the same style:

```markdown
### Builder cut 1 (native linux/arm64)

| Image | Digest |
|---|---|
| `<ECR_REPO>` | `<SHA256_DIGEST>` |
```

Substitute `<ECR_REPO>` and the digest captured at Task F2 Step 3.

- [ ] **Step 3: Tick the builder sub-item in step 8**

Locate the "Step checklist" section and find the line:

```markdown
- [ ] **8. Stand up remaining gouda infra** (TOMORROW — see "Next session plan" below): builder, tx-cache, pylon, sequencer.
```

Edit to mark the builder portion done. Since this is one checkbox covering four services, change to a sub-list:

```markdown
- [ ] **8. Stand up remaining gouda infra** (see "Next session plan" below):
  - [x] builder — image cut, PR #<PR_NUMBER> open; awaiting node-ops deploy.
  - [ ] tx-cache
  - [ ] pylon (verify whether shared with parmigiana)
  - [ ] sequencer
```

This preserves the structure for the next session while recording completion of our piece.

---

## Done

At this point:
- Branch `feat/gouda-deps` is pushed, PR is open as draft.
- ECR has verified `linux/arm64` `gouda-<sha>` + `gouda-latest` tags for the builder.
- Parmigiana progress doc reflects the builder row + image digest + ticked sub-item.

The next session (per the parmigiana plan) will tackle tx-cache, pylon, and sequencer — each its own brainstorm → plan → execute cycle.

---

## Notes for the executor

- **Commit cadence:** prefer one commit per logical fix area in Phase C. Don't squash; small commits make review easier and let us bisect if anything regresses.
- **Don't touch unrelated files.** The parallel `chore/bump-1.0.1` work is owned elsewhere — do not bump `package.version` in this PR.
- **Stop and surface** on:
  - Transitive dep conflicts at `cargo update` (Task B1).
  - Any "category 4" API change (structural rewrite) at Task C1.
  - Runtime assertion failure in unit tests at Task D4.
  - Non-network failures in integration tests at Task D5.
  - arm64 verification failures at Task E3 or F3.
- **No new tests in this PR.** It's a migration, not a feature. The existing test suite is what gates correctness.
