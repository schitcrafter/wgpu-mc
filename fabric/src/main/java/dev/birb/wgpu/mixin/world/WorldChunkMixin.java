package dev.birb.wgpu.mixin.world;

import dev.birb.wgpu.mixin.accessors.PackedIntegerArrayAccessor;
import dev.birb.wgpu.palette.RustPalette;
import dev.birb.wgpu.render.Wgpu;
import dev.birb.wgpu.rust.WgpuNative;
import dev.birb.wgpu.rust.WmChunk;
import net.minecraft.block.BlockState;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.world.ClientWorld;
import net.minecraft.nbt.NbtCompound;
import net.minecraft.network.PacketByteBuf;
import net.minecraft.network.packet.s2c.play.ChunkData;
import net.minecraft.util.collection.PackedIntegerArray;
import net.minecraft.util.collection.PaletteStorage;
import net.minecraft.util.math.BlockPos;
import net.minecraft.util.math.ChunkPos;
import net.minecraft.world.World;
import net.minecraft.world.chunk.Chunk;
import net.minecraft.world.chunk.WorldChunk;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

import java.util.function.Consumer;

@Mixin(WorldChunk.class)
public abstract class WorldChunkMixin {

    @Inject(method = "loadFromPacket", at = @At("RETURN"))
    public void loadFromPacket(PacketByteBuf buf, NbtCompound nbt, Consumer<ChunkData.BlockEntityVisitor> consumer, CallbackInfo ci) {
        WmChunk chunk = new WmChunk((WorldChunk) (Object) this);
        try {
            long time1 = System.currentTimeMillis();
            chunk.upload();
            long time2 = System.currentTimeMillis();
            chunk.bake();
            long timeNow = System.currentTimeMillis();
            System.out.println("Uploading: " + (time2 - time1) + " bake: " + (timeNow - time2));
        } catch(ClassCastException e) {
            e.printStackTrace();
        }
    }

}
