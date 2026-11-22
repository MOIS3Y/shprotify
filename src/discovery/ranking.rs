//! Deterministic, explainable recommendation scoring without an ML service.

use super::{ArtistAffinity, Credit, Profile, Recommendation};
use crate::library::reactions::Reaction;
use std::collections::HashMap;

/// Match known UUIDs first; display names are only a fallback for missing IDs.
pub(crate) fn affinity<'a>(
    profile: &'a Profile,
    credits: &[Credit],
) -> Option<&'a ArtistAffinity> {
    credits
        .iter()
        .filter_map(|credit| {
            profile.artists.iter().find(|artist| {
                match artist.mbid.as_ref().zip(credit.mbid.as_ref()) {
                    Some((a, b)) => a == b,
                    None => {
                        artist.name.to_lowercase()
                            == credit.name.to_lowercase()
                    }
                }
            })
        })
        .max_by(|a, b| a.weight.total_cmp(&b.weight))
}

/// Apply live personal exclusions and affinity, then diversify artist runs.
/// Charts can preserve their source order while still honoring user filters.
pub(crate) fn rank(
    items: Vec<Recommendation>,
    profile: &Profile,
    personalized: bool,
    include_hidden: bool,
    missing_only: bool,
) -> Vec<Recommendation> {
    let mut items: Vec<_> = items
        .into_iter()
        .filter(|item| {
            (include_hidden
                || (!item.feedback.hidden
                    && item.reaction != Some(Reaction::Dislike)))
                && (!missing_only || !item.has_registered_audio)
        })
        .collect();
    if !personalized {
        return items;
    }
    for item in &mut items {
        let affinity = affinity(profile, &item.candidate.artists);
        if let Some(artist) = affinity {
            item.score += (artist.weight / 8.0).min(4.0);
            item.reasons
                .push(format!("Matches your interest in {}", artist.name));
        }
        if item.reaction == Some(Reaction::Like) {
            item.score += 0.5;
        }
    }
    items.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    diversify(items, |item| {
        item.candidate.artists.first().map_or_else(
            || format!("unknown:{}", item.id),
            |a| a.name.to_lowercase(),
        )
    })
}

/// Move fourth/subsequent artist occurrences after diverse choices.
/// Apply before candidate-pool capping and again after live personal scoring.
pub(crate) fn diversify<T>(
    items: Vec<T>,
    key: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut counts = HashMap::<String, usize>::new();
    let mut first = Vec::new();
    let mut repeated = Vec::new();
    for item in items {
        let count = counts.entry(key(&item)).or_default();
        *count += 1;
        if *count <= 3 {
            first.push(item)
        } else {
            repeated.push(item)
        }
    }
    first.extend(repeated);
    first
}

#[cfg(test)]
mod tests {
    //! Affinity, deterministic diversity and explicit source-order behavior.
    use super::*;
    use crate::discovery::{
        ArtistAffinity, CandidateData, CandidateKind, Credit, Feedback,
        Preferences,
    };

    /// Synthetic candidate with no database/filesystem dependency.
    fn item(id: i64, artist: &str) -> Recommendation {
        Recommendation {
            id,
            candidate: CandidateData {
                kind: CandidateKind::Recording,
                title: format!("Track {id}"),
                artists: vec![Credit {
                    name: artist.into(),
                    mbid: None,
                }],
                mbid: None,
                provider: "lastfm".into(),
                external_id: id.to_string(),
                url: None,
                release_date: None,
            },
            local_id: None,
            match_method: None,
            has_registered_audio: false,
            reaction: None,
            feedback: Feedback::default(),
            score: 1.0,
            reasons: vec![],
        }
    }

    /// Explicit/derived profile snapshot used by the pure ranker.
    fn profile() -> Profile {
        Profile {
            preferences: Preferences {
                version: 0,
                artist_ids: vec![],
                tags: vec![],
            },
            artists: vec![ArtistAffinity {
                name: "Кино".into(),
                mbid: None,
                weight: 32.0,
            }],
            tracks: vec![],
        }
    }

    /// Three strong matches precede another artist before a fourth repetition.
    #[test]
    fn explains_affinity_and_diversifies_without_dropping_repeated_artists() {
        let items = vec![
            item(4, "Кино"),
            item(2, "Кино"),
            item(1, "кино"),
            item(3, "Кино"),
            item(5, "Other"),
        ];
        let ranked = rank(items, &profile(), true, false, false);
        assert_eq!(
            ranked.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 5, 4]
        );
        assert!(ranked[0].reasons[0].contains("Кино"));
        assert!(ranked[0].score > ranked[3].score);
    }

    /// Filters precede pagination and source collections retain original
    /// order.
    #[test]
    fn filters_decisions_and_registered_audio_without_reordering_charts() {
        let mut hidden = item(1, "One");
        hidden.feedback.hidden = true;
        let mut disliked = item(2, "Two");
        disliked.reaction = Some(Reaction::Dislike);
        let mut local = item(3, "Three");
        local.has_registered_audio = true;
        let items =
            vec![hidden, disliked, local, item(5, "Other"), item(4, "Кино")];
        let ranked = rank(items, &profile(), false, false, true);
        assert_eq!(
            ranked.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![5, 4]
        );
    }
}
