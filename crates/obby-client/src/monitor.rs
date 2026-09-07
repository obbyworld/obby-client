//! Watching whether people are online.
//!
//! MONITOR asks the server to tell us when a nick comes or goes, instead of us polling with ISON.
//! The list has a server-set limit, and asking past it is refused for the whole request rather than
//! partially, so the limit has to be respected on our side.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use obby_proto::CaseFolded;

/// Who we are watching, and whether each is online.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Monitor {
    watching: BTreeSet<CaseFolded>,
    online: BTreeSet<CaseFolded>,
}

impl Monitor {
    /// Watch nobody.
    pub fn new() -> Self {
        Self::default()
    }

    /// Note that we asked to watch these.
    pub fn watch(&mut self, folded: impl IntoIterator<Item = CaseFolded>) {
        self.watching.extend(folded);
    }

    /// Note that we asked to stop watching these.
    pub fn unwatch(&mut self, folded: &[CaseFolded]) {
        for nick in folded {
            self.watching.remove(nick);
            self.online.remove(nick);
        }
    }

    /// Forget everyone, which is what `MONITOR C` does.
    pub fn clear(&mut self) {
        self.watching.clear();
        self.online.clear();
    }

    /// Record that someone is online.
    pub fn mark_online(&mut self, folded: CaseFolded) {
        self.watching.insert(folded.clone());
        self.online.insert(folded);
    }

    /// Record that someone is offline.
    pub fn mark_offline(&mut self, folded: &CaseFolded) {
        self.online.remove(folded);
    }

    /// True when we are watching this nick.
    pub fn is_watching(&self, folded: &CaseFolded) -> bool {
        self.watching.contains(folded)
    }

    /// True when this nick is online, as far as the server has told us.
    pub fn is_online(&self, folded: &CaseFolded) -> bool {
        self.online.contains(folded)
    }

    /// Everyone we are watching, so a reconnect can ask for them again.
    pub fn watched(&self) -> impl Iterator<Item = &CaseFolded> {
        self.watching.iter()
    }

    /// How many we are watching.
    pub fn len(&self) -> usize {
        self.watching.len()
    }

    /// True when we are watching nobody.
    pub fn is_empty(&self) -> bool {
        self.watching.is_empty()
    }

    /// Nobody is online until the server says so, which is what a dropped link resets us to.
    pub fn forget_presence(&mut self) {
        self.online.clear();
    }
}

/// Split a list of targets into requests that each stay inside the server's limit.
///
/// A `MONITOR +` naming more than `limit` targets is refused whole, so a caller with a long list has
/// to send several requests rather than one and hope.
pub(crate) fn batched(targets: &[String], limit: usize) -> Vec<String> {
    let per_request = limit.max(1);
    targets
        .chunks(per_request)
        .map(|chunk| chunk.join(","))
        .collect()
}

/// Read the targets out of a comma-separated MONITOR parameter.
pub(crate) fn split_targets(list: &str) -> Vec<String> {
    list.split(',')
        .filter(|target| !target.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use obby_proto::Casemapping;

    fn fold(nick: &str) -> CaseFolded {
        Casemapping::Rfc1459.fold(nick)
    }

    #[test]
    fn watching_someone_says_nothing_about_whether_they_are_here() {
        let mut monitor = Monitor::new();
        monitor.watch([fold("alice")]);
        assert!(monitor.is_watching(&fold("alice")));
        assert!(
            !monitor.is_online(&fold("alice")),
            "we have not heard yet, and guessing online would show a false green dot"
        );
    }

    #[test]
    fn the_server_deciding_someone_is_online_also_means_we_watch_them() {
        let mut monitor = Monitor::new();
        monitor.mark_online(fold("alice"));
        assert!(monitor.is_watching(&fold("alice")));
        assert!(monitor.is_online(&fold("alice")));

        monitor.mark_offline(&fold("alice"));
        assert!(
            monitor.is_watching(&fold("alice")),
            "going offline is not the same as being dropped"
        );
        assert!(!monitor.is_online(&fold("alice")));
    }

    #[test]
    fn dropping_someone_forgets_both_facts() {
        let mut monitor = Monitor::new();
        monitor.mark_online(fold("alice"));
        monitor.unwatch(&[fold("alice")]);
        assert!(!monitor.is_watching(&fold("alice")));
        assert!(!monitor.is_online(&fold("alice")));
    }

    #[test]
    fn folding_means_one_person_however_they_are_spelled() {
        let mut monitor = Monitor::new();
        monitor.mark_online(fold("[alice]"));
        assert!(
            monitor.is_online(&fold("{ALICE}")),
            "rfc1459 folds braces onto brackets, so this is the same person"
        );
        assert_eq!(monitor.len(), 1);
    }

    #[test]
    fn a_dropped_link_forgets_who_was_here_but_not_who_we_watch() {
        let mut monitor = Monitor::new();
        monitor.mark_online(fold("alice"));
        monitor.forget_presence();
        assert!(monitor.is_watching(&fold("alice")));
        assert!(
            !monitor.is_online(&fold("alice")),
            "presence from the old link says nothing about the new one"
        );
    }

    #[test]
    fn a_long_list_is_split_to_stay_inside_the_servers_limit() {
        let targets: Vec<String> = (0..7).map(|i| alloc::format!("nick{i}")).collect();
        let requests = batched(&targets, 3);
        assert_eq!(
            requests,
            [
                "nick0,nick1,nick2".to_string(),
                "nick3,nick4,nick5".to_string(),
                "nick6".to_string()
            ],
            "one oversized request is refused whole, so it has to be several"
        );
    }

    #[test]
    fn a_limit_of_zero_still_makes_progress() {
        let targets = alloc::vec!["alice".to_string()];
        assert_eq!(batched(&targets, 0), ["alice".to_string()]);
    }

    #[test]
    fn reading_a_target_list_ignores_empty_entries() {
        assert_eq!(
            split_targets("alice,,bob,"),
            ["alice".to_string(), "bob".to_string()]
        );
        assert!(split_targets("").is_empty());
    }
}
