# HTTPS Proxy Benchmark

本文档说明如何复现 `ylong_http_client` 在 HTTPS proxy 场景下的本地性能对比。
benchmark 不访问外网，默认覆盖 `HTTP target over HTTPS proxy`，对照组包括
`ylong_http_client`、curl 命令行和 libcurl multi API。

## 运行环境

需要 Rust toolchain、OpenSSL 开发库和 `curl` 命令行。libcurl multi 对照需要额外安装
libcurl 开发头文件；Ubuntu/WSL 可使用：

```bash
sudo apt install build-essential libcurl4-openssl-dev
```

脚本优先通过 `pkg-config --cflags --libs libcurl` 编译 libcurl multi 小程序；Ubuntu/WSL
下头文件通常位于 `/usr/include/x86_64-linux-gnu/curl/curl.h`。如果仍缺少 libcurl
开发头文件，脚本会跳过 libcurl multi，对 ylong 和 curl CLI 的 smoke benchmark 仍可运行。
默认使用仓库内测试证书：

- CA：`ylong_http_client/tests/file/root-ca.pem`
- HTTPS proxy certificate：`ylong_http_client/tests/file/cert.pem`
- HTTPS proxy private key：`ylong_http_client/tests/file/key.pem`

## 运行命令

```bash
scripts/bench_https_proxy.sh --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --requests 1000 --concurrency 10 --keep-alive
```

可单独选择客户端：

```bash
scripts/bench_https_proxy.sh --client ylong --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --client curl-cli --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --client libcurl --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --client all --requests 100 --concurrency 1 --keep-alive
```

常用参数：

- `--requests N`：总请求数。
- `--concurrency N`：并发 worker 数。
- `--keep-alive`：每个 worker 复用连接，默认模式。
- `--cold`：每个请求使用冷连接；该模式主要用于观察建连成本。
- `--body-size N`：target 固定响应体大小，默认 `1024`，可设为 `65536` 或 `262144`。
- `--proxy-ca PATH` / `--proxy-cert PATH` / `--proxy-key PATH`：HTTPS proxy TLS 文件。

## Benchmark 架构

脚本先构建 `ylong_http_client/examples/bench_https_proxy_ylong.rs`，再启动一个本地 HTTP
target 和一个本地 HTTPS proxy。target 返回固定 `Content-Length` 响应体；proxy 使用
TLS 接收客户端连接，并把 absolute-form HTTP 请求转发到 target。

三组客户端使用同一个 target、同一个 HTTPS proxy、同一个 proxy CA、同一请求数、同一并发度、
同一响应体大小和同一 keep-alive/cold 设置。`ylong` 使用自定义 resolver 把 benchmark
域名映射到 `127.0.0.1`；curl CLI 和 libcurl multi 使用 `--resolve` /
`CURLOPT_RESOLVE` 固定 proxy 解析。

## 对照组语义

- `ylong_http_client`：每个 worker 持有一个 `Client`；keep-alive 模式下复用该 worker
  的 HTTPS proxy 连接；请求会完整读取 body。
- `curl-cli`：每个 worker 启动一个 curl 进程；keep-alive 模式下同一进程传入多个 URL，
  由 curl 在进程内复用连接；请求输出到 `/dev/null`，会完整读取 body。
- `libcurl`：C 小程序 `tools/bench_libcurl_https_proxy.c` 使用 libcurl multi API；每个
  worker 复用一个 easy handle，multi handle 管理并发和连接缓存；cold 模式设置
  `CURLOPT_FRESH_CONNECT` 和 `CURLOPT_FORBID_REUSE`。

curl CLI benchmark 可作为 smoke benchmark，但它不完全等价于 libcurl multi API；最终性能
结论应优先参考 libcurl multi 对照。

## 输出字段

核心输出示例：

```text
client=ylong mode=keep-alive requests=1000 concurrency=10 total_ms=...
client=curl-cli mode=keep-alive requests=1000 concurrency=10 total_ms=...
client=libcurl mode=keep-alive requests=1000 concurrency=10 total_ms=...
comparison=ylong_vs_libcurl total_time_delta_pct=... reached_20pct=...
```

字段含义：

- `total_ms`：整组请求总耗时，不包含 server 启动时间，包含首批请求的 TLS/proxy 建连时间。
- `rps`：按 `requests / total_ms` 计算的吞吐量。
- `avg_ms` / `p95_ms` / `p99_ms`：单请求耗时统计。
- `first_request_ms`：每个 worker 第一条请求的平均耗时，用于观察 TLS/proxy warmup。
- `steady_avg_ms`：排除每个 worker 第一条请求后的平均耗时。
- `min_ms` / `max_ms`：单请求最小/最大耗时。
- `body_bytes`：实际读取的响应体总字节数。
- `total_errors` / `per_worker_errors` / `errors`：错误计数。
- `total_time_delta_pct`：`(baseline_total_ms - ylong_total_ms) / baseline_total_ms * 100`；
  正数表示 ylong 更快。

## 当前限制

- 当前默认只覆盖 `HTTP target over HTTPS proxy`，尚未实现 `HTTPS target over HTTPS proxy`
  benchmark。双层 TLS 场景需要补充 HTTPS target server、target CA 参数和两侧一致的 target
  TLS 校验设置。
- cold 模式已支持，但 ylong、curl CLI、libcurl 的冷连接实现细节仍不完全相同，只适合观察趋势。
- 本地 target/proxy 是简化 mock server，适合可复现对照，不代表公网代理环境。
- 本机已通过 libcurl multi 实测；curl CLI 仍只作为 smoke benchmark，最终对照优先看
  libcurl multi。

## 本机验证结果

以下结果来自当前开发机的一次本地运行，仅说明脚本可执行；性能结论以评审环境复跑为准。
当前开发机环境：

```text
curl 7.81.0 (x86_64-pc-linux-gnu) libcurl/7.81.0 OpenSSL/3.0.2
cc 11.4.0
pkg-config libcurl: 7.81.0
libcurl header: /usr/include/x86_64-linux-gnu/curl/curl.h
```

基础验证结果如下：

| 场景 | client | total_ms | rps | avg_ms | p95_ms | p99_ms | first_request_ms | steady_avg_ms | 20%+ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 100 req / 1 concurrency / 1KB / keep-alive | ylong | 25.563 | 3911.979 | 0.233 | 0.307 | 0.372 | 2.571 | 0.209 | 否 |
| 100 req / 1 concurrency / 1KB / keep-alive | curl-cli | 30.335 | 3296.522 | 0.208 | 0.271 | 0.448 | 3.190 | 0.178 | 基线 |
| 100 req / 1 concurrency / 1KB / keep-alive | libcurl | 18.984 | 5267.535 | 0.188 | 0.257 | 0.291 | 3.323 | 0.157 | 基线 |
| 1000 req / 10 concurrency / 1KB / keep-alive | ylong | 72.915 | 13714.673 | 0.479 | 0.610 | 0.974 | 6.746 | 0.416 | 否 |
| 1000 req / 10 concurrency / 1KB / keep-alive | curl-cli | 59.517 | 16801.922 | 0.428 | 0.533 | 0.687 | 5.421 | 0.378 | 基线 |
| 1000 req / 10 concurrency / 1KB / keep-alive | libcurl | 45.425 | 22014.319 | 0.440 | 0.552 | 0.719 | 9.764 | 0.346 | 基线 |

按总耗时计算，基础验证中 ylong 相对 curl CLI 分别为 `15.731%` 和 `-22.511%`；
相对 libcurl multi 分别为 `-34.655%` 和 `-60.517%`。未达到 20%+ 性能提升目标。

三轮稳定性测试取各 client `total_ms` 中位数：

| 场景 | client | median total_ms | median rps | 对 ylong 结论 |
| --- | --- | ---: | ---: | --- |
| 1000 req / 10 concurrency / 1KB / keep-alive | ylong | 75.746 | 13202.012 | - |
| 1000 req / 10 concurrency / 1KB / keep-alive | curl-cli | 59.507 | 16804.746 | ylong 慢 `27.290%` |
| 1000 req / 10 concurrency / 1KB / keep-alive | libcurl | 46.435 | 21535.325 | ylong 慢 `63.119%` |
| 3000 req / 30 concurrency / 1KB / keep-alive | ylong | 119.728 | 25056.870 | - |
| 3000 req / 30 concurrency / 1KB / keep-alive | curl-cli | 112.134 | 26753.705 | ylong 慢 `6.772%` |
| 3000 req / 30 concurrency / 1KB / keep-alive | libcurl | 86.225 | 34792.721 | ylong 慢 `38.856%` |
| 1000 req / 10 concurrency / 64KB / keep-alive | ylong | 4412.626 | 226.622 | - |
| 1000 req / 10 concurrency / 64KB / keep-alive | curl-cli | 4461.921 | 224.119 | ylong 快 `1.105%` |
| 1000 req / 10 concurrency / 64KB / keep-alive | libcurl | 4408.574 | 226.831 | ylong 慢 `0.092%` |

结论：本机实测未证明 ylong 在 HTTPS proxy 场景下相对 libcurl multi 达到 20%+。
1KB 小响应场景 ylong 主要输在总耗时和稳定态请求延迟；64KB 响应场景三者接近，说明大响应下
传输吞吐更接近，瓶颈更可能集中在小包调度、请求构造、解析或 buffer copy 路径。

## 后续优化方向

- 对比 `first_request_ms` 与 `steady_avg_ms`，区分建连/TLS/proxy warmup 和稳定态开销。
- 优先检查 1KB 小响应场景的连接复用、body drain、request 构造、runtime 调度、
  parser/buffer copy 等路径。
- 补充 `HTTPS target over HTTPS proxy` benchmark，覆盖双层 TLS 场景。
