//! The HTTP transport: the same simulator, on a real socket.
//!
//! This is what lets a real `reqwest`-based client be tested **unmodified**. A
//! client tested only against an in-process fake has never had its URL building,
//! its query encoding, its `Authorization` header or its multipart body checked
//! by anything but itself.
//!
//! The server always binds `127.0.0.1:0`, so two tests can never collide on a
//! port, and it serves the OAuth token endpoint as well as the API — a client
//! pointed at [`SimServer::base_url`] can do the whole refresh dance without a
//! second fixture.
//!
//! Unlike the in-process transport, this one **really sleeps** for a
//! [`Delay`](crate::fault::Fault::Delay) fault. That is the only way a client's
//! own timeout can fire, and a timeout that never fires is a retry path that has
//! never been tested.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    response::Response,
    routing::any,
    Router,
};

use crate::{
    api::{parse_query, Method, SimRequest},
    simulator::Simulator,
};

/// The largest request body the server will read.
///
/// A batch of 100 `messages.get`s is a few kilobytes; 32 MiB is far beyond
/// anything real and still bounded, so a runaway test fails rather than eats the
/// machine.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// A running simulator on an ephemeral loopback port.
///
/// Dropping it shuts the server down.
#[derive(Debug)]
pub struct SimServer {
    address: SocketAddr,
    simulator: Arc<Simulator>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SimServer {
    /// Bind `127.0.0.1:0` and start serving.
    ///
    /// # Errors
    /// Whatever binding the listener returns.
    pub async fn start(simulator: Arc<Simulator>) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let app = Router::new()
            .fallback(any(serve))
            .with_state(Arc::clone(&simulator));
        let task = tokio::spawn(async move {
            // A serve error here means the listener died, which in a test means
            // the test is already over. Nothing useful to do but stop.
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            address,
            simulator,
            task: Some(task),
        })
    }

    /// Start a server over a fresh simulator.
    ///
    /// # Errors
    /// Whatever binding the listener returns.
    pub async fn fresh() -> std::io::Result<Self> {
        Self::start(Arc::new(Simulator::new())).await
    }

    /// The address it bound.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// `http://127.0.0.1:<port>` — what a client should use as its API base, in
    /// place of `https://gmail.googleapis.com`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The batch endpoint, in place of
    /// `https://gmail.googleapis.com/batch/gmail/v1`.
    #[must_use]
    pub fn batch_url(&self) -> String {
        format!("{}/batch/gmail/v1", self.base_url())
    }

    /// The OAuth token endpoint, in place of `https://oauth2.googleapis.com/token`.
    #[must_use]
    pub fn token_url(&self) -> String {
        format!("{}/token", self.base_url())
    }

    /// The simulator behind the socket — for driving the mailbox and asserting
    /// on state.
    #[must_use]
    pub fn simulator(&self) -> &Arc<Simulator> {
        &self.simulator
    }

    /// Stop serving and wait for the task to finish.
    ///
    /// Dropping the server does the same thing without waiting, so a test only
    /// needs this when it wants the port released before the next line.
    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for SimServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve(State(simulator): State<Arc<Simulator>>, request: Request) -> Response {
    let method = Method::parse(request.method().as_str()).unwrap_or(Method::Get);
    let uri = request.uri().clone();
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();

    let body = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();

    let sim_request = SimRequest {
        method,
        path: crate::api::decode_component(uri.path(), false),
        query: parse_query(uri.query().unwrap_or_default()),
        headers,
        body,
    };

    let response = simulator.handle(&sim_request);

    // Only here. The in-process transport reports the latency and returns at
    // once; over a socket a client's timeout has to have something to time out
    // against.
    if !response.latency.is_zero() {
        tokio::time::sleep(response.latency).await;
    }

    let mut builder = Response::builder().status(response.status);
    for (name, value) in &response.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(response.body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageSpec;

    async fn server() -> SimServer {
        install_crypto_provider();
        SimServer::fresh().await.expect("bind 127.0.0.1:0")
    }

    /// Nothing here speaks TLS, and this is still required.
    ///
    /// Cargo unifies features across the workspace, so under `cargo test` at the
    /// root our `reqwest` is built with `nade-server`'s `rustls-no-provider` and
    /// `Client::new()` panics without a process-wide provider. Per-crate runs
    /// (`cargo test -p nade-gmail-sim`) do not unify and so never saw it - which
    /// is exactly why it hid, since that is the command `CLAUDE.md` documents.
    fn install_crypto_provider() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // Errs only if something already installed one, which is fine.
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn two_servers_get_different_ephemeral_ports() {
        let first = server().await;
        let second = server().await;
        assert_ne!(first.address().port(), second.address().port());
        assert_ne!(first.address().port(), 0);
        assert!(first.base_url().starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn a_real_reqwest_client_can_drive_the_whole_api() {
        let server = server().await;
        let sim = server.simulator();
        sim.insert_message(&MessageSpec::new().subject("hello").text("body"))
            .unwrap();
        let token = sim.access_token();
        let client = reqwest::Client::new();

        let profile: serde_json::Value = client
            .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(profile["messagesTotal"], 1);
        assert!(profile["historyId"].is_string());

        // A query with characters that must be percent-encoded on the way out
        // and decoded on the way in.
        let listed: serde_json::Value = client
            .get(format!("{}/gmail/v1/users/me/messages", server.base_url()))
            .query(&[("q", "newer_than:30d is:unread"), ("maxResults", "10")])
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_missing_bearer_token_is_a_401_over_the_wire_too() {
        let server = server().await;
        let response = reqwest::Client::new()
            .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["error"]["errors"][0]["reason"], "authError");
    }

    #[tokio::test]
    async fn the_token_endpoint_serves_a_refresh_over_http() {
        let server = server().await;
        let sim = server.simulator();
        let before = sim.access_token();
        sim.expire_access_token();

        let refreshed: serde_json::Value = reqwest::Client::new()
            .post(server.token_url())
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &sim.refresh_token()),
                ("client_id", "sim"),
                ("client_secret", "sim"),
            ])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let fresh = refreshed["access_token"].as_str().unwrap();
        assert_ne!(fresh, before);
        assert_eq!(refreshed["token_type"], "Bearer");

        let status = reqwest::Client::new()
            .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
            .bearer_auth(fresh)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn a_batch_survives_the_socket_intact() {
        let server = server().await;
        let sim = server.simulator();
        let ids: Vec<String> = (0..3)
            .map(|n| {
                sim.insert_message(&MessageSpec::new().subject(format!("m{n}")))
                    .unwrap()
            })
            .collect();

        let boundary = "nade_batch_over_http";
        let mut body = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(b"Content-Type: application/http\r\n");
            body.extend_from_slice(b"Content-Transfer-Encoding: binary\r\n");
            body.extend_from_slice(format!("Content-ID: <item-{index}>\r\n\r\n").as_bytes());
            body.extend_from_slice(
                format!("GET /gmail/v1/users/me/messages/{id}?format=raw HTTP/1.1\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let response = reqwest::Client::new()
            .post(server.batch_url())
            .bearer_auth(sim.access_token())
            .header(
                "Content-Type",
                format!("multipart/mixed; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(content_type.starts_with("multipart/mixed; boundary="));
        let text = response.text().await.unwrap();
        for index in 0..3 {
            assert!(
                text.contains(&format!("Content-ID: <response-item-{index}>")),
                "part {index} missing from {text}"
            );
        }
        assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 3);
    }

    #[tokio::test]
    async fn the_http_and_in_process_paths_return_the_same_bytes() {
        let server = server().await;
        let sim = server.simulator();
        for n in 0..3 {
            sim.insert_message(&MessageSpec::new().subject(format!("m{n}")).text("body"))
                .unwrap();
        }
        let token = sim.access_token();
        let client = reqwest::Client::new();

        for target in [
            "/gmail/v1/users/me/profile",
            "/gmail/v1/users/me/labels",
            "/gmail/v1/users/me/messages?maxResults=2",
            "/gmail/v1/users/me/history?startHistoryId=1000000",
        ] {
            let over_http = client
                .get(format!("{}{target}", server.base_url()))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let in_process = sim.handle(&sim.authorized(target));
            assert_eq!(
                over_http,
                String::from_utf8_lossy(&in_process.body),
                "{target} differs between the two transports"
            );
        }
    }
}
