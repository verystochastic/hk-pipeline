use anyhow::Result;
use rand::Rng;
pub use types::EmbeddingRecord;

mod trend;
pub use trend::rank_trending;

pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

fn centroid(embeddings: &[&Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return vec![];
    }
    let dim = embeddings[0].len();
    let mut center = vec![0f32; dim];
    for emb in embeddings {
        for (i, v) in emb.iter().enumerate() {
            center[i] += v;
        }
    }
    let n = embeddings.len() as f32;
    center.iter_mut().for_each(|v| *v /= n);
    center
}

pub struct Cluster {
    pub centroid: Vec<f32>,
    pub members: Vec<usize>,
}

pub fn kmeans(records: &[EmbeddingRecord], k: usize, max_iter: usize) -> Vec<Cluster> {
    if records.is_empty() || k == 0 {
        return vec![];
    }
    let k = k.min(records.len());
    let embeddings: Vec<&Vec<f32>> = records.iter().map(|r| &r.embedding).collect();
    let n = embeddings.len();

    let mut rng = rand::thread_rng();

    // k-means++: first centroid chosen at random, subsequent ones weighted by distance
    let first = rng.gen_range(0..n);
    let mut centroids: Vec<Vec<f32>> = vec![embeddings[first].to_vec()];
    for _ in 1..k {
        let distances: Vec<f32> = embeddings
            .iter()
            .map(|emb| {
                centroids
                    .iter()
                    .map(|c| 1.0 - cosine_similarity(emb, c))
                    .fold(f32::INFINITY, f32::min)
            })
            .collect();
        let next = distances
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        centroids.push(embeddings[next].to_vec());
    }

    let mut assignments = vec![0usize; n];

    for _ in 0..max_iter {
        let mut changed = false;
        for (i, emb) in embeddings.iter().enumerate() {
            let best = centroids
                .iter()
                .enumerate()
                .map(|(ci, c)| (ci, cosine_similarity(emb, c)))
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(ci, _)| ci)
                .unwrap_or(0);
            if assignments[i] != best {
                changed = true;
            }
            assignments[i] = best;
        }
        if !changed {
            break;
        }

        for ci in 0..k {
            let members: Vec<&Vec<f32>> = (0..n)
                .filter(|&i| assignments[i] == ci)
                .map(|i| embeddings[i])
                .collect();
            if !members.is_empty() {
                centroids[ci] = centroid(&members);
            }
        }
    }

    (0..k)
        .map(|ci| {
            let members: Vec<usize> = (0..n).filter(|&i| assignments[i] == ci).collect();
            Cluster {
                centroid: centroids[ci].clone(),
                members,
            }
        })
        .collect()
}

pub struct ClusterSummary {
    pub cluster_id: usize,
    pub size: usize,
    pub top_titles: Vec<String>,
    pub sources: Vec<String>,
    pub centroid: Vec<f32>,
}

pub fn summarise(records: &[EmbeddingRecord], clusters: &[Cluster]) -> Vec<ClusterSummary> {
    clusters
        .iter()
        .enumerate()
        .map(|(ci, cluster)| {
            let mut ranked: Vec<(usize, f32)> = cluster
                .members
                .iter()
                .map(|&i| (i, cosine_similarity(&records[i].embedding, &cluster.centroid)))
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            let top_titles = ranked
                .iter()
                .take(3)
                .map(|(i, _)| records[*i].title.clone())
                .collect();

            let mut sources: Vec<String> = cluster
                .members
                .iter()
                .map(|&i| records[i].source.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            sources.sort();

            ClusterSummary {
                cluster_id: ci,
                size: cluster.members.len(),
                top_titles,
                sources,
                centroid: cluster.centroid.clone(),
            }
        })
        .collect()
}

pub fn analyse(records: &[EmbeddingRecord], k: usize) -> Result<Vec<ClusterSummary>> {
    if records.len() < k {
        anyhow::bail!(
            "Not enough records ({}) to form {} clusters. Ingest more data first.",
            records.len(),
            k
        );
    }
    let clusters = kmeans(records, k, 100);
    Ok(summarise(records, &clusters))
}
