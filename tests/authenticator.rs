use builder::{tasks::oauth::Authenticator, test_utils::setup_test_config};

#[ignore = "integration test"]
#[tokio::test]
async fn test_authenticator() -> eyre::Result<()> {
    let config = setup_test_config()?;
    let auth = Authenticator::new(&config)?;

    let _ = auth.fetch_oauth_token().await?;

    let token = auth.token();

    assert!(token.is_authenticated());
    Ok(())
}
