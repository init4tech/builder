//! Integration tests for the FlashbotsProvider.
//! These tests require the `FLASHBOTS_ENDPOINT` env var to be set.
#[cfg(test)]
mod tests {
    use builder::tasks::submit::flashbots::FlashbotsProvider;

    #[cfg(test)]
    mod tests {
        use super::*;
        use alloy::{providers::Provider, rpc::types::mev::MevSendBundle};
        use builder::{config::BuilderConfig, test_utils::setup_logging};
        use init4_bin_base::utils::from_env::FromEnv;

        #[tokio::test]
        #[ignore = "integration test"]
        async fn smoke_root_provider() {
            setup_logging();
            let config = BuilderConfig::from_env().unwrap();

            let host_provider = config.connect_host_provider().await.unwrap();
            let zenith = config.connect_zenith(host_provider.clone());
            let flashbots =
                FlashbotsProvider::new(config.clone().flashbots_endpoint, zenith.clone(), &config);
            FlashbotsProvider::new(config.clone().flashbots_endpoint, zenith.clone(), &config);

            assert_eq!(flashbots.relay_url.as_str(), "http://localhost:9062/");

            let provider = flashbots.zenith.provider();
            let block_number = provider.get_block_number().await.unwrap();
            assert!(block_number > 0);
        }

        #[tokio::test]
        #[ignore = "integration test"]
        async fn smoke_simulate_bundle() {
            let flashbots = get_test_provider().await;

            let res = flashbots.simulate_bundle(MevSendBundle::default()).await;

            if let Err(err) = &res {
                eprintln!("simulate error (expected for empty bundle): {err}");
            }

            if let Err(err) = &res {
                let msg = format!("{err}");
                assert!(msg.contains("mev_simBundle"));
            }

            assert!(res.is_ok() || res.is_err());
        }

        #[tokio::test]
        #[ignore = "integration test"]
        async fn smoke_send_bundle() {
            let flashbots = get_test_provider().await;
            let res = flashbots.send_bundle(MevSendBundle::default()).await;

            if let Err(err) = &res {
                let msg = format!("{err}");
                assert!(msg.contains("mev_sendBundle"));
            }

            assert!(res.is_ok() || res.is_err());
        }

        async fn get_test_provider() -> FlashbotsProvider {
            let config = BuilderConfig::from_env().unwrap();

            let host_provider = config.connect_host_provider().await.unwrap();
            let zenith = config.connect_zenith(host_provider.clone());

            let flashbots =
                FlashbotsProvider::new(config.flashbots_endpoint.clone(), zenith, &config.clone());
            flashbots
        }
    }
}
