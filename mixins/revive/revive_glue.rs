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
pub const VERSION: &str = "0.4.0";

/// The effects a downed player gets: the only ones taken off again, so a
/// player keeps their potions, beacon and fire resistance.
const DOWNED_EFFECTS: [(&str, u8); 4] = [
    ("slowness", 4),
    ("weakness", 255),
    ("mining_fatigue", 255),
    ("glowing", 0),
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

/// Whether the player with `uuid` is downed.
pub fn is_downed(uuid: u128) -> bool {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.revive.is_downed(uuid))
}

/// Whether the player acting in a hook is downed: their item use, block
/// clicks and blows are refused, so they can't eat, drink, pearl away,
/// shoot or build while they lie there.
pub fn acting_is_downed(game: &mut Game<'_>) -> bool {
    let Some(uuid) = acting_uuid(game) else {
        return false;
    };
    let downed = is_downed(uuid);
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

    // Maintain crawling / lying down pose for all downed players
    if let Some(own_uuid) = host_uuid(game) {
        if is_downed(own_uuid) {
            if let Some(p) = game.player.as_mut() {
                let _ = p.set_pose(horizon_client::pose::Pose::Swimming);
            }
        }
    }
    if let Some(host) = &mut game.lan {
        for g in &mut host.hub.guests {
            if is_downed(g.uuid) {
                if let Some(m) = g.motion.as_mut() {
                    m.pose = horizon_client::pose::Pose::Swimming;
                }
            }
        }
    }

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
            Action::Down { name, sick } => {
                let who = target(&name);
                // Dismount from vehicles
                game.native_run_command(&format!("ride {who} dismount"));
                if sick {
                    game.native_broadcast(Text::literal(format!(
                        "[Revive] §c{name} collapsed again from Revive Sickness! Only 20s left!"
                    )));
                } else {
                    game.native_broadcast(Text::literal(format!(
                        "[Revive] {name} is down! Sneak near them to revive!"
                    )));
                }

                // Set visual health to 1.0 (half heart)
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    if let Some(own_uuid) = host_uuid(game) {
                        if uuid == own_uuid {
                            if let Some(p) = game.player.as_mut() {
                                let _ = p.set_health(1.0, false);
                            }
                        }
                    }
                    if let Some(host) = &mut game.lan {
                        if let Some(g) = host.hub.guests.iter_mut().find(|g| g.uuid == uuid) {
                            g.health = 1.0;
                        }
                    }
                }

                // Bell resonance sound and damage indicator particles
                game.native_run_command(&format!("execute at {who} run playsound minecraft:block.bell.resonate player @a ~ ~ ~ 1.5 0.8"));
                game.native_run_command(&format!("execute at {who} run particle minecraft:damage_indicator ~ ~1 ~ 0.5 0.5 0.5 0.1 20"));
                apply_downed_effects(game, &who);
            }
            Action::Bleeding { name, seconds } => {
                let who = target(&name);
                apply_downed_effects(game, &who);
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    game.native_action_bar(
                        uuid,
                        Text::literal(format!("§cBleeding out! Time left: {seconds}s")),
                    );
                }
                // Heartbeat sound
                if seconds <= 15 {
                    game.native_run_command(&format!("execute at {who} run playsound minecraft:entity.warden.heartbeat player @a ~ ~ ~ 1.2 1.3"));
                } else if seconds % 3 == 0 {
                    game.native_run_command(&format!("execute at {who} run playsound minecraft:entity.warden.heartbeat player @a ~ ~ ~ 0.8 0.8"));
                }
                // Blood dust particles
                game.native_run_command(&format!("execute at {who} run particle minecraft:dust 1 0 0 1 ~ ~0.5 ~ 0.3 0.2 0.3 0 4"));
            }
            Action::Progress { name, by, percent } => {
                let who = target(&name);
                apply_downed_effects(game, &who);
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
                // Rising chime sound with percentage
                if percent % 10 == 0 {
                    let pitch = 0.5 + (percent as f32 / 100.0) * 0.5;
                    game.native_run_command(&format!("execute at {who} run playsound minecraft:block.note_block.chime player @a ~ ~ ~ 0.6 {pitch:.2}"));
                }
                // Happy villager / heart sparkles
                game.native_run_command(&format!("execute at {who} run particle minecraft:happy_villager ~ ~0.8 ~ 0.4 0.4 0.4 0 6"));
            }
            Action::Revived { name, by } => {
                let who = target(&name);
                clear_downed_effects(game, &who);

                // Set health to 4.0 HP (2 hearts) - hardcore recovery
                if let Some(&uuid) = name_to_uuid.get(&name) {
                    if let Some(own_uuid) = host_uuid(game) {
                        if uuid == own_uuid {
                            if let Some(p) = game.player.as_mut() {
                                let _ = p.set_health(4.0, false);
                            }
                        }
                    }
                    if let Some(host) = &mut game.lan {
                        if let Some(g) = host.hub.guests.iter_mut().find(|g| g.uuid == uuid) {
                            g.health = 4.0;
                        }
                    }
                }

                // Hardcore "Shock & Trauma" recovery debuffs:
                // 1. Brief resistance (2s) just to prevent instant frame-1 one-tap
                game.native_run_command(&format!("effect give {who} resistance 2 0 true"));
                // 2. Concussion: Darkness for 4s (vision blacking out / waking up)
                game.native_run_command(&format!("effect give {who} darkness 4 0 true"));
                // 3. Limping: Slowness I for 15s (injured legs, cannot sprint away)
                game.native_run_command(&format!("effect give {who} slowness 15 0 true"));
                // 4. Trauma: Weakness I for 20s (shaking hands, reduced damage)
                game.native_run_command(&format!("effect give {who} weakness 20 0 true"));
                // 5. Exhaustion: Hunger II for 10s (depletes food, requires eating)
                game.native_run_command(&format!("effect give {who} hunger 10 1 true"));

                game.native_broadcast(Text::literal(format!("[Revive] {by} revived {name}!")));
                // Totem and levelup celebration sounds & particles
                game.native_run_command(&format!("execute at {who} run playsound minecraft:item.totem.use player @a ~ ~ ~ 1.0 1.0"));
                game.native_run_command(&format!("execute at {who} run playsound minecraft:entity.player.levelup player @a ~ ~ ~ 1.0 1.0"));
                game.native_run_command(&format!("execute at {who} run particle minecraft:totem_of_undying ~ ~1 ~ 0.5 0.8 0.5 0.2 50"));
            }
            Action::BledOut { name } => {
                let who = target(&name);
                clear_downed_effects(game, &who);
                game.native_run_command(&format!("execute at {who} run playsound minecraft:entity.wither.spawn player @a ~ ~ ~ 0.8 0.5"));
                game.native_run_command(&format!("kill {who}"));
                game.native_broadcast(Text::literal(format!("[Revive] {name} bled out.")));
            }
            Action::SquadWipe => {
                game.native_broadcast(Text::literal("[Revive] §cAll teammates are down! Squad eliminated."));
                game.native_run_command("playsound minecraft:entity.wither.spawn player @a ~ ~ ~ 1.0 0.5");
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
            let has_totem = g.offhand.as_ref().is_some_and(|s| &*s.item == "minecraft:totem_of_undying")
                || g.inventory.get(g.selected).and_then(|s| s.as_ref()).is_some_and(|s| &*s.item == "minecraft:totem_of_undying");
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
                has_totem,
                on_ground: g.on_ground,
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
            let has_totem = game.held().is_some_and(|s| &*s.item == "minecraft:totem_of_undying")
                || game.offhand.as_ref().is_some_and(|s| &*s.item == "minecraft:totem_of_undying");
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
                has_totem,
                on_ground: p.ground.on_ground,
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
