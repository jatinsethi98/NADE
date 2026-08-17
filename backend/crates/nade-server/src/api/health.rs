//! `GET /v1/healthz` - unauthenticated liveness plus a real database probe.

use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

use crate::state::AppState;

/// A hung database must not hang the health check.
const DB_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
    pub db: &'static str,
    pub version: &'static str,
}

/// `200 {"status":"ok","db":"ok","version":"…"}` when the pool can round-trip
/// `select 1`; `503` with `"db":"down"` when it cannot.
pub async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let alive = matches!(
        tokio::time::timeout(
            DB_PROBE_TIMEOUT,
            sqlx::query_scalar::<_, i32>("select 1").fetch_one(&state.pool),
        )
        .await,
        Ok(Ok(1))
    );

    if alive {
        (
            StatusCode::OK,
            Json(Health {
                status: "ok",
                db: "ok",
                version: crate::VERSION,
            }),
        )
    } else {
        // The process is up and serving; only its dependency is missing.
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "degraded",
                db: "down",
                version: crate::VERSION,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::{
        api,
        config::Env,
        state::AppState,
        test_support::{get, response_json, test_app, test_config},
    };

    /// Criteria D1 + D2.
    #[tokio::test]
    async fn healthz_is_unauthenticated_and_reports_ok() {
        let app = test_app(Env::Prod).await;
        let response = get(&app, "/v1/healthz", None).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({ "status": "ok", "db": "ok", "version": crate::VERSION })
        );
    }

    /// Criterion D3.
    #[tokio::test]
    async fn healthz_reports_db_down_when_the_pool_is_unreachable() {
        // Port 1 is reserved and refuses instantly; `connect_lazy` means the
        // pool is built without ever reaching it.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://nade:nade@127.0.0.1:1/nade")
            .unwrap();

        let state = AppState::new(pool, test_config(Env::Prod));
        let router = api::router(state);
        let response = crate::test_support::send(&router, "GET", "/v1/healthz", None, None).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(response).await,
            json!({ "status": "degraded", "db": "down", "version": crate::VERSION })
        );
    }
}
