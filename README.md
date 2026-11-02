# Horizon Revival Mod

A native Rust mod for [Horizon](https://github.com/horizon-rust/horizon) that adds a co-op knockdown and revive mechanic for hardcore mode.

When taking fatal damage, players are knocked down instead of dying instantly, giving teammates a chance to save them.

## Features

* **Knocked Down, Not Dead:** Fall to the ground (crawling pose). A shockwave knocks back nearby mobs and defuses creeper fuses. You gain Invisibility so mobs drop aggro and retarget standing players, while a glowing outline keeps you visible to teammates.
* **Action Lock:** Downed players cannot move, attack, break blocks, use items, drop items (`Q`), or use portals.
* **40s Bleed-out Timer:** Actionbar countdown with heartbeat sounds and blood particles. Damage penalties have a 1s cooldown (i-frames) so mobs cannot shred your timer instantly.
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
