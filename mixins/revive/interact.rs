fn revive_interact(game: &mut crate::game::Game<'_>, blocked: &mut bool) {
    if crate::game::revive_glue::acting_is_downed(game) {
        *blocked = true;
    }
}
