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

脚本使用 release profile 构建并运行
`target/release/examples/bench_https_proxy_ylong`，再启动一个本地 HTTP target 和一个本地
HTTPS proxy。target 返回固定 `Content-Length` 响应体；proxy 使用 TLS 接收客户端连接，
并把 absolute-form HTTP 请求转发到 target。

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
  `CURLOPT_FRESH_CONNECT` 和 `CURLOPT_FORBID_REUSE`。脚本通过 `pkg-config` 获取 libcurl
  编译参数，并使用 `cc -O2` 构建该 C 小程序。

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
ylong build: release
libcurl build: cc -O2 with pkg-config cflags/libs
```

说明：早期 benchmark 脚本使用 debug profile 构建 ylong helper，而 libcurl C 程序使用
`-O2`，该对比不公平。当前结果均来自 release ylong helper 和同一 release binary 提供的
本地 target/proxy server。

release 基础验证结果如下：

| 场景 | client | total_ms | rps | avg_ms | p95_ms | p99_ms | first_request_ms | steady_avg_ms | 20%+ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1000 req / 10 concurrency / 1KB / keep-alive | ylong | 48.883 | 20456.897 | 0.258 | 0.279 | 0.399 | 6.797 | 0.192 | 否 |
| 1000 req / 10 concurrency / 1KB / keep-alive | curl-cli | 43.361 | 23062.199 | 0.278 | 0.357 | 0.503 | 4.463 | 0.235 | 基线 |
| 1000 req / 10 concurrency / 1KB / keep-alive | libcurl | 26.943 | 37115.678 | 0.265 | 0.279 | 0.442 | 10.035 | 0.167 | 基线 |
| 100 req / 1 concurrency / 1KB / cold | ylong | 204.928 | 487.976 | 2.049 | 2.240 | 3.143 | 4.322 | 2.026 | 否 |
| 100 req / 1 concurrency / 1KB / cold | curl-cli | 695.489 | 143.784 | 2.927 | 3.319 | 3.441 | 3.276 | 2.923 | 基线 |
| 100 req / 1 concurrency / 1KB / cold | libcurl | 179.562 | 556.911 | 1.793 | 1.968 | 2.209 | 2.968 | 1.781 | 基线 |

按总耗时计算，release 基础验证中 ylong 相对 libcurl multi 在 1000/c10/1KB keep-alive
场景为 `-81.431%`；cold smoke 场景为 `-14.127%`。ylong 在 cold smoke 中相对 curl CLI
快 `70.535%`，但 curl CLI 不是最终 libcurl 组件对照。未达到相对 libcurl multi 的
20%+ 性能提升目标。

三轮稳定性测试取各 client `total_ms` 中位数：

| 场景 | client | median total_ms | median rps | 对 ylong 结论 |
| --- | --- | ---: | ---: | --- |
| 1000 req / 10 concurrency / 1KB / keep-alive | ylong | 48.883 | 20456.897 | - |
| 1000 req / 10 concurrency / 1KB / keep-alive | curl-cli | 43.711 | 22877.537 | ylong 慢 `11.831%` |
| 1000 req / 10 concurrency / 1KB / keep-alive | libcurl | 28.643 | 34912.380 | ylong 慢 `70.663%` |
| 3000 req / 30 concurrency / 1KB / keep-alive | ylong | 88.650 | 33841.069 | - |
| 3000 req / 30 concurrency / 1KB / keep-alive | curl-cli | 87.345 | 34346.557 | ylong 慢 `1.494%` |
| 3000 req / 30 concurrency / 1KB / keep-alive | libcurl | 79.097 | 37928.167 | ylong 慢 `12.078%` |
| 1000 req / 10 concurrency / 64KB / keep-alive | ylong | 4434.731 | 225.493 | - |
| 1000 req / 10 concurrency / 64KB / keep-alive | curl-cli | 4470.581 | 223.685 | ylong 快 `0.802%` |
| 1000 req / 10 concurrency / 64KB / keep-alive | libcurl | 4366.644 | 229.009 | ylong 慢 `1.559%` |

结论：本机实测未证明 ylong 在 HTTPS proxy 场景下相对 libcurl multi 达到 20%+。
release 构建显著缩小了差距，但 1KB 小响应场景仍慢于 libcurl multi；64KB 响应场景三者
接近，说明大响应下传输吞吐不是主要差异。cold smoke 中 ylong 快于 curl CLI，但仍慢于
libcurl multi，不能用于证明相对 libcurl 达标。

## 后续优化方向

- 对比 `first_request_ms` 与 `steady_avg_ms`，区分建连/TLS/proxy warmup 和稳定态开销。
- 优先检查 1KB 小响应场景的连接复用、body drain、request 构造、runtime 调度、
  parser/buffer copy 等路径。
- 补充 `HTTPS target over HTTPS proxy` benchmark，覆盖双层 TLS 场景。
