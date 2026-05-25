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

//! Connection pool implementation.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ylong_http::request::uri::{Authority, Scheme};

use crate::util::progress::SpeedConfig;

pub(crate) struct Pool<K, V> {
    pool: Arc<Mutex<HashMap<K, V>>>,
}

impl<K, V> Pool<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            pool: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<K: Eq + Hash, V: Clone> Pool<K, V> {
    pub(crate) fn get<F>(
        &self,
        key: K,
        create_fn: F,
        allowed_num: usize,
        speed_conf: SpeedConfig,
    ) -> V
    where
        F: FnOnce(usize, SpeedConfig) -> V,
    {
        let mut inner = self.pool.lock().unwrap();
        match (*inner).entry(key) {
            Entry::Occupied(conns) => conns.get().clone(),
            Entry::Vacant(e) => e.insert(create_fn(allowed_num, speed_conf)).clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct PoolKey {
    target_scheme: Scheme,
    target_authority: Authority,
    proxy: Option<ProxyKey>,
    tls_config: Option<TlsConfigKey>,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct ProxyKey {
    scheme: Scheme,
    authority: Authority,
    auth: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct TlsConfigKey {
    target: usize,
    proxy: usize,
}

impl TlsConfigKey {
    pub(crate) fn next() -> Self {
        static NEXT_TLS_CONFIG_KEY: AtomicUsize = AtomicUsize::new(1);
        Self {
            target: NEXT_TLS_CONFIG_KEY.fetch_add(1, Ordering::Relaxed),
            proxy: NEXT_TLS_CONFIG_KEY.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl PoolKey {
    pub(crate) fn new(scheme: Scheme, authority: Authority) -> Self {
        Self {
            target_scheme: scheme,
            target_authority: authority,
            proxy: None,
            tls_config: None,
        }
    }

    pub(crate) fn with_proxy(
        target_scheme: Scheme,
        target_authority: Authority,
        proxy_scheme: Scheme,
        proxy_authority: Authority,
        proxy_auth: Option<String>,
    ) -> Self {
        Self {
            target_scheme,
            target_authority,
            proxy: Some(ProxyKey {
                scheme: proxy_scheme,
                authority: proxy_authority,
                auth: proxy_auth,
            }),
            tls_config: None,
        }
    }

    pub(crate) fn with_tls_config(mut self, tls_config: TlsConfigKey) -> Self {
        self.tls_config = Some(tls_config);
        self
    }
}

#[cfg(test)]
mod ut_pool {
    use ylong_http::request::uri::Uri;

    use crate::pool::{Pool, PoolKey, TlsConfigKey};
    use crate::util::progress::SpeedConfig;

    /// UT test cases for `Pool::get`.
    ///
    /// # Brief
    /// 1. Creates a `pool` by calling `Pool::new()`.
    /// 2. Uses `pool::get` to get connection.
    /// 3. Checks if the results are correct.
    #[test]
    fn ut_pool_get() {
        let uri = Uri::from_bytes(b"http://example1.com:80/foo?a=1").unwrap();
        let key = PoolKey::new(
            uri.scheme().unwrap().clone(),
            uri.authority().unwrap().clone(),
        );
        let data = String::from("Data info");
        let consume_and_return_data = move |_size: usize, _conf: SpeedConfig| data;
        let pool = Pool::new();
        let res = pool.get(key, consume_and_return_data, 6, SpeedConfig::none());
        assert_eq!(res, "Data info".to_string());
    }

    /// UT test cases for proxy-aware pool keys.
    ///
    /// # Brief
    /// 1. Creates pool keys with the same target and different proxy transports.
    /// 2. Checks if HTTP proxy and HTTPS proxy keys are isolated.
    #[test]
    fn ut_pool_key_proxy_isolation() {
        let target = Uri::from_bytes(b"https://target.example:443/foo").unwrap();
        let http_proxy = Uri::from_bytes(b"http://proxy.example:8080").unwrap();
        let https_proxy = Uri::from_bytes(b"https://proxy.example:8080").unwrap();
        let other_proxy = Uri::from_bytes(b"https://other-proxy.example:8080").unwrap();
        let other_target = Uri::from_bytes(b"https://other-target.example:443/foo").unwrap();

        let direct = PoolKey::new(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
        );
        let via_http = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            http_proxy.scheme().unwrap().clone(),
            http_proxy.authority().unwrap().clone(),
            None,
        );
        let via_https = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            https_proxy.scheme().unwrap().clone(),
            https_proxy.authority().unwrap().clone(),
            None,
        );
        let via_https_with_auth = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            https_proxy.scheme().unwrap().clone(),
            https_proxy.authority().unwrap().clone(),
            Some(String::from("auth")),
        );
        let via_other_proxy = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            other_proxy.scheme().unwrap().clone(),
            other_proxy.authority().unwrap().clone(),
            None,
        );
        let via_other_target = PoolKey::with_proxy(
            other_target.scheme().unwrap().clone(),
            other_target.authority().unwrap().clone(),
            https_proxy.scheme().unwrap().clone(),
            https_proxy.authority().unwrap().clone(),
            None,
        );

        assert_ne!(direct, via_http);
        assert_ne!(via_http, via_https);
        assert_ne!(via_https, via_https_with_auth);
        assert_ne!(via_https, via_other_proxy);
        assert_ne!(via_https, via_other_target);
    }

    /// UT test cases for proxy-aware pool keys with TLS config identity.
    ///
    /// # Brief
    /// 1. Creates pool keys with the same target and proxy.
    /// 2. Adds different TLS config identities.
    /// 3. Checks if the keys are isolated.
    #[test]
    fn ut_pool_key_tls_config_isolation() {
        let target = Uri::from_bytes(b"https://target.example:443/foo").unwrap();
        let proxy = Uri::from_bytes(b"https://proxy.example:8080").unwrap();
        let key_a = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            proxy.scheme().unwrap().clone(),
            proxy.authority().unwrap().clone(),
            None,
        )
        .with_tls_config(TlsConfigKey::next());
        let key_b = PoolKey::with_proxy(
            target.scheme().unwrap().clone(),
            target.authority().unwrap().clone(),
            proxy.scheme().unwrap().clone(),
            proxy.authority().unwrap().clone(),
            None,
        )
        .with_tls_config(TlsConfigKey::next());

        assert_ne!(key_a, key_b);
    }
}
