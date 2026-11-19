# Community map sources

- **Challenge Games arena** by Alex334, listed for Java 1.8.9 and above. Its spawn and setup differ from Cloud 9’s tested Chicken Jockey arena. [Creator’s download page](https://www.planetminecraft.com/project/popularmmos-challenge-games-map-recreation-with-the-real-seed-from-1-6-4-works-in-1-8-9-and-above/)
- **Fantasia Lucky Block BedWars** by Skywardvoyager, released for Java 1.8.9 and intended for its companion Fantasia modpack and at least two players. The ZIP was checked for Anvil region files and a readable `level.dat`. SHA-256 `159dd52b938dc97a5d810867272891510ff9d7a5553953b42a74f8bf7624f884`. [Creator’s CurseForge release](https://www.curseforge.com/minecraft/worlds/luckyblockbedwarsmap-fantasia/files/9534755)

Cloud 9’s Chicken Jockey round uses its own generated arena. To contribute a setup for another map, locate the creator’s world folder containing `level.dat` under `templates/`, point a community patch at that folder with `map.template_id`, then test the map coordinates and required mods. Cloud 9 clones the chosen save for the instance.
