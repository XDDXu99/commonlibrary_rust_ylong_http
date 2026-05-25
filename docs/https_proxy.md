# HTTPS 代理

`ylong_http_client` 在异步 HTTP/1.1 TLS 路径上支持 HTTPS 代理传输。HTTPS
代理表示客户端先与代理建立 TLS 连接，再通过该加密连接转发普通代理请求或
CONNECT 隧道；它不同于“通过明文 HTTP 代理访问 HTTPS 目标站点”。

HTTPS 目标站点经过 HTTPS 代理时，连接阶段如下：

```text
Client -> TCP(proxy) -> TLS(proxy) -> CONNECT target -> TLS(target) -> HTTPS request
```

HTTP 目标站点经过 HTTPS 代理时，请求仍使用代理所需的 absolute-form：

```http
GET http://target.example/path HTTP/1.1
```

## 代理选择

`Proxy::all`、`Proxy::http` 和 `Proxy::https` 描述哪些目标请求会被代理拦截。
代理 URL 的 scheme 才描述代理本身的传输方式：

```rust
Proxy::all("http://proxy.example:8080")   // 明文 HTTP 代理
Proxy::all("https://proxy.example:8443")  // HTTPS 代理
```

`Proxy::https("...")` 的含义是“代理 HTTPS 目标请求”，不等价于“代理服务器本身
使用 HTTPS 传输”。如果需要 HTTPS 代理，请在代理 URL 中使用 `https://`。

## TLS 配置

proxy TLS 与 target TLS 使用独立配置，互不影响：

- `tls_ca_file`、`danger_accept_invalid_certs`、
  `danger_accept_invalid_hostnames` 只作用于目标站点 TLS。
- `proxy_tls_ca_file`、`danger_accept_invalid_proxy_certs`、
  `danger_accept_invalid_proxy_hostnames` 只作用于 HTTPS 代理 TLS。
- `proxy_tls_certificate_file`、`proxy_tls_certificate_chain_file`、
  `proxy_tls_private_key_file` 用于 HTTPS 代理 mTLS 客户端认证。
- `proxy_min_tls_version`、`proxy_max_tls_version`、`proxy_tls_cipher_list`
  只作用于 HTTPS 代理 TLS。
- proxy TLS 使用 proxy host 做 SNI 和 hostname verification。
- target TLS 使用 target host 做 SNI 和 hostname verification。

默认行为会校验代理证书和目标站点证书。`danger_*` API 会关闭对应证书或主机名校验，
只应在测试或受控环境中使用。

```rust
use ylong_http_client::{Proxy, TlsFileType, TlsVersion};
use ylong_http_client::async_impl::ClientBuilder;

let proxy = Proxy::all("https://proxy.example:8443").build()?;
let client = ClientBuilder::new()
    .proxy(proxy)
    .proxy_tls_ca_file("proxy-ca.pem")
    .proxy_tls_certificate_file("proxy-client.pem", TlsFileType::PEM)
    .proxy_tls_private_key_file("proxy-client.key", TlsFileType::PEM)
    .proxy_min_tls_version(TlsVersion::TLS_1_2)
    .proxy_max_tls_version(TlsVersion::TLS_1_3)
    .proxy_tls_cipher_list("DEFAULT:!aNULL:!eNULL")
    .tls_ca_file("target-ca.pem")
    .build()?;
```

当前 HTTPS 代理实现覆盖 `async + tokio_base + http1_1 + tls_default` 组合。
HTTP/1.1 代理连接不设置 proxy ALPN；target ALPN 仍由普通 HTTP 版本配置控制。

## 连接池与安全行为

连接池 key 会区分 target scheme、target authority、proxy scheme、
proxy authority、proxy basic auth，以及 Client 构建时生成的 TLS 配置标识。
这可以避免直连、HTTP 代理、HTTPS 代理和不同 TLS 配置的客户端错误复用连接。

`Proxy-Authorization` 只发送给代理服务器。CONNECT 返回 407 或其他非 2xx 状态时，
客户端不会继续进行目标站点 TLS 握手。

## Benchmark 状态

当前仓库尚未提交 HTTPS 代理与 libcurl 的性能对比结果，也不能声明已达到 20%+
性能提升目标。有效 benchmark 需要使用相同的本地 HTTPS 代理、相同 HTTP target、
相同请求数、并发度和 keep-alive 条件，并分别统计冷连接与连接复用场景。建议至少
报告总耗时、吞吐量、平均延迟、P95 和 P99。

## 构建说明

启用 C OpenSSL TLS 能力时，`build.rs` 会自动链接 `ssl` 和 `crypto`。如果本机
OpenSSL 库不在平台默认搜索路径中，可以通过 `OPENSSL_LIB_DIR` 指定库目录。
