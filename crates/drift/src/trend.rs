use types::{ClusterSnapshot, TrendSignal};

use crate::{cosine_similarity, ClusterSummary};

/// Matches each current cluster against its nearest previous-run snapshot by
/// centroid cosine similarity (k-means cluster ids aren't stable across
/// independent runs, so identity is approximated this way) and scores the
/// resulting growth. Unmatched current clusters are brand-new trends
/// (previous_size = 0); unmatched previous clusters are decayed trends
/// (current_size = 0). Results are sorted by score, descending.
pub fn rank_trending(
    current: &[ClusterSummary],
    previous: &[ClusterSnapshot],
    as_of: &str,
    similarity_threshold: f32,
) -> Vec<TrendSignal> {
    let mut previous_used = vec![false; previous.len()];

    let mut signals: Vec<TrendSignal> = current
        .iter()
        .map(|cluster| {
            let best_match = previous
                .iter()
                .enumerate()
                .filter(|(i, _)| !previous_used[*i])
                .map(|(i, snap)| (i, cosine_similarity(&cluster.centroid, &snap.centroid)))
                .filter(|(_, sim)| *sim >= similarity_threshold)
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

            let previous_size = match best_match {
                Some((i, _)) => {
                    previous_used[i] = true;
                    previous[i].size
                }
                None => 0,
            };

            build_signal(cluster, previous_size, as_of)
        })
        .collect();

    for (i, snap) in previous.iter().enumerate() {
        if previous_used[i] {
            continue;
        }
        signals.push(TrendSignal {
            label: snap.label.clone(),
            current_size: 0,
            previous_size: snap.size,
            velocity: -1.0,
            score: 0.0,
            sources: snap.sources.clone(),
            as_of: as_of.to_string(),
        });
    }

    signals.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    signals
}

fn build_signal(cluster: &ClusterSummary, previous_size: usize, as_of: &str) -> TrendSignal {
    let current_size = cluster.size;
    let velocity = (current_size as f32 - previous_size as f32) / (previous_size.max(1) as f32);
    // Reward growth (velocity) but temper it by absolute size, so a single
    // brand-new two-member cluster doesn't outrank a large cluster that's
    // still steadily growing. Both terms are normalized to 0..1.
    let velocity_term = velocity.max(0.0) / (velocity.max(0.0) + 1.0);
    let size_term = (current_size as f32).sqrt() / ((current_size as f32).sqrt() + 1.0);
    let score = velocity_term * size_term;

    TrendSignal {
        label: cluster.top_titles.first().cloned().unwrap_or_default(),
        current_size,
        previous_size,
        velocity,
        score,
        sources: cluster.sources.clone(),
        as_of: as_of.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster(label: &str, size: usize, centroid: Vec<f32>) -> ClusterSummary {
        ClusterSummary {
            cluster_id: 0,
            size,
            top_titles: vec![label.to_string()],
            sources: vec!["test".to_string()],
            centroid,
        }
    }

    fn snapshot(label: &str, size: usize, centroid: Vec<f32>) -> ClusterSnapshot {
        ClusterSnapshot {
            run_id: "prev".to_string(),
            label: label.to_string(),
            size,
            centroid,
            sources: vec!["test".to_string()],
            captured_at: "t0".to_string(),
        }
    }

    #[test]
    fn matches_growing_cluster_and_scores_it_positively() {
        let current = vec![cluster("ai agents", 20, vec![1.0, 0.0])];
        let previous = vec![snapshot("ai agents", 5, vec![1.0, 0.0])];

        let signals = rank_trending(&current, &previous, "t1", 0.8);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].previous_size, 5);
        assert_eq!(signals[0].current_size, 20);
        assert!(signals[0].velocity > 0.0);
        assert!(signals[0].score > 0.0);
    }

    #[test]
    fn brand_new_cluster_has_zero_previous_size() {
        let current = vec![cluster("new topic", 10, vec![0.0, 1.0])];
        let previous = vec![snapshot("unrelated", 5, vec![1.0, 0.0])];

        let signals = rank_trending(&current, &previous, "t1", 0.8);

        let new_trend = signals.iter().find(|s| s.label == "new topic").unwrap();
        assert_eq!(new_trend.previous_size, 0);
    }

    #[test]
    fn decayed_cluster_reported_with_zero_current_size() {
        let current: Vec<ClusterSummary> = vec![];
        let previous = vec![snapshot("fading topic", 8, vec![1.0, 0.0])];

        let signals = rank_trending(&current, &previous, "t1", 0.8);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].current_size, 0);
        assert_eq!(signals[0].previous_size, 8);
    }

    #[test]
    fn results_sorted_by_score_descending() {
        let current = vec![
            cluster("slow grower", 6, vec![1.0, 0.0]),
            cluster("fast grower", 50, vec![0.0, 1.0]),
        ];
        let previous = vec![
            snapshot("slow grower", 5, vec![1.0, 0.0]),
            snapshot("fast grower", 5, vec![0.0, 1.0]),
        ];

        let signals = rank_trending(&current, &previous, "t1", 0.8);

        assert_eq!(signals[0].label, "fast grower");
    }
}
