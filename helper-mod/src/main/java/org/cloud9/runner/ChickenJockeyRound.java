package org.cloud9.runner;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.entity.Entity;
import net.minecraft.entity.monster.EntityZombie;
import net.minecraft.entity.passive.EntityChicken;
import net.minecraft.entity.player.EntityPlayer;
import net.minecraft.init.Blocks;
import net.minecraft.init.Items;
import net.minecraft.item.ItemStack;
import net.minecraft.util.BlockPos;
import net.minecraft.util.ChatComponentText;
import net.minecraft.world.WorldServer;
import net.minecraftforge.event.entity.living.LivingDeathEvent;
import net.minecraftforge.event.entity.player.PlayerInteractEvent;
import net.minecraftforge.fml.common.eventhandler.SubscribeEvent;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;

public final class ChickenJockeyRound {
    private static final String RIDER_TAG = "Cloud9ChickenJockey";
    private final Map<WorldServer, JsonObject> worlds;

    ChickenJockeyRound(Map<WorldServer, JsonObject> worlds) {
        this.worlds = worlds;
    }

    @SubscribeEvent
    public void start(PlayerInteractEvent event) {
        if (event.action != PlayerInteractEvent.Action.RIGHT_CLICK_BLOCK || !(event.world instanceof WorldServer)) {
            return;
        }
        WorldServer world = (WorldServer) event.world;
        JsonObject setup = worlds.get(world);
        if (setup == null || !setup.has("challenge")) {
            return;
        }
        JsonObject challenge = setup.getAsJsonObject("challenge");
        if (!"chicken_jockey".equals(challenge.get("kind").getAsString())
                || !position(challenge.getAsJsonArray("trigger")).equals(event.pos)
                || world.getBlockState(event.pos).getBlock() != Blocks.gold_block) {
            return;
        }
        event.setCanceled(true);
        String run = setup.get("run_id").getAsString();
        if (Files.isRegularFile(completion(world))) {
            tell(event.entityPlayer, "The round is complete. Start a fresh round from Cloud 9 to play again.");
            return;
        }
        for (Entity entity : world.loadedEntityList) {
            if (!entity.isDead && run.equals(entity.getEntityData().getString(RIDER_TAG))) {
                tell(event.entityPlayer, "The Chicken Jockey is already in the arena.");
                return;
            }
        }
        BlockPos spawn = position(challenge.getAsJsonArray("opponent"));
        EntityChicken mount = new EntityChicken(world);
        mount.setChickenJockey(true);
        mount.setLocationAndAngles(spawn.getX() + 0.5, spawn.getY(), spawn.getZ() + 0.5, 0, 0);
        EntityZombie rider = new EntityZombie(world);
        rider.setChild(true);
        rider.enablePersistence();
        rider.setCustomNameTag("Chicken Jockey");
        rider.setCurrentItemOrArmor(4, new ItemStack(Items.leather_helmet));
        rider.setEquipmentDropChance(4, 0);
        rider.getEntityData().setString(RIDER_TAG, run);
        rider.setLocationAndAngles(mount.posX, mount.posY, mount.posZ, 0, 0);
        if (!world.spawnEntityInWorld(mount) || !world.spawnEntityInWorld(rider)) {
            mount.setDead();
            rider.setDead();
            tell(event.entityPlayer, "The opponent could not spawn. Try the gold block again.");
            return;
        }
        rider.mountEntity(mount);
        tell(event.entityPlayer, "Defeat the Chicken Jockey in the arena!");
    }

    @SubscribeEvent
    public void victory(LivingDeathEvent event) {
        if (!(event.entityLiving.worldObj instanceof WorldServer)
                || !(event.source.getEntity() instanceof EntityPlayer)) {
            return;
        }
        WorldServer world = (WorldServer) event.entityLiving.worldObj;
        JsonObject setup = worlds.get(world);
        if (setup == null || !setup.has("challenge")) {
            return;
        }
        String run = setup.get("run_id").getAsString();
        if (!run.equals(event.entityLiving.getEntityData().getString(RIDER_TAG))) {
            return;
        }
        EntityPlayer player = (EntityPlayer) event.source.getEntity();
        try {
            Files.write(completion(world), run.getBytes(StandardCharsets.UTF_8));
            tell(player, "Round complete. You defeated the Chicken Jockey!");
        } catch (IOException e) {
            tell(player, "You defeated the Chicken Jockey. Cloud 9 could not save the result.");
        }
    }

    private static Path completion(WorldServer world) {
        return world.getSaveHandler().getWorldDirectory().toPath().resolve("runner_challenge_complete.txt");
    }

    private static BlockPos position(JsonArray value) {
        return new BlockPos(value.get(0).getAsInt(), value.get(1).getAsInt(), value.get(2).getAsInt());
    }

    private static void tell(EntityPlayer player, String message) {
        player.addChatMessage(new ChatComponentText(message));
    }
}
