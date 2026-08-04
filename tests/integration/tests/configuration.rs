use integration_tests::IntegrationConfig;

#[test]
fn loads_deployed_integration_environment_outputs() {
    let config = IntegrationConfig::load().expect(
        "integration outputs should exist; run `pnpm integration:deploy` before integration tests",
    );

    config
        .validate()
        .expect("deployed integration outputs should have valid resource shapes");
}
