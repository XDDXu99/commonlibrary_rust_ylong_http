#!/usr/bin/env bash
set -euo pipefail

REQUESTS=1000
CONCURRENCY=10
MODE="keep-alive"
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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

BIN="$REPO_ROOT/target/debug/examples/bench_https_proxy_ylong"
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

echo "running ylong_http_client..."
YLONG_OUTPUT="$("$BIN" ylong \
    --target-url "$TARGET_URL" \
    --proxy-url "$PROXY_URL" \
    --proxy-ca "$PROXY_CA" \
    --requests "$REQUESTS" \
    --concurrency "$CONCURRENCY" \
    "$MODE_FLAG")"
echo "$YLONG_OUTPUT"

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
            --write-out '%{time_total}\n' \
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
                --write-out '%{time_total}\n' \
                --proxy "$PROXY_URL" \
                --proxy-cacert "$PROXY_CA" \
                --resolve "$resolve" \
                "$TARGET_URL" >>"$output"
        done
    fi
}

summarize_curl() {
    local times_file="$1"
    local total_ms="$2"
    local count
    count="$(wc -l <"$times_file" | tr -d ' ')"
    if [[ "$count" -eq 0 ]]; then
        echo "curl produced no timing data" >&2
        return 1
    fi
    if [[ "$count" -ne "$REQUESTS" ]]; then
        echo "curl timing count mismatch: expected $REQUESTS, got $count" >&2
        return 1
    fi

    local sorted="$TMP_DIR/curl.sorted"
    sort -n "$times_file" >"$sorted"
    local avg p95 p99 rps
    avg="$(awk '{ sum += $1 * 1000 } END { printf "%.3f", sum / NR }' "$times_file")"
    p95="$(awk -v n="$count" -v p="0.95" 'BEGIN { idx = int(n * p); if (idx < n * p) idx++; if (idx < 1) idx = 1 } NR == idx { printf "%.3f", $1 * 1000 }' "$sorted")"
    p99="$(awk -v n="$count" -v p="0.99" 'BEGIN { idx = int(n * p); if (idx < n * p) idx++; if (idx < 1) idx = 1 } NR == idx { printf "%.3f", $1 * 1000 }' "$sorted")"
    rps="$(awk -v requests="$REQUESTS" -v total_ms="$total_ms" 'BEGIN { printf "%.3f", requests * 1000 / total_ms }')"
    echo "client=curl mode=$MODE requests=$REQUESTS concurrency=$CONCURRENCY total_ms=$total_ms rps=$rps avg_ms=$avg p95_ms=$p95 p99_ms=$p99 errors=0"
}

extract_field() {
    local line="$1"
    local field="$2"
    echo "$line" | tr ' ' '\n' | awk -F= -v field="$field" '$1 == field { print $2; exit }'
}

echo "running curl..."
START_NS="$(date +%s%N)"
PIDS=()
for ((worker = 0; worker < CONCURRENCY; worker++)); do
    count=$((REQUESTS / CONCURRENCY))
    if [[ "$worker" -lt $((REQUESTS % CONCURRENCY)) ]]; then
        count=$((count + 1))
    fi
    run_curl_worker "$count" "$TMP_DIR/curl_$worker.times" &
    PIDS+=("$!")
done

CURL_STATUS=0
for pid in "${PIDS[@]}"; do
    if ! wait "$pid"; then
        CURL_STATUS=1
    fi
done
END_NS="$(date +%s%N)"
if [[ "$CURL_STATUS" -ne 0 ]]; then
    echo "curl benchmark failed" >&2
    exit "$CURL_STATUS"
fi

cat "$TMP_DIR"/curl_*.times >"$TMP_DIR/curl.times"
CURL_TOTAL_MS="$(awk -v start="$START_NS" -v end="$END_NS" 'BEGIN { printf "%.3f", (end - start) / 1000000 }')"
CURL_OUTPUT="$(summarize_curl "$TMP_DIR/curl.times" "$CURL_TOTAL_MS")"
echo "$CURL_OUTPUT"

YLONG_TOTAL="$(extract_field "$YLONG_OUTPUT" "total_ms")"
CURL_TOTAL="$(extract_field "$CURL_OUTPUT" "total_ms")"
DELTA="$(awk -v ylong="$YLONG_TOTAL" -v curl="$CURL_TOTAL" 'BEGIN { printf "%.3f", (curl - ylong) * 100 / curl }')"
REACHED="$(awk -v delta="$DELTA" 'BEGIN { print (delta >= 20.0) ? "yes" : "no" }')"
echo "comparison=ylong_vs_curl total_time_delta_pct=$DELTA reached_20pct=$REACHED"
