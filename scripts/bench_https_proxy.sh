#!/usr/bin/env bash
#
# Copyright (c) 2023 Huawei Device Co., Ltd.
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

set -euo pipefail
export RUSTFLAGS="${RUSTFLAGS:--Awarnings}"

REQUESTS=1000
CONCURRENCY=10
ROUNDS=1
WARMUP_REQUESTS=0
MODE="keep-alive"
CLIENT="all"
TARGET_PORT=18080
PROXY_PORT=18443
PROXY_HOST="foobar.com"
TARGET_HOST="foobar.com"
TARGET_SCHEME="http"
BODY_SIZE=1024
PROXY_CA="ylong_http_client/tests/file/root-ca.pem"
PROXY_CERT="ylong_http_client/tests/file/cert.pem"
PROXY_KEY="ylong_http_client/tests/file/key.pem"
TARGET_CA="ylong_http_client/tests/file/root-ca.pem"
TARGET_CERT="ylong_http_client/tests/file/cert.pem"
TARGET_KEY="ylong_http_client/tests/file/key.pem"
RUNTIME_THREADS=""

usage() {
    cat <<'USAGE'
Usage: scripts/bench_https_proxy.sh [options]

Options:
  --requests N          Total request count. Default: 1000
  --concurrency N       Concurrent workers. Default: 10
  --rounds N            Benchmark rounds; prints median totals when N > 1. Default: 1
  --warmup-requests N   Untimed warmup requests per selected client. Default: 0
  --runtime-threads N   Tokio worker threads for the local server and ylong client.
  --official            Use the reproducible high-concurrency profile:
                        100000 requests, concurrency 30, 5 rounds, 1000 warmup requests.
  --client NAME         ylong, curl-cli, libcurl, or all. Default: all
  --keep-alive          Reuse one HTTPS proxy connection per worker. Default.
  --cold                Create a fresh client/process per request.
  --target-port PORT    Local HTTP target port. Default: 18080
  --target-scheme NAME  Target scheme: http or https. Default: http
  --target-host HOST    Hostname used for target TLS verification. Default: foobar.com
  --target-ca PATH      CA file used to verify an HTTPS target.
  --target-cert PATH    Local HTTPS target certificate.
  --target-key PATH     Local HTTPS target private key.
  --proxy-port PORT     Local HTTPS proxy port. Default: 18443
  --proxy-host HOST     Hostname used for proxy certificate verification. Default: foobar.com
  --proxy-ca PATH       CA file used to verify the HTTPS proxy. Default: ylong_http_client/tests/file/root-ca.pem
  --proxy-cert PATH     HTTPS proxy certificate. Default: ylong_http_client/tests/file/cert.pem
  --proxy-key PATH      HTTPS proxy private key. Default: ylong_http_client/tests/file/key.pem
  --body-size N         Fixed target response body size. Default: 1024
  -h, --help            Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --requests)
            REQUESTS="$2"
            shift 2
            ;;
        --concurrency)
            CONCURRENCY="$2"
            shift 2
            ;;
        --rounds)
            ROUNDS="$2"
            shift 2
            ;;
        --warmup-requests)
            WARMUP_REQUESTS="$2"
            shift 2
            ;;
        --runtime-threads)
            RUNTIME_THREADS="$2"
            shift 2
            ;;
        --official)
            REQUESTS=100000
            CONCURRENCY=30
            ROUNDS=5
            WARMUP_REQUESTS=1000
            BODY_SIZE=1024
            MODE="keep-alive"
            CLIENT="all"
            shift
            ;;
        --client)
            CLIENT="$2"
            shift 2
            ;;
        --keep-alive)
            MODE="keep-alive"
            shift
            ;;
        --cold)
            MODE="cold"
            shift
            ;;
        --target-port)
            TARGET_PORT="$2"
            shift 2
            ;;
        --target-scheme)
            TARGET_SCHEME="$2"
            shift 2
            ;;
        --target-host)
            TARGET_HOST="$2"
            shift 2
            ;;
        --target-ca)
            TARGET_CA="$2"
            shift 2
            ;;
        --target-cert)
            TARGET_CERT="$2"
            shift 2
            ;;
        --target-key)
            TARGET_KEY="$2"
            shift 2
            ;;
        --proxy-port)
            PROXY_PORT="$2"
            shift 2
            ;;
        --proxy-host)
            PROXY_HOST="$2"
            shift 2
            ;;
        --proxy-ca)
            PROXY_CA="$2"
            shift 2
            ;;
        --proxy-cert)
            PROXY_CERT="$2"
            shift 2
            ;;
        --proxy-key)
            PROXY_KEY="$2"
            shift 2
            ;;
        --body-size)
            BODY_SIZE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$REQUESTS" -le 0 || "$CONCURRENCY" -le 0 || "$ROUNDS" -le 0 || "$WARMUP_REQUESTS" -lt 0 ]]; then
    echo "requests, concurrency, and rounds must be greater than zero; warmup requests cannot be negative" >&2
    exit 2
fi
if [[ -n "$RUNTIME_THREADS" && "$RUNTIME_THREADS" -le 0 ]]; then
    echo "runtime threads must be greater than zero" >&2
    exit 2
fi
if [[ "$TARGET_SCHEME" != "http" && "$TARGET_SCHEME" != "https" ]]; then
    echo "target scheme must be http or https" >&2
    exit 2
fi

case "$CLIENT" in
    ylong|curl-cli|libcurl|all)
        ;;
    *)
        echo "client must be ylong, curl-cli, libcurl, or all" >&2
        exit 2
        ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [[ -n "$RUNTIME_THREADS" ]]; then
    export TOKIO_WORKER_THREADS="$RUNTIME_THREADS"
fi

BIN="$REPO_ROOT/target/release/examples/bench_https_proxy_ylong"
LIBCURL_SRC="$REPO_ROOT/tools/bench_libcurl_https_proxy.c"
LIBCURL_BIN="$REPO_ROOT/target/bench_libcurl_https_proxy"
TMP_DIR="$(mktemp -d /tmp/ylong_https_proxy_bench.XXXXXX)"
SERVER_PID=""

cleanup() {
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

echo "building benchmark helper profile=release..."
cargo build --release -p ylong_http_client --features async,tokio_base,http1_1,tls_default --example bench_https_proxy_ylong >/dev/null

"$BIN" serve \
    --target-addr "127.0.0.1:$TARGET_PORT" \
    --proxy-addr "127.0.0.1:$PROXY_PORT" \
    --cert "$PROXY_CERT" \
    --key "$PROXY_KEY" \
    --target-scheme "$TARGET_SCHEME" \
    --target-cert "$TARGET_CERT" \
    --target-key "$TARGET_KEY" \
    --body-size "$BODY_SIZE" \
    >"$TMP_DIR/server.log" 2>&1 &
SERVER_PID="$!"

for _ in $(seq 1 50); do
    if grep -q '^READY ' "$TMP_DIR/server.log"; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        cat "$TMP_DIR/server.log" >&2
        exit 1
    fi
    sleep 0.1
done

if ! grep -q '^READY ' "$TMP_DIR/server.log"; then
    echo "server did not become ready" >&2
    cat "$TMP_DIR/server.log" >&2
    exit 1
fi

TARGET_URL="$TARGET_SCHEME://$TARGET_HOST:$TARGET_PORT/bench"
PROXY_URL="https://$PROXY_HOST:$PROXY_PORT"
MODE_FLAG="--keep-alive"
if [[ "$MODE" == "cold" ]]; then
    MODE_FLAG="--cold"
fi

echo "server=$(grep '^READY ' "$TMP_DIR/server.log" | tail -1)"
echo "ylong_build=release binary=$BIN"
echo "target_url=$TARGET_URL"
echo "target_scheme=$TARGET_SCHEME"
echo "proxy_url=$PROXY_URL"
echo "curl_version=$(curl --version | sed -n '1p')"
if command -v curl-config >/dev/null 2>&1; then
    echo "libcurl_version=$(curl-config --version)"
else
    echo "libcurl_version=unavailable"
fi
echo "client_selection=$CLIENT"
echo "rounds=$ROUNDS warmup_requests=$WARMUP_REQUESTS runtime_threads=${RUNTIME_THREADS:-default}"

run_curl_worker() {
    local count="$1"
    local output="$2"
    local resolve="$PROXY_HOST:$PROXY_PORT:127.0.0.1"

    if [[ "$count" -eq 0 ]]; then
        : >"$output"
        return 0
    fi

    if [[ "$MODE" == "keep-alive" ]]; then
        local urls=()
        local i
        for ((i = 0; i < count; i++)); do
            urls+=(--output /dev/null --url "$TARGET_URL")
        done
        curl --silent --show-error --fail --http1.1 \
            --write-out '%{time_total} %{size_download}\n' \
            --proxy "$PROXY_URL" \
            --proxy-cacert "$PROXY_CA" \
            --cacert "$TARGET_CA" \
            --resolve "$resolve" \
            "${urls[@]}" >"$output"
    else
        local i
        : >"$output"
        for ((i = 0; i < count; i++)); do
            curl --silent --show-error --fail --http1.1 \
                --output /dev/null \
                --write-out '%{time_total} %{size_download}\n' \
                --proxy "$PROXY_URL" \
                --proxy-cacert "$PROXY_CA" \
                --cacert "$TARGET_CA" \
                --resolve "$resolve" \
                "$TARGET_URL" >>"$output"
        done
    fi
}

summarize_curl() {
    local total_ms="$1"
    shift
    local times_file="$TMP_DIR/curl.times"
    cat "$@" >"$times_file"
    local count
    count="$(awk 'NF >= 1 { count++ } END { print count + 0 }' "$times_file")"
    if [[ "$count" -eq 0 ]]; then
        echo "curl produced no timing data" >&2
        return 1
    fi
    if [[ "$count" -ne "$REQUESTS" ]]; then
        echo "curl timing count mismatch: expected $REQUESTS, got $count" >&2
        return 1
    fi

    local sorted="$TMP_DIR/curl.sorted"
    sort -n -k1,1 "$times_file" >"$sorted"
    local avg p95 p99 rps first steady min max body_bytes per_worker_errors
    avg="$(awk '{ sum += $1 * 1000 } END { printf "%.3f", sum / NR }' "$times_file")"
    p95="$(awk -v n="$count" -v p="0.95" 'BEGIN { idx = int(n * p); if (idx < n * p) idx++; if (idx < 1) idx = 1 } NR == idx { printf "%.3f", $1 * 1000 }' "$sorted")"
    p99="$(awk -v n="$count" -v p="0.99" 'BEGIN { idx = int(n * p); if (idx < n * p) idx++; if (idx < 1) idx = 1 } NR == idx { printf "%.3f", $1 * 1000 }' "$sorted")"
    rps="$(awk -v requests="$REQUESTS" -v total_ms="$total_ms" 'BEGIN { printf "%.3f", requests * 1000 / total_ms }')"
    first="$(awk 'FNR == 1 && NF >= 1 { sum += $1 * 1000; count++ } END { printf "%.3f", count ? sum / count : 0 }' "$@")"
    steady="$(awk 'FNR > 1 && NF >= 1 { sum += $1 * 1000; count++ } END { printf "%.3f", count ? sum / count : 0 }' "$@")"
    min="$(awk 'NR == 1 { printf "%.3f", $1 * 1000 }' "$sorted")"
    max="$(awk -v n="$count" 'NR == n { printf "%.3f", $1 * 1000 }' "$sorted")"
    body_bytes="$(awk '{ sum += $2 } END { printf "%.0f", sum }' "$times_file")"
    per_worker_errors="$(zero_list "$CONCURRENCY")"
    local requests_per_worker_min requests_per_worker_max
    requests_per_worker_min="$((REQUESTS / CONCURRENCY))"
    requests_per_worker_max="$requests_per_worker_min"
    if [[ "$((REQUESTS % CONCURRENCY))" -ne 0 ]]; then
        requests_per_worker_max="$((requests_per_worker_max + 1))"
    fi
    echo "client=curl-cli mode=$MODE requests=$REQUESTS concurrency=$CONCURRENCY total_ms=$total_ms rps=$rps avg_ms=$avg p95_ms=$p95 p99_ms=$p99 first_request_ms=$first steady_avg_ms=$steady min_ms=$min max_ms=$max body_bytes=$body_bytes response_size=$BODY_SIZE workers=$CONCURRENCY requests_per_worker_min=$requests_per_worker_min requests_per_worker_max=$requests_per_worker_max total_errors=0 per_worker_errors=$per_worker_errors errors=0"
}

extract_field() {
    local line="$1"
    local field="$2"
    echo "$line" | tr ' ' '\n' | awk -F= -v field="$field" '$1 == field { print $2; exit }'
}

zero_list() {
    local count="$1"
    local out=""
    local i
    for ((i = 0; i < count; i++)); do
        if [[ "$i" -eq 0 ]]; then
            out="0"
        else
            out="$out,0"
        fi
    done
    echo "$out"
}

client_selected() {
    [[ "$CLIENT" == "all" || "$CLIENT" == "$1" ]]
}

build_libcurl_bench() {
    mkdir -p "$(dirname "$LIBCURL_BIN")"
    if ! command -v cc >/dev/null 2>&1; then
        echo "libcurl_build=skipped reason=missing_cc hint=\"sudo apt install build-essential libcurl4-openssl-dev\""
        return 1
    fi
    local cflags=()
    local libs=(-lcurl)
    if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists libcurl; then
        read -r -a cflags <<<"$(pkg-config --cflags libcurl)"
        read -r -a libs <<<"$(pkg-config --libs libcurl)"
    fi
    if ! printf '#include <curl/curl.h>\nint main(void){return 0;}\n' \
        | cc -x c - "${cflags[@]}" "${libs[@]}" -o "$TMP_DIR/libcurl_probe" \
            >"$TMP_DIR/libcurl_probe.out" 2>"$TMP_DIR/libcurl_probe.err"; then
        echo "libcurl_build=skipped reason=missing_libcurl_dev hint=\"sudo apt install libcurl4-openssl-dev\""
        return 1
    fi
    if ! cc "$LIBCURL_SRC" -O2 -Wall -Wextra "${cflags[@]}" "${libs[@]}" -o "$LIBCURL_BIN"; then
        echo "failed to build libcurl benchmark from $LIBCURL_SRC" >&2
        return 2
    fi
}

run_curl_cli() {
    echo "running curl CLI..."
    rm -f "$TMP_DIR"/curl_*.times
    local start_ns end_ns curl_status
    start_ns="$(date +%s%N)"
    PIDS=()
    for ((worker = 0; worker < CONCURRENCY; worker++)); do
        count=$((REQUESTS / CONCURRENCY))
        if [[ "$worker" -lt $((REQUESTS % CONCURRENCY)) ]]; then
            count=$((count + 1))
        fi
        run_curl_worker "$count" "$TMP_DIR/curl_$worker.times" &
        PIDS+=("$!")
    done

    curl_status=0
    for pid in "${PIDS[@]}"; do
        if ! wait "$pid"; then
            curl_status=1
        fi
    done
    end_ns="$(date +%s%N)"
    if [[ "$curl_status" -ne 0 ]]; then
        echo "curl CLI benchmark failed" >&2
        return "$curl_status"
    fi

    local curl_total_ms
    curl_total_ms="$(awk -v start="$start_ns" -v end="$end_ns" 'BEGIN { printf "%.3f", (end - start) / 1000000 }')"
    summarize_curl "$curl_total_ms" "$TMP_DIR"/curl_*.times
}

run_libcurl() {
    echo "building libcurl multi benchmark..."
    set +e
    build_libcurl_bench
    local build_status="$?"
    set -e
    if [[ "$build_status" -eq 1 ]]; then
        echo "client=libcurl status=skipped reason=missing_libcurl_build_env hint=\"sudo apt install build-essential libcurl4-openssl-dev\""
        return 0
    fi
    if [[ "$build_status" -ne 0 ]]; then
        return "$build_status"
    fi

    echo "running libcurl multi..."
    "$LIBCURL_BIN" \
        --target-url "$TARGET_URL" \
        --proxy-url "$PROXY_URL" \
        --proxy-ca "$PROXY_CA" \
        --target-ca "$TARGET_CA" \
        --resolve "$PROXY_HOST:$PROXY_PORT:127.0.0.1" \
        --requests "$REQUESTS" \
        --concurrency "$CONCURRENCY" \
        --response-size "$BODY_SIZE" \
        "$MODE_FLAG"
}

compare_outputs() {
    local baseline_name="$1"
    local baseline_output="$2"
    if [[ -z "$YLONG_OUTPUT" || -z "$baseline_output" ]]; then
        return 0
    fi
    local ylong_total baseline_total
    ylong_total="$(extract_field "$YLONG_OUTPUT" "total_ms")"
    baseline_total="$(extract_field "$baseline_output" "total_ms")"
    if [[ -z "$ylong_total" || -z "$baseline_total" ]]; then
        return 0
    fi
    local delta reached
    delta="$(awk -v ylong="$ylong_total" -v baseline="$baseline_total" 'BEGIN { printf "%.3f", (baseline - ylong) * 100 / baseline }')"
    reached="$(awk -v delta="$delta" 'BEGIN { print (delta >= 20.0) ? "yes" : "no" }')"
    echo "comparison=ylong_vs_$baseline_name total_time_delta_pct=$delta reached_20pct=$reached"
}

YLONG_OUTPUT=""
CURL_OUTPUT=""
LIBCURL_OUTPUT=""

RESULTS_FILE="$TMP_DIR/results.txt"

run_selected_clients() {
    local emit="$1"
    YLONG_OUTPUT=""
    CURL_OUTPUT=""
    LIBCURL_OUTPUT=""

    if client_selected "ylong"; then
        [[ "$emit" == "yes" ]] && echo "running ylong_http_client..."
        YLONG_OUTPUT="$("$BIN" ylong \
            --target-url "$TARGET_URL" \
            --proxy-url "$PROXY_URL" \
            --proxy-ca "$PROXY_CA" \
            --target-ca "$TARGET_CA" \
            --requests "$REQUESTS" \
            --concurrency "$CONCURRENCY" \
            --response-size "$BODY_SIZE" \
            "$MODE_FLAG")"
        [[ "$emit" == "yes" ]] && echo "$YLONG_OUTPUT"
    fi

    if client_selected "curl-cli"; then
        CURL_OUTPUT="$(run_curl_cli)"
        [[ "$emit" == "yes" ]] && echo "$CURL_OUTPUT"
    fi

    if client_selected "libcurl"; then
        LIBCURL_OUTPUT="$(run_libcurl)"
        [[ "$emit" == "yes" ]] && echo "$LIBCURL_OUTPUT"
    fi

    if [[ "$emit" == "yes" ]]; then
        compare_outputs "curl_cli" "$CURL_OUTPUT"
        compare_outputs "libcurl" "$LIBCURL_OUTPUT"
        printf '%s\n' "$YLONG_OUTPUT" "$CURL_OUTPUT" "$LIBCURL_OUTPUT" \
            | grep '^client=' >>"$RESULTS_FILE" || true
    fi
}

if [[ "$WARMUP_REQUESTS" -gt 0 ]]; then
    measured_requests="$REQUESTS"
    REQUESTS="$WARMUP_REQUESTS"
    echo "running untimed warmup..."
    run_selected_clients "no"
    REQUESTS="$measured_requests"
fi

for ((round = 1; round <= ROUNDS; round++)); do
    echo "benchmark_round=$round/$ROUNDS"
    run_selected_clients "yes"
done

median_field() {
    local client="$1"
    local field="$2"
    awk -v client="$client" -v field="$field" '
        $1 == "client=" client {
            for (i = 1; i <= NF; i++) {
                split($i, pair, "=")
                if (pair[1] == field) {
                    print pair[2]
                }
            }
        }
    ' "$RESULTS_FILE" | sort -n | awk '
        { values[NR] = $1 }
        END {
            if (NR == 0) {
                exit 1
            }
            if (NR % 2 == 1) {
                printf "%.3f", values[(NR + 1) / 2]
            } else {
                printf "%.3f", (values[NR / 2] + values[NR / 2 + 1]) / 2
            }
        }
    '
}

if [[ "$ROUNDS" -gt 1 ]]; then
    for client in ylong curl-cli libcurl; do
        if grep -q "^client=$client " "$RESULTS_FILE"; then
            median_total="$(median_field "$client" total_ms)"
            median_rps="$(median_field "$client" rps)"
            echo "median client=$client rounds=$ROUNDS total_ms=$median_total rps=$median_rps"
        fi
    done
    if grep -q '^client=ylong ' "$RESULTS_FILE" && grep -q '^client=libcurl ' "$RESULTS_FILE"; then
        ylong_median="$(median_field ylong total_ms)"
        libcurl_median="$(median_field libcurl total_ms)"
        delta="$(awk -v ylong="$ylong_median" -v baseline="$libcurl_median" 'BEGIN { printf "%.3f", (baseline - ylong) * 100 / baseline }')"
        reached="$(awk -v delta="$delta" 'BEGIN { print (delta >= 20.0) ? "yes" : "no" }')"
        echo "median_comparison=ylong_vs_libcurl total_time_delta_pct=$delta reached_20pct=$reached"
    fi
fi
