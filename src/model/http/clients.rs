//! Owner-scoped connection registry. Readers retain one immutable generation.
use super::{Config, HttpModelClient, ModelError};
use crate::config::Connection;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock, RwLock},
};

pub(crate) struct ClientSnapshot {
    pub(crate) connections: BTreeMap<String, Connection>,
    pub(crate) default_connection: Option<String>,
    pub(crate) default_model: Option<String>,
    entries: BTreeMap<String, Result<HttpModelClient, ModelError>>,
}
impl ClientSnapshot {
    pub(crate) fn get(&self, name: &str) -> Result<HttpModelClient, ModelError> {
        self.entries.get(name).cloned().unwrap_or_else(|| {
            Err(ModelError::new(
                "config_invalid",
                "Connection is not configured",
            ))
        })
    }
}
pub(crate) struct HttpClients {
    current: RwLock<Arc<ClientSnapshot>>,
    diagnostics: OnceLock<crate::diagnostics::Diagnostics>,
}
impl HttpClients {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            current: RwLock::new(Arc::new(ClientSnapshot {
                connections: config.connections.clone(),
                default_connection: config.default_connection.clone(),
                default_model: config.default_model.clone(),
                entries: config
                    .connections
                    .iter()
                    .map(|(name, connection)| {
                        (
                            name.clone(),
                            HttpModelClient::for_connection(name, connection.clone()),
                        )
                    })
                    .collect(),
            })),
            diagnostics: OnceLock::new(),
        }
    }
    pub(crate) fn snapshot(&self) -> Arc<ClientSnapshot> {
        self.current.read().unwrap().clone()
    }
    /// Called under config_write, after durable replacement and before releasing it.
    pub(crate) fn adopt(&self, config: &Config, embedding: Option<&str>) {
        let previous = self.snapshot();
        let mut connections = config.connections.clone();
        // The index profile key belongs to the startup embedding definition.
        if let Some(name) = embedding
            && let Some(connection) = previous.connections.get(name)
        {
            connections.insert(name.to_owned(), connection.clone());
        }
        let entries = connections
            .iter()
            .map(|(name, connection)| {
                let client = if previous
                    .connections
                    .get(name)
                    .is_some_and(|old| crate::config_edit::same_connection(old, connection))
                {
                    previous.get(name)
                } else {
                    HttpModelClient::for_connection(name, connection.clone())
                };
                if let (Ok(client), Some(diagnostics)) = (&client, self.diagnostics.get()) {
                    let _ = client.diagnostics.set(diagnostics.clone());
                }
                (name.clone(), client)
            })
            .collect();
        *self.current.write().unwrap() = Arc::new(ClientSnapshot {
            connections,
            default_connection: config.default_connection.clone(),
            default_model: config.default_model.clone(),
            entries,
        });
    }
    pub(crate) fn set_diagnostics(&self, diagnostics: crate::diagnostics::Diagnostics) {
        let _ = self.diagnostics.set(diagnostics.clone());
        for client in self.snapshot().entries.values().flatten() {
            let _ = client.diagnostics.set(diagnostics.clone());
        }
    }
    pub(crate) fn get(&self, name: &str) -> Result<HttpModelClient, ModelError> {
        self.snapshot().get(name)
    }
}

#[cfg(test)]
mod tests;
