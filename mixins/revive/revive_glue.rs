// Revive: glue between Horizon's server loop and the revive rules.
// Added to the game as `game::revive_glue`.

use crate::game::{
    Game,
    revive_rules::{Action, Config, PlayerView, Revive},
};
use horizon_client::text::Text;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const ID: &str = "revive";
pub const NAME: &str = "Revive";
pub const VERSION: &str = "0.1.0";

pub struct ModState {
    pub dir: PathBuf,
    pub revive: Revive,
}

static STATE: Mutex<Option<ModState>> = Mutex::new(None);

fn open(dir: PathBuf) -> ModState {
    let mut revive = Revive::new(Config::default());
    let path = dir.join("horizon-revive").join("revive.json");
    if let Ok(bytes) = fs::read(&path)
        && let Ok(val) = serde_json::from_slice(&bytes)
    {
        revive.load_json(&val);
    }
    ModState { dir, revive }
}

fn save(state: &ModState) {
    let folder = state.dir.join("horizon-revive");
    let _ = fs::create_dir_all(&folder);
    let path = folder.join("revive.json");
    let json = state.revive.to_json();
    if let Ok(bytes) = serde_json::to_vec_pretty(&json) {
        let temp = folder.join("revive.tmp");
        if fs::write(&temp, bytes).is_ok() {
            let _ = fs::rename(temp, path);
        }
    }
}

/// The damage hook: called from `native_living_incoming_damage` (which has no `&mut Game`).
pub fn on_damage(entity: i32, kind: &str, damage_type: &str, amount: f32) -> bool {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else {
        return false;
    };
    state.revive.on_damage(entity, kind, damage_type, amount)
}

/// The server tick hook: end of each server tick with running world.
pub fn tick(game: &mut Game<'_>) {
    let Some(dir) = game.native_world_dir() else {
        return;
    };

    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_none_or(|s| s.dir != dir) {
        if let Some(old) = guard.as_ref() {
            save(old);
        }
        *guard = Some(open(dir));
    }
    let Some(state) = guard.as_mut() else {
        return;
    };

    let (players, name_to_uuid) = collect_players(game);
    let actions = state.revive.tick(&players);

    for action in actions {
        match action {
            Action::Down { name } => {
                game.native_broadcast(Text::literal(format!(
                    "[Revive] {name} is down! Sneak near them to revive!"
                )));
                apply_downed_effects(game, &name);
            }
            Action::Bleeding { name, seconds } => {
                apply_downed_effects(game, &name);
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    game.native_action_bar(
                        uuid,
                        Text::literal(format!("§cBleeding out! Time left: {seconds}s")),
                    );
                }
            }
            Action::Progress { name, by, percent } => {
                apply_downed_effects(game, &name);
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    game.native_action_bar(
                        uuid,
                        Text::literal(format!("§eBeing revived by {by}... {percent}%")),
                    );
                }
                if let Some(&uuid) = name_to_uuid.get(&by) {
                    game.native_action_bar(
                        uuid,
                        Text::literal(format!("§aReviving {name}... {percent}%")),
                    );
                }
            }
            Action::Revived { name, by } => {
                game.native_run_command(&format!("effect clear {name}"));
                game.native_run_command(&format!("effect give {name} regeneration 8 1 true"));
                game.native_run_command(&format!("effect give {name} resistance 8 1 true"));
                game.native_broadcast(Text::literal(format!("[Revive] {by} revived {name}!")));
            }
            Action::BledOut { name } => {
                game.native_run_command(&format!("effect clear {name}"));
                game.native_run_command(&format!("kill {name}"));
                game.native_broadcast(Text::literal(format!("[Revive] {name} bled out.")));
            }
            Action::Cleared { name } => {
                game.native_run_command(&format!("effect clear {name}"));
            }
            Action::Save => {
                save(state);
            }
        }
    }
}

/// Applies restrictive debuffs to keep the downed player from running around or fighting.
fn apply_downed_effects(game: &mut Game<'_>, name: &str) {
    game.native_run_command(&format!("effect give {name} slowness 3 4 true"));
    game.native_run_command(&format!("effect give {name} weakness 3 255 true"));
    game.native_run_command(&format!("effect give {name} mining_fatigue 3 255 true"));
}

/// Gathers the current state of all online players on the server.
fn collect_players(game: &Game<'_>) -> (Vec<PlayerView>, HashMap<String, u128>) {
    let mut players = Vec::new();
    let mut name_to_uuid = HashMap::new();

    if let Some(host) = &game.lan {
        for g in &host.hub.guests {
            if !g.placed {
                continue;
            }
            players.push(PlayerView {
                uuid: g.uuid,
                entity: g.entity_id,
                name: g.name.clone(),
                dimension: g.dimension.clone(),
                pos: g.position,
                health: g.health,
                game_mode: g.game_mode,
                sneaking: g.sneaking,
            });
            name_to_uuid.insert(g.name.clone(), g.uuid);
        }
    }

    if let Some(p) = &game.player {
        let own_uuid = game.level.player_uuid.map(|[a, b, c, d]| {
            (u128::from(a as u32) << 96)
                | (u128::from(b as u32) << 64)
                | (u128::from(c as u32) << 32)
                | u128::from(d as u32)
        }).unwrap_or(0);

        let entity = game.lan.as_ref().map_or(p.entity_id, |h| h.host_entity);
        let name = game.options.player_name();

        if !name_to_uuid.contains_key(&name) {
            let sneaking = p.shift_key_down || p.pose == horizon_client::pose::Pose::Crouching;
            players.push(PlayerView {
                uuid: own_uuid,
                entity,
                name: name.clone(),
                dimension: game.dim.id().to_owned(),
                pos: [p.position.x, p.position.y, p.position.z],
                health: p.health.health(),
                game_mode: game.mode_id(),
                sneaking,
            });
            name_to_uuid.insert(name, own_uuid);
        }
    }

    (players, name_to_uuid)
}

/// The world lifecycle hook: writes state on world close.
pub fn closed() {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.take() {
        save(&state);
    }
}
