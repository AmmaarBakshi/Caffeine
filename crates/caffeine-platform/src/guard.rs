//! Combining several native assertions into one RAII guard.

use caffeine_core::Result;
use caffeine_core::power::{ActiveAssertion, AwakeGuard};

/// Holds several guards and releases them in reverse acquisition order.
#[derive(Default)]
pub struct CompositeGuard {
    parts: Vec<Box<dyn AwakeGuard>>,
}

impl CompositeGuard {
    pub fn new() -> CompositeGuard {
        CompositeGuard::default()
    }

    pub fn push(&mut self, guard: Box<dyn AwakeGuard>) {
        self.parts.push(guard);
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

impl std::fmt::Debug for CompositeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeGuard")
            .field("assertions", &self.assertions())
            .finish()
    }
}

impl AwakeGuard for CompositeGuard {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        self.parts.iter().flat_map(|g| g.assertions()).collect()
    }

    fn refresh(&mut self) -> Result<()> {
        let mut first_err = None;
        for part in &mut self.parts {
            if let Err(e) = part.refresh() {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        let mut first_err = None;
        while let Some(part) = self.parts.pop() {
            if let Err(e) = part.release() {
                log::error!("releasing assertion: {e}");
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

// Dropping a CompositeGuard drops each part, and every part releases itself
// in its own Drop implementation.
