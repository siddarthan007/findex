# Security policy

## Supported versions

Security fixes are applied to the latest published Findex release. Older releases
may be asked to upgrade before a fix is backported.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting on the repository **Security** tab and include:

- the affected version and operating system;
- a minimal reproduction or proof of concept;
- the expected impact; and
- any suggested mitigation, if known.

You should receive an acknowledgement within seven days. We will coordinate a
fix and disclosure timeline with the reporter. Please avoid accessing other
people's data, degrading services, or publishing details before a fix is ready.

The project does not run a paid bug-bounty program.

## Trust boundaries

- Source, query text, deep links, MCP requests, model artifacts, update manifests, and hosted-auth callbacks are untrusted input.
- Desktop/Axum and OAuth callback listeners bind to loopback. Requests require an in-memory bearer token or one-time state and exact origin policy. Deep-link routes and sizes are allowlisted; URLs never become shell commands.
- MCP Streamable HTTP requires authentication for non-loopback binds, validates browser origins, caps request bodies, issues expiring UUID sessions, bounds replay buffers, scopes event IDs to one session, and supports explicit session deletion.
- Source preview accepts only exact indexed paths and a bounded line window. VFS input, context bundles, graph traversal, query parsing, telemetry queues, tasks, and caches all have hard count/byte/time limits.

## Account and diagnostics

Firebase's browser API key and app identifiers are public client configuration, not secrets. Google sign-in runs in the system browser; Findex validates a random one-time loopback state, exchanges identity through Firebase Auth, stores refresh credentials in the operating-system vault, and keeps only a non-secret profile on disk.

Diagnostics are opt-in and default off. With the master gate off, Findex does not create a telemetry queue or make telemetry requests. Automatic events exclude source, queries, paths, symbol names, repository identity, raw IP, country, device identifiers, and serial numbers. Optional hardware values are coarse buckets/counts. Payloads are zstd-compressed, bounded, authenticated, and stored under the signed-in user's Firestore namespace. Firestore rules deny all undeclared fields and all cross-user access. Source permission is reserved for an explicit manual attachment flow and is never used by automatic events.

Firestore location selection is permanent. The rules are intentionally kept deployable independently from Hosting/Auth; do not provision or deploy the database until the project owner has selected and documented its region.

## Dependency advisory status

- `lru` is upgraded to `0.16.4`, outside GHSA-rhfx-m35p-ff5j's affected range.
- Tauri 2.11 still resolves GTK `glib` 0.18 on Linux. This workspace pins the GTK Rust core crates to Quickture commit `40b7211287f85b7d06077dcf4457c80a4ab1c57e`, which backports the upstream iterator soundness fix. Version-only scanners may continue to report 0.18.5 even though the vulnerable implementation is replaced. Keep the alert visible and remove the patch only after the Tauri/WebKit dependency chain uses an upstream fixed release.

Release assets are produced only from version tags after locked tests, version agreement, and signing-secret guards. CLI archives and Tauri updater artifacts are signed; release checksum catalogues cover attached installers and archives. Private signing keys and Firebase CLI credentials must never be committed or logged.
