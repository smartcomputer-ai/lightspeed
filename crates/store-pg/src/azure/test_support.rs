//! A local TLS fake of Entra and Azure Storage. Certificates are generated per test.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use object_store::{Certificate, ClientOptions};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        crypto::aws_lc_rs,
        pki_types::{CertificateDer, PrivateKeyDer},
    },
};

pub(super) struct TestCa {
    pub(super) certificate: Certificate,
    issuer: Issuer<'static, KeyPair>,
    der: CertificateDer<'static>,
}

impl TestCa {
    pub(super) fn new() -> Self {
        let mut params = CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        Self {
            certificate: Certificate::from_der(certificate.der()).unwrap(),
            der: certificate.der().clone(),
            issuer: Issuer::new(params, key),
        }
    }

    pub(super) fn acceptor(&self, host: &str) -> TlsAcceptor {
        let params = CertificateParams::new(vec![host.to_owned()]).unwrap();
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.issuer).unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone(), self.der.clone()],
                PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        TlsAcceptor::from(Arc::new(config))
    }

    /// Only this CA is trusted, so results do not depend on the machine's store.
    pub(super) fn reqwest_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .tls_certs_only([reqwest::Certificate::from_der(&self.der).unwrap()])
            .build()
            .unwrap()
    }

    pub(super) fn client_options(&self) -> ClientOptions {
        ClientOptions::new()
            .with_root_certificate(self.certificate.clone())
            .with_no_system_certificates(true)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) body: Vec<u8>,
}

impl Request {
    pub(super) fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .into_owned()
            .collect()
    }
}

pub(super) struct Response {
    pub(super) status: u16,
    pub(super) headers: Vec<(&'static str, String)>,
    pub(super) body: Vec<u8>,
}

impl Response {
    pub(super) fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

pub(super) type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// One request per connection. A rejected TLS handshake records nothing, which
/// is how the rejection tests prove that no credential reached the server.
pub(super) struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub(super) async fn start(acceptor: TlsAcceptor, handler: Arc<Handler>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (acceptor, handler, recorded) =
                    (acceptor.clone(), handler.clone(), recorded.clone());
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Some(request) = read_request(&mut stream).await else {
                        return;
                    };
                    let response = handler(&request);
                    recorded.lock().unwrap().push(request);
                    let mut head = format!(
                        "HTTP/1.1 {} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                        response.status,
                        response.body.len()
                    );
                    for (name, value) in response.headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&response.body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            port,
            requests,
            task,
        }
    }

    pub(super) fn url(&self) -> String {
        format!("https://localhost:{}", self.port)
    }

    pub(super) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Option<Request> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        let mut chunk = [0; 4096];
        match stream.read(&mut chunk).await.ok()? {
            0 => return None,
            read => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let head = String::from_utf8(buffer[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let (method, path) = (request_line.next()?, request_line.next()?);
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map_or(Some(0), |value| value.parse().ok())?;
    let mut body = buffer.split_off(head_end + 4);
    while body.len() < length {
        let mut chunk = [0; 4096];
        match stream.read(&mut chunk).await.ok()? {
            0 => return None,
            read => body.extend_from_slice(&chunk[..read]),
        }
    }
    Some(Request {
        method: method.into(),
        path: path.into(),
        headers,
        body,
    })
}

/// Issues `secret-access-token-<n>` valid for `expires_in` seconds.
pub(super) fn entra(expires_in: u64) -> Arc<Handler> {
    let issued = Mutex::new(0);
    Arc::new(move |_| {
        let mut issued = issued.lock().unwrap();
        *issued += 1;
        Response::new(
            200,
            format!(
                r#"{{"access_token":"secret-access-token-{issued}","token_type":"Bearer","expires_in":{expires_in}}}"#
            ),
        )
    })
}
