// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Accumulates Python generators from batchable rules for GIL-traced stepping.

use std::collections::HashMap;

use internment::Intern;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::python::{Failure, Params, TypeId, Value};

/// A generator waiting to be GIL-traced with its batch peers.
pub struct PendingGenerator {
    pub generator: Value,
    pub params: Params,
    pub entry: Intern<rule_graph::Entry<crate::tasks::Rule>>,
    pub result_tx: oneshot::Sender<Result<(Value, TypeId), Failure>>,
}

struct PendingBatch {
    items: Vec<PendingGenerator>,
    generation: u64,
}

/// Accumulates generators from batchable rules for GIL-traced stepping.
/// Uses generation-based flushing: after enqueueing, callers yield and
/// check if the generation changed. If unchanged, the burst is over.
pub struct GeneratorBatch {
    pending: Mutex<HashMap<u64, PendingBatch>>,
}

pub struct EnqueueResult {
    pub result_rx: oneshot::Receiver<Result<(Value, TypeId), Failure>>,
    pub generation: u64,
}

impl GeneratorBatch {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn enqueue(
        &self,
        batch_key: u64,
        generator: Value,
        params: Params,
        entry: Intern<rule_graph::Entry<crate::tasks::Rule>>,
    ) -> EnqueueResult {
        let (result_tx, result_rx) = oneshot::channel();
        let mut pending = self.pending.lock();
        let batch = pending.entry(batch_key).or_insert_with(|| PendingBatch {
            items: Vec::new(),
            generation: 0,
        });
        batch.items.push(PendingGenerator {
            generator,
            params,
            entry,
            result_tx,
        });
        batch.generation += 1;
        let generation = batch.generation;
        EnqueueResult {
            result_rx,
            generation,
        }
    }

    /// Take the batch if the generation hasn't changed (burst is over).
    pub fn try_take(
        &self,
        batch_key: u64,
        expected_generation: u64,
    ) -> Option<Vec<PendingGenerator>> {
        let mut pending = self.pending.lock();
        if let Some(batch) = pending.get(&batch_key) {
            if batch.generation == expected_generation {
                return pending.remove(&batch_key).map(|b| b.items);
            }
        }
        None
    }
}
