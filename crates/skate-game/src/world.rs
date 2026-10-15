use crate::assets::AssetManifest;
use bevy::prelude::*;

/// World transform owner. Animation only writes descendant bone transforms.
#[derive(Component)]
pub(crate) struct PlayerRoot;

pub(crate) struct WorldPlugin;
impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.065, 0.08, 0.10)))
            .insert_resource(GlobalAmbientLight {
                color: Color::WHITE,
                brightness: 350.,
                ..default()
            })
            .add_systems(Startup, spawn);
    }
}
fn spawn(world: &mut World) {
    let scene = {
        let server = world.resource::<AssetServer>();
        let manifest = world.resource::<AssetManifest>();
        server.load(GltfAssetLabel::Scene(0).from_asset(manifest.0.character_scene.clone()))
    };
    world.spawn((PlayerRoot, Transform::default(), Visibility::default()))
        .with_children(|parent| { parent.spawn(SceneRoot(scene)); });
    let mut prepared = crate::map_render::PreparedScene::new(world);
    let (mut map, root) = {
        let mut config = world.resource_mut::<crate::config::Config>();
        (config.map.take(), config.asset_root.clone())
    };
    // Startup owns the package, so packing may free each texture's pixels as
    // soon as they are in a page, and the meshes built afterwards can reuse
    // what the pixels held instead of growing the heap past it.
    let pixels = map.as_mut().map(crate::retail_render::TexturePixels::taken);
    prepared.prepare(map.as_ref(), pixels, &root);
    prepared
        .publish(world)
        .unwrap_or_else(|error| panic!("Could not publish initial world: {error}"));
}

pub(crate) fn spawn_test_world(
    commands: &mut crate::map_render::SceneCommands,
    meshes: &mut impl crate::map_render::AssetSink<Mesh>,
    materials: &mut impl crate::map_render::AssetSink<StandardMaterial>,
) {
    let colors = [
        Color::srgb(0.16, 0.19, 0.21),
        Color::srgb(0.48, 0.35, 0.22),
        Color::srgb(0.30, 0.43, 0.48),
        Color::srgb(0.24, 0.48, 0.31),
    ];
    for (quads, color) in crate::physics::ground::surfaces().into_iter().zip(colors) {
        let positions: Vec<[f32; 3]> = quads.into_iter().flat_map(|vertices| {
            [0, 2, 1, 0, 3, 2].map(|i| {
                let v = vertices[i];
                [v.x, v.y, v.z]
            })
        }).collect();
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        ).with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.compute_flat_normals();
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: color,
                perceptual_roughness: 0.9,
                double_sided: true,
                cull_mode: None,
                ..default()
            })),
            Transform::default(),
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 11000.,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4., 7., 4.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}
