//! Determinism is a hard requirement, so it gets its own suite.
//!
//! Two claims are pinned here, and both are the kind of thing that quietly stops
//! being true:
//!
//! * **A8** — the same script of calls against two fresh simulators produces
//!   byte-identical responses. Not "equivalent JSON": the same bytes. A single
//!   `Uuid::new_v4` for a boundary, or one `HashMap` iteration order leaking into
//!   an array, breaks this and nothing else would notice.
//! * **A7** — the HTTP transport and the in-process transport return the same
//!   bytes for the same request, because they are the same implementation.

use std::{sync::Arc, time::Duration};

use nade_gmail_sim::{
    api::SimRequest,
    fault::{Fault, FaultRule},
    http::SimServer,
    message::ReceivedAt,
    seed::{seed_mime_corpus, SeedOptions},
    MessageSpec, Scenario, Simulator, Target,
};

/// Nothing here speaks TLS, and this is still required.
///
/// Cargo unifies features across the workspace, so under `cargo test` at the
/// root our `reqwest` is built with `nade-server`'s `rustls-no-provider`, and
/// `reqwest::Client::new()` panics without a process-wide provider. An
/// integration test is its own binary, so installing it in the library's own
/// tests does not reach here.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Errs only if something already installed one, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A mailbox and a script rich enough that almost any nondeterminism shows up:
/// every endpoint, a batch, a fault, paging, unicode, an attachment, a thread.
fn build_world(sim: &Simulator) -> Vec<String> {
    sim.mailbox_mut(|mailbox| {
        seed_mime_corpus(mailbox, &SeedOptions::default().limit(8)).expect("corpus seeds")
    });

    // Threading needs a real id, and `MessageSpec::reply_to` takes one at build
    // time, so the two inserts that thread together are made directly rather
    // than through the scenario.
    let unicode = sim
        .insert_message(
            &MessageSpec::new()
                .subject("Föhn — Ihre Bestellung 📦")
                .from("Versand <versand@beispiel.de>")
                .text("Don't miss out")
                .html("<p>Don&#39;t miss out</p>")
                .received_at(ReceivedAt::AtMs(1_788_000_000_000)),
        )
        .expect("insert");
    let report = sim
        .insert_message(
            &MessageSpec::new()
                .subject("Report")
                .attachment("r.pdf", "application/pdf", b"%PDF-1.4 body".to_vec())
                .received_at(ReceivedAt::AtMs(1_788_100_000_000)),
        )
        .expect("insert");

    let outcome = Scenario::new()
        .advance(Duration::from_secs(3600))
        .insert(
            MessageSpec::new()
                .subject("Re: Report")
                .reply_to(&report)
                .received_at(ReceivedAt::AtMs(1_788_110_000_000)),
        )
        .mark_read(Target::Id(unicode.clone()))
        .add_label(Target::Id(report.clone()), "STARRED")
        .trash(Target::Id(unicode.clone()))
        .run(sim)
        .expect("scenario applies");

    let mut ids = vec![unicode, report];
    ids.extend(outcome.inserted);
    ids
}

/// Every call the script makes, as `(method, target, body)`.
fn script(ids: &[String]) -> Vec<SimRequest> {
    let mut calls = vec![
        SimRequest::get("/gmail/v1/users/me/profile"),
        SimRequest::get("/gmail/v1/users/me/labels"),
        SimRequest::get("/gmail/v1/users/me/labels/INBOX"),
        SimRequest::get("/gmail/v1/users/me/messages"),
        SimRequest::get("/gmail/v1/users/me/messages?maxResults=3"),
        SimRequest::get("/gmail/v1/users/me/messages?q=newer_than%3A30d&maxResults=4"),
        SimRequest::get("/gmail/v1/users/me/messages?q=is%3Aunread"),
        SimRequest::get("/gmail/v1/users/me/messages?q=has%3Aattachment"),
        SimRequest::get("/gmail/v1/users/me/messages?includeSpamTrash=true&maxResults=50"),
        SimRequest::get("/gmail/v1/users/me/threads?maxResults=5"),
        SimRequest::get("/gmail/v1/users/me/history?startHistoryId=1000000"),
        SimRequest::get(
            "/gmail/v1/users/me/history?startHistoryId=1000000&maxResults=2\
             &historyTypes=messageAdded&historyTypes=labelRemoved",
        ),
        SimRequest::post(
            "/gmail/v1/users/me/watch",
            br#"{"topicName":"projects/p/topics/gmail-events"}"#.to_vec(),
        ),
        SimRequest::post("/gmail/v1/users/me/stop", Vec::new()),
    ];

    for id in ids.iter().take(4) {
        for format in ["minimal", "metadata", "full", "raw"] {
            calls.push(SimRequest::get(&format!(
                "/gmail/v1/users/me/messages/{id}?format={format}"
            )));
        }
    }
    calls.push(SimRequest::get(
        "/gmail/v1/users/me/messages/does-not-exist",
    ));

    // A batch, which exercises the boundary generator and the response ordering.
    let boundary = "nade_determinism";
    let mut body = Vec::new();
    for (index, id) in ids.iter().take(5).enumerate() {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/http\r\n");
        body.extend_from_slice(b"Content-Transfer-Encoding: binary\r\n");
        body.extend_from_slice(format!("Content-ID: <item-{index}>\r\n\r\n").as_bytes());
        body.extend_from_slice(
            format!("GET /gmail/v1/users/me/messages/{id}?format=raw HTTP/1.1\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    calls.push(SimRequest::post("/batch/gmail/v1", body).header(
        "Content-Type",
        format!("multipart/mixed; boundary={boundary}"),
    ));

    calls
}

/// One call's answer, reduced to everything a client could observe.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Run the script and collect every answer.
fn run_script(sim: &Simulator) -> Vec<Answer> {
    let ids = build_world(sim);
    // One fault, so the failure path is in the fingerprint too.
    sim.inject(
        FaultRule::new(Fault::Throttle {
            retry_after_seconds: Some(4),
        })
        .on_path_containing("/threads")
        .on_nth_call(1),
    );
    let token = sim.access_token();
    script(&ids)
        .into_iter()
        .map(|request| {
            let response = sim.handle(&request.bearer(&token));
            Answer {
                status: response.status,
                headers: response.headers,
                body: response.body,
            }
        })
        .collect()
}

#[test]
fn the_same_script_twice_is_byte_identical() {
    let first = run_script(&Simulator::new());
    let second = run_script(&Simulator::new());

    assert_eq!(
        first.len(),
        second.len(),
        "the script must make the same number of calls"
    );
    for (index, (left, right)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(left.status, right.status, "call {index}: status differs");
        assert_eq!(left.headers, right.headers, "call {index}: headers differ");
        assert_eq!(
            String::from_utf8_lossy(&left.body),
            String::from_utf8_lossy(&right.body),
            "call {index}: body differs"
        );
    }
    // Belt and braces: one hash over everything.
    assert_eq!(first, second);
}

#[test]
fn a_third_run_still_matches_so_the_state_is_not_carried_between_them() {
    let first = run_script(&Simulator::new());
    let _ = run_script(&Simulator::new());
    let third = run_script(&Simulator::new());
    assert_eq!(first, third);
}

#[test]
fn repeating_one_request_against_unchanged_state_returns_the_same_bytes() {
    let sim = Simulator::new();
    build_world(&sim);
    let token = sim.access_token();
    for request in script(&[]) {
        // Skip the mutating pair: `watch` records an expiry and `stop` clears
        // it, so the second call legitimately sees different state.
        if request.path.ends_with("/watch") || request.path.ends_with("/stop") {
            continue;
        }
        let once = sim.handle(&request.clone().bearer(&token));
        let twice = sim.handle(&request.bearer(&token));
        assert_eq!(
            once.body, twice.body,
            "a read repeated against unchanged state must be identical: {}",
            once.status
        );
    }
}

#[tokio::test]
async fn http_and_in_process_agree_byte_for_byte() {
    install_crypto_provider();
    // Two simulators driven identically, one behind a socket and one not. If the
    // transports diverged at all — a header dropped, a query parameter decoded
    // differently, a body truncated — the bytes would differ here.
    let in_process = Simulator::new();
    let over_http = Arc::new(Simulator::new());
    let ids = build_world(&in_process);
    let _ = build_world(&over_http);

    let server = SimServer::start(Arc::clone(&over_http))
        .await
        .expect("bind 127.0.0.1:0");
    let client = reqwest::Client::new();
    let token = in_process.access_token();

    for request in script(&ids) {
        let url = format!("{}{}", server.base_url(), request.target());
        let mut builder = match request.method {
            nade_gmail_sim::api::Method::Post => client.post(&url).body(request.body.clone()),
            _ => client.get(&url),
        };
        builder = builder.bearer_auth(&token);
        if let Some(content_type) = request.header_value("content-type") {
            builder = builder.header("Content-Type", content_type);
        }

        let remote = builder.send().await.expect("the server answers");
        let remote_status = remote.status().as_u16();
        let remote_body = remote.bytes().await.expect("a body").to_vec();

        let target = request.target();
        let local = in_process.handle(&request.bearer(&token));
        assert_eq!(remote_status, local.status, "status differs for {target}");
        assert_eq!(
            String::from_utf8_lossy(&remote_body),
            String::from_utf8_lossy(&local.body),
            "body differs for {target}"
        );
    }
}

#[tokio::test]
async fn the_http_transport_preserves_the_headers_that_carry_meaning() {
    install_crypto_provider();
    let sim = Arc::new(Simulator::new());
    sim.insert_message(&MessageSpec::new()).unwrap();
    sim.inject(
        FaultRule::new(Fault::Throttle {
            retry_after_seconds: Some(11),
        })
        .times(1),
    );
    let server = SimServer::start(Arc::clone(&sim)).await.unwrap();

    let response = reqwest::Client::new()
        .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
        .bearer_auth(sim.access_token())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.headers().get("retry-after").unwrap(),
        "11",
        "a retry policy reads this header, so the socket must not eat it"
    );
    assert!(response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/json"));
}

#[tokio::test]
async fn a_slow_fault_really_waits_over_http_so_a_client_timeout_can_fire() {
    install_crypto_provider();
    let sim = Arc::new(Simulator::new());
    sim.inject(FaultRule::new(Fault::Delay(Duration::from_millis(400))).times(1));
    let server = SimServer::start(Arc::clone(&sim)).await.unwrap();

    let timed_out = reqwest::Client::builder()
        .timeout(Duration::from_millis(80))
        .build()
        .unwrap()
        .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
        .bearer_auth(sim.access_token())
        .send()
        .await;
    assert!(
        timed_out.is_err(),
        "the client's own timeout must be reachable; got {timed_out:?}"
    );

    // The fault was one-shot, so the next call is prompt.
    let prompt = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("{}/gmail/v1/users/me/profile", server.base_url()))
        .bearer_auth(sim.access_token())
        .send()
        .await
        .expect("no fault left");
    assert_eq!(prompt.status(), 200);
}

#[test]
fn generated_ids_tokens_and_boundaries_do_not_move_between_runs() {
    let ids_of = |sim: &Simulator| -> Vec<String> {
        sim.mailbox(|mailbox| {
            mailbox
                .messages_newest_first()
                .into_iter()
                .map(|message| message.id.clone())
                .collect()
        })
    };
    let first = Simulator::new();
    let second = Simulator::new();
    build_world(&first);
    build_world(&second);
    assert_eq!(ids_of(&first), ids_of(&second));

    let token_of = |sim: &Simulator| {
        sim.handle(&sim.authorized("/gmail/v1/users/me/messages?maxResults=2"))
            .json_body()["nextPageToken"]
            .as_str()
            .map(ToOwned::to_owned)
    };
    assert_eq!(token_of(&first), token_of(&second));
    assert!(token_of(&first).is_some());
}
