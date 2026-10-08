//! Last-good copies of Reddit responses, served when Reddit blocks or
//! rate-limits us.
//!
//! Reddit blocks this instance's IP every so often (minutes to hours). Without
//! this, every page errors until the block lifts. With it, pages read recently
//! still render from the last good response, with a notice saying how old the
//! copy is. Only transient failures fall back (see [`is_transient`]); real
//! answers like "banned" or "private" are passed through.

use hyper::body::Bytes;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Total body bytes kept. The pod has 1 GiB and normally uses ~350 MB.
const BUDGET_BYTES: usize = 96 * 1024 * 1024;
/// Bodies above this (huge threads) aren't kept, so one page can't evict everything.
const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
/// Copies older than this aren't served.
const MAX_AGE_SECS: u64 = 12 * 3600;

struct Entry {
	stored_at: u64,
	body: Bytes,
}

/// Insertion-ordered store with a byte budget: re-storing a key moves it to the
/// back, and the oldest entries go first when over budget.
#[derive(Default)]
struct Store {
	entries: HashMap<String, Entry>,
	order: VecDeque<String>,
	bytes: usize,
}

impl Store {
	fn insert(&mut self, key: String, body: Bytes, now: u64) {
		if body.len() > MAX_ENTRY_BYTES {
			return;
		}
		if let Some(old) = self.entries.remove(&key) {
			self.bytes -= old.body.len();
			self.order.retain(|k| k != &key);
		}
		self.bytes += body.len();
		self.order.push_back(key.clone());
		self.entries.insert(key, Entry { stored_at: now, body });
		while self.bytes > BUDGET_BYTES {
			let Some(oldest) = self.order.pop_front() else { break };
			if let Some(e) = self.entries.remove(&oldest) {
				self.bytes -= e.body.len();
			}
		}
	}

	fn get(&self, key: &str, now: u64) -> Option<(u64, Bytes)> {
		let e = self.entries.get(key)?;
		let age = now.saturating_sub(e.stored_at);
		(age <= MAX_AGE_SECS).then(|| (age, e.body.clone()))
	}
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store::default()));

tokio::task_local! {
	/// Age in seconds of the oldest copy served while handling the current request.
	static SERVED_AGE: Cell<Option<u64>>;
}

fn unix_now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Keep `body` as the last good response for `key`. `Bytes` clones are cheap.
pub fn remember(key: String, body: Bytes) {
	if let Ok(mut store) = STORE.lock() {
		store.insert(key, body, unix_now());
	}
}

/// The last good response for `key`, with its age in seconds, if fresh enough.
pub fn recall(key: &str) -> Option<(u64, Bytes)> {
	STORE.lock().ok()?.get(key, unix_now())
}

/// Record that a copy of this age was served for the current request.
pub fn mark_served(age: u64) {
	let _ = SERVED_AGE.try_with(|c| c.set(Some(c.get().map_or(age, |a| a.max(age)))));
}

/// Run a request handler so that [`notice`] can tell whether it served a copy.
/// Returns the handler's output and the age of the oldest copy served, if any.
pub async fn scope<F: Future>(fut: F) -> (F::Output, Option<u64>) {
	SERVED_AGE
		.scope(Cell::new(None), async {
			let out = fut.await;
			(out, SERVED_AGE.with(Cell::get))
		})
		.await
}

/// For templates: "12 minutes ago" when this page was built from a copy, else "".
pub fn notice() -> String {
	SERVED_AGE.try_with(Cell::get).ok().flatten().map(describe_age).unwrap_or_default()
}

fn describe_age(secs: u64) -> String {
	match secs {
		0..=89 => "a minute ago".to_string(),
		90..=3599 => format!("{} minutes ago", (secs + 30) / 60),
		3600..=5399 => "an hour ago".to_string(),
		_ => format!("{} hours ago", (secs + 1800) / 3600),
	}
}

/// Errors from `client::json` that mean "Reddit didn't answer properly right
/// now", as opposed to a real answer about the content.
pub fn is_transient(err: &str) -> bool {
	const TRANSIENT: [&str; 6] = [
		"Reddit rate limit exceeded",
		"Couldn't send request to Reddit",
		"Failed to parse page JSON data",
		"Failed receiving body from Reddit",
		"Reddit is having issues",
		"OAuth token has expired",
	];
	TRANSIENT.iter().any(|t| err.starts_with(t))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn evicts_oldest_over_budget_and_skips_huge_bodies() {
		let mut s = Store::default();
		let chunk = Bytes::from(vec![0u8; MAX_ENTRY_BYTES]);
		for i in 0..13 {
			s.insert(format!("k{i}"), chunk.clone(), 0);
		}
		assert!(s.bytes <= BUDGET_BYTES);
		assert!(s.get("k0", 0).is_none(), "oldest entry should be evicted");
		assert!(s.get("k12", 0).is_some());

		s.insert("huge".into(), Bytes::from(vec![0u8; MAX_ENTRY_BYTES + 1]), 0);
		assert!(s.get("huge", 0).is_none());
	}

	#[test]
	fn reinserting_refreshes_position_and_age() {
		let mut s = Store::default();
		s.insert("a".into(), Bytes::from_static(b"1"), 0);
		s.insert("a".into(), Bytes::from_static(b"22"), 100);
		assert_eq!(s.bytes, 2);
		assert_eq!(s.order.len(), 1);
		assert_eq!(s.get("a", 160), Some((60, Bytes::from_static(b"22"))));
	}

	#[test]
	fn old_copies_are_not_served() {
		let mut s = Store::default();
		s.insert("a".into(), Bytes::from_static(b"x"), 0);
		assert!(s.get("a", MAX_AGE_SECS).is_some());
		assert!(s.get("a", MAX_AGE_SECS + 1).is_none());
	}

	#[test]
	fn only_transient_errors_fall_back() {
		assert!(is_transient("Reddit rate limit exceeded. Try refreshing in a few seconds.Rate limit will reset in: 233"));
		assert!(is_transient("Couldn't send request to Reddit: timeout | /r/rust"));
		assert!(is_transient("Failed to parse page JSON data: expected value | /r/rust"));
		assert!(!is_transient("banned"));
		assert!(!is_transient("private"));
		assert!(!is_transient("Reddit error 404 \"null\": \"Not Found\" | /r/x"));
	}

	#[test]
	fn describes_ages() {
		assert_eq!(describe_age(30), "a minute ago");
		assert_eq!(describe_age(600), "10 minutes ago");
		assert_eq!(describe_age(3700), "an hour ago");
		assert_eq!(describe_age(3 * 3600), "3 hours ago");
	}

	#[tokio::test]
	async fn scope_reports_the_oldest_copy_served() {
		let ((), age) = scope(async {
			assert_eq!(notice(), "");
			mark_served(120);
			mark_served(60);
			assert_eq!(notice(), "2 minutes ago");
		})
		.await;
		assert_eq!(age, Some(120));
		// Outside a scope nothing is recorded and nothing panics.
		mark_served(5);
		assert_eq!(notice(), "");
	}
}
