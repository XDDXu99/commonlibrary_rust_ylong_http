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

//! This is a simple asynchronous HTTPS proxy client example.

use ylong_http_client::async_impl::{Body, ClientBuilder, Downloader, Request};
use ylong_http_client::{HttpClientError, Proxy, TlsFileType, TlsVersion};

#[tokio::main]
async fn main() -> Result<(), HttpClientError> {
    let proxy = Proxy::all("https://proxy.example.com:8443")
        .basic_auth("username", "password")
        .build()?;

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

    let request = Request::builder()
        .url("https://www.example.com")
        .body(Body::empty())?;

    let response = client.request(request).await?;
    let _ = Downloader::console(response).download().await;
    Ok(())
}
