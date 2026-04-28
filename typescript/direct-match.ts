import { DirectMatchClient } from "@renegade-fi/renegade-sdk";
import { erc20Abi, createPublicClient, createWalletClient, http, type Address, maxUint160, maxUint48 } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";

function sleep(ms: number) { return new Promise(resolve => setTimeout(resolve, ms)); }

const WETH = "0x31a5552AF53C35097Fdb20FFf294c56dc66FA04c";
const USDC = "0xD9961Bb4Cb27192f8dAd20a662be081f546b0E74";

// Permit2 canonical address (same on all chains)
const PERMIT2 = "0x000000000022D473030F116dDEE9F6B43aC78BA3" as Address;
// Darkpool address on Base Sepolia
const DARKPOOL = "0xDE9BfD62B2187d4c14FBcC7D869920d34e4DB3Da" as Address;

// Permit2 AllowanceTransfer ABI
const permit2Abi = [
    {
        type: "function",
        name: "allowance",
        inputs: [
            { name: "user", type: "address" },
            { name: "token", type: "address" },
            { name: "spender", type: "address" },
        ],
        outputs: [
            { name: "amount", type: "uint160" },
            { name: "expiration", type: "uint48" },
            { name: "nonce", type: "uint48" },
        ],
        stateMutability: "view",
    },
    {
        type: "function",
        name: "approve",
        inputs: [
            { name: "token", type: "address" },
            { name: "spender", type: "address" },
            { name: "amount", type: "uint160" },
            { name: "expiration", type: "uint48" },
        ],
        outputs: [],
        stateMutability: "nonpayable",
    },
] as const;

// --- Env setup ---
const privateKey = process.env.PRIVATE_KEY;
if (!privateKey) throw new Error("PRIVATE_KEY is not set");

const account = privateKeyToAccount(privateKey as `0x${string}`);
const publicClient = createPublicClient({ chain: baseSepolia, transport: http() });
const walletClient = createWalletClient({ account, chain: baseSepolia, transport: http() });

/// Ensure ERC20 and Permit2 allowances are sufficient before placing an order.
///
/// This handles the two-step approval flow required by the darkpool:
/// 1. ERC20 approval: allows the Permit2 contract to spend the token
/// 2. Permit2 allowance: allows the darkpool to spend via Permit2
async function ensureAllowances(token: Address, amount: bigint) {
    const owner = account.address;

    // Step 1: ERC20 approval for Permit2
    const erc20Allowance = await publicClient.readContract({
        address: token, abi: erc20Abi, functionName: "allowance", args: [owner, PERMIT2],
    });
    if (erc20Allowance < amount) {
        const hash = await walletClient.writeContract({
            address: token, abi: erc20Abi, functionName: "approve", args: [PERMIT2, amount],
        });
        await publicClient.waitForTransactionReceipt({ hash });
    }

    // Step 2: Permit2 allowance for Darkpool
    const [permit2Amount, permit2Expiration] = await publicClient.readContract({
        address: PERMIT2, abi: permit2Abi, functionName: "allowance", args: [owner, token, DARKPOOL],
    });
    const now = BigInt(Math.floor(Date.now() / 1000));
    if (permit2Amount < amount || permit2Expiration < now) {
        const hash = await walletClient.writeContract({
            address: PERMIT2, abi: permit2Abi, functionName: "approve",
            args: [token, DARKPOOL, maxUint160, maxUint48],
        });
        await publicClient.waitForTransactionReceipt({ hash });
    }
}

// 1. Create a client
const client = await DirectMatchClient.newBaseSepoliaClient(account);

// 2. Create the Renegade account if absent
try {
    await client.getAccount();
} catch {
    await client.createAccount();
}

// 3. Approve Permit2 to spend USDC
const inputAmount = 100_000n; // 0.1 USDC (6 decimals)
await ensureAllowances(USDC as Address, inputAmount);

// 4. Place a buy order for WETH
//    When matched, USDC transfers directly from your EOA via Permit2
await client.placeOrder({
    inputMint: USDC,
    outputMint: WETH,
    inputAmount: inputAmount,
});
console.log("Placed order");

// 5. Wait for a match (in production, poll or use websockets)
await sleep(2_000);

// 6. Cancel unfilled orders
const orders = await client.getOrders(false /* include_historic */);
for (const order of orders) {
    console.log("Cancelling order...", order.id);
    await client.cancelOrder(order.id);
}
console.log("Cancelled orders");
