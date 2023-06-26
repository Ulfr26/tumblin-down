use std::{f32::consts::PI, sync::Arc};

use instant::Instant;

use anyhow::anyhow;
use egui_wgpu::renderer::ScreenDescriptor;
use egui_winit_platform::{Platform, PlatformDescriptor};
use kira::{
    manager::{AudioManager, AudioManagerSettings},
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
};
use rapier3d::{
    na::{Isometry3, Translation3, UnitQuaternion, Vector3},
    prelude::{AngVector, ColliderBuilder, SharedShape},
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    TextureViewDescriptor,
};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, KeyboardInput, VirtualKeyCode, WindowEvent},
    window::Window,
};

use crate::input;
use crate::light;
use crate::{camera::Camera, debug_collider::DebugCollider};
use crate::{
    model::{self, ModelVertex, Vertex},
    resources, texture,
};

const CLEAR_COLOUR: wgpu::Color = wgpu::Color {
    r: 0.5,
    g: 0.82,
    b: 0.98,
    a: 1.0,
};

#[derive(PartialEq)]
pub enum State {
    Loading,
    Playing,
}

pub const SAMPLE_COUNT: u32 = 4;

fn rei_collider() -> rapier3d::prelude::Collider {
    let head_shape = SharedShape::round_cylinder(0.4, 0.95, 0.5);
    let body_shape = SharedShape::capsule_y(0.7, 0.65);

    let head_trans = Isometry3::from_parts(
        Translation3::new(0.0, 1.1, 0.0),
        UnitQuaternion::new(Vector3::x() * PI / 2.0),
    );
    let body_trans = Isometry3::translation(0.0, 3.35, -0.1);

    ColliderBuilder::compound(vec![(head_trans, head_shape), (body_trans, body_shape)]).build()
}

pub struct App {
    // WGPU stuff
    surface: wgpu::Surface,
    config: wgpu::SurfaceConfiguration,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    size: PhysicalSize<u32>,
    window: Window,
    pipeline: wgpu::RenderPipeline,
    depth_texture: texture::Texture,
    msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    // The rest of the app
    // Since this is so simple there's not really much
    //
    // ...
    // This was a comment from a simpler time
    keyboard: input::KeyboardWatcher,
    pub state: State,

    pub rei_model: Option<model::Model>,
    pub light_model: Option<model::Model>,
    camera: Camera,

    light_uniform: light::LightUniform,
    light_buffer: wgpu::Buffer,
    light_bind_group: wgpu::BindGroup,
    light_pipeline: wgpu::RenderPipeline,

    // Audio
    pub song: Option<StaticSoundData>,
    song_handle: Option<StaticSoundHandle>,
    audio_manager: Option<AudioManager>,

    // Egui stuff
    pub egui_platform: Platform,
    egui_renderer: egui_wgpu::Renderer,
    start_time: Instant,

    // Colliders (will remove this eventually)
    body_collider: DebugCollider,
    head_collider: DebugCollider,
    collider_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    show_colliders: bool,
}

fn create_render_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    colour_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    vertex_layouts: &[wgpu::VertexBufferLayout],
    shader: &wgpu::ShaderModule,
    samples: u32,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_main",
            buffers: vertex_layouts,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: colour_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            // Setting this to anything other than Fill requires Features::NON_FILL_POLYGON_MODE
            polygon_mode: wgpu::PolygonMode::Fill,
            // Requires Features::DEPTH_CLIP_CONTROL
            unclipped_depth: false,
            // Requires Features::CONSERVATIVE_RASTERIZATION
            conservative: false,
        },
        depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        },
        multiview: None,
    })
}

impl App {
    pub async fn new(window: Window) -> anyhow::Result<Self> {
        // --- RENDERER CODE ---
        // A lot of this instantiation boilerplate (as well as a lot of the
        // code, to be fair) was taken from the wgpu tutorial at
        // https://sotrh.github.io/learn-wgpu/
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            dx12_shader_compiler: Default::default(),
        });

        // SAFETY: surface should live as long as the window as they are both
        // owned by the same struct. I'm pretty sure. That's what they said
        // on the tutorial. But aren't self referential structs generally
        // unsafe?
        let surface = unsafe { instance.create_surface(&window) }?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: Default::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(anyhow!("Error requesting wgpu adapter."))?;

        log::info!("Backend: {:?}", adapter.get_info().backend);

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    features: wgpu::Features::empty(),
                    limits: if cfg!(target_arch = "wasm32") {
                        wgpu::Limits::downlevel_webgl2_defaults()
                    } else {
                        wgpu::Limits::default()
                    },
                },
                None, /*trace_path*/
            )
            .await?;

        let surface_capabilities = surface.get_capabilities(&adapter);

        let format = surface_capabilities
            .formats
            .iter()
            .copied()
            .find(|f| f.describe().srgb)
            .unwrap_or(surface_capabilities.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: surface_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &config);

        let camera = Camera::new(
            &device,
            &queue,
            (0.0, 2.0, 6.0).into(),
            config.width as f32 / config.height as f32,
        );

        let light_uniform = light::LightUniform::new([2.0, 3.0, 2.0], [0.96, 0.68, 1.0]);

        let light_buffer = device.create_buffer_init(&BufferInitDescriptor {
            label: Some("Light buffer"),
            contents: bytemuck::cast_slice(&[light_uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let light_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("light bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light bind group"),
            layout: &light_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: light_buffer.as_entire_binding(),
            }],
        });

        let camera_bind_group_layout = Camera::bind_group_layout(&device);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout descriptor"),
            bind_group_layouts: &[
                camera_bind_group_layout,
                texture::Texture::texture_bind_group_layout(&device),
                &light_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("model shader"),
            source: wgpu::ShaderSource::Wgsl(
                resources::load_string("shaders/model_shader.wgsl")
                    .await?
                    .into(),
            ),
        });

        let depth_texture =
            texture::Texture::create_depth_texture(&device, &config, "depth texture");

        let pipeline = create_render_pipeline(
            &device,
            "render pipeline",
            &pipeline_layout,
            config.format,
            Some(texture::Texture::DEPTH_FORMAT),
            &[ModelVertex::desc()],
            &shader,
            SAMPLE_COUNT,
        );

        let light_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Light shader"),
            source: wgpu::ShaderSource::Wgsl(
                resources::load_string("shaders/light_shader.wgsl")
                    .await?
                    .into(),
            ),
        });

        let light_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Light pipeline layout"),
                bind_group_layouts: &[camera_bind_group_layout, &light_bind_group_layout],
                push_constant_ranges: &[],
            });

        let light_pipeline = create_render_pipeline(
            &device,
            "light pipeline",
            &light_pipeline_layout,
            config.format,
            Some(texture::Texture::DEPTH_FORMAT),
            &[ModelVertex::desc()],
            &light_shader,
            SAMPLE_COUNT,
        );

        let collider_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("collider shader"),
            source: wgpu::ShaderSource::Wgsl(
                resources::load_string("shaders/collider_debug_shader.wgsl")
                    .await?
                    .into(),
            ),
        });

        let collider_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Collider pipeline layout"),
                bind_group_layouts: &[camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let collider_pipeline = create_render_pipeline(
            &device,
            "collider pipeline",
            &collider_pipeline_layout,
            config.format,
            //None,
            Some(texture::Texture::DEPTH_FORMAT),
            &[DebugCollider::vertex_desc(), model::InstanceRaw::desc()],
            &collider_shader,
            SAMPLE_COUNT,
        );

        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline shader"),
            source: wgpu::ShaderSource::Wgsl(
                resources::load_string("shaders/collider_outline_shader.wgsl")
                    .await?
                    .into(),
            ),
        });

        let outline_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("outline pipeline layout"),
                bind_group_layouts: &[camera_bind_group_layout],
                push_constant_ranges: &[],
            });

        let outline_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Outline pipeline"),
            layout: Some(&outline_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &outline_shader,
                entry_point: "vs_main",
                buffers: &[DebugCollider::vertex_desc(), model::InstanceRaw::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &outline_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                // Setting this to anything other than Fill requires Features::NON_FILL_POLYGON_MODE
                polygon_mode: wgpu::PolygonMode::Fill,
                // Requires Features::DEPTH_CLIP_CONTROL
                unclipped_depth: false,
                // Requires Features::CONSERVATIVE_RASTERIZATION
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: texture::Texture::DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLE_COUNT,
                ..Default::default()
            },
            multiview: None,
        });

        let msaa_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("msaa texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            mip_level_count: 1,
            view_formats: &[],
        });

        let msaa_view = msaa_texture.create_view(&TextureViewDescriptor::default());

        let egui_platform = Platform::new(PlatformDescriptor {
            physical_width: size.width,
            physical_height: size.height,
            scale_factor: window.scale_factor(),
            ..Default::default()
        });

        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            Some(texture::Texture::DEPTH_FORMAT),
            SAMPLE_COUNT,
        );

        let body_collider = ColliderBuilder::capsule_y(0.7, 0.65)
            .translation(Vector3::new(0.0, 1.1, 0.0))
            .build();

        let head_collider = ColliderBuilder::round_cylinder(0.4, 0.95, 0.5)
            .rotation(AngVector::new(3.14 / 2.0, 0.0, 0.0))
            .translation(Vector3::new(0.0, 3.35, -0.1))
            .build();

        let body_collider = DebugCollider::new_capsule(&device, body_collider);
        let head_collider = DebugCollider::new_round_cylinder(&device, head_collider);

        Ok(Self {
            surface,
            config,
            device: Arc::new(device),
            queue: Arc::new(queue),
            size,
            window,
            pipeline,
            depth_texture,
            rei_model: None,
            light_model: None,
            camera,
            msaa_texture,
            msaa_view,

            keyboard: input::KeyboardWatcher::new(),
            song: None,
            song_handle: None,
            audio_manager: None,
            light_uniform,
            light_buffer,
            light_bind_group,
            light_pipeline,

            state: State::Loading,
            egui_platform,
            egui_renderer,
            start_time: Instant::now(),
            body_collider,
            head_collider,
            collider_pipeline,
            outline_pipeline,
            show_colliders: false,
        })
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        match self.state {
            State::Loading => self.render_loading(),
            State::Playing => self.render_loaded(),
        }
    }

    pub fn render_loading(&mut self) -> Result<(), wgpu::SurfaceError> {
        // TODO: Loading screen
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(&view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                    store: true,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_texture.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: true,
                }),
                stencil_ops: None,
            }),
        });

        drop(render_pass);

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    pub fn render_loaded(&mut self) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&Default::default());

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: self.window.scale_factor() as f32,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        // Egui setup
        self.egui_platform
            .update_time(self.start_time.elapsed().as_secs_f64());
        self.egui_platform.begin_frame();

        self.ui(&self.egui_platform.context());

        let full_output = self.egui_platform.end_frame(Some(&self.window));
        let paint_jobs = self.egui_platform.context().tessellate(full_output.shapes);
        let textures_delta = full_output.textures_delta;

        for texture in textures_delta.free.iter() {
            self.egui_renderer.free_texture(texture);
        }

        for (id, image_delta) in textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, id, &image_delta);
        }

        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa_view,
                resolve_target: Some(&view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_COLOUR),
                    store: true,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_texture.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: true,
                }),
                stencil_ops: None,
            }),
        });

        // Light Model
        let light_model = self.light_model.as_ref().unwrap();
        render_pass.set_pipeline(&self.light_pipeline);
        render_pass.set_bind_group(0, &self.camera.bind_group, &[]);
        render_pass.set_bind_group(1, &self.light_bind_group, &[]);
        render_pass.set_vertex_buffer(0, light_model.meshes[0].vertex_buffer.slice(..));
        render_pass.set_index_buffer(
            light_model.meshes[0].index_buffer.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        render_pass.draw_indexed(0..light_model.meshes[0].num_indices as _, 0, 0..1);

        // Rei
        render_pass.set_pipeline(&self.pipeline);
        //render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        render_pass.set_bind_group(2, &self.light_bind_group, &[]);

        let rei_model = self.rei_model.as_ref().unwrap();

        for mesh in rei_model.meshes.iter() {
            let material = &rei_model.materials[mesh.material.unwrap()];

            render_pass.set_bind_group(1, material.diffuse_bind_group.as_ref().unwrap(), &[]);
            render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..mesh.num_indices, 0, 0..1);
        }

        if self.show_colliders {
            render_pass.set_pipeline(&self.outline_pipeline);
            self.body_collider.draw_outline(&mut render_pass);
            self.head_collider.draw_outline(&mut render_pass);

            render_pass.set_pipeline(&self.collider_pipeline);

            self.body_collider.draw(&mut render_pass);
            self.head_collider.draw(&mut render_pass);
        }

        // Egui draw
        self.egui_renderer
            .render(&mut render_pass, &paint_jobs, &screen_descriptor);

        drop(render_pass);

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    fn ui(&mut self, ctx: &egui::Context) {
        egui::Window::new("Hello world!").show(ctx, |ui| {
            ui.label("holy guacamole");

            ui.horizontal(|ui| {
                ui.label("Light colour: ");
                let mut hsva = egui::epaint::Hsva::from_rgb(self.light_uniform.colour);

                ui.color_edit_button_hsva(&mut hsva);

                self.light_uniform.colour = hsva.to_rgb();
            });

            ui.checkbox(&mut self.show_colliders, "Show colliders");
        });
    }

    pub fn process_input(&mut self, event: &WindowEvent) -> bool {
        self.keyboard.process_input(event);
        match event {
            WindowEvent::KeyboardInput {
                input:
                    KeyboardInput {
                        state: ElementState::Pressed,
                        virtual_keycode: Some(VirtualKeyCode::H),
                        ..
                    },
                ..
            } => {
                log::info!("hiii!!!! :3");
                true
            }

            _ => false,
        }
    }

    pub fn update(&mut self) {
        self.light_uniform.update();
        self.queue.write_buffer(
            &self.light_buffer,
            0,
            bytemuck::cast_slice(&[self.light_uniform]),
        );

        self.camera.update(&self.queue, &self.keyboard);

        self.body_collider.update_capsule(&self.queue);
        self.head_collider
            .update_round_cylinder(&self.device, &self.queue);
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
            self.depth_texture =
                texture::Texture::create_depth_texture(&self.device, &self.config, "depth texture");

            self.msaa_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("msaa texture"),
                size: wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
                sample_count: SAMPLE_COUNT,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                mip_level_count: 1,
                view_formats: &[],
            });

            self.msaa_view = self
                .msaa_texture
                .create_view(&TextureViewDescriptor::default());
        }
    }

    pub fn size(&self) -> &PhysicalSize<u32> {
        &self.size
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn play_music(&mut self) {
        if self.audio_manager.is_none() {
            self.audio_manager = AudioManager::new(AudioManagerSettings::default()).ok();
        }
        self.song_handle = self
            .audio_manager
            .as_mut()
            .unwrap()
            .play(self.song.as_ref().unwrap().clone())
            .ok();
    }

    pub fn song_handle_mut(&mut self) -> Option<&mut StaticSoundHandle> {
        self.song_handle.as_mut()
    }
}
