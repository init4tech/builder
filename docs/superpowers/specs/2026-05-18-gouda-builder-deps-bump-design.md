---
date: 2026-05-18
status: approved
project: builder (init4tech/builder)
branch: feat/gouda-deps
worktree: /Users/swanpro/git/builder/.claude/worktrees/gouda-deps
related:
  - https://github.com/init4tech/signet-sdk/pull/236 (ecce6a4)
  - https://github.com/init4tech/bin-base/pull/149 (b2f92e4)
  - https://github.com/init4tech/storage/pull/64 (d5bf6fa)
  - https://github.com/init4tech/node-components/pull/144 (752fa4a)
  - https://github.com/init4tech/signet/pull/105 (0de3da6)
  - /tmp/parmigiana-deploy-progress.md
---

# Gouda builder cohort — dependency bump + image cut

## Goal

Bring `init4tech/builder` into the gouda rollup cohort: dep pins match signet PR
[#105], code compiles against the `signet-sdk` 0.16 → 0.19 family jump, and a
verified `linux/arm64` ECR image exists so node-ops can deploy it into the
`parmigiana` namespace alongside the already-running `signet-sidecar` pod.

K8s manifest changes, CI workflow arch fixes, and finops are out of scope for
this PR — owned by separate tickets / node-ops.

## Context

- Builder is currently on `main` at v1.0.0 with crates.io pins:
  `signet-* = 0.16.0-rc.17`, `init4-bin-base = 0.18.0-rc.13`,
  `node-components @ tag v0.16.0-rc.10`.
- The gouda cohort jumps to the `0.19` family pinned via git-rev (no crates.io
  publishes; see "Path B distribution" in the parmigiana progress doc).
- `BuilderConfig::constants: SignetSystemConstants` is selected entirely by env
  var `CHAIN_NAME`, which after the bump will resolve to gouda's addresses
  (Zenith `0x9872Fa44…`, Passage `0x57348c54…`, etc.). `SlotCalculator` is
  configured the same way. **No code change is required for chain selection**,
  only the dep bump.
- `Dockerfile` already honours `--platform=$TARGETPLATFORM`. Local
  `docker buildx --platform linux/arm64` is the proven path (matches signet
  cut 4).
- Builder does **not** bake a `gouda.genesis.json` like the signet binary does;
  the genesis is consumed via `signet-constants` at compile time. Dockerfile
  changes are not required.
- There is an unrelated active worktree `builder-bump-1.0.1` on
  `chore/bump-1.0.1` — leave it alone.

## Approach

**Single-shot worktree → deps bump → fix breakage inline → local arm64 push.**

One branch (`feat/gouda-deps`), one draft PR targeting `main`, one ECR image
cut. Mirrors the path the signet binary followed across its 4 cuts (with the
cache-poisoning incident already understood and avoidable).

Considered and rejected: splitting the deps bump and API-port into two PRs.
This produces a red intermediate PR with no review benefit, since the
signet-sdk team already validated the porting pattern in signet #105.

## Scope

### In scope

1. **`Cargo.toml`** — replace crates.io and node-components-tag pins with
   git-rev pins matching the cohort:

   | Crate | Was | Becomes |
   |---|---|---|
   | `init4-bin-base` | `0.18.0-rc.13` (crates.io) | `git rev = b2f92e4` |
   | `signet-constants` | `0.16.0-rc.17` (crates.io) | `git rev = ecce6a4` |
   | `signet-sim` | `0.16.0-rc.17` (crates.io) | `git rev = ecce6a4` |
   | `signet-tx-cache` | `0.16.0-rc.17` (crates.io) | `git rev = ecce6a4` |
   | `signet-types` | `0.16.0-rc.17` (crates.io) | `git rev = ecce6a4` |
   | `signet-zenith` | `0.16.0-rc.17` (crates.io) | `git rev = ecce6a4` |
   | `signet-block-processor` | tag `v0.16.0-rc.10` | `git rev = 752fa4a` |
   | `signet-genesis` | tag `v0.16.0-rc.10` | `git rev = 752fa4a` |
   | `signet-bundle` (dev-dep) | `0.16.0-rc.11` (crates.io) | `git rev = ecce6a4` |

2. **`Cargo.lock`** — regenerate via `cargo build`, commit the result.

3. **`src/` and `bin/` code fixes** — port whatever the 0.16 → 0.19 jump
   breaks. Anticipated surface area, in rough order of likelihood:
   - `src/config.rs` — `SignetSystemConstants` accessor shape; `SlotCalculator`
     `FromEnv` derive; `Authenticator`, `LocalOrAws`, `PylonClient` from
     `init4-bin-base`.
   - `src/tasks/block/sim.rs` — `signet-sim` `BlockBuild` API; `SlotCalculator`
     accessors.
   - `src/tasks/block/cfg.rs` — `SignetCfgEnv` shape.
   - `src/tasks/cache/*.rs` — `signet-tx-cache` client / response types.
   - `src/tasks/submit/prep.rs`, `flashbots.rs` — `signet-zenith` contract
     bindings; `signet-types` tx envelope helpers.
   - `src/tasks/env.rs` — `SimEnv` / `BlockEnv` shape.
   - `src/test_utils/mod.rs` — `SlotCalculator::new` signature; `SignetSystem
     Constants` construction helpers.
   - `bin/builder.rs` — likely only touched if any spawn-task signature changes.
   - `benches/sim/main.rs` — same `BlockBuild` API surface as `tasks/block`.

   **Scope creep guard:** no refactoring beyond what is needed to make
   `cargo build`, `make clippy`, and `make test` green. Stylistic tidy-ups go
   in a follow-up PR.

4. **Lint / format / doc** — `make fmt`, `make clippy`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` (per pre-push hook).

5. **Local arm64 image build + push to ECR** — tags
   `637423570300.dkr.ecr.us-east-1.amazonaws.com/builder:gouda-<short-sha>`
   and `:gouda-latest`. Arch verified both pre-push (local) and post-push
   (remote registry).

6. **Progress-doc update** — append a builder row to the "Open PRs" table in
   `/tmp/parmigiana-deploy-progress.md`, append the image digest to a new
   "Builder ECR images" section (if one doesn't exist) or to the existing
   "ECR images" section, and tick the builder sub-item under step 8.

### Out of scope

- K8s manifest / pod deploy in the `parmigiana` namespace (node-ops / separate
  ticket).
- CI workflow arm64 audit
  (`init4tech/actions/.github/workflows/ecr-build-and-push.yml@main`).
  Tracked as open question 2 in the parmigiana progress doc.
- Funding (Phase 2: sequencer ETH, order-submitter approvals, base-fee
  recipient).
- Bumping the `package.version` to 1.1.0. The cohort version-bump is owned by
  the unrelated `chore/bump-1.0.1` worktree; this PR keeps version at 1.0.0
  and lets the bump PR cascade afterwards.
- Tx-cache, pylon, sequencer service updates (subsequent items in Phase 1 of
  the parmigiana plan).

## Verification gates

Each must be green before moving to the next. Stop and report on first failure
that requires design re-evaluation (e.g. structural API changes beyond
mechanical renames).

| # | Check | Pass criterion |
|---|---|---|
| 1 | `cargo build --release --bin zenith-builder-example` | exits 0 |
| 2 | `make fmt` | no diff |
| 3 | `make clippy` | exits 0 with `-D warnings` |
| 4 | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` | exits 0 |
| 5 | `make test` | all unit tests pass |
| 6 | `cargo test --features test-utils` (integration) | all pass; if any are skipped due to network unavailability, note it explicitly in the PR description |
| 7 | `docker buildx prune --all` then `docker buildx build --platform linux/arm64 --load -t builder:gouda-local .` | image loads |
| 8 | `docker buildx imagetools inspect builder:gouda-local --format '{{ .Image.OS }}/{{ .Image.Architecture }}'` | `linux/arm64` |
| 9 | ELF `e_machine` check inside image | first byte `0xb7` (aarch64) |
| 10 | Push to ECR `:gouda-<short-sha>` and `:gouda-latest` | success |
| 11 | Re-inspect ECR remote tag arch | `linux/arm64` |

## Risks

- **API jump size.** Signet's port was ~68-line `lib.rs` rewrite. Builder
  consumes more of the SDK surface than signet's bin does (zenith contracts,
  sim, tx-cache, types, constants) so the diff here could be materially
  larger. **Mitigation:** if breakage looks structural (not mechanical
  renames), pause and surface scope before continuing.
- **buildx cache poisoning** (cut 3 incident — amd64 manifest containing
  aarch64 ELF). **Mitigation:** `docker buildx prune --all` before the first
  build, explicit `--platform linux/arm64`, dual arch verification
  (local + remote registry).
- **Reth pin skew.** Signet uses `reth tag = v1.11.0`; builder is at
  `v1.11.3`. If a transitive node-components dep forces `v1.11.0`, builder
  may need to downgrade. Surfaces at build time; small fix.
- **Diamond dep clashes.** The bin-base / signet-cold-sql diamond that bit
  storage and signet was fixed by pinning signet-cold-sql in bin-base. Builder
  doesn't depend on signet-cold-sql, so this is unlikely to recur — but the
  cohort uses `init4-bin-base` features `["perms", "aws", "pylon"]` which may
  pull in additional indirect pins.

## Deliverables

- Draft PR on `init4tech/builder`, branch `feat/gouda-deps` → `main`. PR
  number assigned by GitHub at creation time; record it in the "Open PRs"
  table of the parmigiana progress doc.
- ECR tags `gouda-<short-sha>` and `gouda-latest` pointing at a verified
  arm64 image, with digests recorded.
- Updated `/tmp/parmigiana-deploy-progress.md`:
  - Builder row added to "Open PRs" table.
  - Image digest recorded.
  - Step 8's builder sub-item ticked.

## Handoff

After approval of this spec, transition to the `superpowers:writing-plans`
skill to produce a detailed implementation plan.

[#105]: https://github.com/init4tech/signet/pull/105
