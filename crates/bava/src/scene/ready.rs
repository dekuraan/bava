// SPDX-License-Identifier: MIT OR Apache-2.0
//! Knowing when a freshly built scene is all there.
//!
//! A scene's textures, terrain atlas and glTF models load on the IO task pool,
//! and each model's instance (and the animation started on it) spawns on
//! whatever frame its load happens to finish: a matter of wall-clock time,
//! not of frames. Live, that is a moment of pop-in. An offline render must
//! not capture anything before it is over, or two renders of the same song
//! differ, so `record` holds its first frame until [`SceneReady`] says so.

use bevy::asset::{DependencyLoadState, LoadState, RecursiveDependencyLoadState, UntypedAssetId};
use bevy::prelude::*;
use bevy::world_serialization::{WorldAssetRoot, WorldInstance, WorldInstanceSpawner};

use super::{SceneEntity, SceneRuntime, SceneSettings, SceneState};

/// Assets the active scene started loading that have not arrived yet. The
/// spawner fills it; each entry leaves once its asset has loaded with all its
/// dependencies — or failed to, which counts as settled: a broken texture must
/// not hang a render.
#[derive(Resource, Default, Debug)]
pub struct SceneLoads {
    pending: Vec<UntypedAssetId>,
}

impl SceneLoads {
    /// Wait for `id` as well.
    pub(crate) fn track(&mut self, id: impl Into<UntypedAssetId>) {
        self.pending.push(id.into());
    }

    /// Forget everything tracked: a new scene is being built.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }
}

/// True once the requested scene has been built (or has failed to build) and
/// everything it loads is in: every tracked asset settled and every glTF
/// model's instance spawned, with its animation started.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneReady(pub bool);

/// Updates [`SceneReady`] (in `PreUpdate`); read it after this set.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneReadySet;

pub(crate) struct SceneReadyPlugin;

impl Plugin for SceneReadyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneLoads>()
            .init_resource::<SceneReady>()
            .add_systems(PreUpdate, update_ready.in_set(SceneReadySet));
    }
}

/// Where one tracked asset stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    Pending,
    Done,
    Failed,
}

/// Classify an asset's load states (as [`AssetServer::get_load_states`]
/// reports them).
fn status(states: Option<(LoadState, DependencyLoadState, RecursiveDependencyLoadState)>) -> Load {
    match states {
        // No longer tracked: every handle to it is gone, so nothing will
        // draw it (the scene that wanted it was torn down).
        None => Load::Done,
        Some((LoadState::Failed(_), ..))
        | Some((_, _, RecursiveDependencyLoadState::Failed(_))) => Load::Failed,
        Some((LoadState::Loaded, _, RecursiveDependencyLoadState::Loaded)) => Load::Done,
        Some(_) => Load::Pending,
    }
}

#[allow(clippy::too_many_arguments)]
fn update_ready(
    settings: Res<SceneSettings>,
    state: Res<SceneState>,
    runtime: Res<SceneRuntime>,
    server: Res<AssetServer>,
    spawner: Option<Res<WorldInstanceSpawner>>,
    mut loads: ResMut<SceneLoads>,
    models: Query<(&WorldAssetRoot, Option<&WorldInstance>), With<SceneEntity>>,
    mut ready: ResMut<SceneReady>,
) {
    // `apply_scene` has acted on the latest request (loaded it, or failed).
    let built = state.attempted.as_ref() == Some(&*settings);

    loads
        .pending
        .retain(|&id| match status(server.get_load_states(id)) {
            Load::Pending => true,
            Load::Done => false,
            Load::Failed => {
                // The asset server logs the error itself; offline, also say that
                // the render goes on without it.
                if runtime.offline {
                    let path = server
                        .get_path(id)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| format!("{id:?}"));
                    warn!("bava: scene asset {path} failed to load; rendering without it");
                }
                false
            }
        });

    // A model is in once its instance has spawned — the frame its animation
    // starts — or never will be because its file failed to load.
    let models_in = models.iter().all(|(root, instance)| {
        let spawned =
            instance.is_some_and(|i| spawner.as_ref().is_none_or(|s| s.instance_is_ready(**i)));
        spawned
            || matches!(
                server.get_load_state(root.0.id()),
                Some(LoadState::Failed(_))
            )
    });

    ready.set_if_neq(SceneReady(built && loads.pending.is_empty() && models_in));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bevy::asset::AssetLoadError;
    use bevy::world_serialization::WorldAsset;

    use super::*;

    #[test]
    fn loads_settle_when_done_failed_or_forgotten() {
        use DependencyLoadState as D;
        use LoadState as L;
        use RecursiveDependencyLoadState as R;
        let failed = || Arc::new(AssetLoadError::AssetMetaReadError);

        assert_eq!(status(None), Load::Done);
        assert_eq!(
            status(Some((L::Loading, D::Loading, R::Loading))),
            Load::Pending
        );
        assert_eq!(
            status(Some((L::Loaded, D::Loading, R::Loading))),
            Load::Pending,
            "its textures are still on the way"
        );
        assert_eq!(status(Some((L::Loaded, D::Loaded, R::Loaded))), Load::Done);
        assert_eq!(
            status(Some((L::Failed(failed()), D::Loading, R::Loading))),
            Load::Failed
        );
        assert_eq!(
            status(Some((L::Loaded, D::Failed(failed()), R::Failed(failed())))),
            Load::Failed,
            "a dependency failed"
        );
    }

    fn ready_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()));
        app.init_asset::<WorldAsset>();
        app.init_resource::<SceneSettings>();
        app.init_resource::<SceneState>();
        app.insert_resource(SceneRuntime { offline: true });
        app.add_plugins(SceneReadyPlugin);
        app
    }

    fn is_ready(app: &App) -> bool {
        app.world().resource::<SceneReady>().0
    }

    #[test]
    fn ready_once_the_request_is_built_and_its_models_are_in() {
        let mut app = ready_app();
        app.world_mut().resource_mut::<SceneSettings>().name = "s".into();
        app.update();
        assert!(!is_ready(&app), "the scene has not been built yet");

        let wanted = app.world().resource::<SceneSettings>().clone();
        app.world_mut().resource_mut::<SceneState>().attempted = Some(wanted);
        // A model whose instance has not spawned yet.
        let handle = app
            .world()
            .resource::<Assets<WorldAsset>>()
            .reserve_handle();
        let model = app
            .world_mut()
            .spawn((SceneEntity, WorldAssetRoot(handle)))
            .id();
        app.update();
        assert!(!is_ready(&app), "the model is still on its way");

        app.world_mut().despawn(model);
        app.update();
        assert!(is_ready(&app));

        // A new request is not ready until it, too, has been built.
        app.world_mut().resource_mut::<SceneSettings>().name = "t".into();
        app.update();
        assert!(!is_ready(&app));
    }
}
