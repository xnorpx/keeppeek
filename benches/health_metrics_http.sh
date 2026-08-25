#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <keeppeek-binary> <camera-count> <output-directory>" >&2
    exit 2
fi

binary=$1
camera_count=$2
output_directory=$3
port=${KEEPPEEK_BENCH_PORT:-18081}
runs=${KEEPPEEK_BENCH_RUNS:-5}
requests=${KEEPPEEK_BENCH_REQUESTS:-250}
warmup_requests=${KEEPPEEK_BENCH_WARMUP_REQUESTS:-50}
concurrency=${KEEPPEEK_BENCH_CONCURRENCY:-8}
access_key=11111111-1111-4111-8111-111111111111
endpoint="http://127.0.0.1:${port}/metrics"
authorization="Authorization: Bearer ${access_key}"
runtime_directory="${output_directory}/runtime"
config_file="${output_directory}/config.toml"

for command in ab curl ps; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command is unavailable: $command" >&2
        exit 1
    fi
done
if [[ ! -x "$binary" ]]; then
    echo "KeepPeek binary is not executable: $binary" >&2
    exit 1
fi
if [[ ! "$camera_count" =~ ^[0-9]+$ ]] || (( camera_count < 1 || camera_count > 254 )); then
    echo "camera count must be between 1 and 254" >&2
    exit 2
fi

rm -rf "$output_directory"
mkdir -p "$runtime_directory"

cat >"$config_file" <<EOF
host = "127.0.0.1"
port = ${port}
access_key = "${access_key}"

[battery_wake]
enabled = false

[storage]
medium_term_path = "${runtime_directory}/recordings"
long_term_path = "${runtime_directory}/recordings"
recording_catalog_path = "${runtime_directory}/recordings.db"
event_thumbnail_path = "${runtime_directory}/event-thumbnails"
long_term_max_gb = 0

[cameras]
EOF

for ((camera_number = 1; camera_number <= camera_count; camera_number++)); do
    camera_name=$(printf 'camera_%03d' "$camera_number")
    printf '%s = { ip = "192.0.2.%d", backend = "retina", main_rtsp_url = "rtsp://127.0.0.1:1/stream", recording_mode = "off" }\n' \
        "$camera_name" "$camera_number" >>"$config_file"
done

RUST_LOG=error "$binary" --config "$config_file" >"${output_directory}/server.log" 2>&1 &
server_pid=$!

cleanup() {
    if kill -0 "$server_pid" 2>/dev/null; then
        kill -INT "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

curl --silent --show-error --fail \
    --retry 200 --retry-all-errors --retry-connrefused --retry-delay 0 --retry-max-time 180 \
    --connect-timeout 1 --max-time 2 \
    -H "$authorization" "$endpoint" -o "${output_directory}/initial.prom"

ab -q -l -n "$warmup_requests" -c "$concurrency" -H "$authorization" "$endpoint" \
    >"${output_directory}/warmup.txt"
ps -o rss= -p "$server_pid" | tr -d ' ' >"${output_directory}/rss-before-kib.txt"

for ((run_number = 1; run_number <= runs; run_number++)); do
    result="${output_directory}/run-${run_number}.txt"
    ab -q -l -n "$requests" -c "$concurrency" -H "$authorization" "$endpoint" >"$result"
    grep -Eq '^Failed requests:[[:space:]]+0$' "$result"
done

ps -o rss= -p "$server_pid" | tr -d ' ' >"${output_directory}/rss-after-kib.txt"
wc -c <"${output_directory}/initial.prom" | tr -d ' ' \
    >"${output_directory}/response-bytes.txt"

cleanup
trap - EXIT INT TERM

echo "cameras=${camera_count} runs=${runs} requests=${requests} concurrency=${concurrency}"
echo "rss_before_kib=$(<"${output_directory}/rss-before-kib.txt") rss_after_kib=$(<"${output_directory}/rss-after-kib.txt") response_bytes=$(<"${output_directory}/response-bytes.txt")"
for ((run_number = 1; run_number <= runs; run_number++)); do
    echo "run=${run_number}"
    grep -E 'Failed requests:|Requests per second:|^[[:space:]]+(50|95|99)%' \
        "${output_directory}/run-${run_number}.txt"
done