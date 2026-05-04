use std::str::FromStr;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, U160, U256, aliases::U48};
use alloy::providers::{Provider, ProviderBuilder, WalletProvider};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use renegade_external_api::types::OrderType;
use renegade_sdk::client::RenegadeClient;
use renegade_sdk::{
    AssembleQuoteOptionsV2, ExternalMatchClient, ExternalOrderBuilderV2,
};

/// Dummy WETH token address on Base Sepolia
pub const WETH: &str = "0x31a5552AF53C35097Fdb20FFf294c56dc66FA04c";
/// Dummy USDC token address on Base Sepolia
pub const USDC: &str = "0xD9961Bb4Cb27192f8dAd20a662be081f546b0E74";

pub const RPC_URL: &str = "https://sepolia.base.org";

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function allowance(address owner, address spender) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
    }

    #[sol(rpc)]
    interface IPermit2 {
        function allowance(address user, address token, address spender) external view returns (uint160 amount, uint48 expiration, uint48 nonce);
    }
}

/// Ensure ERC20 and Permit2 allowances are sufficient before placing an order.
///
/// This handles the two-step approval flow required by the darkpool:
/// 1. ERC20 approval: allows the Permit2 contract to spend the token
/// 2. Permit2 allowance: allows the darkpool to spend via Permit2
async fn ensure_allowances(
    client: &RenegadeClient,
    token: Address,
    amount: u128,
    signer: &PrivateKeySigner,
) -> eyre::Result<()> {
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_http(RPC_URL.parse()?);

    let user_address = signer.address();
    let permit2_address = client.get_permit2_address();
    let darkpool_address = client.get_darkpool_address();
    let required = U256::from(amount);

    // Step 1: ERC20 approval for Permit2
    let erc20 = IERC20::new(token, &provider);
    let current_allowance = erc20.allowance(user_address, permit2_address).call().await?;
    if current_allowance < required {
        let tx = client.build_erc20_approval_tx(token, required);
        provider.send_transaction(tx).await?.watch().await?;
    }

    // Step 2: Permit2 allowance for Darkpool
    let permit2 = IPermit2::new(permit2_address, &provider);
    let result = permit2.allowance(user_address, token, darkpool_address).call().await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();

    if result.amount < U160::from(amount) || result.expiration < now {
        let tx = client.build_permit2_allowance_tx(token, U160::MAX, U48::MAX);
        provider.send_transaction(tx).await?.watch().await?;
    }

    Ok(())
}

/// Approves Permit2, places a buy-WETH order, waits for a match,
/// then cancels unfilled orders. Tokens transfer directly from the
/// user's EOA via Permit2 on fill — no deposit or withdrawal needed.
#[tokio::test]
async fn direct_match_example() -> eyre::Result<()> {
    // 1. Create a client
    let private_key = std::env::var("PRIVATE_KEY")?;
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let client = RenegadeClient::new_base_sepolia(&signer)?;

    // 2. Create the Renegade account if absent
    if client.get_account().await.is_err() {
        client.create_account().await?;
    }

    // 3. Approve Permit2 to spend USDC
    let usdc_mint: Address = USDC.parse()?;
    let input_amount: u128 = 100_000; // 0.1 USDC (6 decimals)
    ensure_allowances(&client, usdc_mint, input_amount, &signer).await?;

    // 4. Place a buy order for WETH
    //    When matched, USDC transfers directly from your EOA via Permit2
    let order = client
        .new_order_builder()
        .with_input_mint(USDC)?
        .with_output_mint(WETH)?
        .with_input_amount(input_amount)
        .with_order_type(OrderType::PublicOrder)
        .build()?;

    client.place_order(order).await?;
    println!("Placed order");

    // 5. Wait for a match (in production, poll or use websockets)
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // 6. Cancel unfilled orders
    let orders = client.get_orders(false /* include_historic */).await?;
    for order in orders {
        println!("Cancelling order... {}", &order.id);
        client.cancel_order(order.id).await?;
    }
    println!("Cancelled orders");

    Ok(())
}

/// Requests a quote for an external match, assembles it into a
/// settlement transaction, and submits it on-chain.
#[tokio::test]
async fn rfq_example() -> eyre::Result<()> {
    // 1. Create an external match client
    let api_key = std::env::var("EXTERNAL_MATCH_KEY")?;
    let api_secret = std::env::var("EXTERNAL_MATCH_SECRET")?;
    let ext_client = ExternalMatchClient::new_base_sepolia_client(&api_key, &api_secret)?;

    // 2. Ensure the darkpool has approval to spend USDC before requesting a
    //    quote, so that the settlement tx can be submitted immediately
    let private_key = std::env::var("PRIVATE_KEY")?;
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(RPC_URL.parse()?);

    let darkpool: Address = ext_client.get_exchange_metadata().await?.settlement_contract_address.parse()?;
    let input_mint: Address = USDC.parse()?;
    let erc20 = IERC20::new(input_mint, &provider);
    let allowance = erc20.allowance(provider.default_signer_address(), darkpool).call().await?;
    if allowance < U256::from(10_000_000u128) {
        erc20.approve(darkpool, U256::MAX).send().await?.watch().await?;
        println!("Approved darkpool to spend USDC");
    }

    // 3. Build an external order
    let order = ExternalOrderBuilderV2::new()
        .input_mint(USDC)
        .output_mint(WETH)
        .input_amount(10_000_000) // 10 USDC
        .build()?;

    // 4. Request a quote
    let Some(quote) = ext_client.request_quote_v2(order).await? else {
        println!("No quote available");
        return Ok(());
    };

    println!(
        "Quote: receive {} of {}",
        quote.quote.receive.amount, quote.quote.receive.mint
    );

    // 5. Assemble the quote into a settlement transaction
    let Some(resp) = ext_client.assemble_quote_v2(quote).await? else {
        println!("No bundle returned");
        return Ok(());
    };

    // 6. Submit the settlement transaction on-chain
    let tx = resp.settlement_tx().with_gas_limit(1_000_000);
    let pending = provider.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;
    if receipt.status() {
        println!("Settlement tx confirmed: {:#x}", receipt.transaction_hash);
    } else {
        println!("Settlement tx reverted: {:#x} (bundle may have expired)", receipt.transaction_hash);
    }

    Ok(())
}

/// Requests a quote at one input amount, then assembles it at a smaller
/// amount via `with_updated_order`. The original quote's price is
/// preserved; only the size is adjusted. The bundle's signature still
/// validates because the executor signs a bounded match (a [min, max]
/// range), not a point amount.
#[tokio::test]
async fn rfq_updated_order_example() -> eyre::Result<()> {
    // 1. Create an external match client
    let api_key = std::env::var("EXTERNAL_MATCH_KEY")?;
    let api_secret = std::env::var("EXTERNAL_MATCH_SECRET")?;
    let ext_client = ExternalMatchClient::new_base_sepolia_client(&api_key, &api_secret)?;

    // 2. Approve the darkpool to spend USDC
    let private_key = std::env::var("PRIVATE_KEY")?;
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(RPC_URL.parse()?);

    let darkpool: Address = ext_client.get_exchange_metadata().await?.settlement_contract_address.parse()?;
    let input_mint: Address = USDC.parse()?;
    let erc20 = IERC20::new(input_mint, &provider);
    let allowance = erc20.allowance(provider.default_signer_address(), darkpool).call().await?;
    if allowance < U256::from(10_000_000u128) {
        erc20.approve(darkpool, U256::MAX).send().await?.watch().await?;
        println!("Approved darkpool to spend USDC");
    }

    // 3. Request a quote at the originally desired amount
    let original_amount: u128 = 10_000_000; // 10 USDC
    let order = ExternalOrderBuilderV2::new()
        .input_mint(USDC)
        .output_mint(WETH)
        .input_amount(original_amount)
        .build()?;

    let Some(quote) = ext_client.request_quote_v2(order).await? else {
        println!("No quote available");
        return Ok(());
    };

    // 4. Build an updated order with a smaller input amount.
    //    In production, you would typically re-check the user's on-chain
    //    balance here and only rescale if it has dropped below
    //    `original_amount`. This snippet unconditionally scales down to
    //    keep the example focused on `with_updated_order` itself.
    let reduced_amount: u128 = 5_000_000; // 5 USDC
    let updated_order = ExternalOrderBuilderV2::new()
        .input_mint(USDC)
        .output_mint(WETH)
        .input_amount(reduced_amount)
        .build()?;

    // 5. Assemble the quote with the smaller amount. The quoted price is
    //    preserved; only the size changes.
    let opts = AssembleQuoteOptionsV2::default().with_updated_order(updated_order);
    let Some(resp) = ext_client.assemble_quote_with_options_v2(quote, opts).await? else {
        println!("No bundle returned");
        return Ok(());
    };

    // 6. Submit the settlement transaction on-chain
    let tx = resp.settlement_tx().with_gas_limit(1_000_000);
    let pending = provider.send_transaction(tx).await?;
    let receipt = pending.get_receipt().await?;
    if receipt.status() {
        println!("Settlement tx confirmed: {:#x}", receipt.transaction_hash);
    } else {
        println!("Settlement tx reverted: {:#x} (bundle may have expired)", receipt.transaction_hash);
    }

    Ok(())
}

/// Reads the order-book depth feed: lists the total number of pairs returned
/// by `/v2/markets/depth`, then prints the per-side depth for WETH from
/// `/v2/markets/{mint}/depth`. Useful for routing-time scoring without
/// hitting `/quote` on every candidate path.
#[tokio::test]
async fn rfq_depth_example() -> eyre::Result<()> {
    // 1. Create an external match client
    let api_key = std::env::var("EXTERNAL_MATCH_KEY")?;
    let api_secret = std::env::var("EXTERNAL_MATCH_SECRET")?;
    let ext_client = ExternalMatchClient::new_base_sepolia_client(&api_key, &api_secret)?;

    // 2. Fetch depth for all pairs and print the count
    let all_depths = ext_client.get_market_depths_all_pairs().await?;
    println!("Pairs returned by /v2/markets/depth: {}", all_depths.market_depths.len());

    // 3. Fetch depth for WETH specifically and print both sides
    let weth_depth = ext_client.get_market_depth(WETH).await?.market_depth;
    let price = &weth_depth.market.price;
    println!(
        "WETH market: base={} quote={} price={} (ts={})",
        weth_depth.market.base.address,
        weth_depth.market.quote.address,
        price.price,
        price.timestamp,
    );
    println!(
        "  buy:  total_quantity={} ({} USD)",
        weth_depth.buy.total_quantity, weth_depth.buy.total_quantity_usd,
    );
    println!(
        "  sell: total_quantity={} ({} USD)",
        weth_depth.sell.total_quantity, weth_depth.sell.total_quantity_usd,
    );

    Ok(())
}
