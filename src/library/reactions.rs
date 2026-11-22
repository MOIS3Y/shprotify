//! Personal recording assessments; likes define the favorites playlist.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Explicit preference; absence of a database row means no assessment.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    sqlx::Type,
    ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub(crate) enum Reaction {
    /// Include the recording in personal favorites.
    Like,
    /// Negative preference, excluding the recording from favorites.
    Dislike,
}

/// Personal assessment, including a neutral response for existing recordings.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ReactionState {
    /// Shared recording, independent of encoding and release position.
    pub(crate) recording_id: i64,
    /// Null means no current assessment.
    pub(crate) reaction: Option<Reaction>,
    /// UTC time of the last change, or null without an assessment.
    pub(crate) changed_at: Option<String>,
}

/// One favorite, derived from a current like without a copied playlist row.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Favorite {
    /// Shared recording; complete details use the existing recording endpoint.
    pub(crate) recording_id: i64,
    /// Current catalog title, including manual corrections.
    pub(crate) title: String,
    /// UTC time of the most recent transition to like.
    pub(crate) added_at: String,
}
