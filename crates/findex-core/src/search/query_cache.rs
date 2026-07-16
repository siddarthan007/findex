//! Process-local, revision-aware result cache for repeated and paraphrased queries.

use crate::storage::Symbol;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Entry {
    inserted: Instant,
    results: Vec<(Symbol, f32)>,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    values: HashMap<String, Entry>,
    order: VecDeque<String>,
    total_bytes: usize,
}

const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

fn global() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

pub fn get(key: &str, ttl: Duration) -> Option<Vec<(Symbol, f32)>> {
    let mut cache = global().lock().ok()?;
    let entry = cache.values.get(key)?.clone();
    if entry.inserted.elapsed() > ttl {
        cache.values.remove(key);
        cache.total_bytes = cache.total_bytes.saturating_sub(entry.bytes);
        cache.order.retain(|candidate| candidate != key);
        return None;
    }
    cache.order.retain(|candidate| candidate != key);
    cache.order.push_back(key.to_string());
    Some(entry.results)
}

pub fn insert(key: String, results: &[(Symbol, f32)], capacity: usize) {
    let Ok(mut cache) = global().lock() else {
        return;
    };
    let bytes = estimate_bytes(&key, results);
    if bytes > MAX_ENTRY_BYTES {
        return;
    }
    cache.order.retain(|candidate| candidate != &key);
    if let Some(previous) = cache.values.remove(&key) {
        cache.total_bytes = cache.total_bytes.saturating_sub(previous.bytes);
    }
    cache.order.push_back(key.clone());
    cache.values.insert(
        key,
        Entry {
            inserted: Instant::now(),
            results: results.to_vec(),
            bytes,
        },
    );
    cache.total_bytes = cache.total_bytes.saturating_add(bytes);
    while cache.values.len() > capacity.clamp(1, 2_048) || cache.total_bytes > MAX_CACHE_BYTES {
        if let Some(oldest) = cache.order.pop_front() {
            if let Some(entry) = cache.values.remove(&oldest) {
                cache.total_bytes = cache.total_bytes.saturating_sub(entry.bytes);
            }
        } else {
            break;
        }
    }
}

fn estimate_bytes(key: &str, results: &[(Symbol, f32)]) -> usize {
    key.len()
        + results
            .iter()
            .map(|(symbol, _)| {
                std::mem::size_of::<(Symbol, f32)>()
                    + symbol.id.len()
                    + symbol.name.len()
                    + symbol.qualified_name.as_ref().map_or(0, String::len)
                    + symbol.file_path.len()
                    + symbol.kind.len()
                    + symbol.signature.len()
                    + symbol.language.len()
                    + symbol.docstring.as_ref().map_or(0, String::len)
                    + symbol.parent_id.as_ref().map_or(0, String::len)
            })
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_lru_evicts_oldest_entry() {
        insert("first".into(), &[], 1);
        insert("second".into(), &[], 1);
        assert!(get("first", Duration::from_secs(60)).is_none());
        assert!(get("second", Duration::from_secs(60)).is_some());
    }
}
