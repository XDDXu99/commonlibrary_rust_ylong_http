# HTTPS Proxy Benchmark

本文档说明如何复现 `ylong_http_client` 在 HTTPS proxy 场景下的本地性能对比。
benchmark 不访问外网，支持 `HTTP target over HTTPS proxy` 和
`HTTPS target over HTTPS proxy`，对照组包括 `ylong_http_client`、curl 命令行和
libcurl multi API。

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
scripts/bench_https_proxy.sh --target-scheme https --requests 1000 --concurrency 10 --keep-alive
scripts/bench_https_proxy.sh --official
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
- `--rounds N`：连续运行 N 轮并输出各客户端总耗时和 RPS 中位数。
- `--warmup-requests N`：正式计时前为每个客户端运行不计时的预热请求。
- `--runtime-threads N`：设置本地 Rust server 和 ylong 客户端的 Tokio worker 数。
- `--official`：使用本机正式复核参数，即 100000 请求、并发 30、1KB、5 轮和 1000 次预热。
- `--keep-alive`：每个 worker 复用连接，默认模式。
- `--cold`：每个请求使用冷连接；该模式主要用于观察建连成本。
- `--body-size N`：target 固定响应体大小，默认 `1024`，可设为 `65536` 或 `262144`。
- `--target-scheme http|https`：选择 HTTP target 或 HTTPS target，默认 `http`。
- `--target-host HOST`：target TLS 使用的 hostname，默认 `foobar.com`。
- `--target-ca PATH` / `--target-cert PATH` / `--target-key PATH`：HTTPS target TLS 文件。
- `--proxy-ca PATH` / `--proxy-cert PATH` / `--proxy-key PATH`：HTTPS proxy TLS 文件。

## Benchmark 架构

脚本使用 release profile 构建并运行
`target/release/examples/bench_https_proxy_ylong`，再启动一个本地 target 和一个本地
HTTPS proxy。target 返回固定 `Content-Length` 响应体。HTTP target 场景下，proxy 把
absolute-form 请求改写为 origin-form 后转发；HTTPS target 场景下，proxy 响应 CONNECT，
随后在 TCP tunnel 中双向转发 target TLS。

三组客户端使用同一个 target、同一个 HTTPS proxy、同一个 proxy CA、同一请求数、同一并发度、
同一响应体大小和同一 keep-alive/cold 设置。双层 TLS 场景还使用同一 target CA。
`ylong` 使用自定义 resolver 把 benchmark 域名映射到 `127.0.0.1`；curl CLI 和
libcurl multi 使用 `--resolve` / `CURLOPT_RESOLVE` 固定 proxy 解析。

## 对照组语义

- `ylong_http_client`：所有 worker 共享一个 `Client` 和连接池，最大 H1 连接数与并发度一致；
  keep-alive 模式下复用 HTTPS proxy 连接，请求会完整读取 body。Client 在计时前创建。
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
median client=ylong rounds=5 total_ms=... rps=...
median_comparison=ylong_vs_libcurl total_time_delta_pct=... reached_20pct=...
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
- `median` / `median_comparison`：多轮测试的中位数及基于中位数计算的最终差值。正式结论
  不使用单轮最好值。

## 当前限制

- 默认场景仍是 `HTTP target over HTTPS proxy`；双层 TLS 需显式传入
  `--target-scheme https`。
- cold 模式已支持，但 ylong、curl CLI、libcurl 的冷连接实现细节仍不完全相同，只适合观察趋势。
- 本地 target/proxy 是简化 mock server，适合可复现对照，不代表公网代理环境。
- 本机已通过 libcurl multi 实测；curl CLI 仍只作为 smoke benchmark，最终对照优先看
  libcurl multi。
- 不同网络延迟场景尚未覆盖；当前未使用 `tc netem`、mock delay 或脚本延迟注入。
- 尚未采集 CPU time、context switch 和内存分配。256KB 结果轮间波动较大，应结合更多轮次
  和系统级 profiler 解读。
- TLS session resume、zero-copy / 减少拷贝和 batching 仍是后续优化方向，当前性能验证
  未为追求速度关闭 TLS 校验、hostname verification 或 CONNECT 安全检查。

## 本机验证结果

以下结果来自当前开发机的五轮中位数；评审环境仍应使用相同命令复跑。
当前开发机环境：

```text
curl 7.81.0 (x86_64-pc-linux-gnu) libcurl/7.81.0 OpenSSL/3.0.2
cc 11.4.0
pkg-config libcurl: 7.81.0
libcurl header: /usr/include/x86_64-linux-gnu/curl/curl.h
ylong build: release
libcurl build: cc -O2 with pkg-config cflags/libs
```

当前结果均来自 release ylong helper 和 `cc -O2` libcurl 程序。benchmark 脚本会默认
设置 `RUSTFLAGS=-Awarnings` 以屏蔽仓库既有 warning；该变量只用于本地输出降噪，不是
官方构建要求。正式高并发命令为：

```bash
scripts/bench_https_proxy.sh --official
scripts/bench_https_proxy.sh --target-scheme https --requests 10000 --concurrency 30 --rounds 5 --warmup-requests 300 --keep-alive
scripts/bench_https_proxy.sh --requests 3000 --concurrency 30 --rounds 5 --warmup-requests 300 --keep-alive --body-size 65536
scripts/bench_https_proxy.sh --requests 1000 --concurrency 30 --rounds 5 --warmup-requests 300 --keep-alive --body-size 262144
scripts/bench_https_proxy.sh --requests 100 --concurrency 10 --rounds 5 --cold
```

五轮 `total_ms` 中位数：

| 场景 | ylong ms / RPS | curl CLI ms / RPS | libcurl ms / RPS | ylong 相对 libcurl |
| --- | ---: | ---: | ---: | ---: |
| HTTP target，100000 req，c30，1KB，keep-alive | 1117.971 / 89447.728 | 1974.967 / 50633.757 | 1719.869 / 58143.977 | 快 `34.997%` |
| HTTPS target，10000 req，c30，1KB，keep-alive | 100.167 / 99833.169 | 218.457 / 45775.599 | 227.900 / 43878.829 | 快 `56.048%` |
| HTTP target，3000 req，c30，64KB，keep-alive | 4442.721 / 675.262 | 4571.824 / 656.193 | 4440.869 / 675.543 | 慢 `0.042%` |
| HTTP target，1000 req，c30，256KB，keep-alive | 1289.694 / 775.378 | 1463.369 / 683.355 | 1189.492 / 840.695 | 慢 `8.424%` |
| HTTP target，100 req，c10，1KB，cold | 93.722 / 1066.981 | 116.342 / 859.535 | 94.366 / 1059.700 | 快 `0.682%` |

正式 20%+ 结论仅限：

- HTTP target over HTTPS proxy，100000 请求、并发 30、1KB、keep-alive，
  ylong 相对 libcurl multi 快 `34.997%`。
- HTTPS target over HTTPS proxy，10000 请求、并发 30、1KB、keep-alive，
  ylong 相对 libcurl multi 快 `56.048%`。

64KB 基本持平，256KB 场景 ylong 慢于 libcurl，cold 场景不作为正式达标证据。
不同网络延迟场景未覆盖，因此不能宣称所有 HTTPS proxy 参数下均有 20%+ 提升。

脚本仍会输出 `first_request_ms`、`steady_avg_ms`、`p95_ms`、`p99_ms`、错误数和
body 字节数等诊断字段，用于定位建连成本、稳定态请求延迟和 body drain 成本。最新审计的
正式达标判断只采用五轮 `total_ms` 中位数，不使用单轮最好值。

## 后续优化方向

- 继续优化并发 10 场景的连接池查找、request 构造、body drain 和 parser/buffer copy。
- 增加 CPU time / context switch 统计，解释高并发吞吐差异。
- 使用 profiler 定位 256KB 场景中 ylong 的 body drain、buffer copy 和 runtime 调度成本。
