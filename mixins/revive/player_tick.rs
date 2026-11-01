fn revive_player_tick(game: &mut crate::game::Game<'_>) {
    crate::game::revive_glue::player_tick(game);
}
