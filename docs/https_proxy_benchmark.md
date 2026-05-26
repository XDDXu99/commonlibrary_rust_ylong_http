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

如果缺少 `curl/curl.h`，脚本会跳过 libcurl multi，对 ylong 和 curl CLI 的 smoke
benchmark 仍可运行。默认使用仓库内测试证书：

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
- 当前开发机缺少 `curl/curl.h`，libcurl multi 代码已加入脚本，但本机尚未实测 libcurl multi
  结果；安装 `libcurl4-openssl-dev` 后可直接运行 `--client libcurl`。

## 本机验证结果

以下结果来自当前开发机的一次本地运行，仅说明脚本可执行；性能结论以评审环境复跑为准。
当前开发机环境：

```text
curl 7.81.0 (x86_64-pc-linux-gnu) libcurl/7.81.0 OpenSSL/3.0.2
libcurl dev headers: unavailable
```

本阶段一次验证结果如下；libcurl multi 因缺少开发头文件被跳过：

| 场景 | client | total_ms | rps | avg_ms | p95_ms | p99_ms | first_request_ms | steady_avg_ms | 20%+ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 100 req / 1 concurrency / keep-alive | ylong | 31.597 | 3164.899 | 0.271 | 0.354 | 0.496 | 5.183 | 0.221 | 否 |
| 100 req / 1 concurrency / keep-alive | curl-cli | 35.515 | 2815.712 | 0.223 | 0.296 | 0.463 | 3.601 | 0.189 | 基线 |
| 1000 req / 10 concurrency / keep-alive | ylong | 69.566 | 14374.744 | 0.455 | 0.599 | 0.969 | 4.569 | 0.413 | 否 |
| 1000 req / 10 concurrency / keep-alive | curl-cli | 74.629 | 13399.617 | 0.549 | 0.715 | 1.207 | 7.896 | 0.475 | 基线 |

按总耗时计算，本次 ylong 相对 curl CLI 分别快 `11.032%` 和 `6.784%`，未达到稳定
`20%+` 证明。100 请求场景受本地调度波动影响较大，不能单独作为最终结论。

## 后续优化方向

- 先在安装 libcurl dev 包的环境复跑 `--client libcurl`，确认与官方“libcurl 组件”对照一致。
- 对比 `first_request_ms` 与 `steady_avg_ms`，区分建连/TLS/proxy warmup 和稳定态开销。
- 检查连接复用、body drain、request 构造、runtime 调度、parser/buffer copy 等路径。
- 补充 `HTTPS target over HTTPS proxy` benchmark，覆盖双层 TLS 场景。
