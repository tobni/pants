// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Graph-level batching for nodes that support GIL-tracing.
//!
//! When multiple nodes with the same `batch_key` are created in a burst,
//! the BatchAccumulator collects them and executes them together via a
//! caller-provided batch execution function.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::node::{EntryId, Node};

pub struct BatchItem<N: Node> {
    pub entry_id: EntryId,
    pub node: N,
    pub result_tx: oneshot::Sender<Result<N::Item, N::Error>>,
}

struct PendingBatch<N: Node> {
    items: Vec<BatchItem<N>>,
    generation: u64,
}

/// Collects batchable nodes and flushes them when a burst ends.
///
/// Uses a generation counter: each enqueue increments it. After enqueueing,
/// we yield to the tokio executor and check if the generation changed. If not,
/// the burst is over and we flush. This adapts to the actual arrival rate
/// without fixed timeouts.
pub struct BatchAccumulator<N: Node> {
    pending: Arc<Mutex<HashMap<u64, PendingBatch<N>>>>,
}

impl<N: Node> Clone for BatchAccumulator<N> {
    fn clone(&self) -> Self {
        Self {
            pending: self.pending.clone(),
        }
    }
}

impl<N: Node> BatchAccumulator<N> {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Enqueue a batchable node. Returns a future that resolves with the node's result.
    ///
    /// The caller should also call `schedule_flush` to ensure the batch is eventually flushed.
    pub fn enqueue(
        &self,
        batch_key: u64,
        entry_id: EntryId,
        node: N,
    ) -> oneshot::Receiver<Result<N::Item, N::Error>> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock();
        let batch = pending.entry(batch_key).or_insert_with(|| PendingBatch {
            items: Vec::new(),
            generation: 0,
        });
        batch.items.push(BatchItem {
            entry_id,
            node,
            result_tx: tx,
        });
        batch.generation += 1;
        rx
    }

    /// Try to take the batch for a given key if the generation hasn't changed
    /// since `expected_generation`. Returns None if the generation changed
    /// (meaning more items arrived and someone else will flush).
    pub fn try_take(
        &self,
        batch_key: u64,
        expected_generation: u64,
    ) -> Option<Vec<BatchItem<N>>> {
        let mut pending = self.pending.lock();
        if let Some(batch) = pending.get(&batch_key) {
            if batch.generation == expected_generation {
                return pending.remove(&batch_key).map(|b| b.items);
            }
        }
        None
    }

    /// Get the current generation for a batch key.
    pub fn current_generation(&self, batch_key: u64) -> u64 {
        let pending = self.pending.lock();
        pending.get(&batch_key).map_or(0, |b| b.generation)
    }

    /// Send results to waiting batch items.
    pub fn send_results(items: Vec<BatchItem<N>>, results: Vec<Result<N::Item, N::Error>>) {
        for (item, result) in items.into_iter().zip(results) {
            let _ = item.result_tx.send(result);
        }
    }
}
