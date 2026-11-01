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
pub const VERSION: &str = "0.2.0";

/// The effects a downed player gets: the only ones taken off again, so a
/// player keeps their potions, beacon and fire resistance.
const DOWNED_EFFECTS: [(&str, u8); 3] = [
    ("slowness", 4),
    ("weakness", 255),
    ("mining_fatigue", 255),
];

pub struct ModState {
    pub dir: PathBuf,
    pub revive: Revive,
}

static STATE: Mutex<Option<ModState>> = Mutex::new(None);

/// Armour points and toughness by player, read in each player's own tick
/// (`native_player_tick`), where their equipment is swapped in.
static ARMOR: Mutex<Option<HashMap<u128, (f32, f32)>>> = Mutex::new(None);

fn folder(dir: &Path) -> PathBuf {
    dir.join("horizon-revive")
}

fn open(dir: PathBuf) -> ModState {
    let config = load_config(&folder(&dir));
    let mut revive = Revive::new(config);
    let path = folder(&dir).join("revive.json");
    if let Ok(bytes) = fs::read(&path)
        && let Ok(val) = serde_json::from_slice(&bytes)
    {
        revive.load_json(&val);
    }
    ModState { dir, revive }
}

/// `config.json` beside the state, written with the defaults when missing
/// so players find what they can change.
fn load_config(folder: &Path) -> Config {
    let path = folder.join("config.json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(|v| Config::from_json(&v))
            .unwrap_or_default(),
        Err(_) => {
            let config = Config::default();
            let _ = fs::create_dir_all(folder);
            if let Ok(bytes) = serde_json::to_vec_pretty(&config.to_json()) {
                let _ = fs::write(&path, bytes);
            }
            config
        }
    }
}

fn save(state: &ModState) {
    let folder = folder(&state.dir);
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

/// A UUID as commands take it (`8-4-4-4-12`): safe to put in a command
/// whatever the player's name.
fn selector(uuid: u128) -> String {
    let h = format!("{uuid:032x}");
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// The damage hook: called from `native_living_incoming_damage` (which has no `&mut Game`).
pub fn on_damage(entity: i32, kind: &str, damage_type: &str, amount: f32) -> bool {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else {
        return false;
    };
    state.revive.on_damage(entity, kind, damage_type, amount)
}

/// The host's UUID, when the world knows it.
fn host_uuid(game: &Game<'_>) -> Option<u128> {
    game.level.player_uuid.map(|[a, b, c, d]| {
        (u128::from(a as u32) << 96)
            | (u128::from(b as u32) << 64)
            | (u128::from(c as u32) << 32)
            | u128::from(d as u32)
    })
}

/// Who acts in a player's hook: the guest swapped in, else the host.
fn acting_uuid(game: &Game<'_>) -> Option<u128> {
    match game.native_acting_player() {
        Some(p) => Some(p.uuid),
        None if game.options.dedicated.is_none() => host_uuid(game),
        None => None,
    }
}

/// Whether the player acting in a hook is downed: their item use, block
/// clicks and blows are refused, so they can't eat, drink, pearl away,
/// shoot or build while they lie there.
pub fn acting_is_downed(game: &mut Game<'_>) -> bool {
    let Some(uuid) = acting_uuid(game) else {
        return false;
    };
    let downed = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.revive.is_downed(uuid));
    if downed {
        game.native_action_bar(uuid, Text::literal("§cYou are down: wait for a teammate!"));
    }
    downed
}

/// The player tick hook: reads the acting player's worn armour.
pub fn player_tick(game: &mut Game<'_>) {
    let Some(uuid) = acting_uuid(game) else {
        return;
    };
    let (mut armor, mut toughness) = (0.0, 0.0);
    for stack in game.armor.iter().flatten() {
        let (a, t) = horizon_client::mobs::spawning::armor_stats(&stack.item);
        armor += a;
        toughness += t;
    }
    ARMOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(uuid, (armor, toughness));
}

/// The server tick hook: end of each server tick with running world.
pub fn tick(game: &mut Game<'_>) {
    let Some(dir) = game.native_world_dir() else {
        return;
    };

    let (players, name_to_uuid) = collect_players(game);
    // The rules run under the lock; the commands after it, so a hook a
    // command sets off (damage, a player's) never waits on it.
    let actions = {
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
        let actions = state.revive.tick(&players);
        if actions.contains(&Action::Save) {
            save(state);
        }
        actions
    };
    let target = |name: &str| name_to_uuid.get(name).map_or_else(|| name.to_owned(), |&u| selector(u));

    for action in actions {
        match action {
            Action::Down { name } => {
                game.native_broadcast(Text::literal(format!(
                    "[Revive] {name} is down! Sneak near them to revive!"
                )));
                apply_downed_effects(game, &target(&name));
            }
            Action::Bleeding { name, seconds } => {
                apply_downed_effects(game, &target(&name));
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    game.native_action_bar(
                        uuid,
                        Text::literal(format!("§cBleeding out! Time left: {seconds}s")),
                    );
                }
            }
            Action::Progress { name, by, percent } => {
                apply_downed_effects(game, &target(&name));
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
                let who = target(&name);
                clear_downed_effects(game, &who);
                game.native_run_command(&format!("effect give {who} regeneration 8 1 true"));
                game.native_run_command(&format!("effect give {who} resistance 8 1 true"));
                game.native_broadcast(Text::literal(format!("[Revive] {by} revived {name}!")));
            }
            Action::BledOut { name } => {
                let who = target(&name);
                clear_downed_effects(game, &who);
                game.native_run_command(&format!("kill {who}"));
                game.native_broadcast(Text::literal(format!("[Revive] {name} bled out.")));
            }
            Action::Cleared { name } => {
                clear_downed_effects(game, &target(&name));
            }
            Action::Save => {}
        }
    }
}

/// Applies restrictive debuffs to keep the downed player from running around or fighting.
fn apply_downed_effects(game: &mut Game<'_>, who: &str) {
    for (effect, amp) in DOWNED_EFFECTS {
        game.native_run_command(&format!("effect give {who} {effect} 3 {amp} true"));
    }
}

/// Takes off only the downed effects.
fn clear_downed_effects(game: &mut Game<'_>, who: &str) {
    for (effect, _) in DOWNED_EFFECTS {
        game.native_run_command(&format!("effect clear {who} {effect}"));
    }
}

/// Gathers the current state of all online players on the server.
fn collect_players(game: &Game<'_>) -> (Vec<PlayerView>, HashMap<String, u128>) {
    let mut players = Vec::new();
    let mut name_to_uuid = HashMap::new();
    let armor = ARMOR.lock().unwrap_or_else(|e| e.into_inner());
    let worn = |uuid: u128| {
        armor
            .as_ref()
            .and_then(|m| m.get(&uuid).copied())
            .unwrap_or((0.0, 0.0))
    };

    if let Some(host) = &game.lan {
        for g in &host.hub.guests {
            if !g.placed {
                continue;
            }
            let (armor, toughness) = worn(g.uuid);
            players.push(PlayerView {
                uuid: g.uuid,
                entity: g.entity_id,
                name: g.name.clone(),
                dimension: g.dimension.clone(),
                pos: g.position,
                health: g.health,
                game_mode: g.game_mode,
                sneaking: g.sneaking,
                armor,
                toughness,
            });
            name_to_uuid.insert(g.name.clone(), g.uuid);
        }
    }

    // A dedicated server has no player of its own; nor do we count a host
    // whose UUID the world doesn't know.
    if game.options.dedicated.is_none()
        && let Some(p) = &game.player
        && let Some(own_uuid) = host_uuid(game)
    {
        let entity = game.lan.as_ref().map_or(p.entity_id, |h| h.host_entity);
        let name = game.options.player_name();

        if !name_to_uuid.contains_key(&name) {
            let sneaking = p.shift_key_down || p.pose == horizon_client::pose::Pose::Crouching;
            let (armor, toughness) = worn(own_uuid);
            players.push(PlayerView {
                uuid: own_uuid,
                entity,
                name: name.clone(),
                dimension: game.dim.id().to_owned(),
                pos: [p.position.x, p.position.y, p.position.z],
                health: p.health.health(),
                game_mode: game.mode_id(),
                sneaking,
                armor,
                toughness,
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
    *ARMOR.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
