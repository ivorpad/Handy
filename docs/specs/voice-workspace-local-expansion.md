# Local automation and meeting speakers

The latest scope is macOS only. Earlier platform adapters remain isolated by
target configuration; further work and acceptance focus on macOS.

Add the remaining requested workflows without cloud processing, sync, accounts
or OAuth providers. Keep existing local records and earlier workspace changes.
The operator enables background observation and automatic command routing
separately; both default off. Creating the feature does not activate observation.

Background learning may inspect only the nonsecure field where Handy just
inserted a dictation. Verify the insertion against the captured baseline, stop
on target changes, new dictation, disabled learning or timeout, and reject edits
outside the inserted span. Learn conservative spelling corrections as local
vocabulary with visible origin, review and revision-safe undo. Never infer a
replacement rule or send field content to a service.

Automatic command routing uses a local conservative classifier and a captured
selection. Confident edit instructions produce a preview; ambiguous speech
continues as dictation. The original selection and spoken instruction remain
distinct. Cancellation, expiry and target checks apply before replacement.
All text generation, including legacy dictation cleanup and model listing,
accepts Apple Intelligence or a loopback model server, with proxies and redirects
disabled. Remote connector configurations become disabled without deleting
their cached documents. Local server operators control what that server does;
Handy itself never sends text or audio to a remote generation endpoint.

Meeting tracks share a monotonic session clock. Preserve gaps and startup
offsets in PCM and sidecar metadata, including after interruption. Run actual
local Sortformer diarization in its own supervised process; a source name is
not a speaker. Persist timed turns and editable participant names, merge and
reassignment. A missing or unsuitable model produces an actionable error.
Model installation is an explicit local-file selection or checksum-verified
download. Audio and text never accompany that download. Overlapping speaker
turns produce one transcript interval with multiple model candidates, rather
than duplicate text falsely assigned to each speaker. Human names require an
explicit rename; a model speaker ID does not establish a person's identity.

Windows uses WASAPI microphone/loopback capture and UI Automation. Linux uses
PulseAudio (including PipeWire's PulseAudio server) and AT-SPI2. Report missing
services or inaccessible applications honestly. Do not simulate selection
capture by copying arbitrary clipboard contents. Secure fields are excluded;
replacement verifies the original field, selection and document before changing
text. Native permissions remain under the person's control.

Dependencies: platform field adapters precede learning hooks and auto-command
snapshots; the shared capture clock precedes alignment and diarization; isolated
DIARIZE worker support precedes speaker UI. Root owns command registration,
automation settings, Cargo dependencies and local generation. Platform and
feature workers own their implementations, with explicit overlap coordination.

Verify local-only URL handling and redirect refusal with a loopback test server;
exercise independent spelling/routing negatives, cancellation races, timestamp
gaps, recovery, speaker edits and schema migration. Run the frontend build,
affected Rust tests and lint/format checks. Test real local diarization with a
pinned public model and synthetic speech. Compile platform adapters where the
toolchains are available; distinguish that from actual Windows/Linux execution.
Runtime UI and native permission journeys still need the supported automation
surface. Do not claim them from compilation or fixtures.

Rollback disables the new automation flags and removes the integration hooks.
Schema additions are additive. Do not remove stored vocabulary, recordings,
notes, cached documents or speaker corrections during rollback.
