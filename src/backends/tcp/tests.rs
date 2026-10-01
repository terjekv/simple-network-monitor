use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakeTransport {
    pending_dns: bool,
    pending_connect: bool,
    failures: usize,
    calls: AtomicUsize,
}

#[async_trait]
impl TcpTransport for FakeTransport {
    async fn resolve(&self, _: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        if self.pending_dns {
            std::future::pending::<()>().await;
        }
        Ok(vec![
            SocketAddr::new("192.0.2.1".parse().unwrap(), port),
            SocketAddr::new("2001:db8::1".parse().unwrap(), port),
        ])
    }
    async fn connect(&self, _: SocketAddr) -> Result<(), String> {
        if self.pending_connect {
            std::future::pending::<()>().await;
        }
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
            Err("connection refused".into())
        } else {
            Ok(())
        }
    }
}

fn request() -> TcpRequest {
    TcpRequest {
        address: "router.example".into(),
        port: NonZeroU16::new(443).unwrap(),
        timeout: Duration::from_millis(10),
    }
}

#[rstest::rstest]
#[case(true, false)]
#[case(false, true)]
#[tokio::test]
async fn deadline_bounds_resolution_and_connection(
    #[case] pending_dns: bool,
    #[case] pending_connect: bool,
) {
    let check = TcpConnectCheck(FakeTransport {
        pending_dns,
        pending_connect,
        failures: 0,
        calls: AtomicUsize::new(0),
    });
    let result = tokio::time::timeout(Duration::from_secs(2), check.run(&request()))
        .await
        .unwrap();
    assert!(result.unwrap_err().contains("timed out"));
}

#[tokio::test]
async fn tries_the_next_resolved_address_after_failure() {
    let check = TcpConnectCheck(FakeTransport {
        pending_dns: false,
        pending_connect: false,
        failures: 1,
        calls: AtomicUsize::new(0),
    });
    assert!(check.run(&request()).await.is_ok());
    assert_eq!(check.0.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reports_failure_when_every_address_refuses_connection() {
    let check = TcpConnectCheck(FakeTransport {
        pending_dns: false,
        pending_connect: false,
        failures: 2,
        calls: AtomicUsize::new(0),
    });
    assert_eq!(
        check.run(&request()).await.unwrap_err(),
        "connection refused"
    );
}

#[tokio::test]
async fn rejects_invalid_destination_before_transport_io() {
    let check = TcpConnectCheck(FakeTransport {
        pending_dns: false,
        pending_connect: false,
        failures: 0,
        calls: AtomicUsize::new(0),
    });
    let mut request = request();
    request.address = "-oProxyCommand=example".into();
    assert!(check.run(&request).await.is_err());
    assert_eq!(check.0.calls.load(Ordering::SeqCst), 0);
}
