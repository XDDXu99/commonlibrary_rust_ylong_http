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

#![cfg(all(
    feature = "async",
    feature = "http1_1",
    feature = "__tls",
    feature = "tokio_base"
))]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll};

use openssl::ssl::{NameType, Ssl, SslAcceptor, SslFiletype, SslMethod, SslVerifyMode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio_openssl::SslStream;
use ylong_http_client::async_impl::{
    Addrs, Body, Client, Request, Resolver, SocketFuture, StdError,
};
use ylong_http_client::{Proxy, TlsFileType, TlsVersion};

const TEST_HOST: &str = "foobar.com";
const PROXY_HOST: &str = "proxy.test";
const TARGET_HOST: &str = "target.test";
const WRONG_PROXY_HOST: &str = "wrong-proxy.test";
const WRONG_TARGET_HOST: &str = "wrong-target.test";
const PROXY_AUTH: &str = "Basic dXNlcjpwYXNz";

type SniLog = Arc<Mutex<Vec<Option<String>>>>;

enum ProxyIo {
    Plain(TcpStream),
    Tls(SslStream<TcpStream>),
}

impl AsyncRead for ProxyIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxyIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

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

struct ProxyHandle {
    port: u16,
    observed: UnboundedReceiver<String>,
}

struct TargetHandle {
    port: u16,
    observed: UnboundedReceiver<String>,
    accepted: Arc<AtomicBool>,
}

fn cert_path(name: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/file");
    path.push(name);
    path.to_string_lossy().into_owned()
}

struct ServerTlsConfig {
    cert_file: String,
    key_file: String,
    client_ca_file: Option<String>,
    require_client_cert: bool,
    sni_log: Option<SniLog>,
}

impl ServerTlsConfig {
    fn new(cert_name: &str, key_name: &str) -> Self {
        Self {
            cert_file: cert_path(cert_name),
            key_file: cert_path(key_name),
            client_ca_file: None,
            require_client_cert: false,
            sni_log: None,
        }
    }

    fn default_server(require_client_cert: bool) -> Self {
        let mut config = Self::new("cert.pem", "key.pem");
        if require_client_cert {
            config.client_ca_file = Some(cert_path("root-ca.pem"));
            config.require_client_cert = true;
        }
        config
    }

    fn with_sni_log(mut self, sni_log: SniLog) -> Self {
        self.sni_log = Some(sni_log);
        self
    }
}

fn tls_acceptor(config: &ServerTlsConfig) -> SslAcceptor {
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    builder
        .set_private_key_file(&config.key_file, SslFiletype::PEM)
        .unwrap();
    builder
        .set_certificate_chain_file(&config.cert_file)
        .unwrap();
    if config.require_client_cert {
        builder
            .set_ca_file(config.client_ca_file.as_ref().unwrap())
            .unwrap();
        builder.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
    }
    if let Some(sni_log) = config.sni_log.clone() {
        builder.set_servername_callback(move |ssl, _| {
            let servername = ssl.servername(NameType::HOST_NAME).map(str::to_owned);
            sni_log.lock().unwrap().push(servername);
            Ok(())
        });
    }
    builder.build()
}

async fn accept_tls_with_config(
    stream: TcpStream,
    config: ServerTlsConfig,
) -> Result<SslStream<TcpStream>, Box<dyn std::error::Error + Send + Sync>> {
    let ssl = Ssl::new(tls_acceptor(&config).context())?;
    let mut stream = SslStream::new(ssl, stream)?;
    Pin::new(&mut stream).accept().await?;
    Ok(stream)
}

async fn read_headers<S>(stream: &mut S) -> io::Result<String>
where
    S: AsyncRead + Unpin,
{
    let mut buf = [0u8; 8192];
    let mut pos = 0;
    loop {
        let read = stream.read(&mut buf[pos..]).await?;
        if read == 0 {
            break;
        }
        pos += read;
        if buf[..pos].windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if pos == buf.len() {
            return Err(io::Error::new(io::ErrorKind::Other, "headers too long"));
        }
    }
    Ok(String::from_utf8_lossy(&buf[..pos]).into_owned())
}

async fn start_proxy(tls: bool, target: Option<SocketAddr>, status: u16) -> ProxyHandle {
    start_proxy_with_client_auth(tls, target, status, false).await
}

async fn start_mtls_proxy(target: Option<SocketAddr>, status: u16) -> ProxyHandle {
    start_proxy_with_client_auth(true, target, status, true).await
}

async fn start_proxy_with_client_auth(
    tls: bool,
    target: Option<SocketAddr>,
    status: u16,
    require_client_cert: bool,
) -> ProxyHandle {
    let tls_config = tls.then(|| ServerTlsConfig::default_server(require_client_cert));
    start_proxy_with_tls_config(tls_config, target, status).await
}

async fn start_https_proxy_with_named_cert(
    target: Option<SocketAddr>,
    status: u16,
    sni_log: SniLog,
) -> ProxyHandle {
    let tls_config = ServerTlsConfig::new(
        "https_proxy_sni_proxy_cert.pem",
        "https_proxy_sni_proxy_key.pem",
    )
    .with_sni_log(sni_log);
    start_proxy_with_tls_config(Some(tls_config), target, status).await
}

async fn start_proxy_with_tls_config(
    tls_config: Option<ServerTlsConfig>,
    target: Option<SocketAddr>,
    status: u16,
) -> ProxyHandle {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = if let Some(tls_config) = tls_config {
            let Ok(tls_stream) = accept_tls_with_config(tcp, tls_config).await else {
                return;
            };
            ProxyIo::Tls(tls_stream)
        } else {
            ProxyIo::Plain(tcp)
        };
        let head = read_headers(&mut stream).await.unwrap();
        tx.send(head.clone()).unwrap();
        if head.starts_with("CONNECT ") {
            if status != 200 {
                let resp = format!("HTTP/1.1 {status} Proxy Error\r\nContent-Length: 0\r\n\r\n");
                stream.write_all(resp.as_bytes()).await.unwrap();
                return;
            }
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut target = TcpStream::connect(target.unwrap()).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
        } else {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .await
                .unwrap();
        }
    });
    ProxyHandle { port, observed: rx }
}

async fn start_https_target() -> TargetHandle {
    start_https_target_with_config(ServerTlsConfig::default_server(false)).await
}

async fn start_https_target_with_named_cert(sni_log: SniLog) -> TargetHandle {
    let tls_config = ServerTlsConfig::new(
        "https_proxy_sni_target_cert.pem",
        "https_proxy_sni_target_key.pem",
    )
    .with_sni_log(sni_log);
    start_https_target_with_config(tls_config).await
}

async fn start_https_target_with_config(tls_config: ServerTlsConfig) -> TargetHandle {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicBool::new(false));
    let accepted_clone = accepted.clone();
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        accepted_clone.store(true, Ordering::SeqCst);
        let Ok(mut stream) = accept_tls_with_config(tcp, tls_config).await else {
            return;
        };
        let head = read_headers(&mut stream).await.unwrap();
        tx.send(head).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
            .await
            .unwrap();
    });
    TargetHandle {
        port,
        observed: rx,
        accepted,
    }
}

fn sni_log_contains(sni_log: &SniLog, expected: &str) -> bool {
    sni_log
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry.as_deref() == Some(expected))
}

async fn request_via_proxy(
    url: String,
    proxy: String,
    proxy_tls_ca: bool,
    proxy_danger: bool,
    target_tls_ca: bool,
    target_danger: bool,
    proxy_auth: bool,
) -> Result<ylong_http_client::async_impl::Response, ylong_http_client::HttpClientError> {
    request_via_proxy_with_client_cert(
        url,
        proxy,
        proxy_tls_ca,
        proxy_danger,
        target_tls_ca,
        target_danger,
        proxy_auth,
        false,
    )
    .await
}

async fn request_via_proxy_with_client_cert(
    url: String,
    proxy: String,
    proxy_tls_ca: bool,
    proxy_danger: bool,
    target_tls_ca: bool,
    target_danger: bool,
    proxy_auth: bool,
    proxy_client_cert: bool,
) -> Result<ylong_http_client::async_impl::Response, ylong_http_client::HttpClientError> {
    let mut proxy_builder = Proxy::all(proxy.as_str());
    if proxy_auth {
        proxy_builder = proxy_builder.basic_auth("user", "pass");
    }
    let mut builder = Client::builder()
        .dns_resolver(LocalResolver)
        .proxy(proxy_builder.build().unwrap());
    if proxy_tls_ca {
        builder = builder.proxy_tls_ca_file(cert_path("root-ca.pem").as_str());
    }
    if proxy_danger {
        builder = builder
            .danger_accept_invalid_proxy_certs(true)
            .danger_accept_invalid_proxy_hostnames(true);
    }
    if proxy_client_cert {
        builder = builder
            .proxy_tls_certificate_file(cert_path("cert.pem").as_str(), TlsFileType::PEM)
            .proxy_tls_private_key_file(cert_path("key.pem").as_str(), TlsFileType::PEM)
            .proxy_min_tls_version(TlsVersion::TLS_1_2)
            .proxy_max_tls_version(TlsVersion::TLS_1_3)
            .proxy_tls_cipher_list(
                "DEFAULT:!aNULL:!eNULL:!MD5:!3DES:!DES:!RC4:!IDEA:!SEED:!aDSS:!SRP:!PSK",
            );
    }
    if target_tls_ca {
        builder = builder.tls_ca_file(cert_path("root-ca.pem").as_str());
    }
    if target_danger {
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }
    let client = builder.build().unwrap();
    let request = Request::builder()
        .method("GET")
        .url(url.as_str())
        .body(Body::empty())
        .unwrap();
    client.request(request).await
}

async fn request_via_proxy_with_tls_paths(
    url: String,
    proxy: String,
    proxy_tls_ca: Option<&str>,
    proxy_danger_hostname: bool,
    target_tls_ca: Option<&str>,
    target_danger_hostname: bool,
) -> Result<ylong_http_client::async_impl::Response, ylong_http_client::HttpClientError> {
    let proxy = Proxy::all(proxy.as_str()).build().unwrap();
    let mut builder = Client::builder().dns_resolver(LocalResolver).proxy(proxy);

    if let Some(ca) = proxy_tls_ca {
        builder = builder.proxy_tls_ca_file(ca);
    }
    if proxy_danger_hostname {
        builder = builder.danger_accept_invalid_proxy_hostnames(true);
    }
    if let Some(ca) = target_tls_ca {
        builder = builder.tls_ca_file(ca);
    }
    if target_danger_hostname {
        builder = builder.danger_accept_invalid_hostnames(true);
    }

    let client = builder.build().unwrap();
    let request = Request::builder()
        .method("GET")
        .url(url.as_str())
        .body(Body::empty())
        .unwrap();
    client.request(request).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_proxy_tls_uses_proxy_hostname_for_verification() {
    let proxy_sni = Arc::new(Mutex::new(Vec::new()));
    let mut proxy = start_https_proxy_with_named_cert(None, 200, proxy_sni.clone()).await;
    let proxy_ca = cert_path("https_proxy_sni_proxy_ca.pem");
    let url = format!("http://{TARGET_HOST}:80/data");
    let proxy_url = format!("https://{PROXY_HOST}:{}", proxy.port);

    let response = request_via_proxy_with_tls_paths(
        url,
        proxy_url,
        Some(proxy_ca.as_str()),
        false,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let head = proxy.observed.recv().await.unwrap();
    assert!(head.starts_with(&format!("GET http://{TARGET_HOST}:80/data HTTP/1.1\r\n")));
    assert!(
        sni_log_contains(&proxy_sni, PROXY_HOST),
        "{:?}",
        proxy_sni.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_target_tls_uses_target_hostname_after_proxy_tunnel() {
    let proxy_sni = Arc::new(Mutex::new(Vec::new()));
    let target_sni = Arc::new(Mutex::new(Vec::new()));
    let mut target = start_https_target_with_named_cert(target_sni.clone()).await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut proxy =
        start_https_proxy_with_named_cert(Some(target_addr), 200, proxy_sni.clone()).await;
    let proxy_ca = cert_path("https_proxy_sni_proxy_ca.pem");
    let target_ca = cert_path("https_proxy_sni_target_ca.pem");
    let url = format!("https://{TARGET_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{PROXY_HOST}:{}", proxy.port);

    let response = request_via_proxy_with_tls_paths(
        url,
        proxy_url,
        Some(proxy_ca.as_str()),
        false,
        Some(target_ca.as_str()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let connect = proxy.observed.recv().await.unwrap();
    assert!(connect.starts_with(&format!(
        "CONNECT {TARGET_HOST}:{} HTTP/1.1\r\n",
        target.port
    )));

    let target_head = target.observed.recv().await.unwrap();
    assert!(target_head.starts_with("GET /secure HTTP/1.1\r\n"));
    assert!(
        sni_log_contains(&proxy_sni, PROXY_HOST),
        "{:?}",
        proxy_sni.lock().unwrap()
    );
    assert!(
        sni_log_contains(&target_sni, TARGET_HOST),
        "{:?}",
        target_sni.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_proxy_hostname_mismatch_is_not_bypassed_by_target_config() {
    let proxy_sni = Arc::new(Mutex::new(Vec::new()));
    let target_sni = Arc::new(Mutex::new(Vec::new()));
    let target = start_https_target_with_named_cert(target_sni.clone()).await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let proxy = start_https_proxy_with_named_cert(Some(target_addr), 200, proxy_sni.clone()).await;
    let proxy_ca = cert_path("https_proxy_sni_proxy_ca.pem");
    let combined_ca = cert_path("https_proxy_sni_combined_ca.pem");
    let url = format!("https://{TARGET_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{WRONG_PROXY_HOST}:{}", proxy.port);

    let result = request_via_proxy_with_tls_paths(
        url,
        proxy_url,
        Some(proxy_ca.as_str()),
        false,
        Some(combined_ca.as_str()),
        true,
    )
    .await;
    assert!(result.is_err());
    assert!(!target.accepted.load(Ordering::SeqCst));
    assert!(
        sni_log_contains(&proxy_sni, WRONG_PROXY_HOST),
        "{:?}",
        proxy_sni.lock().unwrap()
    );
    assert!(target_sni.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_target_hostname_mismatch_is_not_bypassed_by_proxy_config() {
    let proxy_sni = Arc::new(Mutex::new(Vec::new()));
    let target_sni = Arc::new(Mutex::new(Vec::new()));
    let target = start_https_target_with_named_cert(target_sni.clone()).await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut proxy =
        start_https_proxy_with_named_cert(Some(target_addr), 200, proxy_sni.clone()).await;
    let combined_ca = cert_path("https_proxy_sni_combined_ca.pem");
    let target_ca = cert_path("https_proxy_sni_target_ca.pem");
    let url = format!("https://{WRONG_TARGET_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{PROXY_HOST}:{}", proxy.port);

    let result = request_via_proxy_with_tls_paths(
        url,
        proxy_url,
        Some(combined_ca.as_str()),
        true,
        Some(target_ca.as_str()),
        false,
    )
    .await;
    assert!(result.is_err());
    assert!(target.accepted.load(Ordering::SeqCst));

    let connect = proxy.observed.recv().await.unwrap();
    assert!(connect.starts_with(&format!(
        "CONNECT {WRONG_TARGET_HOST}:{} HTTP/1.1\r\n",
        target.port
    )));
    assert!(
        sni_log_contains(&proxy_sni, PROXY_HOST),
        "{:?}",
        proxy_sni.lock().unwrap()
    );
    assert!(
        sni_log_contains(&target_sni, WRONG_TARGET_HOST),
        "{:?}",
        target_sni.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_http_target_over_https_proxy_uses_absolute_form_and_proxy_auth() {
    let mut proxy = start_proxy(true, None, 200).await;
    let url = format!("http://{TEST_HOST}:80/data");
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let response = request_via_proxy(url, proxy_url, true, false, false, false, true)
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let head = proxy.observed.recv().await.unwrap();
    assert!(head.starts_with(&format!("GET http://{TEST_HOST}:80/data HTTP/1.1\r\n")));
    assert!(
        head.to_ascii_lowercase()
            .contains("proxy-authorization:basic dxnlcjpwyxnz\r\n"),
        "{head}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_https_target_over_https_proxy_builds_double_tls_tunnel() {
    let mut target = start_https_target().await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut proxy = start_proxy(true, Some(target_addr), 200).await;
    let url = format!("https://{TEST_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let response = request_via_proxy(url, proxy_url, true, false, true, false, true)
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let connect = proxy.observed.recv().await.unwrap();
    assert!(connect.starts_with(&format!("CONNECT {TEST_HOST}:{} HTTP/1.1\r\n", target.port)));
    assert!(connect.contains(&format!("Proxy-Authorization: {PROXY_AUTH}\r\n")));

    let target_head = target.observed.recv().await.unwrap();
    assert!(target_head.starts_with("GET /secure HTTP/1.1\r\n"));
    assert!(!target_head
        .to_ascii_lowercase()
        .contains("proxy-authorization"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_https_target_over_http_proxy_still_works() {
    let mut target = start_https_target().await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut proxy = start_proxy(false, Some(target_addr), 200).await;
    let url = format!("https://{TEST_HOST}:{}/secure", target.port);
    let proxy_url = format!("http://{TEST_HOST}:{}", proxy.port);

    let response = request_via_proxy(url, proxy_url, false, false, true, false, false)
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(proxy.observed.recv().await.unwrap().starts_with("CONNECT "));
    assert!(target
        .observed
        .recv()
        .await
        .unwrap()
        .starts_with("GET /secure HTTP/1.1\r\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_https_proxy_certificate_is_verified_separately_from_target() {
    let target = start_https_target().await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let proxy = start_proxy(true, Some(target_addr), 200).await;
    let url = format!("https://{TEST_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let result = request_via_proxy(url, proxy_url, false, false, false, true, false).await;
    assert!(result.is_err());
    assert!(!target.accepted.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_proxy_danger_does_not_disable_target_verification() {
    let target = start_https_target().await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let proxy = start_proxy(true, Some(target_addr), 200).await;
    let url = format!("https://{TEST_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let result = request_via_proxy(url, proxy_url, false, true, false, false, false).await;
    assert!(result.is_err());
    assert!(target.accepted.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_connect_failure_stops_before_target_tls() {
    let target = start_https_target().await;
    let target_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut proxy = start_proxy(true, Some(target_addr), 407).await;
    let url = format!("https://{TEST_HOST}:{}/secure", target.port);
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let result = request_via_proxy(url, proxy_url, true, false, true, false, true).await;
    assert!(result.is_err());
    assert!(proxy.observed.recv().await.unwrap().starts_with("CONNECT "));
    assert!(!target.accepted.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_https_proxy_mtls_succeeds_with_client_certificate() {
    let mut proxy = start_mtls_proxy(None, 200).await;
    let url = format!("http://{TEST_HOST}:80/data");
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let response =
        request_via_proxy_with_client_cert(url, proxy_url, true, false, false, false, false, true)
            .await
            .unwrap();
    assert_eq!(response.status().as_u16(), 200);

    let head = proxy.observed.recv().await.unwrap();
    assert!(head.starts_with(&format!("GET http://{TEST_HOST}:80/data HTTP/1.1\r\n")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdv_https_proxy_mtls_fails_without_client_certificate() {
    let proxy = start_mtls_proxy(None, 200).await;
    let url = format!("http://{TEST_HOST}:80/data");
    let proxy_url = format!("https://{TEST_HOST}:{}", proxy.port);

    let result = request_via_proxy(url, proxy_url, true, false, false, false, false).await;
    assert!(result.is_err());
}
