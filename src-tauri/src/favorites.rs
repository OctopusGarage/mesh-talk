//! Favorites / pinned contacts: per-contact UI preferences the user sets locally —
//! whether a contact is pinned to the top of the sidebar, and an optional custom alias
//! that overrides the announced name. This is purely local UI state (no crypto, no
//! protocol), persisted as JSON in the signed-in host account's profile.
//!
//! Keyed by the stable contact identity used elsewhere in the UI: the `account_id`
//! (a multi-device contact is one entry; channels use their `channel_id`). The store
//! is opaque to what the key means — it just maps an id to a record.

use crate::commands::CommandError;
use crate::config_store;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What we persist about one contact's local UI preferences.
///
/// Self-describing JSON-at-rest, so per-field `#[serde(default)]` is the forward-compat
/// tool: a file written by an older build still loads (missing fields fall back to
/// their defaults) instead of failing the whole parse.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteRecord {
    /// Pinned contacts sort to the top of the sidebar.
    #[serde(default)]
    pub pinned: bool,
    /// A user-set alias that overrides the announced name in the UI. `None` = use the
    /// announced name.
    #[serde(default)]
    pub custom_alias: Option<String>,
    /// Local-only channel visibility; does not change membership or delete history.
    #[serde(default)]
    pub hidden: bool,
}

/// The on-disk shape: `id` (account_id / channel_id) -> record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FavoritesFile {
    #[serde(default)]
    contacts: HashMap<String, FavoriteRecord>,
}

/// Serializes read/modify/write operations within the process.
#[derive(Clone, Default)]
pub struct FavoritesState(Arc<Mutex<()>>);

/// One favorites entry returned to the UI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FavoriteInfo {
    pub id: String,
    pub pinned: bool,
    pub custom_alias: Option<String>,
    pub hidden: bool,
}

/// The persistence file inside the host account's profile.
const FAVORITES_FILE: &str = "favorites.json";

fn capture_favorites(
    app: &AppState,
    owner: &str,
) -> Result<(crate::state::SessionLease, u64, std::path::PathBuf), CommandError> {
    let lease = app
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner {
        return Err(CommandError::Authorization("session owner mismatch".into()));
    }
    let revision = app.app_lock().authorization_revision()?;
    let path = config_store::with_account_lease(app, &lease, FAVORITES_FILE, |path| {
        app.app_lock()
            .with_unchanged_revision(revision, || Ok(path.to_owned()))
    })?;
    Ok((lease, revision, path))
}

fn update_favorites_at(
    app: &AppState,
    lease: &crate::state::SessionLease,
    revision: u64,
    state: &FavoritesState,
    path: &std::path::Path,
    edit: impl FnOnce(&mut FavoritesFile) -> Result<(), CommandError>,
) -> Result<(), CommandError> {
    // Every favorites consumer takes this lane before its short session checks.
    // Never wait for favorites while retaining the global session guard.
    let _guard = state
        .0
        .lock()
        .map_err(|_| CommandError::Internal("Favorites lock unavailable".into()))?;
    config_store::with_account_lease(app, lease, FAVORITES_FILE, |_| {
        app.app_lock().with_unchanged_revision(revision, || Ok(()))
    })?;
    let mut file: FavoritesFile = config_store::read_account(path);
    edit(&mut file)?;
    let prepared = config_store::prepare_account_write(path, &file)?;
    config_store::with_account_lease(app, lease, FAVORITES_FILE, |_| {
        app.app_lock()
            .with_unchanged_revision(revision, || prepared.commit())
    })
}

async fn update_favorite(
    app: AppState,
    state: FavoritesState,
    owner: String,
    id: String,
    edit: impl FnOnce(&mut FavoriteRecord) + Send + 'static,
) -> Result<(), CommandError> {
    let (lease, revision, path) = capture_favorites(&app, &owner)?;
    tokio::task::spawn_blocking(move || {
        update_favorites_at(&app, &lease, revision, &state, &path, |file| {
            let record = file.contacts.entry(id.clone()).or_default();
            edit(record);
            if !record.pinned && record.custom_alias.is_none() && !record.hidden {
                file.contacts.remove(&id);
            }
            Ok(())
        })
    })
    .await
    .map_err(|_| CommandError::Internal("Favorites update terminated".into()))?
}

/// Snapshot every favorites entry the user has set (pin and/or alias).
#[tauri::command]
pub async fn get_favorites(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
) -> Result<Vec<FavoriteInfo>, CommandError> {
    let (lease, revision, path) = capture_favorites(&app, &owner)?;
    let app = app.inner().clone();
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let _guard = state
            .0
            .lock()
            .map_err(|_| CommandError::Internal("Favorites lock unavailable".into()))?;
        config_store::with_account_lease(&app, &lease, FAVORITES_FILE, |_| {
            app.app_lock().with_unchanged_revision(revision, || Ok(()))
        })?;
        let file: FavoritesFile = config_store::read_account(&path);
        let rows = file
            .contacts
            .into_iter()
            .map(|(id, record)| FavoriteInfo {
                id,
                pinned: record.pinned,
                custom_alias: record.custom_alias,
                hidden: record.hidden,
            })
            .collect();
        config_store::with_account_lease(&app, &lease, FAVORITES_FILE, |_| {
            app.app_lock()
                .with_unchanged_revision(revision, || Ok(rows))
        })
    })
    .await
    .map_err(|_| CommandError::Internal("Favorites read terminated".into()))?
}

/// Pin or unpin a contact, pruning empty local preferences.
#[tauri::command]
pub async fn set_favorite(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    pinned: bool,
) -> Result<(), CommandError> {
    update_favorite(
        app.inner().clone(),
        state.inner().clone(),
        owner,
        id,
        move |record| record.pinned = pinned,
    )
    .await
}

/// Set or clear an alias; blank aliases return to the announced name.
#[tauri::command]
pub async fn set_alias(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    alias: Option<String>,
) -> Result<(), CommandError> {
    let trimmed = alias
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    update_favorite(
        app.inner().clone(),
        state.inner().clone(),
        owner,
        id,
        move |record| record.custom_alias = trimmed,
    )
    .await
}

/// Hide or restore a channel locally without changing its membership.
#[tauri::command]
pub async fn set_channel_hidden(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    hidden: bool,
) -> Result<(), CommandError> {
    update_favorite(
        app.inner().clone(),
        state.inner().clone(),
        owner,
        id,
        move |record| record.hidden = hidden,
    )
    .await
}

/// Import complete records with one disk replacement, avoiding partial pin/alias state.
#[tauri::command]
pub async fn set_favorite_records(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    node: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
    records: Vec<FavoriteInfo>,
    expected: HashMap<String, Option<FavoriteRecord>>,
) -> Result<(), CommandError> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &node, &owner)?;
    let app_lock = app.app_lock().clone();
    let revision = app_lock.authorization_revision()?;
    let path = config_store::account_path(&crate::data_dir(), &owner, FAVORITES_FILE)
        .ok_or_else(|| CommandError::Authorization("invalid account owner".into()))?;
    let state = state.inner().clone();
    // Retain the installed runtime until the staged commit completes, even if
    // the IPC waiter disappears. Disk work never holds the session guard.
    let guard = node.0.clone().read_owned().await;
    tokio::task::spawn_blocking(move || {
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let _guard = state
            .0
            .lock()
            .map_err(|_| CommandError::Internal("Favorites lock unavailable".into()))?;
        admission.checked(runtime, || {
            app_lock.with_unchanged_revision(revision, || Ok(()))
        })?;
        let prepared = prepare_linked_records(
            &path,
            runtime.account_id(),
            &expected_account,
            records,
            &expected,
        )?;
        admission.checked(runtime, || {
            app_lock.with_unchanged_revision(revision, || prepared.commit())
        })
    })
    .await
    .map_err(|_| CommandError::Internal("Favorites import terminated".into()))?
}

fn prepare_linked_records(
    path: &std::path::Path,
    current_account: &str,
    expected_account: &str,
    records: Vec<FavoriteInfo>,
    expected: &HashMap<String, Option<FavoriteRecord>>,
) -> Result<config_store::PreparedAccountWrite, CommandError> {
    if current_account != expected_account {
        return Err(CommandError::Authorization(
            "Contact preferences belong to another account".into(),
        ));
    }
    let mut file: FavoritesFile = config_store::read_account(path);
    check_expected(&file, &records, expected)?;
    apply_records(&mut file, records)?;
    config_store::prepare_account_write(path, &file)
}

fn check_expected(
    file: &FavoritesFile,
    records: &[FavoriteInfo],
    expected: &HashMap<String, Option<FavoriteRecord>>,
) -> Result<(), CommandError> {
    if records.iter().any(|row| {
        !expected.contains_key(&row.id)
            || file.contacts.get(&row.id)
                != expected.get(&row.id).and_then(|record| record.as_ref())
    }) {
        return Err(CommandError::Internal(
            "Local favorites changed during linked import".into(),
        ));
    }
    Ok(())
}

fn apply_records(file: &mut FavoritesFile, records: Vec<FavoriteInfo>) -> Result<(), CommandError> {
    if records.len() > 2048
        || records.iter().any(|record| {
            !matches!(record.id.len(), 32 | 64)
                || !record.id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || record
                    .custom_alias
                    .as_ref()
                    .is_some_and(|alias| alias.len() > 1024 || alias.chars().count() > 256)
        })
    {
        return Err(CommandError::InvalidInput(
            "invalid favorite records".into(),
        ));
    }
    for record in records {
        let alias = record
            .custom_alias
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if !record.pinned && alias.is_none() && !record.hidden {
            file.contacts.remove(&record.id);
        } else {
            file.contacts.insert(
                record.id,
                FavoriteRecord {
                    pinned: record.pinned,
                    custom_alias: alias,
                    hidden: record.hidden,
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::time::Duration;
    use tauri::Manager;

    struct CommandFixture {
        root: tempfile::TempDir,
        profile: tempfile::TempDir,
        host: tauri::App<tauri::test::MockRuntime>,
        owner: String,
    }

    fn command_fixture() -> CommandFixture {
        // Exercise the command's real path selection without sharing a profile
        // with another test or any installed account. TempDir owns cleanup.
        let accounts = crate::data_dir().join("accounts");
        std::fs::create_dir_all(&accounts).unwrap();
        let profile = tempfile::Builder::new()
            .prefix("favorites-test-")
            .tempdir_in(accounts)
            .unwrap();
        let owner = profile
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let (root, app, mut user) = session_fixture();
        user.user_id = owner.clone();
        app.session()
            .set("command-session".into(), user, "pw".into());
        let host = tauri::test::mock_builder()
            .manage(app)
            .manage(FavoritesState::default())
            .manage(crate::chat_commands::NodeState::empty())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        CommandFixture {
            root,
            profile,
            host,
            owner,
        }
    }

    async fn install_command_runtime(fixture: &CommandFixture) -> String {
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            fixture.root.path(),
            &fixture.owner,
            "Owner",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let account = runtime.account_id().to_owned();
        *fixture
            .host
            .state::<crate::chat_commands::NodeState>()
            .0
            .write()
            .await = Some(runtime);
        account
    }

    struct BlockedFavoritesLane {
        release: std::sync::mpsc::Sender<()>,
        holder: std::thread::JoinHandle<bool>,
    }

    impl BlockedFavoritesLane {
        fn hold(state: &FavoritesState) -> Self {
            let state = state.clone();
            let (entered, started) = std::sync::mpsc::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let holder = std::thread::spawn(move || {
                let _guard = state.0.lock().unwrap();
                entered.send(()).unwrap();
                // A synchronous-lock regression must release the test scheduler
                // even when its async timeout can no longer be polled.
                blocked.recv_timeout(Duration::from_secs(5)).is_ok()
            });
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            Self { release, holder }
        }

        fn release(self) -> bool {
            let _ = self.release.send(());
            self.holder.join().unwrap()
        }
    }

    #[tokio::test]
    async fn favorite_commands_preserve_independent_fields_and_prune_only_empty_records() {
        let CommandFixture {
            profile,
            host,
            owner,
            root: _root,
        } = command_fixture();
        let id = "a".repeat(64);
        set_favorite(host.state(), host.state(), owner.clone(), id.clone(), true)
            .await
            .unwrap();
        set_alias(
            host.state(),
            host.state(),
            owner.clone(),
            id.clone(),
            Some("  Alice  ".into()),
        )
        .await
        .unwrap();
        set_channel_hidden(host.state(), host.state(), owner.clone(), id.clone(), true)
            .await
            .unwrap();
        set_favorite(host.state(), host.state(), owner.clone(), id.clone(), false)
            .await
            .unwrap();
        let rows = get_favorites(host.state(), host.state(), owner.clone())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert!(!rows[0].pinned);
        assert_eq!(rows[0].custom_alias.as_deref(), Some("Alice"));
        assert!(rows[0].hidden);
        let stored: FavoritesFile =
            config_store::read_account(&profile.path().join(FAVORITES_FILE));
        assert_eq!(stored.contacts[&id].custom_alias.as_deref(), Some("Alice"));

        set_alias(
            host.state(),
            host.state(),
            owner.clone(),
            id.clone(),
            Some(" \t ".into()),
        )
        .await
        .unwrap();
        assert_eq!(
            get_favorites(host.state(), host.state(), owner.clone())
                .await
                .unwrap()
                .len(),
            1
        );
        set_channel_hidden(host.state(), host.state(), owner.clone(), id, false)
            .await
            .unwrap();
        assert!(
            get_favorites(host.state(), host.state(), owner)
                .await
                .unwrap()
                .is_empty()
        );
        let stored: FavoritesFile =
            config_store::read_account(&profile.path().join(FAVORITES_FILE));
        assert!(stored.contacts.is_empty());
    }

    #[tokio::test]
    async fn queued_favorite_commands_yield_and_reject_retired_authority() {
        let CommandFixture {
            profile,
            host,
            owner,
            root: _root,
        } = command_fixture();
        let id = "b".repeat(64);
        set_favorite(host.state(), host.state(), owner.clone(), id.clone(), true)
            .await
            .unwrap();
        let before = std::fs::read(profile.path().join(FAVORITES_FILE)).unwrap();
        // Both reads and edits must yield while waiting for favorites, release
        // session authority, and reject same-owner relogin or lock revision changes.
        for (read, replace_session) in [(true, false), (false, false), (true, true), (false, true)]
        {
            let lane = BlockedFavoritesLane::hold(&host.state::<FavoritesState>());
            let operation = async {
                if read {
                    get_favorites(host.state(), host.state(), owner.clone())
                        .await
                        .map(|_| ())
                } else {
                    set_alias(
                        host.state(),
                        host.state(),
                        owner.clone(),
                        id.clone(),
                        Some("stale".into()),
                    )
                    .await
                }
            };
            tokio::pin!(operation);
            let initial_result =
                tokio::time::timeout(Duration::from_millis(50), &mut operation).await;
            let pending = initial_result.is_err();
            let app = host.state::<AppState>().inner().clone();
            let (observed, observation) = std::sync::mpsc::channel();
            let observer = std::thread::spawn(move || {
                if replace_session {
                    let user = app.session().get().unwrap().user;
                    app.session().set("replacement".into(), user, "pw".into());
                } else {
                    app.app_lock().clear();
                }
                let _ = observed.send(());
            });
            let responsive = observation.recv_timeout(Duration::from_millis(300)).is_ok();
            let released = lane.release();
            let result = match initial_result {
                Ok(result) => result,
                Err(_) => tokio::time::timeout(Duration::from_secs(2), operation)
                    .await
                    .expect("favorites command must finish after lane release"),
            };
            observer.join().unwrap();
            assert!(released, "the blocked lane needed its emergency release");
            assert!(
                pending,
                "the runtime must poll timers while favorites are busy"
            );
            assert!(responsive, "session retirement must not wait for favorites");
            assert!(matches!(result, Err(CommandError::Authorization(_))));
            assert_eq!(
                std::fs::read(profile.path().join(FAVORITES_FILE)).unwrap(),
                before
            );
        }
        assert!(matches!(
            get_favorites(host.state(), host.state(), "another-owner".into()).await,
            Err(CommandError::Authorization(_))
        ));
    }

    #[tokio::test]
    async fn linked_favorite_command_checks_account_and_preserves_newer_local_edits() {
        let fixture = command_fixture();
        let account = install_command_runtime(&fixture).await;
        let CommandFixture {
            profile,
            host,
            owner,
            root: _root,
        } = fixture;
        let local_id = "c".repeat(64);
        set_alias(
            host.state(),
            host.state(),
            owner.clone(),
            local_id.clone(),
            Some("Local".into()),
        )
        .await
        .unwrap();
        let path = profile.path().join(FAVORITES_FILE);
        let before = std::fs::read(&path).unwrap();
        let imported_id = "d".repeat(32);
        let records = || {
            vec![FavoriteInfo {
                id: imported_id.clone(),
                pinned: true,
                custom_alias: Some("  Remote  ".into()),
                hidden: true,
            }]
        };
        let expected = HashMap::from([(imported_id.clone(), None)]);
        let wrong_account = set_favorite_records(
            host.state(),
            host.state(),
            host.state(),
            owner.clone(),
            "another-account".into(),
            records(),
            expected.clone(),
        )
        .await;
        let after_wrong_account = std::fs::read(&path).unwrap();
        let imported = set_favorite_records(
            host.state(),
            host.state(),
            host.state(),
            owner.clone(),
            account.clone(),
            records(),
            expected.clone(),
        )
        .await;
        let stored: FavoritesFile = config_store::read_account(&path);
        let after_import = std::fs::read(&path).unwrap();
        let stale_expected = set_favorite_records(
            host.state(),
            host.state(),
            host.state(),
            owner,
            account,
            records(),
            expected,
        )
        .await;
        let after_stale_expected = std::fs::read(&path).unwrap();
        host.state::<crate::chat_commands::NodeState>()
            .0
            .write()
            .await
            .take()
            .unwrap()
            .stop()
            .await;

        assert!(matches!(wrong_account, Err(CommandError::Authorization(_))));
        assert_eq!(after_wrong_account, before);
        imported.unwrap();
        assert_eq!(stored.contacts.len(), 2);
        assert_eq!(
            stored.contacts[&local_id].custom_alias.as_deref(),
            Some("Local")
        );
        assert_eq!(
            stored.contacts[&imported_id],
            FavoriteRecord {
                pinned: true,
                custom_alias: Some("Remote".into()),
                hidden: true,
            }
        );
        assert!(matches!(stale_expected, Err(CommandError::Internal(_))));
        assert_eq!(after_stale_expected, after_import);
    }

    #[tokio::test]
    async fn cancelled_linked_favorite_command_retains_runtime_until_worker_rejects_retirement() {
        let fixture = command_fixture();
        let account = install_command_runtime(&fixture).await;
        let CommandFixture {
            profile,
            host,
            owner,
            root: _root,
        } = fixture;
        let node = host.state::<crate::chat_commands::NodeState>();
        let lane = BlockedFavoritesLane::hold(&host.state::<FavoritesState>());
        let id = "e".repeat(32);
        let mut operation = Box::pin(set_favorite_records(
            host.state(),
            host.state(),
            host.state(),
            owner,
            account,
            vec![FavoriteInfo {
                id: id.clone(),
                pinned: true,
                custom_alias: None,
                hidden: false,
            }],
            HashMap::from([(id, None)]),
        ));
        let pending = tokio::time::timeout(Duration::from_millis(50), &mut operation)
            .await
            .is_err();
        drop(operation);
        let observation_available = node.0.try_read().is_ok();
        let retirement_blocked = node.0.try_write().is_err();
        let app = host.state::<AppState>().inner().clone();
        let (retired, retirement) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            app.app_lock().clear();
            let _ = retired.send(());
        });
        let responsive = retirement.recv_timeout(Duration::from_millis(300)).is_ok();
        let released = lane.release();
        let runtime = tokio::time::timeout(Duration::from_secs(2), node.0.write())
            .await
            .unwrap()
            .take()
            .unwrap();
        observer.join().unwrap();
        runtime.stop().await;

        assert!(released, "the blocked lane needed its emergency release");
        assert!(pending, "the runtime must poll timers during linked import");
        assert!(
            observation_available,
            "an import must permit unrelated runtime observations"
        );
        assert!(
            retirement_blocked,
            "cancelling IPC must not release the worker's runtime lease"
        );
        assert!(
            responsive,
            "session retirement must not wait for linked import"
        );
        assert!(
            !profile.path().join(FAVORITES_FILE).exists(),
            "retired import must not publish"
        );
    }

    fn session_fixture() -> (tempfile::TempDir, AppState, crate::services::user::User) {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        )));
        let user = crate::services::user::User {
            user_id: "owner".into(),
            account_login_name: None,
            name: "owner".into(),
            display_name: "Owner".into(),
            address: String::new(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session().set("first".into(), user.clone(), "pw".into());
        (root, app, user)
    }

    #[test]
    fn favorite_preparation_cannot_survive_an_unobserved_lock_revision_change() {
        let (root, app, _) = session_fixture();
        let path = root.path().join(FAVORITES_FILE);
        let lease = app.session().capture().unwrap();
        let revision = app.app_lock().authorization_revision().unwrap();
        let result = update_favorites_at(
            &app,
            &lease,
            revision,
            &FavoritesState::default(),
            &path,
            |file| {
                file.contacts.insert(
                    "friend".into(),
                    FavoriteRecord {
                        pinned: true,
                        ..Default::default()
                    },
                );
                // A complete lock/unlock cycle is no longer locked but advances authority.
                app.app_lock().clear();
                Ok(())
            },
        );
        assert!(!app.app_lock().is_locked());
        assert!(result.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn queued_favorite_edit_cannot_follow_same_owner_relogin() {
        let (root, app, user) = session_fixture();
        let path = root.path().join(FAVORITES_FILE);
        let lease = app.session().capture().unwrap();
        let revision = app.app_lock().authorization_revision().unwrap();
        app.session().set("replacement".into(), user, "pw".into());
        let attempted = update_favorites_at(
            &app,
            &lease,
            revision,
            &FavoritesState::default(),
            &path,
            |file| {
                file.contacts.insert(
                    "friend".into(),
                    FavoriteRecord {
                        pinned: true,
                        ..Default::default()
                    },
                );
                Ok(())
            },
        );
        assert!(attempted.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn slow_favorite_preparation_releases_session_and_rechecks_before_publication() {
        let (root, app, user) = session_fixture();
        let path = root.path().join(FAVORITES_FILE);
        let lease = app.session().capture().unwrap();
        let revision = app.app_lock().authorization_revision().unwrap();
        let (entered, started) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker_app = app.clone();
        let worker_path = path.clone();
        let writer = std::thread::spawn(move || {
            update_favorites_at(
                &worker_app,
                &lease,
                revision,
                &FavoritesState::default(),
                &worker_path,
                |file| {
                    file.contacts.insert(
                        "friend".into(),
                        FavoriteRecord {
                            pinned: true,
                            ..Default::default()
                        },
                    );
                    entered.send(()).unwrap();
                    blocked.recv().unwrap();
                    Ok(())
                },
            )
        });
        started
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (observed, observation) = std::sync::mpsc::channel();
        let observer_app = app.clone();
        let observer = std::thread::spawn(move || {
            observer_app
                .session()
                .set("replacement".into(), user, "pw".into());
            observed.send(()).unwrap();
        });
        let responsive = observation
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_ok();
        release.send(()).unwrap();
        let result = writer.join().unwrap();
        observer.join().unwrap();
        assert!(
            responsive,
            "session inspection/replacement waited for preference preparation"
        );
        assert!(
            result.is_err(),
            "a retired operation must not publish its staged file"
        );
        assert!(!path.exists());
    }
    #[test]
    fn stale_crypto_account_cannot_replace_the_same_local_users_saved_alias() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("favorites.json");
        let id = "b".repeat(32);
        let original = FavoriteRecord {
            pinned: false,
            custom_alias: Some("Ronaldo".into()),
            hidden: false,
        };
        let file = FavoritesFile {
            contacts: HashMap::from([(id.clone(), original.clone())]),
        };
        config_store::write_account(&path, &file).unwrap();
        let before = std::fs::read(&path).unwrap();
        let records = vec![FavoriteInfo {
            id: id.clone(),
            pinned: false,
            custom_alias: Some("Previous account alias".into()),
            hidden: false,
        }];
        let expected = HashMap::from([(id, Some(original))]);
        assert!(
            prepare_linked_records(&path, "new-account", "old-account", records, &expected)
                .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn invalid_batch_does_not_apply_any_partial_favorite() {
        let mut file = FavoritesFile::default();
        let row = |id: &str| FavoriteInfo {
            id: id.into(),
            pinned: true,
            custom_alias: Some("Alice".into()),
            hidden: false,
        };
        assert!(apply_records(&mut file, vec![row(&"a".repeat(32)), row("invalid")]).is_err());
        assert!(file.contacts.is_empty());
        apply_records(&mut file, vec![row(&"a".repeat(32))]).unwrap();
        assert_eq!(
            file.contacts
                .values()
                .next()
                .unwrap()
                .custom_alias
                .as_deref(),
            Some("Alice")
        );
    }
    #[test]
    fn linked_batch_preserves_a_newer_local_favorite() {
        let id = "a".repeat(32);
        let mut file = FavoritesFile::default();
        apply_records(
            &mut file,
            vec![FavoriteInfo {
                id: id.clone(),
                pinned: true,
                custom_alias: Some("Local".into()),
                hidden: false,
            }],
        )
        .unwrap();
        let remote = vec![FavoriteInfo {
            id: id.clone(),
            pinned: false,
            custom_alias: Some("Remote".into()),
            hidden: false,
        }];
        let expected = HashMap::from([(id.clone(), None)]);
        assert!(check_expected(&file, &remote, &expected).is_err());
        assert_eq!(file.contacts[&id].custom_alias.as_deref(), Some("Local"));
    }
}
