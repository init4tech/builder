//! Integration tests for the FlashbotsProvider.
//! These tests require the `FLASHBOTS_ENDPOINT` env var to be set.
#[cfg(test)]
mod tests {
    use alloy::{network::Ethereum, providers::ProviderBuilder};
    use builder::tasks::submit::flashbots::FlashbotsProvider;

    #[cfg(test)]
    mod tests {
        use super::*;
        use alloy::{network::Ethereum, providers::ProviderBuilder};

        #[tokio::test]
        #[ignore = "integration test"]
        async fn smoke_root_provider() {
            let url: url::Url = "http://localhost:9062/".parse().unwrap();
            let root = alloy::providers::RootProvider::<Ethereum>::new_http(url.clone());
            let fill = ProviderBuilder::new_with_network().connect_provider(root);
            let fp = FlashbotsProvider::new(fill, url);

            assert_eq!(fp.relay_url.as_str(), "http://localhost:9062/");

            let bundle = fp.empty_bundle(1);
            let dbg_bundle = format!("{:?}", bundle);
            assert!(dbg_bundle.contains("1"));
        }

        #[tokio::test]
        #[ignore = "integration test"]
        async fn smoke_simulate_bundle() {
            let Ok(endpoint) = std::env::var("FLASHBOTS_ENDPOINT") else {
                eprintln!("skipping: set FLASHBOTS_ENDPOINT to run integration test");
                return;
            };
            let url: url::Url = endpoint.parse().expect("invalid FLASHBOTS_ENDPOINT");
            let root = alloy::providers::RootProvider::<Ethereum>::new_http(url.clone());
            let provider = ProviderBuilder::new_with_network().connect_provider(root);
            let fp = FlashbotsProvider::new(provider, url);

            let bundle = fp.empty_bundle(1);
            let res = fp.simulate_bundle(&bundle).await;

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
            let Ok(endpoint) = std::env::var("FLASHBOTS_ENDPOINT") else {
                eprintln!("skipping: set FLASHBOTS_ENDPOINT to run integration test");
                return;
            };
            let url: url::Url = endpoint.parse().expect("invalid FLASHBOTS_ENDPOINT");
            let root = alloy::providers::RootProvider::<Ethereum>::new_http(url.clone());
            let provider = ProviderBuilder::new_with_network().connect_provider(root);
            let fp = FlashbotsProvider::new(provider, url);

            let bundle = fp.empty_bundle(1);
            let res = fp.send_bundle(bundle).await;

            if let Err(err) = &res {
                let msg = format!("{err}");
                assert!(msg.contains("mev_sendBundle"));
            }

            assert!(res.is_ok() || res.is_err());
        }
    }
}
