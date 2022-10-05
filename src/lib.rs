//! There's not much documentation yet. Check out
//! [the examples](https://github.com/jabuwu/bevy_spine/tree/main/examples) and the
//! [rusty_spine docs](https://docs.rs/rusty_spine/0.3.0)

use std::{
    collections::VecDeque,
    f32::EPSILON,
    mem::take,
    sync::{Arc, Mutex},
};

use bevy::{
    prelude::*,
    render::{
        mesh::{Indices, MeshVertexAttribute},
        render_resource::{PrimitiveTopology, VertexFormat},
    },
    sprite::{Material2dPlugin, Mesh2dHandle},
};
use materials::{
    SpineAdditiveMaterial, SpineAdditivePmaMaterial, SpineMultiplyMaterial,
    SpineMultiplyPmaMaterial, SpineNormalMaterial, SpineNormalPmaMaterial, SpineScreenMaterial,
    SpineScreenPmaMaterial, SpineShader,
};
use rusty_spine::{BlendMode, Skeleton};

use crate::{
    assets::{AtlasLoader, SkeletonJsonLoader},
    entity_sync::{spine_sync_bones, spine_sync_entities, spine_sync_entities_applied},
    rusty::{
        draw::CullDirection, AnimationStateData, BoneHandle, EventType, SkeletonControllerSettings,
    },
    textures::SpineTextures,
};

pub use assets::*;
pub use crossfades::Crossfades;
pub use rusty_spine as rusty;
pub use rusty_spine::SkeletonController;
pub use textures::SpineTexture;

#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemLabel)]
pub enum SpineSystem {
    Load,
    Update,
    SyncEntities,
    SyncBones,
    SyncEntitiesApplied,
    Render,
}

pub struct SpinePlugin;

impl Plugin for SpinePlugin {
    fn build(&self, app: &mut App) {
        {
            let mut shaders = app.world.resource_mut::<Assets<Shader>>();
            SpineShader::set(
                shaders.add(Shader::from_wgsl(include_str!("./shader.wgsl"))),
                shaders.add(Shader::from_wgsl(include_str!("./shader_pma.wgsl"))),
            );
        }
        app.add_plugin(Material2dPlugin::<SpineNormalMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineAdditiveMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineMultiplyMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineScreenMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineNormalPmaMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineAdditivePmaMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineMultiplyPmaMaterial>::default())
            .add_plugin(Material2dPlugin::<SpineScreenPmaMaterial>::default())
            .insert_resource(SpineTextures::init())
            .add_asset::<Atlas>()
            .add_asset::<SkeletonJson>()
            .add_asset::<SkeletonBinary>()
            .add_asset::<SkeletonData>()
            .init_asset_loader::<AtlasLoader>()
            .init_asset_loader::<SkeletonJsonLoader>()
            .init_asset_loader::<SkeletonBinaryLoader>()
            .add_event::<SpineReadyEvent>()
            .add_event::<SpineEvent>()
            .add_system(spine_load.label(SpineSystem::Load))
            .add_system(
                spine_update
                    .label(SpineSystem::Update)
                    .after(SpineSystem::Load),
            )
            .add_system(
                spine_sync_entities
                    .label(SpineSystem::SyncEntities)
                    .after(SpineSystem::Update),
            )
            .add_system(
                spine_sync_bones
                    .label(SpineSystem::SyncBones)
                    .after(SpineSystem::SyncEntities),
            )
            .add_system(
                spine_sync_entities_applied
                    .label(SpineSystem::SyncEntitiesApplied)
                    .after(SpineSystem::SyncBones),
            )
            .add_system(
                spine_render
                    .label(SpineSystem::Render)
                    .after(SpineSystem::SyncEntitiesApplied),
            );
    }
}

#[derive(Component)]
pub struct Spine(pub SkeletonController);

#[derive(Component)]
pub struct SpineBone {
    pub spine_entity: Entity,
    pub handle: BoneHandle,
}

#[derive(Component)]
pub struct SpineMesh;

impl core::ops::Deref for Spine {
    type Target = SkeletonController;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Spine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Default, Component)]
pub enum SpineLoader {
    #[default]
    Loading,
    Ready,
    Failed,
}

impl SpineLoader {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Default, Bundle)]
pub struct SpineBundle {
    pub loader: SpineLoader,
    pub skeleton: Handle<SkeletonData>,
    pub crossfades: Crossfades,
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    pub visibility: Visibility,
    pub computed_visibility: ComputedVisibility,
}

#[derive(Clone)]
pub struct SpineReadyEvent(pub Entity);

#[derive(Clone)]
pub enum SpineEvent {
    Start { entity: Entity, animation: String },
    Interrupt { entity: Entity, animation: String },
    End { entity: Entity, animation: String },
    Complete { entity: Entity, animation: String },
    Dispose { entity: Entity },
    Event { entity: Entity, name: String },
}

#[derive(Default)]
struct SpineLoadLocal {
    // used for a one-frame delay in sending ready events
    ready: Vec<Entity>,
}

fn spine_load(
    mut skeleton_query: Query<(
        &mut SpineLoader,
        Entity,
        &Handle<SkeletonData>,
        Option<&Crossfades>,
    )>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ready_events: EventWriter<SpineReadyEvent>,
    mut local: Local<SpineLoadLocal>,
    mut skeleton_data_assets: ResMut<Assets<SkeletonData>>,
    atlases: Res<Assets<Atlas>>,
    jsons: Res<Assets<SkeletonJson>>,
    binaries: Res<Assets<SkeletonBinary>>,
    spine_textures: Res<SpineTextures>,
    asset_server: Res<AssetServer>,
) {
    for entity in local.ready.iter() {
        ready_events.send(SpineReadyEvent(*entity));
    }
    local.ready = vec![];
    for (mut spine_loader, entity, data_handle, crossfades) in skeleton_query.iter_mut() {
        if matches!(spine_loader.as_ref(), SpineLoader::Loading) {
            let mut skeleton_data_asset =
                if let Some(skeleton_data_asset) = skeleton_data_assets.get_mut(data_handle) {
                    skeleton_data_asset
                } else {
                    continue;
                };

            let mut premultipled_alpha = false;
            let skeleton_data = match &mut skeleton_data_asset {
                SkeletonData::JsonFile {
                    atlas,
                    json,
                    loader,
                    data,
                } => {
                    let atlas = if let Some(atlas) = atlases.get(atlas) {
                        atlas
                    } else {
                        continue;
                    };
                    if let Some(page) = atlas.atlas.pages().nth(0) {
                        premultipled_alpha = page.pma();
                    }
                    let json = if let Some(json) = jsons.get(&json) {
                        json
                    } else {
                        continue;
                    };
                    let skeleton_json = if let Some(loader) = &loader {
                        loader
                    } else {
                        *loader = Some(rusty_spine::SkeletonJson::new(atlas.atlas.clone()));
                        loader.as_ref().unwrap()
                    };
                    if let Some(skeleton_data) = &data {
                        skeleton_data.clone()
                    } else {
                        match skeleton_json.read_skeleton_data(&json.json) {
                            Ok(skeleton_data) => {
                                *data = Some(Arc::new(skeleton_data));
                                data.as_ref().unwrap().clone()
                            }
                            Err(_err) => {
                                // TODO: print error?
                                *spine_loader = SpineLoader::Loading;
                                continue;
                            }
                        }
                    }
                }
                SkeletonData::BinaryFile {
                    atlas,
                    binary,
                    loader,
                    data,
                } => {
                    let atlas = if let Some(atlas) = atlases.get(atlas) {
                        atlas
                    } else {
                        continue;
                    };
                    if let Some(page) = atlas.atlas.pages().nth(0) {
                        premultipled_alpha = page.pma();
                    }
                    let binary = if let Some(binary) = binaries.get(&binary) {
                        binary
                    } else {
                        continue;
                    };
                    let skeleton_binary = if let Some(loader) = &loader {
                        loader
                    } else {
                        *loader = Some(rusty_spine::SkeletonBinary::new(atlas.atlas.clone()));
                        loader.as_ref().unwrap()
                    };
                    if let Some(skeleton_data) = &data {
                        skeleton_data.clone()
                    } else {
                        match skeleton_binary.read_skeleton_data(&binary.binary) {
                            Ok(skeleton_data) => {
                                *data = Some(Arc::new(skeleton_data));
                                data.as_ref().unwrap().clone()
                            }
                            Err(_err) => {
                                // TODO: print error?
                                *spine_loader = SpineLoader::Loading;
                                continue;
                            }
                        }
                    }
                }
            };
            let mut animation_state_data = AnimationStateData::new(skeleton_data.clone());
            if let Some(crossfades) = crossfades {
                crossfades.apply(&mut animation_state_data);
            }
            let controller = SkeletonController::new(skeleton_data, Arc::new(animation_state_data))
                .with_settings(
                    SkeletonControllerSettings::new()
                        .with_cull_direction(CullDirection::CounterClockwise)
                        .with_premultiplied_alpha(premultipled_alpha),
                );
            commands
                .entity(entity)
                .with_children(|parent| {
                    let mut z = 0.;
                    for _ in controller.skeleton.slots() {
                        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList);
                        empty_mesh(&mut mesh);
                        let mesh_handle = meshes.add(mesh);
                        parent.spawn_bundle((
                            Mesh2dHandle(mesh_handle.clone()),
                            Transform::from_xyz(0., 0., z),
                            GlobalTransform::default(),
                            Visibility::default(),
                            ComputedVisibility::default(),
                        ));
                        z += EPSILON;
                    }
                    spawn_bones(
                        entity,
                        parent,
                        &controller.skeleton,
                        controller.skeleton.bone_root().handle(),
                    );
                })
                .insert(Spine(controller));
            *spine_loader = SpineLoader::Ready;
            local.ready.push(entity);
        }
    }

    spine_textures.update(asset_server.as_ref());
}

fn spawn_bones(
    spine_entity: Entity,
    parent: &mut ChildBuilder,
    skeleton: &Skeleton,
    bone: BoneHandle,
) {
    if let Some(bone) = bone.get(skeleton) {
        parent
            .spawn_bundle(SpriteBundle {
                sprite: Sprite {
                    custom_size: Some(Vec2::new(8., 32.)),
                    color: Color::NONE,
                    ..Default::default()
                },
                transform: Transform::from_xyz(0., 0., 1.),
                ..Default::default()
            })
            .insert(SpineBone {
                spine_entity,
                handle: bone.handle(),
            })
            .with_children(|parent| {
                for child in bone.children() {
                    spawn_bones(spine_entity, parent, skeleton, child.handle());
                }
            });
    }
}

#[derive(Default)]
struct SpineUpdateLocal {
    events: Arc<Mutex<VecDeque<SpineEvent>>>,
}

fn spine_update(
    mut spine_query: Query<(Entity, &mut Spine)>,
    mut spine_ready_events: EventReader<SpineReadyEvent>,
    mut spine_events: EventWriter<SpineEvent>,
    time: Res<Time>,
    local: Local<SpineUpdateLocal>,
) {
    for event in spine_ready_events.iter() {
        if let Ok((entity, mut spine)) = spine_query.get_mut(event.0) {
            let events = local.events.clone();
            spine.animation_state.set_listener(
                move |_animation_state, event_type, track_entry, spine_event| match event_type {
                    EventType::Start => {
                        let mut events = events.lock().unwrap();
                        events.push_back(SpineEvent::Start {
                            entity,
                            animation: track_entry.animation().name().to_owned(),
                        });
                    }
                    EventType::Interrupt => {
                        let mut events = events.lock().unwrap();
                        events.push_back(SpineEvent::Interrupt {
                            entity,
                            animation: track_entry.animation().name().to_owned(),
                        });
                    }
                    EventType::End => {
                        let mut events = events.lock().unwrap();
                        events.push_back(SpineEvent::End {
                            entity,
                            animation: track_entry.animation().name().to_owned(),
                        });
                    }
                    EventType::Complete => {
                        let mut events = events.lock().unwrap();
                        events.push_back(SpineEvent::Complete {
                            entity,
                            animation: track_entry.animation().name().to_owned(),
                        });
                    }
                    EventType::Dispose => {
                        let mut events = events.lock().unwrap();
                        events.push_back(SpineEvent::Dispose { entity });
                    }
                    EventType::Event => {
                        if let Some(spine_event) = spine_event {
                            let mut events = events.lock().unwrap();
                            events.push_back(SpineEvent::Event {
                                entity,
                                name: spine_event.data().name().to_owned(),
                            });
                        }
                    }
                    _ => {}
                },
            );
        }
    }
    for (_, mut spine) in spine_query.iter_mut() {
        spine.update(time.delta_seconds());
    }
    {
        let mut events = local.events.lock().unwrap();
        while let Some(event) = events.pop_front() {
            spine_events.send(event);
        }
    }
}

fn spine_render(
    mut commands: Commands,
    mut spine_query: Query<(&mut Spine, &Children)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut normal_materials: ResMut<Assets<SpineNormalMaterial>>,
    mut additive_materials: ResMut<Assets<SpineAdditiveMaterial>>,
    mut multiply_materials: ResMut<Assets<SpineMultiplyMaterial>>,
    mut screen_materials: ResMut<Assets<SpineScreenMaterial>>,
    mut normal_pma_materials: ResMut<Assets<SpineNormalPmaMaterial>>,
    mut additive_pma_materials: ResMut<Assets<SpineAdditivePmaMaterial>>,
    mut multiply_pma_materials: ResMut<Assets<SpineMultiplyPmaMaterial>>,
    mut screen_pma_materials: ResMut<Assets<SpineScreenPmaMaterial>>,
    mesh_query: Query<(
        Entity,
        &Mesh2dHandle,
        Option<&Handle<SpineNormalMaterial>>,
        Option<&Handle<SpineAdditiveMaterial>>,
        Option<&Handle<SpineMultiplyMaterial>>,
        Option<&Handle<SpineScreenMaterial>>,
        Option<&Handle<SpineNormalPmaMaterial>>,
        Option<&Handle<SpineAdditivePmaMaterial>>,
        Option<&Handle<SpineMultiplyPmaMaterial>>,
        Option<&Handle<SpineScreenPmaMaterial>>,
    )>,
    asset_server: Res<AssetServer>,
) {
    for (mut spine, spine_children) in spine_query.iter_mut() {
        let mut renderables = spine.0.renderables();
        for (renderable_index, child) in spine_children.iter().enumerate() {
            if let Ok((
                mesh_entity,
                mesh_handle,
                normal_material_handle,
                additive_material_handle,
                multiply_material_handle,
                screen_material_handle,
                normal_pma_material_handle,
                additive_pma_material_handle,
                multiply_pma_material_handle,
                screen_pma_material_handle,
            )) = mesh_query.get(*child)
            {
                let mesh = meshes.get_mut(&mesh_handle.0).unwrap();
                if let Some(renderable) = renderables.get_mut(renderable_index) {
                    let mut normals = vec![];
                    for _ in 0..renderable.vertices.len() {
                        normals.push([0., 0., 0.]);
                    }
                    mesh.set_indices(Some(Indices::U16(take(&mut renderable.indices))));
                    mesh.insert_attribute(
                        MeshVertexAttribute::new("Vertex_Position", 0, VertexFormat::Float32x2),
                        take(&mut renderable.vertices),
                    );
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, take(&mut renderable.uvs));

                    macro_rules! apply_material {
                        ($condition:expr, $material:ty, $handle:ident, $assets:ident) => {
                            if let Some(attachment_render_object) =
                                renderable.attachment_renderer_object
                            {
                                let spine_texture = unsafe {
                                    &mut *(attachment_render_object as *mut SpineTexture)
                                };
                                let texture_path = spine_texture.0.clone();
                                if $condition {
                                    let handle = if let Some(handle) = $handle {
                                        handle.clone()
                                    } else {
                                        let handle = $assets.add(<$material>::new(
                                            asset_server.load(texture_path.as_str()),
                                        ));
                                        commands.entity(mesh_entity).insert(handle.clone());
                                        handle
                                    };
                                    if let Some(material) = $assets.get_mut(&handle) {
                                        material.color.set_r(renderable.color.r);
                                        material.color.set_g(renderable.color.g);
                                        material.color.set_b(renderable.color.b);
                                        material.color.set_a(renderable.color.a);
                                        material.dark_color.set_r(renderable.dark_color.r);
                                        material.dark_color.set_g(renderable.dark_color.g);
                                        material.dark_color.set_b(renderable.dark_color.b);
                                        material.dark_color.set_a(renderable.dark_color.a);
                                        material.image = asset_server.load(texture_path.as_str());
                                    }
                                } else {
                                    if $handle.is_some() {
                                        commands.entity(mesh_entity).remove::<Handle<$material>>();
                                    }
                                }
                            } else {
                                if $handle.is_some() {
                                    commands.entity(mesh_entity).remove::<Handle<$material>>();
                                }
                            }
                        };
                    }

                    apply_material!(
                        renderable.blend_mode == BlendMode::Normal
                            && renderable.premultiplied_alpha == false,
                        SpineNormalMaterial,
                        normal_material_handle,
                        normal_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Additive
                            && renderable.premultiplied_alpha == false,
                        SpineAdditiveMaterial,
                        additive_material_handle,
                        additive_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Multiply
                            && renderable.premultiplied_alpha == false,
                        SpineMultiplyMaterial,
                        multiply_material_handle,
                        multiply_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Screen
                            && renderable.premultiplied_alpha == false,
                        SpineScreenMaterial,
                        screen_material_handle,
                        screen_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Normal
                            && renderable.premultiplied_alpha == true,
                        SpineNormalPmaMaterial,
                        normal_pma_material_handle,
                        normal_pma_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Additive
                            && renderable.premultiplied_alpha == true,
                        SpineAdditivePmaMaterial,
                        additive_pma_material_handle,
                        additive_pma_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Multiply
                            && renderable.premultiplied_alpha == true,
                        SpineMultiplyPmaMaterial,
                        multiply_pma_material_handle,
                        multiply_pma_materials
                    );
                    apply_material!(
                        renderable.blend_mode == BlendMode::Screen
                            && renderable.premultiplied_alpha == true,
                        SpineScreenPmaMaterial,
                        screen_pma_material_handle,
                        screen_pma_materials
                    );
                } else {
                    empty_mesh(mesh);
                }
            }
        }
    }
}

fn empty_mesh(mesh: &mut Mesh) {
    let indices = Indices::U32(vec![]);

    let positions: Vec<[f32; 3]> = vec![];
    let normals: Vec<[f32; 3]> = vec![];
    let uvs: Vec<[f32; 2]> = vec![];

    mesh.set_indices(Some(indices));
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
}

mod assets;
mod crossfades;
mod entity_sync;
mod materials;
mod textures;
