import { ExternalMatchClient, OrderSide } from "@renegade-fi/renegade-sdk";
import { erc20Abi, createPublicClient, createWalletClient, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";

const WETH = "0x31a5552AF53C35097Fdb20FFf294c56dc66FA04c";
const USDC = "0xD9961Bb4Cb27192f8dAd20a662be081f546b0E74";

// --- Env setup ---
const API_KEY = process.env.EXTERNAL_MATCH_KEY;
const API_SECRET = process.env.EXTERNAL_MATCH_SECRET;
const privateKey = process.env.PRIVATE_KEY;
if (!API_KEY) throw new Error("EXTERNAL_MATCH_KEY is not set");
if (!API_SECRET) throw new Error("EXTERNAL_MATCH_SECRET is not set");
if (!privateKey) throw new Error("PRIVATE_KEY is not set");

const account = privateKeyToAccount(privateKey as `0x${string}`);
const owner = account.address;
const publicClient = createPublicClient({ chain: baseSepolia, transport: http() });
const walletClient = createWalletClient({ account, chain: baseSepolia, transport: http() });

// 1. Create external match client
const client = ExternalMatchClient.newBaseSepoliaClient(API_KEY, API_SECRET);

// 2. Build order
const order = {
    base_mint: WETH,
    quote_mint: USDC,
    side: OrderSide.BUY,
    quote_amount: BigInt(2_000_000), // 2 USDC
} as const;

// 3. Request quote
console.log("Fetching quote...");
const quote = await client.requestQuote(order);
if (!quote) { console.error("No quote available"); process.exit(1); }

console.log(`Quote: receive ${quote.quote.receive.amount} of ${quote.quote.receive.mint}`);

// 4. Assemble quote into settlement tx
console.log("Assembling quote...");
const bundle = await client.assembleQuote(quote);
if (!bundle) { console.error("No bundle available"); process.exit(1); }
const tx = bundle.match_bundle.settlement_tx;

// 5. Check & set ERC20 allowance
const isSell = bundle.match_bundle.match_result.direction === "Sell";
const tokenAddress = isSell
    ? bundle.match_bundle.match_result.base_mint as `0x${string}`
    : bundle.match_bundle.match_result.quote_mint as `0x${string}`;
const amount = isSell
    ? bundle.match_bundle.match_result.base_amount
    : bundle.match_bundle.match_result.quote_amount;
const spender = tx.to as `0x${string}`;

const allowance = await publicClient.readContract({
    address: tokenAddress, abi: erc20Abi, functionName: "allowance", args: [owner, spender],
});
if (allowance < amount) {
    const approveTx = await walletClient.writeContract({
        address: tokenAddress, abi: erc20Abi, functionName: "approve", args: [spender, amount],
    });
    await publicClient.waitForTransactionReceipt({ hash: approveTx });
    console.log("Approved");
}

// 6. Submit settlement tx
console.log("Submitting bundle...");
const hash = await walletClient.sendTransaction({
    to: tx.to as `0x${string}`,
    data: tx.data as `0x${string}`,
    type: "eip1559",
});
console.log("Successfully submitted transaction", hash);
