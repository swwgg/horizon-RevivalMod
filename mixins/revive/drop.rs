fn revive_drop_held(game: &mut crate::game::Game<'_>) -> core::ops::ControlFlow<()> {
    if crate::game::revive_glue::acting_is_downed(game) {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
