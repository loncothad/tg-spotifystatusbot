use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use redb::{Database, ReadableTable, TableDefinition};
use scc::{HashIndex, HashMap};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

const USERS: TableDefinition<u64, &[u8]> = TableDefinition::new("users");
const OAUTH_STATES: TableDefinition<&str, &[u8]> = TableDefinition::new("oauth_states");
const ALLOWLIST: TableDefinition<u64, u8> = TableDefinition::new("allowlist");

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpotifyTokens {
    pub access_token: CompactString,
    pub refresh_token: CompactString,
    pub expires_at: i64,
    pub token_type: CompactString,
    pub scope: CompactString,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthState {
    pub telegram_user_id: u64,
    pub code_verifier: CompactString,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    db: Database,
    tokens: HashMap<u64, SpotifyTokens>,
    oauth: HashMap<CompactString, OauthState>,
    allowlist: HashIndex<u64, u8>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let db = Database::create(path.as_ref()).map_err(AppError::database)?;
        init_tables(&db)?;
        let tokens = HashMap::default();
        let oauth = HashMap::default();
        let allowlist = HashIndex::default();
        hydrate(&db, &tokens, &oauth, &allowlist)?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                tokens,
                oauth,
                allowlist,
            }),
        })
    }

    pub async fn put_tokens(&self, telegram_user_id: u64, tokens: &SpotifyTokens) -> Result<()> {
        persist_user(&self.inner.db, telegram_user_id, Some(tokens))?;
        let _ = self
            .inner
            .tokens
            .upsert_async(telegram_user_id, tokens.clone())
            .await;
        Ok(())
    }

    pub async fn get_tokens(&self, telegram_user_id: u64) -> Result<Option<SpotifyTokens>> {
        Ok(self
            .inner
            .tokens
            .read_async(&telegram_user_id, |_, tokens| tokens.clone())
            .await)
    }

    pub async fn delete_tokens(&self, telegram_user_id: u64) -> Result<bool> {
        let removed = persist_user(&self.inner.db, telegram_user_id, None)?;
        let cached = self
            .inner
            .tokens
            .remove_async(&telegram_user_id)
            .await
            .is_some();
        Ok(removed || cached)
    }

    pub async fn put_oauth_state(&self, state: &str, value: &OauthState) -> Result<()> {
        persist_oauth(&self.inner.db, state, Some(value))?;
        let _ = self
            .inner
            .oauth
            .upsert_async(CompactString::from(state), value.clone())
            .await;
        Ok(())
    }

    pub async fn take_oauth_state(&self, state: &str, ttl_secs: i64) -> Result<OauthState> {
        let Some((_, value)) = self.inner.oauth.remove_async(state).await else {
            let _ = persist_oauth(&self.inner.db, state, None)?;
            return Err(AppError::InvalidOauthState);
        };
        if let Err(err) = persist_oauth(&self.inner.db, state, None) {
            let _ = self
                .inner
                .oauth
                .insert_async(CompactString::from(state), value.clone())
                .await;
            return Err(err);
        }
        if now_unix() - value.created_at > ttl_secs {
            return Err(AppError::InvalidOauthState);
        }
        Ok(value)
    }

    pub async fn allow_user(&self, telegram_user_id: u64) -> Result<bool> {
        if self.inner.allowlist.contains(&telegram_user_id) {
            return Ok(false);
        }
        persist_allow(&self.inner.db, telegram_user_id, true)?;
        Ok(self
            .inner
            .allowlist
            .insert_async(telegram_user_id, 1)
            .await
            .is_ok())
    }

    pub async fn deny_user(&self, telegram_user_id: u64) -> Result<bool> {
        persist_allow(&self.inner.db, telegram_user_id, false)?;
        Ok(self.inner.allowlist.remove_async(&telegram_user_id).await)
    }

    pub async fn is_allowlisted(&self, telegram_user_id: u64) -> Result<bool> {
        Ok(self.inner.allowlist.contains(&telegram_user_id))
    }

    pub async fn list_allowlist(&self) -> Result<Vec<u64>> {
        let mut ids = Vec::new();
        self.inner
            .allowlist
            .iter_async(|id, _| {
                ids.push(*id);
                true
            })
            .await;
        ids.sort_unstable();
        Ok(ids)
    }
}

fn init_tables(db: &Database) -> Result<()> {
    let txn = db.begin_write().map_err(AppError::database)?;
    {
        let _users = txn.open_table(USERS).map_err(AppError::database)?;
        let _states = txn.open_table(OAUTH_STATES).map_err(AppError::database)?;
        let _allow = txn.open_table(ALLOWLIST).map_err(AppError::database)?;
    }
    txn.commit().map_err(AppError::database)?;
    Ok(())
}

fn hydrate(
    db: &Database,
    tokens: &HashMap<u64, SpotifyTokens>,
    oauth: &HashMap<CompactString, OauthState>,
    allowlist: &HashIndex<u64, u8>,
) -> Result<()> {
    let txn = db.begin_read().map_err(AppError::database)?;
    {
        let table = txn.open_table(USERS).map_err(AppError::database)?;
        for entry in table.iter().map_err(AppError::database)? {
            let (key, value) = entry.map_err(AppError::database)?;
            let parsed = serde_json::from_slice::<SpotifyTokens>(value.value())?;
            let _ = tokens.insert_sync(key.value(), parsed);
        }
    }
    {
        let table = txn.open_table(OAUTH_STATES).map_err(AppError::database)?;
        for entry in table.iter().map_err(AppError::database)? {
            let (key, value) = entry.map_err(AppError::database)?;
            let parsed = serde_json::from_slice::<OauthState>(value.value())?;
            let _ = oauth.insert_sync(CompactString::from(key.value()), parsed);
        }
    }
    {
        let table = txn.open_table(ALLOWLIST).map_err(AppError::database)?;
        for entry in table.iter().map_err(AppError::database)? {
            let (key, value) = entry.map_err(AppError::database)?;
            let _ = allowlist.insert_sync(key.value(), value.value());
        }
    }
    Ok(())
}

fn persist_user(
    db: &Database,
    telegram_user_id: u64,
    tokens: Option<&SpotifyTokens>,
) -> Result<bool> {
    let bytes = tokens.map(serde_json::to_vec).transpose()?;
    let txn = db.begin_write().map_err(AppError::database)?;
    let existed = {
        let mut table = txn.open_table(USERS).map_err(AppError::database)?;
        match bytes {
            Some(bytes) => {
                table
                    .insert(telegram_user_id, bytes.as_slice())
                    .map_err(AppError::database)?;
                true
            }
            None => table
                .remove(telegram_user_id)
                .map_err(AppError::database)?
                .is_some(),
        }
    };
    txn.commit().map_err(AppError::database)?;
    Ok(existed)
}

fn persist_oauth(db: &Database, state: &str, value: Option<&OauthState>) -> Result<bool> {
    let bytes = value.map(serde_json::to_vec).transpose()?;
    let txn = db.begin_write().map_err(AppError::database)?;
    let existed = {
        let mut table = txn.open_table(OAUTH_STATES).map_err(AppError::database)?;
        match bytes {
            Some(bytes) => {
                table
                    .insert(state, bytes.as_slice())
                    .map_err(AppError::database)?;
                true
            }
            None => table.remove(state).map_err(AppError::database)?.is_some(),
        }
    };
    txn.commit().map_err(AppError::database)?;
    Ok(existed)
}

fn persist_allow(db: &Database, telegram_user_id: u64, insert: bool) -> Result<()> {
    let txn = db.begin_write().map_err(AppError::database)?;
    {
        let mut table = txn.open_table(ALLOWLIST).map_err(AppError::database)?;
        if insert {
            table
                .insert(telegram_user_id, 1u8)
                .map_err(AppError::database)?;
        } else {
            table.remove(telegram_user_id).map_err(AppError::database)?;
        }
    }
    txn.commit().map_err(AppError::database)?;
    Ok(())
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("bot.redb")).unwrap();
        (dir, store)
    }

    fn tokens(access: &str, refresh: &str, expires_at: i64) -> SpotifyTokens {
        SpotifyTokens {
            access_token: access.into(),
            refresh_token: refresh.into(),
            expires_at,
            token_type: "Bearer".into(),
            scope: "user-read-currently-playing".into(),
        }
    }

    #[tokio::test]
    async fn save_load_delete_and_refresh_update() {
        let (_dir, store) = store();
        let user = 42_u64;
        store
            .put_tokens(user, &tokens("a1", "r1", 100))
            .await
            .unwrap();
        assert_eq!(
            store.get_tokens(user).await.unwrap().unwrap().access_token,
            "a1"
        );

        store
            .put_tokens(user, &tokens("a2", "r2", 200))
            .await
            .unwrap();
        let updated = store.get_tokens(user).await.unwrap().unwrap();
        assert_eq!(updated.access_token, "a2");
        assert_eq!(updated.refresh_token, "r2");
        assert_eq!(updated.expires_at, 200);

        assert!(store.delete_tokens(user).await.unwrap());
        assert!(store.get_tokens(user).await.unwrap().is_none());
        assert!(!store.delete_tokens(user).await.unwrap());
    }

    #[tokio::test]
    async fn oauth_state_is_single_use_and_expires() {
        let (_dir, store) = store();
        store
            .put_oauth_state(
                "abc",
                &OauthState {
                    telegram_user_id: 7,
                    code_verifier: "verifier".into(),
                    created_at: now_unix(),
                },
            )
            .await
            .unwrap();

        let taken = store.take_oauth_state("abc", 900).await.unwrap();
        assert_eq!(taken.telegram_user_id, 7);
        assert!(store.take_oauth_state("abc", 900).await.is_err());

        store
            .put_oauth_state(
                "old",
                &OauthState {
                    telegram_user_id: 8,
                    code_verifier: "v".into(),
                    created_at: now_unix() - 10_000,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store.take_oauth_state("old", 900).await,
            Err(AppError::InvalidOauthState)
        ));
    }

    #[tokio::test]
    async fn allowlist_add_list_and_remove() {
        let (_dir, store) = store();
        assert!(!store.is_allowlisted(9).await.unwrap());
        assert!(store.allow_user(9).await.unwrap());
        assert!(!store.allow_user(9).await.unwrap());
        assert!(store.is_allowlisted(9).await.unwrap());
        store.allow_user(3).await.unwrap();
        assert_eq!(store.list_allowlist().await.unwrap(), vec![3, 9]);
        assert!(store.deny_user(9).await.unwrap());
        assert!(!store.is_allowlisted(9).await.unwrap());
        assert!(!store.deny_user(9).await.unwrap());
    }

    #[tokio::test]
    async fn reopens_disk_into_cache() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bot.redb");
        {
            let store = Store::open(&path).unwrap();
            store.put_tokens(1, &tokens("a", "r", 1)).await.unwrap();
            store.allow_user(9).await.unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get_tokens(1).await.unwrap().unwrap().access_token,
            "a"
        );
        assert!(store.is_allowlisted(9).await.unwrap());
    }
}
