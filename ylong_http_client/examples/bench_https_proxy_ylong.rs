// Copyright (c) 2023 Huawei Device Co., Ltd.
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Local HTTPS proxy benchmark helper for HTTP target requests.

use std::env;
use std::error::Error;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::str;
use std::time::{Duration, Instant};

use openssl::ssl::{Ssl, SslAcceptor, SslFiletype, SslMethod};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_openssl::SslStream;
use ylong_http_client::async_impl::{
    Addrs, Body, Client, Request, Resolver, SocketFuture, StdError,
};
use ylong_http_client::Proxy;

type BenchError = Box<dyn Error + Send + Sync>;

const DEFAULT_TARGET_ADDR: &str = "127.0.0.1:18080";
const DEFAULT_PROXY_ADDR: &str = "127.0.0.1:18443";
const DEFAULT_BODY_SIZE: usize = 1024;
const HEADER_LIMIT: usize = 16 * 1024;

#[derive(Clone)]
struct LocalResolver;

impl Resolver for LocalResolver {
    fn resolve(&self, authority: &str) -> SocketFuture<'_> {
        let port = authority
            .rsplit_once(':')
            .map(|(_, port)| port.to_string())
            .unwrap_or_default();
        Box::pin(async move {
            let port = port.parse::<u16>().map_err(|e| Box::new(e) as StdError)?;
            let addrs = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[derive(Clone)]
struct YlongBenchConfig {
    target_url: String,
    proxy_url: String,
    proxy_ca: Option<String>,
    proxy_insecure: bool,
    requests: usize,
    concurrency: usize,
    response_size: usize,
    keep_alive: bool,
}

#[derive(Default)]
struct WorkerReport {
    latencies: Vec<f64>,
    first_ms: Option<f64>,
    steady_sum_ms: f64,
    steady_count: usize,
    errors: usize,
    body_bytes: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), BenchError> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let target_addr = arg_value(&args, "--target-addr", DEFAULT_TARGET_ADDR);
            let proxy_addr = arg_value(&args, "--proxy-addr", DEFAULT_PROXY_ADDR);
            let cert = arg_value(&args, "--cert", "ylong_http_client/tests/file/cert.pem");
            let key = arg_value(&args, "--key", "ylong_http_client/tests/file/key.pem");
            let body_size = arg_value(&args, "--body-size", DEFAULT_BODY_SIZE.to_string().as_str())
                .parse::<usize>()?;
            run_servers(&target_addr, &proxy_addr, &cert, &key, body_size).await
        }
        Some("ylong") => {
            let requests = arg_value(&args, "--requests", "1000").parse::<usize>()?;
            let concurrency = arg_value(&args, "--concurrency", "10").parse::<usize>()?;
            let config = YlongBenchConfig {
                target_url: arg_value(&args, "--target-url", "http://foobar.com:18080/bench"),
                proxy_url: arg_value(&args, "--proxy-url", "https://foobar.com:18443"),
                proxy_ca: optional_arg(&args, "--proxy-ca"),
                proxy_insecure: has_flag(&args, "--proxy-insecure"),
                requests,
                concurrency,
                response_size: arg_value(
                    &args,
                    "--response-size",
                    DEFAULT_BODY_SIZE.to_string().as_str(),
                )
                .parse::<usize>()?,
                keep_alive: !has_flag(&args, "--cold"),
            };
            run_ylong(config).await
        }
        _ => {
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  bench_https_proxy_ylong serve [--target-addr ADDR] [--proxy-addr ADDR] \\
         [--cert PEM] [--key PEM] [--body-size N]\n  bench_https_proxy_ylong ylong \\
         --target-url URL --proxy-url URL [--proxy-ca PEM|--proxy-insecure] \\
         --requests N --concurrency N [--response-size N] [--keep-alive|--cold]"
    );
}

fn arg_value(args: &[String], name: &str, default: &str) -> String {
    optional_arg(args, name).unwrap_or_else(|| default.to_string())
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

async fn run_servers(
    target_addr: &str,
    proxy_addr: &str,
    cert: &str,
    key: &str,
    body_size: usize,
) -> Result<(), BenchError> {
    let target = TcpListener::bind(target_addr).await?;
    let proxy = TcpListener::bind(proxy_addr).await?;
    let target_addr = target.local_addr()?;
    let proxy_addr = proxy.local_addr()?;
    let body = vec![b'x'; body_size];
    let acceptor = tls_acceptor(cert, key)?;

    println!(
        "READY target={} proxy={} body_size={}",
        target_addr, proxy_addr, body_size
    );

    tokio::spawn(async move {
        if let Err(err) = accept_target_loop(target, body).await {
            eprintln!("target server stopped: {err}");
        }
    });

    accept_proxy_loop(proxy, acceptor).await
}

fn tls_acceptor(cert: &str, key: &str) -> Result<SslAcceptor, BenchError> {
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls())?;
    builder.set_private_key_file(key, SslFiletype::PEM)?;
    builder.set_certificate_chain_file(cert)?;
    Ok(builder.build())
}

async fn accept_target_loop(listener: TcpListener, body: Vec<u8>) -> Result<(), BenchError> {
    let body = std::sync::Arc::new(body);
    loop {
        let (stream, _) = listener.accept().await?;
        let body = body.clone();
        tokio::spawn(async move {
            let _ = handle_target_connection(stream, body).await;
        });
    }
}

async fn handle_target_connection(
    mut stream: TcpStream,
    body: std::sync::Arc<Vec<u8>>,
) -> io::Result<()> {
    loop {
        let Some(head) = read_headers(&mut stream).await? else {
            return Ok(());
        };
        let close = header_contains(&head, "connection", "close");
        let connection = if close { "close" } else { "keep-alive" };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
            body.len(),
            connection
        );
        let mut response = Vec::with_capacity(head.len() + body.len());
        response.extend_from_slice(head.as_bytes());
        response.extend_from_slice(&body);
        stream.write_all(&response).await?;
        stream.flush().await?;
        if close {
            return Ok(());
        }
    }
}

async fn accept_proxy_loop(listener: TcpListener, acceptor: SslAcceptor) -> Result<(), BenchError> {
    let acceptor = std::sync::Arc::new(acceptor);
    loop {
        let (stream, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let _ = handle_proxy_connection(stream, acceptor).await;
        });
    }
}

async fn handle_proxy_connection(
    stream: TcpStream,
    acceptor: std::sync::Arc<SslAcceptor>,
) -> Result<(), BenchError> {
    let ssl = Ssl::new(acceptor.context())?;
    let mut client = SslStream::new(ssl, stream)?;
    Pin::new(&mut client).accept().await?;

    let mut target_authority = String::new();
    let mut target = None;
    loop {
        let Some(head) = read_headers(&mut client).await? else {
            return Ok(());
        };
        let (authority, rewritten) = rewrite_proxy_request(&head)?;
        if target_authority != authority {
            target = Some(connect_loopback(&authority).await?);
            target_authority = authority;
        }
        let write_result = target
            .as_mut()
            .expect("target stream must exist")
            .write_all(&rewritten)
            .await;
        if let Err(err) = write_result {
            target = Some(connect_loopback(&target_authority).await?);
            target
                .as_mut()
                .expect("target stream must exist")
                .write_all(&rewritten)
                .await
                .map_err(|_| err)?;
        }
        target
            .as_mut()
            .expect("target stream must exist")
            .flush()
            .await?;

        let response = read_response(target.as_mut().expect("target stream must exist")).await?;
        client.write_all(&response).await?;
        client.flush().await?;
    }
}

async fn connect_loopback(authority: &str) -> io::Result<TcpStream> {
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port)
        .unwrap_or("80")
        .parse::<u16>()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await
}

async fn read_headers<S>(stream: &mut S) -> io::Result<Option<Vec<u8>>>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            if head.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "eof while reading headers",
            ));
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(Some(head));
        }
        if head.len() > HEADER_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "headers are too large",
            ));
        }
    }
}

fn rewrite_proxy_request(head: &[u8]) -> io::Result<(String, Vec<u8>)> {
    let text =
        str::from_utf8(head).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?;
    let uri = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing uri"))?;
    let version = parts.next().unwrap_or("HTTP/1.1");
    let (authority, path) = parse_http_uri(uri)?;

    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("proxy-authorization:") || lower.starts_with("proxy-connection:") {
            continue;
        }
        if lower.starts_with("host:") {
            has_host = true;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str("Host: ");
        out.push_str(&authority);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    Ok((authority, out.into_bytes()))
}

fn parse_http_uri(uri: &str) -> io::Result<(String, String)> {
    let rest = uri.strip_prefix("http://").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "benchmark proxy only accepts http:// absolute-form requests",
        )
    })?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    Ok((authority.to_string(), path.to_string()))
}

async fn read_response(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let head = read_headers(stream)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "target closed"))?;
    let content_length = content_length(&head).unwrap_or(0);
    let mut response = head;
    let old_len = response.len();
    response.resize(old_len + content_length, 0);
    stream.read_exact(&mut response[old_len..]).await?;
    Ok(response)
}

fn content_length(head: &[u8]) -> Option<usize> {
    let text = str::from_utf8(head).ok()?;
    text.split("\r\n").find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
    })
}

fn header_contains(head: &[u8], name: &str, value: &str) -> bool {
    let Ok(text) = str::from_utf8(head) else {
        return false;
    };
    text.split("\r\n").any(|line| {
        line.split_once(':')
            .map(|(header, header_value)| {
                header.eq_ignore_ascii_case(name)
                    && header_value
                        .split(',')
                        .any(|part| part.trim().eq_ignore_ascii_case(value))
            })
            .unwrap_or(false)
    })
}

async fn run_ylong(config: YlongBenchConfig) -> Result<(), BenchError> {
    if config.requests == 0 || config.concurrency == 0 {
        return Err("requests and concurrency must be greater than zero".into());
    }

    let started = Instant::now();
    let mut handles = Vec::with_capacity(config.concurrency);
    for worker in 0..config.concurrency {
        let count = worker_request_count(config.requests, config.concurrency, worker);
        if count == 0 {
            continue;
        }
        let config = config.clone();
        handles.push(tokio::spawn(async move {
            run_ylong_worker(config, count)
                .await
                .map(|report| (worker, report))
        }));
    }

    let mut latencies = Vec::with_capacity(config.requests);
    let mut first_request_sum = 0.0;
    let mut first_request_count = 0usize;
    let mut steady_sum = 0.0;
    let mut steady_count = 0usize;
    let mut per_worker_errors = vec![0usize; config.concurrency];
    let mut total_errors = 0usize;
    let mut body_bytes = 0usize;
    for handle in handles {
        let (worker, report) = handle.await.map_err(|err| err.to_string())??;
        if let Some(first_ms) = report.first_ms {
            first_request_sum += first_ms;
            first_request_count += 1;
        }
        steady_sum += report.steady_sum_ms;
        steady_count += report.steady_count;
        if let Some(errors) = per_worker_errors.get_mut(worker) {
            *errors = report.errors;
        }
        total_errors += report.errors;
        body_bytes += report.body_bytes;
        latencies.extend(report.latencies);
    }
    let total = started.elapsed();
    let diagnostics = Diagnostics {
        first_request_ms: average_or_zero(first_request_sum, first_request_count),
        steady_avg_ms: average_or_zero(steady_sum, steady_count),
        total_errors,
        per_worker_errors,
        body_bytes,
    };
    print_stats("ylong", &config, total, &mut latencies, diagnostics);
    Ok(())
}

async fn run_ylong_worker(
    config: YlongBenchConfig,
    count: usize,
) -> Result<WorkerReport, BenchError> {
    let mut latencies = Vec::with_capacity(count);
    let mut report = WorkerReport::default();
    let keep_alive_client = if config.keep_alive {
        Some(build_client(&config)?)
    } else {
        None
    };

    for request_index in 0..count {
        let started = Instant::now();
        let result = if let Some(client) = keep_alive_client.as_ref() {
            send_ylong_request(client, &config.target_url).await
        } else {
            let client = build_client(&config)?;
            send_ylong_request(&client, &config.target_url).await
        };

        match result {
            Ok(bytes) => {
                let latency = started.elapsed().as_secs_f64() * 1000.0;
                if request_index == 0 {
                    report.first_ms = Some(latency);
                } else {
                    report.steady_sum_ms += latency;
                    report.steady_count += 1;
                }
                report.body_bytes += bytes;
                latencies.push(latency);
            }
            Err(_) => {
                report.errors += 1;
            }
        }
    }
    report.latencies = latencies;
    Ok(report)
}

fn build_client(config: &YlongBenchConfig) -> Result<Client, BenchError> {
    let proxy = Proxy::all(config.proxy_url.as_str()).build()?;
    let mut builder = Client::builder().dns_resolver(LocalResolver).proxy(proxy);
    if let Some(path) = config.proxy_ca.as_deref() {
        builder = builder.proxy_tls_ca_file(path);
    }
    if config.proxy_insecure {
        builder = builder
            .danger_accept_invalid_proxy_certs(true)
            .danger_accept_invalid_proxy_hostnames(true);
    }
    Ok(builder.build()?)
}

async fn send_ylong_request(client: &Client, target_url: &str) -> Result<usize, BenchError> {
    let request = Request::builder()
        .method("GET")
        .url(target_url)
        .body(Body::empty())?;
    let mut response = client.request(request).await?;
    if response.status().as_u16() != 200 {
        return Err(format!("unexpected status {}", response.status().as_u16()).into());
    }
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        let read = response.data(&mut buf).await?;
        if read == 0 {
            return Ok(total);
        }
        total += read;
    }
}

fn worker_request_count(requests: usize, concurrency: usize, worker: usize) -> usize {
    let base = requests / concurrency;
    let remainder = requests % concurrency;
    base + usize::from(worker < remainder)
}

fn print_stats(
    client: &str,
    config: &YlongBenchConfig,
    total: Duration,
    latencies: &mut [f64],
    diagnostics: Diagnostics,
) {
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let total_ms = total.as_secs_f64() * 1000.0;
    let avg = average_or_zero(latencies.iter().sum::<f64>(), latencies.len());
    let p95 = percentile(latencies, 0.95);
    let p99 = percentile(latencies, 0.99);
    let min = latencies.first().copied().unwrap_or(0.0);
    let max = latencies.last().copied().unwrap_or(0.0);
    let rps = config.requests as f64 / total.as_secs_f64();
    let mode = if config.keep_alive {
        "keep-alive"
    } else {
        "cold"
    };
    let per_worker_errors = diagnostics
        .per_worker_errors
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let (requests_per_worker_min, requests_per_worker_max) =
        worker_request_count_bounds(config.requests, config.concurrency);
    println!(
        "client={client} mode={mode} requests={} concurrency={} total_ms={:.3} rps={:.3} avg_ms={:.3} p95_ms={:.3} p99_ms={:.3} first_request_ms={:.3} steady_avg_ms={:.3} min_ms={:.3} max_ms={:.3} body_bytes={} response_size={} workers={} requests_per_worker_min={} requests_per_worker_max={} total_errors={} per_worker_errors={} errors={}",
        config.requests,
        config.concurrency,
        total_ms,
        rps,
        avg,
        p95,
        p99,
        diagnostics.first_request_ms,
        diagnostics.steady_avg_ms,
        min,
        max,
        diagnostics.body_bytes,
        config.response_size,
        config.concurrency,
        requests_per_worker_min,
        requests_per_worker_max,
        diagnostics.total_errors,
        per_worker_errors,
        diagnostics.total_errors
    );
}

struct Diagnostics {
    first_request_ms: f64,
    steady_avg_ms: f64,
    total_errors: usize,
    per_worker_errors: Vec<usize>,
    body_bytes: usize,
}

fn average_or_zero(sum: f64, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

fn worker_request_count_bounds(requests: usize, concurrency: usize) -> (usize, usize) {
    if concurrency == 0 {
        return (0, 0);
    }
    let min = requests / concurrency;
    let max = min + usize::from(requests % concurrency != 0);
    (min, max)
}

fn percentile(latencies: &[f64], fraction: f64) -> f64 {
    if latencies.is_empty() {
        return 0.0;
    }
    let index = ((latencies.len() as f64 * fraction).ceil() as usize)
        .saturating_sub(1)
        .min(latencies.len() - 1);
    latencies[index]
}
