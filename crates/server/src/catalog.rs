//! Models offered when starting a case: each LLM's default model, the suggestions from its
//! configuration, and the models its provider reports (e.g. OpenRouter's catalog).
//!
//! Provider lists are fetched by a background thread, never by a request handler, so the
//! API stays fast and keeps working with the last known list when a provider is down.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use clankjob_core::case::Usage;
use clankjob_core::llm::{LlmProvider, ModelInfo};
use serde::Serialize;
use serde_json::{Value, json};

/// A configured LLM a case can be started with, as returned by `GET /llms`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmChoice {
    /// Name of the `[llm.<name>]` block.
    pub name: String,
    /// Model used when the case names none.
    pub model: String,
    /// Models to offer: the default, then configured suggestions, then discovered ones.
    pub models: Vec<ModelInfo>,
}

/// One configured LLM as the catalog sees it.
pub struct CatalogLlm {
    /// Name of the `[llm.<name>]` block.
    pub name: String,
    /// Its default model.
    pub model: String,
    /// Its configured `models` suggestions.
    pub suggested: Vec<String>,
    /// The provider to ask for its models, or `None` when discovery is off.
    pub provider: Option<Arc<dyn LlmProvider>>,
}

/// The models each configured LLM offers.
pub struct Catalog {
    llms: Vec<CatalogLlm>,
    discovered: RwLock<HashMap<String, Vec<ModelInfo>>>,
}

impl Catalog {
    /// Create a catalog. Nothing is fetched until [`Catalog::refresh`].
    #[must_use]
    pub fn new(llms: Vec<CatalogLlm>) -> Self {
        Self {
            llms,
            discovered: RwLock::new(HashMap::new()),
        }
    }

    /// Ask every provider with discovery enabled for its models.
    ///
    /// Blocks on the network. A provider that fails keeps its previous list.
    pub fn refresh(&self) {
        for llm in &self.llms {
            let Some(provider) = &llm.provider else { continue };
            match provider.list_models() {
                Ok(models) => {
                    tracing::info!(llm = %llm.name, count = models.len(), "model list refreshed");
                    self.discovered
                        .write()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(llm.name.clone(), models);
                }
                Err(error) => tracing::warn!(llm = %llm.name, %error, "cannot list models; keeping the last list"),
            }
        }
    }

    /// What is known about the model a case uses: its own `model`, or the LLM's default.
    ///
    /// # Returns
    ///
    /// The model id and its catalog entry (prices, context size) when the provider
    /// listed it; `None` for an LLM that is not configured
    #[must_use]
    pub fn model(&self, llm: &str, model: Option<&str>) -> Option<(String, Option<ModelInfo>)> {
        let configured = self.llms.iter().find(|candidate| candidate.name == llm)?;
        let id = model.unwrap_or(&configured.model).to_owned();
        let discovered = self.discovered.read().unwrap_or_else(PoisonError::into_inner);
        let info = discovered
            .get(llm)
            .and_then(|models| models.iter().find(|candidate| candidate.id == id))
            .cloned();
        Some((id, info))
    }

    /// Every configured LLM with the models to offer for it.
    #[must_use]
    pub fn choices(&self) -> Vec<LlmChoice> {
        let discovered = self.discovered.read().unwrap_or_else(PoisonError::into_inner);
        self.llms
            .iter()
            .map(|llm| {
                let found = discovered.get(&llm.name).map_or(&[][..], Vec::as_slice);
                let known = |id: &str| {
                    found
                        .iter()
                        .find(|model| model.id == id)
                        .cloned()
                        .unwrap_or_else(|| ModelInfo::from_id(id))
                };
                let mut models: Vec<ModelInfo> = Vec::with_capacity(found.len().saturating_add(1));
                for id in std::iter::once(&llm.model).chain(&llm.suggested) {
                    if !models.iter().any(|model| &model.id == id) {
                        models.push(known(id));
                    }
                }
                let others: Vec<ModelInfo> = found
                    .iter()
                    .filter(|model| !models.iter().any(|listed| listed.id == model.id))
                    .cloned()
                    .collect();
                models.extend(others);
                LlmChoice {
                    name: llm.name.clone(),
                    model: llm.model.clone(),
                    models,
                }
            })
            .collect()
    }
}

/// Estimated cost of a case so far, in US dollars, for `GET /cases/{id}`.
///
/// Tokens used times the model's current prices per million tokens. Cached-token
/// discounts and price changes during the case are not accounted for, hence "estimate".
#[must_use]
pub fn cost_estimate(usage: &Usage, model: Option<(String, Option<ModelInfo>)>) -> Value {
    let Some((id, info)) = model else {
        return json!({ "usd": null, "reason": "the case's LLM is not configured" });
    };
    let prices = info.and_then(|info| Some((info.input_price?, info.output_price?)));
    let Some((input_price, output_price)) = prices else {
        return json!({
            "usd": null,
            "model": id,
            "reason": "no price known for this model (only providers that list prices, such as OpenRouter, give one)",
        });
    };
    // Token counts stay far below 2^53, where f64 starts losing whole numbers.
    #[expect(clippy::cast_precision_loss, reason = "token counts are far below 2^53")]
    let (input, output) = (usage.input_tokens as f64, usage.output_tokens as f64);
    let usd = (input * input_price + output * output_price) / 1_000_000.0;
    json!({
        "usd": usd,
        "model": id,
        "input_price": input_price,
        "output_price": output_price,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
    })
}

/// Refresh the catalog now and then every `interval`, on a background thread.
///
/// The thread is detached: it holds no state worth saving, and the process exiting stops it.
///
/// # Errors
///
/// Returns an I/O error if the thread cannot be created.
pub fn spawn_refresher(catalog: Arc<Catalog>, interval: Duration) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new().name("model-catalog".to_owned()).spawn(move || {
        loop {
            catalog.refresh();
            thread::sleep(interval);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use clankjob_core::llm::{CompletionRequest, CompletionResponse, LlmError};

    use super::*;

    /// A provider whose model list is set by the test.
    struct Listing(Mutex<Result<Vec<ModelInfo>, LlmError>>);

    impl LlmProvider for Listing {
        fn default_model(&self) -> &'static str {
            "unused"
        }

        fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
            Err(LlmError::Fatal("not used".to_owned()))
        }

        fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
            self.0.lock().unwrap().clone()
        }
    }

    fn priced(id: &str, price: f64) -> ModelInfo {
        ModelInfo {
            input_price: Some(price),
            ..ModelInfo::from_id(id)
        }
    }

    fn catalog(provider: Option<Arc<Listing>>) -> Catalog {
        Catalog::new(vec![CatalogLlm {
            name: "default".to_owned(),
            model: "b/default".to_owned(),
            suggested: vec!["c/pinned".to_owned(), "b/default".to_owned()],
            provider: provider.map(|provider| provider as Arc<dyn LlmProvider>),
        }])
    }

    fn ids(choice: &LlmChoice) -> Vec<&str> {
        choice.models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn cost_is_tokens_times_the_case_models_prices() {
        // Arrange
        let listing = Arc::new(Listing(Mutex::new(Ok(vec![ModelInfo {
            output_price: Some(1.6),
            ..priced("b/default", 0.4)
        }]))));
        let catalog = catalog(Some(listing));
        catalog.refresh();
        let usage = Usage {
            activations: 1,
            input_tokens: 1_000_000,
            output_tokens: 500_000,
        };

        // Act
        let priced_cost = cost_estimate(&usage, catalog.model("default", None));
        let unpriced = cost_estimate(&usage, catalog.model("default", Some("c/pinned")));
        let unknown_llm = cost_estimate(&usage, catalog.model("other", None));

        // Assert
        assert!((priced_cost["usd"].as_f64().unwrap() - 1.2).abs() < 1e-9);
        assert_eq!(priced_cost["model"], "b/default");
        assert_eq!(
            (unpriced["usd"].clone(), unpriced["model"].clone()),
            (Value::Null, json!("c/pinned"))
        );
        assert!(unknown_llm["reason"].as_str().unwrap().contains("not configured"));
    }

    #[test]
    fn without_discovery_only_configured_models_are_offered() {
        let choices = catalog(None).choices();

        assert_eq!(ids(&choices[0]), ["b/default", "c/pinned"]);
    }

    #[test]
    fn discovered_models_follow_configured_ones_and_lend_them_prices() {
        // Arrange
        let listing = Arc::new(Listing(Mutex::new(Ok(vec![
            priced("a/other", 1.0),
            priced("b/default", 0.4),
        ]))));
        let catalog = catalog(Some(listing));

        // Act
        catalog.refresh();
        let choices = catalog.choices();

        // Assert
        assert_eq!(ids(&choices[0]), ["b/default", "c/pinned", "a/other"]);
        assert_eq!(choices[0].models[0].input_price, Some(0.4));
        assert_eq!(choices[0].models[1].input_price, None);
    }

    #[test]
    fn a_failed_refresh_keeps_the_previous_list() {
        let listing = Arc::new(Listing(Mutex::new(Ok(vec![ModelInfo::from_id("a/other")]))));
        let catalog = catalog(Some(Arc::clone(&listing)));
        catalog.refresh();

        *listing.0.lock().unwrap() = Err(LlmError::Retryable("offline".to_owned()));
        catalog.refresh();

        assert_eq!(ids(&catalog.choices()[0]), ["b/default", "c/pinned", "a/other"]);
    }
}
