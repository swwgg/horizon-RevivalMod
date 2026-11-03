// Revive: the rules, with no game types. A player whose hit would kill them
// is "downed" instead: they can't walk fast, jump or fight, and they bleed
// out after a while unless a teammate sneaks next to them long enough.
//
// Added to the game as `game::revive_rules`; `revive_glue` feeds it the
// players each server tick and the damage hook's hits, and turns its
// `Action`s into console commands. The same file is the standalone crate's
// library (Cargo.toml), so its tests run without Horizon.

use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// The rules' numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Ticks a downed player lasts before bleeding out (20 a second).
    pub bleed_ticks: u32,
    /// Ticks a teammate must sneak beside them to get them up.
    pub revive_ticks: u32,
    /// How close (blocks, between feet) the teammate must be.
    pub revive_range: f64,
    /// Ticks a revived player has Revive Sickness (2 minutes default).
    pub sickness_ticks: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bleed_ticks: 40 * 20,
            revive_ticks: 5 * 20,
            revive_range: 2.5,
            sickness_ticks: 120 * 20,
        }
    }
}

impl Config {
    /// The config file's form, in seconds and blocks.
    pub fn to_json(&self) -> Value {
        json!({
            "bleed_seconds": self.bleed_ticks as f64 / 20.0,
            "revive_seconds": self.revive_ticks as f64 / 20.0,
            "revive_range": self.revive_range,
            "sickness_seconds": self.sickness_ticks as f64 / 20.0,
        })
    }

    /// Reads `to_json`'s form; a missing or bad field keeps its default.
    pub fn from_json(value: &Value) -> Self {
        let mut c = Self::default();
        let num = |key: &str| value.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && *v >= 0.0);
        let ticks = |s: f64| (s * 20.0).round().min(u32::MAX as f64) as u32;
        if let Some(v) = num("bleed_seconds") {
            c.bleed_ticks = ticks(v).max(1);
        }
        if let Some(v) = num("revive_seconds") {
            c.revive_ticks = ticks(v).max(1);
        }
        if let Some(v) = num("revive_range") {
            c.revive_range = v;
        }
        if let Some(v) = num("sickness_seconds") {
            c.sickness_ticks = (v * 20.0).round().min(u64::MAX as f64) as u64;
        }
        c
    }
}

/// Vanilla's `CombatRules.getDamageAfterAbsorb`: what is left of `damage`
/// after `armor` points and `toughness`. Enchantments and Resistance are
/// left out, so it errs towards downing.
pub fn after_armor(damage: f32, armor: f32, toughness: f32) -> f32 {
    let toughness = toughness.max(0.0);
    let reduce = (armor - damage / (2.0 + toughness / 4.0)).clamp(armor * 0.2, 20.0);
    damage * (1.0 - reduce / 25.0)
}

/// Damage types a teammate couldn't save anyone from: they kill as usual.
pub const UNSAVABLE: [&str; 2] = ["minecraft:out_of_world", "minecraft:generic_kill"];

/// Damage types armour doesn't reduce (vanilla's `#bypasses_armor`).
pub const ARMOR_IGNORED: [&str; 12] = [
    "minecraft:on_fire",
    "minecraft:in_wall",
    "minecraft:cramming",
    "minecraft:drown",
    "minecraft:fly_into_wall",
    "minecraft:freeze",
    "minecraft:starve",
    "minecraft:fall",
    "minecraft:stalagmite",
    "minecraft:magic",
    "minecraft:indirect_magic",
    "minecraft:wither",
];

/// A player on the server, as the glue reads them each tick.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerView {
    pub uuid: u128,
    /// `Entity.getId`, as the damage hook names the hurt player.
    pub entity: i32,
    pub name: String,
    pub dimension: String,
    pub pos: [f64; 3],
    /// 0 once dead.
    pub health: f32,
    /// 0 survival, 1 creative, 2 adventure, 3 spectator.
    pub game_mode: i32,
    pub sneaking: bool,
    /// Armour points and toughness worn, as last read (0 when unknown).
    pub armor: f32,
    pub toughness: f32,
    /// Whether the player holds a Totem of Undying in main hand or offhand.
    pub has_totem: bool,
    /// Standing on ground (not jumping or mid-air).
    pub on_ground: bool,
}

impl PlayerView {
    /// Alive and in a mode where they can be hurt (survival, adventure).
    pub fn mortal(&self) -> bool {
        self.health > 0.0 && matches!(self.game_mode, 0 | 2)
    }
}

/// What the game should do, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `name` just went down: slow them, tell everyone.
    /// `sick` is true if the player collapsed again during Revive Sickness (shorter timer).
    /// `seconds` is the total seconds given to bleed out.
    Down { name: String, sick: bool, seconds: u32 },
    /// `name` is being got up by `by`, `percent` of the way. `milestone` is true when crossing each 10% step.
    Progress { name: String, by: String, percent: u32, milestone: bool },
    /// `name` is bleeding, `seconds` left (once a second, when nobody helps).
    Bleeding { name: String, seconds: u32 },
    /// `by` got `name` up.
    Revived { name: String, by: String },
    /// `name` bled out: take the downed state off and kill them.
    BledOut { name: String },
    /// All players are dead or downed: instant defeat.
    SquadWipe,
    /// `name` stopped being downed some other way (creative, spectator,
    /// dead): only take the downed state off.
    Cleared { name: String },
    /// The saved state changed: write it (`Revive::to_json`).
    Save,
}

/// A downed player.
#[derive(Clone, Debug, PartialEq)]
struct Downed {
    /// Ticks left before they bleed out.
    left: u32,
    /// Ticks of a teammate's help so far.
    progress: u32,
}

/// The rules' state for the open world.
#[derive(Debug, Default)]
pub struct Revive {
    pub config: Config,
    downed: HashMap<u128, Downed>,
    /// Hit this tick with a hit that would have killed them: downed at the
    /// next tick (the damage hook can't run commands).
    pending: Vec<u128>,
    /// Bled out while away: killed when they come back.
    pending_kill: HashSet<u128>,
    /// From the last tick: the hurt player's uuid by entity id.
    uuid_of: HashMap<i32, u128>,
    /// From the last tick, lowered by the hits since: each mortal player's
    /// health.
    health: HashMap<u128, f32>,
    /// From the last tick: each player's armour points and toughness.
    armor: HashMap<u128, (f32, f32)>,
    /// Players holding a Totem of Undying.
    totem: HashSet<u128>,
    /// Expiration tick for Revive Sickness (chain knockout protection).
    sickness: HashMap<u128, u64>,
    /// Players on and present last tick.
    present: HashSet<u128>,
    tick: u64,
}

impl Revive {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Whether `uuid` is downed (or about to be).
    pub fn is_downed(&self, uuid: u128) -> bool {
        self.downed.contains_key(&uuid) || self.pending.contains(&uuid)
    }

    /// All UUIDs currently downed.
    pub fn downed_uuids(&self) -> Vec<u128> {
        self.downed.keys().copied().collect()
    }

    /// A mortal player other than `uuid` who isn't downed: someone who could
    /// come and help.
    fn helper_for(&self, uuid: u128) -> bool {
        self.health
            .iter()
            .any(|(&u, &h)| u != uuid && h > 0.0 && !self.is_downed(u))
    }

    /// The damage hook: `entity` of type `kind` is about to take `amount` of
    /// `damage_type`. True to cancel the hit.
    pub fn on_damage(&mut self, entity: i32, kind: &str, damage_type: &str, amount: f32) -> bool {
        if kind != "minecraft:player" {
            return false;
        }
        let Some(&uuid) = self.uuid_of.get(&entity) else {
            return false;
        };
        // Unsavable damage (void, /kill) ALWAYS kills, even if already downed!
        if UNSAVABLE.contains(&damage_type) {
            self.downed.remove(&uuid);
            self.pending.retain(|&u| u != uuid);
            return false;
        }
        let amount = if amount.is_finite() { amount.max(0.0) } else { f32::MAX };
        if self.downed.contains_key(&uuid) {
            // Damage while downed is completely canceled without timer penalties.
            // The bleed timer only decreases naturally with time (1 second per second).
            return true;
        }
        if self.pending.contains(&uuid) {
            return true;
        }
        // If holding a Totem of Undying, let vanilla totem mechanics fire instead of downing!
        if self.totem.contains(&uuid) {
            return false;
        }
        let Some(&health) = self.health.get(&uuid) else {
            return false;
        };
        if health <= 0.0 {
            return false;
        }
        // The amount is before armour: take the worn armour off, as vanilla
        // will. Enchantments and Resistance are left out, so a hit that
        // might kill still counts as one that does.
        let (armor, toughness) = self.armor.get(&uuid).copied().unwrap_or((0.0, 0.0));
        let amount = if amount == f32::MAX || ARMOR_IGNORED.contains(&damage_type) {
            amount
        } else {
            after_armor(amount, armor, toughness)
        };
        if amount >= health && self.helper_for(uuid) {
            self.pending.push(uuid);
            return true;
        }
        self.health.insert(uuid, health - amount);
        false
    }

    /// The end of a server tick, with every player on.
    pub fn tick(&mut self, players: &[PlayerView]) -> Vec<Action> {
        self.tick += 1;
        let mut out = Vec::new();
        let mut changed = false;
        let by_uuid: HashMap<u128, &PlayerView> = players.iter().map(|p| (p.uuid, p)).collect();

        // Back after bleeding out while away.
        for p in players {
            if !self.present.contains(&p.uuid) && self.pending_kill.remove(&p.uuid) {
                out.push(Action::BledOut {
                    name: p.name.clone(),
                });
                changed = true;
            }
        }
        self.present.clear();
        self.present.extend(players.iter().map(|p| p.uuid));

        self.uuid_of.clear();
        self.uuid_of.extend(players.iter().map(|p| (p.entity, p.uuid)));

        self.health.clear();
        self.health.extend(
            players
                .iter()
                .filter(|p| p.mortal())
                .map(|p| (p.uuid, p.health)),
        );

        self.armor.clear();
        self.armor.extend(players.iter().map(|p| (p.uuid, (p.armor, p.toughness))));

        self.totem.clear();
        self.totem.extend(players.iter().filter(|p| p.has_totem).map(|p| p.uuid));

        // Squad Wipe check: if all players on the server are downed or dead, wipe squad.
        // Requires at least 2 players on server to prevent accidental wipes on single-player rejoin/disconnect.
        let any_capable_helper = players
            .iter()
            .any(|p| p.mortal() && !self.is_downed(p.uuid));

        if players.len() >= 2 && !any_capable_helper && (!self.downed.is_empty() || !self.pending.is_empty()) {
            for uuid in self.downed.keys().copied().chain(self.pending.drain(..)) {
                if let Some(p) = by_uuid.get(&uuid) {
                    out.push(Action::BledOut { name: p.name.clone() });
                }
            }
            self.downed.clear();
            self.sickness.clear();
            out.push(Action::SquadWipe);
            out.push(Action::Save);
            return out;
        }

        for uuid in std::mem::take(&mut self.pending) {
            let Some(p) = by_uuid.get(&uuid) else {
                continue;
            };
            if self.downed.contains_key(&uuid) {
                continue;
            }
            let sick = self.sickness.get(&uuid).is_some_and(|&until| self.tick < until);
            let left = if sick {
                (self.config.bleed_ticks / 2).max(20) // 20s (half of bleed timer)
            } else {
                self.config.bleed_ticks
            };
            self.downed.insert(
                uuid,
                Downed {
                    left,
                    progress: 0,
                },
            );
            out.push(Action::Down {
                name: p.name.clone(),
                sick,
                seconds: left.div_ceil(20),
            });
            changed = true;
        }

        if !self.downed.is_empty() {
            let mut uuids: Vec<u128> = self.downed.keys().copied().collect();
            uuids.sort_unstable();
            for uuid in uuids {
                let Some(p) = by_uuid.get(&uuid) else {
                    // Away: they keep bleeding, and die when they come back.
                    let d = self.downed.get_mut(&uuid).expect("listed");
                    d.progress = 0;
                    d.left = d.left.saturating_sub(1);
                    if d.left == 0 {
                        self.downed.remove(&uuid);
                        self.sickness.remove(&uuid);
                        self.pending_kill.insert(uuid);
                        changed = true;
                    }
                    continue;
                };
                if !p.mortal() {
                    self.downed.remove(&uuid);
                    self.sickness.remove(&uuid);
                    out.push(Action::Cleared {
                        name: p.name.clone(),
                    });
                    changed = true;
                    continue;
                }
                let reviver = self.reviver(p, players);
                let config = self.config.clone();
                let d = self.downed.get_mut(&uuid).expect("listed");
                match reviver {
                    Some(r) => {
                        // Help pauses the bleeding.
                        let old_step = d.progress * 10 / config.revive_ticks.max(1);
                        d.progress += 1;
                        if d.progress >= config.revive_ticks {
                            self.downed.remove(&uuid);
                            self.sickness.insert(uuid, self.tick + config.sickness_ticks);
                            out.push(Action::Revived {
                                name: p.name.clone(),
                                by: r.name.clone(),
                            });
                            changed = true;
                        } else {
                            let new_step = d.progress * 10 / config.revive_ticks.max(1);
                            out.push(Action::Progress {
                                name: p.name.clone(),
                                by: r.name.clone(),
                                percent: d.progress * 100 / config.revive_ticks.max(1),
                                milestone: new_step > old_step,
                            });
                        }
                    }
                    None => {
                        d.progress = 0;
                        d.left = d.left.saturating_sub(1);
                        if d.left == 0 {
                            self.downed.remove(&uuid);
                            self.sickness.remove(&uuid);
                            out.push(Action::BledOut {
                                name: p.name.clone(),
                            });
                            changed = true;
                        } else if d.left.is_multiple_of(20) {
                            out.push(Action::Bleeding {
                                name: p.name.clone(),
                                seconds: d.left.div_ceil(20),
                            });
                        }
                    }
                }
            }
        }
        // Keep the saved time left roughly current.
        if changed || (!self.downed.is_empty() && self.tick.is_multiple_of(100)) {
            out.push(Action::Save);
        }
        out
    }

    /// The teammate getting `downed` up: mortal, not downed, on ground,
    /// in the same dimension, close enough and sneaking. The nearest when several are.
    fn reviver<'a>(&self, downed: &PlayerView, players: &'a [PlayerView]) -> Option<&'a PlayerView> {
        let range = self.config.revive_range;
        players
            .iter()
            .filter(|r| {
                r.uuid != downed.uuid
                    && r.mortal()
                    && r.sneaking
                    && r.on_ground
                    && !self.is_downed(r.uuid)
                    && r.dimension == downed.dimension
                    && distance_sq(r.pos, downed.pos) <= range * range
            })
            .min_by(|a, b| {
                distance_sq(a.pos, downed.pos).total_cmp(&distance_sq(b.pos, downed.pos))
            })
    }

    /// What survives a restart: who is downed (with their time left) and who
    /// bled out while away.
    pub fn to_json(&self) -> Value {
        let mut downed = Map::new();
        for (uuid, d) in &self.downed {
            downed.insert(format!("{uuid:032x}"), json!(d.left));
        }
        let mut kill: Vec<String> = self.pending_kill.iter().map(|u| format!("{u:032x}")).collect();
        kill.sort();
        let mut sickness = Map::new();
        for (&uuid, &until) in &self.sickness {
            if until > self.tick {
                sickness.insert(format!("{uuid:032x}"), json!(until - self.tick));
            }
        }
        json!({ "downed": downed, "pending_kill": kill, "sickness": sickness })
    }

    /// Reads `to_json`'s output; anything it can't read is left out.
    pub fn load_json(&mut self, value: &Value) {
        if let Some(downed) = value.get("downed").and_then(Value::as_object) {
            for (key, left) in downed {
                let (Ok(uuid), Some(left)) = (u128::from_str_radix(key, 16), left.as_u64()) else {
                    continue;
                };
                let left = (left as u32).clamp(1, self.config.bleed_ticks.max(1));
                self.downed.insert(uuid, Downed { left, progress: 0 });
            }
        }
        if let Some(kill) = value.get("pending_kill").and_then(Value::as_array) {
            for key in kill.iter().filter_map(Value::as_str) {
                if let Ok(uuid) = u128::from_str_radix(key, 16) {
                    self.pending_kill.insert(uuid);
                }
            }
        }
        if let Some(sick) = value.get("sickness").and_then(Value::as_object) {
            for (key, left) in sick {
                let (Ok(uuid), Some(left)) = (u128::from_str_radix(key, 16), left.as_u64()) else {
                    continue;
                };
                self.sickness.insert(uuid, self.tick + left);
            }
        }
    }
}

pub fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(uuid: u128, name: &str, pos: [f64; 3]) -> PlayerView {
        PlayerView {
            uuid,
            entity: uuid as i32 + 100,
            name: name.to_owned(),
            dimension: "minecraft:overworld".to_owned(),
            pos,
            health: 20.0,
            game_mode: 0,
            sneaking: false,
            armor: 0.0,
            toughness: 0.0,
            has_totem: false,
            on_ground: true,
        }
    }

    fn quick() -> Config {
        Config {
            bleed_ticks: 40,
            revive_ticks: 10,
            revive_range: 2.0,
            sickness_ticks: 100,
        }
    }

    #[test]
    fn a_lethal_hit_downs_instead_of_killing() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [10.0, 0.0, 0.0])];
        r.tick(&players);
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:mob_attack", 5.0));
        assert!(r.on_damage(101, "minecraft:player", "minecraft:mob_attack", 15.0));
        let out = r.tick(&players);
        assert!(out.contains(&Action::Down { name: "Alex".into(), sick: false, seconds: 2 }));
        assert!(r.is_downed(1));
        // Hits on a downed player are canceled.
        assert!(r.on_damage(101, "minecraft:player", "minecraft:lava", 4.0));
    }

    #[test]
    fn void_damage_while_downed_kills_immediately() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [10.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        assert!(r.is_downed(1));
        // Falling into the void while downed must NOT be canceled: it kills immediately!
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:out_of_world", 10.0));
        assert!(!r.is_downed(1));
    }

    #[test]
    fn squad_wipe_when_all_are_down_or_dead() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [10.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        assert!(r.is_downed(1));

        // Steve dies!
        players[1].health = 0.0;
        let out = r.tick(&players);
        assert!(out.contains(&Action::SquadWipe));
        assert!(out.contains(&Action::BledOut { name: "Alex".into() }));
        assert!(!r.is_downed(1));
    }

    #[test]
    fn reviver_must_be_on_ground() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [1.0, 0.0, 0.0])];
        players[1].sneaking = true;
        players[1].on_ground = false; // jumping or falling
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        // Not on ground -> cannot revive!
        assert!(r.reviver(&players[0], &players).is_none());
        players[1].on_ground = true;
        assert!(r.reviver(&players[0], &players).is_some());
    }

    #[test]
    fn holding_totem_does_not_down() {
        let mut r = Revive::new(quick());
        let mut alex = player(1, "Alex", [0.0; 3]);
        alex.has_totem = true;
        r.tick(&[alex, player(2, "Steve", [10.0, 0.0, 0.0])]);
        // Lethal hit should NOT be canceled: let vanilla totem trigger!
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:mob_attack", 25.0));
        assert!(!r.is_downed(1));
    }

    #[test]
    fn revive_sickness_reduces_bleed_time() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [1.0, 0.0, 0.0])];
        players[1].sneaking = true;
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        for _ in 0..10 {
            r.tick(&players);
        }
        assert!(!r.is_downed(1)); // Revived!

        // Now Alex takes lethal damage again while sick
        players[0].health = 20.0;
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        let out = r.tick(&players);
        assert!(out.contains(&Action::Down { name: "Alex".into(), sick: true, seconds: 1 }));
        // Sickness reduces bleed time (in quick() config: max(40/3, 200) = 200)
        assert!(r.downed.get(&1).unwrap().left <= 200);
    }

    #[test]
    fn damage_while_downed_is_canceled_without_penalty() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [10.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        let initial_left = r.downed.get(&1).unwrap().left;
        // Fire hit is canceled and does NOT reduce the timer
        assert!(r.on_damage(101, "minecraft:player", "minecraft:on_fire", 4.0));
        assert_eq!(r.downed.get(&1).unwrap().left, initial_left);
        // Mob hit is canceled and does NOT reduce the timer
        assert!(r.on_damage(101, "minecraft:player", "minecraft:mob_attack", 10.0));
        assert_eq!(r.downed.get(&1).unwrap().left, initial_left);
    }

    #[test]
    fn alone_you_die_as_usual() {
        let mut r = Revive::new(quick());
        r.tick(&[player(1, "Alex", [0.0; 3])]);
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:fall", 30.0));
    }

    #[test]
    fn the_void_and_kill_are_not_saved() {
        let mut r = Revive::new(quick());
        r.tick(&[player(1, "Alex", [0.0; 3]), player(2, "Steve", [0.0; 3])]);
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:out_of_world", 30.0));
    }

    #[test]
    fn mobs_are_ignored() {
        let mut r = Revive::new(quick());
        r.tick(&[player(1, "Alex", [0.0; 3]), player(2, "Steve", [0.0; 3])]);
        assert!(!r.on_damage(101, "minecraft:zombie", "minecraft:fall", 30.0));
    }

    #[test]
    fn a_sneaking_teammate_gets_you_up() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [1.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        // Standing next to them isn't enough.
        let out = r.tick(&players);
        assert!(!out.iter().any(|a| matches!(a, Action::Progress { .. })));
        players[1].sneaking = true;
        let mut revived = false;
        for _ in 0..10 {
            revived |= r.tick(&players).contains(&Action::Revived {
                name: "Alex".into(),
                by: "Steve".into(),
            });
        }
        assert!(revived);
        assert!(!r.is_downed(1));
    }

    #[test]
    fn too_far_or_elsewhere_does_not_count() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [5.0, 0.0, 0.0])];
        players[1].sneaking = true;
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        assert!(r.reviver(&players[0], &players).is_none());
        players[1].pos = [1.0, 0.0, 0.0];
        players[1].dimension = "minecraft:the_nether".into();
        assert!(r.reviver(&players[0], &players).is_none());
    }

    #[test]
    fn nobody_comes_and_you_bleed_out() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [50.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        let mut bled = false;
        for _ in 0..41 {
            bled |= r.tick(&players).contains(&Action::BledOut { name: "Alex".into() });
        }
        assert!(bled);
        assert!(!r.is_downed(1));
        // Once dead, hits are no longer canceled.
        players[0].health = 0.0;
        r.tick(&players);
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0));
    }

    #[test]
    fn hits_while_down_do_not_shorten_time() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [50.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        let left_before = r.downed.get(&1).unwrap().left;
        assert!(r.on_damage(101, "minecraft:player", "minecraft:explosion", 100.0));
        assert_eq!(r.downed.get(&1).unwrap().left, left_before);
        assert!(r.is_downed(1));
    }

    #[test]
    fn leaving_while_down_does_not_save_you() {
        let mut r = Revive::new(quick());
        let alex = player(1, "Alex", [0.0; 3]);
        let steve = player(2, "Steve", [50.0, 0.0, 0.0]);
        r.tick(&[alex.clone(), steve.clone()]);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&[alex.clone(), steve.clone()]);
        for _ in 0..50 {
            r.tick(&[steve.clone()]);
        }
        let out = r.tick(&[alex, steve]);
        assert!(out.contains(&Action::BledOut { name: "Alex".into() }));
    }

    #[test]
    fn the_state_survives_a_restart() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [50.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        let saved = r.to_json();
        let mut again = Revive::new(quick());
        again.load_json(&saved);
        assert!(again.is_downed(1));
        assert!(!again.is_downed(2));
    }

    #[test]
    fn two_downed_players_cannot_help_each_other() {
        let mut r = Revive::new(quick());
        let mut players = vec![player(1, "Alex", [0.0; 3]), player(2, "Steve", [1.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        // Steve is the last one up: a lethal hit on him kills as usual.
        assert!(!r.on_damage(102, "minecraft:player", "minecraft:fall", 25.0));
        players[1].sneaking = true;
        players[1].health = 0.0;
        assert!(r.reviver(&players[0], &players).is_none());
    }

    #[test]
    fn armour_that_would_save_you_does() {
        let mut r = Revive::new(quick());
        let mut alex = player(1, "Alex", [0.0; 3]);
        alex.armor = 20.0; // full diamond
        alex.toughness = 8.0;
        r.tick(&[alex, player(2, "Steve", [10.0, 0.0, 0.0])]);
        // 24 raw is ~5.8 after diamond: not lethal on 20 health.
        assert!(!r.on_damage(101, "minecraft:player", "minecraft:mob_attack", 24.0));
        // Falls ignore armour.
        assert!(r.on_damage(101, "minecraft:player", "minecraft:fall", 15.0));
    }

    #[test]
    fn config_round_trips_and_ignores_junk() {
        let c = Config::from_json(&Config::default().to_json());
        assert_eq!(c, Config::default());
        let c = Config::from_json(&json!({"bleed_seconds": 30, "revive_range": "far"}));
        assert_eq!(c.bleed_ticks, 600);
        assert_eq!(c.revive_range, Config::default().revive_range);
    }

    #[test]
    fn bleeding_action_ticks_cleanly_once_per_second() {
        let mut c = quick();
        c.bleed_ticks = 100;
        let mut r = Revive::new(c);
        let alex = player(1, "Alex", [0.0; 3]);
        let steve = player(2, "Steve", [50.0, 0.0, 0.0]); // far away
        r.tick(&[alex.clone(), steve.clone()]);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&[alex.clone(), steve.clone()]);

        let mut bleeding_count = 0;
        // Run 40 ticks (2 seconds)
        for _ in 0..40 {
            let actions = r.tick(&[alex.clone(), steve.clone()]);
            for a in actions {
                if let Action::Bleeding { .. } = a {
                    bleeding_count += 1;
                }
            }
        }
        // In 40 ticks, exactly 2 seconds should pass and emit exactly 2 Bleeding actions
        assert_eq!(bleeding_count, 2);
    }

    #[test]
    fn disconnect_does_not_squad_wipe() {
        let mut r = Revive::new(quick());
        let alex = player(1, "Alex", [0.0; 3]);
        let steve = player(2, "Steve", [1.0, 0.0, 0.0]);
        r.tick(&[alex.clone(), steve.clone()]);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&[alex.clone(), steve.clone()]);
        assert!(r.is_downed(1));

        // Steve disconnects (e.g. crash/lag): only Alex is on the server now
        let out = r.tick(&[alex.clone()]);
        // Alex MUST NOT be squad wiped immediately!
        assert!(!out.contains(&Action::SquadWipe));
        assert!(r.is_downed(1));

        // Steve reconnects and can save Alex
        let mut steve_reconnected = steve.clone();
        steve_reconnected.sneaking = true;
        let out = r.tick(&[alex.clone(), steve_reconnected]);
        assert!(!out.contains(&Action::SquadWipe));
        assert!(out.iter().any(|a| matches!(a, Action::Progress { .. })));
    }

    #[test]
    fn sickness_cleared_on_death() {
        let mut r = Revive::new(quick());
        let mut alex = player(1, "Alex", [0.0; 3]);
        let mut steve = player(2, "Steve", [1.0, 0.0, 0.0]);
        steve.sneaking = true;
        r.tick(&[alex.clone(), steve.clone()]);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&[alex.clone(), steve.clone()]);

        // Revive Alex
        for _ in 0..10 {
            r.tick(&[alex.clone(), steve.clone()]);
        }
        assert!(!r.is_downed(1));
        assert!(r.sickness.contains_key(&1));

        // Alex takes lethal damage again (with sickness) and bleeds out
        alex.health = 20.0;
        steve.sneaking = false;
        r.tick(&[alex.clone(), steve.clone()]);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&[alex.clone(), steve.clone()]);
        assert!(r.is_downed(1));

        // Let bleed timer run down completely
        for _ in 0..25 {
            r.tick(&[alex.clone(), steve.clone()]);
        }
        assert!(!r.is_downed(1));
        // Sickness must be cleared upon death so fresh respawn isn't penalized!
        assert!(!r.sickness.contains_key(&1));
    }
}
