#!/usr/bin/env bash
set -euo pipefail

REQUESTS=1000
CONCURRENCY=10
MODE="keep-alive"
CLIENT="all"
TARGET_PORT=18080
PROXY_PORT=18443
PROXY_HOST="foobar.com"
BODY_SIZE=1024
PROXY_CA="ylong_http_client/tests/file/root-ca.pem"
PROXY_CERT="ylong_http_client/tests/file/cert.pem"
PROXY_KEY="ylong_http_client/tests/file/key.pem"

usage() {
    cat <<'USAGE'
Usage: scripts/bench_https_proxy.sh [options]

Options:
  --requests N          Total request count. Default: 1000
  --concurrency N       Concurrent workers. Default: 10
  --client NAME         ylong, curl-cli, libcurl, or all. Default: all
  --keep-alive          Reuse one HTTPS proxy connection per worker. Default.
  --cold                Create a fresh client/process per request.
  --target-port PORT    Local HTTP target port. Default: 18080
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

if [[ "$REQUESTS" -le 0 || "$CONCURRENCY" -le 0 ]]; then
    echo "requests and concurrency must be greater than zero" >&2
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

BIN="$REPO_ROOT/target/debug/examples/bench_https_proxy_ylong"
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

echo "building benchmark helper..."
cargo build -p ylong_http_client --features async,tokio_base,http1_1,tls_default --example bench_https_proxy_ylong >/dev/null

"$BIN" serve \
    --target-addr "127.0.0.1:$TARGET_PORT" \
    --proxy-addr "127.0.0.1:$PROXY_PORT" \
    --cert "$PROXY_CERT" \
    --key "$PROXY_KEY" \
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

TARGET_URL="http://$PROXY_HOST:$TARGET_PORT/bench"
PROXY_URL="https://$PROXY_HOST:$PROXY_PORT"
MODE_FLAG="--keep-alive"
if [[ "$MODE" == "cold" ]]; then
    MODE_FLAG="--cold"
fi

echo "server=$(grep '^READY ' "$TMP_DIR/server.log" | tail -1)"
echo "target_url=$TARGET_URL"
echo "proxy_url=$PROXY_URL"
echo "curl_version=$(curl --version | sed -n '1p')"
if command -v curl-config >/dev/null 2>&1; then
    echo "libcurl_version=$(curl-config --version)"
else
    echo "libcurl_version=unavailable"
fi
echo "client_selection=$CLIENT"

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

if client_selected "ylong"; then
    echo "running ylong_http_client..."
    YLONG_OUTPUT="$("$BIN" ylong \
        --target-url "$TARGET_URL" \
        --proxy-url "$PROXY_URL" \
        --proxy-ca "$PROXY_CA" \
        --requests "$REQUESTS" \
        --concurrency "$CONCURRENCY" \
        --response-size "$BODY_SIZE" \
        "$MODE_FLAG")"
    echo "$YLONG_OUTPUT"
fi

if client_selected "curl-cli"; then
    CURL_OUTPUT="$(run_curl_cli)"
    echo "$CURL_OUTPUT"
fi

if client_selected "libcurl"; then
    LIBCURL_OUTPUT="$(run_libcurl)"
    echo "$LIBCURL_OUTPUT"
fi

compare_outputs "curl_cli" "$CURL_OUTPUT"
compare_outputs "libcurl" "$LIBCURL_OUTPUT"
