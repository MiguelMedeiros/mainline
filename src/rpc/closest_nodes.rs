use std::{convert::TryInto, vec::IntoIter};

use crate::{common::MAX_BUCKET_SIZE_K, Id, Node};

const CORRECTION_FACTOR: f64 = 1.0544;

#[derive(Debug, Clone)]
/// Manage closest nodes found in a query.
///
/// Useful to estimate the Dht size.
pub struct ClosestNodes {
    target: Id,
    nodes: Vec<Node>,
}

impl ClosestNodes {
    pub fn new(target: Id) -> Self {
        Self {
            target,
            nodes: Vec::with_capacity(200),
        }
    }

    // === Getters ===

    pub fn target(&self) -> Id {
        self.target
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    // === Public Methods ===

    pub fn add(&mut self, node: Node) {
        let seek = node.id.xor(&self.target);

        match self.nodes.binary_search_by(|prope| {
            if prope.id == node.id {
                std::cmp::Ordering::Equal
            } else {
                prope.id.xor(&self.target).cmp(&seek)
            }
        }) {
            Err(pos) => self.nodes.insert(pos, node),
            _ => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// An estimation of the Dht from the distribution of closest nodes
    /// responding to a query.
    ///
    /// [Read more](../../docs/dht_size_estimate/README.md)
    pub fn dht_size_estimate(&self) -> usize {
        if self.is_empty() {
            return 0;
        };

        let sum = self.nodes.iter().take(20).enumerate().fold(
            0,
            |sum: usize, (i, node): (usize, &Node)| {
                let xor = node.id.xor(&self.target);

                // Round up the lower 4 bytes to get a u128 from u160.
                let distance =
                    u128::from_be_bytes(xor.as_bytes()[0..16].try_into().expect("infallible"));

                let intervals = (u128::MAX / distance) as usize;
                let estimated_n = intervals.saturating_mul(i);

                sum + estimated_n as usize
            },
        );

        let count = MAX_BUCKET_SIZE_K.min(self.nodes.len());

        (CORRECTION_FACTOR * (sum / count) as f64) as usize
    }
}

impl IntoIterator for ClosestNodes {
    type Item = Node;
    type IntoIter = IntoIter<Node>;

    fn into_iter(self) -> Self::IntoIter {
        self.nodes.into_iter()
    }
}

impl<'a> IntoIterator for &'a ClosestNodes {
    type Item = &'a Node;
    type IntoIter = std::slice::Iter<'a, Node>;

    fn into_iter(self) -> Self::IntoIter {
        self.nodes.iter()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn add_sorted_by_id() {
        let target = Id::random();

        let mut closest_nodes = ClosestNodes::new(target);

        for _ in 0..10 {
            let node = Node::random();
            closest_nodes.add(node.clone());
            closest_nodes.add(node);
        }

        assert_eq!(closest_nodes.nodes().len(), 10);

        let distances = closest_nodes
            .nodes()
            .iter()
            .map(|n| n.id.distance(&target))
            .collect::<Vec<_>>();

        let mut sorted = distances.clone();
        sorted.sort();

        assert_eq!(sorted, distances);
    }

    #[test]
    fn simulation() {
        let lookups = 4;
        let acceptable_margin = 0.2;
        let sims = 10;
        let dht_size = 2500 as f64;

        let mean = (0..sims)
            .into_iter()
            .map(|_| simulate(dht_size as usize, lookups) as f64)
            .sum::<f64>()
            / (sims as f64);

        let margin = (mean - dht_size).abs() / dht_size;

        dbg!(&margin);
        assert!(margin <= acceptable_margin);
    }

    fn simulate(dht_size: usize, lookups: usize) -> usize {
        let mut nodes = BTreeMap::new();
        for _ in 0..dht_size {
            let node = Node::random();
            nodes.insert(node.id, node);
        }

        (0..lookups)
            .map(|_| {
                let target = Id::random();

                let mut closest_nodes = ClosestNodes::new(target);

                for (_, node) in nodes.range(target..).take(100) {
                    closest_nodes.add(node.clone())
                }
                for (_, node) in nodes.range(..target).rev().take(100) {
                    closest_nodes.add(node.clone())
                }

                let estimate = closest_nodes.dht_size_estimate();

                estimate
            })
            .sum::<usize>()
            / lookups
    }
}
