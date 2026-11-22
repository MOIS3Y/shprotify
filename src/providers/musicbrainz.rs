//! MusicBrainz search/lookup normalization and Cover Art Archive transport.

use super::Transport;
use crate::library::{
    artwork::{self, Artwork},
    enrichment::{ArtistCredit, EntityKind, Proposal, mbid},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

/// Normalize one bounded upstream entity; malformed provider data fails
/// closed.
pub(crate) fn proposal(kind: EntityKind, value: &Value) -> Result<Proposal> {
    let id = mbid(value["id"].as_str().context("Missing provider ID")?)
        .map_err(anyhow::Error::msg)?;
    let name = value[if kind == EntityKind::Artist {
        "name"
    } else {
        "title"
    }]
    .as_str()
    .context("Missing provider display name")?;
    let name = crate::library::enrichment::Correction {
        version: 1,
        name: name.into(),
        artist_ids: vec![],
        release_kind: if kind == EntityKind::Release {
            Some(crate::library::releases::ReleaseKind::Other)
        } else {
            None
        },
        mbid: Some(id.clone()),
    };
    let mut name = name;
    name.validate(kind).map_err(anyhow::Error::msg)?;
    let mut artists = Vec::new();
    if let Some(credits) = value["artist-credit"].as_array() {
        ensure!(credits.len() <= 100, "Too many provider credits");
        for credit in credits {
            let artist = &credit["artist"];
            let id = mbid(
                artist["id"]
                    .as_str()
                    .context("Missing credited artist ID")?,
            )
            .map_err(anyhow::Error::msg)?;
            let name = crate::library::ArtistName::parse(
                artist["name"].as_str().context("Missing artist name")?,
            )?;
            if !artists.iter().any(|a: &ArtistCredit| a.mbid == id) {
                artists.push(ArtistCredit {
                    mbid: id,
                    name: name.as_str().into(),
                });
            }
        }
    }
    let score = value["score"]
        .as_f64()
        .or_else(|| value["score"].as_str().and_then(|s| s.parse().ok()));
    let mut context = Vec::new();
    for key in ["disambiguation", "date", "country"] {
        if let Some(text) = value[key].as_str().filter(|s| !s.is_empty()) {
            ensure!(
                text.len() <= 1024 && !text.chars().any(char::is_control),
                "Invalid provider context"
            );
            context.push(text);
        }
    }
    let description = if context.is_empty() {
        None
    } else {
        Some(context.join("; "))
    };
    Ok(Proposal {
        source: "musicbrainz".into(),
        mbid: Some(id),
        name: Some(name.name),
        description,
        artists,
        score,
        artwork_digest: None,
        artwork_mime: None,
    })
}

/// Escape Lucene syntax so caller text remains a literal search phrase.
fn phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Search up to ten candidates or look up an exact UUID, with artist credits.
pub(crate) async fn search(
    http: &Transport,
    kind: EntityKind,
    query: &str,
    exact: Option<&str>,
) -> Result<Vec<Proposal>> {
    search_at(http, "https://musicbrainz.org/ws/2", kind, query, exact).await
}

/// Endpoint-parameterized adapter core permits isolated local contract tests.
async fn search_at(
    http: &Transport,
    base: &str,
    kind: EntityKind,
    query: &str,
    exact: Option<&str>,
) -> Result<Vec<Proposal>> {
    http.pace().await;
    let url = format!(
        "{base}/{}{}",
        kind.as_str(),
        exact.map_or(String::new(), |id| format!("/{id}"))
    );
    let mut request = http.client.get(url).query(&[("fmt", "json")]);
    if exact.is_some() && kind != EntityKind::Artist {
        request = request.query(&[("inc", "artist-credits")]);
    } else if exact.is_none() {
        request =
            request.query(&[("query", phrase(query)), ("limit", "10".into())]);
    }
    let bytes = Transport::bytes(request.send().await?, 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if exact.is_some() {
        return Ok(vec![proposal(kind, &value)?]);
    }
    let key = match kind {
        EntityKind::Artist => "artists",
        EntityKind::Release => "releases",
        EntityKind::Recording => "recordings",
    };
    let items = value[key]
        .as_array()
        .context("Missing provider result list")?;
    ensure!(items.len() <= 10, "Too many provider results");
    items.iter().map(|v| proposal(kind, v)).collect()
}

/// Download a front-cover thumbnail only through the documented CAA endpoint.
pub(crate) async fn cover(http: &Transport, id: &str) -> Result<Artwork> {
    let id = mbid(id).map_err(anyhow::Error::msg)?;
    http.pace().await;
    let response = http
        .client
        .get(format!(
            "https://coverartarchive.org/release/{id}/front-500"
        ))
        .send()
        .await?;
    let bytes = Transport::bytes(response, artwork::MAX_BYTES).await?;
    tokio::task::spawn_blocking(move || artwork::validate(bytes)).await?
}

#[cfg(test)]
mod tests {
    //! Upstream contracts are exercised against a local mock, never live APIs.
    use super::*;
    use axum::{Json, Router, extract::OriginalUri, routing::get};
    use serde_json::json;

    /// Credits preserve IDs/order and provider scores are not auto-acceptance.
    #[test]
    fn parses_entities_without_matching_artist_names() {
        let item = json!({"id":"12345678-1234-1234-1234-123456789abc",
            "title":" Track ","score":"99","artist-credit":[
                {"artist":{"id":"12345678-1234-1234-1234-123456789abd",
                    "name":"Same"}},
                {"artist":{"id":"12345678-1234-1234-1234-123456789abe",
                    "name":"Same"}}]});
        let parsed = proposal(EntityKind::Recording, &item).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Track"));
        assert_eq!(parsed.artists.len(), 2);
        assert_ne!(parsed.artists[0].mbid, parsed.artists[1].mbid);
        assert_eq!(parsed.score, Some(99.0));
        assert!(
            proposal(EntityKind::Recording, &json!({"id":"bad"})).is_err()
        );
        assert_eq!(phrase("a\" OR *"), "\"a\\\" OR *\"");
    }

    /// Query encoding and bounded JSON errors are checked over real HTTP.
    #[tokio::test]
    async fn uses_json_search_contract_and_rejects_bad_provider_bodies() {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/recording",
                get(|OriginalUri(uri): OriginalUri| async move {
                    assert!(uri.query().unwrap().contains("fmt=json"));
                    assert!(uri.query().unwrap().contains("limit=10"));
                    Json(json!({"recordings":[{
                    "id":"12345678-1234-1234-1234-123456789abc",
                    "title":"Track"}]}))
                }),
            )
            .route("/artist", get(|| async { "not json" }))
            .route("/release", get(|| async { "x".repeat(1024 * 1024 + 1) }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http =
            Transport::new("shprotify/test (test@example.org)").unwrap();
        let base = format!("http://{address}");
        let found =
            search_at(&http, &base, EntityKind::Recording, "Track", None)
                .await
                .unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            search_at(&http, &base, EntityKind::Artist, "a", None)
                .await
                .is_err()
        );
        assert!(
            search_at(&http, &base, EntityKind::Release, "a", None)
                .await
                .is_err()
        );
        server.abort();
    }
}
