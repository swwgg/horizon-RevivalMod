// Hook: native_living_incoming_damage
// Intercepts lethal damage on players and marks them as downed instead of dying.
fn revive_living_incoming_damage(
    entity: i32,
    kind: &str,
    damage_type: &str,
    amount: f32,
    canceled: &mut bool,
) {
    if crate::game::revive_glue::on_damage(entity, kind, damage_type, amount) {
        *canceled = true;
    }
}
