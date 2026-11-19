package org.cloud9.runner;

import net.minecraft.entity.monster.EntityZombie;
import net.minecraft.entity.passive.EntityChicken;
import net.minecraft.entity.passive.EntityVillager;
import net.minecraft.entity.player.EntityPlayerMP;
import net.minecraft.init.Items;
import net.minecraft.inventory.ContainerMerchant;
import net.minecraft.inventory.IInventory;
import net.minecraft.item.ItemStack;
import net.minecraft.util.AxisAlignedBB;
import net.minecraft.util.BlockPos;
import net.minecraft.util.EnumFacing;
import net.minecraft.world.WorldServer;
import net.minecraftforge.fml.common.FMLCommonHandler;
import net.minecraftforge.fml.common.Mod;
import net.minecraftforge.fml.common.event.FMLInitializationEvent;
import net.minecraftforge.fml.common.eventhandler.SubscribeEvent;
import net.minecraftforge.fml.common.gameevent.TickEvent;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

@Mod(modid = "cloud9-playtest", name = "Cloud 9 Test Driver", version = "1",
        dependencies = "required-after:runner-helper", clientSideOnly = true)
public final class CommunityPlaytest {
    private boolean tested;
    private EntityZombie rider;
    private int ticks;
    private int strikes;

    @Mod.EventHandler
    public void initialize(FMLInitializationEvent event) {
        if (Boolean.getBoolean("cloud9.openPreparedWorld")) {
            FMLCommonHandler.instance().bus().register(this);
        }
    }

    @SubscribeEvent
    public void test(TickEvent.WorldTickEvent event) throws Exception {
        if (tested || event.phase != TickEvent.Phase.END || !(event.world instanceof WorldServer)
                || event.world.playerEntities.isEmpty()) {
            return;
        }
        WorldServer world = (WorldServer) event.world;
        Path directory = world.getSaveHandler().getWorldDirectory().toPath();
        if (!Files.isRegularFile(directory.resolve("runner_applied.txt"))) {
            return;
        }
        EntityPlayerMP player = (EntityPlayerMP) world.playerEntities.get(0);
        if (!player.inventory.hasItem(Items.stone_sword)) {
            return;
        }
        Path result = directory.resolve("runner_playtest.txt");
        try {
            if (rider == null) {
                rider = prepare(world, player);
                return;
            }
            if (rider.isEntityAlive()) {
                if (++ticks % 11 == 0) {
                    check(++strikes <= 12, "iron sword could not defeat the opponent, health " + rider.getHealth());
                    player.setPositionAndUpdate(rider.posX, rider.posY, rider.posZ - 1);
                    player.attackTargetEntityWithCurrentItem(rider);
                }
                return;
            }
            tested = true;
            finish(world, player);
            Files.write(result, "passed".getBytes(StandardCharsets.UTF_8));
        } catch (RuntimeException error) {
            tested = true;
            Files.write(result, error.toString().getBytes(StandardCharsets.UTF_8));
            throw error;
        }
    }

    private static EntityZombie prepare(WorldServer world, EntityPlayerMP player) {
        IInventory chest = (IInventory) world.getTileEntity(new BlockPos(-4, 65, -4));
        check(chest != null && chest.getStackInSlot(0).stackSize == 6, "starter Lucky Blocks absent");
        ItemStack emeralds = chest.getStackInSlot(4);
        check(emeralds != null && emeralds.getItem() == Items.emerald, "trading currency absent");

        AxisAlignedBB ring = new AxisAlignedBB(-15, 63, -15, 15, 80, 15);
        EntityVillager trader = world.getEntitiesWithinAABB(EntityVillager.class, ring).get(0);
        player.setPositionAndUpdate(4.5, 65, -3.5);
        player.displayVillagerTradeGui(trader);
        ContainerMerchant shop = (ContainerMerchant) player.openContainer;
        shop.getSlot(0).putStack(chest.decrStackSize(4, 2));
        ItemStack purchase = shop.transferStackInSlot(player, 2);
        check(purchase != null && purchase.getItem() == Items.iron_sword, "iron sword trade failed");
        check(player.inventory.hasItem(Items.iron_sword) && shop.getSlot(0).getStack() == null,
                "trade did not exchange currency for the sword");
        player.closeScreen();

        BlockPos trigger = new BlockPos(0, 65, 5);
        player.setPositionAndUpdate(0.5, 65, 4.5);
        player.theItemInWorldManager.activateBlockOrUseItem(
                player, world, null, trigger, EnumFacing.NORTH, .5F, .5F, .5F);
        EntityZombie rider = world.getEntitiesWithinAABB(EntityZombie.class, ring).get(0);
        check(rider.isChild() && rider.ridingEntity instanceof EntityChicken, "opponent did not spawn mounted");
        for (int slot = 0; slot < 9; slot++) {
            ItemStack stack = player.inventory.mainInventory[slot];
            if (stack != null && stack.getItem() == Items.iron_sword) {
                player.inventory.currentItem = slot;
                break;
            }
        }
        check(player.getHeldItem().getItem() == Items.iron_sword, "could not equip the purchased sword");
        return rider;
    }

    private static void finish(WorldServer world, EntityPlayerMP player) {
        check(Files.isRegularFile(world.getSaveHandler().getWorldDirectory().toPath()
                .resolve("runner_challenge_complete.txt")), "victory was not recorded");

        BlockPos lucky = new BlockPos(-22, 65, 0);
        player.setPositionAndUpdate(-21.5, 65, 1.5);
        check(player.theItemInWorldManager.tryHarvestBlock(lucky), "could not break the placed Lucky Block");
    }

    private static void check(boolean condition, String message) {
        if (!condition) {
            throw new IllegalStateException(message);
        }
    }
}
