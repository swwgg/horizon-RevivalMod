// Hook: native_world_lifecycle
// Flushes and saves downed state when the world closes.
fn revive_world_lifecycle(phase: crate::game::native_lifecycle::WorldPhase) {
    use crate::game::native_lifecycle::WorldPhase;
    if matches!(
        phase,
        WorldPhase::WorldCloseFinal | WorldPhase::WorldCloseFailed
    ) {
        crate::game::revive_glue::closed();
    }
}
