# HTTPS Proxy Benchmark

本文档说明如何复现 `ylong_http_client` 在 HTTP over HTTPS proxy 场景下与
`curl` 的本地性能对比。

## 运行环境

需要安装 Rust toolchain、OpenSSL 开发库和 `curl`。benchmark 全程只访问
`127.0.0.1`，不依赖外网。默认使用仓库内测试证书：

- CA：`ylong_http_client/tests/file/root-ca.pem`
- HTTPS proxy certificate：`ylong_http_client/tests/file/cert.pem`
- HTTPS proxy private key：`ylong_http_client/tests/file/key.pem`

## 运行命令

```bash
scripts/bench_https_proxy.sh --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --requests 1000 --concurrency 10 --keep-alive
```

可选参数：

```bash
scripts/bench_https_proxy.sh \
  --requests 1000 \
  --concurrency 10 \
  --keep-alive \
  --proxy-ca ylong_http_client/tests/file/root-ca.pem
```

`--cold` 会让 ylong 每次请求创建新 `Client`，curl 每次请求启动一次单请求传输。
该模式主要用于观察冷连接成本，规模较大时耗时会明显增加。

## Benchmark 结构

脚本会先构建 `ylong_http_client/examples/bench_https_proxy_ylong.rs`，再启动一个本地
HTTP target server 和一个本地 HTTPS proxy server。target 返回固定大小响应体；
proxy 使用 TLS 接收客户端连接，并把 absolute-form 请求转发到 target。

对照组：

- `ylong_http_client`：Rust example 的 `ylong` 模式发起请求。
- `curl` 命令行：脚本输出 `curl --version` 第一行，并用后台 worker 实现并发。

## 公平性约束

两组使用相同的 HTTP target、HTTPS proxy、target URL、proxy URL、请求数、并发数和
响应体大小。keep-alive 模式下，每个 worker 保持一条 HTTPS proxy 连接并在该连接上
发送多次请求；并发数等于 worker 数。

curl 通过 `--resolve foobar.com:<proxy_port>:127.0.0.1` 固定 proxy DNS 解析，并通过
`--proxy-cacert` 校验本地 HTTPS proxy 证书。ylong 使用自定义 resolver 把 benchmark
域名映射到 `127.0.0.1`，并通过 `proxy_tls_ca_file` 校验 proxy 证书。

## 输出字段

脚本输出两行核心结果：

```text
client=ylong mode=keep-alive requests=1000 concurrency=10 total_ms=... rps=... avg_ms=... p95_ms=... p99_ms=... errors=0
client=curl  mode=keep-alive requests=1000 concurrency=10 total_ms=... rps=... avg_ms=... p95_ms=... p99_ms=... errors=0
```

字段含义：

- `total_ms`：整组请求总耗时。
- `rps`：按总耗时计算的吞吐量。
- `avg_ms`：单请求平均耗时。
- `p95_ms` / `p99_ms`：单请求耗时分位数。
- `total_time_delta_pct`：`(curl_total_ms - ylong_total_ms) / curl_total_ms * 100`。

## 已知限制

当前 benchmark 只覆盖 HTTP target over HTTPS proxy，不覆盖 HTTPS target over HTTPS
proxy。proxy 和 target 都是本地简化实现，适合做可复现对照，不代表真实公网代理环境。

curl 命令行 keep-alive 模式通过“每个 worker 一次 curl invocation + 多个 URL 参数”
复用连接；这与 ylong 的“每个 worker 一个 Client”保持接近，但不等同于直接使用
libcurl multi API。后续如需更严格对照，可以补充 libcurl multi benchmark。

## 本机验证结果

以下结果来自当前开发机的一次本地运行，仅用于说明脚本已经可执行；性能结论应以评审环境
复跑结果为准。

环境中的 curl 版本：

```text
curl 7.81.0 (x86_64-pc-linux-gnu) libcurl/7.81.0 OpenSSL/3.0.2
```

命令：

```bash
scripts/bench_https_proxy.sh --requests 100 --concurrency 1 --keep-alive
scripts/bench_https_proxy.sh --requests 1000 --concurrency 10 --keep-alive
```

结果：

| 场景 | client | total_ms | rps | avg_ms | p95_ms | p99_ms | 20%+ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 100 req / 1 concurrency / keep-alive | ylong | 69.369 | 1441.559 | 0.597 | 1.179 | 1.323 | 否 |
| 100 req / 1 concurrency / keep-alive | curl | 41.436 | 2413.360 | 0.253 | 0.323 | 0.382 | 基线 |
| 1000 req / 10 concurrency / keep-alive | ylong | 81.994 | 12195.960 | 0.471 | 0.629 | 0.810 | 否 |
| 1000 req / 10 concurrency / keep-alive | curl | 61.771 | 16188.826 | 0.449 | 0.615 | 0.781 | 基线 |

按总耗时计算，ylong 相对 curl 的变化为：

- 100 req / 1 concurrency：`-67.412%`
- 1000 req / 10 concurrency：`-32.739%`

负数表示 ylong 总耗时更长。本机结果没有达到 HTTPS proxy 场景下 20%+ 性能提升目标。

## 当前结论

当前仓库提供可复现 benchmark 脚本，但性能是否达到 20%+ 必须以本机实际运行结果为准。
本机实测结果未达到 20%+，不要在未复测和未优化的情况下宣称达成该目标。
