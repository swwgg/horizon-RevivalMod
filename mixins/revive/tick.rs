// Hook: native_server_tick
// Runs at the end of each server tick to update bleeding, reviving progress, and effects.
fn revive_server_tick(
    game: &mut crate::game::Game<'_>,
    phase: crate::game::native_lifecycle::ServerTickPhase,
) {
    if matches!(
        phase,
        crate::game::native_lifecycle::ServerTickPhase::ServerTickEnd { world_running: true }
    ) {
        crate::game::revive_glue::tick(game);
    }
}
