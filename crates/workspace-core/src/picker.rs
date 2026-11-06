use serde::Serialize;
use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
};

#[derive(Clone, Default)]
pub struct PickerRequests(Arc<AtomicU64>);
#[derive(Clone)]
pub struct Request {
    generation: u64,
    current: Arc<AtomicU64>,
}
impl PickerRequests {
    pub fn begin(&self) -> Request {
        Request {
            generation: self.0.fetch_add(1, AtomicOrdering::AcqRel) + 1,
            current: self.0.clone(),
        }
    }
}
impl Request {
    pub fn cancelled(&self) -> bool {
        self.current.load(AtomicOrdering::Acquire) != self.generation
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct Match {
    pub index: usize,
    pub score: i64,
    pub positions: Vec<usize>,
}
impl Ord for Match {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.index.cmp(&other.index))
    }
}
impl PartialOrd for Match {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn score(query: &[char], candidate: &str, index: usize) -> Option<Match> {
    let mut remaining = query.iter();
    let mut wanted = remaining.next();
    let mut positions = Vec::with_capacity(query.len());
    let mut points = 0;
    let mut previous_end = None;
    let mut boundary = true;
    for (offset, ch) in candidate.char_indices() {
        if wanted.is_none() {
            break;
        }
        // Query and candidate must use the same complete lowercase mapping.
        // One source scalar can match multiple lowered scalars (İ -> i + dot),
        // but contributes one highlight/score at its original UTF-8 boundary.
        let mut matched = false;
        for lowered in ch.to_lowercase() {
            if wanted == Some(&lowered) {
                matched = true;
                wanted = remaining.next();
            }
        }
        if matched {
            positions.push(offset);
            points += 10 + if boundary { 8 } else { 0 };
            if previous_end == Some(offset) {
                points += 12;
            }
            if offset == 0 {
                points += 15;
            }
            previous_end = Some(offset + ch.len_utf8());
        } else if previous_end.is_some() {
            points -= 1;
        }
        boundary = matches!(ch, '/' | '\\' | '_' | '-' | ' ' | '.');
    }
    wanted.is_none().then_some(Match {
        index,
        score: points,
        positions,
    })
}

/// CPU work belongs on a worker. Candidates are supplied, never discovered here.
pub fn rank(
    request: &Request,
    query: &str,
    candidates: &[String],
    limit: usize,
) -> Result<Vec<Match>, String> {
    if candidates.len() > 100_000 || query.chars().count() > 256 || limit > 200 {
        return Err("picker request exceeds bounded budget".into());
    }
    let query: Vec<_> = query.chars().flat_map(char::to_lowercase).collect();
    let mut best = BinaryHeap::<Match>::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if request.cancelled() {
            return Err("cancelled".into());
        }
        if limit == 0 {
            break;
        }
        if candidate.len() > 4096 {
            continue;
        }
        if let Some(item) = score(&query, candidate, index) {
            if best.len() < limit {
                best.push(item);
            } else if let Some(worst) = best.peek()
                && (item.score > worst.score
                    || (item.score == worst.score && item.index < worst.index))
            {
                best.pop();
                best.push(item);
            }
        }
    }
    if request.cancelled() {
        return Err("cancelled".into());
    }
    let mut result = best.into_vec();
    result.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lowercase_expansion_matches_itself_and_highlights_original_bytes() {
        let request = PickerRequests::default().begin();
        let items = vec!["İstanbul".into(), "/İ/file".into()];
        let found = rank(&request, "İ", &items, 10).unwrap();
        assert_eq!(
            found.len(),
            2,
            "a candidate must match its own lowercase expansion"
        );
        assert_eq!(found[0].index, 0);
        assert_eq!(found[0].positions, vec![0]);
        let found = rank(&request, "İf", &items, 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].index, 1);
        assert_eq!(found[0].positions, vec![1, 4]);
    }
    #[test]
    fn ascii_scores_and_unicode_case_mapping_remain_explicit() {
        let request = PickerRequests::default().begin();
        let items = vec!["abc".into(), "a_b_c".into(), "prefix_abc".into()];
        let found = rank(&request, "ABC", &items, 10).unwrap();
        assert_eq!(
            found.iter().map(|m| (m.index, m.score)).collect::<Vec<_>>(),
            vec![(0, 77), (1, 67), (2, 62)]
        );
        let items = vec!["ñÁ".into(), "İ".into(), "i\u{307}".into(), "ß".into()];
        let found = rank(&request, "Ñá", &items, 10).unwrap();
        assert_eq!(found[0].positions, vec![0, 2]);
        let found = rank(&request, "i\u{307}", &items, 10).unwrap();
        assert_eq!(found.len(), 2);
        for item in found {
            assert!(
                item.positions
                    .iter()
                    .all(|&offset| items[item.index].is_char_boundary(offset))
            );
            assert!(item.positions.windows(2).all(|p| p[0] < p[1]));
        }
        // Lowercase mapping is neither full Unicode case folding nor NFC.
        assert!(rank(&request, "ss", &items, 10).unwrap().is_empty());
        let accents = vec!["é".into()];
        assert!(rank(&request, "e\u{301}", &accents, 10).unwrap().is_empty());
    }
    #[test]
    fn hundred_thousand_unicode_candidates_keep_stable_top_200_and_budgets() {
        let requests = PickerRequests::default();
        let old = requests.begin();
        let request = requests.begin();
        let mut items: Vec<String> = (0..100_000)
            .map(|i| format!("projects/{i:05}/İstanbul.rs"))
            .collect();
        let found = rank(&request, "İrs", &items, 200).unwrap();
        assert_eq!(found.len(), 200);
        assert_eq!(
            found.iter().map(|m| m.index).collect::<Vec<_>>(),
            (0..200).collect::<Vec<_>>()
        );
        assert!(found.iter().all(|m| {
            m.positions.len() == 3
                && m.positions
                    .iter()
                    .all(|&p| items[m.index].is_char_boundary(p))
        }));
        assert_eq!(rank(&old, "İrs", &items, 200), Err("cancelled".into()));
        assert!(rank(&request, "a", &items, 201).is_err());
        assert!(rank(&request, &"a".repeat(257), &items, 200).is_err());
        items.push("extra".into());
        assert!(rank(&request, "", &items, 200).is_err());
        assert!(rank(&request, "", &[], 0).unwrap().is_empty());
        assert!(
            rank(&request, "a", &["a".repeat(4097)], 10)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn stale_queries_cannot_replace_newer_picker_results() {
        let requests = PickerRequests::default();
        let old = requests.begin();
        let newest = requests.begin();
        let items = vec!["workspace".into(), "sandbox".into()];
        assert_eq!(rank(&old, "w", &items, 10), Err("cancelled".into()));
        assert_eq!(rank(&newest, "w", &items, 10).unwrap()[0].index, 0);
    }
    #[test]
    fn stable_order_unicode_highlights_and_result_cap() {
        let request = PickerRequests::default().begin();
        let items = vec!["ñandú".into(), "ñandú".into(), "sandbox".into()];
        let found = rank(&request, "ÑÚ", &items, 1).unwrap();
        assert_eq!(found[0].index, 0);
        assert_eq!(found[0].positions, vec![0, 5]);
        assert!(rank(&request, "missing", &items, 10).unwrap().is_empty());
        assert_eq!(
            rank(&request, "", &items, 2)
                .unwrap()
                .iter()
                .map(|m| m.index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
