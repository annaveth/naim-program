// Protocol constants. These are part of program logic (PDA derivation / sizing),
// not deployment config — addresses, cluster and fees still come from env/SDK.

/// PDA seed prefix for every name record.
pub const SEED_PREFIX: &[u8] = b"naim";

/// Top-level domain for agent names.
pub const TLD_AGENT: &[u8] = b".agent";

/// PDA seed for the singleton protocol config.
pub const CONFIG_SEED: &[u8] = b"config";

/// PDA seed for the singleton tokenomics config ($NAIM fees + fee split).
pub const TOKEN_CONFIG_SEED: &[u8] = b"token_config";

/// PDA seed for the stakers-vault authority (owns the NAIM ATA that accrues
/// the stakers' share of every fee; the staking update distributes from it).
pub const STAKERS_VAULT_SEED: &[u8] = b"stakers_vault";

/// Basis-points denominator (100% = 10_000).
pub const BPS_DENOM: u16 = 10_000;

// ===== staking (update/2) ===========================================

/// PDA seed for the singleton staking pool.
pub const STAKE_POOL_SEED: &[u8] = b"stake_pool";

/// PDA seed for a user's stake position: [STAKE_SEED, owner].
pub const STAKE_SEED: &[u8] = b"stake";

/// PDA seed for the authority that custodies staked principal.
pub const STAKE_VAULT_SEED: &[u8] = b"stake_vault";

/// Fixed-point precision for the reward-per-share accumulator (1e12).
pub const ACC_PRECISION: u128 = 1_000_000_000_000;

// ===== reputation (update/3) ========================================

/// PDA seed for a name's on-chain reputation record: [REP_SEED, name_hash].
pub const REP_SEED: &[u8] = b"rep";

// ===== categories (update/4) ========================================

/// PDA seed for a category namespace record: [CATEGORY_SEED, category_hash].
pub const CATEGORY_SEED: &[u8] = b"category";

/// Default royalty a category owner earns on each sub-name mint (basis points).
pub const DEFAULT_ROYALTY_BPS: u16 = 500; // 5%

/// Registration-fee discount tiers by amount of $NAIM staked (base units).
/// Tiers are protocol logic, tuned here rather than per-deploy config.
pub const STAKE_DISCOUNT_TIER1: u64 = 1_000_000_000; // 1_000 $NAIM -> 10% off
pub const STAKE_DISCOUNT_BPS1: u16 = 1_000;
pub const STAKE_DISCOUNT_TIER2: u64 = 5_000_000_000; // 5_000 $NAIM -> 25% off
pub const STAKE_DISCOUNT_BPS2: u16 = 2_500;

/// Max length of a name (characters), e.g. `executor.defi`.
pub const MAX_NAME_LEN: usize = 63;

/// Max length of the AgentCard metadata URI (Arweave/IPFS).
pub const MAX_METADATA_URI_LEN: usize = 200;

/// Max number of secondary wallets linked to one identity.
pub const MAX_LINKED_WALLETS: usize = 5;

// ===== sponsored discovery ==========================================

/// PDA seed for a sponsored ranking bid: [RANK_BID_SEED, name_hash, capability_hash].
pub const RANK_BID_SEED: &[u8] = b"rankbid";

/// Length of a sponsored-slot epoch, in seconds. A bid placed during epoch N
/// competes for epoch N+1's slots (short here for devnet; tune for mainnet).
pub const RANK_EPOCH_SECS: i64 = 60;

// ===== marketplace ==================================================

/// PDA seed for a name's marketplace listing: [LISTING_SEED, name_hash].
pub const LISTING_SEED: &[u8] = b"listing";

/// PDA seed for the singleton marketplace config (accepted USDC mint + fee
/// tiers). A separate PDA so the existing Config / TokenConfig layouts are
/// never touched by this update.
pub const MARKET_CONFIG_SEED: &[u8] = b"market_config";

/// Listing payment currencies. A listing is priced in exactly one of these;
/// buying with $NAIM pays a lower fee than SOL/USDC (fee tiers in MarketConfig).
pub const CURRENCY_NAIM: u8 = 0;
pub const CURRENCY_SOL: u8 = 1;
pub const CURRENCY_USDC: u8 = 2;
