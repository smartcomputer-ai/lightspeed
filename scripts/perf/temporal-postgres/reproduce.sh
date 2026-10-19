#!/usr/bin/env bash
# Run from repo root. These are separate-terminal commands after preparation.
# Do not run concurrently with another runtime on ports 18080/19090/19092.
# Keep the previous SQLite archive and volumes; never use compose down -v.
# Prerequisites: local dev PostgreSQL with pg_stat_statements preloaded, MinIO.
# ./dev.sh infra --no-envd  # only if the local dev services are not already up
# docker stop lightspeed-temporal  # preserve the old SQLite volume, reduce idle load
scripts/perf/temporal-postgres/up.sh
cargo build --release -p temporal-runtime -p llm-simulator --locked
scripts/perf/temporal-postgres/prepare-runtime.sh
source scripts/dev/env.sh >/dev/null
source scripts/perf/temporal-postgres/env.sh
# Terminal 1: target/release/lightspeed-runtime
# Terminal 2: target/release/llm-simulator scripts/perf/fixtures/simulator.json
# Terminal 3 (same environment):
# python3 scripts/perf/observe.py "$PERF_OUTPUT/observations.jsonl" RUNTIME_PID SIMULATOR_PID
# Terminal 4:
# python3 scripts/perf/temporal-postgres/observe-services.py "$PERF_OUTPUT"
# Terminal 5 (same environment):
# scripts/perf/sweep.sh "$PERF_OUTPUT/sweep"
# Repeats: npx tsx scripts/perf/run.ts "$PERF_OUTPUT/sweep/rapid-10.json" "$PERF_OUTPUT/sweep/rapid-10-repeat"
# Additional readers: copy idle-readers fixture, durationMs=30000, concurrency=4096,
# rate=128,512,2048 respectively; run.ts CONFIG OUTPUT. Run overload last.
# Analysis:
# python3 scripts/perf/analyze.py "$PERF_OUTPUT" > "$PERF_OUTPUT/tables.txt"
# python3 scripts/perf/report.py "$PERF_OUTPUT" > "$PERF_OUTPUT/report-tables.md"

# Separate profiled repeat after baselines, before overload:
# scripts/perf/temporal-postgres/profile-step.sh "$PERF_OUTPUT/sweep/rapid-40.json" "$PERF_OUTPUT/sweep/rapid-40-profile"
# go tool pprof -top "$PERF_OUTPUT/sweep/rapid-40-profile/temporal-cpu.pb.gz"
# go tool pprof -sample_index=alloc_space -top "$PERF_OUTPUT/sweep/rapid-40-profile/temporal-allocs.pb.gz"
# python3 scripts/perf/temporal-postgres/report.py "$PERF_OUTPUT" SQLITE_BASELINE_DIRECTORY
