fn revive_enter_dimension(
    game: &mut crate::game::Game<'_>,
) -> core::ops::ControlFlow<Result<()>> {
    if crate::game::revive_glue::acting_is_downed(game) {
        return core::ops::ControlFlow::Break(Ok(()));
    }
    core::ops::ControlFlow::Continue(())
}
