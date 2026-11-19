package org.cloud9.runner;

import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.GuiMainMenu;
import net.minecraftforge.fml.common.FMLCommonHandler;
import net.minecraftforge.fml.common.eventhandler.SubscribeEvent;
import net.minecraftforge.fml.common.gameevent.TickEvent;

import java.nio.file.Files;
import java.nio.file.Paths;

public final class ClientStart extends GameStart {
    @Override
    public void initialize() {
        if (Boolean.getBoolean("cloud9.openPreparedWorld")) {
            FMLCommonHandler.instance().bus().register(this);
        }
    }

    @SubscribeEvent
    public void openPreparedWorld(TickEvent.ClientTickEvent event) {
        Minecraft game = Minecraft.getMinecraft();
        if (event.phase != TickEvent.Phase.END || !(game.currentScreen instanceof GuiMainMenu)) {
            return;
        }
        FMLCommonHandler.instance().bus().unregister(this);
        if (Files.isRegularFile(Paths.get("saves", "Cloud9", "runner_setup.json"))) {
            game.launchIntegratedServer("Cloud9", "Cloud 9", null);
        }
    }
}
