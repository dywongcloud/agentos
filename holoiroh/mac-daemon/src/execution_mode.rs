use clap::ValueEnum;
use holoiroh_wire::ClientMessage;

pub const RESTRICTED_CAPABILITIES: &[&str] = &[
    "plan_task",
    "clarify_request",
    "observation_media",
    "signed_remote_control",
];

pub const LEGACY_HOLO_CAPABILITIES: &[&str] = &[
    "plan_task",
    "clarify_request",
    "observation_media",
    "signed_remote_control",
    "autonomous_holo",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExecutionMode {
    #[default]
    Restricted,
    LegacyHolo,
}

impl ExecutionMode {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Restricted => "restricted",
            Self::LegacyHolo => "legacy_holo",
        }
    }

    pub const fn capabilities(self) -> &'static [&'static str] {
        match self {
            Self::Restricted => RESTRICTED_CAPABILITIES,
            Self::LegacyHolo => LEGACY_HOLO_CAPABILITIES,
        }
    }

    pub fn admits(self, message: &ClientMessage) -> bool {
        self == Self::LegacyHolo
            || !matches!(
                message,
                ClientMessage::Prompt { .. }
                    | ClientMessage::VoiceTranscript { .. }
                    | ClientMessage::Redirect { .. }
                    | ClientMessage::Resume
                    | ClientMessage::InputResponse { .. }
            )
    }
}
