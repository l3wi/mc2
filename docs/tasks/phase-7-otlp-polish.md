# Phase 7 — OTLP + polish

**Status:** Done  
**Branch:** `feat/phase-7-otlp-polish`  
**Integrated into:** `dev` (after merge; `dev` already holds Phases 0–6)

## Scope (MVP)

| Item | Approach |
| ---- | -------- |
| OTLP metrics | `mcc-metrics` crate; endpoint via `MCC_OTLP_ENDPOINT` / `OTEL_EXPORTER_OTLP_ENDPOINT` |
| Example collector | `examples/otel/collector-config.yaml` |
| Ingress stub | Already rejected in YAML parse; keep clear error |
| `mcc doctor` | Exit code + OTLP + platform checks |
| Release | `.github/workflows/release.yml` → linux-amd64, linux-arm64, darwin-arm64 |
| Docs | `docs/guides/quickstart.md` + README status |

## Metrics names

- `mcc.agent.heartbeats`, `mcc.agent.reconciles`, `mcc.agent.reconcile_errors`
- `mcc.server.applies`, `mcc.server.schedule_binds`, `mcc.server.reschedule_unbinds`
- `mcc.server.instances{phase=…}`, `mcc.server.nodes_ready`, `mcc.server.nodes_total`

Sandbox series remain on msb-metrics (D10).
