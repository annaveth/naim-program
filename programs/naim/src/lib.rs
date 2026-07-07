use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer as sol_transfer, Transfer as SolTransfer};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{
    self, Burn, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use solana_sha256_hasher::hash as sha256;
#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

pub mod constants;
pub mod error;
pub mod state;

use constants::*;
use error::NaimError;
use state::*;

declare_id!("DgBLc1GJpvwWdb9UbDDkMFPGMbC1sCvP5APgjC43AQp5");

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "NAIM",
    project_url: "https://x.com/nAImProtocol",
    contacts: "link:https://x.com/nAImProtocol",
    policy: "Report vulnerabilities privately via the contact above and allow reasonable time to patch before public disclosure. Good-faith research is welcome; no formal bug bounty yet.",
    source_code: "https://github.com/annaveth/naim-program",
    auditors: "None"
}

#[program]
pub mod naim {
    use super::*;

    // ===== ADMIN =====================================================

    /// One-time protocol setup. Stores treasury + fee tiers + periods.
    pub fn initialize(ctx: Context<Initialize>, p: ConfigParams) -> Result<()> {
        require!(p.registration_period > 0, NaimError::InvalidConfig);
        require!(p.grace_period >= 0, NaimError::InvalidConfig);

        let c = &mut ctx.accounts.config;
        c.admin = ctx.accounts.admin.key();
        c.treasury = p.treasury;
        c.fee_1_4 = p.fee_1_4;
        c.fee_5_9 = p.fee_5_9;
        c.fee_10_plus = p.fee_10_plus;
        c.verify_fee = p.verify_fee;
        c.registration_period = p.registration_period;
        c.grace_period = p.grace_period;
        c.bump = ctx.bumps.config;
        Ok(())
    }

    /// Admin-only: update periods / treasury (and legacy fee fields) without redeploying.
    pub fn update_config(ctx: Context<UpdateConfig>, p: ConfigParams) -> Result<()> {
        require!(p.registration_period > 0, NaimError::InvalidConfig);
        require!(p.grace_period >= 0, NaimError::InvalidConfig);
        let c = &mut ctx.accounts.config;
        c.treasury = p.treasury;
        c.fee_1_4 = p.fee_1_4;
        c.fee_5_9 = p.fee_5_9;
        c.fee_10_plus = p.fee_10_plus;
        c.verify_fee = p.verify_fee;
        c.registration_period = p.registration_period;
        c.grace_period = p.grace_period;
        Ok(())
    }

    /// One-time tokenomics setup. Pins the $NAIM mint, the per-tier fees (in
    /// $NAIM) and the treasury/stakers/burn split, and creates the treasury and
    /// stakers-vault token accounts. Admin-only.
    pub fn init_token_config(ctx: Context<InitTokenConfig>, p: TokenConfigParams) -> Result<()> {
        require!(
            p.treasury_bps as u32 + p.stakers_bps as u32 + p.burn_bps as u32 == BPS_DENOM as u32,
            NaimError::InvalidConfig
        );
        let tc = &mut ctx.accounts.token_config;
        tc.admin = ctx.accounts.admin.key();
        tc.naim_mint = ctx.accounts.naim_mint.key();
        tc.treasury = ctx.accounts.treasury.key();
        tc.fee_1_4 = p.fee_1_4;
        tc.fee_5_9 = p.fee_5_9;
        tc.fee_10_plus = p.fee_10_plus;
        tc.verify_fee = p.verify_fee;
        tc.treasury_bps = p.treasury_bps;
        tc.stakers_bps = p.stakers_bps;
        tc.burn_bps = p.burn_bps;
        tc.total_burned = 0;
        tc.bump = ctx.bumps.token_config;
        tc.vault_bump = ctx.bumps.stakers_vault;
        Ok(())
    }

    /// Admin-only: update the tokenomics after init — fees, verify fee, split,
    /// treasury and even the $NAIM mint. Mirrors init_token_config's accounts so
    /// the treasury and stakers-vault token accounts for the (possibly new) mint
    /// are ensured to exist.
    pub fn update_token_config(ctx: Context<UpdateTokenConfig>, p: TokenConfigParams) -> Result<()> {
        require!(
            p.treasury_bps as u32 + p.stakers_bps as u32 + p.burn_bps as u32 == BPS_DENOM as u32,
            NaimError::InvalidConfig
        );
        let tc = &mut ctx.accounts.token_config;
        tc.naim_mint = ctx.accounts.naim_mint.key();
        tc.treasury = ctx.accounts.treasury.key();
        tc.fee_1_4 = p.fee_1_4;
        tc.fee_5_9 = p.fee_5_9;
        tc.fee_10_plus = p.fee_10_plus;
        tc.verify_fee = p.verify_fee;
        tc.treasury_bps = p.treasury_bps;
        tc.stakers_bps = p.stakers_bps;
        tc.burn_bps = p.burn_bps;
        // admin, total_burned and the PDA bumps are preserved.
        Ok(())
    }

    // ===== REGISTRATION ==============================================

    /// Register a new name. Creates its PDA, collects the tier fee, sets the
    /// owner = resolver = payer. 1–4 char names are permanent (expiry 0).
    pub fn register_name(
        ctx: Context<RegisterName>,
        name: String,
        name_hash: [u8; 32],
        metadata_uri: String,
    ) -> Result<()> {
        let len = validate_name(&name)?;
        require!(
            sha256(name.as_bytes()).to_bytes() == name_hash,
            NaimError::InvalidName
        );
        require!(
            metadata_uri.len() <= MAX_METADATA_URI_LEN,
            NaimError::MetadataUriTooLong
        );

        let staked = ctx
            .accounts
            .stake_account
            .as_ref()
            .filter(|s| s.owner == ctx.accounts.payer.key())
            .map(|s| s.amount)
            .unwrap_or(0);
        let fee = apply_discount(fee_for(&ctx.accounts.token_config, len), staked);

        let now = Clock::get()?.unix_timestamp;
        // reclaim: a name whose account still exists can be taken over only once
        // its expiry + grace has fully passed (permanent names, expiry 0, never).
        if ctx.accounts.name_record.owner != Pubkey::default() {
            let rec = &ctx.accounts.name_record;
            require!(
                rec.expiry_timestamp != 0
                    && now >= rec.expiry_timestamp + ctx.accounts.config.grace_period,
                NaimError::NameAlreadyRegistered
            );
        }

        collect_fee(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.payer,
            &ctx.accounts.payer_naim_ata,
            &ctx.accounts.treasury_naim_ata,
            &ctx.accounts.stakers_vault_ata,
            &mut ctx.accounts.token_config,
            fee,
        )?;

        let registration_period = ctx.accounts.config.registration_period;
        let rec = &mut ctx.accounts.name_record;
        rec.owner = ctx.accounts.payer.key();
        rec.resolver = ctx.accounts.payer.key();
        rec.metadata_uri = metadata_uri;
        rec.expiry_timestamp = if is_permanent(len) {
            0
        } else {
            now + registration_period
        };
        rec.verified = false;
        rec.linked_wallets = Vec::new();
        rec.bump = ctx.bumps.name_record;

        let rep = &mut ctx.accounts.reputation_record;
        rep.name_hash = name_hash;
        rep.created_at = now;
        rep.renew_count = 0;
        rep.bump = ctx.bumps.reputation_record;
        Ok(())
    }

    /// Extend a non-permanent name by one registration period. Collects the
    /// tier fee. Extends from the later of `now` and the current expiry.
    pub fn renew_name(ctx: Context<RenewName>, name: String, name_hash: [u8; 32]) -> Result<()> {
        let len = validate_name(&name)?;
        require!(
            sha256(name.as_bytes()).to_bytes() == name_hash,
            NaimError::InvalidName
        );
        require!(
            ctx.accounts.name_record.expiry_timestamp != 0,
            NaimError::NamePermanent
        );

        let fee = fee_for(&ctx.accounts.token_config, len);
        collect_fee(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.owner,
            &ctx.accounts.payer_naim_ata,
            &ctx.accounts.treasury_naim_ata,
            &ctx.accounts.stakers_vault_ata,
            &mut ctx.accounts.token_config,
            fee,
        )?;

        let now = Clock::get()?.unix_timestamp;
        let registration_period = ctx.accounts.config.registration_period;
        let rec = &mut ctx.accounts.name_record;
        let base = core::cmp::max(now, rec.expiry_timestamp);
        rec.expiry_timestamp = base + registration_period;

        let rep = &mut ctx.accounts.reputation_record;
        if rep.created_at == 0 {
            rep.name_hash = name_hash;
            rep.created_at = now;
            rep.bump = ctx.bumps.reputation_record;
        }
        rep.renew_count = rep.renew_count.saturating_add(1);
        Ok(())
    }

    /// Voluntarily release a name owned by the caller, refunding rent.
    pub fn release_name(_ctx: Context<ReleaseName>) -> Result<()> {
        // `close = owner` on the account refunds rent and zeroes the PDA,
        // freeing the name for re-registration.
        Ok(())
    }

    // ===== OWNERSHIP / RESOLUTION ====================================

    /// Transfer ownership to a new pubkey.
    pub fn transfer_name(ctx: Context<UpdateName>, new_owner: Pubkey) -> Result<()> {
        ctx.accounts.name_record.owner = new_owner;
        Ok(())
    }

    /// Change the wallet/program a name resolves to.
    pub fn update_resolver(ctx: Context<UpdateName>, new_resolver: Pubkey) -> Result<()> {
        ctx.accounts.name_record.resolver = new_resolver;
        Ok(())
    }

    /// Change the AgentCard metadata URI.
    pub fn update_metadata(ctx: Context<UpdateName>, new_uri: String) -> Result<()> {
        require!(
            new_uri.len() <= MAX_METADATA_URI_LEN,
            NaimError::MetadataUriTooLong
        );
        ctx.accounts.name_record.metadata_uri = new_uri;
        Ok(())
    }

    /// Add a secondary wallet to the identity. Both the owner and the new
    /// wallet must sign.
    pub fn link_wallet(ctx: Context<LinkWallet>) -> Result<()> {
        let new_wallet = ctx.accounts.new_wallet.key();
        let rec = &mut ctx.accounts.name_record;
        require!(
            rec.linked_wallets.len() < MAX_LINKED_WALLETS,
            NaimError::TooManyLinkedWallets
        );
        require!(
            new_wallet != rec.owner && !rec.linked_wallets.contains(&new_wallet),
            NaimError::WalletAlreadyLinked
        );
        rec.linked_wallets.push(new_wallet);
        Ok(())
    }

    /// Set the verified flag in exchange for a payment to the treasury.
    pub fn verify_name(ctx: Context<VerifyName>) -> Result<()> {
        let fee = ctx.accounts.token_config.verify_fee;
        collect_fee(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.owner,
            &ctx.accounts.payer_naim_ata,
            &ctx.accounts.treasury_naim_ata,
            &ctx.accounts.stakers_vault_ata,
            &mut ctx.accounts.token_config,
            fee,
        )?;
        ctx.accounts.name_record.verified = true;
        Ok(())
    }

    // ===== STAKING (update/2) ========================================

    /// One-time staking setup. Creates the pool + the principal vault. Rewards
    /// are paid from the stakers vault (the tokenomics update's fee sink).
    pub fn init_stake_pool(ctx: Context<InitStakePool>) -> Result<()> {
        let p = &mut ctx.accounts.stake_pool;
        p.admin = ctx.accounts.admin.key();
        p.naim_mint = ctx.accounts.naim_mint.key();
        p.total_staked = 0;
        p.acc_reward_per_share = 0;
        // skip rewards that arrived before any staking existed
        p.last_reward_balance = ctx.accounts.rewards_vault.amount;
        p.bump = ctx.bumps.stake_pool;
        p.stake_vault_bump = ctx.bumps.stake_vault;
        Ok(())
    }

    /// Stake $NAIM (locking it for `lock_seconds`). Harvests any pending rewards
    /// first. A larger stake means a bigger registration discount.
    pub fn stake(ctx: Context<Stake>, amount: u64, lock_seconds: i64) -> Result<()> {
        require!(amount > 0 && lock_seconds >= 0, NaimError::InvalidConfig);
        let rewards_bal = ctx.accounts.rewards_vault.amount;
        sync_pool(&mut ctx.accounts.stake_pool, rewards_bal);
        let acc = ctx.accounts.stake_pool.acc_reward_per_share;
        let decimals = ctx.accounts.naim_mint.decimals;

        if ctx.accounts.stake_account.owner == Pubkey::default() {
            ctx.accounts.stake_account.owner = ctx.accounts.user.key();
            ctx.accounts.stake_account.bump = ctx.bumps.stake_account;
        }
        let pend = pending(&ctx.accounts.stake_account, acc);

        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.user_naim_ata.to_account_info(),
                    mint: ctx.accounts.naim_mint.to_account_info(),
                    to: ctx.accounts.stake_vault_ata.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount,
            decimals,
        )?;

        let now = Clock::get()?.unix_timestamp;
        {
            let s = &mut ctx.accounts.stake_account;
            s.amount = s.amount.checked_add(amount).unwrap();
            let new_lock = now + lock_seconds;
            if new_lock > s.lock_end {
                s.lock_end = new_lock;
            }
            s.reward_debt = (s.amount as u128) * acc / ACC_PRECISION;
        }
        ctx.accounts.stake_pool.total_staked =
            ctx.accounts.stake_pool.total_staked.checked_add(amount).unwrap();

        if pend > 0 {
            let bump = ctx.accounts.token_config.vault_bump;
            pay_from_vault(
                &ctx.accounts.token_program,
                &ctx.accounts.naim_mint,
                &ctx.accounts.rewards_vault,
                &ctx.accounts.user_naim_ata,
                &ctx.accounts.stakers_vault,
                &[STAKERS_VAULT_SEED, &[bump]],
                pend,
                decimals,
            )?;
            ctx.accounts.stake_pool.last_reward_balance =
                ctx.accounts.stake_pool.last_reward_balance.saturating_sub(pend);
        }
        Ok(())
    }

    /// Claim accrued rewards without touching principal.
    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        let rewards_bal = ctx.accounts.rewards_vault.amount;
        sync_pool(&mut ctx.accounts.stake_pool, rewards_bal);
        let acc = ctx.accounts.stake_pool.acc_reward_per_share;
        let decimals = ctx.accounts.naim_mint.decimals;
        let pend = pending(&ctx.accounts.stake_account, acc);
        require!(pend > 0, NaimError::NothingToClaim);

        let bump = ctx.accounts.token_config.vault_bump;
        pay_from_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.rewards_vault,
            &ctx.accounts.user_naim_ata,
            &ctx.accounts.stakers_vault,
            &[STAKERS_VAULT_SEED, &[bump]],
            pend,
            decimals,
        )?;
        ctx.accounts.stake_account.reward_debt =
            (ctx.accounts.stake_account.amount as u128) * acc / ACC_PRECISION;
        ctx.accounts.stake_pool.last_reward_balance =
            ctx.accounts.stake_pool.last_reward_balance.saturating_sub(pend);
        Ok(())
    }

    /// Unstake principal after the lock expires; harvests pending rewards too.
    pub fn unstake(ctx: Context<Unstake>, amount: u64) -> Result<()> {
        require!(amount > 0, NaimError::InvalidConfig);
        let now = Clock::get()?.unix_timestamp;
        require!(now >= ctx.accounts.stake_account.lock_end, NaimError::StakeLocked);
        require!(amount <= ctx.accounts.stake_account.amount, NaimError::InsufficientStake);

        let rewards_bal = ctx.accounts.rewards_vault.amount;
        sync_pool(&mut ctx.accounts.stake_pool, rewards_bal);
        let acc = ctx.accounts.stake_pool.acc_reward_per_share;
        let decimals = ctx.accounts.naim_mint.decimals;
        let pend = pending(&ctx.accounts.stake_account, acc);

        let svbump = ctx.accounts.stake_pool.stake_vault_bump;
        pay_from_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.stake_vault_ata,
            &ctx.accounts.user_naim_ata,
            &ctx.accounts.stake_vault,
            &[STAKE_VAULT_SEED, &[svbump]],
            amount,
            decimals,
        )?;
        if pend > 0 {
            let bump = ctx.accounts.token_config.vault_bump;
            pay_from_vault(
                &ctx.accounts.token_program,
                &ctx.accounts.naim_mint,
                &ctx.accounts.rewards_vault,
                &ctx.accounts.user_naim_ata,
                &ctx.accounts.stakers_vault,
                &[STAKERS_VAULT_SEED, &[bump]],
                pend,
                decimals,
            )?;
            ctx.accounts.stake_pool.last_reward_balance =
                ctx.accounts.stake_pool.last_reward_balance.saturating_sub(pend);
        }
        {
            let s = &mut ctx.accounts.stake_account;
            s.amount -= amount;
            s.reward_debt = (s.amount as u128) * acc / ACC_PRECISION;
        }
        ctx.accounts.stake_pool.total_staked -= amount;
        Ok(())
    }

    // ===== MARKETPLACE ===============================================

    /// One-time marketplace setup: pins the accepted USDC mint and the two fee
    /// tiers ($NAIM cheaper than SOL/USDC). Admin-only (gated by Config.admin).
    pub fn init_market_config(ctx: Context<InitMarketConfig>, p: MarketConfigParams) -> Result<()> {
        require!(
            p.fee_naim_bps < BPS_DENOM && p.fee_stable_bps < BPS_DENOM,
            NaimError::InvalidConfig
        );
        let mc = &mut ctx.accounts.market_config;
        mc.admin = ctx.accounts.admin.key();
        mc.usdc_mint = p.usdc_mint;
        mc.treasury = p.treasury;
        mc.fee_naim_bps = p.fee_naim_bps;
        mc.fee_stable_bps = p.fee_stable_bps;
        mc.bump = ctx.bumps.market_config;
        Ok(())
    }

    /// Admin-only: tune the marketplace fees / USDC mint / treasury live.
    pub fn update_market_config(
        ctx: Context<UpdateMarketConfig>,
        p: MarketConfigParams,
    ) -> Result<()> {
        require!(
            p.fee_naim_bps < BPS_DENOM && p.fee_stable_bps < BPS_DENOM,
            NaimError::InvalidConfig
        );
        let mc = &mut ctx.accounts.market_config;
        mc.usdc_mint = p.usdc_mint;
        mc.treasury = p.treasury;
        mc.fee_naim_bps = p.fee_naim_bps;
        mc.fee_stable_bps = p.fee_stable_bps;
        Ok(())
    }

    /// List a name you own for sale at `price` in `currency` (0=$NAIM, 1=SOL,
    /// 2=USDC). The name stays yours until someone buys it; the Listing is your
    /// on-chain authorization for the buy instruction to reassign the record.
    /// One listing per name (PDA per name_hash).
    pub fn list_name(
        ctx: Context<ListName>,
        name_hash: [u8; 32],
        currency: u8,
        price: u64,
    ) -> Result<()> {
        require!(price > 0, NaimError::InvalidPrice);
        require!(
            currency == CURRENCY_NAIM || currency == CURRENCY_SOL || currency == CURRENCY_USDC,
            NaimError::InvalidCurrency
        );
        let l = &mut ctx.accounts.listing;
        l.seller = ctx.accounts.owner.key();
        l.name_hash = name_hash;
        l.currency = currency;
        l.price = price;
        l.created_at = Clock::get()?.unix_timestamp;
        l.bump = ctx.bumps.listing;
        Ok(())
    }

    /// Change the asking price of your listing (currency stays the same).
    pub fn update_listing(
        ctx: Context<UpdateListing>,
        _name_hash: [u8; 32],
        new_price: u64,
    ) -> Result<()> {
        require!(new_price > 0, NaimError::InvalidPrice);
        ctx.accounts.listing.price = new_price;
        Ok(())
    }

    /// Delist your name and reclaim the listing rent.
    pub fn unlist_name(_ctx: Context<UnlistName>, _name_hash: [u8; 32]) -> Result<()> {
        Ok(())
    }

    /// Buy a name listed in $NAIM or USDC. The buyer pays `price` in the pay
    /// mint: the fee (lower for $NAIM) goes to the treasury, the rest to the
    /// seller. The NameRecord is reassigned to the buyer and the listing closed
    /// (rent to the seller). Fails if the seller no longer owns the name.
    pub fn buy_with_token(ctx: Context<BuyWithToken>, _name_hash: [u8; 32]) -> Result<()> {
        require!(
            ctx.accounts.name_record.owner == ctx.accounts.listing.seller,
            NaimError::SellerNotOwner
        );
        require!(
            ctx.accounts.buyer.key() != ctx.accounts.listing.seller,
            NaimError::SelfPurchase
        );
        let mc = &ctx.accounts.market_config;
        // route the pay mint + fee tier by the listing's currency
        let (expected_mint, fee_bps) = match ctx.accounts.listing.currency {
            CURRENCY_NAIM => (ctx.accounts.token_config.naim_mint, mc.fee_naim_bps),
            CURRENCY_USDC => (mc.usdc_mint, mc.fee_stable_bps),
            _ => return err!(NaimError::CurrencyMismatch), // SOL -> buy_with_sol
        };
        require!(
            ctx.accounts.pay_mint.key() == expected_mint,
            NaimError::WrongPayMint
        );

        let price = ctx.accounts.listing.price;
        let fee = (price as u128 * fee_bps as u128 / BPS_DENOM as u128) as u64;
        let to_seller = price.saturating_sub(fee);
        let decimals = ctx.accounts.pay_mint.decimals;

        if to_seller > 0 {
            token_interface::transfer_checked(
                CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.buyer_ata.to_account_info(),
                        mint: ctx.accounts.pay_mint.to_account_info(),
                        to: ctx.accounts.seller_ata.to_account_info(),
                        authority: ctx.accounts.buyer.to_account_info(),
                    },
                ),
                to_seller,
                decimals,
            )?;
        }
        if fee > 0 {
            token_interface::transfer_checked(
                CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.buyer_ata.to_account_info(),
                        mint: ctx.accounts.pay_mint.to_account_info(),
                        to: ctx.accounts.treasury_ata.to_account_info(),
                        authority: ctx.accounts.buyer.to_account_info(),
                    },
                ),
                fee,
                decimals,
            )?;
        }

        let rec = &mut ctx.accounts.name_record;
        rec.owner = ctx.accounts.buyer.key();
        rec.resolver = ctx.accounts.buyer.key();
        Ok(())
    }

    /// Buy a name listed in SOL. The buyer pays `price` lamports: the fee goes
    /// to the treasury wallet, the rest to the seller, both by native transfer.
    /// The NameRecord is reassigned and the listing closed (rent to the seller).
    pub fn buy_with_sol(ctx: Context<BuyWithSol>, _name_hash: [u8; 32]) -> Result<()> {
        require!(
            ctx.accounts.name_record.owner == ctx.accounts.listing.seller,
            NaimError::SellerNotOwner
        );
        require!(
            ctx.accounts.buyer.key() != ctx.accounts.listing.seller,
            NaimError::SelfPurchase
        );
        require!(
            ctx.accounts.listing.currency == CURRENCY_SOL,
            NaimError::CurrencyMismatch
        );

        let price = ctx.accounts.listing.price;
        let fee = (price as u128 * ctx.accounts.market_config.fee_stable_bps as u128
            / BPS_DENOM as u128) as u64;
        let to_seller = price.saturating_sub(fee);

        if to_seller > 0 {
            sol_transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    SolTransfer {
                        from: ctx.accounts.buyer.to_account_info(),
                        to: ctx.accounts.seller.to_account_info(),
                    },
                ),
                to_seller,
            )?;
        }
        if fee > 0 {
            sol_transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    SolTransfer {
                        from: ctx.accounts.buyer.to_account_info(),
                        to: ctx.accounts.treasury.to_account_info(),
                    },
                ),
                fee,
            )?;
        }

        let rec = &mut ctx.accounts.name_record;
        rec.owner = ctx.accounts.buyer.key();
        rec.resolver = ctx.accounts.buyer.key();
        Ok(())
    }

    // ===== CATEGORIES (update/4) =====================================

    /// Register a category namespace (e.g. "defi.agent") — the creator owns it
    /// and earns a 5% royalty on every sub-name minted beneath it. Fee in $NAIM.
    pub fn register_category(
        ctx: Context<RegisterCategory>,
        category: String,
        category_hash: [u8; 32],
    ) -> Result<()> {
        let len = validate_category(&category)?;
        require!(
            sha256(category.as_bytes()).to_bytes() == category_hash,
            NaimError::InvalidCategory
        );
        let fee = fee_for(&ctx.accounts.token_config, len);
        collect_fee(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.payer,
            &ctx.accounts.payer_naim_ata,
            &ctx.accounts.treasury_naim_ata,
            &ctx.accounts.stakers_vault_ata,
            &mut ctx.accounts.token_config,
            fee,
        )?;
        let c = &mut ctx.accounts.category_record;
        c.owner = ctx.accounts.payer.key();
        c.royalty_bps = DEFAULT_ROYALTY_BPS;
        c.sub_count = 0;
        c.total_earned = 0;
        c.bump = ctx.bumps.category_record;
        Ok(())
    }

    /// Register a name directly under a category. Routes the 5% royalty (in
    /// $NAIM) to the category owner; the remainder splits treasury/stakers/burn.
    pub fn register_under_category(
        ctx: Context<RegisterUnderCategory>,
        name: String,
        name_hash: [u8; 32],
        parent: String,
        parent_hash: [u8; 32],
        metadata_uri: String,
    ) -> Result<()> {
        let len = validate_name(&name)?;
        require!(sha256(name.as_bytes()).to_bytes() == name_hash, NaimError::InvalidName);
        require!(
            sha256(parent.as_bytes()).to_bytes() == parent_hash,
            NaimError::InvalidCategory
        );
        require!(
            name.split_once('.').map(|(_, rest)| rest) == Some(parent.as_str()),
            NaimError::NotUnderCategory
        );
        require!(
            metadata_uri.len() <= MAX_METADATA_URI_LEN,
            NaimError::MetadataUriTooLong
        );

        let fee = fee_for(&ctx.accounts.token_config, len);
        let royalty =
            (fee as u128 * ctx.accounts.category.royalty_bps as u128 / BPS_DENOM as u128) as u64;
        let rem = fee.saturating_sub(royalty);
        let decimals = ctx.accounts.naim_mint.decimals;

        if royalty > 0 {
            token_interface::transfer_checked(
                CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.payer_naim_ata.to_account_info(),
                        mint: ctx.accounts.naim_mint.to_account_info(),
                        to: ctx.accounts.category_owner_naim_ata.to_account_info(),
                        authority: ctx.accounts.payer.to_account_info(),
                    },
                ),
                royalty,
                decimals,
            )?;
        }
        collect_fee(
            &ctx.accounts.token_program,
            &ctx.accounts.naim_mint,
            &ctx.accounts.payer,
            &ctx.accounts.payer_naim_ata,
            &ctx.accounts.treasury_naim_ata,
            &ctx.accounts.stakers_vault_ata,
            &mut ctx.accounts.token_config,
            rem,
        )?;
        ctx.accounts.category.sub_count = ctx.accounts.category.sub_count.saturating_add(1);
        ctx.accounts.category.total_earned =
            ctx.accounts.category.total_earned.saturating_add(royalty);

        let now = Clock::get()?.unix_timestamp;
        let registration_period = ctx.accounts.config.registration_period;
        {
            let rec = &mut ctx.accounts.name_record;
            rec.owner = ctx.accounts.payer.key();
            rec.resolver = ctx.accounts.payer.key();
            rec.metadata_uri = metadata_uri;
            rec.expiry_timestamp = if is_permanent(len) { 0 } else { now + registration_period };
            rec.verified = false;
            rec.linked_wallets = Vec::new();
            rec.bump = ctx.bumps.name_record;
        }
        let rep = &mut ctx.accounts.reputation_record;
        rep.name_hash = name_hash;
        rep.created_at = now;
        rep.renew_count = 0;
        rep.bump = ctx.bumps.reputation_record;
        Ok(())
    }

    /// Transfer category ownership (the royalty stream) to a new pubkey.
    pub fn transfer_category(ctx: Context<UpdateCategory>, new_owner: Pubkey) -> Result<()> {
        ctx.accounts.category_record.owner = new_owner;
        Ok(())
    }

    /// Owner closes a category and reclaims its rent. Sub-names already minted
    /// keep resolving; they simply no longer sit under a live category.
    pub fn close_category(_ctx: Context<CloseCategory>) -> Result<()> {
        Ok(())
    }

    // ===== SPONSORED DISCOVERY (update/5) ============================

    /// Bid $NAIM to boost one of your names for a capability. The bid is burned
    /// 100% (pure deflation, no receiver). Bids accumulate within an epoch; a
    /// new epoch resets the amount. The off-chain ranker boosts names whose bid
    /// was placed in the previous epoch (so positions are stable within an epoch).
    pub fn place_rank_bid(
        ctx: Context<PlaceRankBid>,
        name_hash: [u8; 32],
        capability: String,
        capability_hash: [u8; 32],
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, NaimError::InvalidConfig);
        require!(
            sha256(capability.as_bytes()).to_bytes() == capability_hash,
            NaimError::InvalidName
        );

        // 100% burn — visibility spend is a permanent supply sink, no fee receiver.
        token_interface::burn(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Burn {
                    mint: ctx.accounts.naim_mint.to_account_info(),
                    from: ctx.accounts.bidder_naim_ata.to_account_info(),
                    authority: ctx.accounts.owner.to_account_info(),
                },
            ),
            amount,
        )?;
        ctx.accounts.token_config.total_burned =
            ctx.accounts.token_config.total_burned.saturating_add(amount);

        let epoch = Clock::get()?.unix_timestamp / RANK_EPOCH_SECS;
        let b = &mut ctx.accounts.rank_bid;
        if b.owner == Pubkey::default() {
            b.owner = ctx.accounts.owner.key();
            b.name_hash = name_hash;
            b.capability_hash = capability_hash;
            b.bump = ctx.bumps.rank_bid;
        }
        if b.epoch == epoch {
            b.amount = b.amount.saturating_add(amount);
        } else {
            b.epoch = epoch;
            b.amount = amount;
        }
        Ok(())
    }
}

// ===== HELPERS =======================================================

/// Validate a name's charset and length; return its character length.
fn validate_name(name: &str) -> Result<usize> {
    let len = name.chars().count();
    require!(len >= 1 && len <= MAX_NAME_LEN, NaimError::InvalidName);
    for ch in name.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.';
        require!(ok, NaimError::InvalidName);
    }
    // no leading/trailing/double dots, no leading/trailing hyphen
    require!(
        !name.starts_with('.')
            && !name.ends_with('.')
            && !name.contains("..")
            && !name.starts_with('-')
            && !name.ends_with('-'),
        NaimError::InvalidName
    );
    Ok(len)
}

fn is_permanent(char_len: usize) -> bool {
    char_len <= 4
}

/// A category must be a valid name that ends in the TLD (e.g. "defi.agent").
fn validate_category(category: &str) -> Result<usize> {
    let len = validate_name(category)?;
    require!(
        category.as_bytes().ends_with(TLD_AGENT) && category.len() > TLD_AGENT.len(),
        NaimError::InvalidCategory
    );
    Ok(len)
}

fn fee_for(tc: &TokenConfig, char_len: usize) -> u64 {
    if char_len <= 4 {
        tc.fee_1_4
    } else if char_len <= 9 {
        tc.fee_5_9
    } else {
        tc.fee_10_plus
    }
}

/// Collect a fee in $NAIM and split it: `treasury_bps` to the treasury token
/// account, `stakers_bps` to the stakers vault, the remainder burned (so no
/// rounding dust escapes). All three legs are authorized by the payer's
/// signature — no PDA signing, no buyback crank.
#[allow(clippy::too_many_arguments)]
fn collect_fee<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    payer: &Signer<'info>,
    payer_ata: &InterfaceAccount<'info, TokenAccount>,
    treasury_ata: &InterfaceAccount<'info, TokenAccount>,
    stakers_vault_ata: &InterfaceAccount<'info, TokenAccount>,
    token_config: &mut Account<'info, TokenConfig>,
    fee: u64,
) -> Result<()> {
    if fee == 0 {
        return Ok(());
    }
    let treasury_amt =
        (fee as u128 * token_config.treasury_bps as u128 / BPS_DENOM as u128) as u64;
    let stakers_amt =
        (fee as u128 * token_config.stakers_bps as u128 / BPS_DENOM as u128) as u64;
    let burn_amt = fee
        .checked_sub(treasury_amt)
        .and_then(|x| x.checked_sub(stakers_amt))
        .ok_or(NaimError::InvalidConfig)?;
    let decimals = mint.decimals;

    if treasury_amt > 0 {
        token_interface::transfer_checked(
            CpiContext::new(
                token_program.to_account_info(),
                TransferChecked {
                    from: payer_ata.to_account_info(),
                    mint: mint.to_account_info(),
                    to: treasury_ata.to_account_info(),
                    authority: payer.to_account_info(),
                },
            ),
            treasury_amt,
            decimals,
        )?;
    }
    if stakers_amt > 0 {
        token_interface::transfer_checked(
            CpiContext::new(
                token_program.to_account_info(),
                TransferChecked {
                    from: payer_ata.to_account_info(),
                    mint: mint.to_account_info(),
                    to: stakers_vault_ata.to_account_info(),
                    authority: payer.to_account_info(),
                },
            ),
            stakers_amt,
            decimals,
        )?;
    }
    if burn_amt > 0 {
        token_interface::burn(
            CpiContext::new(
                token_program.to_account_info(),
                Burn {
                    mint: mint.to_account_info(),
                    from: payer_ata.to_account_info(),
                    authority: payer.to_account_info(),
                },
            ),
            burn_amt,
        )?;
        token_config.total_burned = token_config.total_burned.saturating_add(burn_amt);
    }
    Ok(())
}

/// Reduce a fee by the discount the caller's staked balance earns.
fn apply_discount(base: u64, staked: u64) -> u64 {
    let bps: u16 = if staked >= STAKE_DISCOUNT_TIER2 {
        STAKE_DISCOUNT_BPS2
    } else if staked >= STAKE_DISCOUNT_TIER1 {
        STAKE_DISCOUNT_BPS1
    } else {
        0
    };
    base - (base as u128 * bps as u128 / BPS_DENOM as u128) as u64
}

/// Fold newly-arrived rewards (the growth of the stakers vault) into the
/// per-share accumulator. Rewards arriving while nothing is staked are skipped.
fn sync_pool(pool: &mut StakePool, rewards_balance: u64) {
    if pool.total_staked > 0 {
        let delta = rewards_balance.saturating_sub(pool.last_reward_balance);
        if delta > 0 {
            pool.acc_reward_per_share +=
                (delta as u128) * ACC_PRECISION / (pool.total_staked as u128);
        }
    }
    pool.last_reward_balance = rewards_balance;
}

/// Rewards owed to a position since its last interaction.
fn pending(stake: &StakeAccount, acc: u128) -> u64 {
    (((stake.amount as u128) * acc / ACC_PRECISION).saturating_sub(stake.reward_debt)) as u64
}

/// Transfer `amount` out of a program-owned vault, signed by its PDA authority.
#[allow(clippy::too_many_arguments)]
fn pay_from_vault<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    from_ata: &InterfaceAccount<'info, TokenAccount>,
    to_ata: &InterfaceAccount<'info, TokenAccount>,
    vault_authority: &UncheckedAccount<'info>,
    seeds: &[&[u8]],
    amount: u64,
    decimals: u8,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            TransferChecked {
                from: from_ata.to_account_info(),
                mint: mint.to_account_info(),
                to: to_ata.to_account_info(),
                authority: vault_authority.to_account_info(),
            },
            &[seeds],
        ),
        amount,
        decimals,
    )?;
    Ok(())
}

// ===== PARAMS ========================================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ConfigParams {
    pub treasury: Pubkey,
    pub fee_1_4: u64,
    pub fee_5_9: u64,
    pub fee_10_plus: u64,
    pub verify_fee: u64,
    pub registration_period: i64,
    pub grace_period: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct TokenConfigParams {
    /// Per-tier fees in $NAIM base units.
    pub fee_1_4: u64,
    pub fee_5_9: u64,
    pub fee_10_plus: u64,
    pub verify_fee: u64,
    /// Fee split in basis points; the three must sum to 10_000.
    pub treasury_bps: u16,
    pub stakers_bps: u16,
    pub burn_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct MarketConfigParams {
    /// USDC SPL mint accepted as a sale currency.
    pub usdc_mint: Pubkey,
    /// Wallet that receives marketplace fees.
    pub treasury: Pubkey,
    /// Fee (bps) on a $NAIM sale — the cheaper tier.
    pub fee_naim_bps: u16,
    /// Fee (bps) on a SOL or USDC sale.
    pub fee_stable_bps: u16,
}

// ===== ACCOUNTS ======================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ NaimError::Unauthorized)]
    pub config: Box<Account<'info, Config>>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct InitTokenConfig<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + TokenConfig::INIT_SPACE,
        seeds = [TOKEN_CONFIG_SEED],
        bump
    )]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Wallet that owns the treasury token account.
    pub treasury: SystemAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = naim_mint,
        associated_token::authority = treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: PDA authority for the stakers-vault token account, by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateTokenConfig<'info> {
    #[account(
        mut,
        seeds = [TOKEN_CONFIG_SEED],
        bump = token_config.bump,
        has_one = admin @ NaimError::Unauthorized,
    )]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Wallet that owns the treasury token account.
    pub treasury: SystemAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = naim_mint,
        associated_token::authority = treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: PDA authority for the stakers-vault token account, by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name: String, name_hash: [u8; 32])]
pub struct RegisterName<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    // init_if_needed: an expired-past-grace name can be reclaimed (handler resets it).
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + NameRecord::INIT_SPACE,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub payer: Signer<'info>,
    // init_if_needed: a released name's reputation record may still exist; the
    // handler overwrites all fields, so re-registration always starts fresh.
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + ReputationRecord::INIT_SPACE,
        seeds = [REP_SEED, name_hash.as_ref()],
        bump
    )]
    pub reputation_record: Box<Account<'info, ReputationRecord>>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = payer,
        associated_token::token_program = token_program,
    )]
    pub payer_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = token_config.treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: PDA authority for the stakers vault, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    /// Optional: the payer's stake position — grants a fee discount if present.
    /// No seeds (so the client can omit it with `null`); the handler only honors
    /// it when `stake_account.owner == payer`, and there is one stake PDA per
    /// owner, so a forged or someone else's account grants no discount.
    pub stake_account: Option<Box<Account<'info, StakeAccount>>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name: String, name_hash: [u8; 32])]
pub struct RenewName<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(
        mut,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump = name_record.bump,
        has_one = owner @ NaimError::Unauthorized
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + ReputationRecord::INIT_SPACE,
        seeds = [REP_SEED, name_hash.as_ref()],
        bump
    )]
    pub reputation_record: Box<Account<'info, ReputationRecord>>,
    pub system_program: Program<'info, System>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = owner,
        associated_token::token_program = token_program,
    )]
    pub payer_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = token_config.treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: PDA authority for the stakers vault, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ReleaseName<'info> {
    #[account(
        mut,
        has_one = owner @ NaimError::Unauthorized,
        close = owner
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

/// Shared context for owner-only single-field updates
/// (transfer_name / update_resolver / update_metadata).
#[derive(Accounts)]
pub struct UpdateName<'info> {
    #[account(mut, has_one = owner @ NaimError::Unauthorized)]
    pub name_record: Box<Account<'info, NameRecord>>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct LinkWallet<'info> {
    #[account(mut, has_one = owner @ NaimError::Unauthorized)]
    pub name_record: Box<Account<'info, NameRecord>>,
    pub owner: Signer<'info>,
    pub new_wallet: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyName<'info> {
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut, has_one = owner @ NaimError::Unauthorized)]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = owner,
        associated_token::token_program = token_program,
    )]
    pub payer_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = token_config.treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: PDA authority for the stakers vault, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
}

// ===== staking accounts (update/2) ===================================

#[derive(Accounts)]
pub struct InitStakePool<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + StakePool::INIT_SPACE,
        seeds = [STAKE_POOL_SEED],
        bump
    )]
    pub stake_pool: Box<Account<'info, StakePool>>,
    #[account(seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump, has_one = admin)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: stake-vault authority PDA, verified by seeds.
    #[account(seeds = [STAKE_VAULT_SEED], bump)]
    pub stake_vault: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = naim_mint,
        associated_token::authority = stake_vault,
        associated_token::token_program = token_program,
    )]
    pub stake_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault (rewards) authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub rewards_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump)]
    pub stake_pool: Box<Account<'info, StakePool>>,
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + StakeAccount::INIT_SPACE,
        seeds = [STAKE_SEED, user.key().as_ref()],
        bump
    )]
    pub stake_account: Box<Account<'info, StakeAccount>>,
    #[account(seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(address = stake_pool.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = user,
        associated_token::token_program = token_program,
    )]
    pub user_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stake-vault authority PDA, verified by seeds.
    #[account(seeds = [STAKE_VAULT_SEED], bump = stake_pool.stake_vault_bump)]
    pub stake_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stake_vault,
        associated_token::token_program = token_program,
    )]
    pub stake_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault (rewards) authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub rewards_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump)]
    pub stake_pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, user.key().as_ref()], bump = stake_account.bump)]
    pub stake_account: Box<Account<'info, StakeAccount>>,
    #[account(seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(address = stake_pool.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = user,
        associated_token::token_program = token_program,
    )]
    pub user_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault (rewards) authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub rewards_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut, seeds = [STAKE_POOL_SEED], bump = stake_pool.bump)]
    pub stake_pool: Box<Account<'info, StakePool>>,
    #[account(mut, seeds = [STAKE_SEED, user.key().as_ref()], bump = stake_account.bump)]
    pub stake_account: Box<Account<'info, StakeAccount>>,
    #[account(seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(address = stake_pool.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = user,
        associated_token::token_program = token_program,
    )]
    pub user_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stake-vault authority PDA, verified by seeds.
    #[account(seeds = [STAKE_VAULT_SEED], bump = stake_pool.stake_vault_bump)]
    pub stake_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stake_vault,
        associated_token::token_program = token_program,
    )]
    pub stake_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault (rewards) authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub rewards_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

// ===== MARKETPLACE ACCOUNTS ==========================================

#[derive(Accounts)]
pub struct InitMarketConfig<'info> {
    // gate: only the protocol admin (Config.admin) may create the market config
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ NaimError::Unauthorized)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        init,
        payer = admin,
        space = 8 + MarketConfig::INIT_SPACE,
        seeds = [MARKET_CONFIG_SEED],
        bump
    )]
    pub market_config: Box<Account<'info, MarketConfig>>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateMarketConfig<'info> {
    #[account(
        mut,
        seeds = [MARKET_CONFIG_SEED],
        bump = market_config.bump,
        has_one = admin @ NaimError::Unauthorized,
    )]
    pub market_config: Box<Account<'info, MarketConfig>>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32])]
pub struct ListName<'info> {
    // seller must own the name being listed
    #[account(
        has_one = owner @ NaimError::Unauthorized,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump = name_record.bump,
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(
        init,
        payer = owner,
        space = 8 + Listing::INIT_SPACE,
        seeds = [LISTING_SEED, name_hash.as_ref()],
        bump
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32])]
pub struct UpdateListing<'info> {
    #[account(
        mut,
        has_one = seller @ NaimError::Unauthorized,
        seeds = [LISTING_SEED, name_hash.as_ref()],
        bump = listing.bump,
    )]
    pub listing: Box<Account<'info, Listing>>,
    pub seller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32])]
pub struct UnlistName<'info> {
    #[account(
        mut,
        close = seller,
        has_one = seller @ NaimError::Unauthorized,
        seeds = [LISTING_SEED, name_hash.as_ref()],
        bump = listing.bump,
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(mut)]
    pub seller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32])]
pub struct BuyWithToken<'info> {
    #[account(
        mut,
        close = seller,
        has_one = seller @ NaimError::Unauthorized,
        seeds = [LISTING_SEED, name_hash.as_ref()],
        bump = listing.bump,
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        mut,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump = name_record.bump,
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub buyer: Signer<'info>,
    /// CHECK: seller = listing.seller (has_one); receives the sale proceeds + listing rent.
    #[account(mut)]
    pub seller: UncheckedAccount<'info>,
    #[account(seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(seeds = [MARKET_CONFIG_SEED], bump = market_config.bump)]
    pub market_config: Box<Account<'info, MarketConfig>>,
    /// CHECK: must equal market_config.treasury; owns the treasury pay-mint ATA
    /// (needed as an account so init_if_needed can create that ATA).
    #[account(address = market_config.treasury @ NaimError::Unauthorized)]
    pub market_treasury: UncheckedAccount<'info>,
    /// The pay mint — validated in the handler to match the listing's currency
    /// ($NAIM mint for CURRENCY_NAIM, usdc_mint for CURRENCY_USDC).
    #[account(mut)]
    pub pay_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = pay_mint,
        associated_token::authority = buyer,
        associated_token::token_program = token_program,
    )]
    pub buyer_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = buyer,
        associated_token::mint = pay_mint,
        associated_token::authority = seller,
        associated_token::token_program = token_program,
    )]
    pub seller_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = buyer,
        associated_token::mint = pay_mint,
        associated_token::authority = market_treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32])]
pub struct BuyWithSol<'info> {
    #[account(
        mut,
        close = seller,
        has_one = seller @ NaimError::Unauthorized,
        seeds = [LISTING_SEED, name_hash.as_ref()],
        bump = listing.bump,
    )]
    pub listing: Box<Account<'info, Listing>>,
    #[account(
        mut,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump = name_record.bump,
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(mut)]
    pub buyer: Signer<'info>,
    /// CHECK: seller = listing.seller (has_one); receives the sale proceeds + listing rent.
    #[account(mut)]
    pub seller: UncheckedAccount<'info>,
    #[account(seeds = [MARKET_CONFIG_SEED], bump = market_config.bump)]
    pub market_config: Box<Account<'info, MarketConfig>>,
    /// CHECK: must equal market_config.treasury; receives the SOL fee.
    #[account(mut, address = market_config.treasury @ NaimError::Unauthorized)]
    pub treasury: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

// ===== category accounts (update/4) ==================================

#[derive(Accounts)]
#[instruction(category: String, category_hash: [u8; 32])]
pub struct RegisterCategory<'info> {
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(
        init,
        payer = payer,
        space = 8 + CategoryRecord::INIT_SPACE,
        seeds = [CATEGORY_SEED, category_hash.as_ref()],
        bump
    )]
    pub category_record: Box<Account<'info, CategoryRecord>>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = payer,
        associated_token::token_program = token_program,
    )]
    pub payer_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = token_config.treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name: String, name_hash: [u8; 32], parent: String, parent_hash: [u8; 32])]
pub struct RegisterUnderCategory<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    #[account(
        init,
        payer = payer,
        space = 8 + NameRecord::INIT_SPACE,
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + ReputationRecord::INIT_SPACE,
        seeds = [REP_SEED, name_hash.as_ref()],
        bump
    )]
    pub reputation_record: Box<Account<'info, ReputationRecord>>,
    #[account(mut, seeds = [CATEGORY_SEED, parent_hash.as_ref()], bump = category.bump)]
    pub category: Box<Account<'info, CategoryRecord>>,
    /// CHECK: category owner (royalty recipient), verified against the record.
    #[account(address = category.owner @ NaimError::Unauthorized)]
    pub category_owner: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = payer,
        associated_token::token_program = token_program,
    )]
    pub payer_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = naim_mint,
        associated_token::authority = category_owner,
        associated_token::token_program = token_program,
    )]
    pub category_owner_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = token_config.treasury,
        associated_token::token_program = token_program,
    )]
    pub treasury_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = stakers_vault,
        associated_token::token_program = token_program,
    )]
    pub stakers_vault_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: stakers-vault authority PDA, verified by seeds.
    #[account(seeds = [STAKERS_VAULT_SEED], bump = token_config.vault_bump)]
    pub stakers_vault: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateCategory<'info> {
    #[account(mut, has_one = owner @ NaimError::Unauthorized)]
    pub category_record: Box<Account<'info, CategoryRecord>>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CloseCategory<'info> {
    #[account(mut, has_one = owner @ NaimError::Unauthorized, close = owner)]
    pub category_record: Box<Account<'info, CategoryRecord>>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(name_hash: [u8; 32], capability: String, capability_hash: [u8; 32])]
pub struct PlaceRankBid<'info> {
    #[account(mut, seeds = [TOKEN_CONFIG_SEED], bump = token_config.bump)]
    pub token_config: Box<Account<'info, TokenConfig>>,
    // the bidder must own the name they are boosting
    #[account(
        seeds = [SEED_PREFIX, name_hash.as_ref(), TLD_AGENT],
        bump = name_record.bump,
        has_one = owner @ NaimError::Unauthorized
    )]
    pub name_record: Box<Account<'info, NameRecord>>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + RankBid::INIT_SPACE,
        seeds = [RANK_BID_SEED, name_hash.as_ref(), capability_hash.as_ref()],
        bump
    )]
    pub rank_bid: Box<Account<'info, RankBid>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut, address = token_config.naim_mint)]
    pub naim_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = naim_mint,
        associated_token::authority = owner,
        associated_token::token_program = token_program,
    )]
    pub bidder_naim_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}
