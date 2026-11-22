//! Authenticated personal reactions and the dynamic favorites playlist.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::{CurrentUser, User},
    db,
    library::reactions::{Favorite, Reaction, ReactionState},
};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Register routes whose account is derived only from bearer authentication.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(me))
        .routes(routes!(get_reaction, set_reaction, clear_reaction))
        .routes(routes!(favorites))
        .with_state(pool)
}

/// Explicit assessment; DELETE represents neutral preferences.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SetReaction {
    /// Replace the user's assessment with like or dislike.
    #[schema(required = true, nullable = false)]
    reaction: Option<Reaction>,
}

/// Dynamic favorites, available even when empty without a copied playlist.
#[derive(Serialize, ToSchema)]
struct FavoritePage {
    /// Current likes ordered newest first, then recording ID descending.
    items: Vec<Favorite>,
    /// Maximum entries in the page.
    limit: i64,
    /// Number of entries skipped.
    offset: i64,
}

/// Read the authenticated identity without credential information.
#[utoipa::path(get, path = "/me", tag = "personal",
    security(("personal_key" = [])), responses(
        (status = 200, body = User, description = "Current account"),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}

/// Read an assessment; neutral returns null for existing recordings.
#[utoipa::path(get, path = "/me/recordings/{id}/reaction", tag = "personal",
    security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1, description = "Recording ID")),
    responses(
        (status = 200, body = ReactionState, description = "Assessment"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_reaction(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<ReactionState>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid recording ID"))?;
    Ok(Json(
        db::reactions::find(&pool, user.id, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Replace an assessment; repeated likes preserve their favorites position.
#[utoipa::path(put, path = "/me/recordings/{id}/reaction", tag = "personal",
    security(("personal_key" = [])), request_body = SetReaction,
    params(("id" = i64, Path, minimum = 1, description = "Recording ID")),
    responses(
        (status = 200, body = ReactionState, description = "Stored"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_reaction(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    payload: Result<Json<SetReaction>, JsonRejection>,
) -> Result<Json<ReactionState>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid recording ID"))?;
    let Json(input) = payload.map_err(ApiError::from)?;
    let reaction = input
        .reaction
        .ok_or(ApiError::Validation("reaction must be like or dislike"))?;
    Ok(Json(
        db::reactions::set(&pool, user.id, id, Some(reaction))
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Clear the assessment and remove the recording from favorites atomically.
#[utoipa::path(delete, path = "/me/recordings/{id}/reaction", tag = "personal",
    security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1, description = "Recording ID")),
    responses(
        (status = 200, body = ReactionState, description = "Cleared"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn clear_reaction(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<ReactionState>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid recording ID"))?;
    Ok(Json(
        db::reactions::set(&pool, user.id, id, None)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Read this user's current likes, with catalog titles and bounded pagination.
#[utoipa::path(get, path = "/me/favorites", tag = "personal",
    security(("personal_key" = [])), params(Pagination), responses(
        (status = 200, body = FavoritePage, description = "Favorites"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn favorites(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<FavoritePage>, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid query"))?;
    page.validate()?;
    let items =
        db::reactions::favorites(&pool, user.id, page.limit, page.offset)
            .await?;
    Ok(Json(FavoritePage {
        items,
        limit: page.limit,
        offset: page.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Authentication, isolation, reaction transitions, and dynamic favorites.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send JSON with an optional bearer key through the complete router.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        value: Option<Value>,
    ) -> (u16, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = if let Some(value) = value {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        } else {
            Body::empty()
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        if status == 401 {
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        }
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// Accounts have isolated preferences and cannot duplicate favorites.
    #[tokio::test]
    async fn isolates_accounts_and_implements_the_favorites_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let pool = db::connect(&database).await.unwrap();
        let alice_key = crate::auth::token().unwrap();
        let bob_key = crate::auth::token().unwrap();
        let alice = db::users::create(
            &pool,
            "alice",
            &crate::auth::digest(&alice_key),
        )
        .await
        .unwrap();
        db::users::create(&pool, "bob", &crate::auth::digest(&bob_key))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Track')")
            .execute(&pool)
            .await
            .unwrap();
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &crate::config::DocsConfig { enabled: true },
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        );
        let path = "/api/v1/me/recordings/1/reaction";
        for token in [None, Some("bad-key")] {
            assert_eq!(
                request(&app, "GET", "/api/v1/me/favorites", token, None)
                    .await
                    .0,
                401
            );
        }
        let (status, me) =
            request(&app, "GET", "/api/v1/me", Some(&alice_key), None).await;
        assert_eq!(status, 200);
        assert_eq!(me, json!({"id":alice.id,"name":"alice","role":"member"}));
        let (_, empty) = request(
            &app,
            "GET",
            "/api/v1/me/favorites",
            Some(&alice_key),
            None,
        )
        .await;
        assert_eq!(empty["items"], json!([]));
        let (_, neutral) =
            request(&app, "GET", path, Some(&alice_key), None).await;
        assert!(neutral["reaction"].is_null());
        let (status, liked) = request(
            &app,
            "PUT",
            path,
            Some(&alice_key),
            Some(json!({"reaction":"like"})),
        )
        .await;
        assert_eq!(status, 200);
        let (_, repeated) = request(
            &app,
            "PUT",
            path,
            Some(&alice_key),
            Some(json!({"reaction":"like"})),
        )
        .await;
        assert_eq!(liked, repeated);
        let (_, favorites) = request(
            &app,
            "GET",
            "/api/v1/me/favorites",
            Some(&alice_key),
            None,
        )
        .await;
        assert_eq!(favorites["items"].as_array().unwrap().len(), 1);
        let (_, bob) =
            request(&app, "GET", "/api/v1/me/favorites", Some(&bob_key), None)
                .await;
        assert_eq!(bob["items"], json!([]));
        assert_eq!(
            request(
                &app,
                "PUT",
                path,
                Some(&alice_key),
                Some(json!({"reaction":"dislike"}))
            )
            .await
            .0,
            200
        );
        let (_, favorites) = request(
            &app,
            "GET",
            "/api/v1/me/favorites",
            Some(&alice_key),
            None,
        )
        .await;
        assert_eq!(favorites["items"], json!([]));
        let (_, cleared) =
            request(&app, "DELETE", path, Some(&alice_key), None).await;
        assert!(cleared["reaction"].is_null());
        assert!(cleared["changed_at"].is_null());
        request(
            &app,
            "PUT",
            path,
            Some(&alice_key),
            Some(json!({"reaction":"like"})),
        )
        .await;
        db::users::revoke(&pool, "alice").await.unwrap();
        assert_eq!(
            request(&app, "GET", "/api/v1/me", Some(&alice_key), None)
                .await
                .0,
            401
        );
        let new_key = crate::auth::token().unwrap();
        let same =
            db::users::issue(&pool, "alice", &crate::auth::digest(&new_key))
                .await
                .unwrap();
        assert_eq!(same.id, alice.id);
        let (_, favorites) =
            request(&app, "GET", "/api/v1/me/favorites", Some(&new_key), None)
                .await;
        assert_eq!(favorites["items"].as_array().unwrap().len(), 1);
        let (_, schema) =
            request(&app, "GET", "/openapi.json", None, None).await;
        assert_eq!(
            schema["components"]["securitySchemes"]["personal_key"]["scheme"],
            "bearer"
        );
        assert!(
            schema["paths"]["/api/v1/me/favorites"]["get"]["security"]
                .is_array()
        );
        pool.close().await;
        let pool = db::connect(&database).await.unwrap();
        assert_eq!(
            db::users::authenticate(&pool, &crate::auth::digest(&new_key))
                .await
                .unwrap()
                .unwrap()
                .id,
            alice.id
        );
        assert_eq!(
            db::reactions::favorites(&pool, alice.id, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        pool.close().await;
    }

    /// Invalid data and attempted user-ID injection never create preferences.
    #[tokio::test]
    async fn validates_requests_and_rejects_credential_ambiguity() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let token = crate::auth::token().unwrap();
        db::users::create(&pool, "alice", &crate::auth::digest(&token))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Track')")
            .execute(&pool)
            .await
            .unwrap();
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &crate::config::DocsConfig::default(),
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        );
        let path = "/api/v1/me/recordings/1/reaction";
        for value in [
            json!({}),
            json!({"reaction":null}),
            json!({"reaction":"neutral"}),
            json!({"reaction":"like","user_id":2}),
        ] {
            assert_eq!(
                request(&app, "PUT", path, Some(&token), Some(value))
                    .await
                    .0,
                422
            );
        }
        for method in ["GET", "PUT", "DELETE"] {
            assert_eq!(
                request(
                    &app,
                    method,
                    "/api/v1/me/recordings/999/reaction",
                    Some(&token),
                    Some(json!({"reaction":"like"}))
                )
                .await
                .0,
                404
            );
        }
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/me/favorites?user_id=2",
                Some(&token),
                None
            )
            .await
            .0,
            400
        );
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/me/favorites?limit=0",
                Some(&token),
                None
            )
            .await
            .0,
            400
        );
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        pool.close().await;
        let (status, error) =
            request(&app, "GET", "/api/v1/me", Some(&token), None).await;
        assert_eq!(status, 500);
        assert_eq!(error["error"]["message"], "Internal server error");
    }
}
