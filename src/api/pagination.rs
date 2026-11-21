//! Shared bounded pagination for catalog collection endpoints.

use serde::Deserialize;
use utoipa::IntoParams;

use super::error::ApiError;

/// Page selection in ascending resource identifier order.
#[derive(Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(super) struct Pagination {
    /// Maximum items to return; between 1 and 100, default 50.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub(super) limit: i64,
    /// Number of resources to skip; nonnegative, default zero.
    #[param(minimum = 0, default = 0)]
    pub(super) offset: i64,
}

impl Default for Pagination {
    /// Select the first 50 resources when parameters are omitted.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
        }
    }
}

impl Pagination {
    /// Reject unbounded page sizes and negative offsets before querying SQL.
    pub(super) fn validate(&self) -> Result<(), ApiError> {
        if !(1..=100).contains(&self.limit) || self.offset < 0 {
            return Err(ApiError::BadRequest(
                "limit must be 1–100 and offset must be nonnegative",
            ));
        }
        Ok(())
    }
}
