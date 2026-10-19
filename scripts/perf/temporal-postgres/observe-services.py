"""Retain five-second container resources and PostgreSQL wait/database counters."""
import datetime
import json
import pathlib
import signal
import subprocess
import sys
import time

running = True

def stop(_signum, _frame):
    global running
    running = False

signal.signal(signal.SIGTERM, stop)

root = pathlib.Path(sys.argv[1])
containers = ['lightspeed-postgres', 'lightspeed-perf-temporal',
              'lightspeed-perf-temporal-postgres', 'lightspeed-minio']
with (root / 'container-stats.jsonl').open('w', buffering=1) as stats, (root / 'pg-observations.jsonl').open('w', buffering=1) as pg:
    while running:
        stats.write(datetime.datetime.now(datetime.timezone.utc).isoformat() + '\n')
        result = subprocess.run(['docker', 'stats', '--no-stream', '--format', '{{json .}}', *containers], capture_output=True, text=True)
        stats.write(result.stdout)
        row = {'time': time.time(), 'databases': {}}
        for container, user, db in [('lightspeed-postgres', 'lightspeed', 'lightspeed_perf_postgres'), ('lightspeed-perf-temporal-postgres', 'temporal', 'temporal'), ('lightspeed-perf-temporal-postgres', 'temporal', 'temporal_visibility')]:
            sql = "SELECT json_build_object('database', (SELECT row_to_json(s) FROM pg_stat_database s WHERE datname=current_database()), 'waits', (SELECT json_agg(w) FROM (SELECT state,wait_event_type,wait_event,count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() GROUP BY 1,2,3) w));"
            result = subprocess.run(['docker', 'exec', container, 'psql', '-XAt', '-U', user, '-d', db, '-c', sql], capture_output=True, text=True)
            row['databases'][db] = json.loads(result.stdout) if result.returncode == 0 else {'error': result.stderr}
        pg.write(json.dumps(row) + '\n')
        time.sleep(3)
