# Horizon Revival Mod

A native Rust mod for [Horizon](https://github.com/horizon-rust/horizon) that adds a co-op knockdown and revive mechanic for hardcore mode.

When taking fatal damage, players are knocked down instead of dying instantly, giving teammates a chance to save them.

## Features

* **Knocked Down, Not Dead:** Fall into a slow wounded crawl (lying down pose) with restricted mobility and no jumping. Fully visible with glowing outline. Nearby mobs are blasted back by a shockwave and creeper fuses defused.
* **Action Lock:** Downed players cannot move fast, attack, break blocks, use items, drop items (`Q`), or use portals.
* **Mob De-Aggro:** Hostile mobs automatically drop aggro and ignore downed players, retargeting standing teammates or wandering.
* **40s Pure Bleed-out Timer:** Actionbar countdown with heartbeat sounds and blood particles. Damage while downed is completely absorbed with zero penalty — the timer ONLY ticks down naturally with time.
* **Totem Support:** Totem of Undying triggers normally first and prevents knockdown.
* **Reviving:** Approach a downed teammate (≤ 2.5m), stand on the ground, and **hold Shift (sneak)** for 5 seconds.
* **Hardcore Recovery:** Get back up with only **2 hearts (4 HP)** and trauma debuffs (Darkness, Slowness, Weakness, Hunger). No free regeneration!
* **Revive Sickness:** Getting downed again within 2 minutes halves bleed-out time to 20 seconds.
* **Squad Wipe:** If all players are down or dead, the run ends immediately.
* **Instant Death:** Void damage and `/kill` bypass knockdown. Single-player deaths are instant.

## Installation

1. Open **Horizon Launcher** → **Mods** → **Horizon Mods** tab.
2. In **"Add native mod from GitHub"**, paste:
   ```text
   https://github.com/swwgg/horizon-RevivalMod
   ```
3. Click **Add from GitHub**, then click **Build**.

## Configuration

Customizable via `<world>/horizon-revive/config.json` (auto-generated on world start).
