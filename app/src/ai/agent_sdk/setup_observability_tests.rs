use super::{SetupScope, SetupStep};

/// Expected scope classification for every setup step.
const EXPECTED: &[(SetupStep, SetupScope)] = &[
    (SetupStep::TeamMetadataRefresh, SetupScope::Agent),
    (SetupStep::WarpDriveSync, SetupScope::Agent),
    (SetupStep::AgentCredentialsFetch, SetupScope::Agent),
    (SetupStep::EnvironmentResolution, SetupScope::Agent),
    (SetupStep::SkillRepoClone, SetupScope::Agent),
    (SetupStep::TerminalBootstrap, SetupScope::Agent),
    (SetupStep::McpServerStartup, SetupScope::Agent),
    (SetupStep::AgentProfileConfiguration, SetupScope::Agent),
    (SetupStep::GlobalSkillResolution, SetupScope::Agent),
    (SetupStep::GlobalSkillRepoClone, SetupScope::Agent),
    (SetupStep::EnvironmentRepoClone, SetupScope::Agent),
    (SetupStep::CacheSetup, SetupScope::Agent),
    (SetupStep::EnvironmentSetupCommands, SetupScope::Agent),
    (SetupStep::EnvironmentCodebaseIndexing, SetupScope::Agent),
    (SetupStep::FileBasedMcpDiscovery, SetupScope::Agent),
    (SetupStep::FileBasedMcpReadiness, SetupScope::Agent),
    (SetupStep::InitialGlobalMcpScan, SetupScope::Agent),
    (SetupStep::InitialGlobalMcpReadiness, SetupScope::Agent),
    (SetupStep::EnvironmentSkillLoading, SetupScope::Agent),
    (SetupStep::GlobalSkillLoading, SetupScope::Agent),
    (SetupStep::SkillsDirsLoading, SetupScope::Agent),
    (SetupStep::TaskDataFetch, SetupScope::Run),
    (SetupStep::CloudProviderSetup, SetupScope::Run),
    (SetupStep::SharedSessionEstablishment, SetupScope::Run),
    (SetupStep::ConversationResumeLoading, SetupScope::Run),
    (SetupStep::ThirdPartyHarnessPreparation, SetupScope::Run),
    (
        SetupStep::ThirdPartyHarnessExternalConversation,
        SetupScope::Run,
    ),
    (
        SetupStep::ThirdPartyHarnessPreparationNotificationPluginInstall,
        SetupScope::Run,
    ),
    (
        SetupStep::ThirdPartyHarnessPreparationNotificationPluginUpdate,
        SetupScope::Run,
    ),
    (
        SetupStep::ThirdPartyHarnessPreparationPlatformPluginInstall,
        SetupScope::Run,
    ),
    (
        SetupStep::ThirdPartyHarnessPreparationPlatformPluginUpdate,
        SetupScope::Run,
    ),
];

#[test]
fn every_setup_step_has_its_expected_scope() {
    for (step, expected) in EXPECTED {
        assert_eq!(step.scope(), *expected, "unexpected scope for {step:?}");
    }
}

/// Guards against a new variant being added to [`SetupStep`] without being classified here.
/// `scope()` is matched exhaustively, so a new variant cannot reach production unclassified,
/// but it could still be missing from `EXPECTED` and escape the assertions above.
#[test]
fn expected_scopes_cover_every_setup_step() {
    for step in enum_iterator::all::<SetupStep>() {
        assert!(
            EXPECTED.iter().any(|(listed, _)| *listed == step),
            "{step:?} is missing from EXPECTED"
        );
    }
}
