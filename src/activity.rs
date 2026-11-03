//! Shared saved-work activity projection for CLI and human displays.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityPhase {
    #[default]
    Idle,
    Sending,
    Reflecting,
    Preparing,
    AwaitingModel,
    Retrying,
    Receiving,
    Tools,
    Running,
    Waiting,
    Background,
    Continuing,
    Stopping,
    Recovery,
    Failed,
    Stopped,
    Completed,
}
impl ActivityPhase {
    /// Bounded table label; full descriptions remain available in details.
    pub fn column_label(self) -> &'static str {
        match self {
            Self::Idle => "未実行",
            Self::Sending => "送信中",
            Self::Reflecting => "取得中",
            Self::Preparing => "準備中",
            Self::AwaitingModel => "応答待ち",
            Self::Retrying => "再試行",
            Self::Receiving => "受信中",
            Self::Tools => "ツール中",
            Self::Running => "処理中",
            Self::Waiting => "待機中",
            Self::Background => "背景処理",
            Self::Continuing => "継続中",
            Self::Stopping => "停止中",
            Self::Recovery => "復旧待ち",
            Self::Failed => "異常終了",
            Self::Stopped => "中断済み",
            Self::Completed => "完了",
        }
    }

    pub fn column_label_for(self, language: crate::config::UiLanguage) -> &'static str {
        if language == crate::config::UiLanguage::Ja {
            return self.column_label();
        }
        match self {
            Self::Idle => "Idle",
            Self::Sending => "Sending",
            Self::Reflecting => "Loading",
            Self::Preparing => "Preparing",
            Self::AwaitingModel => "Awaiting",
            Self::Retrying => "Retrying",
            Self::Receiving => "Receiving",
            Self::Tools => "Tools",
            Self::Running => "Running",
            Self::Waiting => "Waiting",
            Self::Background => "Background",
            Self::Continuing => "Continuing",
            Self::Stopping => "Stopping",
            Self::Recovery => "Recovery",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Completed => "Done",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "入力できます",
            Self::Sending => "送信受付中",
            Self::Reflecting => "保存済み・取得中",
            Self::Preparing => "準備中",
            Self::AwaitingModel => "モデル応答待ち",
            Self::Retrying => "一時的なエラーのため再試行中",
            Self::Receiving => "応答受信中",
            Self::Tools => "ツール処理中",
            Self::Running => "処理中",
            Self::Waiting => "待機中",
            Self::Background => "回答済み背景処理中",
            Self::Continuing => "実行継続中",
            Self::Stopping => "停止処理中",
            Self::Recovery => "復旧待ち",
            Self::Failed => "異常終了",
            Self::Stopped => "中断済み",
            Self::Completed => "完了",
        }
    }

    pub fn label_for(self, language: crate::config::UiLanguage) -> &'static str {
        if language == crate::config::UiLanguage::Ja {
            return self.label();
        }
        match self {
            Self::Idle => "Ready for input",
            Self::Sending => "Accepting input",
            Self::Reflecting => "Loading saved input",
            Self::Preparing => "Preparing",
            Self::AwaitingModel => "Waiting for the model",
            Self::Retrying => "Retrying after a temporary error",
            Self::Receiving => "Receiving a response",
            Self::Tools => "Running tools",
            Self::Running => "Running",
            Self::Waiting => "Waiting",
            Self::Background => "Answered; background work continues",
            Self::Continuing => "Continuing",
            Self::Stopping => "Stopping",
            Self::Recovery => "Recovery needed",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Completed => "Completed",
        }
    }
}

/// Pure projection of saved facts; never a lifecycle transition.
pub fn classify(
    run: Option<&str>,
    turn: Option<&str>,
    owned: bool,
    inflight: bool,
    retrying: bool,
    tools: bool,
) -> ActivityPhase {
    use ActivityPhase::*;
    match (run, turn) {
        (None, _) => Idle,
        (Some("stopping"), _) => Stopping,
        (Some("failed"), _) => Failed,
        (Some("stopped"), _) => Stopped,
        (Some("completed"), _) => Completed,
        (_, Some("recovery_wait")) => Recovery,
        (_, Some("preparing")) => Preparing,
        (_, Some("waiting")) => Waiting,
        (_, Some("running")) if inflight => AwaitingModel,
        (Some("live"), Some("running")) if retrying => Retrying,
        (_, Some("running")) if tools => Tools,
        (_, Some("running")) => Running,
        (Some("live"), Some("completed")) if owned => Background,
        (_, Some("failed")) => Failed,
        (_, Some("stopped")) => Stopped,
        (Some("live"), _) => Continuing,
        _ => Completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_lifecycle_precedence_and_running_refinements() {
        use ActivityPhase::*;
        for (run, turn, owned, expected) in [
            (None, None, false, Idle),
            (Some("live"), Some("completed"), true, Background),
            (Some("live"), Some("completed"), false, Continuing),
            (Some("live"), None, true, Continuing),
            (Some("stopping"), Some("completed"), true, Stopping),
            (Some("completed"), Some("running"), true, Completed),
            (Some("failed"), Some("running"), true, Failed),
            (Some("stopped"), Some("recovery_wait"), true, Stopped),
            (Some("live"), Some("recovery_wait"), true, Recovery),
            (Some("live"), Some("waiting"), false, Waiting),
            (Some("live"), Some("preparing"), false, Preparing),
        ] {
            assert_eq!(classify(run, turn, owned, false, false, false), expected);
        }
        assert_eq!(
            classify(Some("live"), Some("running"), true, true, true, true),
            AwaitingModel
        );
        assert_eq!(
            classify(Some("live"), Some("running"), true, false, true, true),
            Retrying
        );
        assert_eq!(
            classify(Some("live"), Some("running"), true, false, false, true),
            Tools
        );
        assert_eq!(
            classify(Some("live"), Some("running"), true, false, false, false),
            Running
        );
        assert_eq!(serde_json::to_value(Background).unwrap(), "background");
        assert_eq!(Idle.column_label_for(crate::config::UiLanguage::En), "Idle");
        assert_eq!(
            Idle.label_for(crate::config::UiLanguage::En),
            "Ready for input"
        );
        assert_eq!(
            Idle.column_label_for(crate::config::UiLanguage::Ja),
            "未実行"
        );
        assert_eq!(
            Idle.label_for(crate::config::UiLanguage::Ja),
            "入力できます"
        );
    }
}
