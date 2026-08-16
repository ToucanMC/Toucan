use std::collections::{BTreeMap, HashMap};

use toucan_registry::BlockId;

use crate::{BlockPosition, ChunkPosition, FluidKind, MIN_Y, WORLD_HEIGHT};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TickTarget {
    Fluid(FluidKind),
    Block(BlockId),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ScheduledTick {
    pub position: BlockPosition,
    pub target: TickTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoredTick {
    pub scheduled: ScheduledTick,
    pub delay: u32,
}

#[derive(Default)]
pub(crate) struct TickScheduler {
    current_tick: u64,
    due: BTreeMap<u64, Vec<ScheduledTick>>,
    scheduled: HashMap<ScheduledTick, u64>,
}

impl TickScheduler {
    pub fn schedule(&mut self, scheduled: ScheduledTick, delay: u64) {
        if !(MIN_Y..MIN_Y + WORLD_HEIGHT).contains(&scheduled.position.y) {
            return;
        }
        let due = self.current_tick.saturating_add(delay.max(1));
        if self
            .scheduled
            .get(&scheduled)
            .is_some_and(|existing| *existing <= due)
        {
            return;
        }
        self.scheduled.insert(scheduled, due);
        self.due.entry(due).or_default().push(scheduled);
    }

    pub fn restore(&mut self, tick: StoredTick) {
        self.schedule(tick.scheduled, u64::from(tick.delay));
    }

    pub fn advance(&mut self, maximum: usize) -> Vec<ScheduledTick> {
        self.current_tick = self.current_tick.wrapping_add(1);
        let mut ready = Vec::with_capacity(maximum);
        while ready.len() < maximum {
            let Some((&due, _)) = self.due.first_key_value() else {
                break;
            };
            if due > self.current_tick {
                break;
            }
            let mut scheduled_ticks = self.due.remove(&due).unwrap_or_default();
            while let Some(scheduled) = scheduled_ticks.pop() {
                if self.scheduled.get(&scheduled) != Some(&due) {
                    continue;
                }
                if ready.len() == maximum {
                    let retry = self.current_tick.saturating_add(1);
                    self.due.entry(retry).or_default().push(scheduled);
                    self.scheduled.insert(scheduled, retry);
                    continue;
                }
                self.scheduled.remove(&scheduled);
                ready.push(scheduled);
            }
        }
        ready
    }

    pub fn snapshot(&self, chunk: ChunkPosition) -> Vec<StoredTick> {
        let mut pending = self
            .scheduled
            .iter()
            .filter(|(scheduled, _)| {
                ChunkPosition::from_block(scheduled.position.x, scheduled.position.z) == chunk
            })
            .map(|(scheduled, due)| StoredTick {
                scheduled: *scheduled,
                delay: due
                    .saturating_sub(self.current_tick)
                    .min(u64::from(u32::MAX)) as u32,
            })
            .collect::<Vec<_>>();
        pending.sort_unstable_by_key(|tick| {
            let target = match tick.scheduled.target {
                TickTarget::Fluid(FluidKind::Water) => (0, 0),
                TickTarget::Fluid(FluidKind::Lava) => (0, 1),
                TickTarget::Block(block) => (1, block.raw()),
            };
            (
                tick.scheduled.position.y,
                tick.scheduled.position.z,
                tick.scheduled.position.x,
                target,
            )
        });
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::{ScheduledTick, TickScheduler, TickTarget};
    use crate::{BlockPosition, FluidKind};

    #[test]
    fn scheduler_deduplicates_targets_and_honors_budget() {
        let position = BlockPosition { x: 1, y: 64, z: 2 };
        let water = ScheduledTick {
            position,
            target: TickTarget::Fluid(FluidKind::Water),
        };
        let lava = ScheduledTick {
            position,
            target: TickTarget::Fluid(FluidKind::Lava),
        };
        let mut scheduler = TickScheduler::default();
        scheduler.schedule(water, 5);
        scheduler.schedule(water, 3);
        scheduler.schedule(water, 4);
        scheduler.schedule(lava, 3);
        assert!(scheduler.advance(1).is_empty());
        assert!(scheduler.advance(1).is_empty());
        assert_eq!(scheduler.advance(1), vec![lava]);
        assert_eq!(scheduler.advance(1), vec![water]);
    }
}
