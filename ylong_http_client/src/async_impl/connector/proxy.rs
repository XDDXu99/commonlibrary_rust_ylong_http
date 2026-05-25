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

use std::error;
use std::fmt::{Debug, Display, Formatter};
use std::io::{Error, ErrorKind, Write};

use crate::runtime::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) async fn tunnel<S>(
    conn: &mut S,
    host: &str,
    port: u16,
    auth: Option<String>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut req = Vec::new();

    write!(
        &mut req,
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n"
    )?;

    if let Some(value) = auth {
        write!(&mut req, "Proxy-Authorization: Basic {value}\r\n")?;
    }

    write!(&mut req, "\r\n")?;

    conn.write_all(&req).await?;

    let mut buf = [0; 8192];
    let mut pos = 0;

    loop {
        let n = conn.read(&mut buf[pos..]).await?;

        if n == 0 {
            return Err(other_io_error(CreateTunnelErr::Unsuccessful));
        }

        pos += n;
        let resp = &buf[..pos];
        if resp.starts_with(b"HTTP/1.1 2") || resp.starts_with(b"HTTP/1.0 2") {
            if resp.windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(());
            }
            if pos == buf.len() {
                return Err(other_io_error(CreateTunnelErr::ProxyHeadersTooLong));
            }
        } else if resp.starts_with(b"HTTP/1.1 407") {
            return Err(other_io_error(CreateTunnelErr::ProxyAuthenticationRequired));
        } else {
            return Err(other_io_error(CreateTunnelErr::Unsuccessful));
        }
    }
}

fn other_io_error(err: CreateTunnelErr) -> Error {
    Error::new(ErrorKind::Other, err)
}

enum CreateTunnelErr {
    ProxyHeadersTooLong,
    ProxyAuthenticationRequired,
    Unsuccessful,
}

impl Debug for CreateTunnelErr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProxyHeadersTooLong => f.write_str("Proxy headers too long for tunnel"),
            Self::ProxyAuthenticationRequired => f.write_str("Proxy authentication required"),
            Self::Unsuccessful => f.write_str("Unsuccessful tunnel"),
        }
    }
}

impl Display for CreateTunnelErr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self, f)
    }
}

impl error::Error for CreateTunnelErr {}

#[cfg(test)]
mod ut_tunnel_error_debug {
    use super::CreateTunnelErr;

    /// UT test cases for debug of`CreateTunnelErr`.
    ///
    /// # Brief
    /// 1. Checks `CreateTunnelErr` debug by calling `CreateTunnelErr::fmt`.
    /// 2. Checks if the result is as expected.
    #[test]
    fn ut_tunnel_error_debug_assert() {
        assert_eq!(
            format!("{:?}", CreateTunnelErr::ProxyHeadersTooLong),
            "Proxy headers too long for tunnel"
        );
        assert_eq!(
            format!("{:?}", CreateTunnelErr::ProxyAuthenticationRequired),
            "Proxy authentication required"
        );
        assert_eq!(
            format!("{:?}", CreateTunnelErr::Unsuccessful),
            "Unsuccessful tunnel"
        );
        assert_eq!(
            format!("{}", CreateTunnelErr::ProxyHeadersTooLong),
            "Proxy headers too long for tunnel"
        );
        assert_eq!(
            format!("{}", CreateTunnelErr::ProxyAuthenticationRequired),
            "Proxy authentication required"
        );
        assert_eq!(
            format!("{}", CreateTunnelErr::Unsuccessful),
            "Unsuccessful tunnel"
        );
    }
}

#[cfg(all(test, feature = "ylong_base"))]
mod ut_create_tunnel_err_debug {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use ylong_runtime::io::AsyncWriteExt;

    use super::{other_io_error, tunnel, CreateTunnelErr};
    use crate::async_impl::connector::tcp_stream;
    use crate::async_impl::dns::{EyeBallConfig, HappyEyeballs};
    use crate::start_tcp_server;
    use crate::util::test_utils::{format_header_str, TcpHandle};

    /// UT test cases for `tunnel`.
    ///
    /// # Brief
    /// 1. Creates a `tcp stream` by calling `tcp_stream`.
    /// 2. Sends a `Request` by `tunnel`.
    /// 3. Checks if the result is as expected.
    #[test]
    fn ut_ssl_tunnel_error() {
        let mut handles = vec![];
        start_tcp_server!(
           Handles: handles,
           EndWith: "\r\n\r\n",
           Shutdown: std::net::Shutdown::Both,
        );
        let handle = handles.pop().expect("No more handles !");

        let eyeballs = HappyEyeballs::new(
            vec![SocketAddr::from_str(handle.addr.as_str()).unwrap()],
            EyeBallConfig::new(None, None),
        );

        let handle = ylong_runtime::spawn(async move {
            let mut tcp = tcp_stream(eyeballs).await.unwrap();
            let res = tunnel(
                &mut tcp,
                "ylong_http.com",
                443,
                Some(String::from("base64 bytes")),
            )
            .await;
            assert_eq!(
                format!("{:?}", res.err()),
                format!("{:?}", Some(other_io_error(CreateTunnelErr::Unsuccessful)))
            );
            handle
                .server_shutdown
                .recv()
                .expect("server send order failed !");
        });
        ylong_runtime::block_on(handle).unwrap();

        start_tcp_server!(
           Handles: handles,
           EndWith: "\r\n\r\n",
           Response: {
               Status: 407,
               Version: "HTTP/1.1",
               Header: "Content-Length", "11",
               Body: "METHOD GET!",
           },
           Shutdown: std::net::Shutdown::Both,
        );
        let handle = handles.pop().expect("No more handles !");

        let eyeballs = HappyEyeballs::new(
            vec![SocketAddr::from_str(handle.addr.as_str()).unwrap()],
            EyeBallConfig::new(None, None),
        );
        let handle = ylong_runtime::spawn(async move {
            let mut tcp = tcp_stream(eyeballs).await.unwrap();
            let res = tunnel(
                &mut tcp,
                "ylong_http.com",
                443,
                Some(String::from("base64 bytes")),
            )
            .await;
            assert_eq!(
                format!("{:?}", res.err()),
                format!(
                    "{:?}",
                    Some(other_io_error(CreateTunnelErr::ProxyAuthenticationRequired))
                )
            );
            handle
                .server_shutdown
                .recv()
                .expect("server send order failed !");
        });
        ylong_runtime::block_on(handle).unwrap();

        start_tcp_server!(
           Handles: handles,
           EndWith: "\r\n\r\n",
           Response: {
               Status: 402,
               Version: "HTTP/1.1",
               Header: "Content-Length", "11",
               Body: "METHOD GET!",
           },
           Shutdown: std::net::Shutdown::Both,
        );
        let handle = handles.pop().expect("No more handles !");

        let eyeballs = HappyEyeballs::new(
            vec![SocketAddr::from_str(handle.addr.as_str()).unwrap()],
            EyeBallConfig::new(None, None),
        );
        let handle = ylong_runtime::spawn(async move {
            let mut tcp = tcp_stream(eyeballs).await.unwrap();
            let res = tunnel(
                &mut tcp,
                "ylong_http.com",
                443,
                Some(String::from("base64 bytes")),
            )
            .await;
            assert_eq!(
                format!("{:?}", res.err()),
                format!("{:?}", Some(other_io_error(CreateTunnelErr::Unsuccessful)))
            );
            handle
                .server_shutdown
                .recv()
                .expect("server send order failed !");
        });
        ylong_runtime::block_on(handle).unwrap();
    }

    /// UT test cases for `tunnel`.
    ///
    /// # Brief
    /// 1. Creates a `tcp stream` by calling `tcp_stream`.
    /// 2. Sends a `Request` by `tunnel`.
    /// 3. Checks if the result is as expected.
    #[test]
    fn ut_ssl_tunnel_connect() {
        let mut handles = vec![];

        start_tcp_server!(
           Handles: handles,
           EndWith: "\r\n\r\n",
            Response: {
               Status: 200,
               Version: "HTTP/1.1",
               Body: "",
           },
           Shutdown: std::net::Shutdown::Both,
        );
        let handle = handles.pop().expect("No more handles !");

        let eyeballs = HappyEyeballs::new(
            vec![SocketAddr::from_str(handle.addr.as_str()).unwrap()],
            EyeBallConfig::new(None, None),
        );
        let handle = ylong_runtime::spawn(async move {
            let mut tcp = tcp_stream(eyeballs).await.unwrap();
            let res = tunnel(
                &mut tcp,
                "ylong_http.com",
                443,
                Some(String::from("base64 bytes")),
            )
            .await;
            assert!(res.is_ok());
            handle
                .server_shutdown
                .recv()
                .expect("server send order failed !");
        });
        ylong_runtime::block_on(handle).unwrap();
    }

    /// UT test cases for response beyond size of `tunnel`.
    ///
    /// # Brief
    /// 1. Creates a `tcp stream` by calling `tcp_stream`.
    /// 2. Sends a `Request` by `tunnel`.
    /// 3. Checks if the result is as expected.
    #[test]
    fn ut_ssl_tunnel_resp_beyond_size() {
        use std::io::{Read, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let mut response = b"HTTP/1.1 200 ".to_vec();
            response.resize(8192, b'b');
            stream.write_all(&response).unwrap();
        });

        let eyeballs = HappyEyeballs::new(
            vec![SocketAddr::from_str(addr.to_string().as_str()).unwrap()],
            EyeBallConfig::new(None, None),
        );
        let handle = ylong_runtime::spawn(async move {
            let mut tcp = tcp_stream(eyeballs).await.unwrap();
            let res = tunnel(
                &mut tcp,
                "ylong_http.com",
                443,
                Some(String::from("base64 bytes")),
            )
            .await;
            assert_eq!(
                format!("{:?}", res.err()),
                format!(
                    "{:?}",
                    Some(other_io_error(CreateTunnelErr::ProxyHeadersTooLong))
                )
            );
        });
        ylong_runtime::block_on(handle).unwrap();
        server.join().unwrap();
    }
}
