#![feature(once_cell)]
#![feature(mixed_integer_ops)]

extern crate core;

use core::slice;
use std::{fs, thread};
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::sync::mpsc::{channel, RecvError, Sender};
use std::time::Instant;

use arc_swap::{ArcSwap, AsRaw};
use cgmath::{Matrix4, Ortho, SquareMatrix, Vector3};
use dashmap::DashMap;
use futures::executor::block_on;
use jni::{JavaVM, JNIEnv};
use jni::errors::Error;
use jni::objects::{JByteBuffer, JClass, JObject, JString, JValue, ReleaseMode};
use jni::sys::{jboolean, jbyteArray, jint, jintArray, jobject, jobjectArray, jstring, jlong, jfloat, jfloatArray, jbyte};
use parking_lot::{Mutex, RwLock};
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Event, KeyboardInput, VirtualKeyCode, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

use gl::pipeline::{GLCommand, GlPipeline};
use wgpu_mc::{HasWindowSize, WindowSize, WmRenderer};
use wgpu_mc::mc::block::{Block, BlockDirection, BlockState};
use wgpu_mc::mc::BlockEntry;
use wgpu_mc::mc::chunk::{Chunk, CHUNK_HEIGHT, CHUNK_VOLUME};
use wgpu_mc::mc::datapack::{NamespacedResource, TextureVariableOrResource};
use wgpu_mc::mc::resource::ResourceProvider;
use wgpu_mc::model::BindableTexture;
use wgpu_mc::render::world::chunk::BakedChunk;
use wgpu_mc::render::pipeline::WmPipeline;
use wgpu_mc::texture::TextureSamplerView;

use wgpu_mc::wgpu;

use crate::mc_interface::xyz_to_index;
use std::cell::{RefCell, Cell};
use std::io::Cursor;
use wgpu::{RenderPipelineDescriptor, PipelineLayoutDescriptor, TextureDescriptor, Extent3d, ImageDataLayout, BindGroupDescriptor, BindGroupEntry, BindingResource, TextureViewDescriptor};
use crate::gl::{GL_ALLOC, GL_COMMANDS, GlResource, GlTexture};
use wgpu::util::{DeviceExt, BufferInitDescriptor};
use std::num::NonZeroU32;
use std::rc::Rc;
use byteorder::{LittleEndian, ReadBytesExt};
use wgpu_mc::camera::UniformMatrixHelper;
use once_cell::sync::OnceCell;
use wgpu_mc::render::pipeline::terrain::TerrainPipeline;
use wgpu_mc::render::shader::WmShader;
use wgpu_mc::render::shader::GlslShader;
use crate::gl::pipeline::GlPipelineState;

//SAFETY: It is assumed that this FFI will be accessed only single-threadedly

mod mc_interface;
mod gl;
mod palette;

enum RenderMessage {
    SetTitle(String),
    Task(Box<dyn FnOnce() -> ()>)
}

struct MinecraftRenderState {
    //draw_queue: Vec<>,
    render_world: bool
}

static mut RENDERER: OnceCell<WmRenderer> = OnceCell::new();

static mut EVENT_LOOP: OnceCell<EventLoop<()>> = OnceCell::new();
static mut WINDOW: OnceCell<Window> = OnceCell::new();

static mut CHANNELS: OnceCell<(Mutex<mpsc::Sender<RenderMessage>>, Mutex<mpsc::Receiver<RenderMessage>>)> = OnceCell::new();

static mut MC_STATE: OnceCell<RwLock<MinecraftRenderState>> = OnceCell::new();
static mut GL_PIPELINE: OnceCell<GlPipeline> = OnceCell::new();

struct WinitWindowWrapper<'a> {
    window: &'a Window
}

impl HasWindowSize for WinitWindowWrapper<'_> {
    fn get_window_size(&self) -> WindowSize {
        WindowSize {
            width: self.window.inner_size().width,
            height: self.window.inner_size().height,
        }
    }
}

unsafe impl HasRawWindowHandle for WinitWindowWrapper<'_> {

    fn raw_window_handle(&self) -> RawWindowHandle {
        self.window.raw_window_handle()
    }

}

struct JarShaderProvider {

}

struct MinecraftResourceManagerAdapter {
    jvm: JavaVM
}

impl ResourceProvider for MinecraftResourceManagerAdapter {
    fn get_resource(&self, id: &NamespacedResource) -> Vec<u8> {
        let env = self.jvm.attach_current_thread()
            .unwrap();

        let ident_ns = env.new_string(&id.0).unwrap();
        let ident_path = env.new_string(&id.1).unwrap();

        //Could just simplify this to a formatted string
        let bytes = match env.call_static_method(
            "dev/birb/wgpu/rust/WgpuResourceProvider",
            "getResource",
            "(Ljava/lang/String;Ljava/lang/String;)[B",
            &[
                JValue::Object(ident_ns.into()),
                JValue::Object(ident_path.into())
            ]) {
            Ok(jvalue) => jvalue.l().unwrap(),
            Err(e) => panic!("{:?}\nID {}", e, id)
        };

        let elements = env.get_byte_array_elements(bytes.into_inner(), ReleaseMode::NoCopyBack)
            .unwrap();

        let mut vec = vec![0u8; elements.size().unwrap() as usize];
        vec.copy_from_slice(
            unsafe {
                std::slice::from_raw_parts(elements.as_ptr() as *const u8, elements.size().unwrap() as usize)
            }
        );

        vec
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_uploadChunk(
    env: JNIEnv,
    class: JClass,
    world_chunk: JObject) {

    mc_interface::chunk_from_java_world_chunk(&env, &world_chunk);

}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_getBackend(
    env: JNIEnv,
    class: JClass) -> jstring {

    let renderer = unsafe { RENDERER.get().unwrap() };
    let backend = renderer.get_backend_description();

    env.new_string(backend)
         .unwrap()
        .into_inner()

}

// #[no_mangle]
// pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_registerSprite(
//     env: JNIEnv,
//     class: JClass,
//     sprite_type: JString,
//     jnamespace: JString,
//     buffer: jbyteArray) {
//
//     let renderer = unsafe { RENDERER.get().unwrap() };
//
//
//
//     let namespace_str: String = env.get_string(jnamespace).unwrap().into();
//     let sprite_type: String = env.get_string(sprite_type).unwrap().into();
//     let identifier = NamespacedResource::try_from(namespace_str.as_str())
//         .unwrap();
//
//     let buffer_arr = env.get_byte_array_elements(
//         buffer,
//         ReleaseMode::NoCopyBack)
//         .unwrap();
//
//     let buffer_arr_size = buffer_arr.size().unwrap() as usize;
//     let mut buffer: Vec<u8> = vec![0; buffer_arr_size];
//     //Create a slice looking into the buffer so that we can immediately copy the data into a Vec.
//     let slice: &[u8] = unsafe {
//         std::mem::transmute(
//             std::slice::from_raw_parts(
//                 buffer_arr.as_ptr(),
//                 buffer_arr_size)
//         )
//     };
//
//     buffer.copy_from_slice(slice);
//
//     // println!("{:?}", renderer.mc.texture_manager.textures);
//     renderer.mc.texture_manager.textures.insert(identifier, buffer);
// }

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_registerEntry(
    env: JNIEnv,
    class: JClass,
    entry_type: jint,
    name: JString) {

    let rname: String = env.get_string(name).unwrap().into();

    let renderer = unsafe { RENDERER.get_mut().unwrap() };

    let mut block_manager = renderer.mc.block_manager.write();

    match entry_type {
        0 => {
            let identifier = NamespacedResource::try_from(&rname[..]).unwrap();

            let resource = renderer.mc.resource_provider.get_resource(
                &identifier.prepend("blockstates/").append(".json")
            );
            let json_str = std::str::from_utf8(&resource).unwrap();
            let model = Block::from_json(
                &identifier.to_string(),
                json_str
            ).or_else(|| {
                Block::from_json(
                    &identifier.to_string(),
                    "{\"variants\":{\"\": {\"model\": \"minecraft:block/bedrock\"}}}"
                )
            }).unwrap();
            block_manager.blocks.insert(identifier, model);
        },
        _ => unimplemented!()
    };

}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_initialize(
    env: JNIEnv,
    class: JClass,
    string: JString
) {

    use winit::event_loop::EventLoop;

    let title: String = env.get_string(string).unwrap().into();

    let event_loop = EventLoop::new();

    let window = winit::window::WindowBuilder::new()
        .with_title(&format!("{}", title))
        .with_inner_size(winit::dpi::Size::Physical(PhysicalSize {
            width: 1280,
            height: 720
        }))
        .build(&event_loop)
        .unwrap();

    unsafe {
        gl::init();
        GL_PIPELINE.set(GlPipeline {
            state: RefCell::new(GlPipelineState {
                matrix_stacks: [([Matrix4::identity(); 32], 0); 3],
                matrix_mode: 0,
                // vertex_attributes: RefCell::new(HashMap::new()),
                active_texture_unit: 0,
                texture_units: HashMap::new(),
                vertex_array: None,
                client_states: Vec::new(),
                // shaders: RefCell::new(None)
                active_pipeline: 0
            }),
            commands: ArcSwap::new(Arc::new(Vec::new()))
        });
        CHANNELS.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<RenderMessage>();
            (Mutex::new(tx), Mutex::new(rx))
        });
        EVENT_LOOP.set(event_loop);
        WINDOW.set(window);
        MC_STATE.set(RwLock::new(MinecraftRenderState {
            render_world: false
        }));
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_doEventLoop(
    env: JNIEnv,
    class: JClass) {

    let renderer = unsafe { RENDERER.get_mut().unwrap() };
    let event_loop = unsafe { EVENT_LOOP.take().unwrap() };

    let (_, rx) = unsafe { CHANNELS.get().unwrap() };
    let rx = rx.lock();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Poll;
        let mc_state = unsafe { MC_STATE.get().unwrap() }.read();

        let (mut state, window) = unsafe {
            (
                RENDERER.get_mut().unwrap(),
                WINDOW.get().unwrap()
            )
        };

        let mut msg_r = rx.try_recv();
        while msg_r.is_ok() {
            let msg = msg_r.unwrap();

            match msg {
                RenderMessage::SetTitle(title) => window.set_title(&title),
                RenderMessage::Task(func) => func()
            };

            msg_r = rx.try_recv();
        }

        match event {
            Event::MainEventsCleared => window.request_redraw(),
            Event::WindowEvent {
                ref event,
                window_id,
            } if window_id == window.id() => {
                match event {
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    WindowEvent::Resized(physical_size) => {
                        &state.resize(WindowSize {
                            width: physical_size.width,
                            height: physical_size.height
                        });
                    }
                    WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                        &state.resize(WindowSize {
                            width: new_inner_size.width,
                            height: new_inner_size.height
                        });
                    }
                    _ => {}
                }
            }
            Event::RedrawRequested(_) => {
                state.update();

                state.render(&[
                    unsafe { GL_PIPELINE.get().unwrap() }
                ]);

                // let delta = Instant::now().duration_since(frame_begin).as_millis()+1; //+1 so we don't divide by zero
                // frame_begin = Instant::now();

                // println!("Frametime {}, FPS {}", delta, 1000/delta);
            }
            _ => {}
        }
    });

}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_initRenderer(
    env: JNIEnv,
    class: JClass
) {
    let wrapper = &WinitWindowWrapper {
        window: unsafe { WINDOW.get().unwrap() }
    };

    let wgpu_state = block_on(
        WmRenderer::init_wgpu(wrapper)
    );

    let resource_provider = Arc::new(MinecraftResourceManagerAdapter {
        jvm: env.get_java_vm().unwrap()
    });

    let mut state = WmRenderer::new(
        wgpu_state,
        resource_provider
    );

    state.init(
        &[
            &TerrainPipeline,
            unsafe { &GL_PIPELINE }.get().unwrap()
        ]
    );

    unsafe {
        RENDERER.set(state);
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_digestInputStream(
    env: JNIEnv,
    class: JClass,
    input_stream: JObject) -> jbyteArray {

    let mut vec = Vec::with_capacity(1024);
    let array = env.new_byte_array(1024).unwrap();

    loop {
        let bytes_read = env.call_method(
            input_stream,
            "read",
            "([B)I",
            &[array.into()]).unwrap().i().unwrap();

        //bytes_read being -1 means EOF
        if bytes_read > 0 {
            let elements = env.get_byte_array_elements(array, ReleaseMode::NoCopyBack)
                .unwrap();

            let slice: &[u8] = unsafe {
                std::mem::transmute(
                    std::slice::from_raw_parts(elements.as_ptr(), bytes_read as usize)
                )
            };

            vec.extend_from_slice(slice);
        } else {
            break;
        }
    }


    let bytes = env.new_byte_array(vec.len() as i32).unwrap();
    let bytes_elements = env.get_byte_array_elements(bytes, ReleaseMode::CopyBack).unwrap();

    unsafe {
        std::ptr::copy(vec.as_ptr(), bytes_elements.as_ptr() as *mut u8, vec.len());
    }

    bytes

}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_updateWindowTitle(
    env: JNIEnv,
    class: JClass,
    jtitle: JString) {

    let (tx, _) = unsafe { CHANNELS.get().unwrap() };
    let tx = tx.lock();

    let title: String = env.get_string(jtitle).unwrap().into();

    tx.send(RenderMessage::SetTitle(title));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_bakeBlockModels(
    env: JNIEnv,
    class: JClass) -> jobject {

    let renderer = unsafe { RENDERER.get().unwrap() };

    let block_hashmap = env.new_object("java/util/HashMap", "()V", &[])
        .unwrap();

    // let instant = Instant::now();
    // renderer.mc.bake_blocks(renderer);
    // println!("Baked block models in {}", Instant::now().duration_since(instant).as_millis());

    let instant = Instant::now();
    renderer.mc.block_manager.read().baked_block_variants.iter().for_each(|(identifier, (key, _))| {
        let _integer = env.new_object("java/lang/Integer", "(I)V", &[
            JValue::Int(*key as i32)
        ]).unwrap();

        env.call_method(block_hashmap, "put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;", &[
            JValue::Object(env.new_string(identifier.to_string()).unwrap().into()),
            JValue::Object(_integer.into())
        ]).unwrap();
    });
    println!("Uploaded blocks to java HashMap in {}ms", Instant::now().duration_since(instant).as_millis());

    block_hashmap.into_inner()
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_setWorldRenderState(
    env: JNIEnv,
    class: JClass,
    boolean: jboolean) {

    let render_state = unsafe { MC_STATE.get().unwrap() };
    render_state.write().render_world = boolean != 0;
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_uploadChunkSimple(
    env: JNIEnv,
    class: JClass,
    blocks: jintArray,
    x: jint,
    z: jint) {
    let renderer = unsafe { RENDERER.get().unwrap() };
    
    let elements = env.get_int_array_elements(blocks, ReleaseMode::NoCopyBack)
        .unwrap();
    
    let elements_ptr = elements.as_ptr();
    let blocks: Box<[BlockState; CHUNK_VOLUME]> = (0..elements.size().unwrap()).map(|index| {
        let element_ptr = unsafe { elements_ptr.offset(index as isize) };
        let block_index = unsafe { *element_ptr } as usize;
    
        BlockState {
            packed_key: Some(block_index as u32)
        }
    }).collect::<Box<[BlockState]>>().try_into().unwrap();

    let mut chunk = Chunk::new((x, z), blocks);
    let baked = BakedChunk::bake(
        &*renderer.mc.block_manager.read(),
        &chunk,
        &renderer.wgpu_state.device);

    chunk.baked = Some(baked);
    renderer.mc.chunks.loaded_chunks.insert((x, z), ArcSwap::new(Arc::new(chunk)));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_bindBuffer(
    env: JNIEnv,
    class: JClass,
    target: jint,
    buffer: jint
) {
    let mut state = unsafe { gl::GL_STATE.get_mut().unwrap() };
    state.buffers.insert(target, buffer);

    //TODO: Is this necessary
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::BindBuffer(target, buffer));
}


#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_activeTexture(
    env: JNIEnv,
    class: JClass,
    tex: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::ActiveTexture(tex));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_pushMatrix(
    env: JNIEnv,
    class: JClass
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::PushMatrix);
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_popMatrix(
    env: JNIEnv,
    class: JClass
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::PopMatrix);
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_submitCommands(
    env: JNIEnv,
    class: JClass
) {
    let commands = unsafe { GL_COMMANDS.get_mut().unwrap() };

    // println!("{:?}", commands);

    unsafe { GL_PIPELINE.get_mut().unwrap() }.commands.store(
        Arc::new(
            commands.clone()
        )
    );

    commands.clear();
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_vertexPointer(
    env: JNIEnv,
    class: JClass,
    size: jint,
    vertex_type: jint,
    stride: jint,
    pointer: jlong
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::VertexPointer(size, vertex_type, stride, pointer as *const u8));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_colorPointer(
    env: JNIEnv,
    class: JClass,
    size: jint,
    vertex_type: jint,
    stride: jint,
    pointer: jlong
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::ColorPointer(size, vertex_type, stride, pointer as *const u8));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_texCoordPointer(
    env: JNIEnv,
    class: JClass,
    size: jint,
    vertex_type: jint,
    stride: jint,
    pointer: jlong
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::TexCoordPointer(size, vertex_type, stride, pointer as *const u8));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_drawArray(
    env: JNIEnv,
    class: JClass,
    mode: jint,
    first: jint,
    count: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::DrawArray(mode, first, count));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_texImage2D(
    env: JNIEnv,
    class: JClass,
    texture_id: jint,
    target: jint,
    level: jint,
    internal_format: jint,
    width: jint,
    height: jint,
    border: jint,
    format: jint,
    _type: jint,
    pixels_ptr: jlong
) {
    //For when the renderer is initialized
    let task = move || {
        let area = width * height;
        //In bytes
        assert_eq!(_type, 0x1401);
        let size = area as usize * 4;

        let data = if pixels_ptr != 0 {
            println!("Non null pointer provided for tex image command");
            Vec::from(
                unsafe {
                    std::slice::from_raw_parts(pixels_ptr as *const u8, size)
                }
            )
        } else {
            println!("Null pointer provided for tex image command");
            vec![0; size]
        };

        let wm = unsafe { &RENDERER }.get().unwrap();

        let tsv = TextureSamplerView::from_rgb_bytes(
            &wm.wgpu_state,
            &data[..],
            wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1
            },
            None,
            match format {
                0x1908 => wgpu::TextureFormat::Rgba8Unorm,
                0x80E1 => wgpu::TextureFormat::Bgra8Unorm,
                _ => unimplemented!()
            }
        ).unwrap();

        let bindable = BindableTexture::from_tsv(
            &wm.wgpu_state,
            &wm.render_pipeline_manager.load(),
            tsv,
        );

        unsafe { GL_ALLOC.get_mut() }.unwrap().insert(texture_id, GlResource::Texture(
            GlTexture {
                width: width as u16,
                height: height as u16,
                bindable_texture: Some(Rc::new(bindable))
            },
            data
        ));
    };

    let (tx, _) = unsafe { &CHANNELS }.get_or_init(|| {
        let (tx, rx) = channel();
        (Mutex::new( tx), Mutex::new(rx))
    });

    let tx = tx.lock();
    tx.send(RenderMessage::Task(Box::new(task)));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_subImage2D(
    env: JNIEnv,
    class: JClass,
    texture_id: jint,
    target: jint,
    level: jint,
    offsetX: jint,
    offsetY: jint,
    width: jint,
    height: jint,
    format: jint,
    _type: jint,
    pixels: jlong
) {
    let pixel_size = match format {
        0x1908 | 0x80E1 => 4,
        _ => panic!("Unknown format {:x}", format)
    };

    //For when the renderer is initialized
    let task = move || {
        let area = width * height;
        //In bytes
        assert_eq!(_type, 0x1401);
        let size = area as usize * 4 ;

        let mut pixel_data = if pixels != 0 {
            println!("Non null pointer provided for sub tex image command");
            Vec::from(
                unsafe {
                    std::slice::from_raw_parts(pixels as *const u8, size)
                }
            )
        } else {
            println!("Null pointer provided for sub tex image command");
            vec![0; size]
        };

        let wm = unsafe { &RENDERER }.get().unwrap();

        match unsafe { GL_ALLOC.get_mut() }.unwrap().get_mut(&texture_id).unwrap() {
            GlResource::Texture(gl_texture, data) => {
                let src_row_byte_width = (width * pixel_size);
                let dest_row_byte_width = gl_texture.width as i32 * pixel_size;

                for y in 0..height {
                    let src_begin = src_row_byte_width * y;
                    let src_end = src_begin + src_row_byte_width;
                    let src_slice = &pixel_data[src_begin as usize..src_end as usize];

                    let dest_begin = (dest_row_byte_width * (y + offsetY)) + (offsetX * pixel_size);
                    let dest_end = dest_begin + (width * pixel_size);

                    assert_eq!(dest_end - dest_begin, src_end - src_begin);
                    let dest_slice = &mut data[dest_begin as usize..dest_end as usize];
                    dest_slice.copy_from_slice(src_slice)
                }

                let tsv = TextureSamplerView::from_rgb_bytes(
                    &wm.wgpu_state,
                    &data,
                    Extent3d {
                        width: gl_texture.width as u32,
                        height: gl_texture.height as u32,
                        depth_or_array_layers: 1
                    },
                    None,
                    match format {
                        0x1908 => wgpu::TextureFormat::Rgba8Unorm,
                        0x80E1 => wgpu::TextureFormat::Bgra8Unorm,
                        _ => unimplemented!()
                    }
                ).unwrap();

                let bindable_texture = BindableTexture::from_tsv(
                    &wm.wgpu_state,
                    &wm.render_pipeline_manager.load(),
                    tsv
                );

                gl_texture.bindable_texture = Some(Rc::new(bindable_texture));
            }
            GlResource::Buffer(_) => panic!("Invalid texture")
        }
    };

    let (tx, _) = unsafe { &CHANNELS }.get_or_init(|| {
        let (tx, rx) = channel();
        (Mutex::new( tx), Mutex::new(rx))
    });

    let tx = tx.lock();
    tx.send(RenderMessage::Task(Box::new(task)));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_getMaxTextureSize(
    env: JNIEnv,
    class: JClass,
) -> jint {
    let wm = unsafe { RENDERER.get().unwrap() };
    wm.wgpu_state.adapter.limits().max_texture_dimension_2d as i32
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_getWindowWidth(
    env: JNIEnv,
    class: JClass,
) -> jint {
    unsafe { WINDOW.get().unwrap().inner_size().width as i32 }
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_getWindowHeight(
    env: JNIEnv,
    class: JClass,
) -> jint {
    unsafe { WINDOW.get().unwrap().inner_size().height as i32 }
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_enableClientState(
    env: JNIEnv,
    class: JClass,
    int: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::EnableClientState(int as u32));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_disableClientState(
    env: JNIEnv,
    class: JClass,
    int: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::DisableClientState(int as u32));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_ortho(
    env: JNIEnv,
    class: JClass,
    l: jfloat,
    r: jfloat,
    b: jfloat,
    t: jfloat,
    n: jfloat,
    f: jfloat
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::MultMatrix(
        cgmath::ortho(l,r,b,t,n, f)
    ));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_translate(
    env: JNIEnv,
    class: JClass,
    x: jfloat,
    y: jfloat,
    z: jfloat
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::MultMatrix(
        Matrix4::from_translation(Vector3::new(x,y,z))
    ));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_loadIdentity(
    env: JNIEnv,
    class: JClass,
    x: jfloat,
    y: jfloat,
    z: jfloat
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::SetMatrix(
        Matrix4::identity()
    ));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_matrixMode(
    env: JNIEnv,
    class: JClass,
    mode: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::MatrixMode(mode as usize));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_texSubImage2D(
    env: JNIEnv,
    class: JClass,
    target: jint,
    level: jint,
    offsetX: jint,
    offsetY: jint,
    width: jint,
    height: jint,
    format: jint,
    gtype: jint,
    pixels: jlong) {



}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_bindVertexArray(
    env: JNIEnv,
    class: JClass,
    arr: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::BindVertexArray(arr));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_drawElements(
    env: JNIEnv,
    class: JClass,
    mode: jint,
    first: jint,
    type_: jint,
    indices: jlong
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::DrawElements(mode, first, type_, indices as *const u8));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_clearColor(
    env: JNIEnv,
    class: JClass,
    r: jfloat,
    g: jfloat,
    b: jfloat
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::ClearColor(r, g, b));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_attachTextureBindGroup(
    env: JNIEnv,
    class: JClass,
    id: i32
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::AttachTexture(id));
}

// #[no_mangle]
// pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_bufferData(
//     env: JNIEnv,
//     class: JClass,
//     buf: JObject,
//     target: jint,
//     usage: jint
// ) {
//     let wm = unsafe { RENDERER.get().unwrap() };
//     let data = env.get_direct_buffer_address(buf.into()).unwrap();
//     let vec = Vec::from(data);
//
//     let state = unsafe { gl::GL_STATE.get_mut().unwrap() };
//     let active_buffer = *state.buffers.get(&target).unwrap();
//
//     unsafe {
//         gl::upload_buffer_data(
//             active_buffer as usize,
//             &vec[..],
//             &wm.wgpu_state.device
//         );
//     }
// }

// #[no_mangle]
// pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_initBuffer(
//     env: JNIEnv,
//     class: JClass,
//     target: jint,
//     size: jlong,
//     usage: jint
// ) {
//     let wm = unsafe { RENDERER.get().unwrap() };
//
//     let state = unsafe { gl::GL_STATE.get_mut().unwrap() };
//     let active_buffer = *state.buffers.get(&target).unwrap();
//     let vec = vec![0u8; size as usize];
//
//     unsafe {
//         gl::upload_buffer_data(
//             active_buffer as usize,
//             &vec[..],
//             &wm.wgpu_state.device
//         );
//     }
// }

//Awful code
// #[no_mangle]
// pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_mapBuffer(
//     env: JNIEnv,
//     class: JClass,
//     target: jint,
//     usage: jint
// ) -> jobject {
//     // let wm = unsafe { RENDERER.get().unwrap() };
//     let state = unsafe { gl::GL_STATE.get_mut().unwrap() };
//     let active_buffer = *state.buffers.get(&target).unwrap();
//
//     let buf = unsafe { gl::get_buffer(active_buffer as usize) }.unwrap();
//     let maps = unsafe { gl::GL_MAPPED_BUFFERS.get_mut().unwrap() };
//
//     if !maps.contains_key(&(target as usize)) {
//         maps.insert(active_buffer as usize, buf.data.as_ref().unwrap().clone());
//     } else {
//         panic!("Buffer {} already mapped!", target);
//     }
//     let mut reference = maps.get_mut(&(active_buffer as usize)).unwrap();
//     env.new_direct_byte_buffer(&mut reference[..]).unwrap().as_raw()
// }

// #[no_mangle]
// pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_unmapBuffer(
//     env: JNIEnv,
//     class: JClass,
//     target: jint,
// ) {
//     let wm = unsafe { RENDERER.get().unwrap() };
//     let state = unsafe { gl::GL_STATE.get_mut().unwrap() };
//     let active_buffer = state.buffers.remove(&target).unwrap();
//     let mapped = unsafe { gl::GL_MAPPED_BUFFERS.get_mut().unwrap() };
//     let vec = mapped.remove(&(active_buffer as usize)).unwrap();
//
//     unsafe {
//         gl::upload_buffer_data(
//             active_buffer as usize,
//             &vec[..],
//             &wm.wgpu_state.device
//         );
//     }
// }

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_wmUsePipeline(
    env: JNIEnv,
    class: JClass,
    pipeline: jint,
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::UsePipeline(pipeline as usize));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_bindMatrix4f(
    env: JNIEnv,
    class: JClass,
    slot: jint,
    floatarr: JObject
) {
    let elem = env.get_float_array_elements(floatarr.as_raw(), ReleaseMode::NoCopyBack)
        .unwrap();

    assert_eq!(elem.size().unwrap(), 16);

    let float_slice = unsafe {
        std::slice::from_raw_parts(elem.as_ptr(), 16)
    };

    let mat_helper: &UniformMatrixHelper = bytemuck::from_bytes(
        bytemuck::cast_slice::<_, u8>(float_slice)
    );

    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    commands.push(GLCommand::BindMat(slot as usize, Matrix4::from(
        Ortho {
            left: 0.0,
            right: 1280.0,
            bottom: 720.0,
            top: 0.0,
            near: 0.0,
            far: 10000.0
        }
    )));
    // commands.push(GLCommand::BindMat(slot as usize, mat_helper.view_proj.into()));
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_createArrayPaletteStore(
    env: JNIEnv,
    class: JClass
) -> jlong {
    0
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_setProjectionMatrix(
    env: JNIEnv,
    class: JClass,
    float_array: jfloatArray
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    let elements = env.get_float_array_elements(float_array, ReleaseMode::NoCopyBack)
        .unwrap();

    let slice = unsafe {
        slice::from_raw_parts(elements.as_ptr() as *mut f32, elements.size().unwrap() as usize)
    };

    let mut cursor = Cursor::new(bytemuck::cast_slice::<f32, u8>(slice));
    let mut converted = Vec::with_capacity(slice.len());

    for _ in 0..slice.len() {
        use byteorder::ByteOrder;
        converted.push(cursor.read_f32::<LittleEndian>().unwrap());
    }

    let slice_4x4: [[f32; 4]; 4] = *bytemuck::from_bytes(
        bytemuck::cast_slice(&converted)
    );

    let matrix = Matrix4::from_translation(
        Vector3::new(0.0, 0.0, 2.1)
    ) * Matrix4::from(slice_4x4);

    commands.push(
        GLCommand::SetMatrix(matrix)
    );
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_drawIndexed(
    env: JNIEnv,
    class: JClass,
    count: jint
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };

    commands.push(
        GLCommand::DrawIndexed(count as u32)
    );
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_setVertexBuffer(
    env: JNIEnv,
    class: JClass,
    byte_array: jbyteArray
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };

    let mut bytes = vec![0; env.get_array_length(byte_array).unwrap() as usize];
    env.get_byte_array_region(byte_array, 0, &mut bytes[..])
        .unwrap();

    let byte_slice = bytemuck::cast_slice(&bytes);
    let mut cursor = Cursor::new(byte_slice);
    let mut converted = Vec::with_capacity(bytes.len() / 4);

    assert_eq!(bytes.len() % 4, 0);

    for _ in 0..bytes.len() / 4 {
        use byteorder::ByteOrder;
        converted.push(cursor.read_f32::<LittleEndian>().unwrap());
    }

    commands.push(
        GLCommand::SetVertexBuffer(Vec::from(bytemuck::cast_slice(&converted)))
    );
}

#[no_mangle]
pub extern "system" fn Java_dev_birb_wgpu_rust_WgpuNative_setIndexBuffer(
    env: JNIEnv,
    class: JClass,
    int_array: jintArray
) {
    let commands = unsafe { gl::GL_COMMANDS.get_mut().unwrap() };
    let elements = env.get_int_array_elements(int_array, ReleaseMode::NoCopyBack)
        .unwrap();

    let slice = unsafe {
        slice::from_raw_parts(elements.as_ptr() as *mut u32, elements.size().unwrap() as usize)
    };

    commands.push(
        GLCommand::SetIndexBuffer(Vec::from(slice))
    );
}

