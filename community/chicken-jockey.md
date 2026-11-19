# Chicken Jockey Challenge Games

The episode names the Chicken Jockey as the opponent and describes trading with a villager. This community adaptation uses a vanilla chicken jockey, an original arena, and new trades and starting supplies. [Original episode](https://www.youtube.com/watch?v=Xr5TLvfhZuU)

## Play

1. Build the runner helper with Java 8 as described in the [main setup guide](../README.md).
2. Run `python -m scraper seed community/episodes` from the Cloud 9 root. In Episodes, search for **Chicken Jockey** and open its card.
3. Select **Play** to download and verify the pinned Lucky Block jar and open the prepared save. To get the jar first, select **Download pinned mods**. [Official mod file](https://www.curseforge.com/minecraft/mc-mods/lucky-block/files/3652657)
4. Take the supplies from the chest. The west gate leads to a Lucky Block opening pad. Trade emeralds with the Arena Trader for a sword and armor.
5. Right-click the gold block on the south side of the arena and defeat the mounted baby zombie. A victory message confirms the win. Use **Start fresh round** in the instance’s run plan to restore the chest, trader, and fight.

## Verify or contribute

The [base manifest](episodes/Xr5TLvfhZuU.json) supplies episode metadata. The [reviewed patch](Xr5TLvfhZuU.json) pins the mod, defines the opening pad and chest, records the villager trades, and configures the fight. Run `python -m scraper check-community` to validate the merged manifest.

The opt-in playtest uses the same Forge installation, mod resolver, registry preflight, save preparation, and client launch as the desktop app. It verifies the chest, completes a merchant trade, starts and wins the fight, breaks a Lucky Block, and restores the snapshot. The test opens Minecraft and needs a desktop and the pinned jar in `local-mods/`.

```sh
gradle -p helper-mod -Pcloud9Playtest setupCIWorkspace build
cargo test -p cloud9-world --test community_round -- --ignored
```
