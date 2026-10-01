# Combined review remediation — 2026-09-05

The backend and sibling frontend have been updated together. The original
[review](review-2026-09-05.md) remains a historical record; its line references
and original failures describe the pre-fix source. At the time of this
remediation, changes were local and uncommitted. No releases, remote CI settings,
or operational inventories were modified during that work.

## Finding coverage

| Review finding | Implemented change | Regression evidence |
| --- | --- | --- |
| 1: Proxy origin escape and malformed URL crash | Shared production/Vite proxy validates raw paths, assigns paths on a fixed origin, rejects redirects, and catches the full handler | Double slash, malformed bracket, encoded separator, dot path, backslash, redirect and normal forwarding tests |
| 2: Authentication removal during reload | Validate against the actual listener; preserve actual immutable runtime settings across repeated reloads | IPv4/IPv6 token removal and repeated-reload tests |
| 3: SSH command transport | Send script over stdin to fixed `sh -s` | Script execution against fake loginctl; empty sessions and command failure cases |
| 4: Frontend visitor authentication | Stored backend credentials require a separate visitor token or explicit authenticated-ingress mode; non-loopback exposure requires that boundary | Missing/wrong visitor token and insecure startup configuration tests; actual browser login |
| 5: Lost ICMP transition | Retain the observation until persistence succeeds; repeat writes are idempotent | Injected first-write failure and duplicate transition write tests |
| 6: False inactivity | Require enabled collection, freshness, continuous coverage and available retained history | Continuous, disabled, stale, gapped and retention-window tests |
| 7: Token leakage in TOML diagnostics | Safe location diagnostics omit original source and nested deserializer snippets | Syntax/type/nested-field errors checked for fake-secret absence |
| 8: Host-key policy and option injection | Explicit strict SSH policy, validated destinations, argument terminators | Strict-argument and invalid destination tests |
| 9: Deadlines and unbounded work | Raw deadline includes DNS; at most 32 outstanding OS resolver jobs retain admission until completion; subprocess streams bounded; reader admission before blocking work; proxy deadline, body limit, concurrency and disconnect cancellation | Resolver/read saturation, queued write, subprocess output/deadline, proxy slow/oversized/disconnected request tests |
| 10: Inventory cloning | Immutable catalog snapshots; clone one host for lookup; set-based inventory membership | Existing storage tests, large real-API fixture and code-path inspection |
| 11: Quadratic frontend grouping | Append to room buckets; count rooms once; use sets for ignored rooms | 20,000-host grouping fixture and existing filter tests |
| 12: Full refresh cost | Compatible host pages with conditional responses; 100 rendered rows per table page; completion-scheduled polling, hidden-tab pause, error backoff; preserve full-inventory search/export | Authenticated ETag/cursor API tests, client cache tests, 1,000-host render bound, actual 10,000-host browser run |
| 13: Stale drawer and misleading failures | Select by ID, close removed hosts, guard aborted completions, show explicit history errors/retry and unavailable usage | Drawer refresh/removal, late response, history failure/retry, failed usage tests |
| 14: Duration panic | Validate durations and checked date arithmetic at query/config boundaries | Huge-duration config and actual API error checks |
| 15: Unbounded history lifetime | Configurable history retention; bounded maintenance every second independent of polling; preserve a usage anchor and monotonic history boundary | Removed-host cleanup, anchor/window and existing sample-retention tests |
| 16: Mutable tagged artifacts | Existing tagged assets must compare byte-identically; mismatches fail; rolling main artifacts remain separately mutable | Fake `gh` tests for identical and different release payloads |
| 17: Publication ignore gaps | Nested environment files, databases/sidecars, keys and certificate formats ignored; safe examples retained | Tracked-file and ignore-pattern checks for both actual repositories |
| 18: Frontend lint/CI coverage | ESM lint, locked npm checks, audit, CodeQL, Dependabot, explicit counterpart SHA workflows and common validation command | Frontend lint/test/build/audit, actionlint, local combined command |

Additional improvements include supervised host-task failure, generation-fenced
monitor writes, persistent host identity across restart, truthful system-ping
RTT, explicit collector failures, widened aggregate counts, generic HTTP 5xx
errors, checked SQLite latency conversion, and transactional schema migration.
Validated security values and resolved host settings now live in the domain;
the unused query-service abstraction was removed. SQLite queries/maintenance,
frontend selectors, data loading, dialogs and table rendering have separate
modules. Documentation now matches these boundaries.

The UI traps and restores dialog focus, makes background content inert, validates
stored preferences, tolerates unavailable storage and clipboard access, removes
persisted credentials, and neutralizes spreadsheet formulas in exports. Inert
notification/profile/saved-view-option controls were removed. Stale or disabled
last-known successes count as unknown in dashboard summaries.

## Verification

- Rust: **123 library tests + 3 binary tests**; rustfmt check, all-target Clippy
  with warnings denied, warning-free rustdoc, and safe example validation.
- Frontend: **47 tests** across component, API-client, production-proxy, shared
  security and Vite integration suites; locked npm installation, lint and build.
- Strict RustSec audit with the installed pinned `cargo-audit 0.22.2`: no findings.
  npm audit: zero vulnerabilities. No advisory ignores were added.
- Backend and frontend workflows pass actionlint. Mocked release publication and
  publication-boundary checks pass. Remote Actions/CodeQL execution is not claimed.
- The pinned musl `release-artifacts` target builds successfully. The resulting
  executable has neither `DT_NEEDED` entries nor a `PT_INTERP` header and validates
  the example config. The local default container store had a broken layer, so
  this build used an isolated temporary store. RPM packaging definitions and
  payload-comparison gates were preserved; an RPM build was not rerun locally.
- The common command `bash scripts/check-projects.sh ../simple-network-monitor-frontend`
  exercises the actual backend, production proxy and Chromium with probes disabled.
  A 10,000-host run covers authentication, real error envelopes, pagination/304,
  search, history, refreshed details, disabled modules, keyboard focus and mobile
  width. Browser JavaScript errors: zero.

The load fixture records 1/10/50 simultaneous viewers requesting a bounded
100-host page. Successful and explicitly rejected overload responses are counted
separately in the JSON artifact. These short local bursts demonstrate admission
behavior, not sustainable production capacity or long-term storage growth.
Screenshots and measurements are in the frontend's ignored `test-results/`.

## Dependencies

Unused Actix HTTP/2 features were removed, eliminating the affected `h2` package
from the application graph. `chacha20` moved from yanked 0.10.1 to 0.10.2.
The registry supplied no yank explanation; the 0.10.2 upstream changelog records
a fix for an SSE4.1 intrinsic used by the SSE2 RNG/legacy backend
([upstream change](https://github.com/RustCrypto/stream-ciphers/pull/580)).
The frontend lockfile resolves patched `js-yaml` and `nanoid`. Vite and its React
plugin are development dependencies; production proxy code uses Node built-ins.

## Rollout requirements

1. Back up the SQLite database before upgrading. Schema v2 records host identity,
   observation coverage and the retained-history boundary. The first v1 upgrade
   clears current observations whose identity cannot be verified, preserving
   historical events. Downgrading to a v1 binary requires restoring the backup.
2. If the frontend stores `SNM_API_TOKEN`, configure a distinct
   `SNM_FRONTEND_TOKEN`, or explicitly configure `SNM_TRUST_AUTH_PROXY=true` behind
   an authenticated, exclusive ingress. Browser-entered tokens are no longer
   persisted and must be entered again after a page reload.
3. Publish the coordinated commits before selecting a remote revision pair.
   Set `SNM_FRONTEND_REF` on the backend and `SNM_BACKEND_REF` on the frontend
   to full commit SHAs; repository names can be overridden with their matching
   `*_REPOSITORY` variables. The paired workflow also accepts an explicit
   frontend repository/SHA manually. Require the resulting checks in repository
   settings. These settings cannot be populated with unpublished working-tree
   revisions, and were not changed by this local task.

The paged endpoint returns successive current snapshots, not one multi-request
transaction. The old filtered array endpoint remains compatible. The frontend
still holds the complete inventory for global filtering/export, while transport
pages, conditional caching and row limits bound individual request/render work.
OS DNS calls may continue after the probe stops waiting; the separate admission
limit prevents an accumulating resolver queue. Larger or sustained deployments
should use the recorded load fixture as a starting point for capacity testing.
