//! Local file-owner Vault, using the native RDVX envelope and auth KV payload.
//! The sealed image is an internal Vault collection, never an ordinary SQL table.
//! Its WAL action joins the caller's SQL transaction; closing drops pending writes.

use super::*;
use crate::catalog::CollectionModel;
use crate::storage::entity::{EntityId, RowData, UnifiedEntity};
use reddb_crypto::{
    aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt},
    os_random,
};
use zeroize::Zeroizing;

const COLLECTION: &str = "red.local.vault";
const PAYLOAD_BYTES_MAX: usize = 1024 * 1024;

/// Session-owned opening key. Contains no password and zeroizes on drop.
/// A random persisted salt binds it to one Vault identity across file reopen.
pub struct LocalVaultKey {
    key: Zeroizing<[u8; 32]>,
    salt: [u8; 16],
}

fn failure(code: &'static str) -> LocalReadError {
    LocalReadError::Vault(code)
}

impl LocalVaultKey {
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.len() > PAYLOAD_BYTES_MAX {
            return Err(failure("VAULT_SIZE_LIMIT"));
        }
        let mut nonce = [0u8; 12];
        os_random::fill_bytes(&mut nonce).map_err(|_| failure("VAULT_RANDOM_FAILED"))?;
        let ciphertext = aes256_gcm_encrypt(
            &self.key,
            &nonce,
            reddb_file::VAULT_LOGICAL_EXPORT_AAD,
            plaintext,
        );
        hex::decode(reddb_file::encode_vault_logical_export(
            &self.salt,
            &nonce,
            &ciphertext,
        ))
        .map_err(|_| failure("VAULT_CORRUPT"))
    }

    fn unseal(&self, bytes: &[u8]) -> Result<Zeroizing<String>> {
        if bytes.len() > PAYLOAD_BYTES_MAX + 64 {
            return Err(failure("VAULT_SIZE_LIMIT"));
        }
        let envelope = reddb_file::decode_vault_logical_export(&hex::encode(bytes))
            .map_err(|_| failure("VAULT_CORRUPT"))?;
        if envelope.salt != self.salt {
            return Err(failure("VAULT_LOCKED"));
        }
        let plaintext = Zeroizing::new(
            aes256_gcm_decrypt(
                &self.key,
                &envelope.nonce,
                reddb_file::VAULT_LOGICAL_EXPORT_AAD,
                &envelope.ciphertext,
            )
            .map_err(|_| failure("VAULT_AUTHENTICATION_FAILED"))?,
        );
        let text = std::str::from_utf8(&plaintext).map_err(|_| failure("VAULT_CORRUPT"))?;
        Ok(Zeroizing::new(text.to_string()))
    }
}

fn key_valid(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
        return Err(failure("VAULT_INVALID_KEY"));
    }
    Ok(())
}

/// Preserve all non-KV records (MASTER_SECRET, USER, KEY, SEALED and future
/// auth fields) verbatim. A local credential write must never drop auth state.
fn payload_entries(payload: &str) -> Result<HashMap<String, Zeroizing<String>>> {
    let mut entries = HashMap::new();
    for line in payload.lines() {
        if let Some(record) = line.strip_prefix("KV:") {
            let (key, value) = record
                .split_once('\t')
                .ok_or_else(|| failure("VAULT_CORRUPT"))?;
            key_valid(key)?;
            let bytes = Zeroizing::new(hex::decode(value).map_err(|_| failure("VAULT_CORRUPT"))?);
            let value = std::str::from_utf8(&bytes).map_err(|_| failure("VAULT_CORRUPT"))?;
            if entries
                .insert(key.into(), Zeroizing::new(value.into()))
                .is_some()
            {
                return Err(failure("VAULT_CORRUPT"));
            }
        }
    }
    Ok(entries)
}

impl<F: VfsFile> LocalDatabase<F> {
    fn vault_blob(&self) -> Result<Option<Vec<u8>>> {
        self.check_healthy()?;
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.aborted)
        {
            return Err(query_error("transaction is aborted; ROLLBACK is required"));
        }
        let (resident, _) = self.visible()?;
        let Some(manager) = resident.get_collection(COLLECTION) else {
            return Ok(None);
        };
        let entity = manager
            .get(EntityId::new(1))
            .ok_or_else(|| failure("VAULT_CORRUPT"))?;
        match entity
            .data
            .as_row()
            .and_then(|row| row.get_by_name("value"))
        {
            Some(Value::Secret(bytes)) => Ok(Some(bytes.clone())),
            _ => Err(failure("VAULT_CORRUPT")),
        }
    }

    /// Metadata only; no key derivation and no credential material returned.
    pub fn vault_initialized(&self) -> Result<bool> {
        Ok(self.vault_blob()?.is_some())
    }

    /// Creation is explicit and only admitted outside a transaction. Existing
    /// files are authenticated first; wrong passwords never replace saved state.
    pub fn vault_unlock(&mut self, password: &str, create: bool) -> Result<LocalVaultKey> {
        let blob = self.vault_blob()?;
        let mut salt = [0u8; 16];
        if let Some(bytes) = &blob {
            salt = reddb_file::decode_vault_logical_export(&hex::encode(bytes))
                .map_err(|_| failure("VAULT_CORRUPT"))?
                .salt;
        } else {
            if !create {
                return Err(failure("VAULT_NOT_INITIALIZED"));
            }
            if self.transaction.is_some() {
                return Err(query_error("initialize Vault before BEGIN"));
            }
            os_random::fill_bytes(&mut salt).map_err(|_| failure("VAULT_RANDOM_FAILED"))?;
        }
        let key = LocalVaultKey {
            salt,
            key: reddb_crypto::vault::passphrase_key(password, &salt)
                .map_err(|_| failure("VAULT_INVALID_PASSWORD"))?,
        };
        if let Some(bytes) = blob {
            payload_entries(&key.unseal(&bytes)?)?;
        } else {
            let (resident, contracts) = self.visible()?;
            let staged = copy_resident(resident)?;
            let mut contracts = contracts.clone();
            apply(
                &staged,
                &[StoreWalAction::CreateCollection {
                    name: COLLECTION.into(),
                }],
            )?;
            let QueryExpr::CreateTable(query) =
                reddb_rql::parse_frontend("CREATE TABLE local_vault (value TEXT)")
                    .map_err(LocalReadError::Parse)?
                    .into_query_expr()
            else {
                unreachable!("static DDL")
            };
            let mut contract =
                crate::catalog::ddl::collection_contract_from_create_table(&query, 0)
                    .map_err(query_error)?;
            contract.name = COLLECTION.into();
            contract.declared_model = CollectionModel::Vault;
            contract.table_def = None;
            contract.declared_columns.clear();
            contracts.insert(COLLECTION.into(), contract);
            set_contracts(&staged, &contracts);
            apply(&staged, &[vault_action(key.seal(b"SEALED:false\n")?)])?;
            self.store.publish_snapshot(&snapshot(&staged))?;
            self.contracts = contracts;
        }
        Ok(key)
    }

    pub fn vault_keys(&self, key: &LocalVaultKey) -> Result<Vec<String>> {
        let blob = self
            .vault_blob()?
            .ok_or_else(|| failure("VAULT_NOT_INITIALIZED"))?;
        let mut keys: Vec<_> = payload_entries(&key.unseal(&blob)?)?.into_keys().collect();
        keys.sort();
        Ok(keys)
    }

    /// Plaintext is returned only through this explicit reveal operation.
    pub fn vault_reveal(&self, key: &LocalVaultKey, name: &str) -> Result<Option<String>> {
        key_valid(name)?;
        let blob = self
            .vault_blob()?
            .ok_or_else(|| failure("VAULT_NOT_INITIALIZED"))?;
        Ok(payload_entries(&key.unseal(&blob)?)?
            .remove(name)
            .map(|value| value.to_string()))
    }

    /// Mutations stage an encrypted native action alongside ordinary SQL.
    /// Errors abort an active transaction, just like failed SQL statements.
    pub fn vault_put(
        &mut self,
        key: &LocalVaultKey,
        name: &str,
        value: Option<&str>,
    ) -> Result<()> {
        let result = self.vault_put_inner(key, name, value);
        if result.is_err() {
            if let Some(transaction) = &mut self.transaction {
                transaction.aborted = true;
            }
        }
        result
    }

    fn vault_put_inner(
        &mut self,
        key: &LocalVaultKey,
        name: &str,
        value: Option<&str>,
    ) -> Result<()> {
        key_valid(name)?;
        if value.is_some_and(|value| value.len() > PAYLOAD_BYTES_MAX / 2) {
            return Err(failure("VAULT_SIZE_LIMIT"));
        }
        let blob = self
            .vault_blob()?
            .ok_or_else(|| failure("VAULT_NOT_INITIALIZED"))?;
        let plaintext = key.unseal(&blob)?;
        let mut entries = payload_entries(&plaintext)?;
        match value {
            Some(value) => {
                entries.insert(name.into(), Zeroizing::new(value.into()));
            }
            None => {
                entries.remove(name);
            }
        }
        let mut updated = Zeroizing::new(String::new());
        for line in plaintext.lines().filter(|line| !line.starts_with("KV:")) {
            updated.push_str(line);
            updated.push('\n');
        }
        for (name, value) in entries {
            updated.push_str("KV:");
            updated.push_str(&name);
            updated.push('\t');
            let encoded = Zeroizing::new(hex::encode(value.as_bytes()));
            updated.push_str(&encoded);
            updated.push('\n');
        }
        let action = vault_action(key.seal(updated.as_bytes())?);
        if let Some(transaction) = &mut self.transaction {
            if transaction.actions.len() >= super::super::local_read::LOCAL_READ_ROWS_MAX {
                return Err(LocalReadError::ResourceLimit);
            }
            apply(&transaction.resident, std::slice::from_ref(&action))?;
            transaction.actions.push(action);
        } else {
            self.store.publish_actions(&[action])?;
        }
        Ok(())
    }
}

fn vault_action(blob: Vec<u8>) -> StoreWalAction {
    let mut entity = UnifiedEntity::table_row(EntityId::new(1), COLLECTION, 1, Vec::new());
    entity.data = crate::storage::entity::EntityData::Row(RowData::with_names(
        vec![Value::Secret(blob)],
        vec!["value".into()],
    ));
    StoreWalAction::upsert_entity(COLLECTION, &entity, None, reddb_file::STORE_VERSION_CURRENT)
}
