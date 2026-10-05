use std::net::SocketAddr;

use http::StatusCode;
use instant_acme::ChallengeType;
use tokio::net::TcpListener;

use super::store::testing::issue;
use super::*;
use crate::log::LogLevel;

/// HTTPS on a port of its own, answering every request with the protocol
/// it arrived over, the address it came from, and the host it names.
async fn serve_tls(shared: Arc<Shared>) -> (SocketAddr, tokio_util::sync::DropGuard) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let handler = |request: http::Request<axum::body::Body>, remote: SocketAddr| async move {
        http::Response::new(axum::body::Body::from(format!("{:?} {} {}", request.version(), remote.ip(), crate::app::request_host(&request))))
    };
    tokio::spawn(serve(listener, listener::server_config(shared), handler, shutdown.clone(), Logger::new(LogLevel::Error)));
    (address, shutdown.drop_guard())
}

fn client(ca: &rustls::pki_types::CertificateDer<'_>, address: SocketAddr, http1: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_der(ca).unwrap())
        .resolve("example.org", address)
        .resolve("unknown.example", address);
    if http1 {
        builder = builder.http1_only();
    }
    builder.build().unwrap()
}

#[tokio::test]
async fn serves_the_certificate_for_the_name_asked_over_h2_and_http1() {
    let issued = issue(&["example.org"]);
    let certificate = store::Certificate::from_pem(&issued.chain, &issued.key).unwrap();
    let shared = Arc::new(Shared::default());
    shared.certificates.store(Arc::new([("example.org".to_string(), certificate.key)].into()));
    let (address, _guard) = serve_tls(shared).await;
    let url = format!("https://example.org:{}/", address.port());

    let h2 = client(&issued.ca, address, false).get(&url).send().await.unwrap();
    assert_eq!(h2.status(), StatusCode::OK);
    assert_eq!(h2.text().await.unwrap(), "HTTP/2.0 127.0.0.1 example.org");

    let h1 = client(&issued.ca, address, true).get(&url).send().await.unwrap();
    assert_eq!(h1.text().await.unwrap(), "HTTP/1.1 127.0.0.1 example.org");

    // A name with no certificate gets no handshake.
    let unknown = format!("https://unknown.example:{}/", address.port());
    assert!(client(&issued.ca, address, false).get(&unknown).send().await.is_err());
}

#[test]
fn a_validating_ca_gets_the_challenge_certificate() {
    let shared = Shared::default();
    let issued = issue(&["example.org"]);
    let served = store::Certificate::from_pem(&issued.chain, &issued.key).unwrap().key;
    shared.certificates.store(Arc::new([("example.org".to_string(), served.clone())].into()));
    let challenge = Arc::new(acme::alpn_challenge_certificate("example.org", &[7; 32]).unwrap());
    shared.alpn_challenges.lock().insert("example.org".into(), challenge.clone());

    // The resolver is reached through a real handshake, since rustls offers
    // no other way to make a `ClientHello`.
    let config = listener::server_config(Arc::new(shared));
    let handshake = |alpn: &[u8]| {
        let mut client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnything))
            .with_no_client_auth();
        client.alpn_protocols = vec![alpn.to_vec()];
        let name = rustls::pki_types::ServerName::try_from("example.org").unwrap();
        let mut client = rustls::ClientConnection::new(Arc::new(client), name).unwrap();
        let mut server = rustls::ServerConnection::new(config.clone()).unwrap();
        for _ in 0..4 {
            let mut buffer = Vec::new();
            client.write_tls(&mut buffer).unwrap();
            server.read_tls(&mut buffer.as_slice()).unwrap();
            server.process_new_packets().unwrap();
            let mut buffer = Vec::new();
            server.write_tls(&mut buffer).unwrap();
            client.read_tls(&mut buffer.as_slice()).unwrap();
            client.process_new_packets().unwrap();
        }
        client.peer_certificates().unwrap()[0].clone()
    };
    assert_eq!(handshake(ACME_TLS_ALPN), challenge.cert[0]);
    assert_eq!(handshake(b"h2"), served.cert[0]);
}

#[derive(Debug)]
struct AcceptAnything;

impl rustls::client::danger::ServerCertVerifier for AcceptAnything {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

#[test]
fn orders_when_due_and_backs_off_when_failing() {
    assert_eq!(due_at(None), 0);
    let issued = Row { hostname: "a".into(), validity: Some((0, 90)), failures: 0, attempted_at: Some(0) };
    assert_eq!(due_at(Some(&issued)), 60);
    let failing = Row { hostname: "a".into(), validity: None, failures: 1, attempted_at: Some(1000) };
    assert_eq!(due_at(Some(&failing)), 1000 + 120);
    // A failed renewal waits out its backoff, not just the renewal date.
    let renewing = Row { failures: 3, validity: Some((0, 90)), ..failing };
    assert_eq!(due_at(Some(&renewing)), 1000 + 480);

    assert_eq!([1, 2, 3, 4, 5, 6, 7, 100].map(backoff), [120, 240, 480, 960, 1920, 3600, 3600, 3600]);
}

#[test]
fn budget_spares_the_other_hosts_from_one_over_its_limit() {
    let t = crate::db::testing::temp_db();
    let settings = Settings::load(&t.db, None).unwrap();
    let budget = Budget::new(settings);
    for _ in 0..60 {
        assert_eq!(budget.take("a.example"), Ok(()));
    }
    assert_eq!(budget.take("a.example"), Err("host"));
    // The refusal spent nothing of the total: 540 more fit in it.
    for i in 0..540 {
        assert_eq!(budget.take(&format!("{i}.example")), Ok(()));
    }
    assert_eq!(budget.take("c.example"), Err("total"));
}

/// Gets real certificates from Pebble, Let's Encrypt's test CA, by both
/// challenges. Pebble validates against 127.0.0.1, so npanel listens where
/// it looks: 5001 for TLS-ALPN-01 and 5002 for HTTP-01, its defaults. The
/// challenge test server answers there itself unless told not to.
///
/// ```text
/// docker run -d --name challtestsrv --network host ghcr.io/letsencrypt/pebble-challtestsrv -defaultIPv4 127.0.0.1 -defaultIPv6 "" -http01 "" -tlsalpn01 "" -https01 "" -doh ""
/// docker run -d --name pebble --network host -e PEBBLE_VA_NOSLEEP=1 ghcr.io/letsencrypt/pebble -dnsserver 127.0.0.1:8053
/// docker cp pebble:/test/certs/pebble.minica.pem /tmp/pebble.minica.pem
/// PEBBLE_ROOT=/tmp/pebble.minica.pem cargo test pebble -- --ignored
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a Pebble ACME server"]
async fn pebble() {
    let root = std::fs::read(std::env::var("PEBBLE_ROOT").expect("PEBBLE_ROOT: Pebble's CA certificate")).unwrap();
    let root = reqwest::Certificate::from_pem(&root).unwrap();
    let directory = std::env::var("PEBBLE_DIRECTORY").unwrap_or_else(|_| "https://localhost:14000/dir".into());
    let log = Logger::new(LogLevel::Error);

    let t = crate::db::testing::temp_db();
    let settings = Settings::load(&t.db, None).unwrap();
    let patch = serde_json::json!({ "acme_directory": directory, "acme_email": "ops@npanel.test" });
    settings.update(patch.as_object().unwrap(), "test").await.unwrap();
    let service = TlsService::start(TlsDeps {
        db: t.db.clone(),
        settings: settings.clone(),
        log,
        dashboard_host: "npanel.test".into(),
        acme_root: Some(root.clone()),
    })
    .await
    .unwrap();

    let shutdown = CancellationToken::new();
    let _guard = shutdown.clone().drop_guard();
    let handler = |_, _| async { http::Response::new(axum::body::Body::empty()) };
    let tls = TcpListener::bind("127.0.0.1:5001").await.unwrap();
    tokio::spawn(serve(tls, service.server_config(), handler, shutdown.clone(), log));
    let redirect = Arc::new(service.redirect(5001));
    let http = TcpListener::bind("127.0.0.1:5002").await.unwrap();
    tokio::spawn(crate::server::serve(
        http,
        move |request, _| {
            let redirect = redirect.clone();
            async move { redirect.handle(&request) }
        },
        shutdown.clone(),
        log,
    ));

    // Both challenges, directly, for a name of their own: the service is
    // ordering the dashboard's meanwhile, and one name has one challenge
    // certificate at a time.
    let stored = t.db.with(|conn| Ok(store::account(conn, &directory)?)).unwrap();
    let (account, _) =
        acme::account(|| acme::http_client(Some(root.clone())), &directory, "ops@npanel.test", stored).await.unwrap();
    for kind in [ChallengeType::TlsAlpn01, ChallengeType::Http01] {
        let (chain, key) = acme::order(&account, &service.shared, "direct.npanel.test", kind.clone()).await.unwrap_or_else(|error| panic!("{kind:?}: {error:#}"));
        assert!(store::Certificate::from_pem(&chain, &key).unwrap().not_after > now());
        assert!(service.shared.alpn_challenges.lock().is_empty() && service.shared.http_challenges.lock().is_empty());
    }

    // A name pointed somewhere npanel isn't fails with the CA's reason.
    reqwest::Client::new()
        .post("http://127.0.0.1:8055/add-a")
        .body(r#"{"host":"elsewhere.npanel.test.","addresses":["127.0.0.2"]}"#)
        .send()
        .await
        .unwrap();
    let error = acme::order(&account, &service.shared, "elsewhere.npanel.test", ChallengeType::Http01).await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.starts_with("The CA could not validate elsewhere.npanel.test: ") && error.contains("connection refused"), "{error}");

    // And the service's own, for the dashboard host, which it gets unasked.
    for _ in 0..200 {
        if service.shared.certificates.load().contains_key("npanel.test") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(service.shared.certificates.load().contains_key("npanel.test"));
    assert!(t.db.with(|conn| Ok(store::account(conn, &directory)?)).unwrap().is_some());
    assert_eq!(service.status().await.unwrap().issued, 1);
    service.stop().await;
}
