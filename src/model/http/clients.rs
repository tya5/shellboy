//! Immutable owner-scoped clients, one entry per configured connection.
use super::{Config, HttpModelClient, ModelError};
use std::collections::BTreeMap;

pub(crate) struct HttpClients {
    entries: BTreeMap<String, Result<HttpModelClient, ModelError>>,
}
impl HttpClients {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
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
        }
    }
    pub(crate) fn get(&self, name: &str) -> Result<HttpModelClient, ModelError> {
        self.entries.get(name).cloned().unwrap_or_else(|| {
            Err(ModelError::new(
                "config_missing",
                "Connection is not configured",
            ))
        })
    }
}

#[cfg(test)]
mod tests;
