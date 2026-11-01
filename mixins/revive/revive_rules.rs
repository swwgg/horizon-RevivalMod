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
    /// Ticks of bleeding a hit on a downed player costs, per health point.
    pub penalty_ticks_per_hp: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bleed_ticks: 60 * 20,
            revive_ticks: 5 * 20,
            revive_range: 2.5,
            penalty_ticks_per_hp: 20.0,
        }
    }
}

/// Damage types a teammate couldn't save anyone from: they kill as usual.
pub const UNSAVABLE: [&str; 2] = ["minecraft:out_of_world", "minecraft:generic_kill"];

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
}

impl PlayerView {
    /// Alive and in a mode where they can be hurt (survival, adventure).
    fn mortal(&self) -> bool {
        self.health > 0.0 && matches!(self.game_mode, 0 | 2)
    }
}

/// What the game should do, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `name` just went down: slow them, tell everyone.
    Down { name: String },
    /// `name` is being got up by `by`, `percent` of the way.
    Progress { name: String, by: String, percent: u32 },
    /// `name` is bleeding, `seconds` left (once a second, when nobody helps).
    Bleeding { name: String, seconds: u32 },
    /// `by` got `name` up.
    Revived { name: String, by: String },
    /// `name` bled out: take the downed state off and kill them.
    BledOut { name: String },
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
        let amount = if amount.is_finite() { amount.max(0.0) } else { f32::MAX };
        if let Some(d) = self.downed.get_mut(&uuid) {
            // Hits on a downed player shorten their time instead.
            let penalty = (amount * self.config.penalty_ticks_per_hp).min(u32::MAX as f32) as u32;
            d.left = d.left.saturating_sub(penalty);
            return true;
        }
        if self.pending.contains(&uuid) {
            return true;
        }
        if UNSAVABLE.contains(&damage_type) {
            return false;
        }
        let Some(&health) = self.health.get(&uuid) else {
            return false;
        };
        if health <= 0.0 {
            return false;
        }
        // The amount is before armour and enchantments: a hit that might
        // kill counts as one that does.
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
        self.present = players.iter().map(|p| p.uuid).collect();

        self.uuid_of = players.iter().map(|p| (p.entity, p.uuid)).collect();
        self.health = players
            .iter()
            .filter(|p| p.mortal())
            .map(|p| (p.uuid, p.health))
            .collect();

        for uuid in std::mem::take(&mut self.pending) {
            let Some(p) = by_uuid.get(&uuid) else {
                continue;
            };
            if self.downed.contains_key(&uuid) {
                continue;
            }
            self.downed.insert(
                uuid,
                Downed {
                    left: self.config.bleed_ticks,
                    progress: 0,
                },
            );
            out.push(Action::Down {
                name: p.name.clone(),
            });
            changed = true;
        }

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
                    self.pending_kill.insert(uuid);
                    changed = true;
                }
                continue;
            };
            if !p.mortal() {
                self.downed.remove(&uuid);
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
                    d.progress += 1;
                    if d.progress >= config.revive_ticks {
                        self.downed.remove(&uuid);
                        out.push(Action::Revived {
                            name: p.name.clone(),
                            by: r.name.clone(),
                        });
                        changed = true;
                    } else {
                        out.push(Action::Progress {
                            name: p.name.clone(),
                            by: r.name.clone(),
                            percent: d.progress * 100 / config.revive_ticks.max(1),
                        });
                    }
                }
                None => {
                    d.progress = 0;
                    d.left = d.left.saturating_sub(1);
                    if d.left == 0 {
                        self.downed.remove(&uuid);
                        out.push(Action::BledOut {
                            name: p.name.clone(),
                        });
                        changed = true;
                    } else if d.left % 20 == 0 || self.tick % 20 == 0 {
                        out.push(Action::Bleeding {
                            name: p.name.clone(),
                            seconds: d.left.div_ceil(20),
                        });
                    }
                }
            }
        }
        // Keep the saved time left roughly current.
        if changed || (!self.downed.is_empty() && self.tick % 100 == 0) {
            out.push(Action::Save);
        }
        out
    }

    /// The teammate getting `downed` up: mortal, not downed, in the same
    /// dimension, close enough and sneaking. The nearest when several are.
    fn reviver<'a>(&self, downed: &PlayerView, players: &'a [PlayerView]) -> Option<&'a PlayerView> {
        let range = self.config.revive_range;
        players
            .iter()
            .filter(|r| {
                r.uuid != downed.uuid
                    && r.mortal()
                    && r.sneaking
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
        json!({ "downed": downed, "pending_kill": kill })
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
    }
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
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
        }
    }

    fn quick() -> Config {
        Config {
            bleed_ticks: 40,
            revive_ticks: 10,
            revive_range: 2.0,
            penalty_ticks_per_hp: 1.0,
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
        assert!(out.contains(&Action::Down { name: "Alex".into() }));
        assert!(r.is_downed(1));
        // Hits on a downed player are canceled.
        assert!(r.on_damage(101, "minecraft:player", "minecraft:lava", 4.0));
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
    fn hits_while_down_shorten_the_time() {
        let mut r = Revive::new(quick());
        let players = [player(1, "Alex", [0.0; 3]), player(2, "Steve", [50.0, 0.0, 0.0])];
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:fall", 25.0);
        r.tick(&players);
        r.on_damage(101, "minecraft:player", "minecraft:lava", 100.0);
        assert!(r.tick(&players).contains(&Action::BledOut { name: "Alex".into() }));
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
}
