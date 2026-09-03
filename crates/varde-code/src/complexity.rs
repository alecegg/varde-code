//! McCabe-style decision-point counting over the `ControlFlow` entities
//! `parsing-extraction` already produces: `complexity = 1 + number of
//! ControlFlow entities in the file`. A file with zero ControlFlow entities
//! therefore scores exactly 1 (not 0, not NULL).
//!
//! The per-file count is folded into [`crate::persist`]'s entity write (the
//! same pass that streams entities into the `entities` table), so this module
//! only retains the single-file helper.

use crate::model::{Entity, EntityKind};

/// Cyclomatic complexity of a single file given its entities.
pub fn cyclomatic_for_entities(entities: &[Entity]) -> u32 {
    1 + entities
        .iter()
        // type-hierarchy: unchanged (cyclomatic complexity counts ControlFlow decision points only)
        .filter(|e| e.kind == EntityKind::ControlFlow)
        .count() as u32
}
