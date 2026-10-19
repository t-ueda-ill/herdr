//! Favorites and most-recently-opened order for the open pickers. Pure data:
//! the shell decides when to record and persist.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use super::preferences::{ClientOpenPickerPreferences, ClientOpenPickerRecent};

/// Opens remembered per endpoint. Why not unbounded like favorites: history
/// grows with every host or container ever opened, favorites only by hand.
pub(super) const RECENT_LIMIT: usize = 50;

/// Sort key of one picker row; smaller sorts first.
pub(super) type OpenPickerRank = (u8, Reverse<Option<(u64, u32)>>);

impl ClientOpenPickerPreferences {
    pub(super) fn is_empty(&self) -> bool {
        self.favorites.is_empty() && self.recent.is_empty()
    }

    pub(super) fn toggle_favorite(&mut self, key: &str) {
        let before = self.favorites.len();
        self.favorites.retain(|favorite| favorite != key);
        if self.favorites.len() == before {
            self.favorites.push(key.to_owned());
        }
    }

    pub(super) fn record_open(&mut self, key: &str, now_ms: u64) {
        // Why not the raw clock: two opens in one millisecond, or a clock
        // stepped back, would otherwise list against the order they happened.
        let last_used_ms = self
            .recent
            .iter()
            .map(|recent| recent.last_used_ms.saturating_add(1))
            .max()
            .map_or(now_ms, |after_latest| now_ms.max(after_latest));
        match self.recent.iter_mut().find(|recent| recent.key == key) {
            Some(recent) => {
                recent.last_used_ms = last_used_ms;
                recent.count = recent.count.saturating_add(1);
            }
            None => self.recent.push(ClientOpenPickerRecent {
                key: key.to_owned(),
                last_used_ms,
                count: 1,
            }),
        }
        self.recent
            .sort_by_key(|recent| Reverse((recent.last_used_ms, recent.count)));
        self.recent.truncate(RECENT_LIMIT);
    }

    /// Lookup tables built once per reorder, not per row comparison.
    pub(super) fn ranking(&self) -> OpenPickerRanking<'_> {
        OpenPickerRanking {
            favorites: self.favorites.iter().map(String::as_str).collect(),
            recent: self
                .recent
                .iter()
                .map(|recent| (recent.key.as_str(), (recent.last_used_ms, recent.count)))
                .collect(),
        }
    }
}

pub(super) struct OpenPickerRanking<'a> {
    favorites: HashSet<&'a str>,
    recent: HashMap<&'a str, (u64, u32)>,
}

impl OpenPickerRanking<'_> {
    pub(super) fn is_favorite(&self, key: &str) -> bool {
        self.favorites.contains(key)
    }

    /// Favorites, then the endpoint's default container, then opened rows,
    /// then the rest; within each tier the latest open first, a tie going to
    /// the more often opened. Rows never opened tie, so the caller's
    /// listing order decides among them.
    pub(super) fn rank(&self, key: &str, is_default: bool) -> OpenPickerRank {
        let recent = self.recent.get(key).copied();
        let tier = if self.is_favorite(key) {
            0
        } else if is_default {
            1
        } else if recent.is_some() {
            2
        } else {
            3
        };
        (tier, Reverse(recent))
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recent(key: &str, last_used_ms: u64, count: u32) -> ClientOpenPickerRecent {
        ClientOpenPickerRecent {
            key: key.into(),
            last_used_ms,
            count,
        }
    }

    fn keys_in_rank_order(
        memory: &ClientOpenPickerPreferences,
        rows: &[(&str, bool)],
    ) -> Vec<String> {
        let ranking = memory.ranking();
        let mut rows = rows.to_vec();
        rows.sort_by_key(|(key, is_default)| ranking.rank(key, *is_default));
        rows.into_iter().map(|(key, _)| key.to_owned()).collect()
    }

    #[test]
    fn rows_rank_favorites_then_default_then_recent_then_unused_in_listing_order() {
        let memory = ClientOpenPickerPreferences {
            favorites: vec!["ssh:fav".into()],
            recent: vec![recent("ssh:old", 100, 1), recent("ssh:new", 200, 1)],
        };

        let order = keys_in_rank_order(
            &memory,
            &[
                ("ssh:unused-a", false),
                ("ssh:old", false),
                ("ssh:default", true),
                ("ssh:unused-b", false),
                ("ssh:new", false),
                ("ssh:fav", false),
            ],
        );

        assert_eq!(
            order,
            [
                "ssh:fav",
                "ssh:default",
                "ssh:new",
                "ssh:old",
                "ssh:unused-a",
                "ssh:unused-b"
            ]
        );
    }

    #[test]
    fn rows_opened_at_the_same_time_rank_the_more_often_opened_first() {
        let memory = ClientOpenPickerPreferences {
            favorites: Vec::new(),
            recent: vec![recent("ssh:once", 100, 1), recent("ssh:often", 100, 7)],
        };

        let order = keys_in_rank_order(&memory, &[("ssh:once", false), ("ssh:often", false)]);

        assert_eq!(order, ["ssh:often", "ssh:once"]);
    }

    #[test]
    fn favorites_rank_among_themselves_by_recency() {
        let memory = ClientOpenPickerPreferences {
            favorites: vec!["ssh:a".into(), "ssh:b".into()],
            recent: vec![recent("ssh:b", 100, 1)],
        };

        let order = keys_in_rank_order(&memory, &[("ssh:a", false), ("ssh:b", false)]);

        assert_eq!(order, ["ssh:b", "ssh:a"]);
    }

    #[test]
    fn toggling_a_favorite_adds_it_then_removes_it() {
        let mut memory = ClientOpenPickerPreferences::default();

        memory.toggle_favorite("ssh:prod");
        assert_eq!(memory.favorites, ["ssh:prod"]);
        assert!(memory.ranking().is_favorite("ssh:prod"));

        memory.toggle_favorite("ssh:prod");
        assert!(memory.favorites.is_empty());
        assert!(memory.is_empty());
    }

    #[test]
    fn recording_an_open_stamps_the_time_and_counts_repeat_opens() {
        let mut memory = ClientOpenPickerPreferences::default();

        memory.record_open("ssh:prod", 1_000);
        memory.record_open("ssh:lab", 2_000);
        memory.record_open("ssh:prod", 3_000);

        assert_eq!(
            memory.recent,
            [recent("ssh:prod", 3_000, 2), recent("ssh:lab", 2_000, 1)]
        );
    }

    #[test]
    fn opens_in_one_millisecond_or_after_a_clock_step_back_keep_their_order() {
        let mut memory = ClientOpenPickerPreferences::default();

        memory.record_open("ssh:first", 5_000);
        memory.record_open("ssh:second", 5_000);
        memory.record_open("ssh:third", 4_000);

        assert_eq!(
            keys_in_rank_order(
                &memory,
                &[
                    ("ssh:first", false),
                    ("ssh:second", false),
                    ("ssh:third", false)
                ]
            ),
            ["ssh:third", "ssh:second", "ssh:first"]
        );
    }

    #[test]
    fn history_keeps_only_the_fifty_most_recent_opens() {
        let mut memory = ClientOpenPickerPreferences::default();

        for index in 0..=RECENT_LIMIT as u64 {
            memory.record_open(&format!("ssh:host-{index}"), 1_000 + index);
        }

        assert_eq!(memory.recent.len(), RECENT_LIMIT);
        assert!(!memory.recent.iter().any(|entry| entry.key == "ssh:host-0"));
        assert!(memory
            .recent
            .iter()
            .any(|entry| entry.key == format!("ssh:host-{RECENT_LIMIT}")));
    }

    #[test]
    fn favorites_are_not_capped_by_the_history_limit() {
        let mut memory = ClientOpenPickerPreferences::default();

        for index in 0..=RECENT_LIMIT {
            memory.toggle_favorite(&format!("ssh:host-{index}"));
        }

        assert_eq!(memory.favorites.len(), RECENT_LIMIT + 1);
    }
}
