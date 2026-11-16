use super::*;

#[derive(Debug, Default)]
pub(super) struct ModelSelection {
    pub(super) selected: Option<String>,
    default: Option<String>,
    options: Vec<ChatRunModel>,
    loading: Option<String>,
}

impl ModelSelection {
    pub(super) fn requested_model(&self) -> Option<&str> {
        self.selected.as_deref().or(self.default.as_deref())
    }

    pub(super) fn selection_unavailable(&self) -> bool {
        self.default.is_some()
            && self
                .selected
                .as_deref()
                .is_some_and(|selected| !self.options.iter().any(|m| m.model_id == selected))
    }
}

impl Chats {
    pub(in crate::model) fn request_models(
        &mut self,
        request_id: String,
    ) -> Option<ChatRunRequest> {
        if !self.async_chat || self.models.loading.is_some() {
            return None;
        }
        self.models.loading = Some(request_id.clone());
        Some(ChatRunRequest::ChatRunModels { request_id })
    }

    pub(in crate::model) fn accept_models(&mut self, response: &ChatRunResponse) -> bool {
        let Some(request_id) = self
            .models
            .loading
            .take_if(|id| id == response.request_id())
        else {
            return false;
        };
        let request = ChatRunRequest::ChatRunModels { request_id };
        if let ChatRunResponse::ChatRunModelsListed {
            default_model_id,
            models,
            ..
        } = response
        {
            if response.is_valid_for(&request) {
                self.models.default = Some(default_model_id.clone());
                self.models.options = models.clone();
            }
        }
        true
    }

    pub(in crate::model) fn models_network_failed(&mut self, request_id: &str) {
        self.models.loading.take_if(|id| id == request_id);
    }

    pub(in crate::model) fn select_model(&mut self, value: String) -> Result<(), UiError> {
        if !self.async_chat
            || self.workbench
            || self.pending.is_some()
            || !self
                .models
                .options
                .iter()
                .any(|model| model.model_id == value)
        {
            return Err(UiError::InvalidAction);
        }
        self.models.selected = Some(value);
        Ok(())
    }

    pub(super) fn project_models(&self, view: &mut ViewModel, connected: bool) {
        view.visibility.push(VisibilityBinding {
            id: "chat-model-choice",
            visible: connected && self.async_chat,
        });
        if !connected || !self.async_chat {
            return;
        }
        let value = self.models.requested_model().unwrap_or_default();
        let mut options: Vec<_> = self
            .models
            .options
            .iter()
            .map(|model| OptionView {
                value: model.model_id.clone(),
                label: format!(
                    "{} · {}",
                    model.label,
                    if model.ready {
                        "loaded"
                    } else {
                        "loads on request"
                    }
                ),
            })
            .collect();
        if !options.iter().any(|option| option.value == value) {
            let label = if self.models.selection_unavailable() {
                format!("{value} · unavailable")
            } else if !value.is_empty() {
                value.to_owned()
            } else if self.models.loading.is_some() {
                "Loading models…".into()
            } else {
                "Server default".into()
            };
            options.insert(
                0,
                OptionView {
                    value: value.into(),
                    label,
                },
            );
        }
        view.choices.push(SelectBinding {
            id: "chat-model",
            value: value.into(),
            disabled: self.models.options.is_empty() || self.pending.is_some(),
            options,
        });
    }
}
