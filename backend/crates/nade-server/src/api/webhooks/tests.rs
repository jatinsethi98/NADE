//! Webhook tests.
//!
//! The forgery matrix is the point. "Forged JWT → 401" is only a real claim if
//! every *class* of forgery is tried, because each one is defeated by a
//! different check and any of them could be missing.
//!
//! Every rejection asserts four things: the status, the exact contract body,
//! that **no job was enqueued**, and that `last_webhook_at` was not stamped. A
//! test that checked only the status would pass on a handler that rejected the
//! response but did the work anyway.

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts, RsaPrivateKey};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use crate::{
    config::Env,
    test_support::{fixture, response_json, test_app, TestApp},
};

const KID: &str = "test-key-1";
const AUDIENCE: &str = "https://tunnel.test/v1/webhooks/gmail";
const SA: &str = "nade-push@deliveriesapp-293223.iam.gserviceaccount.com";
const ISSUER: &str = "https://accounts.google.com";

/// An RSA keypair plus the JWK Set that publishes it.
struct Signer {
    key: RsaPrivateKey,
    encoding: EncodingKey,
}

impl Signer {
    fn new() -> Self {
        // 2048 is Google's size. Generating it per test is a second or two;
        // these tests are worth it.
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("generating a test key");
        let pem = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        let encoding = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("a usable signing key");
        Self { key, encoding }
    }

    /// The JWK Set Google would publish for this key.
    fn jwks(&self, kid: &str) -> Value {
        let url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        json!({"keys":[{
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": kid,
            "n": url.encode(self.key.n().to_bytes_be()),
            "e": url.encode(self.key.e().to_bytes_be()),
        }]})
    }

    fn sign(&self, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, &self.encoding).expect("signing")
    }
}

fn claims() -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "azp": SA,
        "email": SA,
        "email_verified": true,
        "iat": now,
        "exp": now + 600,
    })
}

/// A Pub/Sub push body carrying a Gmail notification for `email`.
fn push_body(email: &str, history_id: u64, url_safe: bool) -> Vec<u8> {
    let inner = format!(r#"{{"emailAddress":"{email}","historyId":{history_id}}}"#);
    let encoded = if url_safe {
        base64::engine::general_purpose::URL_SAFE.encode(inner)
    } else {
        base64::engine::general_purpose::STANDARD.encode(inner)
    };
    format!(
        r#"{{"message":{{"data":"{encoded}","messageId":"12345"}},"subscription":"projects/p/subscriptions/s"}}"#
    )
    .into_bytes()
}

/// An app whose verifier trusts `server`'s key set, with one account connected.
async fn app_trusting(server: &MockServer) -> (TestApp, Uuid) {
    let mut app = test_app(Env::Dev).await;
    app.set_push(&format!("{}/certs", server.uri()), AUDIENCE, SA);

    let account: Uuid = sqlx::query_scalar(
        "insert into accounts (email, status) values ('jatinsethi98@gmail.com', 'ok') returning id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    (app, account)
}

async fn serve_jwks(server: &MockServer, jwks: Value) {
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

async fn jobs_for(app: &TestApp, kind: &str) -> i64 {
    sqlx::query_scalar("select count(*) from jobs where kind = $1")
        .bind(kind)
        .fetch_one(&app.db.pool)
        .await
        .unwrap()
}

async fn webhook_stamped(app: &TestApp, account: Uuid) -> bool {
    sqlx::query_scalar::<_, Option<bool>>(
        "select last_webhook_at is not null from sync_state where account_id = $1",
    )
    .bind(account)
    .fetch_optional(&app.db.pool)
    .await
    .unwrap()
    .flatten()
    .unwrap_or(false)
}

/// POST a push with a raw `Authorization` value - raw, because some of the
/// forgery classes are about the header's *shape*, which `test_support::send`
/// would normalise away by prefixing `Bearer `.
async fn post_push(
    app: &TestApp,
    authorization: Option<&str>,
    body: Vec<u8>,
) -> axum::http::Response<axum::body::Body> {
    use tower::ServiceExt as _;

    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/webhooks/gmail")
        .header(axum::http::header::CONTENT_TYPE, "application/json");
    if let Some(value) = authorization {
        builder = builder.header(axum::http::header::AUTHORIZATION, value);
    }
    app.router
        .clone()
        .oneshot(builder.body(axum::body::Body::from(body)).unwrap())
        .await
        .unwrap()
}

/// Every rejection asserts the same four things. A test that checked only the
/// status would pass on a handler that answered 401 and did the work anyway.
async fn assert_rejected(app: &TestApp, account: Uuid, authorization: Option<&str>, why: &str) {
    let response = post_push(
        app,
        authorization,
        push_body("jatinsethi98@gmail.com", 42, true),
    )
    .await;

    assert_eq!(response.status(), 401, "{why}: wrong status");
    assert_eq!(
        response_json(response).await,
        fixture("error_unauthorized.json"),
        "{why}: the body must be identical for every forgery class"
    );
    assert_eq!(
        jobs_for(app, crate::sync::incremental::KIND).await,
        0,
        "{why}: work was enqueued"
    );
    assert!(
        !webhook_stamped(app, account).await,
        "{why}: the push was recorded"
    );
}

// ------------------------------------------------------ the happy path --

/// Criterion T1 - a correctly signed push is a `204` and exactly one job.
#[tokio::test]
async fn a_correctly_signed_push_returns_204_and_enqueues_exactly_one_job() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let token = signer.sign(KID, &claims());
    let response = post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("jatinsethi98@gmail.com", 9_412_800, true),
    )
    .await;

    assert_eq!(response.status(), 204);
    assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 1);
    assert!(webhook_stamped(&app, account).await);
}

/// Criterion T4 - the notification's own `historyId` is never written to
/// `sync_state`. That single rule is what makes Pub/Sub's out-of-order,
/// at-least-once delivery irrelevant.
#[tokio::test]
async fn the_push_bodys_history_id_is_never_written_to_sync_state() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let token = signer.sign(KID, &claims());
    post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("jatinsethi98@gmail.com", 9_999_999, true),
    )
    .await;

    let cursor: Option<i64> =
        sqlx::query_scalar("select last_history_id from sync_state where account_id = $1")
            .bind(account)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(cursor, None, "the push moved the cursor");
}

/// Criterion T3 - a burst collapses to one job, and the extras are not lost:
/// they become the rerun flag the running walk consumes.
#[tokio::test]
async fn a_duplicate_push_enqueues_one_job_and_records_a_rerun() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;
    let token = signer.sign(KID, &claims());
    let bearer = format!("Bearer {token}");

    for _ in 0..3 {
        let response = post_push(
            &app,
            Some(&bearer),
            push_body("jatinsethi98@gmail.com", 42, true),
        )
        .await;
        assert_eq!(response.status(), 204);
    }

    assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 1);
    let rerun: bool =
        sqlx::query_scalar("select rerun_requested from sync_state where account_id = $1")
            .bind(account)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert!(rerun, "the suppressed notifications were dropped");
}

/// Criterion T7 - `message.data` arrives base64url **and** standard, padded and
/// not. Asserting either one is how a real notification gets permanently
/// acknowledged as malformed.
///
/// The unpadded cases matter: the first version of this test only ever produced
/// padded strings, so "padded and not" was a claim with no evidence.
#[tokio::test]
async fn message_data_is_accepted_unpadded_in_either_alphabet() {
    for url_safe in [true, false] {
        let server = MockServer::start().await;
        let signer = Signer::new();
        serve_jwks(&server, signer.jwks(KID)).await;
        let (app, _) = app_trusting(&server).await;
        let token = signer.sign(KID, &claims());

        let inner = r#"{"emailAddress":"jatinsethi98@gmail.com","historyId":42}"#;
        let encoded = if url_safe {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(inner)
        } else {
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(inner)
        };
        assert!(!encoded.ends_with('='), "this case must be unpadded");

        let body = format!(r#"{{"message":{{"data":"{encoded}","messageId":"1"}}}}"#).into_bytes();
        let response = post_push(&app, Some(&format!("Bearer {token}")), body).await;
        assert_eq!(response.status(), 204, "unpadded, url_safe = {url_safe}");
        assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 1);
    }
}

#[tokio::test]
async fn message_data_is_accepted_in_either_base64_alphabet() {
    for url_safe in [true, false] {
        let server = MockServer::start().await;
        let signer = Signer::new();
        serve_jwks(&server, signer.jwks(KID)).await;
        let (app, _) = app_trusting(&server).await;
        let token = signer.sign(KID, &claims());

        let response = post_push(
            &app,
            Some(&format!("Bearer {token}")),
            push_body("jatinsethi98@gmail.com", 42, url_safe),
        )
        .await;
        assert_eq!(response.status(), 204, "url_safe = {url_safe}");
        assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 1);
    }
}

/// Criterion T8 - Gmail sends `historyId` as a JSON **number** here, though the
/// REST API returns it as a string. Both must parse, or every push is a 500.
#[tokio::test]
async fn a_history_id_that_is_a_number_or_a_string_both_parse() {
    for inner in [
        r#"{"emailAddress":"jatinsethi98@gmail.com","historyId":9412800}"#,
        r#"{"emailAddress":"jatinsethi98@gmail.com","historyId":"9412800"}"#,
    ] {
        let server = MockServer::start().await;
        let signer = Signer::new();
        serve_jwks(&server, signer.jwks(KID)).await;
        let (app, _) = app_trusting(&server).await;
        let token = signer.sign(KID, &claims());

        let encoded = base64::engine::general_purpose::URL_SAFE.encode(inner);
        let body = format!(r#"{{"message":{{"data":"{encoded}","messageId":"1"}}}}"#).into_bytes();

        let response = post_push(&app, Some(&format!("Bearer {token}")), body).await;
        assert_eq!(response.status(), 204, "{inner}");
        assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 1);
    }
}

/// Criterion T6 - an unreadable body from an **already-verified** sender is a
/// `204`, not a `400`. Only Google can send a verified body, so a shape we
/// cannot read means the shape changed, and a 400 would make Pub/Sub retry a
/// poison message for ever.
#[tokio::test]
async fn an_unreadable_body_from_a_verified_sender_is_204_not_400() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, _) = app_trusting(&server).await;
    let token = signer.sign(KID, &claims());
    let bearer = format!("Bearer {token}");

    for body in [
        Vec::new(),
        b"{}".to_vec(),
        b"not json at all".to_vec(),
        br#"{"message":{"data":"!!!not base64!!!"}}"#.to_vec(),
        br#"{"message":{"data":"eyJub3QiOiJnbWFpbCJ9"}}"#.to_vec(),
    ] {
        let response = post_push(&app, Some(&bearer), body.clone()).await;
        assert_eq!(
            response.status(),
            204,
            "an unreadable body must be acknowledged: {body:?}"
        );
    }
    assert_eq!(
        jobs_for(&app, crate::sync::incremental::KIND).await,
        0,
        "an unreadable body must not enqueue work"
    );
}

/// Criterion T5, re-scoped by D45: a push routes by its **address**. One this
/// server does not hold is acknowledged and audited to no account - inventing
/// an owner would file evidence under the wrong user.
#[tokio::test]
async fn a_push_for_an_unknown_mailbox_is_acknowledged_and_ignored() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;
    let token = signer.sign(KID, &claims());

    let response = post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("someone.else@gmail.com", 42, true),
    )
    .await;

    assert_eq!(response.status(), 204);
    assert_eq!(jobs_for(&app, crate::sync::incremental::KIND).await, 0);
    assert!(!webhook_stamped(&app, account).await);

    let audited: Vec<Option<Uuid>> = sqlx::query_scalar(
        "select account_id from audit_log where action = 'gmail_push_unknown_mailbox'",
    )
    .fetch_all(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(
        audited,
        vec![None],
        "the unrouted push must leave evidence, owned by nobody"
    );
}

/// The point of routing by address: with two accounts on one server, each push
/// drives its own mailbox's cursor and nobody else's.
#[tokio::test]
async fn a_push_routes_to_the_account_its_address_names() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, first) = app_trusting(&server).await;
    let second: Uuid = sqlx::query_scalar(
        "insert into accounts (email, status) values ('someone.else@gmail.com', 'ok') returning id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let token = signer.sign(KID, &claims());

    // EDGE (unicode/case): Gmail echoes whatever the consent typed, so the
    // match must ignore case.
    let response = post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("Someone.Else@Gmail.com", 7, true),
    )
    .await;

    assert_eq!(response.status(), 204);
    assert!(webhook_stamped(&app, second).await);
    assert!(
        !webhook_stamped(&app, first).await,
        "the other account's cursor must not move"
    );
    let dedupe: String = sqlx::query_scalar("select dedupe_key from jobs where kind = $1")
        .bind(crate::sync::incremental::KIND)
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        dedupe,
        format!("{}:{}", crate::sync::incremental::KIND, second),
        "the job is deduplicated per account, keyed by the addressed one"
    );
}

// ------------------------------------------------- the forgery matrix --

#[tokio::test]
async fn a_push_with_no_authorization_header_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;
    assert_rejected(&app, account, None, "no header").await;
}

#[tokio::test]
async fn a_non_bearer_authorization_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;
    let token = signer.sign(KID, &claims());

    assert_rejected(&app, account, Some(&format!("Basic {token}")), "not Bearer").await;
    assert_rejected(&app, account, Some(&token), "no scheme").await;
    assert_rejected(&app, account, Some("Bearer "), "empty bearer").await;
}

#[tokio::test]
async fn a_bearer_that_is_not_a_jwt_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    assert_rejected(&app, account, Some("Bearer not-a-jwt"), "not a JWT").await;
    assert_rejected(&app, account, Some("Bearer a.b.c"), "three junk segments").await;
}

/// `alg: none` is the oldest JWT attack there is.
#[tokio::test]
async fn an_alg_none_token_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = url.encode(format!(r#"{{"alg":"none","kid":"{KID}"}}"#));
    let payload = url.encode(claims().to_string());
    let token = format!("{header}.{payload}.");

    assert_rejected(&app, account, Some(&format!("Bearer {token}")), "alg none").await;
}

/// Algorithm confusion: sign with HMAC using the RSA modulus as the secret, and
/// hope the verifier picks the algorithm from the header.
#[tokio::test]
async fn an_algorithm_confusion_token_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let modulus = signer.key.n().to_bytes_be();
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    let token = jsonwebtoken::encode(&header, &claims(), &EncodingKey::from_secret(&modulus))
        .expect("signing an HS256 forgery");

    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "HS256 algorithm confusion",
    )
    .await;
}

/// A well-formed RS256 token signed by a key that is not Google's.
#[tokio::test]
async fn a_token_signed_by_the_wrong_key_is_unauthorized() {
    let server = MockServer::start().await;
    let google = Signer::new();
    serve_jwks(&server, google.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let attacker = Signer::new();
    let token = attacker.sign(KID, &claims());

    assert_rejected(&app, account, Some(&format!("Bearer {token}")), "wrong key").await;
}

#[tokio::test]
async fn an_unknown_key_id_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let token = signer.sign("some-other-kid", &claims());
    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "unknown kid",
    )
    .await;
}

#[tokio::test]
async fn a_token_for_another_audience_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let mut claims = claims();
    claims["aud"] = json!("https://someone-elses-tunnel.test/v1/webhooks/gmail");
    let token = signer.sign(KID, &claims);

    assert_rejected(&app, account, Some(&format!("Bearer {token}")), "wrong aud").await;
}

#[tokio::test]
async fn a_token_from_another_issuer_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let mut claims = claims();
    claims["iss"] = json!("https://accounts.evil.example");
    let token = signer.sign(KID, &claims);

    assert_rejected(&app, account, Some(&format!("Bearer {token}")), "wrong iss").await;
}

#[tokio::test]
async fn a_token_from_another_service_account_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let mut claims = claims();
    claims["email"] = json!("someone-else@some-project.iam.gserviceaccount.com");
    let token = signer.sign(KID, &claims);

    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "wrong email",
    )
    .await;
}

#[tokio::test]
async fn an_unverified_email_claim_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let mut claims = claims();
    claims["email_verified"] = json!(false);
    let token = signer.sign(KID, &claims);

    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "unverified",
    )
    .await;
}

#[tokio::test]
async fn an_expired_token_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let now = chrono::Utc::now().timestamp();
    let mut claims = claims();
    // Past the 60-second leeway.
    claims["exp"] = json!(now - 120);
    claims["iat"] = json!(now - 600);
    let token = signer.sign(KID, &claims);

    assert_rejected(&app, account, Some(&format!("Bearer {token}")), "expired").await;
}

/// A token *issued* in the future. `jsonwebtoken` validates `exp` and `nbf`
/// and has no opinion on `iat`, so this is checked by hand — and without the
/// check it was accepted.
#[tokio::test]
async fn a_token_issued_in_the_future_is_unauthorized() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let now = chrono::Utc::now().timestamp();
    let mut claims = claims();
    claims["iat"] = json!(now + 3600);
    claims["exp"] = json!(now + 7200);
    let token = signer.sign(KID, &claims);

    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "future iat",
    )
    .await;
}

/// ...and one issued just inside the leeway is fine, so the check above is a
/// boundary rather than a ban.
#[tokio::test]
async fn a_token_issued_within_the_leeway_is_accepted() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, _) = app_trusting(&server).await;

    let now = chrono::Utc::now().timestamp();
    let mut claims = claims();
    claims["iat"] = json!(now + 30);
    let token = signer.sign(KID, &claims);

    let response = post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("jatinsethi98@gmail.com", 42, true),
    )
    .await;
    assert_eq!(
        response.status(),
        204,
        "30s of skew is inside the 60s leeway"
    );
}

/// Both of Google's legal issuers are accepted.
#[tokio::test]
async fn either_of_googles_two_issuers_is_accepted() {
    for issuer in ["https://accounts.google.com", "accounts.google.com"] {
        let server = MockServer::start().await;
        let signer = Signer::new();
        serve_jwks(&server, signer.jwks(KID)).await;
        let (app, _) = app_trusting(&server).await;

        let mut claims = claims();
        claims["iss"] = json!(issuer);
        let token = signer.sign(KID, &claims);

        let response = post_push(
            &app,
            Some(&format!("Bearer {token}")),
            push_body("jatinsethi98@gmail.com", 42, true),
        )
        .await;
        assert_eq!(response.status(), 204, "{issuer} is a legal Google issuer");
    }
}

/// **Key rotation while the cache is still fresh.**
///
/// Google rotates, and the new `kid` arrives inside our TTL. Gating the
/// refetch on freshness rejected every push until the TTL expired — up to the
/// 24-hour clamp — which is an outage caused entirely by our own cache.
#[tokio::test]
async fn a_rotated_key_is_fetched_even_while_the_cached_set_is_fresh() {
    let server = MockServer::start().await;
    let old = Signer::new();
    let new = Signer::new();

    // First /certs call serves the old key, later ones the new key.
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(old.jwks("old-kid")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(new.jwks("new-kid")))
        .mount(&server)
        .await;

    let (app, _) = app_trusting(&server).await;

    // Warm the cache with the old key by sending a token it can verify.
    let warm = post_push(
        &app,
        Some(&format!("Bearer {}", old.sign("old-kid", &claims()))),
        push_body("jatinsethi98@gmail.com", 1, true),
    )
    .await;
    assert_eq!(warm.status(), 204, "the old key must verify first");

    // Now a token signed by the rotated key, while the set is still fresh.
    let rotated = post_push(
        &app,
        Some(&format!("Bearer {}", new.sign("new-kid", &claims()))),
        push_body("jatinsethi98@gmail.com", 2, true),
    )
    .await;
    assert_eq!(
        rotated.status(),
        204,
        "a rotated key must be fetched rather than waiting out the TTL"
    );
}

/// **The stale-key fallback, which criterion T12 claims.**
///
/// Google's JWKS goes away; the key we already hold still verifies. Rejecting
/// the token would turn a transient Google outage into a mail outage.
#[tokio::test]
async fn a_cached_key_still_verifies_when_the_jwks_stops_answering() {
    let server = MockServer::start().await;
    let signer = Signer::new();

    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signer.jwks(KID)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let (app, _) = app_trusting(&server).await;
    let bearer = format!("Bearer {}", signer.sign(KID, &claims()));

    let first = post_push(
        &app,
        Some(&bearer),
        push_body("jatinsethi98@gmail.com", 1, true),
    )
    .await;
    assert_eq!(first.status(), 204);

    // The key set is now unreachable, but the key is cached and still correct.
    let second = post_push(
        &app,
        Some(&bearer),
        push_body("jatinsethi98@gmail.com", 2, true),
    )
    .await;
    assert_eq!(
        second.status(),
        204,
        "a cached key that still verifies beats dropping the user's mail"
    );
}

/// **The one the whole design turns on.** `aud` alone is forgeable, so a token
/// with a correct audience and nothing else must still be refused.
#[tokio::test]
async fn audience_alone_is_not_enough() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (app, account) = app_trusting(&server).await;

    let now = chrono::Utc::now().timestamp();
    let token = signer.sign(
        KID,
        &json!({"iss": ISSUER, "aud": AUDIENCE, "iat": now, "exp": now + 600}),
    );

    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "aud correct, email claims absent",
    )
    .await;
}

/// An unconfigured webhook rejects everything. Fail-closed is the only safe
/// default: a half-configured server that accepted pushes would be worse than
/// one that accepted none.
#[tokio::test]
async fn an_unconfigured_webhook_rejects_a_genuine_push() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    serve_jwks(&server, signer.jwks(KID)).await;
    let (mut app, account) = app_trusting(&server).await;
    app.clear_push_claims();

    let token = signer.sign(KID, &claims());
    assert_rejected(
        &app,
        account,
        Some(&format!("Bearer {token}")),
        "no claims configured",
    )
    .await;
}

/// Criterion T13 - a cold, unreachable key set is a `502`, not a `401`. Both
/// make Pub/Sub retry, but only one of them is true.
#[tokio::test]
async fn a_jwks_that_cannot_be_fetched_at_all_is_a_502_not_a_401() {
    let server = MockServer::start().await;
    let signer = Signer::new();
    // No /certs mount: every fetch 404s, so the cache never warms.
    let (app, _) = app_trusting(&server).await;

    let token = signer.sign(KID, &claims());
    let response = post_push(
        &app,
        Some(&format!("Bearer {token}")),
        push_body("jatinsethi98@gmail.com", 42, true),
    )
    .await;

    assert_eq!(
        response.status(),
        502,
        "a cold key set is an upstream failure, not a forgery"
    );
}
