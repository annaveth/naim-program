use anchor_lang::prelude::*;

use crate::constants::{MAX_LINKED_WALLETS, MAX_METADATA_URI_LEN};

/// Singleton protocol config. PDA seeds = [CONFIG_SEED].
/// All economic values are set at `initialize` (from env via the deploy script),
/// never hardcoded in the program.
#[account]
#[derive(InitSpace)]
pub struct Config {
    /// Admin authority (may update config later).
    pub admin: Pubkey,
    /// Wallet that receives all protocol fees.
    pub treasury: Pubkey,
    /// Registration fee for 1–4 char names (lamports). These are permanent.
    pub fee_1_4: u64,
    /// Registration fee for 5–9 char names (lamports).
    pub fee_5_9: u64,
    /// Registration fee for 10+ char names (lamports).
    pub fee_10_plus: u64,
    /// Fee to set the verified flag (lamports).
    pub verify_fee: u64,
    /// Registration / renewal period in seconds (e.g. 1 year).
    pub registration_period: i64,
    /// Grace period in seconds after expiry before public re-registration.
    pub grace_period: i64,
    /// PDA bump.
    pub bump: u8,
}

/// Singleton tokenomics config. PDA seeds = [TOKEN_CONFIG_SEED].
/// Introduced in the tokenomics update: every protocol fee is paid in $NAIM,
/// then split treasury / stakers-vault / burn. Kept separate from `Config` so
/// the already-deployed `Config` account layout is never broken.
#[account]
#[derive(InitSpace)]
pub struct TokenConfig {
    /// Admin authority (may update fees / split later).
    pub admin: Pubkey,
    /// The $NAIM SPL mint that fees are paid in (and burned from).
    pub naim_mint: Pubkey,
    /// Wallet that owns the treasury $NAIM token account.
    pub treasury: Pubkey,
    /// Registration fee for 1–4 char names ($NAIM base units). Permanent names.
    pub fee_1_4: u64,
    /// Registration fee for 5–9 char names ($NAIM base units).
    pub fee_5_9: u64,
    /// Registration fee for 10+ char names ($NAIM base units).
    pub fee_10_plus: u64,
    /// Fee to set the verified flag ($NAIM base units).
    pub verify_fee: u64,
    /// Share of every fee sent to the treasury (basis points).
    pub treasury_bps: u16,
    /// Share of every fee sent to the stakers vault (basis points).
    pub stakers_bps: u16,
    /// Share of every fee burned (basis points). The three must sum to 10_000.
    pub burn_bps: u16,
    /// Running total of $NAIM burned by the protocol (stat).
    pub total_burned: u64,
    /// PDA bump for this account.
    pub bump: u8,
    /// PDA bump for the stakers-vault authority.
    pub vault_bump: u8,
}

/// Singleton staking pool. PDA seeds = [STAKE_POOL_SEED]. Distributes the
/// stakers' share of fees (which accrues in the stakers vault from the
/// tokenomics update) via a reward-per-share accumulator (MasterChef-style).
#[account]
#[derive(InitSpace)]
pub struct StakePool {
    pub admin: Pubkey,
    pub naim_mint: Pubkey,
    /// Total $NAIM currently staked (principal).
    pub total_staked: u64,
    /// Accumulated reward per staked unit, scaled by ACC_PRECISION.
    pub acc_reward_per_share: u128,
    /// Rewards-vault balance already folded into the accumulator.
    pub last_reward_balance: u64,
    pub bump: u8,
    /// Bump of the stake-vault authority (custodies principal).
    pub stake_vault_bump: u8,
}

/// A user's stake position. PDA seeds = [STAKE_SEED, owner].
#[account]
#[derive(InitSpace)]
pub struct StakeAccount {
    pub owner: Pubkey,
    /// Staked principal in $NAIM base units.
    pub amount: u64,
    /// amount * acc_reward_per_share / ACC_PRECISION at last interaction.
    pub reward_debt: u128,
    /// Unix timestamp before which `unstake` is rejected.
    pub lock_end: i64,
    pub bump: u8,
}

/// A sponsored ranking bid (update/5). PDA = [RANK_BID_SEED, name_hash, capability_hash].
/// The bid amount is burned (100%); this record just remembers how much a name
/// burned for a capability in a given epoch, so the off-chain ranker can boost it.
#[account]
#[derive(InitSpace)]
pub struct RankBid {
    pub owner: Pubkey,
    pub name_hash: [u8; 32],
    pub capability_hash: [u8; 32],
    /// $NAIM burned during `epoch`.
    pub amount: u64,
    /// unix_timestamp / RANK_EPOCH_SECS at the time of the bid.
    pub epoch: i64,
    pub bump: u8,
}

/// Category namespace (update/4). PDA seeds = [CATEGORY_SEED, category_hash].
/// The owner earns a royalty on every sub-name minted beneath the category.
#[account]
#[derive(InitSpace)]
pub struct CategoryRecord {
    /// Current owner / authority of the category (transferable).
    pub owner: Pubkey,
    /// Royalty on sub-name mints, in basis points (500 = 5%).
    pub royalty_bps: u16,
    /// Number of sub-names minted under this category.
    pub sub_count: u64,
    /// Cumulative $NAIM royalty paid out to the owner from sub-name mints.
    pub total_earned: u64,
    /// PDA bump.
    pub bump: u8,
}

/// Tamper-proof reputation inputs for a name (update/3). PDA seeds =
/// [REP_SEED, name_hash]. The protocol writes these at lifecycle events — they
/// can't be self-reported. Discovery ranks by a deterministic score computed
/// off these plus the NameRecord (verified, linked wallets, category depth).
#[account]
#[derive(InitSpace)]
pub struct ReputationRecord {
    pub name_hash: [u8; 32],
    /// Unix timestamp of first registration (account age).
    pub created_at: i64,
    /// Number of renewals paid (commitment signal).
    pub renew_count: u32,
    pub bump: u8,
}

/// On-chain record for a registered name.
/// PDA seeds = [SEED_PREFIX, sha256(name), TLD_AGENT].
#[account]
#[derive(InitSpace)]
pub struct NameRecord {
    /// Current owner / authority of the name.
    pub owner: Pubkey,
    /// Wallet or program the name resolves to.
    pub resolver: Pubkey,
    /// URI of the AgentCard JSON manifest (Arweave/IPFS).
    #[max_len(MAX_METADATA_URI_LEN)]
    pub metadata_uri: String,
    /// Unix timestamp of expiry. 0 = permanent.
    pub expiry_timestamp: i64,
    /// Set by the protocol on on-chain verification.
    pub verified: bool,
    /// Additional wallets associated with this identity.
    #[max_len(MAX_LINKED_WALLETS)]
    pub linked_wallets: Vec<Pubkey>,
    /// PDA bump.
    pub bump: u8,
}

/// Singleton marketplace config. PDA seeds = [MARKET_CONFIG_SEED].
/// Holds the USDC mint accepted as a sale currency and the fee tiers. Kept in
/// its own PDA so the already-deployed Config / TokenConfig layouts are never
/// touched. Values come from env (set at init, tuned live), never hardcoded.
#[account]
#[derive(InitSpace)]
pub struct MarketConfig {
    /// Admin authority (may update fees / usdc mint later).
    pub admin: Pubkey,
    /// USDC SPL mint accepted as a listing/payment currency.
    pub usdc_mint: Pubkey,
    /// Wallet that receives marketplace fees (SOL natively; USDC/$NAIM via ATA).
    pub treasury: Pubkey,
    /// Fee on a $NAIM-priced sale (bps) — lower, to steer volume to $NAIM.
    pub fee_naim_bps: u16,
    /// Fee on a SOL- or USDC-priced sale (bps).
    pub fee_stable_bps: u16,
    /// PDA bump.
    pub bump: u8,
}

/// A secondary-market listing for a name. PDA seeds = [LISTING_SEED, name_hash].
/// The name stays owned by `seller` until bought; creating a listing is the
/// seller's on-chain authorization for the buy instruction to reassign the
/// NameRecord. The buy instruction re-checks ownership at purchase time.
#[account]
#[derive(InitSpace)]
pub struct Listing {
    /// Seller — must own the name when it's listed and when it's bought.
    pub seller: Pubkey,
    /// Hash of the listed name.
    pub name_hash: [u8; 32],
    /// Payment currency: 0 = $NAIM, 1 = SOL, 2 = USDC (see constants).
    pub currency: u8,
    /// Asking price in the currency's base units (lamports / USDC / $NAIM).
    pub price: u64,
    /// Unix timestamp the listing was created.
    pub created_at: i64,
    /// PDA bump.
    pub bump: u8,
}
