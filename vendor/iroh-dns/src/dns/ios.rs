//! iOS system DNS adapter. See `vendor/README.md` for the supported boundary.

use std::io;

use super::*;

#[derive(Debug, Clone)]
pub(super) struct SystemResolver {
    supported: bool,
}

impl SystemResolver {
    pub(super) fn new(config: Builder) -> Self {
        Self {
            supported: config.use_system_defaults
                && config.nameservers.is_empty()
                && config.fallback_nameservers.is_empty()
                && matches!(
                    config.fallback_mode,
                    FallbackMode::Deferred | FallbackMode::Never
                ),
        }
    }
}

impl Resolver for SystemResolver {
    fn lookup_ipv4(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv4Addr>, DnsError>> {
        let supported = self.supported;
        Box::pin(async move {
            let addrs = addresses(supported, host).await?;
            let iter: BoxIter<_> = Box::new(addrs.filter_map(|addr| match addr.ip() {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            }));
            Ok(iter)
        })
    }

    fn lookup_ipv6(&self, host: String) -> BoxFuture<Result<BoxIter<Ipv6Addr>, DnsError>> {
        let supported = self.supported;
        Box::pin(async move {
            let addrs = addresses(supported, host).await?;
            let iter: BoxIter<_> = Box::new(addrs.filter_map(|addr| match addr.ip() {
                IpAddr::V6(ip) => Some(ip),
                IpAddr::V4(_) => None,
            }));
            Ok(iter)
        })
    }

    fn lookup_txt(&self, _host: String) -> BoxFuture<Result<BoxIter<TxtRecordData>, DnsError>> {
        Box::pin(async { Err(unsupported("iOS system resolver supports A/AAAA, not TXT")) })
    }

    fn clear_cache(&self) {}

    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(self.clone())
    }
}

async fn addresses(
    supported: bool,
    host: String,
) -> Result<impl Iterator<Item = SocketAddr>, DnsError> {
    if !supported {
        return Err(unsupported(
            "iOS requires system DNS without custom nameservers or fallback policy",
        ));
    }
    // AF_UNSPEC via getaddrinfo preserves OS DNS routing and DNS64 synthesis.
    // Tokio's blocking OS call can outlive a dropped/timed-out waiting future.
    tokio::net::lookup_host((host, 0))
        .await
        .map_err(|err| e!(DnsError::Resolve, AnyError::from_std(err)))
}

fn unsupported(message: &'static str) -> DnsError {
    e!(
        DnsError::Resolve,
        AnyError::from_std(io::Error::new(io::ErrorKind::Unsupported, message))
    )
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn system() -> SystemResolver {
        SystemResolver::new(Builder::default().with_system_defaults())
    }

    fn assert_unsupported<T>(result: Result<T, DnsError>) {
        match result {
            Err(DnsError::Resolve { source, .. }) => assert_eq!(
                source.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::Unsupported
            ),
            _ => panic!("expected an explicit Unsupported resolver error"),
        }
    }

    #[test]
    fn construct_clear_reset_without_runtime_or_io() {
        let resolver = system();
        resolver.clear_cache();
        let reset = resolver.reset();
        reset.clear_cache();
    }

    #[tokio::test]
    async fn localhost_and_literals_use_the_requested_family() {
        let resolver = system();
        let ipv4: Vec<_> = resolver
            .lookup_ipv4("localhost".into())
            .await
            .unwrap()
            .collect();
        assert!(!ipv4.is_empty());
        assert!(ipv4.iter().all(Ipv4Addr::is_loopback));
        assert_eq!(
            resolver
                .lookup_ipv6("::1".into())
                .await
                .unwrap()
                .collect::<Vec<_>>(),
            [Ipv6Addr::LOCALHOST]
        );
        assert!(
            resolver
                .lookup_ipv4("::1".into())
                .await
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            resolver
                .lookup_ipv6("127.0.0.1".into())
                .await
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(
            resolver
                .reset()
                .lookup_ipv4("127.0.0.1".into())
                .await
                .unwrap()
                .collect::<Vec<_>>(),
            [Ipv4Addr::LOCALHOST]
        );
    }

    #[tokio::test]
    async fn system_modes_have_no_application_fallback() {
        for mode in [FallbackMode::Deferred, FallbackMode::Never] {
            let resolver = SystemResolver::new(
                Builder::default()
                    .with_system_defaults()
                    .with_fallback_mode(mode),
            );
            assert_eq!(
                resolver
                    .lookup_ipv4("127.0.0.1".into())
                    .await
                    .unwrap()
                    .collect::<Vec<_>>(),
                [Ipv4Addr::LOCALHOST]
            );
        }
    }

    #[tokio::test]
    async fn unsupported_settings_and_txt_fail_even_after_reset() {
        let configs = [
            Builder::default(),
            Builder::default()
                .with_system_defaults()
                .add_nameserver_config(NameserverConfig::udp(Ipv4Addr::LOCALHOST.into())),
            Builder::default()
                .with_system_defaults()
                .fallback_nameserver_configs([NameserverConfig::udp(Ipv4Addr::LOCALHOST.into())]),
            Builder::default()
                .with_system_defaults()
                .with_fallback_mode(FallbackMode::Eager),
            Builder::default()
                .with_system_defaults()
                .with_fallback_mode(FallbackMode::IfSystemEmpty),
        ];
        for config in configs {
            let resolver = SystemResolver::new(config);
            assert_unsupported(resolver.lookup_ipv4("127.0.0.1".into()).await);
            assert_unsupported(resolver.lookup_ipv6("::1".into()).await);
            assert_unsupported(resolver.reset().lookup_ipv4("127.0.0.1".into()).await);
        }
        assert_unsupported(system().lookup_txt("localhost".into()).await);
        assert_unsupported(system().reset().lookup_txt("localhost".into()).await);
    }

    #[tokio::test]
    async fn malformed_host_preserves_os_error_type() {
        let result = system().lookup_ipv4("invalid\0host".into()).await;
        match result {
            Err(DnsError::Resolve { source, .. }) => {
                assert!(source.downcast_ref::<io::Error>().is_some());
            }
            _ => panic!("expected the native resolver's I/O error"),
        }
    }
}
