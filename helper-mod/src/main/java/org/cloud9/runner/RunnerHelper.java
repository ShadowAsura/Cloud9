package org.cloud9.runner;

import com.google.gson.Gson;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import net.minecraft.block.Block;
import net.minecraft.entity.item.EntityItem;
import net.minecraft.entity.passive.EntityVillager;
import net.minecraft.entity.player.EntityPlayerMP;
import net.minecraft.init.Blocks;
import net.minecraft.inventory.IInventory;
import net.minecraft.item.Item;
import net.minecraft.item.ItemStack;
import net.minecraft.nbt.JsonToNBT;
import net.minecraft.nbt.NBTTagCompound;
import net.minecraft.nbt.NBTTagList;
import net.minecraft.scoreboard.ScorePlayerTeam;
import net.minecraft.tileentity.TileEntity;
import net.minecraft.util.AxisAlignedBB;
import net.minecraft.util.BlockPos;
import net.minecraft.util.EnumChatFormatting;
import net.minecraft.village.MerchantRecipe;
import net.minecraft.village.MerchantRecipeList;
import net.minecraft.world.WorldServer;
import net.minecraftforge.common.MinecraftForge;
import net.minecraftforge.event.world.WorldEvent;
import net.minecraftforge.fml.common.Mod;
import net.minecraftforge.fml.common.event.FMLInitializationEvent;
import net.minecraftforge.fml.common.event.FMLLoadCompleteEvent;
import net.minecraftforge.fml.common.eventhandler.SubscribeEvent;
import net.minecraftforge.fml.common.gameevent.TickEvent;

import java.io.IOException;
import java.io.Reader;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.util.Map;
import java.util.WeakHashMap;

@Mod(modid = "runner-helper", name = "Cloud 9 Runner Helper", version = "0.1.0",
        acceptedMinecraftVersions = "[1.8.9]", acceptableRemoteVersions = "*")
public final class RunnerHelper {
    private static final String RUN_TAG = "Cloud9Run";
    private static final Gson JSON = new Gson();
    private final Map<WorldServer, JsonObject> worlds = new WeakHashMap<>();

    @Mod.EventHandler
    public void initialize(FMLInitializationEvent event) {
        MinecraftForge.EVENT_BUS.register(this);
        net.minecraftforge.fml.common.FMLCommonHandler.instance().bus().register(this);
    }

    @Mod.EventHandler
    public void dumpRegistry(FMLLoadCompleteEvent event) throws IOException {
        String hash = System.getProperty("cloud9.modSetHash");
        if (hash == null || !hash.matches("[a-f0-9]{64}")) {
            return;
        }
        JsonObject dump = new JsonObject();
        JsonObject items = new JsonObject();
        for (Object name : Item.itemRegistry.getKeys()) {
            Item item = Item.getByNameOrId(name.toString());
            if (item != null) {
                items.addProperty(name.toString(), Item.getIdFromItem(item));
            }
        }
        JsonObject blocks = new JsonObject();
        for (Object name : Block.blockRegistry.getKeys()) {
            Block block = Block.getBlockFromName(name.toString());
            if (block != null) {
                blocks.addProperty(name.toString(), Block.getIdFromBlock(block));
            }
        }
        dump.addProperty("mod_set_hash", hash);
        dump.add("items", items);
        dump.add("blocks", blocks);
        write(Paths.get("registry_dump.json"), JSON.toJson(dump));
    }

    @SubscribeEvent
    public void unload(WorldEvent.Unload event) {
        if (event.world instanceof WorldServer) {
            worlds.remove(event.world);
        }
    }

    @SubscribeEvent
    public void tick(TickEvent.WorldTickEvent event) {
        if (event.phase != TickEvent.Phase.END || !(event.world instanceof WorldServer)
                || event.world.provider.getDimensionId() != 0) {
            return;
        }
        WorldServer world = (WorldServer) event.world;
        if (!worlds.containsKey(world)) {
            Path directory = world.getSaveHandler().getWorldDirectory().toPath();
            Path setupFile = directory.resolve("runner_setup.json");
            if (!Files.isRegularFile(setupFile)) {
                worlds.put(world, null);
                return;
            }
            try (Reader reader = Files.newBufferedReader(setupFile, StandardCharsets.UTF_8)) {
                JsonObject setup = new JsonParser().parse(reader).getAsJsonObject();
                if (setup.get("schema_version").getAsInt() != 1) {
                    throw new IllegalArgumentException("Unsupported Cloud 9 setup version");
                }
                String run = setup.get("run_id").getAsString();
                Path marker = directory.resolve("runner_applied.txt");
                if (!Files.isRegularFile(marker) || !run.equals(new String(Files.readAllBytes(marker), StandardCharsets.UTF_8))) {
                    apply(world, setup, run);
                    world.saveAllChunks(true, null);
                    write(marker, run);
                }
                worlds.put(world, setup);
            } catch (Exception e) {
                throw new IllegalStateException("Cloud 9 setup failed", e);
            }
        }
        JsonObject setup = worlds.get(world);
        if (setup != null) {
            try {
                runGenerators(world, setup);
            } catch (Exception e) {
                throw new IllegalStateException("Cloud 9 generator failed", e);
            }
        }
    }

    private void apply(WorldServer world, JsonObject setup, String run) throws Exception {
        BlockPos spawn = position(setup.getAsJsonArray("spawn"));
        world.setSpawnPoint(spawn);
        for (Map.Entry<String, JsonElement> rule : setup.getAsJsonObject("gamerules").entrySet()) {
            world.getGameRules().setOrCreateGameRule(rule.getKey(), rule.getValue().getAsString());
        }
        JsonArray teams = setup.getAsJsonArray("teams");
        if (setup.get("starter_layout").getAsBoolean()) {
            buildStarterLayout(world, spawn, teams, "lucky_block_bedwars".equals(setup.get("mode").getAsString()));
        }
        for (JsonElement member : teams) {
            JsonObject settings = member.getAsJsonObject();
            String id = settings.get("id").getAsString();
            if (!id.startsWith("cloud9-") || id.length() > 16) {
                throw new IllegalArgumentException("Cloud 9 team ID must begin with cloud9- and fit 16 characters");
            }
            if (world.getScoreboard().getTeam(id) != null) {
                throw new IllegalArgumentException("Cloud 9 team already exists in the source template: " + id);
            }
            ScorePlayerTeam team = world.getScoreboard().createTeam(id);
            team.setTeamName(settings.get("name").getAsString());
            EnumChatFormatting color = EnumChatFormatting.getValueByName(settings.get("color").getAsString());
            if (color != null) {
                team.setChatFormat(color);
                team.setNamePrefix(color.toString());
                team.setNameSuffix(EnumChatFormatting.RESET.toString());
            }
            team.setAllowFriendlyFire(false);
        }
        for (JsonElement entry : setup.getAsJsonArray("villagers")) {
            JsonObject settings = entry.getAsJsonObject();
            BlockPos pos = position(settings.getAsJsonArray("position"));
            EntityVillager villager = new EntityVillager(world);
            villager.setPosition(pos.getX() + 0.5, pos.getY(), pos.getZ() + 0.5);
            villager.setCustomNameTag(settings.get("role").getAsString());
            villager.getEntityData().setString(RUN_TAG, run);
            villager.getEntityData().setString("Cloud9Object", settings.get("id").getAsString());
            MerchantRecipeList recipes = new MerchantRecipeList();
            for (JsonElement trade : settings.getAsJsonArray("trades")) {
                JsonObject offer = trade.getAsJsonObject();
                ItemStack buy = stack(offer.getAsJsonArray("buy").get(0).getAsJsonObject());
                ItemStack other = offer.has("buy_b") && !offer.get("buy_b").isJsonNull()
                        ? stack(offer.getAsJsonObject("buy_b")) : null;
                ItemStack sell = stack(offer.getAsJsonObject("sell"));
                recipes.add(new MerchantRecipe(buy, other, sell, 0, offer.get("max_uses").getAsInt()));
            }
            NBTTagCompound data = new NBTTagCompound();
            villager.writeEntityToNBT(data);
            NBTTagList encoded = recipes.getRecipiesAsTags().getTagList("Recipes", 10);
            NBTTagCompound offers = new NBTTagCompound();
            offers.setTag("Recipes", encoded);
            data.setTag("Offers", offers);
            data.setBoolean("NoAI", true);
            data.setBoolean("PersistenceRequired", true);
            villager.readEntityFromNBT(data);
            if (!world.spawnEntityInWorld(villager)) {
                throw new IllegalArgumentException("Villager could not spawn at " + pos);
            }
        }
        JsonObject center = setup.getAsJsonObject("center");
        for (JsonElement entry : center.getAsJsonArray("blocks")) {
            JsonObject placement = entry.getAsJsonObject();
            JsonObject spec = placement.getAsJsonObject("item");
            String id = spec.get("id").getAsString();
            Block block = Block.getBlockFromName(id);
            if (block == null) {
                throw new IllegalArgumentException("Unknown block " + id);
            }
            world.setBlockState(position(placement.getAsJsonArray("pos")), block.getStateFromMeta(number(spec, "meta", 0)), 2);
        }
        for (JsonElement entry : center.getAsJsonArray("chests")) {
            JsonObject chest = entry.getAsJsonObject();
            BlockPos pos = position(chest.getAsJsonArray("pos"));
            world.setBlockState(pos, Blocks.chest.getDefaultState(), 2);
            TileEntity tile = world.getTileEntity(pos);
            if (!(tile instanceof IInventory)) {
                throw new IllegalArgumentException("Chest could not be created at " + pos);
            }
            tile.getTileData().setString(RUN_TAG, run);
            IInventory inventory = (IInventory) tile;
            for (JsonElement value : chest.getAsJsonArray("slots")) {
                JsonObject slot = value.getAsJsonObject();
                int index = slot.get("slot").getAsInt();
                if (index < 0 || index >= inventory.getSizeInventory()) {
                    throw new IllegalArgumentException("Invalid chest slot " + index + " at " + pos);
                }
                inventory.setInventorySlotContents(index, stack(slot));
            }
            tile.markDirty();
        }
    }

    @SubscribeEvent
    public void player(TickEvent.PlayerTickEvent event) {
        if (event.phase != TickEvent.Phase.END || !(event.player instanceof EntityPlayerMP)) {
            return;
        }
        EntityPlayerMP player = (EntityPlayerMP) event.player;
        JsonObject setup = worlds.get(player.worldObj);
        if (setup == null) {
            return;
        }
        String run = setup.get("run_id").getAsString();
        NBTTagCompound data = player.getEntityData();
        NBTTagCompound persistent = data.getCompoundTag("PlayerPersisted");
        if (run.equals(persistent.getString(RUN_TAG))) {
            return;
        }
        BlockPos spawn = position(setup.getAsJsonArray("spawn"));
        int smallest = Integer.MAX_VALUE;
        String bestTeam = null;
        for (JsonElement entry : setup.getAsJsonArray("teams")) {
            JsonObject settings = entry.getAsJsonObject();
            String id = settings.get("id").getAsString();
            ScorePlayerTeam team = player.worldObj.getScoreboard().getTeam(id);
            if (team != null && team.getMembershipCollection().size() < smallest) {
                bestTeam = id;
                smallest = team.getMembershipCollection().size();
                spawn = position(settings.getAsJsonArray("spawn"));
            }
        }
        if (bestTeam != null) {
            player.worldObj.getScoreboard().addPlayerToTeam(player.getName(), bestTeam);
        }
        player.setPositionAndUpdate(spawn.getX() + 0.5, spawn.getY(), spawn.getZ() + 0.5);
        player.setSpawnPoint(spawn, true);
        try {
            for (JsonElement entry : setup.getAsJsonArray("kits")) {
                ItemStack item = stack(entry.getAsJsonObject());
                if (!player.inventory.addItemStackToInventory(item)) {
                    player.dropPlayerItemWithRandomChoice(item, false);
                }
            }
        } catch (Exception e) {
            throw new IllegalStateException("Cloud 9 kit failed", e);
        }
        persistent.setString(RUN_TAG, run);
        data.setTag("PlayerPersisted", persistent);
    }

    private void runGenerators(WorldServer world, JsonObject setup) throws Exception {
        for (JsonElement entry : setup.getAsJsonObject("center").getAsJsonArray("generators")) {
            JsonObject generator = entry.getAsJsonObject();
            int interval = generator.get("interval_ticks").getAsInt();
            BlockPos pos = position(generator.getAsJsonArray("pos"));
            if (interval <= 0 || world.getTotalWorldTime() % interval != 0 || !world.isBlockLoaded(pos)) {
                continue;
            }
            AxisAlignedBB nearby = new AxisAlignedBB(pos.getX() - 2, pos.getY() - 1, pos.getZ() - 2,
                    pos.getX() + 3, pos.getY() + 3, pos.getZ() + 3);
            if (world.getEntitiesWithinAABB(EntityItem.class, nearby).size() >= 12) {
                continue;
            }
            JsonObject generated = new JsonObject();
            generated.add("item", generator.get("item"));
            generated.addProperty("count", number(generator, "count", 1));
            EntityItem dropped = new EntityItem(world, pos.getX() + 0.5, pos.getY() + 0.3, pos.getZ() + 0.5, stack(generated));
            dropped.getEntityData().setString(RUN_TAG, setup.get("run_id").getAsString());
            world.spawnEntityInWorld(dropped);
        }
    }

    private void buildStarterLayout(WorldServer world, BlockPos spawn, JsonArray teams, boolean bedwars) {
        platform(world, spawn, bedwars ? 7 : 14);
        for (JsonElement entry : teams) {
            BlockPos pos = position(entry.getAsJsonObject().getAsJsonArray("spawn"));
            platform(world, pos, 6);
            world.setBlockState(pos.west(3), Blocks.bed.getStateFromMeta(0), 2);
            world.setBlockState(pos.west(3).south(), Blocks.bed.getStateFromMeta(8), 2);
        }
        if (!bedwars) {
            for (int distance = -14; distance <= 14; ++distance) {
                world.setBlockState(spawn.add(distance, 0, -14), Blocks.cobblestone_wall.getDefaultState(), 2);
                world.setBlockState(spawn.add(distance, 0, 14), Blocks.cobblestone_wall.getDefaultState(), 2);
                if (Math.abs(distance) > 1) {
                    world.setBlockState(spawn.add(-14, 0, distance), Blocks.cobblestone_wall.getDefaultState(), 2);
                    world.setBlockState(spawn.add(14, 0, distance), Blocks.cobblestone_wall.getDefaultState(), 2);
                }
            }
        }
    }

    private void platform(WorldServer world, BlockPos origin, int radius) {
        for (int x = -radius; x <= radius; ++x) {
            for (int z = -radius; z <= radius; ++z) {
                Block block = Math.abs(x) == radius || Math.abs(z) == radius
                        ? Blocks.quartz_block : Blocks.stonebrick;
                if ((Math.abs(x) == radius - 2 && Math.abs(z) == radius - 2) || (x == 0 && z == 0)) {
                    block = Blocks.glowstone;
                }
                world.setBlockState(origin.add(x, -1, z), block.getDefaultState(), 2);
            }
        }
    }

    private static ItemStack stack(JsonObject data) throws Exception {
        JsonObject settings = data.getAsJsonObject("item");
        String id = settings.get("id").getAsString();
        Item item = Item.getByNameOrId(id);
        if (item == null) {
            throw new IllegalArgumentException("Unknown item " + id);
        }
        int count = data.get("count").getAsInt();
        if (count < 1 || count > 64) {
            throw new IllegalArgumentException("Invalid stack count " + count);
        }
        ItemStack result = new ItemStack(item, count, number(settings, "meta", 0));
        if (settings.has("nbt") && settings.get("nbt").isJsonObject()) {
            result.setTagCompound(JsonToNBT.getTagFromJson(settings.get("nbt").toString()));
        }
        if (data.has("nbt") && data.get("nbt").isJsonObject()) {
            NBTTagCompound tag = result.hasTagCompound() ? result.getTagCompound() : new NBTTagCompound();
            tag.merge(JsonToNBT.getTagFromJson(data.get("nbt").toString()));
            result.setTagCompound(tag);
        }
        return result;
    }

    private static int number(JsonObject data, String key, int fallback) {
        return data.has(key) && !data.get(key).isJsonNull() ? data.get(key).getAsInt() : fallback;
    }

    private static BlockPos position(JsonArray value) {
        return new BlockPos(value.get(0).getAsInt(), value.get(1).getAsInt(), value.get(2).getAsInt());
    }

    private static void write(Path file, String data) throws IOException {
        Path absolute = file.toAbsolutePath();
        Path temporary = Files.createTempFile(absolute.getParent(), ".runner-", ".tmp");
        try {
            Files.write(temporary, data.getBytes(StandardCharsets.UTF_8));
            try {
                Files.move(temporary, absolute, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(temporary, absolute, StandardCopyOption.REPLACE_EXISTING);
            }
        } finally {
            Files.deleteIfExists(temporary);
        }
    }
}
