//! Subsequence matcher for the file viewer search.

// Weights: a letter that starts a word or path segment, continues the
// previous match, or lands in the file name outweighs the gap it skips.
const MATCH: i64 = 16;
const BOUNDARY: i64 = 8;
const SEGMENT_START: i64 = 4;
const CONSECUTIVE: i64 = 6;
const IN_FILE_NAME: i64 = 4;
const GAP_START: i64 = 3;
const MAX_GAP_PENALTY: i64 = 16;

/// A lowercased query, reused across every candidate of one search.
pub(in crate::client::shell) struct FuzzyQuery {
    chars: Vec<char>,
}

impl FuzzyQuery {
    pub(in crate::client::shell) fn new(query: &str) -> Self {
        Self {
            chars: query.chars().flat_map(char::to_lowercase).collect(),
        }
    }

    /// Higher is better; `None` when `candidate` does not contain the query
    /// as a case-insensitive subsequence.
    pub(in crate::client::shell) fn score(&self, candidate: &str) -> Option<i64> {
        if self.chars.is_empty() {
            return Some(0);
        }
        // Most candidates fail here, before anything is allocated.
        let mut pending = self.chars.iter().peekable();
        for ch in candidate.chars() {
            if pending.peek().is_some_and(|query| fold(ch) == **query) {
                pending.next();
            }
        }
        if pending.peek().is_some() {
            return None;
        }
        let chars: Vec<char> = candidate.chars().collect();
        let positions = self.tail_biased_positions(&chars)?;
        Some(score_positions(&chars, &positions))
    }

    /// Anchors the match at its rightmost possible start, then packs it
    /// forward from there. Why not the leftmost match: the file name sits at
    /// the end of a path, and a match there is what the user is looking for.
    fn tail_biased_positions(&self, chars: &[char]) -> Option<Vec<usize>> {
        let mut end = chars.len();
        let mut start = 0;
        for query in self.chars.iter().rev() {
            end = chars[..end].iter().rposition(|ch| fold(*ch) == *query)?;
            start = end;
        }
        let mut positions = Vec::with_capacity(self.chars.len());
        let mut next = start;
        for query in &self.chars {
            let offset = chars[next..].iter().position(|ch| fold(*ch) == *query)?;
            positions.push(next + offset);
            next += offset + 1;
        }
        Some(positions)
    }
}

fn fold(ch: char) -> char {
    if ch.is_ascii() {
        ch.to_ascii_lowercase()
    } else {
        // Why not the full lowercase mapping: a few letters expand to several
        // characters, which would shift every later position.
        ch.to_lowercase().next().unwrap_or(ch)
    }
}

fn score_positions(chars: &[char], positions: &[usize]) -> i64 {
    let file_name_start = chars
        .iter()
        .rposition(|ch| *ch == '/')
        .map_or(0, |slash| slash + 1);
    let mut score = 0;
    let mut previous: Option<usize> = None;
    for &position in positions {
        score += MATCH;
        let before = position.checked_sub(1).map(|index| chars[index]);
        let at = chars[position];
        let segment_start = before.is_none_or(|before| before == '/');
        let word_start = segment_start
            || before.is_some_and(|before| {
                !before.is_alphanumeric() || (before.is_lowercase() && at.is_uppercase())
            });
        if word_start {
            score += BOUNDARY;
        }
        if segment_start {
            score += SEGMENT_START;
        }
        if position >= file_name_start {
            score += IN_FILE_NAME;
        }
        match previous {
            Some(previous) if position == previous + 1 => score += CONSECUTIVE,
            Some(previous) => {
                let gap = (position - previous - 1) as i64;
                score -= GAP_START + gap.min(MAX_GAP_PENALTY);
            }
            None => {}
        }
        previous = Some(position);
    }
    score
}

/// Indices of the matching candidates, best first; equal scores put the
/// shorter path first.
pub(in crate::client::shell) fn rank<'a>(
    query: &FuzzyQuery,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<usize> {
    let mut matches: Vec<(i64, usize, &str, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            query
                .score(candidate)
                .map(|score| (score, candidate.len(), candidate, index))
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(right.2))
    });
    matches.into_iter().map(|(_, _, _, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, candidate: &str) -> Option<i64> {
        FuzzyQuery::new(query).score(candidate)
    }

    fn ranked<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        rank(&FuzzyQuery::new(query), candidates.iter().copied())
            .into_iter()
            .map(|index| candidates[index])
            .collect()
    }

    #[test]
    fn a_candidate_matches_only_when_it_contains_the_query_as_a_subsequence() {
        assert!(score("smr", "src/main.rs").is_some());
        assert!(score("nsm", "src/main.rs").is_none());
        assert!(score("mainx", "src/main.rs").is_none());
    }

    #[test]
    fn an_empty_query_matches_everything_equally() {
        assert_eq!(score("", "src/main.rs"), Some(0));
        assert_eq!(score("", ""), Some(0));
    }

    #[test]
    fn matching_ignores_case() {
        assert!(score("MAIN", "src/main.rs").is_some());
        assert!(score("readme", "README.md").is_some());
    }

    #[test]
    fn contiguous_runs_score_above_scattered_letters() {
        assert!(score("abc", "abc.rs") > score("abc", "axbxc.rs"));
    }

    #[test]
    fn word_boundaries_score_above_letters_inside_words() {
        assert!(score("fb", "foo_bar.rs") > score("fb", "fob.rs"));
        assert!(score("fv", "FileViewer.rs") > score("fv", "fiveish.rs"));
    }

    #[test]
    fn matches_in_the_file_name_score_above_matches_in_parent_directories() {
        assert!(score("lib", "src/lib.rs") > score("lib", "lib/src/main.rs"));
        assert_eq!(
            ranked(
                "main",
                &["src/domain/info.rs", "docs/main/README.md", "src/main.rs"]
            ),
            ["src/main.rs", "docs/main/README.md", "src/domain/info.rs"]
        );
    }

    #[test]
    fn ranking_drops_non_matches_and_breaks_ties_by_shorter_path() {
        assert_eq!(
            ranked("rs", &["b/x.rs", "a.rs", "Cargo.toml"]),
            ["a.rs", "b/x.rs"]
        );
    }
}
